//! Compositor profiles and quirks
//!
//! This module defines known compositor behaviors and recommended
//! configurations for optimal operation with each desktop environment.
//!
//! Profiles are generated based on both compositor detection and OS/platform
//! detection. This allows us to handle platform-specific quirks like the
//! AVC444 blur issue on RHEL 9.

use super::{
    capabilities::{BufferType, CaptureBackend, CompositorType},
    probing::detect_os_release,
};

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

    /// AVC444 codec produces blurry/corrupted output on this platform
    ///
    /// This quirk is set for platforms where AVC444 (H.264 YUV444) encoding
    /// is known to produce visual artifacts. When this quirk is present,
    /// the EGFX handler will force AVC420 even if the client supports AVC444.
    ///
    /// Known affected platforms:
    /// - RHEL 9.x (GNOME 40 + Mesa 22.x + older driver stack)
    ///
    /// See: docs/support-matrix.md for full platform compatibility details.
    Avc444Unreliable,

    /// Clipboard synchronization is not available
    ///
    /// This quirk is set when the XDG Desktop Portal does not support
    /// clipboard access (Portal version < 2). When this quirk is present,
    /// clipboard features are disabled at startup rather than failing
    /// at runtime.
    ///
    /// Known affected platforms:
    /// - RHEL 9.x (Portal v1, GNOME 40)
    /// - Any system with Portal < version 2
    ClipboardUnavailable,

    /// Portal clipboard operations crash xdg-desktop-portal-kde
    ///
    /// This quirk is set for KDE Plasma when Klipper is active. The portal's
    /// clipboard handling crashes in QMimeData::data() during Wayland callbacks,
    /// likely due to race conditions with Klipper's aggressive ownership takeover.
    ///
    /// When this quirk is present, clipboard is disabled until Klipper cooperation
    /// mode is implemented (v1.3.0+). The crash occurs in portal-kde, not our code,
    /// but our SetSelection operations trigger it.
    ///
    /// Known affected platforms:
    /// - KDE Plasma 6.x with Klipper enabled
    /// - KDE Plasma 5.x with Klipper enabled
    ///
    /// See: docs/analysis/KDE-PORTAL-CLIPBOARD-CRASH-ANALYSIS.md
    KdePortalClipboardUnstable,
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
            Self::Avc444Unreliable => "AVC444 codec produces artifacts (use AVC420)",
            Self::ClipboardUnavailable => "Clipboard sync not available (Portal v1)",
            Self::KdePortalClipboardUnstable => {
                "Portal clipboard crashes on KDE (Klipper cooperation pending)"
            }
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
    ///
    /// This profile handles GNOME-specific quirks including platform-specific
    /// issues like the AVC444 blur on RHEL 9.
    fn gnome_profile(version: Option<&str>) -> Self {
        let is_modern = version
            .and_then(|v| v.split('.').next())
            .and_then(|major| major.parse::<u32>().ok())
            .is_some_and(|major| major >= 45);

        // Detect OS for platform-specific quirks
        let os_release = detect_os_release();

        // Build quirk list based on compositor and platform
        let mut quirks = vec![Quirk::RequiresWaylandSession, Quirk::RestartCaptureOnResize];

        // RHEL 9 specific quirks
        if let Some(ref os) = os_release {
            if os.is_rhel9() {
                // AVC444 produces blurry output on RHEL 9 due to the combination
                // of GNOME 40 + Mesa 22.x + older driver stack. Force AVC420.
                quirks.push(Quirk::Avc444Unreliable);

                // Portal v1 on RHEL 9/GNOME 40 doesn't support clipboard
                quirks.push(Quirk::ClipboardUnavailable);

                tracing::info!(
                    "RHEL 9 detected ({}) - applying platform quirks: AVC444 disabled, clipboard unavailable",
                    os.version_id
                );
            }
        }

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
            quirks,
            recommended_fps_cap: 30,
            portal_timeout_ms: 30000,
        }
    }

    /// KDE Plasma / KWin profile
    fn kde_profile(version: Option<&str>) -> Self {
        let is_plasma6 = version
            .and_then(|v| v.split('.').next())
            .and_then(|major| major.parse::<u32>().ok())
            .is_some_and(|major| major >= 6);

        // Build quirks list
        let quirks = if is_plasma6 {
            vec![]
        } else {
            vec![Quirk::MultiMonitorPositionQuirk]
        };

        // v1.3.0: Klipper cooperation mode implemented - clipboard enabled
        // Cooperation strategy works WITH Klipper via bidirectional D-Bus sync
        // instead of fighting for ownership. See docs/decisions/CLIPBOARD-KDE-STRATEGIC-DECISION.md
        //
        // Old behavior (v1.2.11): Clipboard disabled due to crashes
        // New behavior (v1.3.0): Cooperation mode handles Klipper interaction
        //
        // quirks.push(Quirk::KdePortalClipboardUnstable);  // REMOVED - cooperation handles this

        tracing::info!(
            "KDE Plasma {} detected - clipboard enabled with cooperation mode",
            version.unwrap_or("unknown")
        );

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
            quirks,
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
