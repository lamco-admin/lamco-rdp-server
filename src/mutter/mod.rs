//! Mutter Direct D-Bus API Integration (GNOME Only)
//!
//! Provides direct access to GNOME Mutter's org.gnome.Mutter.ScreenCast and
//! org.gnome.Mutter.RemoteDesktop D-Bus interfaces, bypassing the XDG Portal
//! permission dialogs entirely.
//!
//! This is a GNOME-specific optimization that enables zero-dialog operation
//! for trusted applications running in the user's session.
//!
//! # Security Model
//!
//! Unlike XDG Portals which require explicit user consent, Mutter's D-Bus APIs
//! are accessible to any application running in the user's D-Bus session.
//! This is appropriate for lamco-rdp-server as a server application that the
//! user explicitly installed and launched.
//!
//! # Compatibility
//!
//! - GNOME 45+: ScreenCast + RemoteDesktop with session linkage
//! - GNOME 46+: ConnectToEIS for low-latency input via EI protocol
//! - GNOME 47+: Clipboard support via RemoteDesktop clipboard methods
//!
//! # Session Linkage
//!
//! RemoteDesktop must be created first. Its `SessionId` property is then passed
//! to ScreenCast.CreateSession as `"remote-desktop-session-id"` to link the two
//! sessions. Without this linkage, input injection cannot target the correct
//! screencast stream.
//!
//! # Usage
//!
//! ```rust,ignore
//! use lamco_rdp_server::mutter::MutterSessionManager;
//!
//! let manager = MutterSessionManager::new().await?;
//! let session = manager.create_session(None).await?;
//! // session provides video (PipeWire node), input (D-Bus or EIS), and clipboard
//! ```

pub(crate) mod clipboard;
pub mod pipewire_helper;
pub mod remote_desktop;
pub mod screencast;
pub mod session_manager;

// Re-exports
pub(crate) use clipboard::MutterClipboardManager;
pub use pipewire_helper::{connect_to_pipewire_daemon, get_pipewire_fd_for_mutter};
pub use remote_desktop::{MutterRemoteDesktop, MutterRemoteDesktopSession};
pub use screencast::{MutterScreenCast, MutterScreenCastSession, MutterScreenCastStream};
pub use session_manager::{MutterSessionHandle, MutterSessionManager};

/// Check if Mutter ScreenCast API is available
///
/// Returns true if org.gnome.Mutter.ScreenCast is accessible on D-Bus
pub async fn is_mutter_screencast_available() -> bool {
    match zbus::Connection::session().await {
        Ok(conn) => screencast::MutterScreenCast::new(&conn).await.is_ok(),
        Err(_) => false,
    }
}

/// Check if Mutter RemoteDesktop API is available
///
/// Returns true if org.gnome.Mutter.RemoteDesktop is accessible on D-Bus
pub async fn is_mutter_remote_desktop_available() -> bool {
    match zbus::Connection::session().await {
        Ok(conn) => remote_desktop::MutterRemoteDesktop::new(&conn)
            .await
            .is_ok(),
        Err(_) => false,
    }
}

/// Check if both Mutter APIs are available (required for full functionality)
pub async fn is_mutter_api_available() -> bool {
    is_mutter_screencast_available().await && is_mutter_remote_desktop_available().await
}
