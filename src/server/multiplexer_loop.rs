//! Full Multiplexer Event Processing Loop
//!
//! Implements priority-based event processing for all server operations.
//! Ensures input is always prioritized over graphics, preventing lag.

use ironrdp_cliprdr::backend::ClipboardMessage;
use ironrdp_server::{KeyboardEvent as IronKeyboardEvent, MouseEvent as IronMouseEvent};
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex};
use tracing::{debug, info};

use crate::input::{CoordinateTransformer, KeyboardHandler, MouseHandler};
use crate::portal::RemoteDesktopManager;

/// Control event for session management
#[derive(Debug)]
pub(super) enum ControlEvent {
    Quit(String),
    SetCredentials(ironrdp_server::Credentials),
}

/// Clipboard event for bidirectional sync
#[derive(Debug)]
pub(super) enum ClipboardEvent {
    Message(ClipboardMessage),
}

/// Full multiplexer event processing task
///
/// Drains control and clipboard queues in priority order
/// Note: Input is handled by input_handler's dedicated batching task
///       Graphics is handled by graphics_drain task
pub(super) async fn run_multiplexer_drain_loop(
    mut control_rx: mpsc::Receiver<ControlEvent>,
    mut clipboard_rx: mpsc::Receiver<ClipboardEvent>,
    _portal: Arc<RemoteDesktopManager>,
    _keyboard_handler: Arc<Mutex<KeyboardHandler>>,
    _mouse_handler: Arc<Mutex<MouseHandler>>,
    _coord_transformer: Arc<Mutex<CoordinateTransformer>>,
    _session: Arc<
        Mutex<
            ashpd::desktop::Session<
                'static,
                ashpd::desktop::remote_desktop::RemoteDesktop<'static>,
            >,
        >,
    >,
    _primary_stream_id: u32,
) {
    debug!(" Multiplexer drain loop started - control + clipboard priority handling");

    let mut stats_control = 0u64;
    let mut stats_clipboard = 0u64;

    loop {
        // Small sleep to prevent busy-loop
        tokio::time::sleep(tokio::time::Duration::from_micros(100)).await;

        // PRIORITY 1: Control events (session management)
        if let Ok(control) = control_rx.try_recv() {
            stats_control += 1;
            match control {
                ControlEvent::Quit(reason) => {
                    info!("🛑 Quit event received: {}", reason);
                    break;
                }
                ControlEvent::SetCredentials(creds) => {
                    info!("🔑 Credentials updated: {}", creds.username);
                }
            }
        }

        // PRIORITY 2: Clipboard events
        if let Ok(clipboard) = clipboard_rx.try_recv() {
            stats_clipboard += 1;
            match clipboard {
                ClipboardEvent::Message(_msg) => {
                    debug!("📋 Clipboard event processed via multiplexer");
                }
            }
        }
    }

    info!(
        "📊 Multiplexer final stats: control={}, clipboard={}",
        stats_control, stats_clipboard
    );
}

/// Process keyboard event (delegates to WrdInputHandler logic)
async fn process_keyboard_event(
    portal: &RemoteDesktopManager,
    keyboard_handler: &Arc<Mutex<KeyboardHandler>>,
    session: &Arc<
        Mutex<
            ashpd::desktop::Session<
                'static,
                ashpd::desktop::remote_desktop::RemoteDesktop<'static>,
            >,
        >,
    >,
    event: IronKeyboardEvent,
    _stream_id: u32,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    use crate::input::KeyboardEvent;

    let mut keyboard = keyboard_handler.lock().await;
    let session_guard = session.lock().await;

    match event {
        IronKeyboardEvent::Pressed { code, extended } => {
            let kbd_event = keyboard
                .handle_key_down(code as u16, extended, false)
                .map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)?;

            let keycode = match kbd_event {
                KeyboardEvent::KeyDown { keycode, .. }
                | KeyboardEvent::KeyRepeat { keycode, .. } => keycode,
                KeyboardEvent::KeyUp { keycode, .. } => {
                    portal
                        .notify_keyboard_keycode(&session_guard, keycode as i32, false)
                        .await?;
                    return Ok(());
                }
            };

            portal
                .notify_keyboard_keycode(&session_guard, keycode as i32, true)
                .await?;
        }
        IronKeyboardEvent::Released { code, extended } => {
            let kbd_event = keyboard
                .handle_key_up(code as u16, extended, false)
                .map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)?;

            if let KeyboardEvent::KeyUp { keycode, .. } = kbd_event {
                portal
                    .notify_keyboard_keycode(&session_guard, keycode as i32, false)
                    .await?;
            }
        }
        _ => {}
    }

    Ok(())
}

/// Process mouse event (delegates to WrdInputHandler logic)
async fn process_mouse_event(
    portal: &RemoteDesktopManager,
    mouse_handler: &Arc<Mutex<MouseHandler>>,
    coord_transformer: &Arc<Mutex<CoordinateTransformer>>,
    session: &Arc<
        Mutex<
            ashpd::desktop::Session<
                'static,
                ashpd::desktop::remote_desktop::RemoteDesktop<'static>,
            >,
        >,
    >,
    event: IronMouseEvent,
    stream_id: u32,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    use crate::input::{MouseButton, MouseEvent as WrdMouseEvent};

    let mut mouse = mouse_handler.lock().await;
    let mut transformer = coord_transformer.lock().await;
    let session_guard = session.lock().await;

    match event {
        IronMouseEvent::Move { x, y } => {
            let mouse_event = mouse
                .handle_absolute_move(x as u32, y as u32, &mut transformer)
                .map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)?;

            let (stream_x, stream_y) = match mouse_event {
                WrdMouseEvent::Move { x, y, .. } => (x, y),
                _ => return Ok(()),
            };

            portal
                .notify_pointer_motion_absolute(&session_guard, stream_id, stream_x, stream_y)
                .await?;
        }
        IronMouseEvent::LeftPressed => {
            mouse
                .handle_button_down(MouseButton::Left)
                .map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)?;
            portal
                .notify_pointer_button(&session_guard, 272, true)
                .await?; // BTN_LEFT
        }
        IronMouseEvent::LeftReleased => {
            mouse
                .handle_button_up(MouseButton::Left)
                .map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)?;
            portal
                .notify_pointer_button(&session_guard, 272, false)
                .await?;
        }
        IronMouseEvent::RightPressed => {
            mouse
                .handle_button_down(MouseButton::Right)
                .map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)?;
            portal
                .notify_pointer_button(&session_guard, 273, true)
                .await?; // BTN_RIGHT
        }
        IronMouseEvent::RightReleased => {
            mouse
                .handle_button_up(MouseButton::Right)
                .map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)?;
            portal
                .notify_pointer_button(&session_guard, 273, false)
                .await?;
        }
        IronMouseEvent::MiddlePressed => {
            mouse
                .handle_button_down(MouseButton::Middle)
                .map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)?;
            portal
                .notify_pointer_button(&session_guard, 274, true)
                .await?; // BTN_MIDDLE
        }
        IronMouseEvent::MiddleReleased => {
            mouse
                .handle_button_up(MouseButton::Middle)
                .map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)?;
            portal
                .notify_pointer_button(&session_guard, 274, false)
                .await?;
        }
        IronMouseEvent::VerticalScroll { value } => {
            portal
                .notify_pointer_axis(&session_guard, 0.0, value as f64 * 15.0)
                .await?;
        }
        _ => {}
    }

    Ok(())
}
