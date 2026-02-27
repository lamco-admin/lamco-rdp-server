//! Clipboard Provider Implementations
//!
//! Each provider bridges a specific clipboard backend to the `ClipboardProvider`
//! trait used by the `ClipboardOrchestrator`.
//!
//! # Providers
//!
//! - **`PortalClipboardProvider`**: Portal D-Bus (universal, always available)
//! - **`MutterClipboardProvider`**: Mutter D-Bus (GNOME only)
//! - **`DataControlClipboardProvider`**: via portal-generic's `ClipboardBackend`
//!   (feature: `portal-generic`, bundled with wlroots session strategy)
//! - **`WlClipboardProvider`**: standalone `wl-clipboard-rs` (feature: `wl-clipboard`,
//!   composable with any session strategy, native-only)

pub mod portal;

#[cfg(feature = "portal-generic")]
pub mod data_control;

#[cfg(feature = "wl-clipboard")]
pub mod wl_clipboard;

pub mod mutter;

#[cfg(feature = "portal-generic")]
pub use data_control::DataControlClipboardProvider;
pub use mutter::MutterClipboardProvider;
pub use portal::PortalClipboardProvider;
#[cfg(feature = "wl-clipboard")]
pub use wl_clipboard::WlClipboardProvider;
