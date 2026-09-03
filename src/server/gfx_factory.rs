//! GFX Server Factory for EGFX/H.264 Video Streaming
//!
//! This module implements `ironrdp_server::GfxServerFactory` to integrate
//! our EGFX handler with IronRDP's server infrastructure.
//!
//! # Architecture
//!
//! ```text
//! LamcoGfxFactory (implements GfxServerFactory)
//!       │
//!       ├─► Creates Arc<Mutex<GraphicsPipelineServer>>
//!       │
//!       ├─► Returns GfxDvcBridge for DrdynvcServer (handles client messages)
//!       │
//!       └─► Stores GfxServerHandle for display handler (frame sending)
//! ```
//!
//! # Hybrid Architecture
//!
//! This factory implements the "Hybrid" approach (Option E) for proactive EGFX
//! frame sending:
//!
//! 1. **GfxDvcBridge** - Wraps the GraphicsPipelineServer in Arc<Mutex<>>
//!    and implements DvcProcessor for the DrdynvcServer to use
//!
//! 2. **GfxServerHandle** - Clone of the Arc given to display handler for
//!    calling send_avc420_frame() directly
//!
//! 3. **ServerEvent::Egfx** - Routes the resulting DVC messages to the wire

use std::sync::{Arc, Mutex};

use ironrdp_egfx::server::{GraphicsPipelineHandler, GraphicsPipelineServer};
use ironrdp_graphics::zgfx::CompressionMode;
use ironrdp_server::{
    GfxDvcBridge, GfxServerFactory, GfxServerHandle, ServerEvent, ServerEventSender,
};
use tokio::sync::{RwLock, mpsc};

use crate::{
    egfx::LamcoGraphicsHandler, health::performance::EgfxSnapshot,
    runtime::metrics::MetricsCollector,
};

/// Factory for creating EGFX graphics pipeline handlers
///
/// This factory is passed to the RdpServer builder and creates
/// a shared `GraphicsPipelineServer` for each client connection.
///
/// # Platform Quirks
///
/// The factory accepts a `force_avc420_only` flag which is passed to the handler.
/// This is used when platform detection (e.g., RHEL 9) identifies that AVC444
/// produces visual artifacts. The handler will then disable AVC444 regardless
/// of client capability.
///
/// # Usage
///
/// ```ignore
/// // Check if platform has AVC444 quirk
/// let force_avc420 = capabilities.profile.has_quirk(&Quirk::ForceAvc420);
///
/// let gfx_factory = LamcoGfxFactory::with_quirks(width, height, force_avc420);
///
/// // Get handle for display handler before passing to RdpServer
/// let gfx_handle = gfx_factory.server_handle();
///
/// let server = RdpServer::builder()
///     .with_gfx_handler(gfx_factory)
///     // ...
///     .build();
///
/// // Display handler uses gfx_handle to send frames
/// display_handler.set_gfx_server(gfx_handle);
/// ```
pub struct LamcoGfxFactory {
    /// Initial desktop dimensions
    width: u16,
    height: u16,

    /// Shared state for checking handler readiness from other parts of the server
    handler_state: Arc<SharedHandlerState>,

    /// Shared GraphicsPipelineServer for proactive frame sending
    /// Created lazily on first call to build_server_with_handle()
    server_handle: Arc<RwLock<Option<GfxServerHandle>>>,

    /// Force AVC420-only mode due to platform quirks (e.g., RHEL 9)
    force_avc420_only: bool,

    /// Maximum frames in flight before backpressure
    max_frames_in_flight: u32,

    /// ZGFX compression mode for EGFX data
    compression_mode: CompressionMode,

    /// Metrics collector for EGFX pipeline data (passed to handler)
    metrics: Option<Arc<MetricsCollector>>,

    /// Shared EGFX snapshot for SnapshotCollector (passed to handler)
    egfx_snapshot: Option<Arc<parking_lot::RwLock<EgfxSnapshot>>>,

    /// Health reporter for EGFX channel state events (passed to handler)
    health_reporter: Option<crate::health::HealthReporter>,
}

/// Shared handler state accessible from display handler (lock-free)
///
/// Updated by `LamcoGraphicsHandler` callbacks via atomic stores, read by
/// `EgfxFrameSender` and display handler via atomic loads. No locks means
/// no contention between the synchronous DVC callback thread and the async
/// display pipeline loop.
///
/// Replaces the previous `Arc<RwLock<Option<HandlerState>>>` which caused
/// a race condition: `try_write()` in the sync callback would fail silently
/// when the async pipeline held a read lock, preventing state propagation.
pub struct SharedHandlerState {
    /// Whether EGFX channel is ready (capabilities negotiated)
    pub is_ready: std::sync::atomic::AtomicBool,
    /// Whether AVC420 (H.264 YUV420) codec is supported
    pub client_supports_avc420: std::sync::atomic::AtomicBool,
    /// Whether AVC444 (H.264 YUV444) codec is supported
    pub is_avc444_enabled: std::sync::atomic::AtomicBool,
    /// Whether a primary surface exists
    pub has_surface: std::sync::atomic::AtomicBool,
    /// Primary surface ID (valid only when has_surface is true)
    pub primary_surface_id: std::sync::atomic::AtomicU16,
    /// Set when the client sends a SECOND CapabilitiesAdvertise mid-session.
    ///
    /// mstsc emits a fresh CapsAdvertise (+ CacheImportOffer) when its EGFX
    /// decoder loses sync — typically after a long P-slice chain under load
    /// where one corrupt frame becomes unrecoverable. The expected server
    /// response is a full re-init: DeleteSurface + ResetGraphics +
    /// CreateSurface + MapSurfaceToOutput + fresh IDR. Without this, mstsc
    /// waits ~ms, then closes the Graphics DVC channel and RSTs the TCP
    /// connection.
    ///
    /// When this flag is set, the display loop performs the full re-init
    /// sequence and clears the flag.
    pub needs_full_reinit: std::sync::atomic::AtomicBool,
    /// Latest `total_frames_decoded` value from the client (MS-RDPEGFX 2.2.2.13).
    ///
    /// Updated on each FrameAcknowledge PDU. Used by display_handler to compute
    /// a "frame delay" = encoded_frames_total - total_frames_decoded, which is
    /// the most direct measure of how far behind the client decoder is.
    /// Reference: GNOME RD `GrdRdpGfxFrameController`.
    pub last_total_frames_decoded: std::sync::atomic::AtomicU32,
    /// Latest queue_depth reported by client (FrameAcknowledge.queueDepth).
    /// 0xFFFFFFFF means SUSPEND_FRAME_ACK per MS-RDPEGFX 2.2.4.3.
    pub last_client_queue_depth: std::sync::atomic::AtomicU32,
    /// Closed-loop EGFX flow controller (GrdRdpGfxFrameController equivalent).
    /// Both `LamcoGraphicsHandler::on_frame_ack_full` (FreeRDP callback thread)
    /// and the display loop (async runtime) touch it via this Mutex. Critical
    /// sections are short (a few atomic counter increments) so contention is
    /// minimal. The display loop checks `should_throttle()` before encoding;
    /// the handler calls `ack_frame()` on each inbound FrameAcknowledge.
    pub flow_controller: std::sync::Mutex<crate::egfx::flow_controller::FlowController>,
    /// Whether the connected client negotiated EGFX with the `AVC_DISABLED`
    /// flag set (RDPGFX Capability Set V10/V10.2-V10.7). In practice this
    /// identifies the Android Microsoft RD Client, which has two pointer
    /// rendering quirks other clients don't: it shows no cursor at all from
    /// `DefaultPointer` alone, and it renders `RGBAPointer`'s XOR mask rows
    /// vertically flipped relative to MS-RDPBCGR 2.2.9.1.1.4.4's bottom-up
    /// requirement. Used by the cursor pipeline to skip the normal
    /// bottom-up row flip for this connection.
    pub needs_android_pointer_updates: std::sync::atomic::AtomicBool,
}

impl SharedHandlerState {
    pub fn new() -> Self {
        Self {
            is_ready: std::sync::atomic::AtomicBool::new(false),
            client_supports_avc420: std::sync::atomic::AtomicBool::new(false),
            is_avc444_enabled: std::sync::atomic::AtomicBool::new(false),
            has_surface: std::sync::atomic::AtomicBool::new(false),
            primary_surface_id: std::sync::atomic::AtomicU16::new(0),
            needs_full_reinit: std::sync::atomic::AtomicBool::new(false),
            last_total_frames_decoded: std::sync::atomic::AtomicU32::new(0),
            last_client_queue_depth: std::sync::atomic::AtomicU32::new(0),
            flow_controller: std::sync::Mutex::new(
                crate::egfx::flow_controller::FlowController::new(
                    crate::egfx::flow_controller::FlowControllerConfig::default(),
                ),
            ),
            needs_android_pointer_updates: std::sync::atomic::AtomicBool::new(false),
        }
    }

    /// Reset all state (for new client connections)
    pub fn reset(&self) {
        self.is_ready
            .store(false, std::sync::atomic::Ordering::Release);
        self.client_supports_avc420
            .store(false, std::sync::atomic::Ordering::Release);
        self.is_avc444_enabled
            .store(false, std::sync::atomic::Ordering::Release);
        self.has_surface
            .store(false, std::sync::atomic::Ordering::Release);
        self.primary_surface_id
            .store(0, std::sync::atomic::Ordering::Release);
        self.needs_full_reinit
            .store(false, std::sync::atomic::Ordering::Release);
        self.last_total_frames_decoded
            .store(0, std::sync::atomic::Ordering::Release);
        self.last_client_queue_depth
            .store(0, std::sync::atomic::Ordering::Release);
        self.needs_android_pointer_updates
            .store(false, std::sync::atomic::Ordering::Release);
        if let Ok(mut fc) = self.flow_controller.lock() {
            fc.clear_all_unacked();
        }
    }
}

impl LamcoGfxFactory {
    pub fn new(width: u16, height: u16) -> Self {
        Self {
            width,
            height,
            handler_state: Arc::new(SharedHandlerState::new()),
            server_handle: Arc::new(RwLock::new(None)),
            force_avc420_only: false,
            max_frames_in_flight: 3,
            compression_mode: CompressionMode::Never,
            metrics: None,
            egfx_snapshot: None,
            health_reporter: None,
        }
    }

    /// Use this constructor when platform detection has identified quirks
    /// that affect codec selection (e.g., RHEL 9 AVC444 blur issue).
    pub fn with_quirks(width: u16, height: u16, force_avc420_only: bool) -> Self {
        Self {
            width,
            height,
            handler_state: Arc::new(SharedHandlerState::new()),
            server_handle: Arc::new(RwLock::new(None)),
            force_avc420_only,
            max_frames_in_flight: 3,
            compression_mode: CompressionMode::Never,
            metrics: None,
            egfx_snapshot: None,
            health_reporter: None,
        }
    }

    pub fn with_config(
        width: u16,
        height: u16,
        force_avc420_only: bool,
        max_frames_in_flight: u32,
        compression_mode: CompressionMode,
    ) -> Self {
        Self {
            width,
            height,
            handler_state: Arc::new(SharedHandlerState::new()),
            server_handle: Arc::new(RwLock::new(None)),
            force_avc420_only,
            max_frames_in_flight,
            compression_mode,
            metrics: None,
            egfx_snapshot: None,
            health_reporter: None,
        }
    }

    /// Attach a MetricsCollector and EGFX snapshot for performance monitoring.
    /// Called from server setup to enable metric recording in the handler.
    pub fn set_monitoring(
        &mut self,
        metrics: Arc<MetricsCollector>,
        egfx_snapshot: Arc<parking_lot::RwLock<EgfxSnapshot>>,
    ) {
        self.metrics = Some(metrics);
        self.egfx_snapshot = Some(egfx_snapshot);
    }

    /// Attach a HealthReporter for EGFX channel state events.
    pub fn set_health_reporter(&mut self, reporter: crate::health::HealthReporter) {
        self.health_reporter = Some(reporter);
    }

    /// Get shared reference to handler state
    ///
    /// This can be used by the display handler to check if EGFX is ready
    /// and which codecs are available.
    pub fn handler_state(&self) -> Arc<SharedHandlerState> {
        Arc::clone(&self.handler_state)
    }

    /// Get the shared GraphicsPipelineServer handle
    ///
    /// This returns the handle that was created by `build_server_with_handle()`.
    /// Use this to access the server for frame sending from the display handler.
    ///
    /// Returns `None` if `build_server_with_handle()` hasn't been called yet
    /// (i.e., the RDP connection hasn't started the channel attachment phase).
    pub fn server_handle(&self) -> Arc<RwLock<Option<GfxServerHandle>>> {
        Arc::clone(&self.server_handle)
    }
}

impl ServerEventSender for LamcoGfxFactory {
    fn set_sender(&mut self, _sender: mpsc::UnboundedSender<ServerEvent>) {
        // GFX factory doesn't need the server event sender directly;
        // EgfxFrameSender already has its own event_tx from server setup.
    }
}

impl Default for SharedHandlerState {
    fn default() -> Self {
        Self::new()
    }
}

impl GfxServerFactory for LamcoGfxFactory {
    fn build_gfx_handler(&self) -> Box<dyn GraphicsPipelineHandler> {
        let mut handler =
            LamcoGraphicsHandler::with_quirks(self.width, self.height, self.force_avc420_only);
        if let Some(ref m) = self.metrics {
            handler.set_metrics(Arc::clone(m));
        }
        if let Some(ref s) = self.egfx_snapshot {
            handler.set_egfx_snapshot(Arc::clone(s));
        }
        if let Some(ref r) = self.health_reporter {
            handler.set_health_reporter(r.clone());
        }
        Box::new(handler)
    }

    fn build_server_with_handle(&self) -> Option<(GfxDvcBridge, GfxServerHandle)> {
        // A new client connection invalidates all previous EGFX negotiation
        // state (codec capabilities, surface IDs, readiness). This factory and
        // its `handler_state` outlive individual connections (they sit on the
        // long-lived `RdpServer`), so without an explicit reset the reused
        // shared state still reads `is_ready`/`has_surface` from the *previous*
        // client. The send loop would then skip the new caps exchange and emit
        // P-slices referencing the old client's decoder state — mstsc closes the
        // Graphics DVC and RSTs. Resetting here pauses the send loop until the
        // new client completes capability exchange. The QEMU factory already
        // does this at its own connection boundary; the desktop path must match.
        self.handler_state.reset();

        // Handler updates handler_state when callbacks are invoked,
        // allowing EgfxFrameSender to check EGFX readiness
        let mut handler = LamcoGraphicsHandler::with_config(
            self.width,
            self.height,
            Arc::clone(&self.handler_state),
            self.force_avc420_only,
            self.max_frames_in_flight,
        );
        if let Some(ref m) = self.metrics {
            handler.set_metrics(Arc::clone(m));
        }
        if let Some(ref s) = self.egfx_snapshot {
            handler.set_egfx_snapshot(Arc::clone(s));
        }
        if let Some(ref r) = self.health_reporter {
            handler.set_health_reporter(r.clone());
        }

        // std::sync::Mutex (not tokio) because DvcProcessor trait
        // has synchronous methods that cannot use async locks
        let server = Arc::new(Mutex::new(GraphicsPipelineServer::with_compression(
            Box::new(handler),
            self.compression_mode,
        )));

        // Retry try_write in a spin loop. The pipeline may hold a brief read lock
        // on the shared handle (is_egfx_ready check). A single try_write failure
        // leaves the handle as None, breaking EGFX on reconnection.
        for attempt in 0..100 {
            if let Ok(mut handle_guard) = self.server_handle.try_write() {
                *handle_guard = Some(Arc::clone(&server));
                break;
            }
            if attempt == 99 {
                tracing::error!(
                    "Failed to acquire gfx_server_handle write lock after 100 attempts"
                );
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }

        let bridge = GfxDvcBridge::new(Arc::clone(&server));

        Some((bridge, server))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::Ordering;

    use super::*;

    #[test]
    fn test_shared_handler_state_defaults() {
        let state = SharedHandlerState::new();
        assert!(!state.is_ready.load(Ordering::Acquire));
        assert!(!state.client_supports_avc420.load(Ordering::Acquire));
        assert!(!state.is_avc444_enabled.load(Ordering::Acquire));
        assert!(!state.has_surface.load(Ordering::Acquire));
        assert_eq!(state.primary_surface_id.load(Ordering::Acquire), 0);
    }

    #[test]
    fn test_shared_handler_state_set_and_read() {
        let state = SharedHandlerState::new();
        state.is_ready.store(true, Ordering::Release);
        state.client_supports_avc420.store(true, Ordering::Release);
        state.has_surface.store(true, Ordering::Release);
        state.primary_surface_id.store(42, Ordering::Release);

        assert!(state.is_ready.load(Ordering::Acquire));
        assert!(state.client_supports_avc420.load(Ordering::Acquire));
        assert!(state.has_surface.load(Ordering::Acquire));
        assert_eq!(state.primary_surface_id.load(Ordering::Acquire), 42);
    }

    #[test]
    fn test_shared_handler_state_reset() {
        let state = SharedHandlerState::new();
        state.is_ready.store(true, Ordering::Release);
        state.client_supports_avc420.store(true, Ordering::Release);
        state.is_avc444_enabled.store(true, Ordering::Release);
        state.has_surface.store(true, Ordering::Release);
        state.primary_surface_id.store(99, Ordering::Release);

        state.reset();

        assert!(!state.is_ready.load(Ordering::Acquire));
        assert!(!state.client_supports_avc420.load(Ordering::Acquire));
        assert!(!state.is_avc444_enabled.load(Ordering::Acquire));
        assert!(!state.has_surface.load(Ordering::Acquire));
        assert_eq!(state.primary_surface_id.load(Ordering::Acquire), 0);
    }

    #[test]
    fn test_shared_handler_state_cross_thread() {
        use std::{sync::Arc, thread};

        let state = Arc::new(SharedHandlerState::new());
        let state_writer = Arc::clone(&state);
        let state_reader = Arc::clone(&state);

        let writer = thread::spawn(move || {
            state_writer.is_ready.store(true, Ordering::Release);
            state_writer
                .client_supports_avc420
                .store(true, Ordering::Release);
            state_writer.has_surface.store(true, Ordering::Release);
            state_writer.primary_surface_id.store(7, Ordering::Release);
        });

        writer.join().unwrap();

        // After writer completes, reader must see all updates
        assert!(state_reader.is_ready.load(Ordering::Acquire));
        assert!(state_reader.client_supports_avc420.load(Ordering::Acquire));
        assert!(state_reader.has_surface.load(Ordering::Acquire));
        assert_eq!(state_reader.primary_surface_id.load(Ordering::Acquire), 7);
    }
}
