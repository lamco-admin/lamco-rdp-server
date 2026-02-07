//! Display capability probe
//!
//! Detects compositor and portal capabilities for screen capture.

use serde::{Deserialize, Serialize};
use tracing::{debug, info};

use crate::capabilities::state::ServiceLevel;

/// Display capabilities
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DisplayCapabilities {
    /// Compositor information
    pub compositor: CompositorInfo,
    /// Portal information
    pub portal: PortalInfo,
    /// Platform quirks that apply
    pub quirks: Vec<String>,
    /// Overall service level
    pub service_level: ServiceLevel,
    /// Reason for degradation (if applicable)
    pub degradation_reason: Option<String>,
    /// Reason for unavailability (if applicable)
    pub unavailable_reason: Option<String>,
}

/// Compositor information
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CompositorInfo {
    /// Compositor name (e.g., "GNOME Shell", "KWin", "Sway")
    pub name: String,
    /// Version if available
    pub version: Option<String>,
    /// Compositor type
    pub compositor_type: String,
}

/// XDG Desktop Portal information
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PortalInfo {
    /// Portal version
    pub version: u32,
    /// Supports ScreenCast interface
    pub supports_screencast: bool,
    /// Supports RemoteDesktop interface
    pub supports_remote_desktop: bool,
    /// Supports clipboard in RemoteDesktop (v4+)
    pub supports_clipboard: bool,
    /// Supports restore tokens for session persistence (v4+)
    pub supports_restore_tokens: bool,
}

/// Display probe
pub struct DisplayProbe;

impl DisplayProbe {
    pub async fn probe() -> DisplayCapabilities {
        info!("Probing display capabilities...");

        let compositor = Self::detect_compositor();
        let portal = Self::probe_portal().await;
        let quirks = Self::detect_quirks(&compositor);

        let (service_level, degradation_reason, unavailable_reason) =
            Self::determine_service_level(&portal, &quirks);

        debug!(
            "Display probe: compositor={}, portal_v{}, service_level={:?}",
            compositor.name, portal.version, service_level
        );

        DisplayCapabilities {
            compositor,
            portal,
            quirks,
            service_level,
            degradation_reason,
            unavailable_reason,
        }
    }

    fn detect_compositor() -> CompositorInfo {
        let desktop = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default();
        let session_type = std::env::var("XDG_SESSION_TYPE").unwrap_or_default();

        let (name, compositor_type) = if desktop.to_lowercase().contains("gnome") {
            ("GNOME Shell".into(), "mutter".into())
        } else if desktop.to_lowercase().contains("kde")
            || desktop.to_lowercase().contains("plasma")
        {
            ("KDE Plasma".into(), "kwin".into())
        } else if desktop.to_lowercase().contains("sway") {
            ("Sway".into(), "wlroots".into())
        } else if desktop.to_lowercase().contains("hyprland") {
            ("Hyprland".into(), "wlroots".into())
        } else if desktop.to_lowercase().contains("wlroots") {
            ("wlroots-based".into(), "wlroots".into())
        } else if session_type == "wayland" {
            ("Unknown Wayland".into(), "unknown".into())
        } else if session_type == "x11" {
            ("X11".into(), "x11".into())
        } else {
            ("Unknown".into(), "unknown".into())
        };

        CompositorInfo {
            name,
            version: None, // Would need D-Bus queries for version
            compositor_type,
        }
    }

    async fn probe_portal() -> PortalInfo {
        if let Some(info) = Self::query_portal_version_dbus().await {
            debug!("Portal version detected via D-Bus: v{}", info.version);
            return info;
        }

        Self::probe_portal_from_environment()
    }

    async fn query_portal_version_dbus() -> Option<PortalInfo> {
        use zbus::Connection;

        let connection = Connection::session().await.ok()?;

        let message = connection
            .call_method(
                Some("org.freedesktop.portal.Desktop"),
                "/org/freedesktop/portal/desktop",
                Some("org.freedesktop.DBus.Properties"),
                "Get",
                &("org.freedesktop.portal.RemoteDesktop", "version"),
            )
            .await
            .ok()?;

        let version: u32 = message
            .body()
            .deserialize::<zbus::zvariant::OwnedValue>()
            .ok()?
            .try_into()
            .ok()?;

        debug!("RemoteDesktop Portal version from D-Bus: {}", version);

        // Version 2+ supports clipboard (added in Portal v2)
        // Version 4+ supports restore tokens
        let supports_clipboard = version >= 2;
        let supports_restore_tokens = version >= 4;

        Some(PortalInfo {
            version,
            supports_screencast: true,
            supports_remote_desktop: true,
            supports_clipboard,
            supports_restore_tokens,
        })
    }

    fn probe_portal_from_environment() -> PortalInfo {
        let desktop = std::env::var("XDG_CURRENT_DESKTOP")
            .unwrap_or_default()
            .to_lowercase();

        // Modern Flatpak runtimes have Portal v2+
        let is_flatpak =
            std::env::var("FLATPAK_ID").is_ok() || std::path::Path::new("/.flatpak-info").exists();

        // GNOME 44+ and KDE 5.27+ have portal v4 with clipboard support
        let (version, supports_clipboard, supports_restore_tokens) = if is_flatpak {
            // Flatpak with org.freedesktop.Platform 24.08+ has Portal v2+ clipboard support
            // Assume modern Flatpak = modern portal (v4)
            debug!("Flatpak detected, assuming Portal v4 with clipboard support");
            (4, true, true)
        } else if desktop.contains("gnome") {
            // Modern GNOME likely has v4+
            (4, true, true)
        } else if desktop.contains("kde") || desktop.contains("plasma") {
            // Modern KDE likely has v4+
            (4, true, true)
        } else if desktop.is_empty() {
            // Unknown desktop but we're in a graphical session
            // Try optimistic approach - assume Portal v2 with clipboard
            debug!("Unknown desktop environment, assuming Portal v2 with clipboard");
            (2, true, false)
        } else {
            // Assume older portal for other desktops
            (1, false, false)
        };

        // Check for RHEL 9 which has portal quirks
        let is_rhel9 = Self::is_rhel9();
        let (version, supports_clipboard) = if is_rhel9 {
            // RHEL 9 has portal v1 without clipboard
            (1, false)
        } else {
            (version, supports_clipboard)
        };

        PortalInfo {
            version,
            supports_screencast: true, // Assume available if session is graphical
            supports_remote_desktop: true,
            supports_clipboard,
            supports_restore_tokens,
        }
    }

    fn is_rhel9() -> bool {
        if let Ok(release) = std::fs::read_to_string("/etc/os-release") {
            let release_lower = release.to_lowercase();
            if (release_lower.contains("rhel") || release_lower.contains("red hat"))
                && release_lower.contains("9.")
            {
                return true;
            }
            // Also check for RHEL 9 clones
            if (release_lower.contains("rocky") || release_lower.contains("alma"))
                && release_lower.contains("9.")
            {
                return true;
            }
        }
        false
    }

    fn detect_quirks(compositor: &CompositorInfo) -> Vec<String> {
        let mut quirks = Vec::new();

        if Self::is_rhel9() {
            quirks.push("rhel9_portal_v1".into());
            quirks.push("no_clipboard".into());
        }

        match compositor.compositor_type.as_str() {
            "mutter" => {
                // GNOME/Mutter specific quirks would go here
            }
            "kwin" => {
                // KDE/KWin specific quirks would go here
            }
            "wlroots" => {
                quirks.push("wlr_screencopy".into());
            }
            _ => {}
        }

        quirks
    }

    fn determine_service_level(
        portal: &PortalInfo,
        quirks: &[String],
    ) -> (ServiceLevel, Option<String>, Option<String>) {
        if !portal.supports_screencast {
            return (
                ServiceLevel::Unavailable,
                None,
                Some("ScreenCast portal not available".into()),
            );
        }

        if !portal.supports_remote_desktop {
            return (
                ServiceLevel::Unavailable,
                None,
                Some("RemoteDesktop portal not available".into()),
            );
        }

        if quirks.contains(&"no_clipboard".to_string()) {
            return (
                ServiceLevel::Degraded,
                Some("Clipboard not available (Portal v1)".into()),
                None,
            );
        }

        if !portal.supports_clipboard {
            return (
                ServiceLevel::Degraded,
                Some("Clipboard requires Portal v4+".into()),
                None,
            );
        }

        (ServiceLevel::Full, None, None)
    }
}
