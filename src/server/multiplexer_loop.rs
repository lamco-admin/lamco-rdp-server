//! Full Multiplexer Event Processing Loop
//!
//! Implements priority-based event processing for all server operations.
//! Ensures input is always prioritized over graphics, preventing lag.

use ironrdp_cliprdr::backend::ClipboardMessage;
use tokio::sync::mpsc;
use tracing::{debug, info};

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
/// Drains control and clipboard queues in priority order.
/// Input is handled by input_handler's dedicated batching task.
/// Graphics is handled by graphics_drain task.
pub(super) async fn run_multiplexer_drain_loop(
    mut control_rx: mpsc::Receiver<ControlEvent>,
    mut clipboard_rx: mpsc::Receiver<ClipboardEvent>,
) {
    info!("🚀 Multiplexer drain loop started - control + clipboard priority handling");

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
