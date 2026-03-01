//! Session Strategy Abstraction
//!
//! Defines the common interface for different session creation strategies:
//! - Portal + Token Strategy (universal)
//! - Mutter Direct API (GNOME only)
//! - libei/EIS (wlroots via Portal, Flatpak-compatible)
//! - wlr-direct (wlroots native protocols, no Flatpak)

use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

use anyhow::Result;
use async_trait::async_trait;
use tokio::sync::RwLock;

use crate::health::HealthReporter;

/// Portal clipboard components
///
/// Contains the Portal clipboard manager and session needed for clipboard operations.
/// Only Portal strategy can provide this; Mutter has no clipboard API.
///
/// Note: On Portal v1 (e.g., RHEL 9 GNOME 40), clipboard is not supported,
/// so `manager` will be `None`. The session is always available.
///
/// # Session Lock Design (RwLock)
///
/// We use RwLock instead of Mutex to allow concurrent operations.
/// Both input injection and clipboard operations use `.read().await` since they
/// don't modify the session - they just pass the session handle to D-Bus calls.
/// This prevents clipboard operations from blocking input injection.
pub struct ClipboardComponents {
    /// Portal clipboard manager - None on Portal v1 (no clipboard support)
    pub manager: Option<Arc<lamco_portal::ClipboardManager>>,
    /// Portal session for clipboard operations (always available)
    /// Uses RwLock to allow concurrent access from input and clipboard operations
    pub session: Arc<
        RwLock<
            ashpd::desktop::Session<
                'static,
                ashpd::desktop::remote_desktop::RemoteDesktop<'static>,
            >,
        >,
    >,
    /// Session validity — false when compositor has destroyed the Portal session.
    /// Clipboard operations should check this before calling Portal D-Bus methods.
    pub session_valid: Arc<AtomicBool>,
}

impl ClipboardComponents {
    /// Check if the Portal session is still valid for clipboard operations.
    pub fn is_session_valid(&self) -> bool {
        self.session_valid.load(Ordering::Acquire)
    }
}

/// Common session handle trait
///
/// Abstracts over different session implementations (Portal, Mutter, wlr)
#[async_trait]
pub trait SessionHandle: Send + Sync {
    fn pipewire_access(&self) -> PipeWireAccess;

    fn streams(&self) -> Vec<StreamInfo>;

    fn session_type(&self) -> SessionType;

    // === Input Injection Methods ===

    async fn notify_keyboard_keycode(&self, keycode: i32, pressed: bool) -> Result<()>;

    async fn notify_pointer_motion_absolute(&self, stream_id: u32, x: f64, y: f64) -> Result<()>;

    async fn notify_pointer_button(&self, button: i32, pressed: bool) -> Result<()>;

    async fn notify_pointer_axis(&self, dx: f64, dy: f64) -> Result<()>;

    // === Health Integration ===

    /// Wire a health reporter into this session handle.
    ///
    /// Called once after session creation. The reporter is used to notify the
    /// health monitor of session lifecycle events (closed, invalidated, errors).
    /// Default: no-op for strategies that don't support health reporting.
    fn set_health_reporter(&self, _reporter: HealthReporter) {}

    /// Provide stream info from an external video source.
    ///
    /// wlr-direct creates input devices before ScreenCast streams are known.
    /// The server calls this after obtaining streams so pointer coordinate
    /// transformation uses the real resolution instead of a fallback.
    fn set_streams(&self, _streams: Vec<StreamInfo>) {}

    // === Clipboard Support ===

    /// Returns Some for Portal strategy (shares session), None for Mutter (no clipboard API).
    /// When None, caller must create a separate Portal session for clipboard operations.
    fn portal_clipboard(&self) -> Option<ClipboardComponents>;

    /// Returns the Mutter clipboard manager if available (MutterDirect strategy only).
    /// Used to create MutterClipboardProvider without exposing MutterClipboardManager broadly.
    fn mutter_clipboard(&self) -> Option<std::sync::Arc<crate::mutter::MutterClipboardManager>> {
        None
    }

    /// Returns the data-control clipboard backend if available (portal-generic strategy only).
    /// Used to create DataControlClipboardProvider for wlroots compositors.
    #[cfg(feature = "portal-generic")]
    fn clipboard_backend(
        &self,
    ) -> Option<
        std::sync::Arc<std::sync::Mutex<Box<dyn xdg_desktop_portal_generic::ClipboardBackend>>>,
    > {
        None
    }
}

/// PipeWire access method
#[derive(Debug, Clone)]
pub enum PipeWireAccess {
    /// Portal provides a file descriptor
    FileDescriptor(std::os::fd::RawFd),
    /// Mutter provides a PipeWire node ID
    NodeId(u32),
}

/// Stream information (unified across strategies)
#[derive(Debug, Clone)]
pub struct StreamInfo {
    pub node_id: u32,
    pub width: u32,
    pub height: u32,
    pub position_x: i32,
    pub position_y: i32,
}

/// Session type
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionType {
    /// XDG Portal session
    Portal,
    /// Mutter direct D-Bus API
    MutterDirect,
    /// wlroots direct protocols (virtual keyboard/pointer)
    WlrDirect,
    /// libei/EIS protocol via Portal RemoteDesktop
    Libei,
    /// Embedded portal-generic backend (wlroots native video + input + clipboard)
    PortalGeneric,
    /// ScreenCast-only (view-only, no input injection)
    /// Used when view-only mode is configured, or as fallback when no input strategy is available
    ScreenCastOnly,
}

impl std::fmt::Display for SessionType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SessionType::Portal => write!(f, "Portal"),
            SessionType::MutterDirect => write!(f, "Mutter Direct API"),
            SessionType::WlrDirect => write!(f, "wlr-direct"),
            SessionType::Libei => write!(f, "libei/EIS"),
            SessionType::PortalGeneric => write!(f, "portal-generic (embedded)"),
            SessionType::ScreenCastOnly => write!(f, "ScreenCast-only (view-only)"),
        }
    }
}

/// Session creation strategy
///
/// Different implementations for Portal, Mutter, wlr-screencopy
#[async_trait]
pub trait SessionStrategy: Send + Sync {
    fn name(&self) -> &'static str;

    fn requires_initial_setup(&self) -> bool;

    fn supports_unattended_restore(&self) -> bool;

    async fn create_session(&self) -> Result<Arc<dyn SessionHandle>>;

    async fn cleanup(&self, session: &dyn SessionHandle) -> Result<()>;
}

/// Session configuration
#[derive(Debug, Clone)]
pub struct SessionConfig {
    /// Session identifier
    pub session_id: String,
    /// Cursor mode preference
    pub cursor_mode: CursorMode,
    /// Monitor connector (for Mutter), or None for virtual/all monitors
    pub monitor_connector: Option<String>,
    /// Enable clipboard
    pub enable_clipboard: bool,
}

/// Cursor mode
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorMode {
    /// Cursor embedded in video
    Embedded,
    /// Cursor as separate metadata
    Metadata,
    /// No cursor
    Hidden,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            session_id: format!("lamco-rdp-{}", uuid::Uuid::new_v4()),
            cursor_mode: CursorMode::Metadata,
            monitor_connector: None,
            enable_clipboard: true,
        }
    }
}
