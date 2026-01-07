//! Compositor profiles and quirks
//!
//! This module defines known compositor behaviors and recommended
//! configurations for optimal operation with each desktop environment.

use super::capabilities::{BufferType, CaptureBackend, CompositorType};

/// Known compositor quirks that require workarounds
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Quirk {
    /// Must be running in a Wayland session (no X11 fallback)
    RequiresWaylandSession,

    /// Portal permission dialogs are slow/blocking
    SlowPortalPermissions,

    /// DMA-BUF support is unreliable
    PoorDmaBufSupport,

    /// Cursor compositing needed (no metadata cursor)
    NeedsExplicitCursorComposite,

    /// Frame timing is inconsistent
    InconsistentFrameTiming,

    /// Portal may not report accurate screen size
    InaccurateScreenSize,

    /// Need to restart capture after resolution change
    RestartCaptureOnResize,

    /// Clipboard paste requires additional handshake
    ClipboardExtraHandshake,

    /// Multi-monitor positions may be incorrect
    MultiMonitorPositionQuirk,

    /// GPU buffer formats may be limited
    LimitedBufferFormats,

    /// Portal session may timeout during idle
    SessionTimeoutOnIdle,

    /// Color space may not be correctly reported
    ColorSpaceQuirk,
}

impl Quirk {
    /// Get a human-readable description
    pub fn description(&self) -> &'static str {
        match self {
            Self::RequiresWaylandSession => "Requires Wayland session",
            Self::SlowPortalPermissions => "Slow portal permission dialogs",
            Self::PoorDmaBufSupport => "Unreliable DMA-BUF support",
            Self::NeedsExplicitCursorComposite => "Needs explicit cursor compositing",
            Self::InconsistentFrameTiming => "Inconsistent frame timing",
            Self::InaccurateScreenSize => "May report inaccurate screen size",
            Self::RestartCaptureOnResize => "Restart capture after resize",
            Self::ClipboardExtraHandshake => "Clipboard needs extra handshake",
            Self::MultiMonitorPositionQuirk => "Multi-monitor positions may be incorrect",
            Self::LimitedBufferFormats => "Limited GPU buffer format support",
            Self::SessionTimeoutOnIdle => "Portal session may timeout when idle",
            Self::ColorSpaceQuirk => "Color space may be incorrect",
        }
    }
}

/// Compositor profile with recommended settings
#[derive(Debug, Clone)]
pub struct CompositorProfile {
    /// Detected compositor type
    pub compositor: CompositorType,

    /// Known supported Wayland protocols
    pub wayland_protocols: Vec<String>,

    /// Portal backend identifier
    pub portal_backend: Option<String>,

    /// Recommended capture backend
    pub recommended_capture: CaptureBackend,

    /// Recommended buffer type
    pub recommended_buffer_type: BufferType,

    /// Whether compositor provides damage hints
    pub supports_damage_hints: bool,

    /// Whether explicit sync is supported
    pub supports_explicit_sync: bool,

    /// Known quirks that need workarounds
    pub quirks: Vec<Quirk>,

    /// Recommended frame rate cap (0 = no cap)
    pub recommended_fps_cap: u32,

    /// Recommended portal timeout (milliseconds)
    pub portal_timeout_ms: u64,
}

impl Default for CompositorProfile {
    fn default() -> Self {
        Self {
            compositor: CompositorType::Unknown { session_info: None },
            wayland_protocols: vec![],
            portal_backend: None,
            recommended_capture: CaptureBackend::Portal,
            recommended_buffer_type: BufferType::Any,
            supports_damage_hints: false,
            supports_explicit_sync: false,
            quirks: vec![],
            recommended_fps_cap: 30,
            portal_timeout_ms: 30000,
        }
    }
}

impl CompositorProfile {
    /// Create a profile for a specific compositor type
    pub fn for_compositor(compositor: &CompositorType) -> Self {
        match compositor {
            CompositorType::Gnome { version } => Self::gnome_profile(version.as_deref()),
            CompositorType::Kde { version } => Self::kde_profile(version.as_deref()),
            CompositorType::Sway { version } => Self::sway_profile(version.as_deref()),
            CompositorType::Hyprland { version } => Self::hyprland_profile(version.as_deref()),
            CompositorType::Weston => Self::weston_profile(),
            CompositorType::Cosmic => Self::cosmic_profile(),
            CompositorType::Wlroots { name } => Self::wlroots_profile(name),
            CompositorType::Unknown { session_info } => {
                Self::unknown_profile(session_info.as_deref())
            }
        }
    }

    /// GNOME Shell / Mutter profile
    fn gnome_profile(version: Option<&str>) -> Self {
        let is_modern = version
            .and_then(|v| v.split('.').next())
            .and_then(|major| major.parse::<u32>().ok())
            .map(|major| major >= 45)
            .unwrap_or(false);

        Self {
            compositor: CompositorType::Gnome {
                version: version.map(String::from),
            },
            wayland_protocols: vec![
                "wl_compositor".to_string(),
                "xdg_wm_base".to_string(),
                "org_gnome_mutter_screen_cast".to_string(),
            ],
            portal_backend: Some("gnome".to_string()),
            recommended_capture: CaptureBackend::Portal,
            // GNOME works best with MemFd (shm) - DMA-BUF support varies
            recommended_buffer_type: BufferType::MemFd,
            supports_damage_hints: is_modern, // GNOME 45+ has better damage tracking
            supports_explicit_sync: false,    // Not yet in GNOME
            quirks: vec![Quirk::RequiresWaylandSession, Quirk::RestartCaptureOnResize],
            recommended_fps_cap: 30,
            portal_timeout_ms: 30000,
        }
    }

    /// KDE Plasma / KWin profile
    fn kde_profile(version: Option<&str>) -> Self {
        let is_plasma6 = version
            .and_then(|v| v.split('.').next())
            .and_then(|major| major.parse::<u32>().ok())
            .map(|major| major >= 6)
            .unwrap_or(false);

        Self {
            compositor: CompositorType::Kde {
                version: version.map(String::from),
            },
            wayland_protocols: vec![
                "wl_compositor".to_string(),
                "xdg_wm_base".to_string(),
                "org_kde_kwin_dpms".to_string(),
            ],
            portal_backend: Some("kde".to_string()),
            recommended_capture: CaptureBackend::Portal,
            // KDE has excellent DMA-BUF support
            recommended_buffer_type: BufferType::DmaBuf,
            supports_damage_hints: is_plasma6, // Plasma 6 has improved damage
            supports_explicit_sync: is_plasma6,
            quirks: if is_plasma6 {
                vec![]
            } else {
                vec![Quirk::MultiMonitorPositionQuirk]
            },
            recommended_fps_cap: 30,
            portal_timeout_ms: 30000,
        }
    }

    /// Sway / wlroots profile
    fn sway_profile(version: Option<&str>) -> Self {
        Self {
            compositor: CompositorType::Sway {
                version: version.map(String::from),
            },
            wayland_protocols: vec![
                "wl_compositor".to_string(),
                "xdg_wm_base".to_string(),
                "zwlr_screencopy_manager_v1".to_string(),
                "zwlr_export_dmabuf_manager_v1".to_string(),
            ],
            portal_backend: Some("wlr".to_string()),
            // Sway supports direct screencopy for lowest latency
            recommended_capture: CaptureBackend::WlrScreencopy,
            recommended_buffer_type: BufferType::DmaBuf,
            supports_damage_hints: true, // wlroots has damage tracking
            supports_explicit_sync: true,
            quirks: vec![
                Quirk::NeedsExplicitCursorComposite, // Cursor not in screencopy by default
            ],
            recommended_fps_cap: 60, // Sway users often want higher FPS
            portal_timeout_ms: 15000,
        }
    }

    /// Hyprland profile
    fn hyprland_profile(version: Option<&str>) -> Self {
        Self {
            compositor: CompositorType::Hyprland {
                version: version.map(String::from),
            },
            wayland_protocols: vec![
                "wl_compositor".to_string(),
                "xdg_wm_base".to_string(),
                "zwlr_screencopy_manager_v1".to_string(),
                "hyprland_toplevel_export_manager_v1".to_string(),
            ],
            portal_backend: Some("wlr".to_string()),
            recommended_capture: CaptureBackend::WlrScreencopy,
            recommended_buffer_type: BufferType::DmaBuf,
            supports_damage_hints: true,
            supports_explicit_sync: true,
            quirks: vec![
                Quirk::NeedsExplicitCursorComposite,
                Quirk::InconsistentFrameTiming, // Can be choppy with animations
            ],
            recommended_fps_cap: 60,
            portal_timeout_ms: 15000,
        }
    }

    /// Weston reference compositor profile
    fn weston_profile() -> Self {
        Self {
            compositor: CompositorType::Weston,
            wayland_protocols: vec!["wl_compositor".to_string(), "xdg_wm_base".to_string()],
            portal_backend: None,
            recommended_capture: CaptureBackend::Portal,
            recommended_buffer_type: BufferType::MemFd,
            supports_damage_hints: false,
            supports_explicit_sync: false,
            quirks: vec![Quirk::LimitedBufferFormats, Quirk::InaccurateScreenSize],
            recommended_fps_cap: 30,
            portal_timeout_ms: 30000,
        }
    }

    /// Cosmic compositor profile
    fn cosmic_profile() -> Self {
        Self {
            compositor: CompositorType::Cosmic,
            wayland_protocols: vec![
                "wl_compositor".to_string(),
                "xdg_wm_base".to_string(),
                "cosmic_screencopy_manager_v1".to_string(),
            ],
            portal_backend: Some("cosmic".to_string()),
            recommended_capture: CaptureBackend::Portal,
            recommended_buffer_type: BufferType::DmaBuf,
            supports_damage_hints: true,
            supports_explicit_sync: true,
            quirks: vec![], // Cosmic is modern and well-behaved
            recommended_fps_cap: 60,
            portal_timeout_ms: 15000,
        }
    }

    /// Generic wlroots-based compositor profile
    fn wlroots_profile(name: &str) -> Self {
        Self {
            compositor: CompositorType::Wlroots {
                name: name.to_string(),
            },
            wayland_protocols: vec![
                "wl_compositor".to_string(),
                "xdg_wm_base".to_string(),
                "zwlr_screencopy_manager_v1".to_string(),
            ],
            portal_backend: Some("wlr".to_string()),
            recommended_capture: CaptureBackend::WlrScreencopy,
            recommended_buffer_type: BufferType::DmaBuf,
            supports_damage_hints: true,
            supports_explicit_sync: true,
            quirks: vec![Quirk::NeedsExplicitCursorComposite],
            recommended_fps_cap: 30,
            portal_timeout_ms: 15000,
        }
    }

    /// Unknown compositor profile (conservative defaults)
    fn unknown_profile(session_info: Option<&str>) -> Self {
        Self {
            compositor: CompositorType::Unknown {
                session_info: session_info.map(String::from),
            },
            wayland_protocols: vec![],
            portal_backend: None,
            recommended_capture: CaptureBackend::Portal, // Safest option
            recommended_buffer_type: BufferType::MemFd,  // Most compatible
            supports_damage_hints: false,
            supports_explicit_sync: false,
            quirks: vec![
                Quirk::PoorDmaBufSupport, // Don't assume DMA-BUF works
                Quirk::NeedsExplicitCursorComposite,
            ],
            recommended_fps_cap: 30,
            portal_timeout_ms: 60000, // Longer timeout for unknown compositors
        }
    }

    /// Check if a specific quirk is present
    pub fn has_quirk(&self, quirk: &Quirk) -> bool {
        self.quirks.contains(quirk)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_gnome_profile() {
        let profile = CompositorProfile::gnome_profile(Some("46.0"));
        assert_eq!(profile.recommended_buffer_type, BufferType::MemFd);
        assert!(profile.supports_damage_hints);
        assert!(profile.has_quirk(&Quirk::RequiresWaylandSession));
    }

    #[test]
    fn test_kde_profile() {
        let profile = CompositorProfile::kde_profile(Some("6.0"));
        assert_eq!(profile.recommended_buffer_type, BufferType::DmaBuf);
        assert!(profile.supports_explicit_sync);
    }

    #[test]
    fn test_sway_profile() {
        let profile = CompositorProfile::sway_profile(Some("1.9"));
        assert_eq!(profile.recommended_capture, CaptureBackend::WlrScreencopy);
        assert!(profile.supports_damage_hints);
    }

    #[test]
    fn test_unknown_profile() {
        let profile = CompositorProfile::unknown_profile(None);
        assert_eq!(profile.recommended_capture, CaptureBackend::Portal);
        assert!(profile.has_quirk(&Quirk::PoorDmaBufSupport));
    }

    #[test]
    fn test_for_compositor() {
        let gnome = CompositorType::Gnome {
            version: Some("46.0".to_string()),
        };
        let profile = CompositorProfile::for_compositor(&gnome);
        assert_eq!(profile.portal_backend, Some("gnome".to_string()));
    }
}
