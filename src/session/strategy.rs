//! Session Strategy Abstraction
//!
//! Defines the common interface for different session creation strategies:
//! - Portal + Token Strategy (universal)
//! - Mutter Direct API (GNOME only)
//! - wlr-screencopy (wlroots only, future)

use anyhow::Result;
use async_trait::async_trait;
use std::sync::Arc;
use tokio::sync::Mutex;

/// Portal clipboard components
///
/// Contains the Portal clipboard manager and session needed for clipboard operations.
/// Only Portal strategy can provide this; Mutter has no clipboard API.
///
/// Note: On Portal v1 (e.g., RHEL 9 GNOME 40), clipboard is not supported,
/// so `manager` will be `None`. The session is always available.
pub struct ClipboardComponents {
    /// Portal clipboard manager - None on Portal v1 (no clipboard support)
    pub manager: Option<Arc<lamco_portal::ClipboardManager>>,
    /// Portal session for clipboard operations (always available)
    pub session: Arc<
        Mutex<
            ashpd::desktop::Session<
                'static,
                ashpd::desktop::remote_desktop::RemoteDesktop<'static>,
            >,
        >,
    >,
}

/// Common session handle trait
///
/// Abstracts over different session implementations (Portal, Mutter, wlr)
#[async_trait]
pub trait SessionHandle: Send + Sync {
    /// Get PipeWire node ID or file descriptor for video capture
    fn pipewire_access(&self) -> PipeWireAccess;

    /// Get stream information
    fn streams(&self) -> Vec<StreamInfo>;

    /// Session type identifier
    fn session_type(&self) -> SessionType;

    // === Input Injection Methods ===

    /// Inject keyboard keycode event
    ///
    /// # Arguments
    ///
    /// * `keycode` - Linux keycode (evdev)
    /// * `pressed` - true for press, false for release
    async fn notify_keyboard_keycode(&self, keycode: i32, pressed: bool) -> Result<()>;

    /// Inject absolute pointer motion
    ///
    /// # Arguments
    ///
    /// * `stream_id` - PipeWire stream node ID
    /// * `x` - Absolute X coordinate (stream-relative)
    /// * `y` - Absolute Y coordinate (stream-relative)
    async fn notify_pointer_motion_absolute(&self, stream_id: u32, x: f64, y: f64) -> Result<()>;

    /// Inject pointer button event
    ///
    /// # Arguments
    ///
    /// * `button` - Button code (evdev: 272=left, 273=right, 274=middle)
    /// * `pressed` - true for press, false for release
    async fn notify_pointer_button(&self, button: i32, pressed: bool) -> Result<()>;

    /// Inject pointer axis (scroll) event
    ///
    /// # Arguments
    ///
    /// * `dx` - Horizontal scroll delta
    /// * `dy` - Vertical scroll delta
    async fn notify_pointer_axis(&self, dx: f64, dy: f64) -> Result<()>;

    // === Clipboard Support ===

    /// Get Portal clipboard components (if available)
    ///
    /// Returns Some for Portal strategy (shares session), None for Mutter (no clipboard API).
    /// When None, caller must create a separate Portal session for clipboard operations.
    fn portal_clipboard(&self) -> Option<ClipboardComponents>;
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
    /// wlr-screencopy protocol (future)
    WlrScreencopy,
}

impl std::fmt::Display for SessionType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SessionType::Portal => write!(f, "Portal"),
            SessionType::MutterDirect => write!(f, "Mutter Direct API"),
            SessionType::WlrScreencopy => write!(f, "wlr-screencopy"),
        }
    }
}

/// Session creation strategy
///
/// Different implementations for Portal, Mutter, wlr-screencopy
#[async_trait]
pub trait SessionStrategy: Send + Sync {
    /// Human-readable strategy name
    fn name(&self) -> &'static str;

    /// Does this strategy require initial user interaction?
    fn requires_initial_setup(&self) -> bool;

    /// Can this strategy restore sessions without user interaction?
    fn supports_unattended_restore(&self) -> bool;

    /// Create a new capture session
    ///
    /// Returns a session handle that can be used for video capture and input injection
    async fn create_session(&self) -> Result<Arc<dyn SessionHandle>>;

    /// Clean up session resources
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
