//! RDP Display Handler Implementation
//!
//! Implements the IronRDP `RdpServerDisplay` and `RdpServerDisplayUpdates` traits
//! to provide video frames from PipeWire to RDP clients.
//!
//! # Overview
//!
//! This module implements the video streaming pipeline from Wayland compositor to
//! RDP clients, handling frame capture, format conversion, and efficient streaming.
//!
//! # Architecture
//!
//! ```text
//! Wayland Compositor
//!        │
//!        ├─> Portal ScreenCast API
//!        │
//!        ▼
//! PipeWire Streams (one per monitor)
//!        │
//!        ├─> PipeWireThreadManager
//!        │     └─> Frame extraction via process() callback
//!        │
//!        ▼
//! Frame Channel (std::sync::mpsc)
//!        │
//!        ├─> Display Handler (async task)
//!        │     ├─> BitmapConverter (VideoFrame → RDP bitmap)
//!        │     └─> Format mapping (BGRA/RGB → IronRDP formats)
//!        │
//!        ▼
//! DisplayUpdate Channel (tokio::mpsc)
//!        │
//!        ├─> IronRDP Server
//!        │     └─> RemoteFX encoding
//!        │
//!        ▼
//! RDP Client Display
//! ```
//!
//! # Frame Processing Pipeline
//!
//! 1. **Capture:** PipeWire thread extracts frame from buffer
//! 2. **Transfer:** Frame sent via channel (zero-copy Arc)
//! 3. **Convert:** BitmapConverter transforms to RDP format
//! 4. **Map:** Pixel formats mapped to IronRDP types
//! 5. **Stream:** DisplayUpdate sent to IronRDP
//! 6. **Encode:** IronRDP applies RemoteFX compression
//! 7. **Transmit:** Sent to RDP client over TLS
//!
//! # Pixel Format Handling
//!
//! The handler supports multiple pixel formats with intelligent conversion:
//!
//! - **BgrX32** → IronRDP::BgrX32 (direct mapping)
//! - **Bgr24** → IronRDP::XBgr32 (upsample to 32-bit)
//! - **Rgb16** → IronRDP::XRgb32 (upsample to 32-bit)
//! - **Rgb15** → IronRDP::XRgb32 (upsample to 32-bit)
//!
//! # Performance Characteristics
//!
//! - **Frame latency:** <3ms (PipeWire → IronRDP)
//! - **Channel capacity:** 64 frames buffered
//! - **Frame rate:** Non-blocking, supports up to 144Hz
//! - **Memory:** Zero-copy where possible (Arc<Vec<u8>>)

use std::{
    num::{NonZeroU16, NonZeroUsize},
    os::fd::OwnedFd,
    sync::Arc,
    time::Instant,
};

use anyhow::Result;
use bytes::Bytes;
use ironrdp_server::{
    BitmapUpdate as IronBitmapUpdate, DesktopSize, DisplayUpdate, GfxServerHandle,
    PixelFormat as IronPixelFormat, RdpServerDisplay, RdpServerDisplayUpdates, ServerEvent,
};
use tokio::sync::{mpsc, Mutex, RwLock};
use tracing::{debug, error, info, trace, warn};

use crate::{
    damage::{DamageConfig, DamageDetector, DamageRegion},
    egfx::{Avc420Encoder, Avc444Encoder, ColorSpaceConfig, EncoderConfig},
    performance::{AdaptiveFpsController, EncodingDecision, LatencyGovernor, LatencyMode},
    pipewire::{PipeWireThreadCommand, PipeWireThreadManager, VideoFrame},
    portal::StreamInfo,
    server::{
        egfx_sender::EgfxFrameSender, event_multiplexer::GraphicsFrame, gfx_factory::HandlerState,
        input_handler::LamcoInputHandler,
    },
    services::{ServiceId, ServiceRegistry},
    video::{BitmapConverter, BitmapUpdate, RdpPixelFormat},
};

/// Client-initiated resize request
///
/// Sent from `request_layout()` (sync context) to the pipeline loop (async)
/// via a bounded sync channel. The pipeline coalesces multiple requests
/// and executes the resize sequence.
struct ResizeRequest {
    width: u16,
    height: u16,
}

/// Video encoder abstraction for codec-agnostic frame encoding
///
/// Supports both AVC420 (standard H.264 4:2:0) and AVC444 (premium H.264 4:4:4).
/// The codec is selected at runtime based on client capability negotiation.
enum VideoEncoder {
    /// Standard H.264 with 4:2:0 chroma subsampling
    Avc420(Avc420Encoder),
    /// Premium H.264 with 4:4:4 chroma via dual-stream encoding
    Avc444(Avc444Encoder),
}

/// Result of encoding a frame - varies by codec
enum EncodedVideoFrame {
    /// Single H.264 stream (AVC420)
    Single(Vec<u8>),
    /// Dual H.264 streams (AVC444: main + auxiliary)
    /// Phase 1: aux is now Option for bandwidth optimization
    Dual {
        main: Vec<u8>,
        aux: Option<Vec<u8>>, // Optional for aux omission
    },
}

impl VideoEncoder {
    /// Encode a BGRA frame to H.264
    ///
    /// Returns the encoded frame data, or None if the encoder skipped the frame.
    fn encode_bgra(
        &mut self,
        bgra_data: &[u8],
        width: u32,
        height: u32,
        timestamp_ms: u64,
    ) -> Result<Option<EncodedVideoFrame>, crate::egfx::EncoderError> {
        match self {
            VideoEncoder::Avc420(encoder) => encoder
                .encode_bgra(bgra_data, width, height, timestamp_ms)
                .map(|opt| opt.map(|frame| EncodedVideoFrame::Single(frame.data))),
            VideoEncoder::Avc444(encoder) => encoder
                .encode_bgra(bgra_data, width, height, timestamp_ms)
                .map(|opt| {
                    opt.map(|frame| EncodedVideoFrame::Dual {
                        main: frame.stream1_data,
                        aux: frame.stream2_data,
                    })
                }),
        }
    }

    /// Get codec name for logging
    fn codec_name(&self) -> &'static str {
        match self {
            VideoEncoder::Avc420(_) => "AVC420",
            VideoEncoder::Avc444(_) => "AVC444",
        }
    }

    /// Request IDR keyframe (for PLI or manual recovery)
    ///
    /// Forces the next encoded frame to be a full IDR keyframe,
    /// clearing any accumulated compression artifacts.
    #[expect(
        dead_code,
        reason = "PLI-triggered IDR not yet wired to RDP event loop"
    )]
    fn request_idr(&mut self) {
        match self {
            VideoEncoder::Avc420(encoder) => encoder.force_keyframe(),
            VideoEncoder::Avc444(encoder) => encoder.request_idr(),
        }
    }

    /// Check if periodic IDR is due (non-consuming)
    /// Used to bypass damage detection and send full frame when IDR fires
    fn is_periodic_idr_due(&self) -> bool {
        match self {
            VideoEncoder::Avc420(_) => false, // AVC420 doesn't have periodic IDR
            VideoEncoder::Avc444(encoder) => encoder.is_periodic_idr_due(),
        }
    }
}

/// Frame rate regulator using token bucket algorithm
///
/// Ensures smooth video delivery by limiting frame rate to target FPS.
/// Uses token bucket to allow brief bursts while maintaining average rate.
struct FrameRateRegulator {
    /// Target frames per second
    target_fps: u32,
    /// Interval between frames
    #[expect(dead_code, reason = "used in debug logging and rate calculation")]
    frame_interval: std::time::Duration,
    /// Last frame send time
    last_frame_time: Instant,
    /// Token budget for burst handling (allows brief spikes)
    token_budget: f32,
    /// Maximum tokens that can accumulate
    max_tokens: f32,
}

impl FrameRateRegulator {
    fn new(target_fps: u32) -> Self {
        Self {
            target_fps,
            frame_interval: std::time::Duration::from_micros(1_000_000 / target_fps as u64),
            last_frame_time: Instant::now(),
            token_budget: 1.0,
            max_tokens: 2.0, // Allow 2-frame burst
        }
    }

    /// Check if a frame should be sent based on rate limiting
    /// Returns true if frame should be sent, false if it should be dropped
    fn should_send_frame(&mut self) -> bool {
        let now = Instant::now();
        let elapsed = now.duration_since(self.last_frame_time);

        // CRITICAL: Update last_frame_time on EVERY call, not just when sending
        // Otherwise dropped frames cause time to accumulate and earn too many tokens
        self.last_frame_time = now;

        // Add tokens based on elapsed time
        let tokens_earned = elapsed.as_secs_f32() * self.target_fps as f32;
        self.token_budget = (self.token_budget + tokens_earned).min(self.max_tokens);

        // Check if we have budget to send this frame
        if self.token_budget >= 1.0 {
            self.token_budget -= 1.0;
            true
        } else {
            // Drop frame - too fast
            false
        }
    }
}

/// RDP Display Handler
///
/// Provides the display size and update stream to IronRDP server.
/// Manages the video pipeline from PipeWire capture to RDP transmission.
///
/// # EGFX Support
///
/// When EGFX/H.264 is negotiated, frames are encoded with OpenH264 and sent
/// through the EGFX channel for better quality and compression. Falls back
/// to RemoteFX when H.264 is not available.
pub struct LamcoDisplayHandler {
    /// Current desktop size
    size: Arc<RwLock<DesktopSize>>,

    /// PipeWire thread manager
    pipewire_thread: Arc<Mutex<PipeWireThreadManager>>,

    /// Bitmap converter for RDP format conversion
    bitmap_converter: Arc<Mutex<BitmapConverter>>,

    /// Display update sender (for creating update streams to IronRDP)
    update_sender: mpsc::Sender<DisplayUpdate>,

    /// Display update receiver (wrapped for cloning)
    update_receiver: Arc<Mutex<Option<mpsc::Receiver<DisplayUpdate>>>>,

    /// Graphics queue sender (for priority multiplexing)
    graphics_tx: Option<mpsc::Sender<GraphicsFrame>>,

    /// Monitor configuration from streams
    stream_info: Vec<StreamInfo>,

    // === EGFX/H.264 Support ===
    /// Shared GFX server handle for EGFX frame sending
    /// Populated by GfxFactory after channel attachment
    gfx_server_handle: Arc<RwLock<Option<GfxServerHandle>>>,

    /// Handler state for checking EGFX readiness
    gfx_handler_state: Arc<RwLock<Option<HandlerState>>>,

    /// Server event sender for routing EGFX messages
    /// Set after server is built (via set_server_event_sender)
    server_event_tx: Arc<RwLock<Option<mpsc::UnboundedSender<ServerEvent>>>>,

    /// Server configuration (for feature flags and settings)
    config: Arc<crate::config::Config>,

    /// Service registry for compositor-aware feature decisions
    service_registry: Arc<ServiceRegistry>,

    /// EGFX initialization flag - set to true when a new client needs EGFX setup
    ///
    /// This flag is checked by the pipeline to determine if EGFX surface setup
    /// (ResetGraphics, CreateSurface, MapSurfaceToOutput) needs to be performed.
    /// It's reset to `true` when a client reconnects so the new client gets
    /// proper EGFX initialization.
    egfx_needs_init: Arc<std::sync::atomic::AtomicBool>,

    /// Input handler reference for reconnection notification
    /// When client reconnects, we notify input handler to reset internal state
    input_handler: Arc<RwLock<Option<LamcoInputHandler>>>,

    /// Clipboard manager reference for disconnect cleanup
    /// When client disconnects (detected via reconnection), we clear Portal clipboard
    clipboard_manager:
        Arc<RwLock<Option<Arc<tokio::sync::Mutex<crate::clipboard::ClipboardOrchestrator>>>>>,

    /// Resize request sender (sync, used from request_layout() in blocking context)
    resize_tx: std::sync::mpsc::SyncSender<ResizeRequest>,

    /// Resize request receiver (taken by pipeline loop on first start)
    resize_rx: Arc<std::sync::Mutex<Option<std::sync::mpsc::Receiver<ResizeRequest>>>>,

    /// Last resize request timestamp for debouncing
    last_resize_time: std::sync::Mutex<Instant>,

    /// Whether a client is actively connected and consuming frames.
    /// Set true on new connection (in `updates()`), false on disconnect.
    /// The pipeline loop checks this to avoid encoding/sending frames to nobody.
    client_active: Arc<std::sync::atomic::AtomicBool>,

    /// Health reporter for forwarding PipeWire stream state to health monitor
    health_reporter: Arc<RwLock<Option<crate::health::HealthReporter>>>,
}

impl LamcoDisplayHandler {
    #[expect(
        clippy::too_many_arguments,
        reason = "display handler needs pipeline components at construction"
    )]
    pub async fn new(
        initial_width: u16,
        initial_height: u16,
        pipewire_fd: OwnedFd,
        stream_info: Vec<StreamInfo>,
        graphics_tx: Option<mpsc::Sender<GraphicsFrame>>,
        gfx_server_handle: Option<Arc<RwLock<Option<GfxServerHandle>>>>,
        gfx_handler_state: Option<Arc<RwLock<Option<HandlerState>>>>,
        config: Arc<crate::config::Config>,
        service_registry: Arc<ServiceRegistry>,
    ) -> Result<Self> {
        let size = Arc::new(RwLock::new(DesktopSize {
            width: initial_width,
            height: initial_height,
        }));

        let pipewire_thread = Arc::new(Mutex::new(
            PipeWireThreadManager::new(pipewire_fd)
                .map_err(|e| anyhow::anyhow!("Failed to create PipeWire thread: {e}"))?,
        ));

        for (idx, stream) in stream_info.iter().enumerate() {
            let config = lamco_pipewire::StreamConfig {
                name: format!("monitor-{idx}"),
                width: stream.size.0,
                height: stream.size.1,
                framerate: 60,
                use_dmabuf: true,
                buffer_count: 3,
                preferred_format: Some(lamco_pipewire::PixelFormat::BGRx),
            };

            let (response_tx, response_rx) = std::sync::mpsc::sync_channel(1);
            let cmd = PipeWireThreadCommand::CreateStream {
                stream_id: stream.node_id,
                node_id: stream.node_id,
                config,
                response_tx,
            };

            pipewire_thread
                .lock()
                .await
                .send_command(cmd)
                .map_err(|e| anyhow::anyhow!("Failed to send create stream command: {e}"))?;

            response_rx
                .recv_timeout(std::time::Duration::from_secs(5))
                .map_err(|_| anyhow::anyhow!("Timeout creating stream"))?
                .map_err(|e| anyhow::anyhow!("Stream creation failed: {e}"))?;

            debug!("Stream {} created successfully", stream.node_id);
        }

        let bitmap_converter = Arc::new(Mutex::new(BitmapConverter::new(
            initial_width,
            initial_height,
        )));

        let (update_sender, update_receiver) = mpsc::channel(64);
        let update_receiver = Arc::new(Mutex::new(Some(update_receiver)));

        let gfx_server_handle = gfx_server_handle.unwrap_or_else(|| Arc::new(RwLock::new(None)));
        let gfx_handler_state = gfx_handler_state.unwrap_or_else(|| Arc::new(RwLock::new(None)));

        debug!(
            "Display handler created: {}x{}, {} streams, EGFX={}",
            initial_width,
            initial_height,
            stream_info.len(),
            gfx_server_handle
                .try_read()
                .map(|g| g.is_some())
                .unwrap_or(false)
        );

        // Bounded channel for client-initiated resize requests
        // Capacity 4: enough to absorb a burst without blocking, pipeline coalesces
        let (resize_tx, resize_rx) = std::sync::mpsc::sync_channel(4);

        Ok(Self {
            size,
            pipewire_thread,
            bitmap_converter,
            update_sender,
            update_receiver,
            graphics_tx, // Passed from constructor for Phase 1 multiplexer
            stream_info,
            gfx_server_handle,
            gfx_handler_state,
            server_event_tx: Arc::new(RwLock::new(None)),
            config,           // Store config for feature flags
            service_registry, // Service-aware feature decisions
            egfx_needs_init: Arc::new(std::sync::atomic::AtomicBool::new(true)), // New client needs EGFX init
            input_handler: Arc::new(RwLock::new(None)), // Set later via set_input_handler()
            clipboard_manager: Arc::new(RwLock::new(None)), // Set later via set_clipboard_manager()
            resize_tx,
            resize_rx: Arc::new(std::sync::Mutex::new(Some(resize_rx))),
            last_resize_time: std::sync::Mutex::new(
                Instant::now()
                    .checked_sub(std::time::Duration::from_secs(10))
                    .unwrap_or(Instant::now()),
            ),
            client_active: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            health_reporter: Arc::new(RwLock::new(None)),
        })
    }

    /// Set input handler reference for reconnection notifications
    ///
    /// Must be called after input handler is created to enable reconnection reset.
    pub async fn set_input_handler(
        &self,
        handler: Arc<crate::server::input_handler::LamcoInputHandler>,
    ) {
        *self.input_handler.write().await = Some((*handler).clone());
        info!("Input handler reference set for reconnection notifications");
    }

    /// Wire the health reporter so PipeWire stream state events propagate
    /// to the session health monitor.
    pub async fn set_health_reporter(&self, reporter: crate::health::HealthReporter) {
        *self.health_reporter.write().await = Some(reporter);
    }

    /// Set clipboard manager reference for disconnect cleanup
    ///
    /// When client disconnects (detected via reconnection), the display handler
    /// will clear Portal clipboard to prevent stale operations.
    pub async fn set_clipboard_manager(
        &self,
        manager: Arc<tokio::sync::Mutex<crate::clipboard::ClipboardOrchestrator>>,
    ) {
        *self.clipboard_manager.write().await = Some(manager);
        info!("Clipboard manager reference set for disconnect cleanup");
    }

    /// Signal that the client has disconnected.
    ///
    /// The pipeline loop checks `client_active` and skips encoding/sending when
    /// no client is connected. PipeWire frames are still drained to keep the
    /// stream healthy, but no CPU is wasted on encoding or queue pressure.
    pub fn on_client_disconnect(&self) {
        self.client_active
            .store(false, std::sync::atomic::Ordering::SeqCst);
        info!("Client disconnect signaled to pipeline - frame processing paused");
    }

    /// Set graphics queue sender for priority multiplexing
    ///
    /// When set, frames will be routed through the graphics queue instead of
    /// directly to IronRDP's DisplayUpdate channel.
    pub fn set_graphics_queue(&mut self, sender: mpsc::Sender<GraphicsFrame>) {
        info!("Graphics queue sender configured for priority multiplexing");
        self.graphics_tx = Some(sender);
    }

    /// Set the server event sender for EGFX message routing
    ///
    /// This must be called after the RDP server is built, passing a clone of
    /// `event_sender()` from the server. Required for EGFX frame sending.
    pub async fn set_server_event_sender(&self, sender: mpsc::UnboundedSender<ServerEvent>) {
        *self.server_event_tx.write().await = Some(sender);
        info!("Server event sender configured for EGFX routing");
    }

    /// Reset the display update channel for a new client connection
    ///
    /// Called when a client disconnects to allow the next client to claim
    /// display updates. Creates a fresh sender/receiver pair.
    pub async fn reset_update_channel(&mut self) {
        let (new_sender, new_receiver) = mpsc::channel(64);
        self.update_sender = new_sender;
        *self.update_receiver.lock().await = Some(new_receiver);
        debug!("Display update channel reset for new client");
    }

    /// Pad frame to aligned dimensions (16-pixel boundary)
    ///
    /// MS-RDPEGFX requires surface dimensions to be multiples of 16.
    /// This function pads the frame by replicating edge pixels.
    fn pad_frame_to_aligned(
        data: &[u8],
        width: u32,
        height: u32,
        aligned_width: u32,
        aligned_height: u32,
    ) -> Vec<u8> {
        let bytes_per_pixel = 4; // BGRA
        let src_stride = width * bytes_per_pixel;
        let dst_stride = aligned_width * bytes_per_pixel;
        let mut padded = vec![0u8; (aligned_width * aligned_height * bytes_per_pixel) as usize];

        for y in 0..height {
            let src_offset = (y * src_stride) as usize;
            let dst_offset = (y * dst_stride) as usize;
            padded[dst_offset..dst_offset + src_stride as usize]
                .copy_from_slice(&data[src_offset..src_offset + src_stride as usize]);

            if aligned_width > width {
                let last_pixel_src = src_offset + (src_stride - bytes_per_pixel) as usize;
                for x in width..aligned_width {
                    let dst_offset = (y * dst_stride + x * bytes_per_pixel) as usize;
                    padded[dst_offset..dst_offset + bytes_per_pixel as usize].copy_from_slice(
                        &data[last_pixel_src..last_pixel_src + bytes_per_pixel as usize],
                    );
                }
            }
        }

        if aligned_height > height {
            let last_row_offset = ((height - 1) * dst_stride) as usize;
            // Create a copy of the last row to avoid borrow checker issues
            let last_row = padded[last_row_offset..last_row_offset + dst_stride as usize].to_vec();
            for y in height..aligned_height {
                let dst_offset = (y * dst_stride) as usize;
                padded[dst_offset..dst_offset + dst_stride as usize].copy_from_slice(&last_row);
            }
        }

        padded
    }

    /// Check if EGFX is ready for frame sending
    ///
    /// Returns true if:
    /// - GFX server handle is available
    /// - Handler state indicates readiness
    /// - AVC420 codec is negotiated
    /// - Server event sender is configured
    pub async fn is_egfx_ready(&self) -> bool {
        if self.server_event_tx.read().await.is_none() {
            return false;
        }

        if self.gfx_server_handle.read().await.is_none() {
            return false;
        }

        if let Some(state) = self.gfx_handler_state.read().await.as_ref() {
            state.is_ready && state.is_avc420_enabled
        } else {
            false
        }
    }

    /// Get a descriptive reason for why EGFX is not ready
    ///
    /// Returns a human-readable string explaining the current wait state.
    /// Useful for debugging connection/negotiation issues.
    pub async fn egfx_wait_reason(&self) -> &'static str {
        if self.server_event_tx.read().await.is_none() {
            return "waiting for client connection";
        }

        if self.gfx_server_handle.read().await.is_none() {
            return "client connected, waiting for EGFX channel";
        }

        if let Some(state) = self.gfx_handler_state.read().await.as_ref() {
            if !state.is_ready {
                return "EGFX channel open, negotiating capabilities";
            }
            if !state.is_avc420_enabled {
                return "EGFX ready, waiting for AVC420 codec confirmation";
            }
        } else {
            return "EGFX channel open, initializing handler state";
        }

        "ready" // Should not reach here if is_egfx_ready() is false
    }

    /// Update the desktop size
    ///
    /// Called when monitor configuration changes or client requests resize.
    pub async fn update_size(&self, width: u16, height: u16) {
        let mut size = self.size.write().await;
        size.width = width;
        size.height = height;
        debug!("Updated display size to {}x{}", width, height);

        let update = DisplayUpdate::Resize(DesktopSize { width, height });
        if let Err(e) = self.update_sender.send(update).await {
            warn!("Failed to send resize update: {}", e);
        }
    }

    /// Get a clone of the update sender for graphics drain task
    ///
    /// This is used by the Phase 1 multiplexer to get access to the IronRDP update channel.
    pub fn get_update_sender(&self) -> mpsc::Sender<DisplayUpdate> {
        self.update_sender.clone()
    }

    /// Shutdown PipeWire thread explicitly
    ///
    /// Must be called during server shutdown to ensure PipeWire thread exits.
    /// The PipeWireThreadManager lives in Arc<Mutex<>> which may have multiple
    /// references (e.g., from spawned pipeline task), so Drop may not trigger
    /// until after runtime shutdown.
    ///
    /// Calling this method sends shutdown signals directly to the PipeWire thread,
    /// ensuring immediate cleanup regardless of reference count.
    pub async fn shutdown_pipewire(&self) {
        info!("Shutting down PipeWire thread...");
        let mut thread_mgr = self.pipewire_thread.lock().await;
        if let Err(e) = thread_mgr.shutdown() {
            warn!("PipeWire shutdown error: {}", e);
        } else {
            info!("✅ PipeWire thread shut down successfully");
        }
    }

    /// Start the video pipeline
    ///
    /// This spawns a background task that continuously captures frames from PipeWire,
    /// processes them, and sends them via either EGFX (H.264) or RemoteFX path.
    ///
    /// # Path Selection
    ///
    /// - **EGFX/H.264**: When client negotiates AVC420 support, frames are encoded
    ///   with OpenH264 and sent through the EGFX channel for better quality.
    /// - **RemoteFX**: Fallback path when H.264 is not available, converts to
    ///   bitmap and sends through standard display update channel.
    #[expect(clippy::expect_used, reason = "mutex poisoning is unrecoverable")]
    pub fn start_pipeline(self: Arc<Self>) {
        let handler = Arc::clone(&self);

        tokio::spawn(async move {
            info!("🎬 Starting display update pipeline task");

            // === ADAPTIVE FPS CONTROLLER (Premium Feature) ===
            // Dynamically adjusts frame rate based on screen activity:
            // - Static screen: 5 FPS (saves CPU/bandwidth)
            // - Low activity (typing): 15 FPS
            // - Medium activity (scrolling): 20 FPS
            // - High activity (video): 30 FPS
            //
            // SERVICE-AWARE: Only enable when damage tracking service is available
            // (without it, adaptive FPS has no activity detection signal)
            let service_supports_adaptive_fps = self.service_registry.should_enable_adaptive_fps();
            let adaptive_fps_enabled =
                self.config.performance.adaptive_fps.enabled && service_supports_adaptive_fps;
            if self.config.performance.adaptive_fps.enabled && !service_supports_adaptive_fps {
                info!("⚠️ Adaptive FPS disabled: damage tracking service unavailable");
            }
            let adaptive_fps_config = crate::performance::AdaptiveFpsConfig {
                enabled: adaptive_fps_enabled,
                min_fps: self.config.performance.adaptive_fps.min_fps,
                max_fps: self.config.performance.adaptive_fps.max_fps,
                high_activity_threshold: self
                    .config
                    .performance
                    .adaptive_fps
                    .high_activity_threshold,
                medium_activity_threshold: self
                    .config
                    .performance
                    .adaptive_fps
                    .medium_activity_threshold,
                low_activity_threshold: self.config.performance.adaptive_fps.low_activity_threshold,
                ..Default::default()
            };
            let mut adaptive_fps = AdaptiveFpsController::new(adaptive_fps_config);

            // === LATENCY GOVERNOR (Premium Feature) ===
            // Controls encoding latency vs quality trade-off:
            // - Interactive (<50ms): Gaming, CAD - encode immediately
            // - Balanced (<100ms): General desktop - smart batching
            // - Quality (<300ms): Photo/video editing - accumulate for quality
            //
            // SERVICE-AWARE: ExplicitSync service affects frame pacing accuracy
            let explicit_sync_level = self.service_registry.service_level(ServiceId::ExplicitSync);
            let latency_mode = match self.config.performance.latency.mode.as_str() {
                "interactive" => LatencyMode::Interactive,
                "quality" => LatencyMode::Quality,
                _ => LatencyMode::Balanced,
            };
            let mut latency_governor = LatencyGovernor::new(latency_mode);

            // Log service-aware performance feature status
            let damage_level = self
                .service_registry
                .service_level(ServiceId::DamageTracking);
            let dmabuf_level = self
                .service_registry
                .service_level(ServiceId::DmaBufZeroCopy);
            info!(
                "🎛️ Performance features: adaptive_fps={}, latency_mode={:?}",
                adaptive_fps_enabled, latency_mode
            );
            info!(
                "   Services: damage_tracking={}, explicit_sync={}, dmabuf={}",
                damage_level, explicit_sync_level, dmabuf_level
            );

            // Legacy frame regulator (fallback when adaptive FPS disabled)
            // Uses configured max_fps (default: 30, can be 60 for high-performance mode)
            let legacy_fps = self.config.performance.adaptive_fps.max_fps;
            let mut frame_regulator = FrameRateRegulator::new(legacy_fps);
            let mut frames_sent = 0u64;
            let mut frames_dropped = 0u64;
            let mut egfx_frames_sent = 0u64;

            let mut loop_iterations = 0u64;

            // EGFX/H.264 encoder - created lazily when EGFX becomes ready
            // Supports both AVC420 (4:2:0) and AVC444 (4:4:4) based on client negotiation
            // NOTE: These are reset when egfx_needs_init transitions from true to false
            let mut video_encoder: Option<VideoEncoder> = None;
            let mut egfx_sender: Option<EgfxFrameSender> = None;
            // AVC444 vs AVC420 determined by VideoEncoder enum variant match, not a flag

            // Force first frame after initialization - bypasses damage detection
            // Without this, reconnecting clients see black screen until mouse moves
            // because damage detection reports 0% change on first frame (no previous data)
            let mut force_first_frame = false;

            // === DAMAGE DETECTION (Config-controlled) ===
            // Detects changed screen regions to skip unchanged frames (90%+ bandwidth reduction for static content)
            // All parameters now configurable via config.toml [damage_tracking] section
            // See DamageTrackingConfig documentation for sensitivity tuning guidance
            let damage_config = DamageConfig {
                tile_size: self.config.damage_tracking.tile_size,
                diff_threshold: self.config.damage_tracking.diff_threshold,
                pixel_threshold: self.config.damage_tracking.pixel_threshold,
                merge_distance: self.config.damage_tracking.merge_distance,
                min_region_area: self.config.damage_tracking.min_region_area,
            };

            let mut damage_detector_opt = if self.config.damage_tracking.enabled {
                debug!("Damage tracking ENABLED: tile_size={}, threshold={:.2}, pixel_threshold={}, merge_distance={}, min_region_area={}",
                    damage_config.tile_size, damage_config.diff_threshold, damage_config.pixel_threshold,
                    damage_config.merge_distance, damage_config.min_region_area);
                Some(DamageDetector::new(damage_config))
            } else {
                debug!("🎯 Damage tracking DISABLED via config");
                None
            };

            let mut frames_skipped_damage = 0u64; // Frames skipped due to no damage

            // Take the resize receiver for this pipeline instance
            let resize_rx = handler
                .resize_rx
                .lock()
                .ok()
                .and_then(|mut guard| guard.take());

            if resize_rx.is_some() {
                info!("Pipeline acquired resize receiver for client-initiated resolution changes");
            }

            loop {
                loop_iterations += 1;
                if loop_iterations.is_multiple_of(1000) {
                    debug!(
                        "Display pipeline heartbeat: {} iterations, sent {} (egfx: {}), dropped {}, skipped_damage {}",
                        loop_iterations, frames_sent, egfx_frames_sent, frames_dropped, frames_skipped_damage
                    );
                }

                // === CLIENT-INITIATED RESIZE ===
                // Check for pending resize requests. Coalesce: drain all pending and use the last.
                if let Some(ref rx) = resize_rx {
                    let mut latest_resize: Option<ResizeRequest> = None;
                    while let Ok(req) = rx.try_recv() {
                        latest_resize = Some(req);
                    }

                    if let Some(req) = latest_resize {
                        info!("Processing client resize: {}x{}", req.width, req.height);

                        // 1. Destroy existing PipeWire stream
                        if let Some(stream) = handler.stream_info.first() {
                            let node_id = stream.node_id;
                            let (resp_tx, resp_rx) = std::sync::mpsc::sync_channel(1);
                            let destroy_cmd = PipeWireThreadCommand::DestroyStream {
                                stream_id: node_id,
                                response_tx: resp_tx,
                            };

                            let destroy_ok = {
                                let mgr = handler.pipewire_thread.lock().await;
                                if let Err(e) = mgr.send_command(destroy_cmd) {
                                    warn!("Failed to send DestroyStream: {}", e);
                                    false
                                } else {
                                    match resp_rx.recv_timeout(std::time::Duration::from_secs(5)) {
                                        Ok(Ok(())) => {
                                            info!(
                                                "PipeWire stream {} destroyed for resize",
                                                node_id
                                            );
                                            true
                                        }
                                        Ok(Err(e)) => {
                                            warn!("DestroyStream failed: {}", e);
                                            false
                                        }
                                        Err(_) => {
                                            warn!("DestroyStream timeout");
                                            false
                                        }
                                    }
                                }
                            };

                            if destroy_ok {
                                // 2. Create new stream at requested resolution
                                let stream_config = lamco_pipewire::StreamConfig {
                                    name: "monitor-0".to_string(),
                                    width: req.width as u32,
                                    height: req.height as u32,
                                    framerate: 60,
                                    use_dmabuf: true,
                                    buffer_count: 3,
                                    preferred_format: Some(lamco_pipewire::PixelFormat::BGRx),
                                };

                                let (resp_tx2, resp_rx2) = std::sync::mpsc::sync_channel(1);
                                let create_cmd = PipeWireThreadCommand::CreateStream {
                                    stream_id: node_id,
                                    node_id,
                                    config: stream_config,
                                    response_tx: resp_tx2,
                                };

                                let create_ok = {
                                    let mgr = handler.pipewire_thread.lock().await;
                                    if let Err(e) = mgr.send_command(create_cmd) {
                                        warn!("Failed to send CreateStream: {}", e);
                                        false
                                    } else {
                                        match resp_rx2
                                            .recv_timeout(std::time::Duration::from_secs(5))
                                        {
                                            Ok(Ok(())) => {
                                                info!(
                                                    "PipeWire stream {} recreated at {}x{}",
                                                    node_id, req.width, req.height
                                                );
                                                true
                                            }
                                            Ok(Err(e)) => {
                                                warn!(
                                                    "CreateStream at new resolution failed: {}",
                                                    e
                                                );
                                                false
                                            }
                                            Err(_) => {
                                                warn!("CreateStream timeout");
                                                false
                                            }
                                        }
                                    }
                                };

                                if create_ok {
                                    // 3. Recreate BitmapConverter for new dimensions
                                    {
                                        let mut converter = handler.bitmap_converter.lock().await;
                                        *converter = BitmapConverter::new(req.width, req.height);
                                        debug!(
                                            "BitmapConverter recreated for {}x{}",
                                            req.width, req.height
                                        );
                                    }

                                    // 4. Trigger EGFX re-initialization (encoder + surface recreation)
                                    handler
                                        .egfx_needs_init
                                        .store(true, std::sync::atomic::Ordering::SeqCst);

                                    // 5. Reset pipeline encoder state
                                    video_encoder = None;
                                    egfx_sender = None;
                                    force_first_frame = false;

                                    // 6. Invalidate damage detector for new resolution
                                    if let Some(ref mut detector) = damage_detector_opt {
                                        detector.invalidate();
                                    }

                                    // 7. Update display size (triggers deactivation-reactivation in IronRDP)
                                    handler.update_size(req.width, req.height).await;

                                    info!(
                                        "Resize complete: {}x{} - awaiting client reactivation",
                                        req.width, req.height
                                    );
                                }
                            }
                        } else {
                            warn!("No stream_info available for resize");
                        }

                        // Skip frame processing this iteration to let reactivation proceed
                        continue;
                    }
                }

                let frame = {
                    let thread_mgr = handler.pipewire_thread.lock().await;

                    // Forward PipeWire stream state changes to health monitor
                    if let Some(ref reporter) = *handler.health_reporter.read().await {
                        for event in thread_mgr.drain_state_events() {
                            let health_state = match event.state {
                                lamco_pipewire::StreamStateSnapshot::Streaming => {
                                    crate::health::VideoStreamState::Streaming
                                }
                                lamco_pipewire::StreamStateSnapshot::Paused => {
                                    crate::health::VideoStreamState::Paused
                                }
                                lamco_pipewire::StreamStateSnapshot::Error(_) => {
                                    crate::health::VideoStreamState::Error
                                }
                                // Connecting/Unconnected are transient — not health events
                                _ => continue,
                            };
                            reporter.report(crate::health::HealthEvent::VideoStreamStateChanged {
                                state: health_state,
                            });
                        }
                    }

                    thread_mgr.try_recv_frame()
                };

                let frame = match frame {
                    Some(f) => {
                        // Drain PipeWire frames even when no client is connected,
                        // but skip all encoding and sending to avoid wasted work
                        if !handler
                            .client_active
                            .load(std::sync::atomic::Ordering::Relaxed)
                        {
                            continue;
                        }
                        debug!("Received frame from PipeWire");
                        f
                    }
                    None => {
                        // No frame available, sleep briefly and retry
                        tokio::time::sleep(tokio::time::Duration::from_millis(5)).await;
                        continue;
                    }
                };

                let should_process = if adaptive_fps_enabled {
                    adaptive_fps.should_capture_frame()
                } else {
                    frame_regulator.should_send_frame()
                };

                if !should_process {
                    frames_dropped += 1;
                    if frames_dropped.is_multiple_of(30) {
                        let current_fps = if adaptive_fps_enabled {
                            adaptive_fps.current_fps()
                        } else {
                            30
                        };
                        info!(
                            "Frame rate regulation: dropped {} frames, sent {}, target_fps={}",
                            frames_dropped, frames_sent, current_fps
                        );
                    }
                    continue;
                }

                frames_sent += 1;
                if frames_sent.is_multiple_of(30) || frames_sent < 10 {
                    let activity = if adaptive_fps_enabled {
                        format!(
                            " [activity={:?}, fps={}]",
                            adaptive_fps.activity_level(),
                            adaptive_fps.current_fps()
                        )
                    } else {
                        String::new()
                    };
                    info!(
                        "🎬 Processing frame {} ({}x{}) - sent: {} (egfx: {}), dropped: {}{}",
                        frame.frame_id,
                        frame.width,
                        frame.height,
                        frames_sent,
                        egfx_frames_sent,
                        frames_dropped,
                        activity
                    );
                }

                // === WAIT FOR EGFX ===
                // CRITICAL: Suppress ALL output until EGFX is ready
                // Sending RemoteFX before EGFX establishes wrong framebuffer
                // When EGFX activates with ResetGraphics, client may clear display
                // Result: EGFX frames render to invisible surface
                if !handler.is_egfx_ready().await {
                    // EGFX not ready yet - drop this frame and wait
                    frames_dropped += 1;
                    if frames_dropped.is_multiple_of(30) {
                        let reason = handler.egfx_wait_reason().await;
                        debug!("⏳ {} (dropped {} frames)", reason, frames_dropped);
                    }
                    continue;
                }

                // === EGFX/H.264 PATH ===
                // EGFX is ready - process frame
                if true {
                    // Initialize encoder and sender on first EGFX-ready frame OR when reconnecting
                    // The egfx_needs_init flag is set to true on reconnection (in updates())
                    let needs_init = handler
                        .egfx_needs_init
                        .load(std::sync::atomic::Ordering::SeqCst);

                    if needs_init {
                        // Mark as initialized BEFORE doing work (prevents re-entry)
                        handler
                            .egfx_needs_init
                            .store(false, std::sync::atomic::Ordering::SeqCst);

                        // Reset encoder and sender for fresh client
                        // (Previous client's state is stale)
                        video_encoder = None;
                        egfx_sender = None;

                        // Invalidate damage detector to clear previous frame buffer
                        // This ensures first frame comparison returns 100% damage
                        if let Some(ref mut detector) = damage_detector_opt {
                            detector.invalidate();
                            info!("🔄 Damage detector invalidated for reconnection");
                        }

                        info!(
                            "🎬 EGFX channel ready - initializing H.264 encoder (needs_init=true)"
                        );

                        // Calculate aligned dimensions first (needed for encoder and surface)
                        use crate::egfx::align_to_16;
                        let aligned_width = align_to_16(frame.width as u32) as u16;
                        let aligned_height = align_to_16(frame.height as u32) as u16;

                        // Create H.264 encoder with resolution-appropriate level
                        // Use config values for quality settings and color space
                        let color_space = ColorSpaceConfig::from_config(
                            &self.config.egfx.color_matrix,
                            &self.config.egfx.color_range,
                            aligned_width as u32,
                            aligned_height as u32,
                        );
                        let config = EncoderConfig {
                            bitrate_kbps: self.config.egfx.h264_bitrate,
                            max_fps: self.config.video.target_fps as f32,
                            enable_skip_frame: true,
                            width: Some(aligned_width),
                            height: Some(aligned_height),
                            color_space: Some(color_space),
                            qp_min: self.config.egfx.qp_min,
                            qp_max: self.config.egfx.qp_max,
                            encoder_threads: self.config.performance.encoder_threads as u16,
                        };
                        let threads_desc = if self.config.performance.encoder_threads == 0 {
                            "auto".to_string()
                        } else {
                            self.config.performance.encoder_threads.to_string()
                        };
                        info!(
                            "🎬 H.264 encoder config: {}kbps, {}fps, QP[{}-{}], threads={}, color={}",
                            self.config.egfx.h264_bitrate,
                            self.config.video.target_fps,
                            self.config.egfx.qp_min,
                            self.config.egfx.qp_max,
                            threads_desc,
                            color_space.description()
                        );

                        // Determine codec based on config preference and client capabilities
                        // Config codec setting: "auto", "avc420", "avc444"
                        let client_supports_avc444 =
                            if let Some(state) = handler.gfx_handler_state.read().await.as_ref() {
                                state.is_avc444_enabled
                            } else {
                                false
                            };

                        // Resolve codec preference from config
                        let codec_pref = self.config.egfx.codec.to_lowercase();
                        let avc444_enabled = match codec_pref.as_str() {
                            "avc420" => {
                                info!("Codec preference: AVC420 forced by config");
                                false
                            }
                            "avc444" => {
                                if client_supports_avc444 && self.config.egfx.avc444_enabled {
                                    info!("Codec preference: AVC444 requested and supported");
                                    true
                                } else if !client_supports_avc444 {
                                    info!("Codec preference: AVC444 requested but client doesn't support it, using AVC420");
                                    false
                                } else {
                                    info!("Codec preference: AVC444 requested but disabled in config, using AVC420");
                                    false
                                }
                            }
                            _ => {
                                // "auto" or unrecognized: use best available
                                if self.config.egfx.avc444_enabled && client_supports_avc444 {
                                    info!("Codec preference: auto → AVC444 (client supports, enabled in config)");
                                    true
                                } else if !self.config.egfx.avc444_enabled {
                                    info!("Codec preference: auto → AVC420 (AVC444 disabled in config)");
                                    false
                                } else {
                                    info!("Codec preference: auto → AVC420 (client doesn't support AVC444)");
                                    false
                                }
                            }
                        };

                        if avc444_enabled {
                            // Try AVC444 first (premium 4:4:4 chroma)
                            match Avc444Encoder::new(config.clone()) {
                                Ok(mut encoder) => {
                                    // Wire aux omission config from EgfxConfig
                                    encoder.configure_aux_omission(
                                        self.config.egfx.avc444_enable_aux_omission,
                                        self.config.egfx.avc444_max_aux_interval,
                                        self.config.egfx.avc444_aux_change_threshold,
                                        self.config.egfx.avc444_force_aux_idr_on_return,
                                    );
                                    // Wire periodic IDR config for artifact recovery
                                    encoder.configure_periodic_idr(
                                        self.config.egfx.periodic_idr_interval,
                                    );

                                    video_encoder = Some(VideoEncoder::Avc444(encoder));
                                    info!(
                                        "✅ AVC444 encoder initialized for {}×{} (4:4:4 chroma)",
                                        aligned_width, aligned_height
                                    );
                                }
                                Err(e) => {
                                    warn!("Failed to create AVC444 encoder: {:?} - falling back to AVC420", e);
                                    // Fall through to AVC420
                                    match Avc420Encoder::new(config) {
                                        Ok(encoder) => {
                                            video_encoder = Some(VideoEncoder::Avc420(encoder));
                                            info!("✅ AVC420 encoder initialized for {}×{} (4:2:0 fallback)", aligned_width, aligned_height);
                                        }
                                        Err(e) => {
                                            warn!("Failed to create AVC420 encoder: {:?} - falling back to RemoteFX", e);
                                        }
                                    }
                                }
                            }
                        } else {
                            // Use AVC420 (standard 4:2:0 chroma)
                            match Avc420Encoder::new(config) {
                                Ok(encoder) => {
                                    video_encoder = Some(VideoEncoder::Avc420(encoder));
                                    info!(
                                        "✅ AVC420 encoder initialized for {}×{} (aligned)",
                                        aligned_width, aligned_height
                                    );
                                }
                                Err(e) => {
                                    warn!("Failed to create H.264 encoder: {:?} - falling back to RemoteFX", e);
                                }
                            }
                        }

                        // Create EGFX sender and surface
                        if let (Some(gfx_handle), Some(event_tx)) = (
                            handler.gfx_server_handle.read().await.clone(),
                            handler.server_event_tx.read().await.clone(),
                        ) {
                            // Create primary surface for EGFX rendering
                            // Must be done BEFORE sending any frames
                            // MS-RDPEGFX REQUIRES 16-pixel alignment!
                            {
                                info!(
                                    "📐 Aligning surface: {}×{} → {}×{} (16-pixel boundary)",
                                    frame.width, frame.height, aligned_width, aligned_height
                                );

                                let mut server =
                                    gfx_handle.lock().expect("GfxServerHandle mutex poisoned");

                                // CRITICAL FIX: Set desktop size BEFORE creating surface
                                // This prevents desktop size mismatch when ResetGraphics is auto-sent
                                // Desktop = actual resolution (800×600)
                                // Surface = aligned resolution (800×608)
                                server
                                    .set_output_dimensions(frame.width as u16, frame.height as u16);
                                info!(
                                    "✅ EGFX desktop dimensions set: {}×{} (actual)",
                                    frame.width, frame.height
                                );

                                // Create surface with ALIGNED dimensions
                                // create_surface() will auto-send ResetGraphics using output_dimensions
                                if let Some(surface_id) =
                                    server.create_surface(aligned_width, aligned_height)
                                {
                                    info!(
                                        "✅ EGFX surface {} created ({}×{} aligned)",
                                        surface_id, aligned_width, aligned_height
                                    );
                                    // Map surface to output at origin (0,0)
                                    if server.map_surface_to_output(surface_id, 0, 0) {
                                        info!("✅ EGFX surface {} mapped to output", surface_id);
                                    } else {
                                        warn!("Failed to map EGFX surface to output");
                                    }

                                    // Send the CreateSurface and MapSurfaceToOutput PDUs to client
                                    let channel_id = server.channel_id();
                                    let dvc_messages = server.drain_output();
                                    if !dvc_messages.is_empty() {
                                        info!("EGFX: drain_output returned {} DVC messages for surface setup", dvc_messages.len());
                                        // Log the size of each DVC message (GfxPdu)
                                        for (i, msg) in dvc_messages.iter().enumerate() {
                                            info!("  DVC msg {}: {} bytes", i, msg.size());
                                        }

                                        if let Some(ch_id) = channel_id {
                                            use ironrdp_dvc::encode_dvc_messages;
                                            use ironrdp_server::EgfxServerMessage;
                                            use ironrdp_svc::ChannelFlags;

                                            match encode_dvc_messages(
                                                ch_id,
                                                dvc_messages,
                                                ChannelFlags::SHOW_PROTOCOL,
                                            ) {
                                                Ok(svc_messages) => {
                                                    info!("EGFX: Encoded {} SVC messages for DVC channel {}", svc_messages.len(), ch_id);
                                                    let msg = EgfxServerMessage::SendMessages {
                                                        messages: svc_messages,
                                                    };
                                                    let _ = event_tx.send(ServerEvent::Egfx(msg));
                                                    info!("✅ EGFX surface PDUs sent to client");
                                                }
                                                Err(e) => {
                                                    error!(
                                                        "EGFX: Failed to encode DVC messages: {:?}",
                                                        e
                                                    );
                                                }
                                            }
                                        }
                                    }
                                } else {
                                    warn!(
                                        "Failed to create EGFX surface - server may not be ready"
                                    );
                                }
                            }

                            let sender = EgfxFrameSender::new(
                                gfx_handle,
                                handler.gfx_handler_state.clone(),
                                event_tx,
                            );
                            egfx_sender = Some(sender);
                            info!("✅ EGFX frame sender initialized");

                            // Force first frame to be sent regardless of damage detection
                            // This ensures reconnecting clients see the screen immediately
                            force_first_frame = true;
                            info!("📺 First frame after init will be forced (bypass damage detection)");
                        }
                    }

                    // Try to send via EGFX if encoder is available
                    if let (Some(ref mut encoder), Some(ref sender)) =
                        (&mut video_encoder, &egfx_sender)
                    {
                        use crate::egfx::align_to_16;

                        // Compute presentation timestamp from configured target FPS
                        let frame_interval_ms =
                            1000 / u64::from(self.config.video.target_fps.max(1));
                        let timestamp_ms = frames_sent * frame_interval_ms;

                        // PipeWire sometimes sends zero-size buffers
                        let expected_size = (frame.width * frame.height * 4) as usize;
                        if frame.data.len() < expected_size {
                            trace!(
                                "Skipping invalid frame: size={}, expected={} for {}×{}",
                                frame.data.len(),
                                expected_size,
                                frame.width,
                                frame.height
                            );
                            frames_dropped += 1;
                            continue;
                        }

                        // === DAMAGE DETECTION (Config-controlled) ===
                        // Detect which regions changed since the last frame
                        // Skip encoding entirely if nothing changed (huge bandwidth savings)
                        //
                        // CRITICAL: Bypass damage detection when:
                        // 1. Periodic IDR is due (clear ghost artifacts)
                        // 2. First frame after initialization (reconnecting clients need immediate display)
                        let periodic_idr_due = encoder.is_periodic_idr_due();
                        let force_full_frame = periodic_idr_due || force_first_frame;

                        if force_first_frame {
                            info!("📺 Forcing first frame after init (IDR will be sent)");
                            force_first_frame = false;
                        }

                        let damage_regions = if force_full_frame {
                            // Force full frame - either periodic IDR or first frame after init
                            if periodic_idr_due {
                                debug!(
                                    "Forcing full frame for periodic IDR (bypassing damage detection)"
                                );
                            }
                            vec![DamageRegion::full_frame(frame.width, frame.height)]
                        } else if let Some(ref mut detector) = damage_detector_opt {
                            // Damage tracking enabled - detect changed regions
                            detector.detect(&frame.data, frame.width, frame.height)
                        } else {
                            // Damage tracking disabled - use full frame
                            vec![DamageRegion::full_frame(frame.width, frame.height)]
                        };

                        let damage_ratio = if !damage_regions.is_empty() {
                            let frame_area = (frame.width * frame.height) as u64;
                            let damage_area: u64 = damage_regions
                                .iter()
                                .map(super::super::damage::DamageRegion::area)
                                .sum();
                            damage_area as f32 / frame_area as f32
                        } else {
                            0.0
                        };

                        if adaptive_fps_enabled {
                            adaptive_fps.update(damage_ratio);
                        }

                        let encoding_decision = latency_governor.should_encode_frame(damage_ratio);
                        match encoding_decision {
                            EncodingDecision::Skip => {
                                frames_dropped += 1;
                                continue;
                            }
                            EncodingDecision::WaitForMore => {
                                continue;
                            }
                            EncodingDecision::EncodeNow
                            | EncodingDecision::EncodeKeepalive
                            | EncodingDecision::EncodeBatch
                            | EncodingDecision::EncodeTimeout => {}
                        }

                        if damage_regions.is_empty() {
                            frames_skipped_damage += 1;
                            if frames_skipped_damage.is_multiple_of(100) {
                                if let Some(ref detector) = damage_detector_opt {
                                    let stats = detector.stats();
                                    debug!(
                                        "🎯 Damage tracking: {} frames skipped (no change), {:.1}% bandwidth saved",
                                        frames_skipped_damage,
                                        stats.bandwidth_reduction_percent()
                                    );
                                }
                            }
                            if adaptive_fps_enabled {
                                adaptive_fps.update(0.0);
                            }
                            continue;
                        }

                        if frames_sent.is_multiple_of(60) {
                            if let Some(ref detector) = damage_detector_opt {
                                let stats = detector.stats();
                                debug!(
                                    "🎯 Damage: {} regions, {:.1}% of frame, avg {:.1}ms detection",
                                    damage_regions.len(),
                                    damage_ratio * 100.0,
                                    stats.avg_detection_time_ms
                                );
                            }
                            if adaptive_fps_enabled {
                                debug!(
                                    "🎛️ Adaptive FPS: activity={:?}, fps={}, latency_mode={:?}",
                                    adaptive_fps.activity_level(),
                                    adaptive_fps.current_fps(),
                                    latency_governor.mode()
                                );
                            }
                        }

                        // MS-RDPEGFX REQUIRES 16-pixel alignment
                        // Frame from PipeWire may not be aligned (e.g., 800×600)
                        // Must align dimensions AND pad frame data
                        let aligned_width = align_to_16(frame.width as u32);
                        let aligned_height = align_to_16(frame.height as u32);

                        let frame_data = if aligned_width != frame.width as u32
                            || aligned_height != frame.height as u32
                        {
                            Self::pad_frame_to_aligned(
                                &frame.data,
                                frame.width,
                                frame.height,
                                aligned_width,
                                aligned_height,
                            )
                        } else {
                            (*frame.data).clone()
                        };

                        // OpenH264's encode() is synchronous and CPU-bound.
                        // On slow hardware (e.g., QEMU VMs) it can block for seconds.
                        // block_in_place tells tokio this thread is occupied so the
                        // runtime can schedule other tasks on remaining threads.
                        let encode_result = tokio::task::block_in_place(|| {
                            encoder.encode_bgra(
                                &frame_data,
                                aligned_width,
                                aligned_height,
                                timestamp_ms,
                            )
                        });
                        match encode_result {
                            Ok(Some(encoded_frame)) => {
                                let send_result = match encoded_frame {
                                    EncodedVideoFrame::Single(data) => {
                                        sender
                                            .send_frame_with_regions(
                                                &data,
                                                aligned_width as u16,
                                                aligned_height as u16,
                                                frame.width as u16,
                                                frame.height as u16,
                                                &damage_regions,
                                                timestamp_ms as u32,
                                            )
                                            .await
                                    }
                                    EncodedVideoFrame::Dual { main, aux } => {
                                        sender
                                            .send_avc444_frame_with_regions(
                                                &main,
                                                aux.as_deref(), // Option<Vec<u8>> → Option<&[u8]>
                                                aligned_width as u16,
                                                aligned_height as u16,
                                                frame.width as u16,
                                                frame.height as u16,
                                                &damage_regions,
                                                timestamp_ms as u32,
                                            )
                                            .await
                                    }
                                };

                                match send_result {
                                    Ok(_frame_id) => {
                                        egfx_frames_sent += 1;
                                        if egfx_frames_sent.is_multiple_of(30) {
                                            let codec = encoder.codec_name();
                                            debug!(
                                                "📹 EGFX: Sent {} {} frames",
                                                egfx_frames_sent, codec
                                            );
                                        }
                                        continue; // Frame sent via EGFX, skip RemoteFX path
                                    }
                                    Err(e) => {
                                        // CRITICAL: Once EGFX is active, NEVER fall back to RemoteFX!
                                        // Mixing codecs causes display conflicts - EGFX surface invisible
                                        trace!("EGFX send failed: {} - dropping frame (no RemoteFX fallback)", e);
                                        frames_dropped += 1;
                                        continue; // Drop frame, don't fall through to RemoteFX
                                    }
                                }
                            }
                            Ok(None) => {
                                trace!("H.264 encoder skipped frame");
                                frames_dropped += 1;
                                continue;
                            }
                            Err(e) => {
                                // CRITICAL: Once EGFX is active, don't fall back to RemoteFX
                                trace!("H.264 encoding failed: {:?} - dropping frame (no RemoteFX fallback)", e);
                                frames_dropped += 1;
                                continue; // Drop frame, don't fall through to RemoteFX
                            }
                        }
                    }
                }

                let convert_start = std::time::Instant::now();
                let bitmap_update = match handler.convert_to_bitmap(frame).await {
                    Ok(bitmap) => bitmap,
                    Err(e) => {
                        error!("Failed to convert frame to bitmap: {}", e);
                        continue;
                    }
                };
                let convert_elapsed = convert_start.elapsed();

                // EARLY EXIT: Skip empty frames BEFORE expensive IronRDP conversion
                // BitmapConverter returns empty rectangles when frame unchanged (dirty region optimization)
                // This saves ~1-2ms per unchanged frame (40% of frames!)
                if bitmap_update.rectangles.is_empty() {
                    // Log periodically to verify optimization is working
                    static EMPTY_COUNT: std::sync::atomic::AtomicU64 =
                        std::sync::atomic::AtomicU64::new(0);
                    let count = EMPTY_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    if count.is_multiple_of(100) && count > 0 {
                        debug!(
                            "Empty frame optimization: {} unchanged frames skipped",
                            count
                        );
                    }
                    continue;
                }

                let iron_start = std::time::Instant::now();
                let iron_updates = match handler.convert_to_iron_format(&bitmap_update).await {
                    Ok(updates) => updates,
                    Err(e) => {
                        error!("Failed to convert to IronRDP format: {}", e);
                        continue;
                    }
                };
                let iron_elapsed = iron_start.elapsed();

                if frames_sent.is_multiple_of(30) {
                    info!(
                        "🎨 Frame conversion timing: bitmap={:?}, iron={:?}, total={:?}",
                        convert_elapsed,
                        iron_elapsed,
                        convert_start.elapsed()
                    );
                }

                if let Some(ref graphics_tx) = handler.graphics_tx {
                    for iron_bitmap in iron_updates {
                        let graphics_frame = GraphicsFrame {
                            iron_bitmap,
                            sequence: frames_sent,
                        };

                        trace!(
                            "📤 Graphics multiplexer: sending frame {} to queue",
                            frames_sent
                        );
                        if let Err(_e) = graphics_tx.try_send(graphics_frame) {
                            warn!("Graphics queue full - frame dropped (QoS policy)");
                        }
                    }
                } else {
                    for iron_bitmap in iron_updates {
                        let update = DisplayUpdate::Bitmap(iron_bitmap);

                        if let Err(e) = handler.update_sender.send(update).await {
                            error!("Failed to send display update: {}", e);
                            return;
                        }
                    }
                }
            }
        });
    }

    /// Convert video frame to RDP bitmap
    async fn convert_to_bitmap(&self, frame: VideoFrame) -> Result<BitmapUpdate> {
        let mut converter = self.bitmap_converter.lock().await;
        converter
            .convert_frame(&frame)
            .map_err(|e| anyhow::anyhow!("Bitmap conversion failed: {e}"))
    }

    /// Convert our BitmapUpdate format to IronRDP's BitmapUpdate format
    async fn convert_to_iron_format(&self, update: &BitmapUpdate) -> Result<Vec<IronBitmapUpdate>> {
        let mut iron_updates = Vec::new();

        for rect_data in &update.rectangles {
            let iron_format = match rect_data.format {
                RdpPixelFormat::BgrX32 => IronPixelFormat::BgrX32,
                RdpPixelFormat::Bgr24 => {
                    // IronRDP doesn't have Bgr24, use XBgr32 instead
                    warn!("Converting Bgr24 to XBgr32 for IronRDP compatibility");
                    IronPixelFormat::XBgr32
                }
                RdpPixelFormat::Rgb16 => {
                    // IronRDP doesn't have Rgb16, use XRgb32 instead
                    warn!("Converting Rgb16 to XRgb32 for IronRDP compatibility");
                    IronPixelFormat::XRgb32
                }
                RdpPixelFormat::Rgb15 => {
                    // IronRDP doesn't have Rgb15, use XRgb32 instead
                    warn!("Converting Rgb15 to XRgb32 for IronRDP compatibility");
                    IronPixelFormat::XRgb32
                }
            };

            let width = rect_data
                .rectangle
                .right
                .saturating_sub(rect_data.rectangle.left);
            let height = rect_data
                .rectangle
                .bottom
                .saturating_sub(rect_data.rectangle.top);

            let bytes_per_pixel = iron_format.bytes_per_pixel() as usize;
            let stride = NonZeroUsize::new(width as usize * bytes_per_pixel)
                .ok_or_else(|| anyhow::anyhow!("Invalid stride calculation: width={width}"))?;

            let iron_bitmap = IronBitmapUpdate {
                x: rect_data.rectangle.left,
                y: rect_data.rectangle.top,
                width: NonZeroU16::new(width)
                    .ok_or_else(|| anyhow::anyhow!("Invalid width: {width}"))?,
                height: NonZeroU16::new(height)
                    .ok_or_else(|| anyhow::anyhow!("Invalid height: {height}"))?,
                format: iron_format,
                data: Bytes::from(rect_data.data.clone()),
                stride,
            };

            iron_updates.push(iron_bitmap);
        }

        Ok(iron_updates)
    }
}

#[async_trait::async_trait]
impl RdpServerDisplay for LamcoDisplayHandler {
    async fn size(&mut self) -> DesktopSize {
        let size = self.size.read().await;
        *size
    }

    /// Called once per connection to establish the update stream.
    /// If a previous connection consumed the receiver, we create a fresh channel
    /// to allow reconnection without requiring server restart.
    #[expect(
        clippy::expect_used,
        reason = "mutex poisoning is unrecoverable; receiver guaranteed after reset"
    )]
    async fn updates(&mut self) -> Result<Box<dyn RdpServerDisplayUpdates>> {
        let mut receiver_option = self.update_receiver.lock().await;

        // If receiver was already taken by a previous connection, create a new channel
        if receiver_option.is_none() {
            debug!("Display updates channel exhausted, creating new channel for reconnection");
            let (new_sender, new_receiver) = mpsc::channel(64);
            self.update_sender = new_sender;
            *receiver_option = Some(new_receiver);

            // CRITICAL: Reset ALL EGFX state for new client
            // The new client needs fresh EGFX negotiation + ResetGraphics + CreateSurface.
            // Without these resets:
            // 1. egfx_needs_init=false would skip encoder/surface creation
            // 2. stale gfx_handler_state.is_ready=true would skip waiting for new EGFX channel
            // 3. stale gfx_server_handle would have old surface (create_surface returns None)
            info!("Resetting EGFX state for reconnecting client");
            self.egfx_needs_init
                .store(true, std::sync::atomic::Ordering::SeqCst);

            // Clear handler state to force waiting for NEW EGFX channel negotiation
            // The new connection's GfxServerFactory.build_server_with_handle() will
            // create fresh state when the client's EGFX DVC channel is established
            if let Ok(mut state) = self.gfx_handler_state.try_write() {
                *state = None;
                info!("Cleared gfx_handler_state - will wait for new EGFX negotiation");
            }

            // NOTE: Do NOT clear server_event_tx - it's valid for the entire server lifetime
            // and is reused across all client connections. Only gfx_handler_state needs reset
            // because it contains per-connection EGFX negotiation state.

            // Notify input handler about reconnection
            // The input handler is shared across connections but needs to reset internal state
            // (keyboard modifiers, mouse button state) when a new client connects
            if let Some(ref handler) = *self.input_handler.read().await {
                handler.notify_reconnection().await;
            }

            // On reconnect, the clipboard provider manages its own state cleanup.
            if self.clipboard_manager.read().await.is_some() {
                info!("Reconnection detected - clipboard provider handles state reset");
            }
        }

        // Signal pipeline that a client is now consuming frames
        self.client_active
            .store(true, std::sync::atomic::Ordering::SeqCst);
        info!("Client active - pipeline frame processing resumed");

        let receiver = receiver_option
            .take()
            .expect("receiver should exist after reset");

        Ok(Box::new(DisplayUpdatesStream::new(receiver)))
    }

    fn request_layout(&mut self, layout: ironrdp_displaycontrol::pdu::DisplayControlMonitorLayout) {
        use ironrdp_displaycontrol::pdu::MonitorLayoutEntry;

        let monitors = layout.monitors();
        debug!(
            "Client requested layout change: {} monitor(s)",
            monitors.len()
        );

        // Extract the primary monitor (or first monitor for single-monitor case)
        let monitor = match monitors.iter().find(|m| m.is_primary()) {
            Some(m) => m,
            None => match monitors.first() {
                Some(m) => m,
                None => {
                    warn!("Empty monitor layout received, ignoring");
                    return;
                }
            },
        };

        let (raw_w, raw_h) = monitor.dimensions();

        // Gate 1: config allow_resize
        if !self.config.display.allow_resize {
            debug!(
                "Dynamic resize disabled in config, ignoring {}x{} request",
                raw_w, raw_h
            );
            return;
        }

        // Gate 2: apply MS-RDPEDISP constraints (even width, 200-8192 clamping)
        let (w, h) = MonitorLayoutEntry::adjust_display_size(raw_w, raw_h);

        // Gate 3: total area constraint (MaxNumMonitors * FactorA * FactorB = 9,216,000)
        let max_area: u64 = 3840 * 2400; // MaxNumMonitors(1) * FactorA * FactorB
        let requested_area = w as u64 * h as u64;
        if requested_area > max_area {
            warn!("Requested area {w}x{h} = {requested_area} exceeds max {max_area} pixels");
            return;
        }

        let new_w = w as u16;
        let new_h = h as u16;

        // Gate 4: allowed_resolutions filter (empty = all allowed)
        if !self.config.display.allowed_resolutions.is_empty() {
            let target = format!("{new_w}x{new_h}");
            if !self.config.display.allowed_resolutions.contains(&target) {
                debug!(
                    "Resolution {}x{} not in allowed list, ignoring",
                    new_w, new_h
                );
                return;
            }
        }

        // Gate 5: skip if same as current size
        if let Ok(current) = self.size.try_read() {
            if current.width == new_w && current.height == new_h {
                debug!("Requested resolution matches current, ignoring");
                return;
            }
        }

        // Gate 6: debounce (300ms minimum between resize operations)
        // Window edge dragging sends bursts of layout PDUs
        if let Ok(mut last_time) = self.last_resize_time.lock() {
            let elapsed = last_time.elapsed();
            if elapsed < std::time::Duration::from_millis(300) {
                debug!(
                    "Resize debounced ({:.0}ms since last), queuing {}x{}",
                    elapsed.as_millis(),
                    new_w,
                    new_h
                );
            }
            *last_time = Instant::now();
        }

        info!(
            "Resize request accepted: {}x{} (raw: {}x{})",
            new_w, new_h, raw_w, raw_h
        );

        // Send to pipeline loop (non-blocking: if channel full, latest request wins)
        // TrySend avoids blocking the IronRDP dispatch thread
        match self.resize_tx.try_send(ResizeRequest {
            width: new_w,
            height: new_h,
        }) {
            Ok(()) => debug!("Resize request queued for pipeline"),
            Err(std::sync::mpsc::TrySendError::Full(_)) => {
                // Channel full: a resize is already pending. The pipeline
                // coalesces and uses the latest, so this request is safe to drop.
                debug!("Resize channel full, pipeline will process pending request");
            }
            Err(std::sync::mpsc::TrySendError::Disconnected(_)) => {
                error!("Resize channel disconnected, pipeline may have stopped");
            }
        }
    }
}

/// Clone implementation for WrdDisplayHandler
///
/// Allows the handler to be cloned for use with IronRDP's builder pattern.
/// All internal state is Arc'd so cloning is cheap and maintains shared state.
impl Clone for LamcoDisplayHandler {
    fn clone(&self) -> Self {
        Self {
            size: Arc::clone(&self.size),
            pipewire_thread: Arc::clone(&self.pipewire_thread),
            bitmap_converter: Arc::clone(&self.bitmap_converter),
            update_sender: self.update_sender.clone(),
            update_receiver: Arc::clone(&self.update_receiver),
            graphics_tx: self.graphics_tx.clone(),
            stream_info: self.stream_info.clone(),
            // EGFX fields
            gfx_server_handle: Arc::clone(&self.gfx_server_handle),
            gfx_handler_state: Arc::clone(&self.gfx_handler_state),
            server_event_tx: Arc::clone(&self.server_event_tx),
            config: Arc::clone(&self.config), // Clone config Arc
            service_registry: Arc::clone(&self.service_registry), // Clone service registry Arc
            egfx_needs_init: Arc::clone(&self.egfx_needs_init), // Share EGFX init state
            input_handler: Arc::clone(&self.input_handler), // Share input handler ref
            clipboard_manager: Arc::clone(&self.clipboard_manager), // Share clipboard manager ref
            resize_tx: self.resize_tx.clone(),
            resize_rx: Arc::clone(&self.resize_rx),
            last_resize_time: std::sync::Mutex::new(
                Instant::now()
                    .checked_sub(std::time::Duration::from_secs(10))
                    .unwrap_or(Instant::now()),
            ),
            client_active: Arc::clone(&self.client_active),
            health_reporter: Arc::clone(&self.health_reporter),
        }
    }
}

struct DisplayUpdatesStream {
    receiver: mpsc::Receiver<DisplayUpdate>,
}

impl DisplayUpdatesStream {
    fn new(receiver: mpsc::Receiver<DisplayUpdate>) -> Self {
        Self { receiver }
    }
}

#[async_trait::async_trait]
impl RdpServerDisplayUpdates for DisplayUpdatesStream {
    /// Cancellation-safe as required by IronRDP.
    async fn next_update(&mut self) -> Result<Option<DisplayUpdate>> {
        match self.receiver.recv().await {
            Some(update) => {
                trace!("Providing display update: {:?}", update);
                Ok(Some(update))
            }
            None => {
                debug!("Display update stream closed");
                Ok(None)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::video::{BitmapData, Rectangle};

    #[tokio::test]
    async fn test_pixel_format_conversion() {
        // Test our format conversion logic
        let formats = vec![
            (RdpPixelFormat::BgrX32, IronPixelFormat::BgrX32),
            // Bgr24 and Rgb16 get converted to 32-bit formats
        ];

        for (our_format, iron_format) in formats {
            // Verify bytes_per_pixel matches
            let our_bpp = match our_format {
                RdpPixelFormat::BgrX32 => 4,
                RdpPixelFormat::Bgr24 => 3,
                RdpPixelFormat::Rgb16 => 2,
                RdpPixelFormat::Rgb15 => 2,
            };
            // IronRDP formats are all 32-bit
            let iron_bpp = iron_format.bytes_per_pixel();
            debug!(
                "Format {:?} -> {:?}: {} bpp -> {} bpp",
                our_format, iron_format, our_bpp, iron_bpp
            );
        }
    }

    #[tokio::test]
    async fn test_bitmap_data_structure() {
        // Verify our understanding of BitmapData structure
        let rect = Rectangle::new(0, 0, 100, 100);
        let data = BitmapData {
            rectangle: rect,
            format: RdpPixelFormat::BgrX32,
            data: vec![0u8; 100 * 100 * 4],
            compressed: false,
        };

        assert_eq!(data.rectangle.left, 0);
        assert_eq!(data.rectangle.top, 0);
        assert_eq!(data.rectangle.right, 100);
        assert_eq!(data.rectangle.bottom, 100);
        assert_eq!(data.data.len(), 100 * 100 * 4);
    }
}
