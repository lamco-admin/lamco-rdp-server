//! RDP Input Handler Implementation
//!
//! Implements the IronRDP `RdpServerInputHandler` trait to forward input events
//! from RDP clients to the Wayland compositor via Portal RemoteDesktop API.
//!
//! # Overview
//!
//! This module bridges the synchronous IronRDP input event callbacks with the
//! asynchronous Portal API, providing complete keyboard and mouse input forwarding
//! with full scancode translation, modifier tracking, and coordinate transformation.
//!
//! # Architecture
//!
//! ```text
//! RDP Client                    WrdInputHandler                 Wayland
//! ━━━━━━━━━━                    ━━━━━━━━━━━━━━━                 ━━━━━━━
//!
//! Keyboard Event ─────────────> KeyboardEvent
//!   scancode=0x1E                     │
//!   pressed=true                      ├─> KeyboardHandler
//!                                     │     └─> Scancode translation
//!                                     │         (0x1E → evdev KEY_A)
//!                                     │
//!                                     ├─> Portal API
//!                                     │     └─> notify_keyboard_keycode()
//!                                     │
//!                                     └─────────────────────────> Input Stack
//!                                                                   └─> Key Press
//!
//! Mouse Event ────────────────> MouseEvent::Move
//!   x=960, y=540                     │
//!                                    ├─> CoordinateTransformer
//!                                    │     └─> RDP coords → Wayland coords
//!                                    │
//!                                    ├─> Portal API
//!                                    │     └─> notify_pointer_motion_absolute()
//!                                    │
//!                                    └─────────────────────────> Input Stack
//!                                                                  └─> Mouse Move
//! ```
//!
//! # Async/Sync Bridging
//!
//! IronRDP's `RdpServerInputHandler` trait has synchronous methods (`fn`, not `async fn`),
//! but Portal API calls are asynchronous. We bridge this gap by:
//!
//! 1. Trait method called synchronously by IronRDP
//! 2. Clone Arc references to shared state
//! 3. Spawn `tokio::spawn()` async task
//! 4. Task performs async Portal API calls
//! 5. Fire-and-forget (RDP doesn't expect acknowledgment for input events)
//!
//! This pattern ensures the synchronous trait method returns immediately while
//! Portal operations proceed asynchronously without blocking.
//!
//! # Example
//!
//! ```ignore
//! use lamco_rdp_server::server::WrdInputHandler;
//! use lamco_rdp_server::portal::RemoteDesktopManager;
//! use lamco_rdp_server::input::MonitorInfo;
//! use std::sync::Arc;
//!
//! let portal = Arc::new(RemoteDesktopManager::new(/* ... */).await?);
//! let session = portal.create_session().await?;
//! let monitors = vec![/* MonitorInfo instances */];
//!
//! let handler = WrdInputHandler::new(portal, session, monitors)?;
//!
//! // Handler is now ready to receive input events from IronRDP
//! // Events are automatically forwarded to Wayland via Portal
//! ```

use std::{
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Instant,
};

use ironrdp_server::{
    KeyboardEvent as IronKeyboardEvent, MouseButton as IronMouseButton,
    MouseEvent as IronMouseEvent, RdpServerInputHandler, TouchContactFlags as IronTouchFlags,
    TouchEventPdu,
};
use tokio::sync::{Mutex, mpsc};
use tracing::{debug, error, info, trace, warn};

use crate::input::{
    CoordinateTransformer, InputError, KeyboardHandler, MonitorInfo, MouseButton, MouseHandler,
    TouchContactFlags, TouchEvent as LamcoTouchEvent, TouchHandler,
};

/// Map a Unicode code point to an evdev keycode and whether Shift is needed.
/// Covers printable ASCII (0x20-0x7E) on US QWERTY layout.
fn unicode_to_evdev(cp: u16) -> Option<(u32, bool)> {
    // evdev keycodes from lamco-rdp-input::mapper::keycodes
    const KEY_SPACE: u32 = 57;
    const KEY_1: u32 = 2;
    const KEY_2: u32 = 3;
    const KEY_3: u32 = 4;
    const KEY_4: u32 = 5;
    const KEY_5: u32 = 6;
    const KEY_6: u32 = 7;
    const KEY_7: u32 = 8;
    const KEY_8: u32 = 9;
    const KEY_9: u32 = 10;
    const KEY_0: u32 = 11;
    const KEY_MINUS: u32 = 12;
    const KEY_EQUAL: u32 = 13;
    const KEY_TAB: u32 = 15;
    const KEY_Q: u32 = 16;
    const KEY_W: u32 = 17;
    const KEY_E: u32 = 18;
    const KEY_R: u32 = 19;
    const KEY_T: u32 = 20;
    const KEY_Y: u32 = 21;
    const KEY_U: u32 = 22;
    const KEY_I: u32 = 23;
    const KEY_O: u32 = 24;
    const KEY_P: u32 = 25;
    const KEY_LEFTBRACE: u32 = 26;
    const KEY_RIGHTBRACE: u32 = 27;
    const KEY_ENTER: u32 = 28;
    const KEY_A: u32 = 30;
    const KEY_S: u32 = 31;
    const KEY_D: u32 = 32;
    const KEY_F: u32 = 33;
    const KEY_G: u32 = 34;
    const KEY_H: u32 = 35;
    const KEY_J: u32 = 36;
    const KEY_K: u32 = 37;
    const KEY_L: u32 = 38;
    const KEY_SEMICOLON: u32 = 39;
    const KEY_APOSTROPHE: u32 = 40;
    const KEY_GRAVE: u32 = 41;
    const KEY_BACKSLASH: u32 = 43;
    const KEY_Z: u32 = 44;
    const KEY_X: u32 = 45;
    const KEY_C: u32 = 46;
    const KEY_V: u32 = 47;
    const KEY_B: u32 = 48;
    const KEY_N: u32 = 49;
    const KEY_M: u32 = 50;
    const KEY_COMMA: u32 = 51;
    const KEY_DOT: u32 = 52;
    const KEY_SLASH: u32 = 53;

    // (evdev_keycode, needs_shift)
    match cp {
        // Whitespace
        0x20 => Some((KEY_SPACE, false)),        // ' '
        0x09 => Some((KEY_TAB, false)),          // Tab
        0x0A | 0x0D => Some((KEY_ENTER, false)), // Newline / CR

        // Digits
        0x30 => Some((KEY_0, false)), // '0'
        0x31 => Some((KEY_1, false)), // '1'
        0x32 => Some((KEY_2, false)), // '2'
        0x33 => Some((KEY_3, false)), // '3'
        0x34 => Some((KEY_4, false)), // '4'
        0x35 => Some((KEY_5, false)), // '5'
        0x36 => Some((KEY_6, false)), // '6'
        0x37 => Some((KEY_7, false)), // '7'
        0x38 => Some((KEY_8, false)), // '8'
        0x39 => Some((KEY_9, false)), // '9'

        // Lowercase letters
        0x61 => Some((KEY_A, false)), // 'a'
        0x62 => Some((KEY_B, false)),
        0x63 => Some((KEY_C, false)),
        0x64 => Some((KEY_D, false)),
        0x65 => Some((KEY_E, false)),
        0x66 => Some((KEY_F, false)),
        0x67 => Some((KEY_G, false)),
        0x68 => Some((KEY_H, false)),
        0x69 => Some((KEY_I, false)),
        0x6A => Some((KEY_J, false)),
        0x6B => Some((KEY_K, false)),
        0x6C => Some((KEY_L, false)),
        0x6D => Some((KEY_M, false)),
        0x6E => Some((KEY_N, false)),
        0x6F => Some((KEY_O, false)),
        0x70 => Some((KEY_P, false)),
        0x71 => Some((KEY_Q, false)),
        0x72 => Some((KEY_R, false)),
        0x73 => Some((KEY_S, false)),
        0x74 => Some((KEY_T, false)),
        0x75 => Some((KEY_U, false)),
        0x76 => Some((KEY_V, false)),
        0x77 => Some((KEY_W, false)),
        0x78 => Some((KEY_X, false)),
        0x79 => Some((KEY_Y, false)),
        0x7A => Some((KEY_Z, false)), // 'z'

        // Uppercase letters (same keys, with Shift)
        0x41 => Some((KEY_A, true)), // 'A'
        0x42 => Some((KEY_B, true)),
        0x43 => Some((KEY_C, true)),
        0x44 => Some((KEY_D, true)),
        0x45 => Some((KEY_E, true)),
        0x46 => Some((KEY_F, true)),
        0x47 => Some((KEY_G, true)),
        0x48 => Some((KEY_H, true)),
        0x49 => Some((KEY_I, true)),
        0x4A => Some((KEY_J, true)),
        0x4B => Some((KEY_K, true)),
        0x4C => Some((KEY_L, true)),
        0x4D => Some((KEY_M, true)),
        0x4E => Some((KEY_N, true)),
        0x4F => Some((KEY_O, true)),
        0x50 => Some((KEY_P, true)),
        0x51 => Some((KEY_Q, true)),
        0x52 => Some((KEY_R, true)),
        0x53 => Some((KEY_S, true)),
        0x54 => Some((KEY_T, true)),
        0x55 => Some((KEY_U, true)),
        0x56 => Some((KEY_V, true)),
        0x57 => Some((KEY_W, true)),
        0x58 => Some((KEY_X, true)),
        0x59 => Some((KEY_Y, true)),
        0x5A => Some((KEY_Z, true)), // 'Z'

        // Symbols (unshifted)
        0x2D => Some((KEY_MINUS, false)),      // '-'
        0x3D => Some((KEY_EQUAL, false)),      // '='
        0x5B => Some((KEY_LEFTBRACE, false)),  // '['
        0x5D => Some((KEY_RIGHTBRACE, false)), // ']'
        0x5C => Some((KEY_BACKSLASH, false)),  // '\'
        0x3B => Some((KEY_SEMICOLON, false)),  // ';'
        0x27 => Some((KEY_APOSTROPHE, false)), // '\''
        0x60 => Some((KEY_GRAVE, false)),      // '`'
        0x2C => Some((KEY_COMMA, false)),      // ','
        0x2E => Some((KEY_DOT, false)),        // '.'
        0x2F => Some((KEY_SLASH, false)),      // '/'

        // Symbols (shifted)
        0x21 => Some((KEY_1, true)),          // '!'
        0x40 => Some((KEY_2, true)),          // '@'
        0x23 => Some((KEY_3, true)),          // '#'
        0x24 => Some((KEY_4, true)),          // '$'
        0x25 => Some((KEY_5, true)),          // '%'
        0x5E => Some((KEY_6, true)),          // '^'
        0x26 => Some((KEY_7, true)),          // '&'
        0x2A => Some((KEY_8, true)),          // '*'
        0x28 => Some((KEY_9, true)),          // '('
        0x29 => Some((KEY_0, true)),          // ')'
        0x5F => Some((KEY_MINUS, true)),      // '_'
        0x2B => Some((KEY_EQUAL, true)),      // '+'
        0x7B => Some((KEY_LEFTBRACE, true)),  // '{'
        0x7D => Some((KEY_RIGHTBRACE, true)), // '}'
        0x7C => Some((KEY_BACKSLASH, true)),  // '|'
        0x3A => Some((KEY_SEMICOLON, true)),  // ':'
        0x22 => Some((KEY_APOSTROPHE, true)), // '"'
        0x7E => Some((KEY_GRAVE, true)),      // '~'
        0x3C => Some((KEY_COMMA, true)),      // '<'
        0x3E => Some((KEY_DOT, true)),        // '>'
        0x3F => Some((KEY_SLASH, true)),      // '?'

        _ => None,
    }
}

fn input_injection_err(e: impl std::fmt::Display) -> InputError {
    InputError::PortalError(format!("Input injection error: {e}"))
}

/// Map an IronRDP mouse button to the evdev `BTN_*` code the session
/// backends expect and to our own [`MouseButton`] for handler state tracking.
fn map_iron_button(button: IronMouseButton) -> (MouseButton, i32) {
    match button {
        IronMouseButton::Left => (MouseButton::Left, 272), // BTN_LEFT
        IronMouseButton::Right => (MouseButton::Right, 273), // BTN_RIGHT
        IronMouseButton::Middle => (MouseButton::Middle, 274), // BTN_MIDDLE
        IronMouseButton::X1 => (MouseButton::Extra1, 275), // BTN_SIDE
        IronMouseButton::X2 => (MouseButton::Extra2, 276), // BTN_EXTRA
        other => {
            warn!(
                "Unhandled MouseButton variant {:?}, treating as Left",
                other
            );
            (MouseButton::Left, 272)
        }
    }
}

/// Lamco RDP Input Handler
///
/// Bridges IronRDP input events to our Portal-based input injection system.
/// This handler receives keyboard and mouse events from RDP clients and forwards
/// them to the Wayland compositor through the RemoteDesktop portal.
///
/// Since IronRDP's trait methods are synchronous but portal operations are async,
/// we use channels and spawned tasks to bridge the gap.
/// Input event for batching/multiplexing
#[derive(Debug)]
pub enum InputEvent {
    /// Keyboard event from RDP client
    Keyboard(IronKeyboardEvent),
    /// Mouse event from RDP client
    Mouse(IronMouseEvent),
    /// MS-RDPEI touch frame from RDP client. Never coalesced or dropped:
    /// unlike a mouse Move, a touch frame can carry a DOWN or UP transition
    /// whose loss would desync the per-contact state machine.
    Touch(TouchEventPdu),
}

/// Coalesce consecutive Move and RelMove events in a mouse-event batch.
///
/// Remote-desktop clients (notably mstsc) stream hundreds of mouse-move
/// events per millisecond during window manipulation. Each event becomes a
/// separate `wl_pointer.motion` request that must be flushed to the
/// compositor. Under software-rendering load the compositor cannot drain
/// the Wayland socket fast enough; flushes return EAGAIN/WouldBlock and the
/// input subsystem fails.
///
/// Coalescing rules:
/// - Consecutive `Move { x, y }` events collapse to a single Move with the
///   latest position. Intermediate trail positions are visually equivalent
///   to the final position from the compositor's perspective.
/// - Consecutive `RelMove { x, y }` events sum into a single RelMove with
///   the total delta. Equivalent end-effect.
/// - When a Move/RelMove run is interrupted by the other variant, the
///   pending run flushes before the new run starts.
/// - Non-move events (buttons, scroll, etc.) pass through unchanged, but
///   flush any pending Move/RelMove first so the button/scroll occurs at
///   the final cursor position rather than at an arbitrary intermediate
///   point.
fn coalesce_mouse_batch(events: Vec<IronMouseEvent>) -> Vec<IronMouseEvent> {
    if events.len() < 2 {
        return events;
    }
    fn flush_pending(
        out: &mut Vec<IronMouseEvent>,
        abs: &mut Option<(u16, u16)>,
        rel: &mut Option<(i32, i32)>,
    ) {
        if let Some((x, y)) = abs.take() {
            out.push(IronMouseEvent::Move { x, y });
        }
        if let Some((dx, dy)) = rel.take() {
            out.push(IronMouseEvent::RelMove { x: dx, y: dy });
        }
    }

    let mut out: Vec<IronMouseEvent> = Vec::with_capacity(events.len());
    let mut pending_abs: Option<(u16, u16)> = None;
    let mut pending_rel: Option<(i32, i32)> = None;

    for ev in events {
        match ev {
            IronMouseEvent::Move { x, y } => {
                if pending_rel.is_some() {
                    flush_pending(&mut out, &mut pending_abs, &mut pending_rel);
                }
                pending_abs = Some((x, y));
            }
            IronMouseEvent::RelMove { x, y } => {
                if pending_abs.is_some() {
                    flush_pending(&mut out, &mut pending_abs, &mut pending_rel);
                }
                let (dx, dy) = pending_rel.unwrap_or((0, 0));
                pending_rel = Some((dx.saturating_add(x), dy.saturating_add(y)));
            }
            other => {
                flush_pending(&mut out, &mut pending_abs, &mut pending_rel);
                out.push(other);
            }
        }
    }
    flush_pending(&mut out, &mut pending_abs, &mut pending_rel);
    out
}

/// Lamco RDP input handler that bridges IronRDP input events to Portal injection
///
/// Receives keyboard and mouse events from RDP clients and injects them
/// into the Wayland compositor via the Portal RemoteDesktop API.
pub struct LamcoInputHandler {
    /// Session handle for input injection (abstraction over Portal/Mutter)
    session_handle: Arc<dyn crate::session::SessionHandle>,

    /// Keyboard event handler (pub for multiplexer access)
    pub keyboard_handler: Arc<Mutex<KeyboardHandler>>,

    /// Mouse event handler (pub for multiplexer access)
    pub mouse_handler: Arc<Mutex<MouseHandler>>,

    /// Coordinate transformer for multi-monitor support (pub for multiplexer access)
    pub coordinate_transformer: Arc<Mutex<CoordinateTransformer>>,

    /// Touch contact state tracker (MS-RDPEI), shared with the RDPEI DVC handler
    pub touch_handler: Arc<Mutex<TouchHandler>>,

    /// Primary stream node ID for input injection (PipeWire node ID)
    primary_stream_id: u32,

    /// Input event queue sender (for multiplexer - bounded with drop policy)
    input_tx: mpsc::Sender<InputEvent>,
}

impl LamcoInputHandler {
    pub fn new(
        session_handle: Arc<dyn crate::session::SessionHandle>,
        monitors: Vec<MonitorInfo>,
        primary_stream_id: u32,
        input_tx: mpsc::Sender<InputEvent>,
        mut input_rx: mpsc::Receiver<InputEvent>,
        mut shutdown_rx: tokio::sync::broadcast::Receiver<()>,
    ) -> Result<Self, InputError> {
        let keyboard_handler = Arc::new(Mutex::new(KeyboardHandler::new()));
        let mouse_handler = Arc::new(Mutex::new(MouseHandler::new()));
        let touch_handler = Arc::new(Mutex::new(TouchHandler::new()));

        let coordinate_transformer = Arc::new(Mutex::new(CoordinateTransformer::new(monitors)?));

        debug!(
            "Input handler using PipeWire stream node ID: {}",
            primary_stream_id
        );

        // Start input batching task (10ms windows for responsive typing)
        // Receives from multiplexer input queue, batches, and sends to Portal
        let session_handle_clone = Arc::clone(&session_handle);
        let keyboard_clone = Arc::clone(&keyboard_handler);
        let mouse_clone = Arc::clone(&mouse_handler);
        let coord_clone = Arc::clone(&coordinate_transformer);
        let touch_clone = Arc::clone(&touch_handler);

        tokio::spawn(async move {
            let mut keyboard_batch = Vec::with_capacity(16);
            let mut mouse_batch = Vec::with_capacity(16);
            let mut touch_batch = Vec::with_capacity(16);
            let mut last_flush = Instant::now();
            let batch_interval = tokio::time::Duration::from_millis(10);

            // Rate-limit input injection errors to avoid log spam when the
            // portal session becomes unresponsive (e.g. PipeWire stream pauses)
            let consecutive_mouse_errors = AtomicU64::new(0);
            let consecutive_kbd_errors = AtomicU64::new(0);
            let consecutive_touch_errors = AtomicU64::new(0);

            loop {
                tokio::select! {
                    Some(event) = input_rx.recv() => {
                        match event {
                            InputEvent::Keyboard(kbd) => {
                                trace!("📥 Input queue: received keyboard event");
                                keyboard_batch.push(kbd);
                            }
                            InputEvent::Mouse(mouse) => {
                                trace!("📥 Input queue: received mouse event");
                                mouse_batch.push(mouse);
                            }
                            InputEvent::Touch(pdu) => {
                                trace!("📥 Input queue: received touch frame");
                                touch_batch.push(pdu);
                            }
                        }
                    }

                    () = tokio::time::sleep_until(tokio::time::Instant::from_std(last_flush + batch_interval)) => {
                        // Discard a queued batch instead of attempting it against a
                        // session the compositor has already torn down (PerConnection
                        // backends re-establish per client; there's a window between
                        // teardown and the next client's establish where input can
                        // still be queued here).
                        if !session_handle_clone.is_session_valid() {
                            let discarded = keyboard_batch.len() + mouse_batch.len() + touch_batch.len();
                            if discarded > 0 {
                                trace!(
                                    "🔄 Input batching: discarding {} queued events — session invalid",
                                    discarded
                                );
                                keyboard_batch.clear();
                                mouse_batch.clear();
                                touch_batch.clear();
                            }
                            last_flush = Instant::now();
                            continue;
                        }

                        // Process keyboard batch
                        if !keyboard_batch.is_empty() {
                            trace!("🔄 Input batching: flushing {} keyboard events", keyboard_batch.len());
                        }
                        for kbd_event in keyboard_batch.drain(..) {
                            if let Err(e) = Self::handle_keyboard_event_impl(
                                &session_handle_clone,
                                &keyboard_clone,
                                kbd_event
                            ).await {
                                let count = consecutive_kbd_errors.fetch_add(1, Ordering::Relaxed) + 1;
                                if count == 1 {
                                    warn!("Keyboard injection failed: {e}");
                                } else if count.is_power_of_two() {
                                    warn!("Keyboard injection failed ({count} consecutive): {e}");
                                }
                            } else {
                                let prev = consecutive_kbd_errors.swap(0, Ordering::Relaxed);
                                if prev > 1 {
                                    info!("Portal keyboard injection recovered after {prev} failures");
                                }
                            }
                        }

                        // Process mouse batch — coalesce consecutive Move/RelMove
                        // first to limit Wayland flush count under mstsc-style
                        // bursts (see coalesce_mouse_batch for rules).
                        let coalesced: Vec<IronMouseEvent> =
                            coalesce_mouse_batch(std::mem::take(&mut mouse_batch));
                        let coalesced_nonempty = !coalesced.is_empty();
                        if coalesced_nonempty {
                            trace!(
                                "🔄 Input batching: flushing {} mouse events (after coalesce)",
                                coalesced.len()
                            );
                        }
                        for mouse_event in coalesced {
                            if let Err(e) = Self::handle_mouse_event_impl(
                                &session_handle_clone,
                                &mouse_clone,
                                &coord_clone,
                                mouse_event,
                                primary_stream_id
                            ).await {
                                let count = consecutive_mouse_errors.fetch_add(1, Ordering::Relaxed) + 1;
                                if count == 1 {
                                    warn!("Mouse injection failed: {e}");
                                } else if count.is_power_of_two() {
                                    warn!("Mouse injection failed ({count} consecutive): {e}");
                                }
                            } else {
                                let prev = consecutive_mouse_errors.swap(0, Ordering::Relaxed);
                                if prev > 1 {
                                    info!("Portal mouse injection recovered after {prev} failures");
                                }
                            }
                        }
                        // Commit any pointer-device (button/scroll) events staged
                        // above as one atomic EIS frame. No-op for non-EIS
                        // strategies (each stage_* call already committed
                        // immediately via the trait's default passthrough).
                        if coalesced_nonempty
                            && let Err(e) = session_handle_clone.commit_input_batch().await {
                                warn!("Mouse input batch commit failed: {e}");
                            }

                        // Process touch batch. Never coalesced: each PDU can
                        // carry DOWN/UP transitions the per-contact state
                        // machine must see in order.
                        if !touch_batch.is_empty() {
                            trace!("🔄 Input batching: flushing {} touch frames", touch_batch.len());
                        }
                        for touch_pdu in touch_batch.drain(..) {
                            if let Err(e) = Self::handle_touch_event_impl(
                                &session_handle_clone,
                                &touch_clone,
                                &coord_clone,
                                touch_pdu,
                                primary_stream_id
                            ).await {
                                let count = consecutive_touch_errors.fetch_add(1, Ordering::Relaxed) + 1;
                                if count == 1 {
                                    warn!("Touch injection failed: {e}");
                                } else if count.is_power_of_two() {
                                    warn!("Touch injection failed ({count} consecutive): {e}");
                                }
                            } else {
                                let prev = consecutive_touch_errors.swap(0, Ordering::Relaxed);
                                if prev > 1 {
                                    info!("Portal touch injection recovered after {prev} failures");
                                }
                            }
                        }

                        last_flush = Instant::now();
                    }

                    _ = shutdown_rx.recv() => {
                        info!("🛑 Input batching task received shutdown signal");
                        break;
                    }
                }
            }

            let mouse_errs = consecutive_mouse_errors.load(Ordering::Relaxed);
            let kbd_errs = consecutive_kbd_errors.load(Ordering::Relaxed);
            let touch_errs = consecutive_touch_errors.load(Ordering::Relaxed);
            if mouse_errs > 0 || kbd_errs > 0 || touch_errs > 0 {
                info!(
                    "Input batching task stopped (pending errors: mouse={mouse_errs}, kbd={kbd_errs}, touch={touch_errs})"
                );
            } else {
                info!("Input batching task stopped");
            }
        });

        info!("Input batching task started (REAL task, 10ms flush interval)");

        Ok(Self {
            session_handle,
            keyboard_handler,
            mouse_handler,
            coordinate_transformer,
            touch_handler,
            primary_stream_id,
            input_tx,
        })
    }

    /// Clone of the input event queue sender, for feeding events into the
    /// same batching pipeline from outside the mouse/keyboard callbacks
    /// (currently used by the RDPEI DVC handler for touch frames).
    pub fn input_sender(&self) -> mpsc::Sender<InputEvent> {
        self.input_tx.clone()
    }

    /// Activate the input subsystem (deferred EIS creation).
    ///
    /// Called when the first RDP client connects. Delegates to the
    /// session handle's `activate_input()` which creates the EIS
    /// connection on-demand.
    pub async fn activate_input(&self) -> anyhow::Result<()> {
        self.session_handle.activate_input().await
    }

    /// Notify input handler that client reconnected
    ///
    /// Resets internal state to handle new client connection.
    /// Call this when reconnection is detected (e.g., display_updates channel recreated).
    pub async fn notify_reconnection(&self) {
        info!("🔄 Input handler: Client reconnected, resetting state");

        {
            let mut kbd = self.keyboard_handler.lock().await;
            *kbd = KeyboardHandler::new();
            debug!("Keyboard handler state reset");
        }

        {
            let mut mouse = self.mouse_handler.lock().await;
            *mouse = MouseHandler::new();
            debug!("Mouse handler state reset");
        }

        {
            let mut touch = self.touch_handler.lock().await;
            touch.reset();
            debug!("Touch handler state reset");
        }

        info!("✅ Input handler ready for reconnected client");
    }

    /// Update coordinate transformer when monitor configuration changes
    ///
    /// This should be called when the RDP client requests a different resolution
    /// or when monitor configuration changes.
    pub async fn update_monitors(&self, monitors: Vec<MonitorInfo>) -> Result<(), InputError> {
        let mut transformer = self.coordinate_transformer.lock().await;
        *transformer = CoordinateTransformer::new(monitors)?;
        debug!("Updated monitor configuration");
        Ok(())
    }

    /// Handle keyboard event implementation (static for batching task)
    async fn handle_keyboard_event_impl(
        session_handle: &Arc<dyn crate::session::SessionHandle>,
        keyboard_handler: &Arc<Mutex<KeyboardHandler>>,
        event: IronKeyboardEvent,
    ) -> Result<(), InputError> {
        let mut keyboard = keyboard_handler.lock().await;

        match event {
            IronKeyboardEvent::Pressed { code, extended } => {
                // Log V key specifically to trace Ctrl+V paste operations
                if code == 0x2F {
                    // V key scancode
                    info!(
                        "⌨️ V key pressed (scancode=0x{:02X}, extended={})",
                        code, extended
                    );
                }
                trace!("Keyboard pressed: code={}, extended={}", code, extended);

                let kbd_event = keyboard.handle_key_down(code as u16, extended, false)?;

                let keycode = match kbd_event {
                    crate::input::KeyboardEvent::KeyDown { keycode, .. }
                    | crate::input::KeyboardEvent::KeyRepeat { keycode, .. } => keycode,
                    crate::input::KeyboardEvent::KeyUp { keycode, .. } => {
                        // handle_key_down returned KeyUp (shouldn't happen but handle gracefully)
                        warn!(
                            "handle_key_down returned KeyUp for code {} - using keycode anyway",
                            code
                        );
                        keycode
                    }
                    #[expect(
                        unreachable_patterns,
                        reason = "defensive: future KeyboardEvent variants"
                    )]
                    other => {
                        error!("handle_key_down returned unexpected event: {:?}", other);
                        return Err(InputError::InvalidKeyEvent(format!(
                            "Unexpected event type: {other:?}"
                        )));
                    }
                };

                // Log V key injection to Portal
                if keycode == 47 {
                    // evdev KEY_V
                    info!(
                        "⌨️ Injecting V key press to Portal (evdev keycode={})",
                        keycode
                    );
                }

                session_handle
                    .notify_keyboard_keycode(keycode as i32, true)
                    .await
                    .map_err(input_injection_err)?;
            }

            IronKeyboardEvent::Released { code, extended } => {
                // Log V key releases
                if code == 0x2F {
                    // V key scancode
                    info!(
                        "⌨️ V key released (scancode=0x{:02X}, extended={})",
                        code, extended
                    );
                }
                trace!("Keyboard released: code={}, extended={}", code, extended);

                let kbd_event = keyboard.handle_key_up(code as u16, extended, false)?;

                let keycode = match kbd_event {
                    crate::input::KeyboardEvent::KeyUp { keycode, .. } => keycode,
                    _ => {
                        return Err(InputError::InvalidKeyEvent(
                            "Unexpected event type".to_string(),
                        ));
                    }
                };

                // Log V key injection release to Portal
                if keycode == 47 {
                    // evdev KEY_V
                    info!(
                        "⌨️ Injecting V key release to Portal (evdev keycode={})",
                        keycode
                    );
                }

                session_handle
                    .notify_keyboard_keycode(keycode as i32, false)
                    .await
                    .map_err(input_injection_err)?;
            }

            IronKeyboardEvent::UnicodePressed(unicode) => {
                if let Some((keycode, needs_shift)) = unicode_to_evdev(unicode) {
                    // Fast path: ASCII characters mapped to evdev keycodes
                    debug!(
                        "Unicode press 0x{:04X} -> evdev {} (shift={})",
                        unicode, keycode, needs_shift
                    );
                    // KEY_LEFTSHIFT = 42
                    if needs_shift {
                        session_handle
                            .notify_keyboard_keycode(42, true)
                            .await
                            .map_err(input_injection_err)?;
                    }
                    session_handle
                        .notify_keyboard_keycode(keycode as i32, true)
                        .await
                        .map_err(input_injection_err)?;
                } else {
                    // Keysym path: CJK, accented, and other non-ASCII characters.
                    // XKB Unicode keysyms: 0x01000000 + Unicode code point.
                    let keysym = 0x0100_0000_u32 + u32::from(unicode);
                    debug!("Unicode press 0x{:04X} -> keysym 0x{:08X}", unicode, keysym);
                    session_handle
                        .notify_keyboard_keysym(keysym, true)
                        .await
                        .map_err(input_injection_err)?;
                }
            }

            IronKeyboardEvent::UnicodeReleased(unicode) => {
                if let Some((keycode, needs_shift)) = unicode_to_evdev(unicode) {
                    debug!(
                        "Unicode release 0x{:04X} -> evdev {} (shift={})",
                        unicode, keycode, needs_shift
                    );
                    session_handle
                        .notify_keyboard_keycode(keycode as i32, false)
                        .await
                        .map_err(input_injection_err)?;
                    if needs_shift {
                        session_handle
                            .notify_keyboard_keycode(42, false)
                            .await
                            .map_err(input_injection_err)?;
                    }
                } else {
                    let keysym = 0x0100_0000_u32 + u32::from(unicode);
                    debug!(
                        "Unicode release 0x{:04X} -> keysym 0x{:08X}",
                        unicode, keysym
                    );
                    session_handle
                        .notify_keyboard_keysym(keysym, false)
                        .await
                        .map_err(input_injection_err)?;
                }
            }

            IronKeyboardEvent::Synchronize(flags) => {
                trace!("Keyboard synchronize: {:?}", flags);
                // Update toggle key states based on sync flags
                // The flags tell us the client's current Caps/Num/Scroll lock states
                // We should sync our local state but portal doesn't have direct sync API
                // This is handled implicitly when keys are pressed
            }
        }

        Ok(())
    }

    /// Handle mouse event with full error handling and logging
    /// Handle mouse event implementation (static for batching task)
    async fn handle_mouse_event_impl(
        session_handle: &Arc<dyn crate::session::SessionHandle>,
        mouse_handler: &Arc<Mutex<MouseHandler>>,
        coordinate_transformer: &Arc<Mutex<CoordinateTransformer>>,
        event: IronMouseEvent,
        stream_id: u32,
    ) -> Result<(), InputError> {
        let mut mouse = mouse_handler.lock().await;
        let mut transformer = coordinate_transformer.lock().await;

        match event {
            IronMouseEvent::Move { x, y } => {
                trace!("Mouse move: x={}, y={}", x, y);

                let mouse_event =
                    mouse.handle_absolute_move(x as u32, y as u32, &mut transformer)?;

                let (stream_x, stream_y) = match mouse_event {
                    crate::input::MouseEvent::Move { x, y, .. } => (x, y),
                    _ => {
                        return Err(InputError::InvalidMouseEvent(
                            "Unexpected event type".to_string(),
                        ));
                    }
                };

                session_handle
                    .notify_pointer_motion_absolute(stream_id, stream_x, stream_y)
                    .await
                    .map_err(input_injection_err)?;
            }

            IronMouseEvent::RelMove { x, y } => {
                trace!("Mouse relative move: dx={}, dy={}", x, y);

                let mouse_event = mouse.handle_relative_move(x, y, &mut transformer)?;

                let (stream_x, stream_y) = match mouse_event {
                    crate::input::MouseEvent::Move { x, y, .. } => (x, y),
                    _ => {
                        return Err(InputError::InvalidMouseEvent(
                            "Unexpected event type".to_string(),
                        ));
                    }
                };

                // We converted relative to absolute already
                session_handle
                    .notify_pointer_motion_absolute(stream_id, stream_x, stream_y)
                    .await
                    .map_err(input_injection_err)?;
            }

            IronMouseEvent::Button {
                x,
                y,
                button,
                pressed,
            } => {
                let (local_button, evdev_code) = map_iron_button(button);
                trace!(
                    "Mouse button: {:?} pressed={} at x={}, y={}",
                    local_button, pressed, x, y
                );

                let mouse_event = if pressed {
                    mouse.handle_button_down(
                        local_button,
                        Some((x as u32, y as u32)),
                        &mut transformer,
                    )?
                } else {
                    mouse.handle_button_up(
                        local_button,
                        Some((x as u32, y as u32)),
                        &mut transformer,
                    )?
                };

                // Position a touch/tap-style client never sent a preceding
                // Move for must be applied before the click, or the click
                // lands at the last stale cursor position instead of where
                // the client actually pressed (IronRDP#1466).
                let position = match mouse_event {
                    crate::input::MouseEvent::ButtonDown { position, .. }
                    | crate::input::MouseEvent::ButtonUp { position, .. } => position,
                    _ => {
                        return Err(InputError::InvalidMouseEvent(
                            "Unexpected event type".to_string(),
                        ));
                    }
                };
                if let Some((stream_x, stream_y)) = position {
                    session_handle
                        .notify_pointer_motion_absolute(stream_id, stream_x, stream_y)
                        .await
                        .map_err(input_injection_err)?;
                }

                // Staged, not committed here -- flushed as one atomic frame
                // with any other pointer-device (button/scroll) event from
                // this same coalesced batch once the caller's loop finishes
                // and calls commit_input_batch(). The MotionAbsolute reposition
                // above is a *different* EIS device (pointer_absolute), so it
                // is not part of this batch and still commits its own frame
                // immediately -- EIS `frame()` is per-device, so a
                // cross-device sequence can't be made atomic this way
                // regardless; sending the reposition first (as above) is
                // already the best available mitigation for that case.
                session_handle
                    .stage_pointer_button(evdev_code, pressed)
                    .await
                    .map_err(input_injection_err)?;
            }

            IronMouseEvent::ButtonRel {
                x,
                y,
                button,
                pressed,
            } => {
                let (local_button, evdev_code) = map_iron_button(button);
                trace!(
                    "Mouse button (relative source): {:?} pressed={} at x={}, y={}",
                    local_button, pressed, x, y
                );

                // x/y are the accumulated position MS-RDPBCGR 2.2.8.1.1.3.1.1.7
                // reports the delta was applied at; saturate rather than cast, since
                // a client's cumulative deltas going negative shouldn't wrap into a
                // huge position far off-screen.
                let position = Some((u32::try_from(x).unwrap_or(0), u32::try_from(y).unwrap_or(0)));

                let mouse_event = if pressed {
                    mouse.handle_button_down(local_button, position, &mut transformer)?
                } else {
                    mouse.handle_button_up(local_button, position, &mut transformer)?
                };

                // Same reasoning as the absolute Button case above: reposition
                // before the click so it doesn't land at a stale cursor position.
                let position = match mouse_event {
                    crate::input::MouseEvent::ButtonDown { position, .. }
                    | crate::input::MouseEvent::ButtonUp { position, .. } => position,
                    _ => {
                        return Err(InputError::InvalidMouseEvent(
                            "Unexpected event type".to_string(),
                        ));
                    }
                };
                if let Some((stream_x, stream_y)) = position {
                    session_handle
                        .notify_pointer_motion_absolute(stream_id, stream_x, stream_y)
                        .await
                        .map_err(input_injection_err)?;
                }

                session_handle
                    .stage_pointer_button(evdev_code, pressed)
                    .await
                    .map_err(input_injection_err)?;
            }

            IronMouseEvent::VerticalScroll { value } => {
                // RDP wheel is 120-units per notch. Forward as discrete detents:
                // EIS strategies emit true scroll_discrete, others fall back to
                // the continuous conversion in the trait default. Staged, not
                // committed -- see the Button arm's comment above.
                mouse.handle_scroll(0, value as i32)?;
                session_handle
                    .stage_pointer_axis_discrete(0, value as i32)
                    .await
                    .map_err(input_injection_err)?;
            }

            IronMouseEvent::HorizontalScroll { value } => {
                mouse.handle_scroll(value as i32, 0)?;
                session_handle
                    .stage_pointer_axis_discrete(value as i32, 0)
                    .await
                    .map_err(input_injection_err)?;
            }

            IronMouseEvent::Scroll { x, y } => {
                mouse.handle_scroll(x, y)?;
                session_handle
                    .stage_pointer_axis_discrete(x, y)
                    .await
                    .map_err(input_injection_err)?;
            }

            other => {
                warn!("Unhandled mouse event variant: {:?}", other);
            }
        }

        Ok(())
    }

    /// Handle an MS-RDPEI touch frame (static for batching task).
    async fn handle_touch_event_impl(
        session_handle: &Arc<dyn crate::session::SessionHandle>,
        touch_handler: &Arc<Mutex<TouchHandler>>,
        coordinate_transformer: &Arc<Mutex<CoordinateTransformer>>,
        pdu: TouchEventPdu,
        stream_id: u32,
    ) -> Result<(), InputError> {
        let mut touch = touch_handler.lock().await;
        let mut transformer = coordinate_transformer.lock().await;

        for frame in pdu.frames {
            for contact in frame.contacts {
                let flags = touch_contact_flags_from_iron(contact.contact_flags);
                let event = touch.handle_contact(
                    contact.contact_id,
                    contact.x,
                    contact.y,
                    flags,
                    &mut transformer,
                )?;

                match event {
                    Some(LamcoTouchEvent::Down { slot, x, y }) => {
                        session_handle
                            .notify_touch_down(stream_id, slot, x, y)
                            .await
                            .map_err(input_injection_err)?;
                    }
                    Some(LamcoTouchEvent::Motion { slot, x, y }) => {
                        session_handle
                            .notify_touch_motion(stream_id, slot, x, y)
                            .await
                            .map_err(input_injection_err)?;
                    }
                    Some(LamcoTouchEvent::Up { slot }) => {
                        session_handle
                            .notify_touch_up(slot)
                            .await
                            .map_err(input_injection_err)?;
                    }
                    None => {}
                }
            }
        }

        Ok(())
    }
}

/// Decode the MS-RDPEI wire `contactFlags` bit field into the plain booleans
/// [`TouchHandler::handle_contact`] expects — kept decoupled from any
/// IronRDP crate type, matching how mouse button decoding takes raw wire
/// values rather than a foreign PDU type.
fn touch_contact_flags_from_iron(flags: IronTouchFlags) -> TouchContactFlags {
    TouchContactFlags {
        down: flags.contains(IronTouchFlags::DOWN),
        update: flags.contains(IronTouchFlags::UPDATE),
        up: flags.contains(IronTouchFlags::UP),
        in_range: flags.contains(IronTouchFlags::INRANGE),
        in_contact: flags.contains(IronTouchFlags::INCONTACT),
        canceled: flags.contains(IronTouchFlags::CANCELED),
    }
}

impl RdpServerInputHandler for LamcoInputHandler {
    fn keyboard(&mut self, event: IronKeyboardEvent) {
        trace!("⌨️  Input multiplexer: routing keyboard to queue");
        if let Err(e) = self.input_tx.try_send(InputEvent::Keyboard(event)) {
            error!("Failed to queue keyboard event for batching: {}", e);
        }
    }

    fn mouse(&mut self, event: IronMouseEvent) {
        trace!("🖱️  Input multiplexer: routing mouse to queue");
        // Position events (Move/RelMove) are coalesce-safe: if the channel
        // is full the dropped event will be effectively replaced by the
        // next arriving position. Buttons and scrolls cannot be dropped
        // without semantic loss, so they remain ERROR.
        let is_position_event = matches!(
            &event,
            IronMouseEvent::Move { .. } | IronMouseEvent::RelMove { .. }
        );
        if let Err(e) = self.input_tx.try_send(InputEvent::Mouse(event)) {
            if is_position_event {
                trace!("Dropped queued mouse position event: {}", e);
            } else {
                error!("Failed to queue mouse event for batching: {}", e);
            }
        }
    }
}

/// RdpServer needs ownership but we want to share state
impl Clone for LamcoInputHandler {
    fn clone(&self) -> Self {
        Self {
            session_handle: Arc::clone(&self.session_handle),
            keyboard_handler: Arc::clone(&self.keyboard_handler),
            mouse_handler: Arc::clone(&self.mouse_handler),
            coordinate_transformer: Arc::clone(&self.coordinate_transformer),
            touch_handler: Arc::clone(&self.touch_handler),
            primary_stream_id: self.primary_stream_id,
            input_tx: self.input_tx.clone(),
        }
    }
}

/// Maps a client-negotiated keyboard layout identifier (KLID, the low word of
/// a Windows locale identifier; MS-RDPBCGR 2.2.1.3.2 `keyboardLayout`) to a
/// layout string understood by `lamco_rdp_input::KeyboardHandler::set_layout`.
///
/// Only locales with a real (non-empty) override table in `ScancodeMapper`
/// are mapped; anything else falls back to `"us"`, matching the pre-existing
/// default. CJK and Indic-phonetic input arrive as Unicode keyboard events
/// (`IronKeyboardEvent::UnicodePressed`/`UnicodeReleased`, handled separately
/// below) and are unaffected by this mapping either way.
fn klid_to_layout_str(klid: u32) -> &'static str {
    match klid {
        0x0409 => "us",
        0x0809 => "uk",
        0x0407 => "de",
        0x040c => "fr",
        0x080c => "be",
        0x0410 => "it",
        0x040a => "es",
        0x0816 => "pt",
        _ => "us",
    }
}

/// Applies the client's negotiated keyboard layout to the session's
/// [`KeyboardHandler`] as soon as it is known.
///
/// IronRDP surfaces the negotiated layout via
/// [`ironrdp_server::ConnectionHandler::on_connection_info`], fired once per
/// connection before the session loop starts. This handler is built fresh
/// per connection (mirroring the fresh [`LamcoInputHandler`] each connection
/// already gets), so it needs no connection-identity bookkeeping of its own.
pub struct KeyboardLayoutConnectionHandler {
    keyboard_handler: Arc<Mutex<KeyboardHandler>>,
}

impl KeyboardLayoutConnectionHandler {
    pub fn new(keyboard_handler: Arc<Mutex<KeyboardHandler>>) -> Self {
        Self { keyboard_handler }
    }
}

impl ironrdp_server::ConnectionHandler for KeyboardLayoutConnectionHandler {
    fn on_connection_info(&mut self, info: &ironrdp_server::ConnectionInfo) {
        let layout = klid_to_layout_str(info.keyboard_layout);
        match self.keyboard_handler.try_lock() {
            Ok(mut kbd) => {
                kbd.set_layout(layout);
                debug!(
                    keyboard_layout = info.keyboard_layout,
                    layout, "Applied negotiated keyboard layout"
                );
            }
            Err(_) => {
                // Fires once at connection setup, before any keystroke could
                // possibly be in flight; contention here would indicate a bug
                // elsewhere, not a real race. Fail open to the "us" default
                // already set at construction rather than blocking.
                warn!(
                    keyboard_layout = info.keyboard_layout,
                    layout, "Could not acquire keyboard handler lock to apply negotiated layout"
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {

    #[test]
    fn test_input_handler_clone() {
        // Verify clone compiles and works
        // Full tests require portal mocking
    }

    #[test]
    fn test_touch_contact_flags_from_iron_decodes_down_inrange_incontact() {
        use super::{IronTouchFlags, touch_contact_flags_from_iron};
        use crate::input::TouchContactFlags;

        let iron = IronTouchFlags::DOWN | IronTouchFlags::INRANGE | IronTouchFlags::INCONTACT;
        let decoded = touch_contact_flags_from_iron(iron);
        assert_eq!(
            decoded,
            TouchContactFlags {
                down: true,
                update: false,
                up: false,
                in_range: true,
                in_contact: true,
                canceled: false,
            }
        );
    }

    #[test]
    fn test_map_iron_button_matches_evdev_codes() {
        use super::{IronMouseButton, MouseButton, map_iron_button};

        assert_eq!(
            map_iron_button(IronMouseButton::Left),
            (MouseButton::Left, 272)
        );
        assert_eq!(
            map_iron_button(IronMouseButton::Right),
            (MouseButton::Right, 273)
        );
        assert_eq!(
            map_iron_button(IronMouseButton::Middle),
            (MouseButton::Middle, 274)
        );
        // X1/X2 are the extended side buttons (button 4/5), not Left/Right.
        assert_eq!(
            map_iron_button(IronMouseButton::X1),
            (MouseButton::Extra1, 275)
        );
        assert_eq!(
            map_iron_button(IronMouseButton::X2),
            (MouseButton::Extra2, 276)
        );
    }

    #[test]
    fn test_klid_to_layout_str_known_locales() {
        use super::klid_to_layout_str;

        assert_eq!(klid_to_layout_str(0x0409), "us");
        assert_eq!(klid_to_layout_str(0x0809), "uk");
        assert_eq!(klid_to_layout_str(0x0407), "de");
        assert_eq!(klid_to_layout_str(0x040c), "fr");
        assert_eq!(klid_to_layout_str(0x080c), "be");
        assert_eq!(klid_to_layout_str(0x0410), "it");
        assert_eq!(klid_to_layout_str(0x040a), "es");
        assert_eq!(klid_to_layout_str(0x0816), "pt");
    }

    #[test]
    fn test_klid_to_layout_str_unknown_falls_back_to_us() {
        use super::klid_to_layout_str;

        // 0x0412 (Korean) has no scancode-remap table; Korean input arrives as
        // Unicode keyboard events instead, so falling back to "us" is correct.
        assert_eq!(klid_to_layout_str(0x0412), "us");
        assert_eq!(klid_to_layout_str(0), "us");
    }

    #[test]
    fn test_unicode_to_evdev_ascii() {
        use super::unicode_to_evdev;

        // Space
        assert_eq!(unicode_to_evdev(0x20), Some((57, false)));
        // Lowercase 'a'
        assert_eq!(unicode_to_evdev(0x61), Some((30, false)));
        // Uppercase 'A' (shift)
        assert_eq!(unicode_to_evdev(0x41), Some((30, true)));
        // Digit '0'
        assert_eq!(unicode_to_evdev(0x30), Some((11, false)));
        // Exclamation '!' (shift+1)
        assert_eq!(unicode_to_evdev(0x21), Some((2, true)));
        // Tab
        assert_eq!(unicode_to_evdev(0x09), Some((15, false)));
        // Enter
        assert_eq!(unicode_to_evdev(0x0D), Some((28, false)));
    }

    #[test]
    fn test_unicode_to_evdev_symbols() {
        use super::unicode_to_evdev;

        assert_eq!(unicode_to_evdev(0x2D), Some((12, false))); // '-'
        assert_eq!(unicode_to_evdev(0x5F), Some((12, true))); // '_'
        assert_eq!(unicode_to_evdev(0x3D), Some((13, false))); // '='
        assert_eq!(unicode_to_evdev(0x2B), Some((13, true))); // '+'
        assert_eq!(unicode_to_evdev(0x5B), Some((26, false))); // '['
        assert_eq!(unicode_to_evdev(0x7B), Some((26, true))); // '{'
    }

    #[test]
    fn test_unicode_to_evdev_non_ascii_returns_none() {
        use super::unicode_to_evdev;

        // CJK characters should return None (handled by keysym path)
        assert_eq!(unicode_to_evdev(0x754C), None); // unicode codepoint beyond ASCII
        assert_eq!(unicode_to_evdev(0x4E16), None); // unicode codepoint beyond ASCII
        // Accented characters
        assert_eq!(unicode_to_evdev(0x00E9), None); // 'e' with acute
        // High values
        assert_eq!(unicode_to_evdev(0xD83D), None); // high surrogate
    }

    #[test]
    fn test_unicode_keysym_encoding() {
        // Verify the XKB Unicode keysym formula: 0x01000000 + code_point
        let unicode: u16 = 0x754C;
        let keysym = 0x0100_0000_u32 + u32::from(unicode);
        assert_eq!(keysym, 0x0100_754C);

        let unicode: u16 = 0x4E16;
        let keysym = 0x0100_0000_u32 + u32::from(unicode);
        assert_eq!(keysym, 0x0100_4E16);

        // ASCII 'A' would be 0x01000041, but we use evdev for ASCII
        let unicode: u16 = 0x0041;
        let keysym = 0x0100_0000_u32 + u32::from(unicode);
        assert_eq!(keysym, 0x0100_0041);
    }

    #[test]
    fn test_unicode_full_ascii_coverage() {
        use super::unicode_to_evdev;

        // Every printable ASCII character (0x20-0x7E) should map to something
        for cp in 0x20u16..=0x7E {
            assert!(
                unicode_to_evdev(cp).is_some(),
                "ASCII 0x{:02X} ('{}') should have a mapping",
                cp,
                char::from(cp as u8)
            );
        }
    }
}
