//! Session Strategy Abstraction
//!
//! Defines the common interface for different session creation strategies:
//! - Portal + Token Strategy (universal)
//! - Mutter Direct API (GNOME only)
//! - libei/EIS (wlroots via Portal, Flatpak-compatible)
//! - wlr-direct (wlroots native protocols, no Flatpak)

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
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
    pub session:
        Arc<RwLock<ashpd::desktop::Session<ashpd::desktop::remote_desktop::RemoteDesktop>>>,
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

/// Describes how a strategy provides clipboard support.
///
/// Each strategy returns one of these variants from `clipboard_source()`,
/// telling the server what clipboard backend is available without the server
/// needing to know strategy internals.
pub enum ClipboardSource {
    /// Portal RemoteDesktop clipboard (PortalToken, libei+Portal).
    /// The strategy already created a Portal session with clipboard support.
    Portal(ClipboardComponents),

    /// Mutter D-Bus clipboard (MutterDirect strategy).
    /// Clipboard is handled natively via org.gnome.Mutter.RemoteDesktop.
    Mutter(Arc<crate::mutter::MutterClipboard>),

    /// Wayland data-control protocol (PortalGeneric strategy).
    /// Clipboard is handled via ext-data-control-v1 or wlr-data-control-v1.
    #[cfg(feature = "portal-generic")]
    DataControl(Arc<std::sync::Mutex<Box<dyn xdg_desktop_portal_generic::ClipboardBackend>>>),

    /// No clipboard support from this strategy.
    /// Used by ScreenCastOnly (view-only), wlr-direct (input-only),
    /// and libei when not sharing a Portal session.
    None,
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

    /// Send a keyboard event by XKB keysym (for Unicode input).
    ///
    /// Used for characters that don't have a direct evdev keycode mapping,
    /// such as CJK, accented characters, or symbols outside US QWERTY.
    /// XKB Unicode keysyms use the range 0x01000000 + Unicode code point.
    ///
    /// Default implementation returns Ok (no-op for strategies that don't
    /// support keysym input, like EIS).
    async fn notify_keyboard_keysym(&self, _keysym: u32, _pressed: bool) -> Result<()> {
        Ok(())
    }

    async fn notify_pointer_motion_absolute(&self, stream_id: u32, x: f64, y: f64) -> Result<()>;

    async fn notify_pointer_button(&self, button: i32, pressed: bool) -> Result<()>;

    async fn notify_pointer_axis(&self, dx: f64, dy: f64) -> Result<()>;

    /// Inject a discrete (notch-based) scroll. `dx_120`/`dy_120` are RDP wheel
    /// 120-units (one notch = 120).
    ///
    /// Default: convert to a continuous axis event so strategies without native
    /// discrete-scroll support keep their existing behavior. EIS strategies
    /// override this to emit true `scroll_discrete` detents.
    async fn notify_pointer_axis_discrete(&self, dx_120: i32, dy_120: i32) -> Result<()> {
        let dx = (dx_120 as f64 / 120.0) * 15.0;
        let dy = (dy_120 as f64 / 120.0) * 15.0;
        self.notify_pointer_axis(dx, dy).await
    }

    async fn notify_pointer_motion_relative(&self, _dx: f64, _dy: f64) -> Result<()> {
        Ok(())
    }

    // === Batched pointer-device injection ===
    //
    // A caller with several logically-coupled pointer-device events to
    // deliver together (e.g. a drag: relative motion immediately followed by
    // a button press, both read from one coalesced RDP input batch) should
    // stage each one via `stage_pointer_*` and call `commit_input_batch`
    // once at the end, instead of calling the immediate `notify_pointer_*`
    // methods for each. On a strategy with no native "commit" concept, the
    // `stage_*` defaults just call the existing immediate `notify_pointer_*`
    // methods, so this is a no-op behavior change for those strategies.
    // EIS-based strategies override both halves to defer the EIS `frame()`
    // commit until `commit_input_batch` is called, so the whole group lands
    // as one atomic frame instead of one frame per event.
    //
    // Only pointer-device events (relative motion, button, scroll) benefit:
    // absolute motion is a separate EIS device from these, so batching it
    // alongside them would not be atomic anyway (EIS `frame()` is
    // per-device). Keyboard and touch stay on their existing immediate path.

    /// Stage a relative-motion sample without necessarily committing it yet.
    /// Default: same as [`Self::notify_pointer_motion_relative`].
    async fn stage_pointer_motion_relative(&self, dx: f64, dy: f64) -> Result<()> {
        self.notify_pointer_motion_relative(dx, dy).await
    }

    /// Stage a button press/release without necessarily committing it yet.
    /// Default: same as [`Self::notify_pointer_button`].
    async fn stage_pointer_button(&self, button: i32, pressed: bool) -> Result<()> {
        self.notify_pointer_button(button, pressed).await
    }

    /// Stage a continuous-scroll sample without necessarily committing it
    /// yet. Default: same as [`Self::notify_pointer_axis`].
    async fn stage_pointer_axis(&self, dx: f64, dy: f64) -> Result<()> {
        self.notify_pointer_axis(dx, dy).await
    }

    /// Stage a discrete-scroll notch without necessarily committing it yet.
    /// Default: same as [`Self::notify_pointer_axis_discrete`].
    async fn stage_pointer_axis_discrete(&self, dx_120: i32, dy_120: i32) -> Result<()> {
        self.notify_pointer_axis_discrete(dx_120, dy_120).await
    }

    /// Commit every pointer-device event staged since the last commit as one
    /// atomic unit. Default: no-op, matching the `stage_*` defaults above
    /// (which already commit immediately via the existing `notify_pointer_*`
    /// methods, so there is nothing left to flush here).
    async fn commit_input_batch(&self) -> Result<()> {
        Ok(())
    }

    async fn notify_touch_down(&self, _stream_id: u32, _slot: u32, _x: f64, _y: f64) -> Result<()> {
        Ok(())
    }

    async fn notify_touch_motion(
        &self,
        _stream_id: u32,
        _slot: u32,
        _x: f64,
        _y: f64,
    ) -> Result<()> {
        Ok(())
    }

    async fn notify_touch_up(&self, _slot: u32) -> Result<()> {
        Ok(())
    }

    // === Health Integration ===

    /// Wire a health reporter into this session handle.
    ///
    /// Called once after session creation. The reporter is used to notify the
    /// health monitor of session lifecycle events (closed, invalidated, errors).
    /// Default: no-op for strategies that don't support health reporting.
    fn set_health_reporter(&self, _reporter: HealthReporter) {}

    /// Get the shared stream active flag for Portal input coupling.
    ///
    /// Portal sessions return a shared `AtomicBool` that the display handler
    /// updates on PipeWire state transitions. The Portal input methods check
    /// this flag before attempting D-Bus calls, preventing hundreds of futile
    /// errors when the stream is paused.
    ///
    /// Non-Portal strategies return `None` — their input is independent of
    /// PipeWire stream state.
    fn stream_active_flag(&self) -> Option<Arc<std::sync::atomic::AtomicBool>> {
        None
    }

    /// Whether the compositor session is currently valid for input injection.
    ///
    /// `PerConnection` backends (Mutter Direct) tear the session down between
    /// clients and re-establish it on the next connection; there is a window
    /// where queued input batches would otherwise be attempted against a
    /// session the compositor has already destroyed. The input-batching task
    /// checks this before injecting a flushed batch and discards it instead.
    ///
    /// Default: always valid. `Persistent` backends (Portal, wlr-direct) have
    /// no such teardown window between clients.
    fn is_session_valid(&self) -> bool {
        true
    }

    /// Provide stream info from an external video source.
    ///
    /// wlr-direct creates input devices before ScreenCast streams are known.
    /// The server calls this after obtaining streams so pointer coordinate
    /// transformation uses the real resolution instead of a fallback.
    fn set_streams(&self, _streams: Vec<StreamInfo>) {}

    // === Input Lifecycle ===

    /// Activate the input subsystem (e.g., EIS session creation).
    ///
    /// Called when the first RDP client connects. Strategies that need
    /// to defer input setup (like libei, where the EIS socket has a
    /// compositor-imposed idle timeout) implement this to create the
    /// input connection on-demand rather than at server startup.
    async fn activate_input(&self) -> Result<()> {
        Ok(())
    }

    // === Session Lifecycle (per-backend "protocol") ===

    /// How this backend's compositor session behaves across successive RDP
    /// client connections.
    ///
    /// Backends diverge here, and this is the seam that expresses it. An XDG
    /// Portal or wlroots session is created once and reused for the whole
    /// server process ([`SessionLifecyclePolicy::Persistent`]); Mutter's
    /// RemoteDesktop session is reaped by the compositor's idle timeout once
    /// the RDP client leaves and must be re-established per connection
    /// ([`SessionLifecyclePolicy::PerConnection`]). Future compositor session
    /// protocols slot in here without touching the connection layer.
    fn lifecycle_policy(&self) -> SessionLifecyclePolicy {
        SessionLifecyclePolicy::Persistent
    }

    /// Establish (or re-establish) the compositor session for an incoming RDP
    /// client. Returns the capture streams to bind, and whether the session was
    /// actually (re-)established this call (`true`) versus reused (`false`).
    ///
    /// `Persistent` backends reuse the existing session and return
    /// `(streams, false)` (the default). `PerConnection` backends create a fresh
    /// session when the previous one was released and return `(streams, true)` —
    /// the caller must then rebind the capture pipeline **even if the PipeWire
    /// node id is unchanged**, because the compositor can reuse a node id for a
    /// brand-new stream, so number-equality does not imply the same stream.
    async fn establish_for_client(&self) -> Result<(Vec<StreamInfo>, bool)> {
        Ok((self.streams(), false))
    }

    /// Tear the compositor session down after the RDP client disconnects, per
    /// policy. `Persistent` keeps the session alive (default no-op);
    /// `PerConnection` closes it cleanly so the compositor does not reap a
    /// half-idle session and the next client is handed a fresh one.
    async fn release_after_client(&self) {}

    // === Clipboard Support ===

    /// Describes how this strategy provides clipboard functionality.
    ///
    /// The server uses this to wire the correct clipboard provider without
    /// needing to know strategy-specific details.
    fn clipboard_source(&self) -> ClipboardSource;

    /// Build the clipboard provider this strategy backs, if any.
    ///
    /// The default constructs from [`clipboard_source`](Self::clipboard_source):
    /// Portal / Mutter / data-control natively. `portal_fallback` supplies a
    /// server-created Portal session for `None` strategies that still want Portal
    /// clipboard. Strategies override when they source clipboard differently
    /// (libei and wlr-direct use Wayland data-control — no Portal session).
    /// Returns `None` when the strategy provides no clipboard (view-only).
    async fn build_clipboard(
        &self,
        portal_fallback: Option<ClipboardComponents>,
        rate_limit_ms: u64,
    ) -> Option<Arc<dyn crate::clipboard::provider::ClipboardProvider>> {
        let components = match self.clipboard_source() {
            ClipboardSource::Portal(c) => Some(c),
            ClipboardSource::Mutter(m) => {
                return match crate::clipboard::providers::MutterClipboardProvider::new(m).await {
                    Ok(p) => Some(Arc::new(p)),
                    Err(e) => {
                        tracing::warn!("Failed to create Mutter clipboard provider: {e}");
                        None
                    }
                };
            }
            #[cfg(feature = "portal-generic")]
            ClipboardSource::DataControl(b) => {
                return Some(Arc::new(
                    crate::clipboard::providers::DataControlClipboardProvider::new(b),
                ));
            }
            ClipboardSource::None => portal_fallback,
        };

        // Portal source, or a None strategy with a server-provided fallback session.
        let c = components?;
        let manager = c.manager?; // Portal v1 (no clipboard) → None
        Some(Arc::new(
            crate::clipboard::providers::PortalClipboardProvider::new(
                manager,
                c.session,
                c.session_valid,
                rate_limit_ms,
            )
            .await,
        ))
    }
}

/// PipeWire access method
pub enum PipeWireAccess {
    /// Portal provides a file descriptor
    FileDescriptor(std::os::fd::RawFd),
    /// Mutter provides a PipeWire node ID
    NodeId(u32),
    /// Direct frame channel — bypasses PipeWire for frame transport.
    /// Used when the capture backend runs in-process (portal-generic)
    /// and PipeWire buffer sharing across connections doesn't work.
    DirectChannel(std::sync::mpsc::Receiver<lamco_pipewire::frame::RawFrameData>),
}

impl std::fmt::Debug for PipeWireAccess {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::FileDescriptor(fd) => f.debug_tuple("FileDescriptor").field(fd).finish(),
            Self::NodeId(id) => f.debug_tuple("NodeId").field(id).finish(),
            Self::DirectChannel(_) => f.debug_tuple("DirectChannel").finish(),
        }
    }
}

impl Clone for PipeWireAccess {
    fn clone(&self) -> Self {
        match self {
            Self::FileDescriptor(fd) => Self::FileDescriptor(*fd),
            Self::NodeId(id) => Self::NodeId(*id),
            Self::DirectChannel(_) => panic!("DirectChannel cannot be cloned"),
        }
    }
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

/// How a backend's compositor session behaves across successive RDP client
/// connections — the "session lifecycle protocol" for that backend.
///
/// The connection layer drives the same connect/disconnect events for every
/// backend; this policy is what each backend declares so those generic events
/// are interpreted by the backend's own rules rather than a single assumed
/// model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionLifecyclePolicy {
    /// One compositor session for the whole server process, reused across all
    /// RDP connections. Re-establishment is unavailable or needs user
    /// interaction (XDG Portal restore, wlroots, portal-generic, view-only).
    Persistent,
    /// The compositor reaps its session when the RDP client leaves (e.g.
    /// Mutter's idle timeout on an unused RemoteDesktop session). The session
    /// is (re-)established per RDP connection and torn down cleanly on
    /// disconnect; re-establishment is dialog-free. Mutter-direct.
    PerConnection,
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
