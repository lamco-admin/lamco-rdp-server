//! Compositor capability structures
//!
//! This module defines the data structures used to represent
//! compositor capabilities and detected features.

use super::{portal_caps::PortalCapabilities, profiles::CompositorProfile};

/// Compositor type with version information
#[derive(Debug, Clone, PartialEq)]
pub enum CompositorType {
    /// GNOME Shell (Mutter compositor)
    Gnome {
        /// GNOME Shell version (e.g., "46.0")
        version: Option<String>,
    },

    /// KDE Plasma (KWin compositor)
    Kde {
        /// Plasma version (e.g., "6.0")
        version: Option<String>,
    },

    /// Sway (wlroots-based tiling compositor)
    Sway {
        /// Sway version (e.g., "1.9")
        version: Option<String>,
    },

    /// Hyprland (wlroots-based compositor)
    Hyprland {
        /// Hyprland version
        version: Option<String>,
    },

    /// Weston reference compositor
    Weston,

    /// Cosmic (System76 compositor)
    Cosmic,

    /// Generic wlroots-based compositor
    Wlroots {
        /// Name of the compositor
        name: String,
    },

    /// Unknown compositor (fallback)
    Unknown {
        /// Any detected session info
        session_info: Option<String>,
    },
}

impl CompositorType {
    /// Get a human-readable name for the compositor
    pub fn name(&self) -> &str {
        match self {
            Self::Gnome { .. } => "GNOME",
            Self::Kde { .. } => "KDE Plasma",
            Self::Sway { .. } => "Sway",
            Self::Hyprland { .. } => "Hyprland",
            Self::Weston => "Weston",
            Self::Cosmic => "Cosmic",
            Self::Wlroots { name } => name,
            Self::Unknown { .. } => "Unknown",
        }
    }

    /// Check if this is a wlroots-based compositor
    pub fn is_wlroots_based(&self) -> bool {
        matches!(
            self,
            Self::Sway { .. } | Self::Hyprland { .. } | Self::Wlroots { .. }
        )
    }

    /// Get version string if available
    pub fn version(&self) -> Option<&str> {
        match self {
            Self::Gnome { version } => version.as_deref(),
            Self::Kde { version } => version.as_deref(),
            Self::Sway { version } => version.as_deref(),
            Self::Hyprland { version } => version.as_deref(),
            _ => None,
        }
    }
}

impl std::fmt::Display for CompositorType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.version() {
            Some(v) => write!(f, "{} {}", self.name(), v),
            None => write!(f, "{}", self.name()),
        }
    }
}

/// Wayland global protocol information
#[derive(Debug, Clone)]
pub struct WaylandGlobal {
    /// Protocol interface name (e.g., "wl_compositor")
    pub interface: String,

    /// Protocol version
    pub version: u32,

    /// Global registry name
    pub name: u32,
}

/// Preferred buffer type for screen capture
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BufferType {
    /// Memory file descriptor (shm)
    MemFd,

    /// DMA-BUF (zero-copy GPU buffer)
    DmaBuf,

    /// Either type (compositor chooses)
    #[default]
    Any,
}

/// Capture backend preference
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CaptureBackend {
    /// XDG Desktop Portal (most compatible)
    #[default]
    Portal,

    /// wlroots screencopy protocol (direct, low-latency)
    WlrScreencopy,

    /// ext-image-copy-capture (modern standard)
    ExtImageCopyCapture,
}

/// Complete compositor capabilities
///
/// This struct aggregates all detected capabilities:
/// - Compositor type and version
/// - Portal features
/// - Available Wayland protocols
/// - Recommended profile with settings and quirks
/// - Deployment context (affects available strategies)
/// - Session persistence capabilities
#[derive(Debug, Clone)]
pub struct CompositorCapabilities {
    /// Detected compositor type
    pub compositor: CompositorType,

    /// Portal capabilities
    pub portal: PortalCapabilities,

    /// Available Wayland globals (if probed)
    pub wayland_globals: Vec<WaylandGlobal>,

    /// Generated profile with recommended settings
    pub profile: CompositorProfile,

    // === Phase 2: Session Persistence ===
    /// Deployment context (Flatpak, systemd, initd, etc.)
    /// Affects which session strategies are available
    pub deployment: crate::session::DeploymentContext,

    /// D-Bus session bus accessible
    /// Required for portal and most compositor APIs
    pub has_session_dbus: bool,

    /// Direct Secret Service access available
    /// False in Flatpak (must use portal), true in native
    pub has_secret_service_access: bool,

    /// Detected credential storage method
    pub credential_storage_method: crate::session::CredentialStorageMethod,

    /// Credential storage is accessible (unlocked)
    pub credential_storage_accessible: bool,

    /// Encryption type for credential storage
    pub credential_encryption: crate::session::EncryptionType,

    /// Clipboard manager detection (Klipper, CopyQ, etc.)
    pub clipboard_manager: Option<crate::services::DetectedSystemClipboardManager>,
}

impl CompositorCapabilities {
    /// Create capabilities with detected compositor and portal info
    pub fn new(
        compositor: CompositorType,
        portal: PortalCapabilities,
        wayland_globals: Vec<WaylandGlobal>,
    ) -> Self {
        // Generate profile based on detected compositor
        let profile = CompositorProfile::for_compositor(&compositor);

        // Default session persistence fields (will be set by probing)
        let deployment = crate::session::detect_deployment_context();
        let has_session_dbus = true; // If we got this far, D-Bus works
        let has_secret_service_access =
            !matches!(deployment, crate::session::DeploymentContext::Flatpak);

        // Default credential storage (will be set by async probing)
        use crate::session::{CredentialStorageMethod, EncryptionType};
        let credential_storage_method = CredentialStorageMethod::EncryptedFile;
        let credential_storage_accessible = true;
        let credential_encryption = EncryptionType::Aes256Gcm;

        Self {
            compositor,
            portal,
            wayland_globals,
            profile,
            deployment,
            has_session_dbus,
            has_secret_service_access,
            credential_storage_method,
            credential_storage_accessible,
            credential_encryption,
            clipboard_manager: None, // Will be set by async probing
        }
    }

    /// Check if a specific Wayland protocol is available
    pub fn has_protocol(&self, interface: &str, min_version: u32) -> bool {
        self.wayland_globals
            .iter()
            .any(|g| g.interface == interface && g.version >= min_version)
    }

    /// Check if wlroots screencopy is available
    pub fn has_wlr_screencopy(&self) -> bool {
        self.has_protocol("zwlr_screencopy_manager_v1", 1)
    }

    /// Check if ext-image-copy-capture is available
    pub fn has_ext_image_copy_capture(&self) -> bool {
        self.has_protocol("ext_image_copy_capture_manager_v1", 1)
    }

    /// Check if fractional scaling is supported
    pub fn has_fractional_scale(&self) -> bool {
        self.has_protocol("wp_fractional_scale_manager_v1", 1)
    }

    /// Check if ext-data-control-v1 is available (Wayland staging protocol)
    pub fn has_ext_data_control(&self) -> bool {
        self.has_protocol("ext_data_control_manager_v1", 1)
    }

    /// Check if wlr-data-control-v1 is available (wlroots legacy)
    pub fn has_wlr_data_control(&self) -> bool {
        self.has_protocol("zwlr_data_control_manager_v1", 1)
    }

    /// Check if any data-control clipboard protocol is available
    pub fn has_any_data_control(&self) -> bool {
        self.has_ext_data_control() || self.has_wlr_data_control()
    }

    /// Get protocol version if available
    pub fn get_protocol_version(&self, interface: &str) -> Option<u32> {
        self.wayland_globals
            .iter()
            .find(|g| g.interface == interface)
            .map(|g| g.version)
    }

    /// Log a summary of detected capabilities
    pub fn log_summary(&self) {
        use tracing::info;

        info!("╔════════════════════════════════════════════════════════════╗");
        info!("║          Compositor Capabilities Detected                  ║");
        info!("╚════════════════════════════════════════════════════════════╝");
        info!("  Compositor: {}", self.compositor);
        info!(
            "  Type: {}",
            if self.compositor.is_wlroots_based() {
                "wlroots-based"
            } else {
                "native"
            }
        );
        info!("  Portal version: {}", self.portal.version);
        info!(
            "  Portal features: ScreenCast={}, RemoteDesktop={}, Clipboard={}",
            self.portal.supports_screencast,
            self.portal.supports_remote_desktop,
            self.portal.supports_clipboard
        );
        info!("  Cursor modes: {:?}", self.portal.available_cursor_modes);
        info!(
            "  Recommended capture: {:?}",
            self.profile.recommended_capture
        );
        info!(
            "  Recommended buffer: {:?}",
            self.profile.recommended_buffer_type
        );

        if !self.profile.quirks.is_empty() {
            info!("  Quirks to apply:");
            for quirk in &self.profile.quirks {
                info!("    - {:?}", quirk);
            }
        }
        info!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compositor::{portal_caps::PortalCapabilities, profiles::CompositorProfile};

    #[test]
    fn test_compositor_type_name() {
        let gnome = CompositorType::Gnome {
            version: Some("46.0".to_string()),
        };
        assert_eq!(gnome.name(), "GNOME");
        assert_eq!(gnome.version(), Some("46.0"));
        assert_eq!(gnome.to_string(), "GNOME 46.0");
    }

    #[test]
    fn test_compositor_is_wlroots_based() {
        let sway = CompositorType::Sway { version: None };
        let gnome = CompositorType::Gnome { version: None };

        assert!(sway.is_wlroots_based());
        assert!(!gnome.is_wlroots_based());
    }

    #[test]
    fn test_has_protocol() {
        let caps = CompositorCapabilities {
            compositor: CompositorType::Unknown { session_info: None },
            portal: PortalCapabilities::default(),
            wayland_globals: vec![WaylandGlobal {
                interface: "wl_compositor".to_string(),
                version: 5,
                name: 1,
            }],
            profile: CompositorProfile::default(),
            deployment: crate::session::DeploymentContext::Native,
            has_session_dbus: true,
            has_secret_service_access: true,
            credential_storage_method: crate::session::CredentialStorageMethod::EncryptedFile,
            credential_storage_accessible: true,
            credential_encryption: crate::session::EncryptionType::Aes256Gcm,
            clipboard_manager: None,
        };

        assert!(caps.has_protocol("wl_compositor", 1));
        assert!(caps.has_protocol("wl_compositor", 5));
        assert!(!caps.has_protocol("wl_compositor", 6));
        assert!(!caps.has_protocol("nonexistent", 1));
    }
}
