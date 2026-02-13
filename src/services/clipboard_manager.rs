//! System Clipboard Manager Detection
//!
//! **Execution Path:** D-Bus probing + process detection
//! **Status:** Active (v1.0.0+)
//! **Platform:** Universal (detects Klipper, CopyQ, Diodon, GNOME Shell, wl-clipboard)
//!
//! Detects system clipboard managers (Klipper, CopyQ, etc.) for integration strategy selection.
//! **Not to be confused with** the server's clipboard orchestrator (`ClipboardOrchestrator`).

use std::time::Duration;

use tracing::{debug, info, warn};
use zbus::Connection;

use crate::compositor::CompositorType;

/// System clipboard manager detection result
///
/// Contains metadata about a detected OS-level clipboard manager (Klipper, CopyQ, etc.).
/// **Not to be confused with:** `ClipboardOrchestrator` (server's clipboard coordinator)
///
/// # See Also
///
/// - [`SystemClipboardManagerKind`] - Enum of manager types (to be renamed)
/// - [`ClipboardOrchestrator`] - Server's clipboard coordinator
#[derive(Debug, Clone)]
pub struct DetectedSystemClipboardManager {
    /// Type of clipboard manager detected
    pub manager_type: SystemClipboardManagerKind,

    /// Manager version string (if available)
    pub version: Option<String>,

    /// Whether D-Bus interface is responsive
    pub dbus_responsive: bool,

    /// Observed delay before manager takes over clipboard (empirical)
    pub takeover_delay_ms: Option<u64>,

    /// MIME type signatures this manager adds
    pub mime_signatures: Vec<String>,

    /// Whether manager exposes lock/unlock API
    pub lock_api_available: bool,

    /// Additional notes about this manager's behavior
    pub notes: Vec<String>,
}

/// Kind of system clipboard manager
///
/// Represents OS-level clipboard managers (not the server's clipboard orchestrator).
/// Standard Rust pattern for type enums (see std::io::ErrorKind).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SystemClipboardManagerKind {
    /// KDE Klipper (Plasma integrated clipboard)
    Klipper {
        /// Plasma desktop version
        plasma_version: String,
        /// Whether D-Bus API is available
        has_dbus_api: bool,
    },

    /// CopyQ (third-party clipboard manager)
    CopyQ {
        /// CopyQ version
        version: String,
    },

    /// Diodon (lightweight clipboard manager)
    Diodon,

    /// GNOME Shell built-in clipboard (no separate manager)
    GnomeShell,

    /// wl-clipboard tools (wl-copy/wl-paste) - not persistent
    WlClipboard,

    /// No clipboard manager detected
    None,

    /// Unknown/unrecognized clipboard manager
    Unknown,
}

impl DetectedSystemClipboardManager {
    pub async fn detect(compositor: &CompositorType) -> Self {
        info!("═══════════════════════════════════════════════════════════════");
        info!("  Clipboard Manager Detection");
        info!("═══════════════════════════════════════════════════════════════");

        // Try D-Bus service detection first
        match Self::detect_via_dbus().await {
            Ok(manager) if manager.manager_type != SystemClipboardManagerKind::None => {
                info!("  Detected via D-Bus: {:?}", manager.manager_type);
                Self::log_manager_info(&manager);
                info!("═══════════════════════════════════════════════════════════════");
                return manager;
            }
            Ok(_) => {
                debug!("  No D-Bus clipboard manager found");
            }
            Err(e) => {
                debug!("  D-Bus detection failed: {}", e);
            }
        }

        // Try process detection
        if let Some(manager) = Self::detect_via_process().await {
            info!("  Detected via process: {:?}", manager.manager_type);
            Self::log_manager_info(&manager);
            info!("═══════════════════════════════════════════════════════════════");
            return manager;
        }

        // Infer from compositor
        let manager = Self::infer_from_compositor(compositor);
        info!("  Inferred from compositor: {:?}", manager.manager_type);
        Self::log_manager_info(&manager);

        info!("═══════════════════════════════════════════════════════════════");
        manager
    }

    async fn detect_via_dbus() -> Result<Self, zbus::Error> {
        let connection = Connection::session().await?;

        if Self::probe_dbus_service(&connection, "org.kde.klipper").await {
            return Ok(Self::detect_klipper(&connection).await);
        }

        if Self::probe_dbus_service(&connection, "org.copyq.CopyQ").await {
            return Ok(Self::detect_copyq(&connection).await);
        }

        if Self::probe_dbus_service(&connection, "org.diodon.Diodon").await {
            return Ok(Self::detect_diodon(&connection).await);
        }

        Ok(Self::none())
    }

    async fn probe_dbus_service(connection: &Connection, service_name: &str) -> bool {
        debug!("  Probing D-Bus service: {}", service_name);

        let proxy = match zbus::fdo::DBusProxy::new(connection).await {
            Ok(p) => p,
            Err(e) => {
                debug!("    Failed to create DBus proxy: {}", e);
                return false;
            }
        };

        match proxy.list_names().await {
            Ok(names) => {
                let found = names.iter().any(|name| name.as_str() == service_name);
                if found {
                    debug!("    Service {} found", service_name);
                } else {
                    debug!("    Service {} not found", service_name);
                }
                found
            }
            Err(e) => {
                debug!("    Failed to list D-Bus names: {}", e);
                false
            }
        }
    }

    async fn detect_klipper(connection: &Connection) -> Self {
        info!("  Klipper service found - querying details...");

        let responsive = Self::test_klipper_responsive(connection).await;

        let plasma_version = Self::query_plasma_version(connection)
            .await
            .unwrap_or_else(|| "unknown".to_string());

        // Empirical takeover timing: ~1000ms (from v1.2.8 logs)
        let takeover_delay_ms = Some(1000);

        // Known MIME signatures Klipper adds
        let mime_signatures = vec![
            "application/x-kde-onlyReplaceEmpty".to_string(),
            "application/x-kde-cutselection".to_string(),
        ];

        let notes = vec![
            "Takes ownership ~1s after SetSelection".to_string(),
            "Replaces content with text-only representation".to_string(),
            "Uses ext-data-control-v1 to monitor all changes".to_string(),
        ];

        Self {
            manager_type: SystemClipboardManagerKind::Klipper {
                plasma_version: plasma_version.clone(),
                has_dbus_api: responsive,
            },
            version: Some(plasma_version),
            dbus_responsive: responsive,
            takeover_delay_ms,
            mime_signatures,
            lock_api_available: false, // Internal only, not exposed
            notes,
        }
    }

    async fn test_klipper_responsive(connection: &Connection) -> bool {
        debug!("    Testing Klipper D-Bus responsiveness...");

        // Try to call getClipboardContents with timeout
        let result = tokio::time::timeout(
            Duration::from_millis(500),
            connection.call_method(
                Some("org.kde.klipper"),
                "/klipper",
                Some("org.kde.klipper.klipper"),
                "getClipboardContents",
                &(),
            ),
        )
        .await;

        match result {
            Ok(Ok(_)) => {
                debug!("    Klipper D-Bus interface responsive");
                true
            }
            Ok(Err(e)) => {
                warn!("    Klipper D-Bus call failed: {}", e);
                false
            }
            Err(_) => {
                warn!("    Klipper D-Bus call timed out");
                false
            }
        }
    }

    async fn query_plasma_version(connection: &Connection) -> Option<String> {
        debug!("    Querying Plasma version...");

        // Try plasmashell version via D-Bus properties
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
            // Parse the variant response - D-Bus returns a Variant containing the string
            if let Ok(variant) = msg.body().deserialize::<zbus::zvariant::OwnedValue>() {
                // Try to extract string from the variant
                match String::try_from(variant) {
                    Ok(version) => {
                        debug!("    Plasma version from D-Bus: {}", version);
                        return Some(version);
                    }
                    Err(e) => {
                        debug!("    Failed to parse version from D-Bus response: {}", e);
                    }
                }
            }
        }

        // Fallback: KDE_SESSION_VERSION env var
        if let Ok(version) = std::env::var("KDE_SESSION_VERSION") {
            debug!("    Plasma version from env: {}", version);
            return Some(version);
        }

        debug!("    Could not determine Plasma version");
        None
    }

    async fn detect_copyq(connection: &Connection) -> Self {
        info!("  CopyQ service found - querying version...");

        // Try to get version via D-Bus
        let version = Self::query_copyq_version(connection)
            .await
            .unwrap_or_else(|| "unknown".to_string());

        Self {
            manager_type: SystemClipboardManagerKind::CopyQ {
                version: version.clone(),
            },
            version: Some(version),
            dbus_responsive: true,
            takeover_delay_ms: None, // Unknown behavior
            mime_signatures: vec![],
            lock_api_available: false,
            notes: vec!["Third-party clipboard manager - behavior unknown".to_string()],
        }
    }

    async fn query_copyq_version(_connection: &Connection) -> Option<String> {
        debug!("    Querying CopyQ version...");

        // CopyQ doesn't expose version via D-Bus in a standard way
        // Would need to check process command line or config
        // For now, just mark as unknown
        None
    }

    async fn detect_diodon(_connection: &Connection) -> Self {
        info!("  Diodon service found");

        Self {
            manager_type: SystemClipboardManagerKind::Diodon,
            version: None,
            dbus_responsive: true,
            takeover_delay_ms: None,
            mime_signatures: vec![],
            lock_api_available: false,
            notes: vec!["Lightweight clipboard manager".to_string()],
        }
    }

    async fn detect_via_process() -> Option<Self> {
        debug!("  Checking for clipboard manager processes...");

        // wl-clipboard is a CLI tool, not a persistent clipboard manager
        if Self::check_process("wl-copy").await {
            debug!("    wl-clipboard tools detected");
            return Some(Self {
                manager_type: SystemClipboardManagerKind::WlClipboard,
                version: None,
                dbus_responsive: false,
                takeover_delay_ms: None,
                mime_signatures: vec![],
                lock_api_available: false,
                notes: vec!["Command-line tools, not persistent manager".to_string()],
            });
        }

        None
    }

    async fn check_process(process_name: &str) -> bool {
        // Use pgrep to check for process
        match tokio::process::Command::new("pgrep")
            .arg("-x")
            .arg(process_name)
            .output()
            .await
        {
            Ok(output) => output.status.success(),
            Err(_) => false,
        }
    }

    fn infer_from_compositor(compositor: &CompositorType) -> Self {
        match compositor {
            CompositorType::Gnome { .. } => Self {
                manager_type: SystemClipboardManagerKind::GnomeShell,
                version: None,
                dbus_responsive: false,
                takeover_delay_ms: None,
                mime_signatures: vec![],
                lock_api_available: false,
                notes: vec!["GNOME Shell built-in clipboard".to_string()],
            },

            _ => Self::none(),
        }
    }

    fn none() -> Self {
        Self {
            manager_type: SystemClipboardManagerKind::None,
            version: None,
            dbus_responsive: false,
            takeover_delay_ms: None,
            mime_signatures: vec![],
            lock_api_available: false,
            notes: vec![],
        }
    }

    fn log_manager_info(manager: &DetectedSystemClipboardManager) {
        info!("  ┌─────────────────────────────────────────────────────");
        info!("  │ Manager: {:?}", manager.manager_type);
        if let Some(ref ver) = manager.version {
            info!("  │ Version: {}", ver);
        }
        info!("  │ D-Bus Responsive: {}", manager.dbus_responsive);
        if let Some(delay) = manager.takeover_delay_ms {
            info!("  │ Takeover Delay: ~{}ms", delay);
        }
        if !manager.mime_signatures.is_empty() {
            info!("  │ Signatures: {:?}", manager.mime_signatures);
        }
        for note in &manager.notes {
            info!("  │   • {}", note);
        }
        info!("  └─────────────────────────────────────────────────────");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_clipboard_manager_type_equality() {
        let klipper1 = SystemClipboardManagerKind::Klipper {
            plasma_version: "6.5.5".to_string(),
            has_dbus_api: true,
        };

        let klipper2 = SystemClipboardManagerKind::Klipper {
            plasma_version: "6.5.5".to_string(),
            has_dbus_api: true,
        };

        assert_eq!(klipper1, klipper2);

        let gnome = SystemClipboardManagerKind::GnomeShell;
        assert_ne!(klipper1, gnome);
    }

    #[tokio::test]
    async fn test_none_clipboard_manager() {
        let info = DetectedSystemClipboardManager::none();
        assert_eq!(info.manager_type, SystemClipboardManagerKind::None);
        assert!(!info.dbus_responsive);
        assert_eq!(info.version, None);
    }

    #[test]
    fn test_infer_from_compositor() {
        let gnome_info =
            DetectedSystemClipboardManager::infer_from_compositor(&CompositorType::Gnome {
                version: Some("46.0".to_string()),
            });
        assert_eq!(
            gnome_info.manager_type,
            SystemClipboardManagerKind::GnomeShell
        );

        let sway_info =
            DetectedSystemClipboardManager::infer_from_compositor(&CompositorType::Sway {
                version: Some("1.9".to_string()),
            });
        assert_eq!(sway_info.manager_type, SystemClipboardManagerKind::None);
    }
}
