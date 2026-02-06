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
    /// Probe display capabilities
    pub async fn probe() -> DisplayCapabilities {
        info!("Probing display capabilities...");

        let compositor = Self::detect_compositor();
        let portal = Self::probe_portal().await;
        let quirks = Self::detect_quirks(&compositor);

        // Determine service level
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
        // Check XDG_CURRENT_DESKTOP
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
        // TODO: Actually query D-Bus for portal version and capabilities
        // For now, make reasonable assumptions based on environment

        let desktop = std::env::var("XDG_CURRENT_DESKTOP")
            .unwrap_or_default()
            .to_lowercase();

        // Estimate portal version based on desktop environment
        // GNOME 44+ and KDE 5.27+ have portal v4 with clipboard support
        let (version, supports_clipboard, supports_restore_tokens) = if desktop.contains("gnome") {
            // Modern GNOME likely has v4+
            (4, true, true)
        } else if desktop.contains("kde") || desktop.contains("plasma") {
            // Modern KDE likely has v4+
            (4, true, true)
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

        // RHEL 9 quirk
        if Self::is_rhel9() {
            quirks.push("rhel9_portal_v1".into());
            quirks.push("no_clipboard".into());
        }

        // Compositor-specific quirks
        match compositor.compositor_type.as_str() {
            "mutter" => {
                // GNOME/Mutter specific quirks would go here
            }
            "kwin" => {
                // KDE/KWin specific quirks would go here
            }
            "wlroots" => {
                // wlroots-based compositor quirks
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
        // Check for critical failures
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

        // Check for degradations
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
