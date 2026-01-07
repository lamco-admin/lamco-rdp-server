//! Graphics Frame Drain Task
//!
//! Implements Phase 1 of the event multiplexer: Graphics queue with drop/coalesce policy.
//! This prevents graphics congestion from blocking input, control, or clipboard events.
//!
//! # Architecture
//!
//! ```text
//! PipeWire Thread
//!      │
//!      ├─> Display Handler (frame processing)
//!      │
//!      ▼
//! Graphics Queue (bounded 4, try_send with drop)
//!      │
//!      ├─> Graphics Drain Task (this module)
//!      │     └─> Coalesce multiple queued frames → keep only latest
//!      │
//!      ▼
//! IronRDP DisplayUpdate Channel
//!      │
//!      └─> RemoteFX encoding → RDP client
//! ```
//!
//! # QoS Benefits
//!
//! - **Non-blocking:** PipeWire never blocks on full queue (try_send)
//! - **Coalescing:** Multiple queued frames reduced to single latest frame
//! - **Isolation:** Graphics congestion cannot affect other subsystems
//! - **Statistics:** Track drops and coalescing for monitoring

use ironrdp_server::DisplayUpdate;
use tokio::sync::mpsc;
use tracing::{debug, info, trace, warn};

use crate::server::event_multiplexer::GraphicsFrame;

/// Statistics for graphics drain task
#[derive(Debug, Clone, Default)]
pub(super) struct GraphicsDrainStats {
    /// Total frames received from queue
    pub frames_received: u64,
    /// Frames coalesced (dropped because newer frame available)
    pub frames_coalesced: u64,
    /// Frames sent to IronRDP
    pub frames_sent: u64,
}

/// Start the graphics drain task
///
/// This task continuously drains the graphics queue, coalescing multiple frames
/// into a single latest frame, then converts to IronRDP format and sends.
///
/// # Arguments
///
/// * `graphics_rx` - Receiver for graphics frames (bounded 4)
/// * `update_sender` - Sender for IronRDP display updates
///
/// # Returns
///
/// A join handle for the spawned task
pub(super) fn start_graphics_drain_task(
    mut graphics_rx: mpsc::Receiver<GraphicsFrame>,
    update_sender: mpsc::Sender<DisplayUpdate>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        debug!(" Graphics drain task started (Phase 1 multiplexer)");
        let mut stats = GraphicsDrainStats::default();

        loop {
            // Wait for at least one frame
            let mut latest_frame = match graphics_rx.recv().await {
                Some(frame) => {
                    stats.frames_received += 1;
                    trace!(
                        "📥 Graphics queue: received frame {}",
                        stats.frames_received
                    );
                    frame
                }
                None => {
                    info!("Graphics channel closed, drain task exiting");
                    break;
                }
            };

            // Coalesce: Drain any additional queued frames, keep only latest
            let mut coalesced_count = 0u32;
            while let Ok(newer_frame) = graphics_rx.try_recv() {
                stats.frames_received += 1;
                coalesced_count += 1;
                latest_frame = newer_frame;
            }

            if coalesced_count > 0 {
                stats.frames_coalesced += coalesced_count as u64;
                trace!(
                    "🔄 Graphics queue: coalesced {} frames (keeping latest)",
                    coalesced_count
                );
                if stats.frames_coalesced % 100 == 0 {
                    info!(
                        "📊 Graphics coalescing: {} frames coalesced total",
                        stats.frames_coalesced
                    );
                }
            }

            // Send already-converted IronBitmapUpdate directly (no double conversion!)
            let update = DisplayUpdate::Bitmap(latest_frame.iron_bitmap);

            if let Err(e) = update_sender.send(update).await {
                warn!("Failed to send display update: {}", e);
                // Channel closed, exit task
                return;
            }

            stats.frames_sent += 1;
            if stats.frames_sent % 100 == 0 {
                debug!(
                    "📊 Graphics drain stats: received={}, coalesced={}, sent={}",
                    stats.frames_received, stats.frames_coalesced, stats.frames_sent
                );
            }
        }

        info!(
            "📊 Graphics drain task final stats: received={}, coalesced={}, sent={}",
            stats.frames_received, stats.frames_coalesced, stats.frames_sent
        );
    })
}

// Removed convert_to_iron_format - no longer needed!
// GraphicsFrame now contains pre-converted IronBitmapUpdate
// This eliminates double conversion overhead
