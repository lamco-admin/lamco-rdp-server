//! Wayland feature definitions
//!
//! Enumerates detectable Wayland compositor features that can be
//! translated into RDP service advertisements.

use serde::{Deserialize, Serialize};

/// Method used for damage tracking
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DamageMethod {
    /// Portal provides damage hints
    Portal,

    /// Native compositor damage API (wlroots screencopy)
    NativeScreencopy,

    /// Frame differencing in software
    FrameDiff,

    /// Hybrid: use damage hints when available, fall back to diff
    Hybrid,
}

impl Default for DamageMethod {
    fn default() -> Self {
        Self::FrameDiff
    }
}

/// DRM format for DMA-BUF
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DrmFormat {
    /// ARGB8888 (common, compatible)
    Argb8888,

    /// XRGB8888 (no alpha)
    Xrgb8888,

    /// ABGR8888 (reverse byte order)
    Abgr8888,

    /// NV12 (YUV 4:2:0, for hardware encoding)
    Nv12,

    /// Other format (fourcc code)
    Other(u32),
}

impl DrmFormat {
    /// Check if format supports alpha channel
    pub fn has_alpha(&self) -> bool {
        matches!(self, Self::Argb8888 | Self::Abgr8888)
    }

    /// Check if format is YUV (hardware encoder friendly)
    pub fn is_yuv(&self) -> bool {
        matches!(self, Self::Nv12)
    }
}

/// HDR transfer function
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum HdrTransfer {
    /// Standard dynamic range (sRGB)
    Sdr,

    /// Perceptual Quantizer (HDR10)
    Pq,

    /// Hybrid Log-Gamma (broadcast HDR)
    Hlg,

    /// Extended sRGB (scRGB)
    ScRgb,
}

impl Default for HdrTransfer {
    fn default() -> Self {
        Self::Sdr
    }
}

/// Detectable Wayland feature with associated metadata
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum WaylandFeature {
    /// Damage tracking capability
    DamageTracking {
        /// Method used for damage detection
        method: DamageMethod,
        /// Whether compositor provides per-frame damage hints
        compositor_hints: bool,
    },

    /// Zero-copy DMA-BUF buffer access
    DmaBufZeroCopy {
        /// Supported DRM formats
        formats: Vec<DrmFormat>,
        /// Whether modifiers are supported
        supports_modifiers: bool,
    },

    /// Explicit sync protocol support
    ExplicitSync {
        /// Protocol version
        version: u32,
    },

    /// Fractional scaling support
    FractionalScaling {
        /// Maximum supported scale factor
        max_scale: f32,
    },

    /// Metadata cursor (position sent separately from video)
    MetadataCursor {
        /// Whether hotspot is included
        has_hotspot: bool,
        /// Whether cursor image updates are available
        has_shape_updates: bool,
    },

    /// Multi-monitor support
    MultiMonitor {
        /// Maximum number of monitors
        max_monitors: u32,
        /// Whether virtual source type is available
        virtual_source: bool,
    },

    /// Per-window capture
    WindowCapture {
        /// Whether toplevel export is available
        has_toplevel_export: bool,
    },

    /// HDR color space support
    HdrColorSpace {
        /// Transfer function
        transfer: HdrTransfer,
        /// Color gamut
        gamut: String,
    },

    /// Clipboard via portal
    Clipboard {
        /// Portal version supporting clipboard
        portal_version: u32,
    },

    /// Remote input injection
    RemoteInput {
        /// Uses libei for input
        uses_libei: bool,
        /// Keyboard supported
        keyboard: bool,
        /// Pointer supported
        pointer: bool,
        /// Touch supported
        touch: bool,
    },

    /// PipeWire video stream
    PipeWireStream {
        /// Node ID if already connected
        node_id: Option<u32>,
        /// Preferred buffer type
        buffer_type: String,
    },

    // === Session Persistence Features ===
    // Added in Phase 2
    /// Session persistence via portal restore tokens
    SessionPersistence {
        /// Portal supports restore tokens (v4+)
        restore_token_supported: bool,
        /// Maximum persist mode (0=none, 1=transient, 2=permanent)
        max_persist_mode: u8,
        /// How tokens are stored
        token_storage: TokenStorageMethod,
        /// Portal version detected
        portal_version: u32,
    },

    /// Mutter direct D-Bus API (GNOME only)
    MutterDirectAPI {
        /// GNOME Shell version
        version: Option<String>,
        /// org.gnome.Mutter.ScreenCast available
        has_screencast: bool,
        /// org.gnome.Mutter.RemoteDesktop available
        has_remote_desktop: bool,
    },

    /// Credential storage capability
    CredentialStorage {
        /// Primary storage method available
        method: crate::session::CredentialStorageMethod,
        /// Is storage unlocked/accessible?
        is_accessible: bool,
        /// Encryption algorithm used
        encryption: crate::session::EncryptionType,
    },

    /// Unattended access capability (aggregate)
    UnattendedAccess {
        /// Can avoid permission dialog
        can_avoid_dialog: bool,
        /// Can store credentials securely
        can_store_credentials: bool,
    },

    /// wlr-screencopy protocol
    WlrScreencopy {
        /// Protocol version
        version: u32,
        /// Supports DMA-BUF output
        dmabuf_supported: bool,
        /// Supports damage tracking
        damage_supported: bool,
    },

    /// wlr-direct input protocols (virtual keyboard/pointer)
    WlrDirectInput {
        /// Virtual keyboard protocol version
        keyboard_version: u32,
        /// Virtual pointer protocol version
        pointer_version: u32,
        /// Supports modifier state
        supports_modifiers: bool,
        /// Touch input supported
        supports_touch: bool,
    },

    /// libei/EIS input via Portal RemoteDesktop
    LibeiInput {
        /// Portal version
        portal_version: u32,
        /// Has ConnectToEIS method
        has_connect_to_eis: bool,
        /// Keyboard support
        keyboard: bool,
        /// Pointer support
        pointer: bool,
        /// Touch support
        touch: bool,
    },
}

/// Token storage method for session persistence
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TokenStorageMethod {
    /// No token storage available
    None,
    /// Tokens stored in encrypted file
    EncryptedFile,
    /// Tokens stored via Secret Service API
    SecretService,
    /// Tokens stored via Flatpak Secret Portal
    FlatpakSecretPortal,
    /// Tokens stored via TPM 2.0 + systemd-creds
    Tpm2SystemdCreds,
}

impl WaylandFeature {
    /// Get a short identifier for logging
    pub fn short_name(&self) -> &'static str {
        match self {
            Self::DamageTracking { .. } => "damage",
            Self::DmaBufZeroCopy { .. } => "dmabuf",
            Self::ExplicitSync { .. } => "explicit-sync",
            Self::FractionalScaling { .. } => "fractional-scale",
            Self::MetadataCursor { .. } => "metadata-cursor",
            Self::MultiMonitor { .. } => "multi-monitor",
            Self::WindowCapture { .. } => "window-capture",
            Self::HdrColorSpace { .. } => "hdr",
            Self::Clipboard { .. } => "clipboard",
            Self::RemoteInput { .. } => "remote-input",
            Self::PipeWireStream { .. } => "pipewire",
            // Session persistence
            Self::SessionPersistence { .. } => "session-persist",
            Self::MutterDirectAPI { .. } => "mutter-api",
            Self::CredentialStorage { .. } => "cred-storage",
            Self::UnattendedAccess { .. } => "unattended",
            Self::WlrScreencopy { .. } => "wlr-screencopy",
            Self::WlrDirectInput { .. } => "wlr-direct-input",
            Self::LibeiInput { .. } => "libei-input",
        }
    }
}

impl std::fmt::Display for WaylandFeature {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DamageTracking {
                method,
                compositor_hints,
            } => {
                write!(
                    f,
                    "DamageTracking({:?}, hints={})",
                    method, compositor_hints
                )
            }
            Self::DmaBufZeroCopy { formats, .. } => {
                write!(f, "DmaBuf({} formats)", formats.len())
            }
            Self::ExplicitSync { version } => {
                write!(f, "ExplicitSync(v{})", version)
            }
            Self::FractionalScaling { max_scale } => {
                write!(f, "FractionalScale(max={}x)", max_scale)
            }
            Self::MetadataCursor { .. } => write!(f, "MetadataCursor"),
            Self::MultiMonitor { max_monitors, .. } => {
                write!(f, "MultiMonitor(max={})", max_monitors)
            }
            Self::WindowCapture { .. } => write!(f, "WindowCapture"),
            Self::HdrColorSpace { transfer, .. } => {
                write!(f, "HDR({:?})", transfer)
            }
            Self::Clipboard { portal_version } => {
                write!(f, "Clipboard(portal v{})", portal_version)
            }
            Self::RemoteInput {
                keyboard,
                pointer,
                touch,
                ..
            } => {
                write!(
                    f,
                    "RemoteInput(kbd={}, ptr={}, touch={})",
                    keyboard, pointer, touch
                )
            }
            Self::PipeWireStream { buffer_type, .. } => {
                write!(f, "PipeWire({})", buffer_type)
            }
            // Session persistence
            Self::SessionPersistence {
                restore_token_supported,
                max_persist_mode,
                portal_version,
                ..
            } => {
                write!(
                    f,
                    "SessionPersist(portal v{}, tokens={}, mode={})",
                    portal_version, restore_token_supported, max_persist_mode
                )
            }
            Self::MutterDirectAPI {
                version,
                has_screencast,
                has_remote_desktop,
            } => {
                write!(
                    f,
                    "MutterAPI(v{}, sc={}, rd={})",
                    version.as_deref().unwrap_or("unknown"),
                    has_screencast,
                    has_remote_desktop
                )
            }
            Self::CredentialStorage {
                method,
                is_accessible,
                encryption,
            } => {
                write!(
                    f,
                    "CredStorage({}, {}, accessible={})",
                    method, encryption, is_accessible
                )
            }
            Self::UnattendedAccess {
                can_avoid_dialog,
                can_store_credentials,
            } => {
                write!(
                    f,
                    "Unattended(no_dialog={}, creds={})",
                    can_avoid_dialog, can_store_credentials
                )
            }
            Self::WlrScreencopy {
                version,
                dmabuf_supported,
                damage_supported,
            } => {
                write!(
                    f,
                    "wlr-screencopy(v{}, dmabuf={}, damage={})",
                    version, dmabuf_supported, damage_supported
                )
            }
            Self::WlrDirectInput {
                keyboard_version,
                pointer_version,
                supports_modifiers,
                supports_touch,
            } => {
                write!(
                    f,
                    "wlr-direct(kbd=v{}, ptr=v{}, mods={}, touch={})",
                    keyboard_version, pointer_version, supports_modifiers, supports_touch
                )
            }
            Self::LibeiInput {
                portal_version,
                has_connect_to_eis,
                keyboard,
                pointer,
                touch,
            } => {
                write!(
                    f,
                    "libei(portal=v{}, eis={}, kbd={}, ptr={}, touch={})",
                    portal_version, has_connect_to_eis, keyboard, pointer, touch
                )
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_drm_format_alpha() {
        assert!(DrmFormat::Argb8888.has_alpha());
        assert!(!DrmFormat::Xrgb8888.has_alpha());
        assert!(!DrmFormat::Nv12.has_alpha());
    }

    #[test]
    fn test_drm_format_yuv() {
        assert!(DrmFormat::Nv12.is_yuv());
        assert!(!DrmFormat::Argb8888.is_yuv());
    }

    #[test]
    fn test_feature_display() {
        let feature = WaylandFeature::DamageTracking {
            method: DamageMethod::Portal,
            compositor_hints: true,
        };
        assert!(feature.to_string().contains("DamageTracking"));
    }
}
