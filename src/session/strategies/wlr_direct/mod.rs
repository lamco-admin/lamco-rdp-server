//! wlr-direct Strategy: Native wlroots Protocol Support
//!
//! This module implements direct Wayland protocol support for wlroots-based compositors
//! (Sway, Hyprland, River, labwc) using the virtual keyboard and pointer protocols.
//!
//! # Overview
//!
//! The wlr-direct strategy provides input injection without requiring the Portal RemoteDesktop
//! API, which is not implemented by xdg-desktop-portal-wlr. It uses:
//!
//! - `zwp_virtual_keyboard_v1` (virtual-keyboard-unstable-v1) - Standard keyboard protocol
//! - `zwlr_virtual_pointer_v1` (wlr-virtual-pointer-unstable-v1) - wlroots pointer protocol
//!
//! # Supported Compositors
//!
//! - Sway 1.7+
//! - Hyprland
//! - River
//! - labwc
//! - Any wlroots-based compositor with virtual keyboard/pointer support
//!
//! # Architecture
//!
//! ```text
//! WlrDirectStrategy
//!   ├─> Wayland Connection (WAYLAND_DISPLAY socket)
//!   ├─> Protocol Binding (registry enumeration)
//!   │   ├─> zwp_virtual_keyboard_manager_v1
//!   │   ├─> zwlr_virtual_pointer_manager_v1
//!   │   └─> wl_seat (default seat)
//!   └─> WlrSessionHandleImpl
//!       ├─> VirtualKeyboard (XKB keymap + key injection)
//!       └─> VirtualPointer (motion + button + scroll injection)
//! ```
//!
//! # Limitations (MVP)
//!
//! - **Input injection only** (no video capture)
//! - **No clipboard support** (use FUSE approach or separate Portal session)
//! - **Not Flatpak-compatible** (requires direct Wayland socket access)
//! - For video, this strategy would need integration with wlr-screencopy or Portal

mod keyboard;
mod pointer;

use std::{
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
// Re-export for external use
pub use keyboard::VirtualKeyboard as WlrVirtualKeyboard;
use keyboard::{KeyState, VirtualKeyboard};
pub use pointer::VirtualPointer as WlrVirtualPointer;
use pointer::{Axis, AxisSource, ButtonState, VirtualPointer};
use tracing::{debug, info, warn};
use wayland_client::{
    globals::{registry_queue_init, GlobalListContents},
    protocol::{wl_registry, wl_seat::WlSeat},
    Connection, Dispatch, QueueHandle,
};
use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1;
use wayland_protocols_wlr::virtual_pointer::v1::client::zwlr_virtual_pointer_manager_v1::ZwlrVirtualPointerManagerV1;

use crate::{
    health::{HealthEvent, HealthReporter},
    session::strategy::{
        ClipboardComponents, PipeWireAccess, SessionHandle, SessionStrategy, SessionType,
        StreamInfo,
    },
};

/// State for Wayland protocol dispatch
///
/// Fields exist to satisfy Dispatch trait bounds. The actual protocol objects
/// are bound via `globals.bind()` in `create_wlr_devices()`, not read from state.
struct WlrState {
    #[expect(
        dead_code,
        reason = "required by Dispatch<ZwpVirtualKeyboardManagerV1> trait bound"
    )]
    keyboard_manager: Option<ZwpVirtualKeyboardManagerV1>,
    #[expect(
        dead_code,
        reason = "required by Dispatch<ZwlrVirtualPointerManagerV1> trait bound"
    )]
    pointer_manager: Option<ZwlrVirtualPointerManagerV1>,
    #[expect(dead_code, reason = "required by Dispatch<WlSeat> trait bound")]
    seat: Option<WlSeat>,
}

impl WlrState {
    fn new() -> Self {
        Self {
            keyboard_manager: None,
            pointer_manager: None,
            seat: None,
        }
    }
}

/// wlr-direct strategy implementation
///
/// Provides input injection via native Wayland protocols for wlroots compositors.
pub struct WlrDirectStrategy {
    /// Keyboard layout from config (e.g., "us", "de", "auto")
    keyboard_layout: String,
}

impl WlrDirectStrategy {
    pub fn new() -> Self {
        Self {
            keyboard_layout: "auto".to_string(),
        }
    }

    pub fn with_keyboard_layout(keyboard_layout: String) -> Self {
        Self { keyboard_layout }
    }

    pub async fn is_available() -> bool {
        let conn = match Connection::connect_to_env() {
            Ok(conn) => conn,
            Err(e) => {
                debug!("[wlr_direct] Wayland connection failed: {}", e);
                return false;
            }
        };

        match bind_protocols(&conn) {
            Ok(()) => {
                debug!("[wlr_direct] All required protocols available");
                true
            }
            Err(e) => {
                debug!("[wlr_direct] Protocol check failed: {}", e);
                false
            }
        }
    }
}

impl Default for WlrDirectStrategy {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl SessionStrategy for WlrDirectStrategy {
    fn name(&self) -> &'static str {
        "wlr-direct"
    }

    fn requires_initial_setup(&self) -> bool {
        // No user dialog required - direct protocol access
        false
    }

    fn supports_unattended_restore(&self) -> bool {
        // No session tokens needed - always available
        true
    }

    async fn create_session(&self) -> Result<Arc<dyn SessionHandle>> {
        info!("🚀 wlr_direct: Creating session with native Wayland protocols");

        let conn = Connection::connect_to_env()
            .context("Failed to connect to Wayland display. Ensure WAYLAND_DISPLAY is set.")?;

        info!("🔌 wlr_direct: Connected to Wayland display");

        let (keyboard, pointer, event_queue) =
            bind_protocols_and_create_devices(&conn, &self.keyboard_layout)
                .context("Failed to bind protocols and create virtual devices")?;

        info!("✅ wlr_direct: Virtual keyboard and pointer created successfully");

        let handle = WlrSessionHandleImpl {
            connection: conn,
            event_queue: Mutex::new(event_queue),
            keyboard,
            pointer,
            streams: std::sync::RwLock::new(vec![]),
            health_reporter: std::sync::OnceLock::new(),
        };

        Ok(Arc::new(handle))
    }

    async fn cleanup(&self, _session: &dyn SessionHandle) -> Result<()> {
        // Virtual devices are automatically destroyed when dropped
        info!("🔒 wlr_direct: Session cleanup complete");
        Ok(())
    }
}

/// wlr-direct session handle implementation
///
/// Implements the SessionHandle trait for wlroots direct protocol access.
///
/// # Input Injection
///
/// All input methods receive pre-translated inputs from the input handler:
/// - Keyboard: evdev keycodes (not RDP scancodes)
/// - Mouse buttons: evdev button codes (272-276)
/// - Mouse coordinates: Stream-relative, already transformed
///
/// The handle just forwards these to the virtual devices.
pub struct WlrSessionHandleImpl {
    connection: Connection,
    event_queue: Mutex<wayland_client::EventQueue<WlrState>>,
    keyboard: VirtualKeyboard,
    pointer: VirtualPointer,
    /// Stream info populated after ScreenCast starts (wlr-direct creates input
    /// devices before video streams are known).
    streams: std::sync::RwLock<Vec<StreamInfo>>,
    health_reporter: std::sync::OnceLock<HealthReporter>,
}

impl WlrSessionHandleImpl {
    /// Flush pending Wayland events
    ///
    /// Dispatches any pending protocol events and flushes the connection.
    /// This is non-blocking and only processes events already in the queue.
    fn flush(&self) -> Result<()> {
        let mut queue = self.event_queue.lock().unwrap();

        // Dispatch pending events (non-blocking)
        if let Err(e) = queue.dispatch_pending(&mut WlrState::new()) {
            warn!("⚠️  wlr_direct: Failed to dispatch pending events: {}", e);
            // Non-fatal - input events are one-way
        }

        // Flush connection to send queued requests
        if let Err(e) = self.connection.flush() {
            if let Some(reporter) = self.health_reporter.get() {
                reporter.report(HealthEvent::InputFailed {
                    reason: format!("wlr-direct flush failed: {e}"),
                    permanent: true,
                });
            }
            return Err(e).context("Failed to flush Wayland connection");
        }

        Ok(())
    }

    /// Find stream dimensions by node ID.
    ///
    /// Returns (width, height) for coordinate transformation.
    fn stream_extents(&self, stream_id: u32) -> Option<(u32, u32)> {
        let streams = self.streams.read().unwrap();
        streams
            .iter()
            .find(|s| s.node_id == stream_id)
            .map(|s| (s.width, s.height))
    }
}

#[async_trait]
impl SessionHandle for WlrSessionHandleImpl {
    fn set_health_reporter(&self, reporter: HealthReporter) {
        let _ = self.health_reporter.set(reporter);
    }

    fn pipewire_access(&self) -> PipeWireAccess {
        // wlr-direct does not provide video capture (input only)
        // Video would come from a separate strategy (Portal or wlr-screencopy)
        warn!(
            "⚠️  wlr_direct: pipewire_access() called but this strategy provides input only. \
             Video capture requires Portal ScreenCast or wlr-screencopy."
        );
        PipeWireAccess::NodeId(0)
    }

    fn streams(&self) -> Vec<StreamInfo> {
        self.streams.read().unwrap().clone()
    }

    fn set_streams(&self, streams: Vec<StreamInfo>) {
        debug!(
            "[wlr_direct] Stream info updated: {} stream(s)",
            streams.len()
        );
        for s in &streams {
            debug!(
                "[wlr_direct]   node_id={}, {}x{}",
                s.node_id, s.width, s.height
            );
        }
        *self.streams.write().unwrap() = streams;
    }

    fn session_type(&self) -> SessionType {
        SessionType::WlrDirect
    }

    async fn notify_keyboard_keycode(&self, keycode: i32, pressed: bool) -> Result<()> {
        let time = current_time_millis();
        let state = KeyState::from(pressed);

        self.keyboard.key(time, keycode as u32, state);

        self.flush()
            .context("Failed to flush keyboard event to compositor")?;

        Ok(())
    }

    async fn notify_pointer_motion_absolute(&self, stream_id: u32, x: f64, y: f64) -> Result<()> {
        // For MVP with input-only support, we don't have stream info from video capture
        // Use default screen dimensions or accept that motion may not work without video
        //
        // In a full implementation, this would be populated by the video capture strategy
        // For now, use a sensible default or the first stream if available

        let (x_extent, y_extent) = match self.stream_extents(stream_id) {
            Some(extents) => extents,
            None => {
                // No stream info yet or stream_id not found. Use first available
                // stream, or fall back to default dimensions.
                let streams = self.streams.read().unwrap();
                if let Some(first) = streams.first() {
                    (first.width, first.height)
                } else {
                    debug!(
                        "[wlr_direct] No stream info available (input-only mode). \
                         Using default 1920x1080 extents."
                    );
                    (1920_u32, 1080_u32)
                }
            }
        };

        let time = current_time_millis();

        self.pointer
            .motion_absolute(time, x as u32, y as u32, x_extent, y_extent);
        self.pointer.frame();

        self.flush()
            .context("Failed to flush pointer motion to compositor")?;

        Ok(())
    }

    async fn notify_pointer_button(&self, button: i32, pressed: bool) -> Result<()> {
        let time = current_time_millis();
        let state = ButtonState::from(pressed);

        self.pointer.button(time, button as u32, state);
        self.pointer.frame();

        self.flush()
            .context("Failed to flush pointer button event to compositor")?;

        Ok(())
    }

    async fn notify_pointer_axis(&self, dx: f64, dy: f64) -> Result<()> {
        let time = current_time_millis();

        // Set axis source to wheel (RDP scroll events are typically wheel-based)
        self.pointer.axis_source(AxisSource::Wheel);

        // Send axis events for non-zero deltas
        if dx.abs() > 0.01 {
            self.pointer.axis(time, Axis::HorizontalScroll, dx);
        }
        if dy.abs() > 0.01 {
            self.pointer.axis(time, Axis::VerticalScroll, dy);
        }

        self.pointer.frame();

        self.flush()
            .context("Failed to flush pointer axis event to compositor")?;

        Ok(())
    }

    fn portal_clipboard(&self) -> Option<ClipboardComponents> {
        // wlr-direct does not provide clipboard support
        // Caller must use FUSE approach or create separate Portal session
        None
    }
}

/// Bind to required Wayland protocols and create virtual devices
///
/// Uses registry_queue_init to enumerate globals and bind to required protocols.
fn bind_protocols_and_create_devices(
    conn: &Connection,
    keyboard_layout: &str,
) -> Result<(
    VirtualKeyboard,
    VirtualPointer,
    wayland_client::EventQueue<WlrState>,
)> {
    let (globals, mut event_queue) =
        registry_queue_init::<WlrState>(conn).context("Failed to initialize Wayland registry")?;

    let qh = event_queue.handle();

    let keyboard_manager: ZwpVirtualKeyboardManagerV1 = globals.bind(&qh, 1..=1, ()).context(
        "Failed to bind zwp_virtual_keyboard_manager_v1. \
             Compositor does not support virtual keyboard protocol.",
    )?;

    debug!("[wlr_direct] Bound zwp_virtual_keyboard_manager_v1");

    let pointer_manager: ZwlrVirtualPointerManagerV1 = globals.bind(&qh, 1..=2, ()).context(
        "Failed to bind zwlr_virtual_pointer_manager_v1. \
             Compositor does not support wlr virtual pointer protocol (requires wlroots 0.12+).",
    )?;

    debug!("[wlr_direct] Bound zwlr_virtual_pointer_manager_v1");

    let seat: WlSeat = globals
        .bind(&qh, 1..=8, ())
        .context("Failed to bind wl_seat. No seat available.")?;

    debug!("[wlr_direct] Bound wl_seat");

    let keyboard = VirtualKeyboard::new(&keyboard_manager, &seat, &qh, keyboard_layout)
        .context("Failed to create virtual keyboard")?;

    let pointer = VirtualPointer::new(&pointer_manager, &seat, &qh)
        .context("Failed to create virtual pointer")?;

    event_queue
        .roundtrip(&mut WlrState::new())
        .context("Failed to complete Wayland roundtrip for protocol setup")?;

    Ok((keyboard, pointer, event_queue))
}

/// Check if required protocols are available (used by is_available)
fn bind_protocols(conn: &Connection) -> Result<()> {
    let (globals, _event_queue) =
        registry_queue_init::<WlrState>(conn).context("Failed to initialize Wayland registry")?;

    let has_keyboard = globals.contents().with_list(|list| {
        list.iter()
            .any(|global| global.interface == "zwp_virtual_keyboard_manager_v1")
    });

    let has_pointer = globals.contents().with_list(|list| {
        list.iter()
            .any(|global| global.interface == "zwlr_virtual_pointer_manager_v1")
    });

    let has_seat = globals
        .contents()
        .with_list(|list| list.iter().any(|global| global.interface == "wl_seat"));

    if !has_keyboard {
        return Err(anyhow!("zwp_virtual_keyboard_manager_v1 not found"));
    }
    if !has_pointer {
        return Err(anyhow!("zwlr_virtual_pointer_manager_v1 not found"));
    }
    if !has_seat {
        return Err(anyhow!("wl_seat not found"));
    }

    Ok(())
}

/// Get current time in milliseconds since UNIX epoch
///
/// Used for event timestamps in the Wayland protocol.
fn current_time_millis() -> u32 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u32
}

// Implement Dispatch for all protocol objects with WlrState
impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for WlrState {
    fn event(
        _state: &mut Self,
        _proxy: &wl_registry::WlRegistry,
        _event: wl_registry::Event,
        _data: &GlobalListContents,
        _conn: &Connection,
        _qhandle: &QueueHandle<Self>,
    ) {
        // Registry events handled by registry_queue_init
    }
}

impl Dispatch<ZwpVirtualKeyboardManagerV1, ()> for WlrState {
    fn event(
        _state: &mut Self,
        _proxy: &ZwpVirtualKeyboardManagerV1,
        _event: <ZwpVirtualKeyboardManagerV1 as wayland_client::Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qhandle: &QueueHandle<Self>,
    ) {
        // No events expected from keyboard manager
    }
}

impl Dispatch<ZwlrVirtualPointerManagerV1, ()> for WlrState {
    fn event(
        _state: &mut Self,
        _proxy: &ZwlrVirtualPointerManagerV1,
        _event: <ZwlrVirtualPointerManagerV1 as wayland_client::Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qhandle: &QueueHandle<Self>,
    ) {
        // No events expected from pointer manager
    }
}

impl Dispatch<WlSeat, ()> for WlrState {
    fn event(
        _state: &mut Self,
        _proxy: &WlSeat,
        _event: <WlSeat as wayland_client::Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qhandle: &QueueHandle<Self>,
    ) {
        // Ignore seat events (capabilities, name, etc.)
    }
}

impl Dispatch<wayland_protocols_misc::zwp_virtual_keyboard_v1::client::zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1, ()> for WlrState {
    fn event(
        _state: &mut Self,
        _proxy: &wayland_protocols_misc::zwp_virtual_keyboard_v1::client::zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1,
        _event: <wayland_protocols_misc::zwp_virtual_keyboard_v1::client::zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1 as wayland_client::Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qhandle: &QueueHandle<Self>,
    ) {
        // No events expected from virtual keyboard
    }
}

impl Dispatch<wayland_protocols_wlr::virtual_pointer::v1::client::zwlr_virtual_pointer_v1::ZwlrVirtualPointerV1, ()> for WlrState {
    fn event(
        _state: &mut Self,
        _proxy: &wayland_protocols_wlr::virtual_pointer::v1::client::zwlr_virtual_pointer_v1::ZwlrVirtualPointerV1,
        _event: <wayland_protocols_wlr::virtual_pointer::v1::client::zwlr_virtual_pointer_v1::ZwlrVirtualPointerV1 as wayland_client::Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qhandle: &QueueHandle<Self>,
    ) {
        // No events expected from virtual pointer
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[ignore = "Requires wlroots compositor running"]
    async fn test_wlr_direct_availability() {
        let available = WlrDirectStrategy::is_available().await;
        println!("wlr-direct available: {}", available);
        // This will be true on Sway/Hyprland, false on GNOME
    }

    #[tokio::test]
    #[ignore = "Requires wlroots compositor running"]
    async fn test_create_session() {
        if !WlrDirectStrategy::is_available().await {
            println!("Skipping: wlr-direct not available on this compositor");
            return;
        }

        let strategy = WlrDirectStrategy::new();
        let session = strategy
            .create_session()
            .await
            .expect("Failed to create session");

        assert_eq!(session.session_type(), SessionType::WlrDirect);
    }

    #[test]
    fn test_current_time_millis() {
        let time = current_time_millis();
        assert!(time > 0);
        println!("Current time: {} ms", time);
    }
}
