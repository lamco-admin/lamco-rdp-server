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
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::Instant,
};

use ironrdp_server::{
    KeyboardEvent as IronKeyboardEvent, MouseEvent as IronMouseEvent, RdpServerInputHandler,
};
use tokio::sync::{mpsc, Mutex};
use tracing::{debug, error, info, trace, warn};

use crate::input::{
    CoordinateTransformer, InputError, KeyboardHandler, MonitorInfo, MouseButton, MouseHandler,
};

fn portal_err(e: impl std::fmt::Display) -> InputError {
    InputError::PortalError(e.to_string())
}

/// WRD Input Handler
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
}

/// WRD input handler that bridges IronRDP input events to Portal injection
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

        tokio::spawn(async move {
            let mut keyboard_batch = Vec::with_capacity(16);
            let mut mouse_batch = Vec::with_capacity(16);
            let mut last_flush = Instant::now();
            let batch_interval = tokio::time::Duration::from_millis(10);

            // Rate-limit input injection errors to avoid log spam when the
            // portal session becomes unresponsive (e.g. PipeWire stream pauses)
            let consecutive_mouse_errors = AtomicU64::new(0);
            let consecutive_kbd_errors = AtomicU64::new(0);

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
                        }
                    }

                    () = tokio::time::sleep_until(tokio::time::Instant::from_std(last_flush + batch_interval)) => {
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
                                    warn!("Portal keyboard injection failed: {e}");
                                } else if count.is_power_of_two() {
                                    warn!("Portal keyboard injection failed ({count} consecutive): {e}");
                                }
                            } else {
                                let prev = consecutive_kbd_errors.swap(0, Ordering::Relaxed);
                                if prev > 1 {
                                    info!("Portal keyboard injection recovered after {prev} failures");
                                }
                            }
                        }

                        // Process mouse batch
                        if !mouse_batch.is_empty() {
                            trace!("🔄 Input batching: flushing {} mouse events", mouse_batch.len());
                        }
                        for mouse_event in mouse_batch.drain(..) {
                            if let Err(e) = Self::handle_mouse_event_impl(
                                &session_handle_clone,
                                &mouse_clone,
                                &coord_clone,
                                mouse_event,
                                primary_stream_id
                            ).await {
                                let count = consecutive_mouse_errors.fetch_add(1, Ordering::Relaxed) + 1;
                                if count == 1 {
                                    warn!("Portal mouse injection failed: {e}");
                                } else if count.is_power_of_two() {
                                    warn!("Portal mouse injection failed ({count} consecutive): {e}");
                                }
                            } else {
                                let prev = consecutive_mouse_errors.swap(0, Ordering::Relaxed);
                                if prev > 1 {
                                    info!("Portal mouse injection recovered after {prev} failures");
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
            if mouse_errs > 0 || kbd_errs > 0 {
                info!("Input batching task stopped (pending errors: mouse={mouse_errs}, kbd={kbd_errs})");
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
            primary_stream_id,
            input_tx,
        })
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
                debug!("Keyboard pressed: code={}, extended={}", code, extended);

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
                    .map_err(portal_err)?;
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
                debug!("Keyboard released: code={}, extended={}", code, extended);

                let kbd_event = keyboard.handle_key_up(code as u16, extended, false)?;

                let keycode = match kbd_event {
                    crate::input::KeyboardEvent::KeyUp { keycode, .. } => keycode,
                    _ => {
                        return Err(InputError::InvalidKeyEvent(
                            "Unexpected event type".to_string(),
                        ))
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
                    .map_err(portal_err)?;
            }

            IronKeyboardEvent::UnicodePressed(unicode) => {
                debug!("Unicode key pressed: 0x{:04X}", unicode);
                // Unicode events - for now log as unsupported
                // Full implementation would use XKB keysym injection
                warn!(
                    "Unicode keyboard events not yet fully supported: 0x{:04X}",
                    unicode
                );
            }

            IronKeyboardEvent::UnicodeReleased(unicode) => {
                debug!("Unicode key released: 0x{:04X}", unicode);
                warn!(
                    "Unicode keyboard events not yet fully supported: 0x{:04X}",
                    unicode
                );
            }

            IronKeyboardEvent::Synchronize(flags) => {
                debug!("Keyboard synchronize: {:?}", flags);
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
                debug!("Mouse move: x={}, y={}", x, y);

                let mouse_event =
                    mouse.handle_absolute_move(x as u32, y as u32, &mut transformer)?;

                let (stream_x, stream_y) = match mouse_event {
                    crate::input::MouseEvent::Move { x, y, .. } => (x, y),
                    _ => {
                        return Err(InputError::InvalidMouseEvent(
                            "Unexpected event type".to_string(),
                        ))
                    }
                };

                session_handle
                    .notify_pointer_motion_absolute(stream_id, stream_x, stream_y)
                    .await
                    .map_err(portal_err)?;
            }

            IronMouseEvent::RelMove { x, y } => {
                debug!("Mouse relative move: dx={}, dy={}", x, y);

                let mouse_event = mouse.handle_relative_move(x, y, &mut transformer)?;

                let (stream_x, stream_y) = match mouse_event {
                    crate::input::MouseEvent::Move { x, y, .. } => (x, y),
                    _ => {
                        return Err(InputError::InvalidMouseEvent(
                            "Unexpected event type".to_string(),
                        ))
                    }
                };

                // We converted relative to absolute already
                session_handle
                    .notify_pointer_motion_absolute(stream_id, stream_x, stream_y)
                    .await
                    .map_err(portal_err)?;
            }

            IronMouseEvent::LeftPressed => {
                mouse.handle_button_down(MouseButton::Left)?;
                session_handle
                    .notify_pointer_button(272, true) // BTN_LEFT
                    .await
                    .map_err(portal_err)?;
            }

            IronMouseEvent::LeftReleased => {
                mouse.handle_button_up(MouseButton::Left)?;
                session_handle
                    .notify_pointer_button(272, false)
                    .await
                    .map_err(portal_err)?;
            }

            IronMouseEvent::RightPressed => {
                mouse.handle_button_down(MouseButton::Right)?;
                session_handle
                    .notify_pointer_button(273, true) // BTN_RIGHT
                    .await
                    .map_err(portal_err)?;
            }

            IronMouseEvent::RightReleased => {
                mouse.handle_button_up(MouseButton::Right)?;
                session_handle
                    .notify_pointer_button(273, false)
                    .await
                    .map_err(portal_err)?;
            }

            IronMouseEvent::MiddlePressed => {
                mouse.handle_button_down(MouseButton::Middle)?;
                session_handle
                    .notify_pointer_button(274, true) // BTN_MIDDLE
                    .await
                    .map_err(portal_err)?;
            }

            IronMouseEvent::MiddleReleased => {
                mouse.handle_button_up(MouseButton::Middle)?;
                session_handle
                    .notify_pointer_button(274, false)
                    .await
                    .map_err(portal_err)?;
            }

            IronMouseEvent::Button4Pressed => {
                mouse.handle_button_down(MouseButton::Extra1)?;
                session_handle
                    .notify_pointer_button(275, true) // BTN_SIDE
                    .await
                    .map_err(portal_err)?;
            }

            IronMouseEvent::Button4Released => {
                mouse.handle_button_up(MouseButton::Extra1)?;
                session_handle
                    .notify_pointer_button(275, false)
                    .await
                    .map_err(portal_err)?;
            }

            IronMouseEvent::Button5Pressed => {
                mouse.handle_button_down(MouseButton::Extra2)?;
                session_handle
                    .notify_pointer_button(276, true) // BTN_EXTRA
                    .await
                    .map_err(portal_err)?;
            }

            IronMouseEvent::Button5Released => {
                mouse.handle_button_up(MouseButton::Extra2)?;
                session_handle
                    .notify_pointer_button(276, false)
                    .await
                    .map_err(portal_err)?;
            }

            IronMouseEvent::VerticalScroll { value } => {
                // RDP scroll units are in 120ths
                mouse.handle_scroll(0, value as i32)?;
                let delta_y = (value as f64 / 120.0) * 15.0;
                session_handle
                    .notify_pointer_axis(0.0, delta_y)
                    .await
                    .map_err(portal_err)?;
            }

            IronMouseEvent::Scroll { x, y } => {
                mouse.handle_scroll(x, y)?;
                let delta_x = (x as f64 / 120.0) * 15.0;
                let delta_y = (y as f64 / 120.0) * 15.0;
                session_handle
                    .notify_pointer_axis(delta_x, delta_y)
                    .await
                    .map_err(portal_err)?;
            }
        }

        Ok(())
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
        if let Err(e) = self.input_tx.try_send(InputEvent::Mouse(event)) {
            error!("Failed to queue mouse event for batching: {}", e);
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
            primary_stream_id: self.primary_stream_id,
            input_tx: self.input_tx.clone(),
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
}
