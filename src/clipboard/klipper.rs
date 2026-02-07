//! Klipper Integration and Diagnostics
//!
//! **Execution Path:** Klipper D-Bus API (org.kde.klipper)
//! **Status:** Active (v1.0.0+)
//! **Platform:** KDE Plasma only
//! **Purpose:** Detection and monitoring for cooperation mode
//!
//! Provides detection and monitoring of KDE's klipper clipboard manager
//! to enable compositor-aware clipboard behavior.
//!
//! # Klipper Versions
//!
//! | Plasma Version | Klipper Changes |
//! |----------------|-----------------|
//! | Plasma 5.x     | Integrated into plasmashell, klipper is a plasmoid |
//! | Plasma 6.0-6.1 | Same architecture, minor fixes |
//! | Plasma 6.2     | Changed popup behavior (opens full widget at cursor) |
//! | Plasma 6.5     | Starred/pinned items, ext-data-control-v1 transition |
//!
//! # D-Bus Interface
//!
//! Klipper exposes `org.kde.klipper` service with interface `org.kde.klipper.klipper`:
//!
//! - `getClipboardContents()` - Read current clipboard (string only)
//! - `setClipboardContents(string)` - Set clipboard (string only)
//! - `clipboardHistoryUpdated` signal - Emitted on any clipboard change
//!
//! # Limitation
//!
//! The D-Bus interface only supports string data - no binary, images, or
//! multiple MIME types. For full clipboard integration, Portal/data-control
//! protocols are required.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};
use zbus::Connection;

/// Klipper D-Bus service name
const KLIPPER_SERVICE: &str = "org.kde.klipper";

/// Klipper D-Bus object path
const KLIPPER_PATH: &str = "/klipper";

/// Klipper D-Bus interface
const KLIPPER_INTERFACE: &str = "org.kde.klipper.klipper";

/// Klipper detection and monitoring result
#[derive(Debug, Clone)]
pub struct KlipperInfo {
    /// Whether klipper service was detected
    pub detected: bool,

    /// Whether klipper D-Bus interface is responsive
    pub responsive: bool,

    /// Plasma version if detectable
    pub plasma_version: Option<String>,

    /// Number of clipboard history updates observed
    pub history_updates: u64,

    /// Last update timestamp (unix millis)
    pub last_update_ms: u64,
}

impl Default for KlipperInfo {
    fn default() -> Self {
        Self {
            detected: false,
            responsive: false,
            plasma_version: None,
            history_updates: 0,
            last_update_ms: 0,
        }
    }
}

/// Klipper diagnostic event
#[derive(Debug, Clone)]
pub enum KlipperEvent {
    /// Klipper clipboard history was updated
    HistoryUpdated {
        /// Event sequence number
        seq: u64,
        /// Timestamp in milliseconds since epoch
        timestamp_ms: u64,
    },
}

/// Klipper monitor for diagnostic purposes
pub struct KlipperMonitor {
    /// D-Bus connection
    connection: Connection,

    /// Whether monitoring is active
    active: Arc<AtomicBool>,

    /// Event counter
    event_count: Arc<AtomicU64>,

    /// Last event timestamp
    last_event_ms: Arc<AtomicU64>,
}

impl KlipperMonitor {
    /// Check if klipper service is available on the session bus
    pub async fn detect() -> KlipperInfo {
        info!("┌─ Klipper Detection ─────────────────────────────────────────");

        let connection = match Connection::session().await {
            Ok(c) => c,
            Err(e) => {
                warn!("│ Cannot connect to session bus: {}", e);
                info!("└────────────────────────────────────────────────────────────────");
                return KlipperInfo::default();
            }
        };

        let dbus_proxy = zbus::fdo::DBusProxy::new(&connection).await;
        let service_exists = match &dbus_proxy {
            Ok(proxy) => proxy
                .name_has_owner(KLIPPER_SERVICE.try_into().unwrap())
                .await
                .unwrap_or(false),
            Err(_) => false,
        };

        if !service_exists {
            info!("│ Klipper service not detected (not KDE or klipper disabled)");
            info!("└────────────────────────────────────────────────────────────────");
            return KlipperInfo {
                detected: false,
                ..Default::default()
            };
        }

        info!("│ Klipper service DETECTED: {}", KLIPPER_SERVICE);

        let responsive = Self::test_klipper_responsive(&connection).await;

        if responsive {
            info!("│ Klipper D-Bus interface is RESPONSIVE");
        } else {
            warn!("│ Klipper D-Bus interface detected but NOT responding");
        }

        let plasma_version = Self::detect_plasma_version(&connection).await;
        if let Some(ref ver) = plasma_version {
            info!("│ Plasma version: {}", ver);
        }

        info!("│ ");
        info!("│ DIAGNOSTIC IMPLICATIONS:");
        info!("│ - Klipper may take over clipboard ~1s after SetSelection");
        info!("│ - Look for 'x-kde-onlyReplaceEmpty' in MIME types");
        info!("│ - clipboardHistoryUpdated signal indicates klipper activity");
        info!("└────────────────────────────────────────────────────────────────");

        KlipperInfo {
            detected: true,
            responsive,
            plasma_version,
            history_updates: 0,
            last_update_ms: 0,
        }
    }

    /// Test if klipper interface is responsive
    async fn test_klipper_responsive(connection: &Connection) -> bool {
        // Use low-level call to avoid needing a proxy struct
        let result = connection
            .call_method(
                Some(KLIPPER_SERVICE),
                KLIPPER_PATH,
                Some(KLIPPER_INTERFACE),
                "getClipboardContents",
                &(),
            )
            .await;

        match result {
            Ok(_) => true,
            Err(e) => {
                debug!("Klipper getClipboardContents failed: {}", e);
                false
            }
        }
    }

    /// Try to detect Plasma version from various sources
    async fn detect_plasma_version(connection: &Connection) -> Option<String> {
        let result = connection
            .call_method(
                Some("org.kde.plasmashell"),
                "/MainApplication",
                Some("org.freedesktop.DBus.Properties"),
                "Get",
                &("org.qtproject.Qt.QCoreApplication", "applicationVersion"),
            )
            .await;

        if let Ok(msg) = result {
            if let Ok(variant) = msg.body().deserialize::<zbus::zvariant::OwnedValue>() {
                if let Ok(version) = String::try_from(variant) {
                    return Some(version);
                }
            }
        }

        std::env::var("KDE_SESSION_VERSION").ok()
    }

    pub async fn new() -> Result<Self, zbus::Error> {
        let connection = Connection::session().await?;

        Ok(Self {
            connection,
            active: Arc::new(AtomicBool::new(false)),
            event_count: Arc::new(AtomicU64::new(0)),
            last_event_ms: Arc::new(AtomicU64::new(0)),
        })
    }

    /// Start monitoring klipper signals
    ///
    /// Returns a channel that receives klipper events for diagnostic logging.
    pub async fn start_monitoring(
        &self,
    ) -> Result<mpsc::UnboundedReceiver<KlipperEvent>, zbus::Error> {
        let (tx, rx) = mpsc::unbounded_channel();

        let connection = self.connection.clone();
        let active = Arc::clone(&self.active);
        let event_count = Arc::clone(&self.event_count);
        let last_event_ms = Arc::clone(&self.last_event_ms);

        self.active.store(true, Ordering::SeqCst);

        tokio::spawn(async move {
            info!("Klipper signal monitor starting...");

            let rule = format!(
                "type='signal',interface='{}',member='clipboardHistoryUpdated'",
                KLIPPER_INTERFACE
            );

            if let Err(e) = connection
                .call_method(
                    Some("org.freedesktop.DBus"),
                    "/org/freedesktop/DBus",
                    Some("org.freedesktop.DBus"),
                    "AddMatch",
                    &rule,
                )
                .await
            {
                error!("Failed to add D-Bus match rule for klipper: {}", e);
                return;
            }

            info!(
                "Subscribed to {} clipboardHistoryUpdated signal",
                KLIPPER_INTERFACE
            );

            // Note: Full signal monitoring would require using MessageStream
            // For now, we'll use the match rule for diagnostics and rely on
            // our existing Portal signal path for actual clipboard handling

            // This task keeps the match rule active
            while active.load(Ordering::SeqCst) {
                tokio::time::sleep(tokio::time::Duration::from_secs(60)).await;
            }

            info!("Klipper signal monitor stopped");
        });

        Ok(rx)
    }

    /// Stop monitoring
    pub fn stop(&self) {
        self.active.store(false, Ordering::SeqCst);
    }

    /// Get current event count
    pub fn event_count(&self) -> u64 {
        self.event_count.load(Ordering::SeqCst)
    }

    /// Get last event timestamp
    pub fn last_event_ms(&self) -> u64 {
        self.last_event_ms.load(Ordering::SeqCst)
    }
}

/// Log klipper diagnostic summary
pub fn log_klipper_status(info: &KlipperInfo) {
    info!("┌─ Klipper Status ───────────────────────────────────────────────");
    info!("│ Detected: {}", info.detected);
    info!("│ Responsive: {}", info.responsive);
    if let Some(ref ver) = info.plasma_version {
        info!("│ Plasma Version: {}", ver);
    }
    info!("│ History Updates Observed: {}", info.history_updates);
    if info.last_update_ms > 0 {
        info!(
            "│ Last Update: {}ms ago",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as u64 - info.last_update_ms)
                .unwrap_or(0)
        );
    }
    info!("└────────────────────────────────────────────────────────────────");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_klipper_info_default() {
        let info = KlipperInfo::default();
        assert!(!info.detected);
        assert!(!info.responsive);
        assert!(info.plasma_version.is_none());
    }
}
