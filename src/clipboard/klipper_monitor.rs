//! Klipper Signal Monitor
//!
//! **Execution Path:** Klipper D-Bus signals (clipboardHistoryUpdated)
//! **Status:** Active (v1.3.0+)
//! **Platform:** KDE Plasma with Klipper
//! **Purpose:** Diagnostic monitoring for Tier 3 fallback
//!
//! Monitors Klipper's `clipboardHistoryUpdated` D-Bus signal to detect
//! when Klipper processes clipboard changes. This allows precise timing
//! for re-announce mitigation instead of arbitrary delays.

use std::sync::Arc;

use futures::stream::StreamExt;
use tokio::sync::broadcast;
use tracing::{debug, error, info, warn};
use zbus::{proxy, Connection};

/// Event emitted when Klipper processes clipboard
#[derive(Debug, Clone)]
pub struct KlipperClipboardEvent {
    /// Timestamp when signal received
    pub timestamp: std::time::Instant,
}

/// D-Bus proxy for Klipper signals
#[proxy(
    interface = "org.kde.klipper.klipper",
    default_service = "org.kde.klipper",
    default_path = "/klipper"
)]
trait KlipperSignalProxy {
    /// Signal emitted when Klipper updates clipboard history
    #[zbus(signal)]
    fn clipboard_history_updated(&self);
}

/// Klipper signal monitor for KDE-specific clipboard behavior
///
/// Subscribes to Klipper's `clipboardHistoryUpdated` D-Bus signal
/// to detect when Klipper has processed a clipboard change.
pub struct KlipperSignalMonitor {
    _connection: Arc<Connection>,
    sender: broadcast::Sender<KlipperClipboardEvent>,
}

impl KlipperSignalMonitor {
    /// Connect to Klipper's D-Bus signal
    ///
    /// Returns an error if D-Bus connection fails or Klipper service
    /// is not available.
    pub async fn connect() -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let connection = Connection::session().await?;
        let connection = Arc::new(connection);
        let (sender, _) = broadcast::channel(64);

        let monitor = Self {
            _connection: connection.clone(),
            sender,
        };

        let sender_clone = monitor.sender.clone();
        let conn_clone = connection.clone();
        tokio::spawn(async move {
            if let Err(e) = Self::listen_for_signals(conn_clone, sender_clone).await {
                error!("Klipper signal listener error: {}", e);
            }
        });

        info!("Klipper signal monitor connected");
        Ok(monitor)
    }

    /// Subscribe to Klipper clipboard events
    ///
    /// Returns a broadcast receiver that will receive events whenever
    /// Klipper processes clipboard changes.
    pub fn subscribe(&self) -> broadcast::Receiver<KlipperClipboardEvent> {
        self.sender.subscribe()
    }

    /// Check if Klipper D-Bus service is available
    #[expect(clippy::expect_used, reason = "static well-known D-Bus bus name")]
    pub async fn is_available() -> bool {
        let Ok(conn) = Connection::session().await else {
            return false;
        };

        let Ok(dbus) = zbus::fdo::DBusProxy::new(&conn).await else {
            return false;
        };

        dbus.name_has_owner("org.kde.klipper".try_into().expect("valid bus name"))
            .await
            .unwrap_or(false)
    }

    /// Listen for Klipper signals and forward to subscribers
    async fn listen_for_signals(
        connection: Arc<Connection>,
        sender: broadcast::Sender<KlipperClipboardEvent>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        debug!("Starting Klipper signal listener task");

        let proxy = KlipperSignalProxyProxy::new(&connection).await?;

        let mut stream = proxy.receive_clipboard_history_updated().await?;

        info!("Listening for Klipper clipboardHistoryUpdated signals");

        while let Some(signal) = stream.next().await {
            debug!(
                "Klipper clipboardHistoryUpdated signal received: {:?}",
                signal
            );

            let event = KlipperClipboardEvent {
                timestamp: std::time::Instant::now(),
            };

            let _ = sender.send(event);
        }

        warn!("Klipper signal stream ended");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_klipper_availability_check() {
        // This test requires Klipper to be running
        // Will pass or fail depending on environment
        let available = KlipperSignalMonitor::is_available().await;
        println!("Klipper available: {available}");
    }
}
