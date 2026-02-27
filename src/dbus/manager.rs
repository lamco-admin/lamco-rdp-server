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

use std::{
    collections::HashMap,
    time::{SystemTime, UNIX_EPOCH},
};

use tokio::sync::mpsc;
use zbus::{interface, zvariant::Value};

use super::{ClientInfo, SharedServerState};

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
}

impl RdpServerManager {
    pub fn new(state: SharedServerState) -> Self {
        Self {
            state,
            clients: tokio::sync::RwLock::new(Vec::new()),
            version: env!("CARGO_PKG_VERSION").to_string(),
            command_tx: None,
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
        };
        (manager, rx)
    }

    pub async fn add_client(&self, info: ClientInfo) {
        let mut clients = self.clients.write().await;
        clients.push(info);
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
    /// to the server runtime. Otherwise writes the config and returns success.
    async fn reload_config(&self) -> (bool, String) {
        tracing::info!("Configuration reload requested via D-Bus");

        if let Some(tx) = &self.command_tx {
            match tx.send(ServerCommand::ReloadConfig) {
                Ok(()) => (true, String::new()),
                Err(_) => (false, "Server command channel closed".to_string()),
            }
        } else {
            // No command channel: config was written by set_config(),
            // server will pick it up on next connection
            (
                true,
                "Config saved; will take effect on next connection".to_string(),
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

    // =========================================================================
    // Signals
    // =========================================================================

    /// Emitted when the server status changes (distinct from property change signal).
    #[zbus(signal)]
    pub async fn server_state_changed(
        ctxt: &zbus::object_server::SignalContext<'_>,
        old_status: &str,
        new_status: &str,
        message: &str,
    ) -> zbus::Result<()>;

    /// Emitted when a new RDP client connects.
    #[zbus(signal)]
    pub async fn client_connected(
        ctxt: &zbus::object_server::SignalContext<'_>,
        client_id: &str,
        peer_address: &str,
        timestamp: u64,
    ) -> zbus::Result<()>;

    /// Emitted when an RDP client disconnects.
    #[zbus(signal)]
    pub async fn client_disconnected(
        ctxt: &zbus::object_server::SignalContext<'_>,
        client_id: &str,
        reason: &str,
        duration_seconds: u64,
    ) -> zbus::Result<()>;

    /// Emitted when configuration is reloaded.
    #[zbus(signal)]
    pub async fn config_reloaded(
        ctxt: &zbus::object_server::SignalContext<'_>,
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
        assert!(success);
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
