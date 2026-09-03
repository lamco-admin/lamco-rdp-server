//! D-Bus Manager Interface Implementation
//!
//! **Execution Path:** D-Bus service (io.lamco.RdpServer.Manager)
//! **Status:** Active (v1.0.0+)
//! **Platform:** Universal (requires D-Bus)
//! **Role:** Implements D-Bus interface for external management/monitoring
//!
//! Implements the io.lamco.RdpServer.Manager interface using zbus macros.
//!
//! **Note:** Keeps "Manager" suffix because it implements the external D-Bus API
//! contract `io.lamco.RdpServer.Manager`. Not a candidate for humanization rename.

#![expect(
    clippy::too_many_arguments,
    reason = "zbus #[interface] macro expands to dispatch fns taking one arg per property; we can't shrink them"
)]

use std::{
    collections::HashMap,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use tokio::sync::mpsc;
use zbus::{interface, zvariant::Value};

use super::{ClientInfo, SharedServerState};
use crate::health::snapshot_collector::SnapshotCollector;

/// Commands sent from D-Bus manager to the server runtime
#[derive(Debug)]
pub enum ServerCommand {
    /// Reload configuration from disk
    ReloadConfig,
    /// Disconnect a specific client by ID
    DisconnectClient { client_id: String, reason: String },
}

/// Manager interface implementation
///
/// This struct holds the shared state and implements the D-Bus interface
/// methods, properties, and signals.
pub struct RdpServerManager {
    /// Shared server state
    state: SharedServerState,

    /// Active client connections
    clients: tokio::sync::RwLock<Vec<ClientInfo>>,

    /// Server version
    version: String,

    /// Channel to send commands to the server runtime
    command_tx: Option<mpsc::UnboundedSender<ServerCommand>>,

    /// Performance snapshot collector (set after session is established)
    snapshot_collector: tokio::sync::RwLock<Option<Arc<SnapshotCollector>>>,

    /// Health subscriber for reading subsystem health states
    health_subscriber: tokio::sync::RwLock<Option<crate::health::HealthSubscriber>>,
}

impl RdpServerManager {
    pub fn new(state: SharedServerState) -> Self {
        Self {
            state,
            clients: tokio::sync::RwLock::new(Vec::new()),
            version: env!("CARGO_PKG_VERSION").to_string(),
            command_tx: None,
            snapshot_collector: tokio::sync::RwLock::new(None),
            health_subscriber: tokio::sync::RwLock::new(None),
        }
    }

    /// Create a manager with a command channel to the server runtime.
    ///
    /// The server should spawn a task that receives from the returned
    /// `mpsc::UnboundedReceiver<ServerCommand>` and acts on each command.
    pub fn with_command_channel(
        state: SharedServerState,
    ) -> (Self, mpsc::UnboundedReceiver<ServerCommand>) {
        let (tx, rx) = mpsc::unbounded_channel();
        let manager = Self {
            state,
            clients: tokio::sync::RwLock::new(Vec::new()),
            version: env!("CARGO_PKG_VERSION").to_string(),
            command_tx: Some(tx),
            snapshot_collector: tokio::sync::RwLock::new(None),
            health_subscriber: tokio::sync::RwLock::new(None),
        };
        (manager, rx)
    }

    pub async fn add_client(&self, info: ClientInfo) {
        let mut clients = self.clients.write().await;
        clients.push(info);
    }

    /// Set the snapshot collector once the session is established.
    pub async fn set_snapshot_collector(&self, collector: Arc<SnapshotCollector>) {
        *self.snapshot_collector.write().await = Some(collector);
    }

    /// Set the health subscriber for reading subsystem health states.
    pub async fn set_health_subscriber(&self, subscriber: crate::health::HealthSubscriber) {
        *self.health_subscriber.write().await = Some(subscriber);
    }

    pub async fn remove_client(&self, client_id: &str) -> Option<ClientInfo> {
        let mut clients = self.clients.write().await;
        clients
            .iter()
            .position(|c| c.client_id == client_id)
            .map(|pos| clients.remove(pos))
    }
}

/// The D-Bus interface trait
///
/// This is separated to allow for easier testing and mocking.
#[interface(name = "io.lamco.RdpServer.Manager")]
impl RdpServerManager {
    // =========================================================================
    // Properties
    // =========================================================================

    /// Server version string
    #[zbus(property)]
    async fn version(&self) -> String {
        self.version.clone()
    }

    /// Current server status: stopped, starting, running, error
    #[zbus(property)]
    async fn status(&self) -> String {
        self.state.read().await.status.to_string()
    }

    /// Address the server is listening on (empty if not running)
    #[zbus(property)]
    async fn listen_address(&self) -> String {
        self.state
            .read()
            .await
            .listen_address
            .clone()
            .unwrap_or_default()
    }

    /// Active session strategy (e.g., "Portal", "wlr-direct", "Mutter Direct API")
    #[zbus(property)]
    async fn session_type(&self) -> String {
        self.state.read().await.session_type.clone()
    }

    /// Number of active RDP connections
    #[zbus(property)]
    async fn active_connections(&self) -> u32 {
        self.state.read().await.active_connections
    }

    /// Path to the active configuration file
    #[zbus(property)]
    async fn config_path(&self) -> String {
        self.state.read().await.config_path.clone()
    }

    /// Seconds since server started (0 if not running)
    #[zbus(property)]
    async fn uptime(&self) -> u64 {
        let state = self.state.read().await;
        if let Some(start_time) = state.start_time {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            now.saturating_sub(start_time)
        } else {
            0
        }
    }

    // =========================================================================
    // Methods
    // =========================================================================

    /// Query detected system capabilities.
    ///
    /// Returns a map of capability names to values, populated from the
    /// capability probe system that runs at startup.
    async fn get_capabilities(&self) -> HashMap<String, Value<'static>> {
        let mut caps: HashMap<String, Value<'static>> = HashMap::new();

        if !crate::capabilities::Capabilities::is_initialized() {
            caps.insert(
                "error".into(),
                Value::from("not yet initialized".to_string()),
            );
            return caps;
        }

        let global = crate::capabilities::Capabilities::global();
        let state = global.read().await;

        // Compositor
        caps.insert(
            "compositor".into(),
            Value::from(state.state.display.compositor.name.clone()),
        );
        caps.insert(
            "compositor_version".into(),
            Value::from(
                state
                    .state
                    .display
                    .compositor
                    .version
                    .clone()
                    .unwrap_or_default(),
            ),
        );

        // Portal
        caps.insert(
            "portal_version".into(),
            Value::from(state.state.display.portal.version),
        );
        caps.insert(
            "portal_screencast".into(),
            Value::from(state.state.display.portal.supports_screencast),
        );
        caps.insert(
            "portal_remote_desktop".into(),
            Value::from(state.state.display.portal.supports_remote_desktop),
        );
        caps.insert(
            "portal_clipboard".into(),
            Value::from(state.state.display.portal.supports_clipboard),
        );
        caps.insert(
            "session_persistence".into(),
            Value::from(state.state.display.portal.supports_restore_tokens),
        );

        // Encoding
        caps.insert(
            "hardware_encoding".into(),
            Value::from(
                state.state.encoding.service_level == crate::capabilities::ServiceLevel::Full,
            ),
        );
        caps.insert(
            "encoding_level".into(),
            Value::from(state.state.encoding.service_level.name().to_string()),
        );

        // Deployment
        let deployment = if crate::config::is_flatpak() {
            "flatpak"
        } else {
            "native"
        };
        caps.insert("deployment".into(), Value::from(deployment.to_string()));

        // Service levels
        caps.insert(
            "display_level".into(),
            Value::from(state.state.display.service_level.name().to_string()),
        );
        caps.insert(
            "input_level".into(),
            Value::from(state.state.input.service_level.name().to_string()),
        );
        caps.insert(
            "storage_level".into(),
            Value::from(state.state.storage.service_level.name().to_string()),
        );
        caps.insert(
            "rendering_level".into(),
            Value::from(state.state.rendering.service_level.name().to_string()),
        );
        caps.insert(
            "network_level".into(),
            Value::from(state.state.network.service_level.name().to_string()),
        );

        // Summary
        caps.insert(
            "can_serve_rdp".into(),
            Value::from(state.state.minimum_viable.can_serve_rdp),
        );
        caps.insert(
            "can_render_gui".into(),
            Value::from(state.state.minimum_viable.can_render_gui),
        );

        caps
    }

    /// Query service subsystem status.
    ///
    /// Returns a list of (subsystem_name, service_level, level_value) tuples
    /// for each probed subsystem.
    async fn get_service_registry(&self) -> Vec<(String, String, u32)> {
        if !crate::capabilities::Capabilities::is_initialized() {
            return Vec::new();
        }

        let global = crate::capabilities::Capabilities::global();
        let state = global.read().await;

        vec![
            (
                "display".to_string(),
                state.state.display.service_level.name().to_string(),
                state.state.display.service_level as u32,
            ),
            (
                "encoding".to_string(),
                state.state.encoding.service_level.name().to_string(),
                state.state.encoding.service_level as u32,
            ),
            (
                "input".to_string(),
                state.state.input.service_level.name().to_string(),
                state.state.input.service_level as u32,
            ),
            (
                "storage".to_string(),
                state.state.storage.service_level.name().to_string(),
                state.state.storage.service_level as u32,
            ),
            (
                "rendering".to_string(),
                state.state.rendering.service_level.name().to_string(),
                state.state.rendering.service_level as u32,
            ),
            (
                "network".to_string(),
                state.state.network.service_level.name().to_string(),
                state.state.network.service_level as u32,
            ),
        ]
    }

    async fn get_config(&self) -> String {
        let config_path = &self.state.read().await.config_path;
        if config_path.is_empty() {
            return String::new();
        }

        match std::fs::read_to_string(config_path) {
            Ok(content) => content,
            Err(e) => {
                tracing::warn!("Failed to read config: {}", e);
                String::new()
            }
        }
    }

    async fn set_config(&self, config: String) -> (bool, String) {
        let config_path = self.state.read().await.config_path.clone();
        if config_path.is_empty() {
            return (false, "No config path set".to_string());
        }

        if let Err(e) = config.parse::<toml::Table>() {
            return (false, format!("Invalid TOML: {e}"));
        }

        match std::fs::write(&config_path, &config) {
            Ok(()) => {
                tracing::info!("Configuration updated via D-Bus");
                (true, String::new())
            }
            Err(e) => (false, format!("Failed to write config: {e}")),
        }
    }

    /// Request the server to reload its configuration from disk.
    ///
    /// If a command channel is configured, sends a ReloadConfig command
    /// to the server runtime. Otherwise there is no live runtime to notify,
    /// so this reports failure rather than claiming a reload that didn't happen.
    async fn reload_config(&self) -> (bool, String) {
        tracing::info!("Configuration reload requested via D-Bus");

        if let Some(tx) = &self.command_tx {
            match tx.send(ServerCommand::ReloadConfig) {
                Ok(()) => (true, String::new()),
                Err(_) => (false, "Server command channel closed".to_string()),
            }
        } else {
            (
                false,
                "Hot-reload not available in this process; config was written to disk by \
                 SetConfig and will take effect on next connection"
                    .to_string(),
            )
        }
    }

    async fn get_statistics(&self) -> HashMap<String, Value<'static>> {
        let state = self.state.read().await;
        let mut stats = HashMap::new();

        stats.insert(
            "frames_encoded".to_string(),
            Value::from(state.stats.frames_encoded),
        );
        stats.insert(
            "bytes_sent".to_string(),
            Value::from(state.stats.bytes_sent),
        );
        stats.insert(
            "clients_total".to_string(),
            Value::from(state.stats.clients_total),
        );
        stats.insert(
            "average_fps".to_string(),
            Value::from(state.stats.average_fps),
        );
        stats.insert(
            "average_latency_ms".to_string(),
            Value::from(state.stats.average_latency_ms),
        );

        stats
    }

    async fn get_connections(&self) -> Vec<(String, String, String, u64)> {
        let clients = self.clients.read().await;
        clients
            .iter()
            .map(|c| {
                (
                    c.client_id.clone(),
                    c.peer_address.clone(),
                    c.username.clone(),
                    c.connected_at,
                )
            })
            .collect()
    }

    /// Disconnect a client by ID.
    ///
    /// If a command channel is configured, sends a DisconnectClient command
    /// to the server runtime which will close the RDP connection.
    /// Also removes the client from the local tracking list.
    async fn disconnect_client(&self, client_id: String, reason: String) -> bool {
        if let Some(tx) = &self.command_tx {
            let _ = tx.send(ServerCommand::DisconnectClient {
                client_id: client_id.clone(),
                reason,
            });
        }

        if let Some(_info) = self.remove_client(&client_id).await {
            tracing::info!("Client {} disconnected via D-Bus", client_id);
            true
        } else {
            false
        }
    }

    /// Query current session health for all subsystems.
    ///
    /// Returns a map of subsystem names to health status strings.
    /// Health is the liveness layer (is it working?), not performance.
    async fn get_health(&self) -> HashMap<String, Value<'static>> {
        let mut result: HashMap<String, Value<'static>> = HashMap::new();
        result.insert("version".into(), Value::from(self.version.clone()));

        // Read subsystem health states from the health monitor
        let subscriber = self.health_subscriber.read().await;
        if let Some(ref sub) = *subscriber {
            let state = sub.current();
            result.insert("status".into(), Value::from("available".to_string()));
            result.insert("video".into(), Value::from(state.video.to_string()));
            result.insert("input".into(), Value::from(state.input.to_string()));
            result.insert("clipboard".into(), Value::from(state.clipboard.to_string()));
            result.insert("session".into(), Value::from(state.session.to_string()));
            result.insert("overall".into(), Value::from(state.overall.to_string()));
        } else {
            result.insert("status".into(), Value::from("not_available".to_string()));
        }

        // Include sensor summary if available
        let collector = self.snapshot_collector.read().await;
        if let Some(ref collector) = *collector {
            let snap = collector.snapshot();
            result.insert(
                "sensors_registered".into(),
                Value::from(snap.sensor_snapshots.len() as u32),
            );
        }

        result
    }

    /// Query current performance metrics as a flat map.
    ///
    /// Returns all performance metrics from the PerformanceSnapshot.
    /// This is the primary programmatic API for monitoring consumers.
    async fn get_performance(&self) -> HashMap<String, Value<'static>> {
        let mut result: HashMap<String, Value<'static>> = HashMap::new();

        let collector = self.snapshot_collector.read().await;
        let Some(ref collector) = *collector else {
            result.insert(
                "error".into(),
                Value::from("session not established".to_string()),
            );
            return result;
        };

        let snap = collector.snapshot();

        // FPS
        result.insert("fps".into(), Value::from(snap.fps.current_fps));
        result.insert(
            "activity_level".into(),
            Value::from(snap.fps.activity_level.clone()),
        );
        result.insert("adaptive_fps_enabled".into(), Value::from(snap.fps.enabled));

        // Latency
        result.insert(
            "latency_mode".into(),
            Value::from(snap.latency.mode.clone()),
        );
        result.insert(
            "total_latency_avg_ms".into(),
            Value::from(f64::from(snap.latency.total_latency_avg_ms)),
        );
        result.insert(
            "encode_duration_avg_ms".into(),
            Value::from(f64::from(snap.latency.encode_duration_avg_ms)),
        );

        // EGFX
        result.insert("egfx_ready".into(), Value::from(snap.egfx.channel_ready));
        result.insert(
            "egfx_queue_depth".into(),
            Value::from(snap.egfx.queue_depth),
        );
        result.insert("egfx_frame_acks".into(), Value::from(snap.egfx.frame_acks));
        if let Some(decode_us) = snap.egfx.client_decode_render_us {
            result.insert("client_decode_render_us".into(), Value::from(decode_us));
        }
        if let Some(ref version) = snap.egfx.negotiated_version {
            result.insert("egfx_version".into(), Value::from(version.clone()));
        }

        // Encoder
        if let Some(ref enc) = snap.encoder {
            result.insert("encoder_backend".into(), Value::from(enc.backend.clone()));
            result.insert("encoder_fps".into(), Value::from(f64::from(enc.fps)));
            result.insert("encoder_bitrate_kbps".into(), Value::from(enc.bitrate_kbps));
        }

        // Aggregate
        result.insert("uptime_seconds".into(), Value::from(snap.uptime.as_secs()));
        result.insert(
            "frames_received".into(),
            Value::from(snap.metrics.frames_received),
        );
        result.insert(
            "frames_dropped".into(),
            Value::from(snap.metrics.frames_dropped),
        );

        result
    }

    /// Query negotiated protocol versions.
    ///
    /// Returns version information for each active protocol layer.
    async fn get_protocol_versions(&self) -> HashMap<String, Value<'static>> {
        let mut result: HashMap<String, Value<'static>> = HashMap::new();

        let collector = self.snapshot_collector.read().await;
        let Some(ref collector) = *collector else {
            return result;
        };

        let snap = collector.snapshot();
        if let Some(ref v) = snap.egfx.negotiated_version {
            result.insert("egfx".into(), Value::from(v.clone()));
        }
        if let Some(ref enc) = snap.encoder {
            result.insert("encoder".into(), Value::from(enc.backend.clone()));
        }

        result
    }

    // =========================================================================
    // Signals
    // =========================================================================

    /// Emitted periodically with performance metrics update.
    #[zbus(signal)]
    pub async fn performance_updated(
        ctxt: &zbus::object_server::SignalEmitter<'_>,
        fps: u32,
        latency_ms: f32,
        queue_depth: u32,
        encoder_backend: &str,
        activity_level: &str,
        current_qp: u32,
        adaptation_enabled: bool,
        damage_source: &str,
        sensor_count: u32,
        bitrate_kbps: u32,
        health_video: &str,
        health_input: &str,
        health_clipboard: &str,
        health_session: &str,
    ) -> zbus::Result<()>;

    /// Emitted when the server status changes (distinct from property change signal).
    #[zbus(signal)]
    pub async fn server_state_changed(
        ctxt: &zbus::object_server::SignalEmitter<'_>,
        old_status: &str,
        new_status: &str,
        message: &str,
    ) -> zbus::Result<()>;

    /// Emitted when a new RDP client connects.
    #[zbus(signal)]
    pub async fn client_connected(
        ctxt: &zbus::object_server::SignalEmitter<'_>,
        client_id: &str,
        peer_address: &str,
        timestamp: u64,
    ) -> zbus::Result<()>;

    /// Emitted when an RDP client disconnects.
    #[zbus(signal)]
    pub async fn client_disconnected(
        ctxt: &zbus::object_server::SignalEmitter<'_>,
        client_id: &str,
        reason: &str,
        duration_seconds: u64,
    ) -> zbus::Result<()>;

    /// Emitted when configuration is reloaded.
    #[zbus(signal)]
    pub async fn config_reloaded(
        ctxt: &zbus::object_server::SignalEmitter<'_>,
        config_path: &str,
    ) -> zbus::Result<()>;
}

/// Marker trait for the interface
///
/// The zbus `#[interface]` macro generates signal methods that can be called
/// to emit signals. For external signal emission from outside interface methods,
/// use `ObjectServer::with()` to get an `InterfaceRef`.
pub trait ManagerInterface: Send + Sync {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dbus::new_shared_state;

    #[tokio::test]
    async fn test_manager_properties() {
        let state = new_shared_state();
        let manager = RdpServerManager::new(state);

        assert_eq!(manager.version().await, env!("CARGO_PKG_VERSION"));
        assert_eq!(manager.status().await, "stopped");
        assert_eq!(manager.active_connections().await, 0);
    }

    #[tokio::test]
    async fn test_client_management() {
        let state = new_shared_state();
        let manager = RdpServerManager::new(state);

        let client = ClientInfo {
            client_id: "test-1".to_string(),
            peer_address: "192.168.1.100:12345".to_string(),
            username: "user".to_string(),
            connected_at: 1234567890,
        };

        manager.add_client(client).await;
        let connections = manager.get_connections().await;
        assert_eq!(connections.len(), 1);
        assert_eq!(connections[0].0, "test-1");

        manager.remove_client("test-1").await;
        let connections = manager.get_connections().await;
        assert_eq!(connections.len(), 0);
    }

    #[tokio::test]
    async fn test_capabilities_before_init() {
        let state = new_shared_state();
        let manager = RdpServerManager::new(state);

        // Before Capabilities::initialize(), should return error key
        let caps = manager.get_capabilities().await;
        assert!(caps.contains_key("error"));
    }

    #[tokio::test]
    async fn test_service_registry_before_init() {
        let state = new_shared_state();
        let manager = RdpServerManager::new(state);

        // Before Capabilities::initialize(), should return empty
        let registry = manager.get_service_registry().await;
        assert!(registry.is_empty());
    }

    #[tokio::test]
    async fn test_reload_config_without_channel() {
        let state = new_shared_state();
        let manager = RdpServerManager::new(state);

        let (success, msg) = manager.reload_config().await;
        assert!(!success);
        assert!(msg.contains("next connection"));
    }

    #[tokio::test]
    async fn test_reload_config_with_channel() {
        let state = new_shared_state();
        let (manager, mut rx) = RdpServerManager::with_command_channel(state);

        let (success, msg) = manager.reload_config().await;
        assert!(success);
        assert!(msg.is_empty());

        // Verify command was received
        let cmd = rx.try_recv().expect("should have received command");
        assert!(matches!(cmd, ServerCommand::ReloadConfig));
    }

    #[tokio::test]
    async fn test_disconnect_with_channel() {
        let state = new_shared_state();
        let (manager, mut rx) = RdpServerManager::with_command_channel(state);

        let client = ClientInfo {
            client_id: "test-1".to_string(),
            peer_address: "192.168.1.100:12345".to_string(),
            username: "user".to_string(),
            connected_at: 1234567890,
        };
        manager.add_client(client).await;

        let result = manager
            .disconnect_client("test-1".to_string(), "admin request".to_string())
            .await;
        assert!(result);

        // Verify command was sent
        let cmd = rx.try_recv().expect("should have received command");
        assert!(matches!(cmd, ServerCommand::DisconnectClient { .. }));

        // Verify client removed from list
        let connections = manager.get_connections().await;
        assert!(connections.is_empty());
    }
}
