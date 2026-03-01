//! Server Event Relay
//!
//! Decouples the server from D-Bus awareness. The server emits `ServerEvent`
//! on an mpsc channel. When the D-Bus service is active, a relay task
//! converts events to D-Bus signals. When D-Bus is not active, events
//! are simply dropped (sender has no receiver).
//!
//! This pattern also supports future consumers: metrics, logging, CLI tool.

use tokio::sync::mpsc;
use tracing::{debug, info, warn};
use zbus::object_server::InterfaceRef;

use super::{manager::RdpServerManager, SharedServerState, OBJECT_PATH};

/// Events emitted by the server runtime for external consumption.
///
/// These are fire-and-forget — the server sends them without knowing
/// whether anyone is listening.
#[derive(Debug, Clone)]
pub enum ServerEvent {
    /// Server status transition
    StatusChanged {
        old: String,
        new: String,
        message: String,
    },

    /// New RDP client connected
    ClientConnected {
        client_id: String,
        peer_address: String,
        timestamp: u64,
    },

    /// RDP client disconnected
    ClientDisconnected {
        client_id: String,
        reason: String,
        duration_seconds: u64,
    },

    /// Configuration reloaded
    ConfigReloaded { config_path: String },

    /// Session health changed (from health monitor)
    SessionHealthChanged {
        old_health: String,
        new_health: String,
        detail: String,
    },

    /// Session type determined (emitted once during initialization)
    SessionTypeChanged { session_type: String },
}

/// Create an event channel for server events.
///
/// Returns (sender, receiver). The sender is cloned and distributed to
/// subsystems that need to emit events. The receiver is consumed by the
/// relay task.
pub fn event_channel() -> (
    mpsc::UnboundedSender<ServerEvent>,
    mpsc::UnboundedReceiver<ServerEvent>,
) {
    mpsc::unbounded_channel()
}

/// Start the D-Bus signal relay task.
///
/// Consumes events from the channel and emits D-Bus signals via the
/// registered `RdpServerManager` interface. Also updates the shared
/// server state for property changes.
///
/// Returns the task handle.
pub fn start_signal_relay(
    connection: zbus::Connection,
    mut event_rx: mpsc::UnboundedReceiver<ServerEvent>,
    state: SharedServerState,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        info!("D-Bus signal relay started");

        while let Some(event) = event_rx.recv().await {
            debug!("Relaying server event: {event:?}");

            match &event {
                ServerEvent::StatusChanged { old, new, message } => {
                    // Update shared state
                    {
                        let mut s = state.write().await;
                        s.status = super::ServerStatus::from(new.as_str());
                    }

                    // Emit D-Bus signal
                    if let Err(e) = emit_state_changed(&connection, old, new, message).await {
                        warn!("Failed to emit server_state_changed signal: {e}");
                    }
                }

                ServerEvent::ClientConnected {
                    client_id,
                    peer_address,
                    timestamp,
                } => {
                    // Update shared state
                    {
                        let mut s = state.write().await;
                        s.active_connections += 1;
                    }

                    if let Err(e) =
                        emit_client_connected(&connection, client_id, peer_address, *timestamp)
                            .await
                    {
                        warn!("Failed to emit client_connected signal: {e}");
                    }
                }

                ServerEvent::ClientDisconnected {
                    client_id,
                    reason,
                    duration_seconds,
                } => {
                    // Update shared state
                    {
                        let mut s = state.write().await;
                        s.active_connections = s.active_connections.saturating_sub(1);
                    }

                    if let Err(e) =
                        emit_client_disconnected(&connection, client_id, reason, *duration_seconds)
                            .await
                    {
                        warn!("Failed to emit client_disconnected signal: {e}");
                    }
                }

                ServerEvent::ConfigReloaded { config_path } => {
                    if let Err(e) = emit_config_reloaded(&connection, config_path).await {
                        warn!("Failed to emit config_reloaded signal: {e}");
                    }
                }

                ServerEvent::SessionTypeChanged { session_type } => {
                    // Update shared state
                    {
                        let mut s = state.write().await;
                        s.session_type.clone_from(session_type);
                    }
                    debug!("Session type set: {session_type}");
                }

                ServerEvent::SessionHealthChanged {
                    old_health,
                    new_health,
                    detail,
                } => {
                    // Health changes map to status_changed with health-prefixed values
                    let message = format!("Session health: {detail}");
                    let old = format!("running-{old_health}");
                    let new = format!("running-{new_health}");

                    if let Err(e) = emit_state_changed(&connection, &old, &new, &message).await {
                        warn!("Failed to emit health state_changed signal: {e}");
                    }
                }
            }
        }

        info!("D-Bus signal relay stopped (event channel closed)");
    })
}

async fn emit_state_changed(
    connection: &zbus::Connection,
    old: &str,
    new: &str,
    message: &str,
) -> Result<(), zbus::Error> {
    let iface_ref: InterfaceRef<RdpServerManager> =
        connection.object_server().interface(OBJECT_PATH).await?;

    RdpServerManager::server_state_changed(iface_ref.signal_context(), old, new, message).await
}

async fn emit_client_connected(
    connection: &zbus::Connection,
    client_id: &str,
    peer_address: &str,
    timestamp: u64,
) -> Result<(), zbus::Error> {
    let iface_ref: InterfaceRef<RdpServerManager> =
        connection.object_server().interface(OBJECT_PATH).await?;

    RdpServerManager::client_connected(
        iface_ref.signal_context(),
        client_id,
        peer_address,
        timestamp,
    )
    .await
}

async fn emit_client_disconnected(
    connection: &zbus::Connection,
    client_id: &str,
    reason: &str,
    duration_seconds: u64,
) -> Result<(), zbus::Error> {
    let iface_ref: InterfaceRef<RdpServerManager> =
        connection.object_server().interface(OBJECT_PATH).await?;

    RdpServerManager::client_disconnected(
        iface_ref.signal_context(),
        client_id,
        reason,
        duration_seconds,
    )
    .await
}

async fn emit_config_reloaded(
    connection: &zbus::Connection,
    config_path: &str,
) -> Result<(), zbus::Error> {
    let iface_ref: InterfaceRef<RdpServerManager> =
        connection.object_server().interface(OBJECT_PATH).await?;

    RdpServerManager::config_reloaded(iface_ref.signal_context(), config_path).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_event_channel_creation() {
        let (tx, _rx) = event_channel();
        // Sending without receiver should not panic
        let _ = tx.send(ServerEvent::StatusChanged {
            old: "stopped".into(),
            new: "running".into(),
            message: "test".into(),
        });
    }

    #[test]
    fn test_server_event_debug() {
        let event = ServerEvent::ClientConnected {
            client_id: "test-1".into(),
            peer_address: "192.168.1.1:3389".into(),
            timestamp: 123456,
        };
        let debug_str = format!("{event:?}");
        assert!(debug_str.contains("test-1"));
    }
}
