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

use zbus::{interface, zvariant::Value};

use super::{ClientInfo, SharedServerState};

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
}

impl RdpServerManager {
    pub fn new(state: SharedServerState) -> Self {
        Self {
            state,
            clients: tokio::sync::RwLock::new(Vec::new()),
            version: env!("CARGO_PKG_VERSION").to_string(),
        }
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

    async fn get_capabilities(&self) -> HashMap<String, Value<'static>> {
        let mut caps = HashMap::new();

        // TODO: Integrate with actual capability detection
        // For now, return placeholder values
        caps.insert("compositor".to_string(), Value::from("Unknown"));
        caps.insert("compositor_version".to_string(), Value::from(""));
        caps.insert("portal_version".to_string(), Value::from(0u32));
        caps.insert("session_persistence".to_string(), Value::from(false));
        caps.insert("hardware_encoding".to_string(), Value::from(false));
        caps.insert("deployment".to_string(), Value::from("unknown"));

        caps
    }

    async fn get_service_registry(&self) -> Vec<(String, String, u32)> {
        // TODO: Integrate with actual service registry
        // For now, return empty list
        Vec::new()
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

    async fn reload_config(&self) -> (bool, String) {
        // TODO: Signal main server to reload config
        // For now, just return success
        tracing::info!("Configuration reload requested via D-Bus");
        (true, String::new())
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

    async fn disconnect_client(&self, client_id: String, _reason: String) -> bool {
        // TODO: Actually disconnect the client via server
        // For now, just remove from our list
        if let Some(_info) = self.remove_client(&client_id).await {
            tracing::info!("Client {} disconnected via D-Bus", client_id);
            true
        } else {
            false
        }
    }

    // =========================================================================
    // Signals (TODO: Add when zbus signal syntax is resolved)
    // =========================================================================
    // The following signals will be emitted:
    // - status_changed(old_status, new_status, message)
    // - client_connected(client_id, peer_address, timestamp)
    // - client_disconnected(client_id, reason, duration_seconds)
    // - config_reloaded(config_path)
    // - log_message(level, module, message, timestamp)
    //
    // For now, use polling via GetStatistics or GetConnections methods.
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
}
