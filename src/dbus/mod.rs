//! D-Bus Management Interface
//!
//! Exposes the server's management interface over D-Bus, enabling:
//! - GUI to connect and manage the server
//! - Status monitoring via signals
//! - Configuration management
//! - Client connection management
//!
//! ## Bus Selection
//!
//! | Deployment | Bus | Service Name |
//! |------------|-----|--------------|
//! | Native/Flatpak/Systemd User | Session Bus | `io.lamco.RdpServer` |
//! | Systemd System | System Bus | `io.lamco.RdpServer.System` |

mod manager;

use std::sync::Arc;

pub use manager::{ManagerInterface, RdpServerManager};
use tokio::sync::RwLock;
use zbus::Connection;

/// D-Bus service name for session bus
pub const SERVICE_NAME: &str = "io.lamco.RdpServer";

/// D-Bus service name for system bus
pub const SYSTEM_SERVICE_NAME: &str = "io.lamco.RdpServer.System";

/// D-Bus object path
pub const OBJECT_PATH: &str = "/io/lamco/RdpServer";

/// D-Bus interface name
pub const INTERFACE_NAME: &str = "io.lamco.RdpServer.Manager";

/// Server state shared between D-Bus interface and main server
#[derive(Debug, Clone)]
pub struct ServerState {
    /// Current server status
    pub status: ServerStatus,

    /// Listen address (when running)
    pub listen_address: Option<String>,

    /// Active connection count
    pub active_connections: u32,

    /// Server start time (unix timestamp)
    pub start_time: Option<u64>,

    /// Configuration file path
    pub config_path: String,

    /// Runtime statistics
    pub stats: RuntimeStats,
}

impl Default for ServerState {
    fn default() -> Self {
        Self {
            status: ServerStatus::Stopped,
            listen_address: None,
            active_connections: 0,
            start_time: None,
            config_path: String::new(),
            stats: RuntimeStats::default(),
        }
    }
}

/// Server status enum
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ServerStatus {
    #[default]
    Stopped,
    Starting,
    Running,
    Error,
}

impl std::fmt::Display for ServerStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Stopped => write!(f, "stopped"),
            Self::Starting => write!(f, "starting"),
            Self::Running => write!(f, "running"),
            Self::Error => write!(f, "error"),
        }
    }
}

impl From<&str> for ServerStatus {
    fn from(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "stopped" => Self::Stopped,
            "starting" => Self::Starting,
            "running" => Self::Running,
            "error" => Self::Error,
            _ => Self::Stopped,
        }
    }
}

/// Runtime statistics
#[derive(Debug, Clone, Default)]
pub struct RuntimeStats {
    /// Total frames encoded
    pub frames_encoded: u64,

    /// Total bytes sent
    pub bytes_sent: u64,

    /// Total clients that have connected
    pub clients_total: u64,

    /// Average FPS (recent window)
    pub average_fps: f64,

    /// Average latency in ms (recent window)
    pub average_latency_ms: f64,
}

/// Active client connection info
#[derive(Debug, Clone)]
pub struct ClientInfo {
    /// Unique client identifier
    pub client_id: String,

    /// Peer address (IP:port)
    pub peer_address: String,

    /// Authenticated username (empty if no auth)
    pub username: String,

    /// Connection timestamp (unix)
    pub connected_at: u64,
}

/// Shared state type for thread-safe access
pub type SharedServerState = Arc<RwLock<ServerState>>;

pub fn new_shared_state() -> SharedServerState {
    Arc::new(RwLock::new(ServerState::default()))
}

/// Start the D-Bus service
///
/// Registers the management interface on the appropriate bus.
pub async fn start_service(
    use_system_bus: bool,
    state: SharedServerState,
) -> Result<Connection, zbus::Error> {
    let manager = RdpServerManager::new(state);

    let connection = if use_system_bus {
        Connection::system().await?
    } else {
        Connection::session().await?
    };

    let service_name = if use_system_bus {
        SYSTEM_SERVICE_NAME
    } else {
        SERVICE_NAME
    };

    connection.object_server().at(OBJECT_PATH, manager).await?;
    connection.request_name(service_name).await?;

    tracing::info!(
        "D-Bus management interface registered: {} on {} bus",
        service_name,
        if use_system_bus { "system" } else { "session" }
    );

    Ok(connection)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_server_status_display() {
        assert_eq!(ServerStatus::Stopped.to_string(), "stopped");
        assert_eq!(ServerStatus::Running.to_string(), "running");
    }

    #[test]
    fn test_server_status_from_str() {
        assert_eq!(ServerStatus::from("stopped"), ServerStatus::Stopped);
        assert_eq!(ServerStatus::from("RUNNING"), ServerStatus::Running);
    }

    #[test]
    fn test_default_state() {
        let state = ServerState::default();
        assert_eq!(state.status, ServerStatus::Stopped);
        assert_eq!(state.active_connections, 0);
    }
}
