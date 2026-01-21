//! Compositor Capability Probing
//!
//! This module provides automatic detection of Wayland compositor capabilities
//! to enable runtime adaptation without manual per-DE configuration.
//!
//! # Overview
//!
//! Different Wayland compositors (GNOME, KDE, Sway, Weston, Hyprland) have
//! varying capabilities, quirks, and optimal configurations. This module:
//!
//! 1. **Identifies** the compositor type from environment and D-Bus
//! 2. **Probes** available protocols and features
//! 3. **Creates** a capability profile with recommended settings
//! 4. **Applies** quirks and workarounds automatically
//!
//! # Architecture
//!
//! ```text
//! Environment Detection
//!   └─> DESKTOP_SESSION, XDG_CURRENT_DESKTOP, etc.
//!
//! Portal Probing
//!   └─> ScreenCast, RemoteDesktop, Clipboard versions
//!
//! D-Bus Introspection (optional)
//!   └─> Compositor-specific interfaces
//!
//! Profile Generation
//!   └─> CompositorProfile with recommended settings
//! ```
//!
//! # Usage
//!
//! ```no_run
//! use lamco_rdp_server::compositor::{probe_capabilities, CompositorCapabilities};
//!
//! async fn setup() -> anyhow::Result<()> {
//!     let capabilities = probe_capabilities().await?;
//!
//!     println!("Detected: {:?}", capabilities.compositor);
//!     println!("Portal version: {}", capabilities.portal.version);
//!
//!     // Apply recommended settings
//!     for quirk in &capabilities.profile.quirks {
//!         println!("Applying quirk: {:?}", quirk);
//!     }
//!
//!     Ok(())
//! }
//! ```

mod capabilities;
mod portal_caps;
mod probing;
mod profiles;

pub use capabilities::{
    BufferType, CaptureBackend, CompositorCapabilities, CompositorType, WaylandGlobal,
};
pub use portal_caps::{CursorMode, PortalCapabilities, SourceType};
pub use probing::{detect_os_release, identify_compositor, probe_capabilities, OsRelease};
pub use profiles::{CompositorProfile, Quirk};

/// Check if we're running in a Wayland session
pub fn is_wayland_session() -> bool {
    std::env::var("WAYLAND_DISPLAY").is_ok()
        || std::env::var("XDG_SESSION_TYPE")
            .map(|v| v == "wayland")
            .unwrap_or(false)
}

/// Get the current Wayland display name
pub fn wayland_display() -> Option<String> {
    std::env::var("WAYLAND_DISPLAY").ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_wayland_session() {
        // This test is environment-dependent
        // Just verify it doesn't panic
        let _ = is_wayland_session();
    }

    #[test]
    fn test_wayland_display() {
        // This test is environment-dependent
        let _ = wayland_display();
    }
}
