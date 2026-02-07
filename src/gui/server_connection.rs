//! Unified Server Connection
//!
//! Provides a unified interface for managing the server, whether via:
//! - D-Bus connection to an already-running service (preferred)
//! - Spawned child process (fallback for development/standalone)
//!
//! The GUI should try D-Bus first, then fall back to spawning.

use std::time::Duration;

use tokio::sync::mpsc;

use super::dbus_client::DbusClient;
use super::server_process::{ServerLogLine, ServerProcess};
use crate::config::Config;

/// Connection mode for the server
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionMode {
    /// Connected via D-Bus to an existing service
    DBus,
    /// Managing a spawned child process
    Process,
    /// Not connected
    Disconnected,
}

/// Unified server connection
///
/// Abstracts whether we're talking to a D-Bus service or a spawned process.
pub enum ServerConnection {
    /// Connected via D-Bus
    DBus(DbusClient),

    /// Managing a child process
    Process(ServerProcess),
}

impl ServerConnection {
    /// Try to connect to an existing D-Bus server
    ///
    /// Returns None if no server is available.
    pub async fn try_dbus() -> Option<Self> {
        DbusClient::try_connect().await.map(Self::DBus)
    }

    /// Start a new server process
    ///
    /// Falls back to spawning if D-Bus connection is not available.
    pub fn spawn_process(
        config: &Config,
        log_sender: mpsc::UnboundedSender<ServerLogLine>,
    ) -> Result<Self, String> {
        ServerProcess::start(config, log_sender)
            .map(Self::Process)
            .map_err(|e| e.to_string())
    }

    /// Connect to server - tries D-Bus first, then spawns if needed
    ///
    /// This is the recommended method for connecting to the server.
    pub async fn connect(
        config: &Config,
        log_sender: mpsc::UnboundedSender<ServerLogLine>,
        prefer_dbus: bool,
    ) -> Result<Self, String> {
        // Try D-Bus first if preferred
        if prefer_dbus {
            if let Some(mut client) = DbusClient::try_connect().await {
                client.set_log_sender(log_sender);
                tracing::info!("Connected to server via D-Bus");
                return Ok(Self::DBus(client));
            }
            tracing::debug!("D-Bus server not available, falling back to process spawn");
        }

        // Fall back to spawning a process
        Self::spawn_process(config, log_sender)
    }

    pub fn mode(&self) -> ConnectionMode {
        match self {
            Self::DBus(_) => ConnectionMode::DBus,
            Self::Process(_) => ConnectionMode::Process,
        }
    }

    pub async fn is_running(&self) -> bool {
        match self {
            Self::DBus(client) => client.is_running().await,
            Self::Process(process) => process.is_running(),
        }
    }

    pub async fn address(&self) -> String {
        match self {
            Self::DBus(client) => client.address().await.unwrap_or_default(),
            Self::Process(process) => process.address().to_string(),
        }
    }

    pub async fn uptime(&self) -> Duration {
        match self {
            Self::DBus(client) => client.uptime().await.unwrap_or_default(),
            Self::Process(process) => process.uptime(),
        }
    }

    pub async fn active_connections(&self) -> u32 {
        match self {
            Self::DBus(client) => client.active_connections().await.unwrap_or(0),
            Self::Process(_) => 0, // Process mode doesn't have this info directly
        }
    }

    /// Stop the server
    ///
    /// For D-Bus: This doesn't stop the server, just disconnects.
    /// For Process: This stops the child process.
    pub fn stop(&mut self) -> Result<(), String> {
        match self {
            Self::DBus(_) => {
                // D-Bus client just disconnects, doesn't stop the server
                tracing::info!("Disconnected from D-Bus server (server continues running)");
                Ok(())
            }
            Self::Process(process) => process.stop().map_err(|e| e.to_string()),
        }
    }

    pub fn pid(&self) -> Option<u32> {
        match self {
            Self::DBus(_) => None,
            Self::Process(process) => Some(process.pid()),
        }
    }

    pub fn as_dbus(&self) -> Option<&DbusClient> {
        match self {
            Self::DBus(client) => Some(client),
            Self::Process(_) => None,
        }
    }

    pub fn as_dbus_mut(&mut self) -> Option<&mut DbusClient> {
        match self {
            Self::DBus(client) => Some(client),
            Self::Process(_) => None,
        }
    }

    /// Detach the server so it keeps running when GUI exits
    ///
    /// For D-Bus mode, this is a no-op (server already runs independently).
    /// For Process mode, this prevents the server from being killed on drop.
    pub fn detach(&mut self) {
        if let Self::Process(process) = self {
            process.detach();
        }
    }
}

impl std::fmt::Debug for ServerConnection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DBus(_) => write!(f, "ServerConnection::DBus"),
            Self::Process(p) => write!(f, "ServerConnection::Process(pid={})", p.pid()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_connection_mode() {
        // Can't easily test without a running server
        // Just verify the enum works
        assert_eq!(ConnectionMode::DBus, ConnectionMode::DBus);
        assert_ne!(ConnectionMode::DBus, ConnectionMode::Process);
    }
}
