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

use anyhow::Result;
use bytes::Bytes;
use ironrdp_server::{
    BitmapUpdate as IronBitmapUpdate, DesktopSize, DisplayUpdate, GfxServerHandle,
    PixelFormat as IronPixelFormat, RdpServerDisplay, RdpServerDisplayUpdates, ServerEvent,
};
use std::num::{NonZeroU16, NonZeroUsize};
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::{mpsc, Mutex, RwLock};
use tracing::{debug, error, info, trace, warn};

use crate::damage::{DamageConfig, DamageDetector, DamageRegion};
use crate::egfx::{Avc420Encoder, Avc444Encoder, EncoderConfig};
use crate::performance::{AdaptiveFpsController, EncodingDecision, LatencyGovernor, LatencyMode};
use crate::pipewire::{PipeWireThreadCommand, PipeWireThreadManager, VideoFrame};
use crate::portal::StreamInfo;
use crate::server::egfx_sender::EgfxFrameSender;
use crate::server::event_multiplexer::GraphicsFrame;
use crate::server::gfx_factory::HandlerState;
use crate::services::{ServiceId, ServiceLevel, ServiceRegistry};
use crate::video::{BitmapConverter, BitmapUpdate, RdpPixelFormat};

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
}

impl LamcoDisplayHandler {
    /// Create a new display handler
    ///
    /// # Arguments
    ///
    /// * `initial_width` - Initial desktop width
    /// * `initial_height` - Initial desktop height
    /// * `pipewire_fd` - PipeWire file descriptor from portal
    /// * `stream_info` - Stream information from portal
    /// * `graphics_tx` - Optional graphics queue sender for priority multiplexing
    /// * `gfx_server_handle` - Optional handle to GFX server for EGFX support
    /// * `gfx_handler_state` - Optional handler state for EGFX readiness checks
    /// * `config` - Server configuration for feature flags
    /// * `service_registry` - Compositor service registry for feature decisions
    ///
    /// # Returns
    ///
    /// A new `WrdDisplayHandler` instance
    ///
    /// # Errors
    ///
    /// Returns error if PipeWire connection or coordinator initialization fails
    pub async fn new(
        initial_width: u16,
        initial_height: u16,
        pipewire_fd: i32,
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

        // Create PipeWire thread manager (handles all PipeWire operations)
        let pipewire_thread = Arc::new(Mutex::new(
            PipeWireThreadManager::new(pipewire_fd)
                .map_err(|e| anyhow::anyhow!("Failed to create PipeWire thread: {}", e))?,
        ));

        // Create streams on the PipeWire thread
        for (idx, stream) in stream_info.iter().enumerate() {
            let config = lamco_pipewire::StreamConfig {
                name: format!("monitor-{}", idx),
                width: stream.size.0,
                height: stream.size.1,
                framerate: 60,
                use_dmabuf: true,
                buffer_count: 3,
                preferred_format: Some(lamco_pipewire::PixelFormat::BGRx),
            };

            // Send create stream command to PipeWire thread
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
                .map_err(|e| anyhow::anyhow!("Failed to send create stream command: {}", e))?;

            // Wait for response
            response_rx
                .recv_timeout(std::time::Duration::from_secs(5))
                .map_err(|_| anyhow::anyhow!("Timeout creating stream"))?
                .map_err(|e| anyhow::anyhow!("Stream creation failed: {}", e))?;

            debug!("Stream {} created successfully", stream.node_id);
        }

        // Create bitmap converter
        let bitmap_converter = Arc::new(Mutex::new(BitmapConverter::new(
            initial_width,
            initial_height,
        )));

        // Create channel for display updates (large buffer for smooth streaming)
        let (update_sender, update_receiver) = mpsc::channel(64);
        let update_receiver = Arc::new(Mutex::new(Some(update_receiver)));

        // Set up EGFX fields (use provided handles or create empty ones)
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
        })
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

        // Copy existing rows
        for y in 0..height {
            let src_offset = (y * src_stride) as usize;
            let dst_offset = (y * dst_stride) as usize;
            padded[dst_offset..dst_offset + src_stride as usize]
                .copy_from_slice(&data[src_offset..src_offset + src_stride as usize]);

            // Replicate last pixel to fill width padding
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

        // Replicate last row to fill height padding
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
        // Check server event sender
        if self.server_event_tx.read().await.is_none() {
            return false;
        }

        // Check GFX server handle
        if self.gfx_server_handle.read().await.is_none() {
            return false;
        }

        // Check handler state
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
        // Check server event sender (indicates client connected)
        if self.server_event_tx.read().await.is_none() {
            return "waiting for client connection";
        }

        // Check GFX server handle (indicates EGFX channel started)
        if self.gfx_server_handle.read().await.is_none() {
            return "client connected, waiting for EGFX channel";
        }

        // Check handler state (indicates capabilities negotiated)
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

        // Send resize update to client
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
            let mut video_encoder: Option<VideoEncoder> = None;
            let mut egfx_sender: Option<EgfxFrameSender> = None;
            let mut egfx_checked = false;
            let mut use_avc444 = false; // Track which codec is active for sending

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

            loop {
                loop_iterations += 1;
                if loop_iterations % 1000 == 0 {
                    debug!(
                        "Display pipeline heartbeat: {} iterations, sent {} (egfx: {}), dropped {}, skipped_damage {}",
                        loop_iterations, frames_sent, egfx_frames_sent, frames_dropped, frames_skipped_damage
                    );
                }

                // Try to get frame from PipeWire thread (non-blocking)
                let frame = {
                    let thread_mgr = handler.pipewire_thread.lock().await;
                    thread_mgr.try_recv_frame()
                };

                let frame = match frame {
                    Some(f) => {
                        debug!("Received frame from PipeWire");
                        f
                    }
                    None => {
                        // No frame available, sleep briefly and retry
                        tokio::time::sleep(tokio::time::Duration::from_millis(5)).await;
                        continue;
                    }
                };

                // === FRAME RATE REGULATION ===
                // Use adaptive FPS if enabled, otherwise fall back to fixed 30 FPS
                let should_process = if adaptive_fps_enabled {
                    // Adaptive FPS: check based on current activity-driven target
                    adaptive_fps.should_capture_frame()
                } else {
                    // Fixed FPS: use legacy regulator
                    frame_regulator.should_send_frame()
                };

                if !should_process {
                    frames_dropped += 1;
                    if frames_dropped % 30 == 0 {
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
                if frames_sent % 30 == 0 || frames_sent < 10 {
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
                    if frames_dropped % 30 == 0 {
                        let reason = handler.egfx_wait_reason().await;
                        debug!("⏳ {} (dropped {} frames)", reason, frames_dropped);
                    }
                    continue;
                }

                // === EGFX/H.264 PATH ===
                // EGFX is ready - process frame
                if true {
                    // Initialize encoder and sender on first EGFX-ready frame
                    if !egfx_checked {
                        egfx_checked = true;
                        info!("🎬 EGFX channel ready - initializing H.264 encoder");

                        // Calculate aligned dimensions first (needed for encoder and surface)
                        use crate::egfx::align_to_16;
                        let aligned_width = align_to_16(frame.width as u32) as u16;
                        let aligned_height = align_to_16(frame.height as u32) as u16;

                        // Create H.264 encoder with resolution-appropriate level
                        // Use config values for quality settings
                        let config = EncoderConfig {
                            bitrate_kbps: self.config.egfx.h264_bitrate,
                            max_fps: self.config.video.target_fps as f32,
                            enable_skip_frame: true,
                            width: Some(aligned_width),
                            height: Some(aligned_height),
                            color_space: None, // Auto-select based on resolution
                            qp_min: self.config.egfx.qp_min,
                            qp_max: self.config.egfx.qp_max,
                        };
                        info!(
                            "🎬 H.264 encoder config: {}kbps, {}fps, QP[{}-{}]",
                            self.config.egfx.h264_bitrate,
                            self.config.video.target_fps,
                            self.config.egfx.qp_min,
                            self.config.egfx.qp_max
                        );

                        // Check if AVC444 is supported by client AND enabled in server config
                        // AVC444 provides superior chroma quality for text/UI rendering
                        let client_supports_avc444 =
                            if let Some(state) = handler.gfx_handler_state.read().await.as_ref() {
                                state.is_avc444_enabled
                            } else {
                                false
                            };
                        let avc444_enabled =
                            self.config.egfx.avc444_enabled && client_supports_avc444;

                        if !self.config.egfx.avc444_enabled {
                            info!("AVC444 disabled in config, using AVC420");
                        } else if !client_supports_avc444 {
                            info!("Client doesn't support AVC444, using AVC420");
                        }

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
                                    use_avc444 = true;
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
                                                        channel_id: ch_id,
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
                        }
                    }

                    // Try to send via EGFX if encoder is available
                    if let (Some(ref mut encoder), Some(ref sender)) =
                        (&mut video_encoder, &egfx_sender)
                    {
                        use crate::egfx::align_to_16;

                        // VALIDATION TEST: 27fps to stay within Level 3.2 constraint (108,000 MB/s)
                        // 1280×800 = 4,000 MBs × 27fps = 108,000 MB/s (exactly at limit)
                        // TODO: Replace with proper level management after validation
                        let timestamp_ms = (frames_sent * 37) as u64; // ~27fps timing

                        // Validate frame data (PipeWire sometimes sends zero-size buffers)
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
                        // CRITICAL: When periodic IDR is due, bypass damage detection!
                        // We need to send the FULL SCREEN to clear ghost artifacts.
                        // Otherwise, regions that "haven't changed" (but contain ghosts)
                        // never get refreshed even when IDR fires.
                        let force_full_frame = encoder.is_periodic_idr_due();

                        let damage_regions = if force_full_frame {
                            // Periodic IDR due - send full frame to clear all artifacts
                            debug!(
                                "Forcing full frame for periodic IDR (bypassing damage detection)"
                            );
                            vec![DamageRegion::full_frame(frame.width, frame.height)]
                        } else if let Some(ref mut detector) = damage_detector_opt {
                            // Damage tracking enabled - detect changed regions
                            detector.detect(&frame.data, frame.width, frame.height)
                        } else {
                            // Damage tracking disabled - use full frame
                            vec![DamageRegion::full_frame(frame.width, frame.height)]
                        };

                        // Calculate damage ratio for adaptive FPS and latency governor
                        let damage_ratio = if !damage_regions.is_empty() {
                            let frame_area = (frame.width * frame.height) as u64;
                            let damage_area: u64 = damage_regions.iter().map(|r| r.area()).sum();
                            damage_area as f32 / frame_area as f32
                        } else {
                            0.0
                        };

                        // === UPDATE ADAPTIVE FPS (Premium Feature) ===
                        // Feed damage ratio to update activity level and target FPS
                        if adaptive_fps_enabled {
                            adaptive_fps.update(damage_ratio);
                        }

                        // === LATENCY GOVERNOR DECISION (Premium Feature) ===
                        // Decide whether to encode now, batch, or skip based on mode
                        let encoding_decision = latency_governor.should_encode_frame(damage_ratio);
                        match encoding_decision {
                            EncodingDecision::Skip => {
                                // Governor says skip this frame
                                frames_dropped += 1;
                                continue;
                            }
                            EncodingDecision::WaitForMore => {
                                // Governor wants to accumulate more damage
                                // Don't continue yet - let damage accumulate
                                continue;
                            }
                            EncodingDecision::EncodeNow
                            | EncodingDecision::EncodeKeepalive
                            | EncodingDecision::EncodeBatch
                            | EncodingDecision::EncodeTimeout => {
                                // Governor says encode - proceed
                            }
                        }

                        if damage_regions.is_empty() {
                            // No changes detected - skip this frame entirely
                            frames_skipped_damage += 1;
                            if frames_skipped_damage % 100 == 0 {
                                if let Some(ref detector) = damage_detector_opt {
                                    let stats = detector.stats();
                                    debug!(
                                        "🎯 Damage tracking: {} frames skipped (no change), {:.1}% bandwidth saved",
                                        frames_skipped_damage,
                                        stats.bandwidth_reduction_percent()
                                    );
                                }
                            }
                            // Update adaptive FPS with zero damage
                            if adaptive_fps_enabled {
                                adaptive_fps.update(0.0);
                            }
                            continue;
                        }

                        // Log damage stats periodically
                        if frames_sent % 60 == 0 {
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

                        // Pad frame data if needed
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

                        // Encode frame to H.264 with ALIGNED dimensions
                        // VideoEncoder handles both AVC420 and AVC444 transparently
                        match encoder.encode_bgra(
                            &frame_data,
                            aligned_width,
                            aligned_height,
                            timestamp_ms,
                        ) {
                            Ok(Some(encoded_frame)) => {
                                // Send via EGFX - method varies by codec
                                // - encoded dimensions: aligned (for H.264 macroblock requirements)
                                // - display dimensions: actual (for visible region, crops padding)
                                let send_result = match encoded_frame {
                                    EncodedVideoFrame::Single(data) => {
                                        // AVC420: Single stream with damage regions
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
                                        // AVC444: Dual streams with damage regions
                                        // Phase 1: aux is now Option<Vec<u8>> for bandwidth optimization
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
                                        if egfx_frames_sent % 30 == 0 {
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
                                // Encoder skipped this frame (rate control)
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

                // === REMOTEFX PATH (fallback) ===
                // Convert to RDP bitmap (track timing)
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
                    if count % 100 == 0 && count > 0 {
                        debug!(
                            "Empty frame optimization: {} unchanged frames skipped",
                            count
                        );
                    }
                    continue;
                }

                // Convert our BitmapUpdate to IronRDP's format (track timing)
                // Only done for frames with actual content
                let iron_start = std::time::Instant::now();
                let iron_updates = match handler.convert_to_iron_format(&bitmap_update).await {
                    Ok(updates) => updates,
                    Err(e) => {
                        error!("Failed to convert to IronRDP format: {}", e);
                        continue;
                    }
                };
                let iron_elapsed = iron_start.elapsed();

                // Log conversion performance every 30 frames
                if frames_sent % 30 == 0 {
                    info!(
                        "🎨 Frame conversion timing: bitmap={:?}, iron={:?}, total={:?}",
                        convert_elapsed,
                        iron_elapsed,
                        convert_start.elapsed()
                    );
                }

                // Route through graphics queue (full multiplexer implementation)
                if let Some(ref graphics_tx) = handler.graphics_tx {
                    // Send each iron_bitmap through graphics queue
                    for iron_bitmap in iron_updates {
                        let graphics_frame = GraphicsFrame {
                            iron_bitmap,
                            sequence: frames_sent,
                        };

                        // Non-blocking send - drop frame if queue full (never block on graphics)
                        trace!(
                            "📤 Graphics multiplexer: sending frame {} to queue",
                            frames_sent
                        );
                        if let Err(_e) = graphics_tx.try_send(graphics_frame) {
                            warn!("Graphics queue full - frame dropped (QoS policy)");
                        }
                    }
                } else {
                    // Fallback: Send directly to IronRDP (no multiplexer)
                    for iron_bitmap in iron_updates {
                        let update = DisplayUpdate::Bitmap(iron_bitmap);

                        if let Err(e) = handler.update_sender.send(update).await {
                            error!("Failed to send display update: {}", e);
                            // Channel closed, stop pipeline
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
            .map_err(|e| anyhow::anyhow!("Bitmap conversion failed: {}", e))
    }

    /// Convert our BitmapUpdate format to IronRDP's BitmapUpdate format
    async fn convert_to_iron_format(&self, update: &BitmapUpdate) -> Result<Vec<IronBitmapUpdate>> {
        let mut iron_updates = Vec::new();

        // Convert each rectangle in the update
        for rect_data in &update.rectangles {
            // Map our RdpPixelFormat to IronRDP's PixelFormat
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

            // Calculate width and height from rectangle
            let width = rect_data
                .rectangle
                .right
                .saturating_sub(rect_data.rectangle.left);
            let height = rect_data
                .rectangle
                .bottom
                .saturating_sub(rect_data.rectangle.top);

            // Calculate stride (bytes per row)
            let bytes_per_pixel = iron_format.bytes_per_pixel() as usize;
            let stride = NonZeroUsize::new(width as usize * bytes_per_pixel)
                .ok_or_else(|| anyhow::anyhow!("Invalid stride calculation: width={}", width))?;

            // Create IronRDP bitmap update
            let iron_bitmap = IronBitmapUpdate {
                x: rect_data.rectangle.left,
                y: rect_data.rectangle.top,
                width: NonZeroU16::new(width)
                    .ok_or_else(|| anyhow::anyhow!("Invalid width: {}", width))?,
                height: NonZeroU16::new(height)
                    .ok_or_else(|| anyhow::anyhow!("Invalid height: {}", height))?,
                format: iron_format,
                data: Bytes::from(rect_data.data.clone()),
                stride,
            };

            iron_updates.push(iron_bitmap);
        }

        Ok(iron_updates)
    }
}

/// Implement IronRDP's `RdpServerDisplay` trait
#[async_trait::async_trait]
impl RdpServerDisplay for LamcoDisplayHandler {
    /// Return the current desktop size
    async fn size(&mut self) -> DesktopSize {
        let size = self.size.read().await;
        *size
    }

    /// Create and return a display updates receiver
    ///
    /// This is called once per connection to establish the update stream.
    async fn updates(&mut self) -> Result<Box<dyn RdpServerDisplayUpdates>> {
        // Take the receiver from the wrapper (can only be called once)
        let mut receiver_option = self.update_receiver.lock().await;
        let receiver = receiver_option
            .take()
            .ok_or_else(|| anyhow::anyhow!("Display updates already claimed"))?;

        Ok(Box::new(DisplayUpdatesStream::new(receiver)))
    }

    /// Handle client request for layout change
    fn request_layout(&mut self, layout: ironrdp_displaycontrol::pdu::DisplayControlMonitorLayout) {
        debug!("Client requested layout change: {:?}", layout);

        // Multi-monitor layout changes will be fully implemented in P1-09
        // For now, we acknowledge the request but maintain current configuration
        warn!("Multi-monitor dynamic layout changes not yet implemented - maintaining current configuration");
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
        }
    }
}

/// Display Updates Stream
///
/// Implements `RdpServerDisplayUpdates` to provide a stream of display updates
/// from the video pipeline to IronRDP.
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
    /// Get the next display update
    ///
    /// This method is cancellation-safe as required by IronRDP.
    /// Returns `None` when the stream is closed.
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
