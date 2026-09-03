//! Compositor profiles and quirks
//!
//! This module defines known compositor behaviors and recommended
//! configurations for optimal operation with each desktop environment.
//!
//! Profiles are generated based on both compositor detection and OS/platform
//! detection. This allows us to handle platform-specific quirks like the
//! AVC444 blur issue on RHEL 9.

use super::capabilities::{BufferType, CaptureBackend, CompositorType};

/// Known compositor quirks that require workarounds
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Quirk {
    /// Must be running in a Wayland session (no X11 fallback)
    RequiresWaylandSession,

    /// Portal permission dialogs are slow/blocking
    SlowPortalPermissions,

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

    /// ext-image-copy-capture advertised but handshake never completes
    ///
    /// This quirk is set when the compositor advertises the ext-image-copy-capture
    /// protocol via Wayland globals but does not send the required constraint events
    /// (buffer_size, shm_format, done) after session creation. This causes a
    /// permanent zero-frame stall.
    ///
    /// When this quirk is present, capture protocol selection marks
    /// ext-image-copy-capture as broken and prefers wlr-screencopy.
    ///
    /// Known affected compositors:
    /// - Hyprland 0.54+ (advertises ext-capture, doesn't send constraints)
    ExtCaptureIncomplete,

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
            Self::NeedsExplicitCursorComposite => "Needs explicit cursor compositing",
            Self::InconsistentFrameTiming => "Inconsistent frame timing",
            Self::InaccurateScreenSize => "May report inaccurate screen size",
            Self::RestartCaptureOnResize => "Restart capture after resize",
            Self::ClipboardExtraHandshake => "Clipboard needs extra handshake",
            Self::MultiMonitorPositionQuirk => "Multi-monitor positions may be incorrect",
            Self::LimitedBufferFormats => "Limited GPU buffer format support",
            Self::SessionTimeoutOnIdle => "Portal session may timeout when idle",
            Self::ColorSpaceQuirk => "Color space may be incorrect",
            Self::ExtCaptureIncomplete => {
                "ext-capture handshake incomplete (prefer wlr-screencopy)"
            }
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
            CompositorType::Niri { version } => Self::niri_profile(version.as_deref()),
            CompositorType::Smithay { name } => Self::smithay_profile(name),
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

        // Build quirk list based on compositor
        let quirks = vec![Quirk::RequiresWaylandSession, Quirk::RestartCaptureOnResize];

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
            // DMA-BUF all-zero-data bug fixed upstream in lamco-pipewire 2026-06-09
            // (MOD_LINEAR now negotiated explicitly instead of MOD_INVALID). GNOME's
            // DMA-BUF reliability still trails KDE's across driver/version combos, so
            // this stays best-effort rather than guaranteed; is_display_gpu_virgl()
            // in server/mod.rs remains the hardware-capability gate for virtual GPUs.
            recommended_buffer_type: BufferType::Any,
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
        // Hyprland 0.54.0 was observed to advertise ext-image-copy-capture but
        // not send constraint events, causing permanent zero-frame stall. The
        // protocol works fine on earlier versions (where it isn't advertised
        // at all) and is empirically verified working on Hyprland 0.55.2
        // (handshake completes in <200μs, all six events arrive — see
        // 2026-05-23 archie test record). Apply the quirk only to known-bad
        // releases; let newer ones take the ext path.
        let has_broken_ext_capture = version
            .and_then(|v| {
                let parts: Vec<&str> = v.split('.').collect();
                if parts.len() >= 2 {
                    let minor = parts[1].parse::<u32>().ok()?;
                    // Known broken: 0.54.x only (the original observation).
                    // Verified working on 0.55.2; assume 0.55+ is good.
                    Some(minor == 54)
                } else {
                    None
                }
            })
            .unwrap_or(false); // Unknown version: assume good — fall back is automatic if it fails

        let mut quirks = vec![
            Quirk::NeedsExplicitCursorComposite,
            Quirk::InconsistentFrameTiming,
        ];

        if has_broken_ext_capture {
            quirks.push(Quirk::ExtCaptureIncomplete);
            tracing::info!(
                "Hyprland {} - ext-capture handshake incomplete (known issue on 0.54.x), will prefer wlr-screencopy",
                version.unwrap_or("unknown")
            );
        }

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
            // Portal = let auto-detection pick. For modern Hyprland (0.55+)
            // this routes to ext-image-copy-capture which Hyprland implements
            // correctly. For 0.54.x the ExtCaptureIncomplete quirk above
            // forces a fallback to wlr-screencopy. For very old releases
            // that don't advertise ext at all, auto-detection naturally
            // picks wlr-screencopy.
            recommended_capture: CaptureBackend::Portal,
            recommended_buffer_type: BufferType::DmaBuf,
            supports_damage_hints: true,
            supports_explicit_sync: true,
            quirks,
            recommended_fps_cap: 60,
            portal_timeout_ms: 15000,
        }
    }

    /// Weston reference compositor profile
    ///
    /// Weston is not a practical target for this project: it isn't
    /// wlroots-based, implements no xdg-desktop-portal backend at all, and
    /// uses its own weston-output-capture protocol rather than
    /// wlr-screencopy or ext-image-copy-capture. It also ships its own
    /// FreeRDP-based weston-rdp backend for RDP *hosting*, which is
    /// architecturally non-overlapping with this project. The Portal/None
    /// combination below is therefore not a working recommendation — there
    /// is no capture path we support on Weston — it's the same conservative
    /// "safest known default" fallback unknown_profile() uses, kept honest
    /// by declaring zero protocols and zero portal backend rather than
    /// implying a real one exists. See docs/FEATURE-SUPPORT-MATRIX.md.
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

    /// Niri compositor profile (Smithay-based with wlroots-compatible protocols)
    ///
    /// Niri has no RemoteDesktop portal and none is coming (niri#390, dormant
    /// since 2024; a native xdg-desktop-portal-niri attempt was declared
    /// stalled by the maintainer in Feb 2026). It runs a private
    /// org.gnome.Mutter.ScreenCast D-Bus shim so xdg-desktop-portal-gnome
    /// treats it as Mutter, but that only ever yields ScreenCast video, never
    /// RemoteDesktop input. The working, permanent path is portal-generic /
    /// wlr-direct via niri's own wlroots-compatible protocols (zwlr_screencopy,
    /// zwlr_virtual_pointer, zwp_virtual_keyboard, ext_data_control) — same
    /// model as any other wlroots-family compositor, not a fallback. See
    /// docs/FEATURE-SUPPORT-MATRIX.md and issue #64.
    fn niri_profile(version: Option<&str>) -> Self {
        Self {
            compositor: CompositorType::Niri {
                version: version.map(String::from),
            },
            wayland_protocols: vec![
                "wl_compositor".to_string(),
                "xdg_wm_base".to_string(),
                "zwlr_screencopy_manager_v1".to_string(),
                "ext_data_control_manager_v1".to_string(),
            ],
            portal_backend: Some("gnome".to_string()),
            recommended_capture: CaptureBackend::Portal,
            recommended_buffer_type: BufferType::DmaBuf,
            supports_damage_hints: true,
            supports_explicit_sync: true,
            quirks: vec![],
            recommended_fps_cap: 60,
            portal_timeout_ms: 15000,
        }
    }

    /// Smithay-based compositor profile, dispatched by name
    ///
    /// Smithay itself provides no protocol implementations — each compositor
    /// built on it chooses its own protocol surface, and Jay and xfwl4 have
    /// diverged enough that one shared profile misrepresented both (Jay was
    /// undersold, xfwl4 was oversold with a clipboard protocol it doesn't
    /// have — see docs/FEATURE-SUPPORT-MATRIX.md, 2026-08-17 sweep). Known
    /// names get a dedicated profile; anything else falls back to the
    /// conservative generic one.
    fn smithay_profile(name: &str) -> Self {
        match name {
            "jay" => Self::jay_profile(),
            "xfwl4" => Self::xfwl4_profile(),
            _ => Self::smithay_generic_profile(name),
        }
    }

    /// Jay compositor profile
    ///
    /// Jay is a first-class target for Lamco's own xdg-desktop-portal-generic
    /// (embedded, not a system portal): RemoteDesktop v2 via EIS primary with
    /// wlr-virtual-pointer/keyboard fallback, ScreenCast v6 via
    /// ext-image-copy-capture-v1 primary with wlr-screencopy-v1 fallback, and
    /// clipboard via ext-data-control-v1. No distro packages exist yet
    /// (cargo/AUR-only, jay-compositor 1.7.0), so this profile is effectively
    /// unverified in real deployments.
    fn jay_profile() -> Self {
        Self {
            compositor: CompositorType::Smithay {
                name: "jay".to_string(),
            },
            wayland_protocols: vec![
                "wl_compositor".to_string(),
                "xdg_wm_base".to_string(),
                "ext_image_copy_capture_manager_v1".to_string(),
                "zwlr_screencopy_manager_v1".to_string(),
                "zwp_virtual_keyboard_manager_v1".to_string(),
                "zwlr_virtual_pointer_manager_v1".to_string(),
                "ext_data_control_manager_v1".to_string(),
            ],
            // Driven by the embedded portal-generic strategy, not a system portal.
            portal_backend: None,
            recommended_capture: CaptureBackend::ExtImageCopyCapture,
            recommended_buffer_type: BufferType::Any,
            supports_damage_hints: true,
            supports_explicit_sync: true,
            quirks: vec![],
            recommended_fps_cap: 60,
            portal_timeout_ms: 15000,
        }
    }

    /// xfwl4 (Xfce's Rust/Smithay compositor, early preview) profile
    ///
    /// As of the 4.21.1 preview (2026-08-11) xfwl4 only implements the legacy
    /// wlr-screencopy-unstable-v1 capture path and exposes no
    /// virtual-keyboard, virtual-pointer, or clipboard protocol of any kind.
    /// Explicitly not for everyday use per the project itself.
    fn xfwl4_profile() -> Self {
        Self {
            compositor: CompositorType::Smithay {
                name: "xfwl4".to_string(),
            },
            wayland_protocols: vec![
                "wl_compositor".to_string(),
                "xdg_wm_base".to_string(),
                "zwlr_screencopy_manager_v1".to_string(),
            ],
            portal_backend: None,
            recommended_capture: CaptureBackend::WlrScreencopy,
            recommended_buffer_type: BufferType::MemFd,
            supports_damage_hints: false,
            supports_explicit_sync: false,
            quirks: vec![Quirk::NeedsExplicitCursorComposite],
            recommended_fps_cap: 30,
            portal_timeout_ms: 30000,
        }
    }

    /// Generic Smithay-based compositor profile for anything not named above
    /// (smallvil and any future/unrecognized Smithay compositor)
    fn smithay_generic_profile(name: &str) -> Self {
        Self {
            compositor: CompositorType::Smithay {
                name: name.to_string(),
            },
            wayland_protocols: vec![
                "wl_compositor".to_string(),
                "xdg_wm_base".to_string(),
                "zwlr_screencopy_manager_v1".to_string(),
                "ext_data_control_manager_v1".to_string(),
            ],
            portal_backend: Some("gtk".to_string()),
            recommended_capture: CaptureBackend::Portal,
            recommended_buffer_type: BufferType::DmaBuf,
            supports_damage_hints: true,
            supports_explicit_sync: true,
            quirks: vec![],
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
            quirks: vec![Quirk::NeedsExplicitCursorComposite],
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
        assert_eq!(profile.recommended_buffer_type, BufferType::Any);
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
        assert!(profile.has_quirk(&Quirk::NeedsExplicitCursorComposite));
    }

    #[test]
    fn test_smithay_profile_dispatches_jay() {
        let profile = CompositorProfile::smithay_profile("jay");
        assert_eq!(
            profile.recommended_capture,
            CaptureBackend::ExtImageCopyCapture
        );
        assert!(
            profile
                .wayland_protocols
                .iter()
                .any(|p| p == "ext_data_control_manager_v1")
        );
        assert!(
            profile
                .wayland_protocols
                .iter()
                .any(|p| p == "zwp_virtual_keyboard_manager_v1")
        );
    }

    #[test]
    fn test_smithay_profile_dispatches_xfwl4() {
        let profile = CompositorProfile::smithay_profile("xfwl4");
        assert_eq!(profile.recommended_capture, CaptureBackend::WlrScreencopy);
        // xfwl4 4.21.1 has no clipboard protocol at all — must not claim one.
        assert!(
            !profile
                .wayland_protocols
                .iter()
                .any(|p| p == "ext_data_control_manager_v1")
        );
        assert!(
            !profile
                .wayland_protocols
                .iter()
                .any(|p| p == "zwp_virtual_keyboard_manager_v1")
        );
    }

    #[test]
    fn test_smithay_profile_falls_back_for_unknown_name() {
        let profile = CompositorProfile::smithay_profile("smallvil");
        assert_eq!(profile.portal_backend, Some("gtk".to_string()));
        assert_eq!(profile.recommended_capture, CaptureBackend::Portal);
    }

    #[test]
    fn test_for_compositor() {
        let gnome = CompositorType::Gnome {
            version: Some("46.0".to_string()),
        };
        let profile = CompositorProfile::for_compositor(&gnome);
        assert_eq!(profile.portal_backend, Some("gnome".to_string()));
    }

    #[test]
    fn test_hyprland_054_has_ext_capture_quirk() {
        let profile = CompositorProfile::hyprland_profile(Some("0.54.1"));
        // 0.54.x retains the protective quirk (original observation).
        assert!(profile.has_quirk(&Quirk::ExtCaptureIncomplete));
        // recommended_capture is now Portal (auto-detect); the quirk drives
        // the wlr-screencopy fallback at the preference layer, not by
        // forcing the profile field.
        assert_eq!(profile.recommended_capture, CaptureBackend::Portal);
    }

    #[test]
    fn test_hyprland_053_no_ext_capture_quirk() {
        let profile = CompositorProfile::hyprland_profile(Some("0.53.3"));
        assert!(!profile.has_quirk(&Quirk::ExtCaptureIncomplete));
    }

    #[test]
    fn test_hyprland_055_no_ext_capture_quirk() {
        // 0.55.x is empirically verified working (archie 2026-05-23 retest):
        // handshake completes in ~150μs, all six events arrive.
        let profile = CompositorProfile::hyprland_profile(Some("0.55.2"));
        assert!(!profile.has_quirk(&Quirk::ExtCaptureIncomplete));
        assert_eq!(profile.recommended_capture, CaptureBackend::Portal);
    }

    #[test]
    fn test_hyprland_unknown_version_assumes_good() {
        // With the quirk narrowed to 0.54.x specifically, unknown versions
        // get the benefit of the doubt — auto-fallback handles the rare
        // case where ext is actually broken on a release we don't know.
        let profile = CompositorProfile::hyprland_profile(None);
        assert!(!profile.has_quirk(&Quirk::ExtCaptureIncomplete));
    }
}
