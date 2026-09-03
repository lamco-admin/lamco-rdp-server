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
    os::fd::{IntoRawFd, OwnedFd},
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::Result;
use bytes::Bytes;
use ironrdp_server::{
    BitmapUpdate as IronBitmapUpdate, DesktopSize, DisplayUpdate, GfxServerHandle,
    PixelFormat as IronPixelFormat, RdpServerDisplay, RdpServerDisplayUpdates, ServerEvent,
    ServerResult,
};
use tokio::sync::{Mutex, RwLock, mpsc};
use tracing::{debug, error, info, trace, warn};

use super::pipeline_decisions;
use crate::{
    damage::{DamageConfig, DamageDetector, DamageRegion},
    egfx::{Avc420Encoder, Avc444Encoder, ColorSpaceConfig, EncoderConfig},
    performance::{AdaptiveFpsController, EncodingDecision, LatencyGovernor, LatencyMode},
    pipewire::{PipeWireThreadCommand, PipeWireThreadManager, VideoFrame},
    portal::StreamInfo,
    server::{
        egfx_sender::EgfxFrameSender, event_multiplexer::GraphicsFrame,
        gfx_factory::SharedHandlerState, input_handler::LamcoInputHandler,
    },
    services::{ServiceId, ServiceRegistry},
    video::{BitmapConverter, BitmapUpdate, RdpPixelFormat, Rectangle},
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

    /// Active encode backend name for telemetry ("openh264" / "vaapi").
    fn backend_name(&self) -> &'static str {
        match self {
            VideoEncoder::Avc420(e) => e.backend_name(),
            VideoEncoder::Avc444(e) => e.backend_name(),
        }
    }

    /// Get codec name for logging
    fn codec_name(&self) -> &'static str {
        match self {
            VideoEncoder::Avc420(_) => "AVC420",
            VideoEncoder::Avc444(_) => "AVC444",
        }
    }

    /// Request IDR keyframe (for PLI or stress-triggered early-IDR via L2)
    ///
    /// Forces the next encoded frame to be a full IDR keyframe,
    /// clearing any accumulated compression artifacts.
    fn request_idr(&mut self) {
        match self {
            VideoEncoder::Avc420(encoder) => encoder.force_keyframe(),
            VideoEncoder::Avc444(encoder) => encoder.request_idr(),
        }
    }

    /// Milliseconds since the last IDR was emitted.
    ///
    /// Returns `u64::MAX` for AVC420 (no IDR tracking — every keyframe is an IDR,
    /// so any call site that uses this is implicitly asking about AVC444 stress).
    fn ms_since_last_idr(&self) -> u64 {
        match self {
            VideoEncoder::Avc420(_) => u64::MAX,
            VideoEncoder::Avc444(encoder) => encoder.ms_since_last_idr(),
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

/// Wayland client observers, wired in after construction via
/// `set_wayland_observers`. Each is present only when the compositor exposes
/// the matching protocol.
#[cfg(feature = "wayland")]
#[derive(Default, Clone)]
struct WaylandObservers {
    color: std::sync::OnceLock<crate::compositor::ColorObserver>,
    output: std::sync::OnceLock<crate::compositor::OutputObserver>,
}

/// Map an observed output transfer to the tone-mapper's source transfer;
/// `None` for SDR/unknown (no tone mapping needed).
#[cfg(feature = "wayland")]
fn hdr_source_transfer(
    t: crate::compositor::OutputTransfer,
) -> Option<crate::egfx::tone_map::SourceTransfer> {
    use crate::{compositor::OutputTransfer, egfx::tone_map::SourceTransfer};
    match t {
        OutputTransfer::Pq => Some(SourceTransfer::Pq),
        OutputTransfer::Hlg => Some(SourceTransfer::Hlg),
        OutputTransfer::Sdr | OutputTransfer::Unknown => None,
    }
}

/// RDP Display Handler
///
/// Shared handle to the encoder telemetry slot the `SnapshotCollector` reads.
///
/// Two levels of `Option` are load-bearing: the outer one is "no collector has
/// been attached yet", the inner one is "attached, but the encoder has not
/// reported a snapshot yet". The outer lock is async because it is set from the
/// server task; the inner is a `parking_lot` lock because the encode path
/// touches it per frame and must not await.
type EncoderStateSlot = Arc<
    RwLock<Option<Arc<parking_lot::RwLock<Option<crate::health::performance::EncoderSnapshot>>>>>,
>;

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
    /// Arc-wrapped so the pipeline task and IronRDP's clone share the same sender.
    /// On reconnection, updates() swaps this to a new channel — both sides must
    /// see the swap, or the pipeline sends to a dead channel.
    update_sender: Arc<tokio::sync::Mutex<mpsc::Sender<DisplayUpdate>>>,

    /// Display update receiver (wrapped for cloning)
    update_receiver: Arc<Mutex<Option<mpsc::Receiver<DisplayUpdate>>>>,

    /// Graphics queue sender (for priority multiplexing)
    graphics_tx: Option<mpsc::Sender<GraphicsFrame>>,

    /// Monitor configuration from streams
    stream_info: Vec<StreamInfo>,

    /// Server-driven cursor shape/position strategy (mode selection,
    /// shape-change tracking, predictive rendering state).
    cursor_strategy: Arc<Mutex<crate::cursor::CursorStrategy>>,

    /// Server-published NetworkAutoDetect RTT (MS-RDPBCGR 2.2.14), same
    /// handle the EGFX FlowController reads (`server/mod.rs` creates it and
    /// hands it to the ironrdp-server builder, which writes measured RTT
    /// into it; `u32::MAX` until the first measurement). Set post-construction
    /// via `set_autodetect_rtt_handle`, feeding `CursorStrategy::update_latency`
    /// for Predictive-mode auto-switching and lookahead tuning.
    autodetect_rtt: Arc<RwLock<Option<Arc<std::sync::atomic::AtomicU32>>>>,

    // === EGFX/H.264 Support ===
    /// Shared GFX server handle for EGFX frame sending
    /// Populated by GfxFactory after channel attachment
    gfx_server_handle: Arc<RwLock<Option<GfxServerHandle>>>,

    /// Handler state for checking EGFX readiness (None when EGFX not configured)
    gfx_handler_state: Option<Arc<SharedHandlerState>>,

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

    /// Live PipeWire capture node id. A session re-establishment (PerConnection
    /// lifecycle) rebinds capture to a new node via `rebind_capture_node`; this
    /// tracks it so a subsequent client resize acts on the current node rather
    /// than the one captured at startup (which `stream_info` still holds).
    capture_node: Arc<std::sync::atomic::AtomicU32>,

    /// Set true by `on_client_disconnect` on a real disconnect; consumed
    /// (swap→false) by the connect-start reset in `updates()`. Distinguishes a
    /// genuine new-client reconnection from a same-connection
    /// DeactivationReactivation (display resize) — the latter re-enters
    /// `updates()` but must NOT tear down clipboard lifecycle.
    saw_real_disconnect: Arc<std::sync::atomic::AtomicBool>,

    /// Health reporter for forwarding PipeWire stream state to health monitor
    health_reporter: Arc<RwLock<Option<crate::health::HealthReporter>>>,

    /// PipeWire sensor for version-adaptive health monitoring (set via set_pipewire_sensor)
    pipewire_sensor: Arc<RwLock<Option<Arc<crate::health::sensors::pipewire::PipeWireSensor>>>>,

    /// EGFX performance snapshot for encoding adaptation feedback loop
    egfx_snapshot:
        Arc<RwLock<Option<Arc<parking_lot::RwLock<crate::health::performance::EgfxSnapshot>>>>>,

    /// FPS controller performance snapshot for D-Bus/GUI live-metrics reporting
    /// (set via set_fps_state)
    fps_state:
        Arc<RwLock<Option<Arc<parking_lot::RwLock<crate::health::performance::FpsSnapshot>>>>>,

    /// Encoder stats sink shared with the SnapshotCollector (set via
    /// set_encoder_state). Written with the active backend name when the encoder
    /// is created so telemetry reports "openh264"/"vaapi" instead of empty.
    encoder_state: EncoderStateSlot,

    /// Shared stream active flag for Portal input coupling.
    /// Updated on PipeWire state transitions. Portal session reads this
    /// to suppress input injection when the stream is paused.
    /// None for non-Portal strategies (input is independent of stream state).
    stream_active_flag: parking_lot::RwLock<Option<Arc<std::sync::atomic::AtomicBool>>>,

    /// Scale the compositor reports for the captured output, from the
    /// `org.gnome.scale` stream tag.
    ///
    /// `None` until a producer publishes it, and `None` for producers that
    /// never do. Mutter sets this tag only on a virtual-monitor stream, so in
    /// practice it appears on headless GNOME sessions and nowhere else today.
    ///
    /// Live state, not configuration: it is refreshed whenever the producer
    /// republishes its tags, and it describes the stream that is running now.
    compositor_scale: parking_lot::RwLock<Option<f64>>,

    /// True when using direct frame channel (portal-generic) instead of PipeWire.
    /// Resize via PipeWire DestroyStream/CreateStream is not available in this mode.
    direct_channel_mode: bool,

    /// Whether to request DMA-BUF buffers from PipeWire.
    /// Set based on compositor recommendation (DmaBuf for real GPUs, MemFd for virtual/software).
    use_dmabuf: bool,

    /// Wayland color/output observers, wired in after construction via
    /// `set_wayland_observers`.
    #[cfg(feature = "wayland")]
    wayland_observers: WaylandObservers,
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
        gfx_handler_state: Option<Arc<SharedHandlerState>>,
        config: Arc<crate::config::Config>,
        service_registry: Arc<ServiceRegistry>,
        use_dmabuf: bool,
        client_active: Arc<std::sync::atomic::AtomicBool>,
    ) -> Result<Self> {
        let size = Arc::new(RwLock::new(DesktopSize {
            width: initial_width,
            height: initial_height,
        }));

        let pipewire_thread = Arc::new(Mutex::new(
            PipeWireThreadManager::new(pipewire_fd.into_raw_fd())
                .map_err(|e| anyhow::anyhow!("Failed to create PipeWire thread: {e}"))?,
        ));

        for (idx, stream) in stream_info.iter().enumerate() {
            let config = lamco_pipewire::StreamConfig {
                name: format!("monitor-{idx}"),
                width: stream.size.0,
                height: stream.size.1,
                framerate: 60,
                use_dmabuf,
                buffer_count: 3,
                preferred_format: Some(lamco_pipewire::PixelFormat::BGRx),
                dmabuf_passthrough: false,
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
        let update_sender = Arc::new(tokio::sync::Mutex::new(update_sender));
        let update_receiver = Arc::new(Mutex::new(Some(update_receiver)));

        let gfx_server_handle = gfx_server_handle.unwrap_or_else(|| Arc::new(RwLock::new(None)));

        debug!(
            "Display handler created: {}x{}, {} streams, EGFX={}",
            initial_width,
            initial_height,
            stream_info.len(),
            gfx_handler_state.is_some()
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
            capture_node: std::sync::Arc::new(std::sync::atomic::AtomicU32::new(
                stream_info.first().map_or(0, |s| s.node_id),
            )),
            cursor_strategy: Arc::new(Mutex::new(crate::cursor::CursorStrategy::new(
                (&config.cursor).into(),
            ))),
            autodetect_rtt: Arc::new(RwLock::new(None)),
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
            client_active,
            saw_real_disconnect: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            health_reporter: Arc::new(RwLock::new(None)),
            pipewire_sensor: Arc::new(RwLock::new(None)),
            egfx_snapshot: Arc::new(RwLock::new(None)),
            fps_state: Arc::new(RwLock::new(None)),
            encoder_state: Arc::new(RwLock::new(None)),
            stream_active_flag: parking_lot::RwLock::new(None),
            compositor_scale: parking_lot::RwLock::new(None),
            direct_channel_mode: false,
            use_dmabuf,
            #[cfg(feature = "wayland")]
            wayland_observers: WaylandObservers::default(),
        })
    }

    /// Create display handler with a direct frame channel (no PipeWire fd).
    ///
    /// Used by portal-generic where screencopy delivers frames via mpsc channel
    /// rather than through PipeWire's buffer sharing mechanism.
    #[expect(
        clippy::too_many_arguments,
        reason = "display handler needs pipeline components at construction"
    )]
    pub async fn new_direct(
        initial_width: u16,
        initial_height: u16,
        raw_rx: std::sync::mpsc::Receiver<lamco_pipewire::frame::RawFrameData>,
        stream_info: Vec<StreamInfo>,
        graphics_tx: Option<mpsc::Sender<GraphicsFrame>>,
        gfx_server_handle: Option<Arc<RwLock<Option<GfxServerHandle>>>>,
        gfx_handler_state: Option<Arc<SharedHandlerState>>,
        config: Arc<crate::config::Config>,
        service_registry: Arc<ServiceRegistry>,
        client_active: Arc<std::sync::atomic::AtomicBool>,
    ) -> Result<Self> {
        let size = Arc::new(RwLock::new(DesktopSize {
            width: initial_width,
            height: initial_height,
        }));

        let pipewire_thread = Arc::new(Mutex::new(PipeWireThreadManager::new_direct(
            raw_rx,
            initial_width as u32,
            initial_height as u32,
        )));

        info!(
            "Display handler created (direct channel): {}x{}, {} streams",
            initial_width,
            initial_height,
            stream_info.len(),
        );

        let bitmap_converter = Arc::new(Mutex::new(BitmapConverter::new(
            initial_width,
            initial_height,
        )));

        let (update_sender, update_receiver) = mpsc::channel(64);
        let update_sender = Arc::new(tokio::sync::Mutex::new(update_sender));
        let update_receiver = Arc::new(Mutex::new(Some(update_receiver)));

        let gfx_server_handle = gfx_server_handle.unwrap_or_else(|| Arc::new(RwLock::new(None)));

        let (resize_tx, resize_rx) = std::sync::mpsc::sync_channel(4);

        Ok(Self {
            size,
            pipewire_thread,
            bitmap_converter,
            update_sender,
            update_receiver,
            graphics_tx,
            capture_node: std::sync::Arc::new(std::sync::atomic::AtomicU32::new(
                stream_info.first().map_or(0, |s| s.node_id),
            )),
            cursor_strategy: Arc::new(Mutex::new(crate::cursor::CursorStrategy::new(
                (&config.cursor).into(),
            ))),
            autodetect_rtt: Arc::new(RwLock::new(None)),
            stream_info,
            gfx_server_handle,
            gfx_handler_state,
            server_event_tx: Arc::new(RwLock::new(None)),
            config,
            service_registry,
            egfx_needs_init: Arc::new(std::sync::atomic::AtomicBool::new(true)),
            input_handler: Arc::new(RwLock::new(None)),
            clipboard_manager: Arc::new(RwLock::new(None)),
            resize_tx,
            resize_rx: Arc::new(std::sync::Mutex::new(Some(resize_rx))),
            last_resize_time: std::sync::Mutex::new(
                Instant::now()
                    .checked_sub(std::time::Duration::from_secs(10))
                    .unwrap_or(Instant::now()),
            ),
            client_active,
            saw_real_disconnect: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            health_reporter: Arc::new(RwLock::new(None)),
            pipewire_sensor: Arc::new(RwLock::new(None)),
            egfx_snapshot: Arc::new(RwLock::new(None)),
            fps_state: Arc::new(RwLock::new(None)),
            encoder_state: Arc::new(RwLock::new(None)),
            stream_active_flag: parking_lot::RwLock::new(None),
            compositor_scale: parking_lot::RwLock::new(None),
            direct_channel_mode: true,
            use_dmabuf: false, // direct channel is always CPU-resident
            #[cfg(feature = "wayland")]
            wayland_observers: WaylandObservers::default(),
        })
    }

    /// Wire the Wayland color/output observers after construction. Called once
    /// at server startup; repeat calls are ignored.
    #[cfg(feature = "wayland")]
    pub fn set_wayland_observers(
        &self,
        color: Option<crate::compositor::ColorObserver>,
        output: Option<crate::compositor::OutputObserver>,
    ) {
        if let Some(c) = color {
            let _ = self.wayland_observers.color.set(c);
        }
        if let Some(o) = output {
            let _ = self.wayland_observers.output.set(o);
        }
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

    /// Wire the shared NetworkAutoDetect RTT handle (same one the EGFX
    /// FlowController reads) so the cursor prediction loop can feed
    /// measured latency into `CursorStrategy::update_latency`.
    pub async fn set_autodetect_rtt_handle(&self, handle: Arc<std::sync::atomic::AtomicU32>) {
        *self.autodetect_rtt.write().await = Some(handle);
    }

    /// Wire the health reporter so PipeWire stream state events propagate
    /// to the session health monitor.
    pub async fn set_health_reporter(&self, reporter: crate::health::HealthReporter) {
        *self.health_reporter.write().await = Some(reporter);
    }

    /// Set the shared stream active flag for Portal input coupling.
    ///
    /// The display handler updates this flag on PipeWire state transitions.
    /// The Portal session handle reads it before attempting input injection,
    /// preventing hundreds of futile D-Bus calls when the stream is paused.
    pub fn set_stream_active_flag(&self, flag: Arc<std::sync::atomic::AtomicBool>) {
        *self.stream_active_flag.write() = Some(flag);
    }

    /// Scale the compositor reports for the captured output, if it publishes one.
    ///
    /// Read from the `org.gnome.scale` stream tag. `None` means the producer
    /// does not publish a scale, which is the case for every capture source
    /// except a GNOME virtual monitor.
    pub fn compositor_scale(&self) -> Option<f64> {
        *self.compositor_scale.read()
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

    /// Tell the clipboard orchestrator the RDP client disconnected, so it drops
    /// its Ready latch, clears per-connection state, and releases local
    /// clipboard ownership. Best-effort — a missing manager is not fatal.
    pub async fn notify_clipboard_disconnect(&self) {
        let mgr_opt = self.clipboard_manager.read().await.clone();
        if let Some(mgr) = mgr_opt {
            let tx = mgr.lock().await.event_sender();
            if let Err(e) = tx
                .send(crate::clipboard::ClipboardEvent::RdpDisconnect)
                .await
            {
                warn!("Failed to notify clipboard of RDP disconnect: {e}");
            }
        }
    }

    /// Signal that the client has disconnected.
    ///
    /// The pipeline loop checks `client_active` and skips encoding/sending when
    /// no client is connected. PipeWire frames are still drained to keep the
    /// stream healthy, but no CPU is wasted on encoding or queue pressure.
    pub fn on_client_disconnect(&self) {
        self.client_active
            .store(false, std::sync::atomic::Ordering::SeqCst);
        // Record that a real disconnect happened so the next connect-start reset
        // in updates() drives clipboard teardown. A resize-driven
        // DeactivationReactivation leaves this false and is left alone.
        self.saw_real_disconnect
            .store(true, std::sync::atomic::Ordering::SeqCst);
        info!("Client disconnect signaled to pipeline - frame processing paused");
    }

    /// Rebind the capture pipeline to a new PipeWire node after a session
    /// re-establishment (the `PerConnection` lifecycle re-creates the compositor
    /// session, which yields a fresh node). Destroys the stream on the old node
    /// and creates one on the new node; a no-op if the node is unchanged (the
    /// common first-connection case, where the startup session is reused).
    ///
    /// NOTE: this does not rewrite `stream_info`, so a client resize *after* a
    /// rebind still references the startup node. Revisit if per-connection
    /// resize on the Mutter-direct path becomes a requirement.
    pub async fn rebind_capture_node(
        &self,
        old_node: u32,
        new_node: u32,
        width: u32,
        height: u32,
    ) -> bool {
        if old_node == new_node {
            // Not a no-op: the re-established session's stream can land on the
            // same node id as the stopped one. The old PipeWire stream is dead,
            // so we still Destroy + Create to reconnect to the new source.
            debug!(
                "[capture-rebind] Node id {new_node} reused by re-established session — recreating stream"
            );
        }
        info!(
            old_node,
            new_node, width, height, "[capture-rebind] Rebinding capture pipeline to new node"
        );

        // Destroy the stream on the old (now-defunct) node.
        let (resp_tx, resp_rx) = std::sync::mpsc::sync_channel(1);
        {
            let mgr = self.pipewire_thread.lock().await;
            if let Err(e) = mgr.send_command(PipeWireThreadCommand::DestroyStream {
                stream_id: old_node,
                response_tx: resp_tx,
            }) {
                warn!("[capture-rebind] Failed to send DestroyStream for old node {old_node}: {e}");
            } else {
                match resp_rx.recv_timeout(std::time::Duration::from_secs(5)) {
                    Ok(Ok(())) => info!("[capture-rebind] Old stream {old_node} destroyed"),
                    Ok(Err(e)) => warn!("[capture-rebind] DestroyStream({old_node}) failed: {e}"),
                    Err(_) => warn!("[capture-rebind] DestroyStream({old_node}) timeout"),
                }
            }
        }

        // Create a stream on the re-established node.
        let config = lamco_pipewire::StreamConfig {
            name: "monitor-0".to_string(),
            width,
            height,
            framerate: 60,
            use_dmabuf: self.use_dmabuf,
            buffer_count: 3,
            preferred_format: Some(lamco_pipewire::PixelFormat::BGRx),
            dmabuf_passthrough: false,
        };
        let (resp_tx2, resp_rx2) = std::sync::mpsc::sync_channel(1);
        let mgr = self.pipewire_thread.lock().await;
        if let Err(e) = mgr.send_command(PipeWireThreadCommand::CreateStream {
            stream_id: new_node,
            node_id: new_node,
            config,
            response_tx: resp_tx2,
        }) {
            warn!("[capture-rebind] Failed to send CreateStream for new node {new_node}: {e}");
            return false;
        }
        match resp_rx2.recv_timeout(std::time::Duration::from_secs(5)) {
            Ok(Ok(())) => {
                self.capture_node
                    .store(new_node, std::sync::atomic::Ordering::Relaxed);
                info!("[capture-rebind] New stream {new_node} created at {width}x{height}");
                true
            }
            Ok(Err(e)) => {
                warn!("[capture-rebind] CreateStream({new_node}) failed: {e}");
                false
            }
            Err(_) => {
                warn!("[capture-rebind] CreateStream({new_node}) timeout");
                false
            }
        }
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
    /// Set EGFX snapshot for encoding adaptation feedback loop
    pub async fn set_egfx_snapshot(
        &self,
        snapshot: Arc<parking_lot::RwLock<crate::health::performance::EgfxSnapshot>>,
    ) {
        *self.egfx_snapshot.write().await = Some(snapshot);
    }

    /// Set FPS controller snapshot handle for D-Bus/GUI live-metrics reporting
    pub async fn set_fps_state(
        &self,
        state: Arc<parking_lot::RwLock<crate::health::performance::FpsSnapshot>>,
    ) {
        *self.fps_state.write().await = Some(state);
    }

    pub async fn set_encoder_state(
        &self,
        state: Arc<parking_lot::RwLock<Option<crate::health::performance::EncoderSnapshot>>>,
    ) {
        *self.encoder_state.write().await = Some(state);
    }

    /// Set PipeWire sensor for version-adaptive health monitoring
    pub async fn set_pipewire_sensor(
        &self,
        sensor: Arc<crate::health::sensors::pipewire::PipeWireSensor>,
    ) {
        *self.pipewire_sensor.write().await = Some(sensor);
    }

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
        *self.update_sender.lock().await = new_sender;
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
    /// - Handler state indicates readiness (capabilities negotiated)
    /// - Server event sender is configured
    pub async fn is_egfx_ready(&self) -> bool {
        if self.server_event_tx.read().await.is_none() {
            return false;
        }

        if self.gfx_server_handle.read().await.is_none() {
            return false;
        }

        if let Some(ref state) = self.gfx_handler_state {
            state.is_ready.load(std::sync::atomic::Ordering::Acquire)
        } else {
            false
        }
    }

    /// Check if AVC420 (H.264) codec is available
    pub async fn is_avc_supported(&self) -> bool {
        if let Some(ref state) = self.gfx_handler_state {
            state
                .client_supports_avc420
                .load(std::sync::atomic::Ordering::Acquire)
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

        if let Some(ref state) = self.gfx_handler_state {
            if !state.is_ready.load(std::sync::atomic::Ordering::Acquire) {
                return "EGFX channel open, negotiating capabilities";
            }
            if !state
                .client_supports_avc420
                .load(std::sync::atomic::Ordering::Acquire)
            {
                return "EGFX ready, no AVC420 - using bitmap fallback";
            }
        } else {
            return "EGFX not configured";
        }

        "ready" // Should not reach here if is_egfx_ready() is false
    }

    /// Set up EGFX surface for frame delivery (shared by AVC and V8 paths).
    ///
    /// Creates the EGFX surface (ResetGraphics + CreateSurface + MapSurfaceToOutput),
    /// sends setup PDUs to the client, and returns an EgfxFrameSender.
    /// Returns None if gfx_server_handle or server_event_tx is unavailable.
    async fn setup_egfx_surface(
        &self,
        frame_width: u32,
        frame_height: u32,
        aligned_width: u16,
        aligned_height: u16,
    ) -> Option<EgfxFrameSender> {
        let gfx_handle = self.gfx_server_handle.read().await.clone()?;
        let event_tx = self.server_event_tx.read().await.clone()?;

        // Create primary surface for EGFX rendering
        // Must be done BEFORE sending any frames
        // MS-RDPEGFX REQUIRES 16-pixel alignment!
        {
            info!(
                "Aligning surface: {}x{} -> {}x{} (16-pixel boundary)",
                frame_width, frame_height, aligned_width, aligned_height
            );

            #[expect(clippy::expect_used, reason = "mutex poisoning is unrecoverable")]
            let mut server = gfx_handle.lock().expect("GfxServerHandle mutex poisoned");

            // Set desktop size BEFORE creating surface.
            // This prevents desktop size mismatch when ResetGraphics is auto-sent.
            // Desktop = actual resolution, Surface = aligned resolution.
            server.set_output_dimensions(frame_width as u16, frame_height as u16);
            info!(
                "EGFX desktop dimensions set: {}x{} (actual)",
                frame_width, frame_height
            );

            // Create surface with ALIGNED dimensions
            // create_surface() will auto-send ResetGraphics using output_dimensions
            if let Some(surface_id) = server.create_surface(aligned_width, aligned_height) {
                info!(
                    "EGFX surface {} created ({}x{} aligned)",
                    surface_id, aligned_width, aligned_height
                );
                // Map surface to output at origin (0,0)
                if server.map_surface_to_output(surface_id, 0, 0) {
                    info!("EGFX surface {} mapped to output", surface_id);
                } else {
                    warn!("Failed to map EGFX surface to output");
                }

                // Send the CreateSurface and MapSurfaceToOutput PDUs to client
                let channel_id = server.channel_id();
                let dvc_messages = server.drain_output();
                if !dvc_messages.is_empty() {
                    info!(
                        "EGFX: drain_output returned {} DVC messages for surface setup",
                        dvc_messages.len()
                    );
                    for (i, msg) in dvc_messages.iter().enumerate() {
                        info!("  DVC msg {}: {} bytes", i, msg.size());
                    }

                    if let Some(ch_id) = channel_id {
                        use ironrdp_dvc::encode_dvc_messages;
                        use ironrdp_server::EgfxServerMessage;
                        use ironrdp_svc::ChannelFlags;

                        match encode_dvc_messages(ch_id, dvc_messages, ChannelFlags::SHOW_PROTOCOL)
                        {
                            Ok(svc_messages) => {
                                info!(
                                    "EGFX: Encoded {} SVC messages for DVC channel {}",
                                    svc_messages.len(),
                                    ch_id
                                );
                                let msg = EgfxServerMessage::SendMessages {
                                    messages: svc_messages,
                                };
                                let _ = event_tx.send(ServerEvent::Egfx(msg));
                                info!("EGFX surface PDUs sent to client");
                            }
                            Err(e) => {
                                error!("EGFX: Failed to encode DVC messages: {:?}", e);
                            }
                        }
                    }
                }
            } else {
                warn!("Failed to create EGFX surface - server may not be ready");
                return None;
            }
        }

        #[expect(
            clippy::expect_used,
            reason = "invariant: gfx_handler_state is set during EGFX init; caller gated on EGFX active"
        )]
        let sender = EgfxFrameSender::new(
            gfx_handle,
            Arc::clone(
                self.gfx_handler_state
                    .as_ref()
                    .expect("gfx_handler_state must be Some when EGFX is initialized"),
            ),
            event_tx,
        );
        info!("EGFX frame sender initialized");

        Some(sender)
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
        if let Err(e) = self.update_sender.lock().await.send(update).await {
            warn!("Failed to send resize update: {}", e);
        }
    }

    /// Get a shared reference to the update sender for graphics drain task
    ///
    /// This is used by the Phase 1 multiplexer to get access to the IronRDP update channel.
    /// Returns an Arc so the drain task and the handler share the same sender — when the
    /// channel is recreated on reconnection, both sides see the new sender.
    pub fn get_update_sender(&self) -> Arc<tokio::sync::Mutex<mpsc::Sender<DisplayUpdate>>> {
        Arc::clone(&self.update_sender)
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

    /// Periodically re-emit `PointerPosition` at the cursor strategy's
    /// predicted position, so Predictive mode keeps advancing between raw
    /// cursor samples rather than only moving when a fresh compositor frame
    /// happens to carry one. Also the only place that feeds the shared
    /// NetworkAutoDetect RTT handle into `CursorStrategy::update_latency`,
    /// since RTT measurements arrive independently of cursor frames.
    ///
    /// Runs for the lifetime of the display handler, mirroring the main
    /// pipeline task; ticks at `CursorStrategyConfig::cursor_update_fps` and
    /// does nothing but wait when no client is connected or the active mode
    /// isn't Predictive.
    fn start_cursor_prediction_loop(self: Arc<Self>) {
        tokio::spawn(async move {
            loop {
                let fps = self.cursor_strategy.lock().await.update_fps().max(1);
                tokio::time::sleep(Duration::from_millis(1000 / u64::from(fps))).await;

                if !self
                    .client_active
                    .load(std::sync::atomic::Ordering::Acquire)
                {
                    continue;
                }

                let rtt_ms = self
                    .autodetect_rtt
                    .read()
                    .await
                    .as_ref()
                    .map(|handle| handle.load(std::sync::atomic::Ordering::Acquire));

                let predicted = {
                    let mut strategy = self.cursor_strategy.lock().await;
                    if let Some(rtt_ms) = rtt_ms
                        && rtt_ms != u32::MAX
                    {
                        strategy.update_latency(rtt_ms);
                    }
                    if strategy.mode() == crate::cursor::CursorMode::Predictive {
                        Some(strategy.render_position())
                    } else {
                        None
                    }
                };

                let Some((x, y)) = predicted else {
                    continue;
                };
                if x < 0 || y < 0 || x > i32::from(u16::MAX) || y > i32::from(u16::MAX) {
                    continue;
                }
                let position = ironrdp_pdu::pointer::PointerPositionAttribute {
                    x: x as u16,
                    y: y as u16,
                };
                let sender = self.get_update_sender();
                let sender = sender.lock().await;
                if let Err(e) = sender.send(DisplayUpdate::PointerPosition(position)).await {
                    debug!("Failed to send predicted cursor position update: {e}");
                }
            }
        });
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
        Arc::clone(&self).start_cursor_prediction_loop();

        let handler = Arc::clone(&self);

        tokio::spawn(async move {
            info!("🎬 Starting display update pipeline task");

            // Detect if the compositor's display GPU is virgl (virtio-gpu with
            // GL acceleration). virgl produces 180-rotated MemFd buffers via KDE's
            // portal because KWin's grabTexture() mishandles the GL Y-axis convention.
            //
            // Two detection paths:
            // 1. Capability system's GpuInfo (from glxinfo — may report llvmpipe
            //    if the server process forces software rendering)
            // 2. sysfs DRM driver check (env-independent, checks the actual
            //    display GPU by finding cards with connected outputs)
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

            // === ENCODING ADAPTATION (Closed-Loop QP Control) ===
            // Uses EGFX client feedback (queue_depth, QoE) to adjust encoding quality.
            // Disabled by default — opt-in via config [egfx.encoding_adaptation] enabled=true
            let adaptation_config = self.config.egfx.encoding_adaptation.clone();
            let mut encoding_adaptation =
                self.egfx_snapshot.read().await.as_ref().map(|snapshot| {
                    crate::performance::EncodingAdaptation::new(
                        adaptation_config.clone(),
                        Arc::clone(snapshot),
                    )
                });
            if let Some(ref adapt) = encoding_adaptation
                && adapt.is_enabled()
            {
                info!(
                    "🎛️ Encoding adaptation enabled: base_qp={}, interval={}ms",
                    adaptation_config.base_qp, adaptation_config.evaluation_interval_ms
                );
            }

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
            // frames_dropped counts ONLY genuine backpressure drops — frames the
            // L4 flow controller throttled because the client can't keep up.
            // Frames merely skipped by frame-rate pacing go to frames_paced and
            // must NOT feed the L2 drop-rate: capture rate > send rate is normal
            // under high activity, and counting it as "drops" false-fires the L2
            // STRESS-IDR (wasteful keyframes) when nothing is actually wrong.
            let mut frames_dropped = 0u64;
            let mut frames_paced = 0u64;
            let mut egfx_frames_sent = 0u64;

            let mut loop_iterations = 0u64;

            // L2 stress detector: track frames_dropped + frames_sent over a
            // rolling 1-second window. When drop_rate sustains > 50% AND the
            // encoder hasn't emitted an IDR in the last 1500ms, we request an
            // early IDR. This breaks the P-slice chain before mstsc's decoder
            // can desync from a long sequence of arrival-delayed predictions
            // (which is what triggers the mid-session CapsAdvertise recovery
            // sequence — see L1).
            //
            // Snapshot fields: window_start_at_ms is a monotonic-ish millis
            // counter (Instant::now() differences); the dropped/sent_at_start
            // are the counter values at window open.
            let stress_window_ms: u64 = 1000;
            let stress_drop_rate_threshold: f64 = 0.50;
            let stress_min_idr_gap_ms: u64 = 1500;
            let stress_cooldown_ms: u64 = 1000;
            let stress_started_at = std::time::Instant::now();
            let mut stress_window_start = stress_started_at;
            let mut stress_window_dropped_at_start = 0u64;
            let mut stress_window_sent_at_start = 0u64;
            let mut stress_last_trigger = std::time::Instant::now()
                .checked_sub(std::time::Duration::from_mins(1))
                .unwrap_or_else(std::time::Instant::now);

            // L3 FIX: aux-omission shrinks under stress.
            // The configured avc444_max_aux_interval (default 30 frames) is
            // the worst-case staleness for chroma during steady state. Under
            // stress we shrink it so aux refreshes faster, reducing the
            // window where main-channel decoder errors can persist.
            //
            // We track the original config value at encoder-create time and
            // restore it after stress_recovery_ms with no further triggers.
            let stress_recovery_ms: u64 = 5000;
            let mut stress_active = false;
            let mut stress_original_aux_interval: u32 = 0; // set when stress first activates

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

            // Last-frame cache: holds the most recent PipeWire frame for replay on
            // EGFX initialization. Portal ScreenCast is damage-driven — PipeWire only
            // delivers frames when screen content changes. On a static desktop, the
            // initial burst of frames arrives before any RDP client connects (drained
            // at the client_active gate). By the time EGFX negotiation completes, there
            // are no new frames to encode and the client sees nothing.
            //
            // This cache ensures every client gets at least one H.264 frame (the current
            // desktop state) immediately after EGFX becomes ready, regardless of whether
            // PipeWire has pending frames.
            //
            // Cost: one Arc<Vec<u8>> reference (~8MB at 1080p BGRA). VideoFrame.data is
            // Arc-wrapped, so clone is a refcount bump — no pixel data is copied.
            //
            // FUTURE: When SessionStrategy gains a request_current_frame() method (planned
            // for the QEMU D-Bus strategy), per-strategy frame requests can provide fresher
            // frames than this cache for strategies that support it (e.g., QEMU screendump,
            // wlr-screencopy with DRIVER mode). The cache becomes the universal fallback.
            // See: shared/strategy/FRAME-DELIVERY-DECISION.md
            let mut cached_frame: Option<crate::pipewire::VideoFrame> = None;

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
                debug!(
                    "Damage tracking ENABLED: tile_size={}, threshold={:.2}, pixel_threshold={}, merge_distance={}, min_region_area={}",
                    damage_config.tile_size,
                    damage_config.diff_threshold,
                    damage_config.pixel_threshold,
                    damage_config.merge_distance,
                    damage_config.min_region_area
                );
                Some(DamageDetector::new(damage_config))
            } else {
                debug!(
                    "🎯 Damage tracking DISABLED via config -- compositor damage hints (if any) \
                     will be trusted unconditionally, with no calibration probe and no fallback \
                     if they misbehave"
                );
                None
            };

            let mut frames_skipped_damage = 0u64; // Frames skipped due to no damage

            // Compositor-hint trust state (see the damage-source-selection
            // block below). Sticky for this connection's lifetime — reset
            // only at the two true new-connection points, never on resize.
            // A future capture strategy is covered automatically as long as
            // it keeps feeding frames through this same shared pipeline
            // (lamco_pipewire::frame::RawFrameData) rather than a parallel
            // encode/damage path.
            let mut compositor_hint_distrusted = false;
            let mut consecutive_high_divergence = 0u32;

            // === FRAME STALL DETECTION ===
            // Track when we last received a frame from PipeWire. If the stream
            // is active but no frames arrive for 3+ seconds, report degradation
            // to the health monitor. Recovery is reported when frames resume.
            let mut last_frame_time = std::time::Instant::now();
            let mut video_stall_reported = false;
            // Damage-driven capture sends no frames while the desktop is static,
            // so a short window false-positives on normal idle (reading, etc.).
            // Use a longer window so only a genuinely stuck stream is flagged.
            let stall_threshold = std::time::Duration::from_secs(10);

            // Corrupted-buffer storm detection. A compositor stuck in direct
            // scanout keeps cycling its buffer pool while marking every chunk
            // SPA_CHUNK_FLAG_CORRUPTED, so the picture freezes for seconds at a
            // time without ever crossing the 10s stall threshold. Counting the
            // flagged frames in a short window separates that from an idle
            // desktop, which produces no buffers at all.
            let corrupted_window = std::time::Duration::from_secs(2);
            let corrupted_storm_threshold: u32 = 30;
            let mut corrupted_window_start = std::time::Instant::now();
            let mut corrupted_window_count: u32 = 0;
            let mut corrupted_storm_reported = false;

            // Most corrupted buffers carry size 0 and are dropped before a
            // VideoFrame exists, so the counter above never sees them. The
            // capture crate counts them at the source; sampling that is the
            // only way to tell this apart from an idle desktop.
            let mut corrupted_total_seen: u64 = 0;
            let mut corrupted_since_report: u64 = 0;
            // Ten in a two-second window: far above the interleave seen on healthy
            // sessions, far below the rate of a real stall (about 75 a second).
            let scanout_corrupted_floor: u64 = 10;
            let mut last_scanout_report: Option<std::time::Instant> = None;
            let scanout_report_interval = std::time::Duration::from_secs(2);
            let scanout_stall_threshold = std::time::Duration::from_millis(1500);

            // Pace the "Processing frame" INFO log so low-throughput sessions
            // are not silent: log on every 30th frame OR when >2s elapsed since
            // last log, whichever comes first. Under 1fps effective delivery
            // the time-based branch dominates.
            let mut last_processing_log = std::time::Instant::now();
            let processing_log_interval = std::time::Duration::from_secs(2);

            // Damage-source / adaptive-fps telemetry cadence. Frame-count-based
            // gating (frames_sent.is_multiple_of(N)) is unsafe here: frames_sent
            // increments before the latency governor's Skip/WaitForMore continues
            // and the empty-damage continue, so a stable throttle pattern can put
            // every Nth-count frame on a skipped iteration -- deterministically,
            // if the skip period shares a factor with N. Observed in practice:
            // this fired ZERO times across an entire 12s sustained-high-activity
            // session with frames_sent crossing 60/120/180. Time-based gating
            // fires on whatever frame is actually being processed when the
            // interval elapses, independent of skip patterns.
            let mut last_telemetry_log = std::time::Instant::now();
            let telemetry_log_interval = std::time::Duration::from_secs(2);

            // Zero-frame detection: if we never receive ANY frame within 10 seconds
            // of session start, something is fundamentally wrong (e.g., ext-capture
            // handshake completed but compositor never delivers frames).
            let mut session_start = std::time::Instant::now();
            let mut first_frame_received = false;
            let mut zero_frame_reported = false;

            // EGFX readiness timeout: if EGFX hasn't become ready within 5 seconds
            // of the first PipeWire frame, assume the client doesn't support DVC or
            // EGFX negotiation failed. Bypass the EGFX gate and deliver frames via
            // FastPath bitmap only. Without this, clients without DVC get zero frames.
            let egfx_timeout = std::time::Duration::from_secs(5);
            let mut egfx_gate_bypassed = false;
            let mut was_client_active = false;
            // Set after PipeWire CreateStream during resize — cleared when the
            // first frame from the new stream arrives and we finalize the resize
            // using the actual negotiated resolution
            let mut pending_resize = false;
            let zero_frame_threshold = std::time::Duration::from_secs(10);

            // === PTS INTERVAL TRACKING ===
            // Track PipeWire presentation timestamps to measure actual frame
            // delivery cadence. Reported in the heartbeat log.
            let mut last_pts_nsec: u64 = 0;
            let mut pts_interval_sum_ms: f64 = 0.0;
            let mut pts_interval_count: u64 = 0;
            let mut pts_interval_min_ms: f64 = f64::MAX;
            let mut pts_interval_max_ms: f64 = 0.0;

            // Take the resize receiver for this pipeline instance
            let resize_rx = handler
                .resize_rx
                .lock()
                .ok()
                .and_then(|mut guard| guard.take());

            if resize_rx.is_some() {
                info!("Pipeline acquired resize receiver for client-initiated resolution changes");
            }

            // Per-iteration timing diagnostics. The heartbeat (every 1000
            // iterations) tells us throughput, but not WHERE time goes inside
            // each iteration. When the client connects and the loop slows to
            // <30 iter/sec from 200/sec idle, we need per-iteration tail
            // latency to know if it's the no-frame async-lock path, the
            // damage-bailout path, or the encode+send path. WARN above 50ms
            // because that's already over the 33ms 30fps budget.
            let mut last_iter_start = std::time::Instant::now();
            loop {
                let prev_iter_elapsed = last_iter_start.elapsed();
                if prev_iter_elapsed >= std::time::Duration::from_millis(50) && loop_iterations > 0
                {
                    warn!(
                        prev_iter_ms = prev_iter_elapsed.as_millis() as u64,
                        loop_iterations, "Display pipeline iteration slow"
                    );
                }
                last_iter_start = std::time::Instant::now();

                loop_iterations += 1;
                if loop_iterations.is_multiple_of(1000) {
                    if pts_interval_count > 0 {
                        let avg_ms = pts_interval_sum_ms / pts_interval_count as f64;
                        debug!(
                            "Display pipeline heartbeat: {} iterations, sent {} (egfx: {}), dropped {}, skipped_damage {}, pts_interval {:.1}/{:.1}/{:.1}ms (min/avg/max, n={})",
                            loop_iterations,
                            frames_sent,
                            egfx_frames_sent,
                            frames_dropped,
                            frames_skipped_damage,
                            pts_interval_min_ms,
                            avg_ms,
                            pts_interval_max_ms,
                            pts_interval_count,
                        );
                        // Reset for next window
                        pts_interval_sum_ms = 0.0;
                        pts_interval_count = 0;
                        pts_interval_min_ms = f64::MAX;
                        pts_interval_max_ms = 0.0;
                    } else {
                        debug!(
                            "Display pipeline heartbeat: {} iterations, sent {} (egfx: {}), dropped {}, skipped_damage {}",
                            loop_iterations,
                            frames_sent,
                            egfx_frames_sent,
                            frames_dropped,
                            frames_skipped_damage
                        );
                    }
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

                        // On wlroots-family compositors, drive a real output mode
                        // change so the source output physically matches the
                        // client's requested resolution rather than only
                        // re-capturing a scaled region. Best-effort: a skip is
                        // non-fatal (the capture-side resize below still runs).
                        #[cfg(feature = "wayland")]
                        if self.config.display.resize_drives_output_mode
                            && let Some(out) = self.wayland_observers.output.get()
                            && let Some((head, _)) =
                                out.heads().into_iter().find(|(_, h)| h.enabled)
                        {
                            match out.request_custom_mode(
                                &head,
                                i32::from(req.width),
                                i32::from(req.height),
                                0,
                            ) {
                                Ok(()) => info!(
                                    "Drove wlroots output '{}' to {}x{}",
                                    head, req.width, req.height
                                ),
                                Err(e) => debug!("Output-mode change skipped: {e:#}"),
                            }
                        }

                        if handler.direct_channel_mode {
                            // Direct frame channel (portal-generic): the capture
                            // resolution follows the compositor output. The only
                            // real resize is the wlroots output-mode change
                            // attempted above; there is no PipeWire stream to
                            // recreate here.
                            info!(
                                "Resize to {}x{} in direct channel mode: relying on the \
                                 compositor output mode (no PipeWire stream to recreate)",
                                req.width, req.height
                            );
                            continue;
                        }

                        // 1. Destroy existing PipeWire stream. Use the live
                        // capture node — a session re-establishment may have
                        // rebound it away from the startup node in stream_info.
                        if !handler.stream_info.is_empty() {
                            let node_id = handler
                                .capture_node
                                .load(std::sync::atomic::Ordering::Relaxed);
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
                                let use_dmabuf_for_resize = self.use_dmabuf;
                                let stream_config = lamco_pipewire::StreamConfig {
                                    name: "monitor-0".to_string(),
                                    width: req.width as u32,
                                    height: req.height as u32,
                                    framerate: 60,
                                    use_dmabuf: use_dmabuf_for_resize,
                                    buffer_count: 3,
                                    preferred_format: Some(lamco_pipewire::PixelFormat::BGRx),
                                    dmabuf_passthrough: false,
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
                                    // Defer display update until the first frame arrives
                                    // from the new stream. The compositor controls the
                                    // actual output resolution — it may differ from what
                                    // we requested. We use the frame's negotiated
                                    // width/height to tell the RDP client the truth.
                                    pending_resize = true;

                                    // Reset pipeline encoder state so the first frame
                                    // from the new stream triggers full re-init
                                    video_encoder = None;
                                    egfx_sender = None;
                                    force_first_frame = false;

                                    if let Some(ref mut detector) = damage_detector_opt {
                                        detector.invalidate();
                                    }

                                    info!(
                                        "PipeWire stream recreated - deferring display update \
                                         until first frame confirms actual resolution"
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

                    // Tags a producer publishes about the stream. Mutter puts the
                    // logical monitor's scale here on a virtual-monitor stream,
                    // which is the only place a scale is observable from the
                    // capture side at all. Read every pass because the producer
                    // republishes on change and this is live state, not setup.
                    {
                        let tags = thread_mgr.stream_tags();
                        let scale = tags.get_f64("org.gnome.scale");
                        let mut current = handler.compositor_scale.write();
                        if *current != scale {
                            match scale {
                                Some(s) => info!("Compositor reports output scale {s}"),
                                None if current.is_some() => {
                                    info!("Compositor stopped reporting an output scale");
                                }
                                None => {}
                            }
                            *current = scale;
                        }
                    }

                    // Forward PipeWire stream state changes to health monitor and sensor
                    let pw_sensor = handler.pipewire_sensor.read().await.clone();
                    if let Some(ref reporter) = *handler.health_reporter.read().await {
                        for event in thread_mgr.drain_state_events() {
                            let health_state = match event.state {
                                lamco_pipewire::PwStreamState::Streaming => {
                                    // Notify Portal session that stream is active (input OK)
                                    if let Some(ref flag) = *handler.stream_active_flag.read() {
                                        flag.store(true, std::sync::atomic::Ordering::Release);
                                    }
                                    if let Some(ref sensor) = pw_sensor {
                                        sensor.set_stream_state(2);
                                    }
                                    crate::health::VideoStreamState::Streaming
                                }
                                lamco_pipewire::PwStreamState::Paused => {
                                    // Notify Portal session that stream is paused (input will fail)
                                    if let Some(ref flag) = *handler.stream_active_flag.read() {
                                        flag.store(false, std::sync::atomic::Ordering::Release);
                                    }
                                    if let Some(ref sensor) = pw_sensor {
                                        sensor.set_stream_state(1);
                                    }
                                    crate::health::VideoStreamState::Paused
                                }
                                lamco_pipewire::PwStreamState::Error(ref msg) => {
                                    warn!("PipeWire stream error: {}", msg);
                                    if let Some(ref sensor) = pw_sensor {
                                        sensor.set_stream_state(0);
                                    }
                                    crate::health::VideoStreamState::Error
                                }
                                lamco_pipewire::PwStreamState::Unconnected => {
                                    warn!(
                                        "PipeWire stream disconnected - screen capture unavailable"
                                    );
                                    if std::env::var("WAYLAND_DISPLAY").is_err() {
                                        warn!(
                                            "WAYLAND_DISPLAY is not set - this is likely the cause"
                                        );
                                    }
                                    if let Some(ref sensor) = pw_sensor {
                                        sensor.set_stream_state(0);
                                    }
                                    continue;
                                }
                                // Connecting is transient -- not a health event
                                lamco_pipewire::PwStreamState::Connecting => continue,
                            };
                            reporter.report(crate::health::HealthEvent::VideoStreamStateChanged {
                                state: health_state,
                            });
                        }
                    }

                    // A frame the producer flagged SPA_CHUNK_FLAG_CORRUPTED carries
                    // untrustworthy pixels and untrustworthy per-frame metadata
                    // (damage regions in particular are stale slot contents rather
                    // than a fresh claim), so encoding it puts garbage on the wire.
                    // Drop it here and let the None path handle the resulting gap.
                    let frame = match thread_mgr.try_recv_frame() {
                        Some(f) if f.flags.is_corrupted() => {
                            if corrupted_window_start.elapsed() > corrupted_window {
                                corrupted_window_start = std::time::Instant::now();
                                corrupted_window_count = 0;
                                corrupted_storm_reported = false;
                            }
                            corrupted_window_count += 1;
                            if !corrupted_storm_reported
                                && corrupted_window_count >= corrupted_storm_threshold
                            {
                                corrupted_storm_reported = true;
                                if let Some(ref reporter) = *handler.health_reporter.read().await {
                                    reporter.report(
                                        crate::health::HealthEvent::VideoFramesCorrupted {
                                            count: corrupted_window_count,
                                            window_ms: corrupted_window_start.elapsed().as_millis()
                                                as u64,
                                        },
                                    );
                                }
                            }
                            None
                        }
                        other => other,
                    };
                    if frame.is_some()
                        && let Some(ref sensor) = pw_sensor
                    {
                        sensor.increment_frames();
                    }
                    let corrupted_at_source = thread_mgr.corrupted_buffer_count();
                    (frame, corrupted_at_source)
                };
                let (frame, corrupted_at_source) = frame;

                let mut frame = match frame {
                    Some(f) => {
                        // Always cache the latest frame for replay on EGFX init.
                        // Clone is cheap: VideoFrame.data is Arc<Vec<u8>>.
                        cached_frame = Some(f.clone());
                        last_frame_time = std::time::Instant::now();

                        // Track PTS intervals for heartbeat diagnostics
                        if f.pts > 0 && last_pts_nsec > 0 && f.pts > last_pts_nsec {
                            let interval_ms = (f.pts - last_pts_nsec) as f64 / 1_000_000.0;
                            pts_interval_sum_ms += interval_ms;
                            pts_interval_count += 1;
                            if interval_ms < pts_interval_min_ms {
                                pts_interval_min_ms = interval_ms;
                            }
                            if interval_ms > pts_interval_max_ms {
                                pts_interval_max_ms = interval_ms;
                            }
                        }
                        if f.pts > 0 {
                            last_pts_nsec = f.pts;
                        }

                        // Mark that we've received at least one frame
                        first_frame_received = true;

                        // Finalize deferred resize using the frame's actual
                        // dimensions (set by PipeWire param_changed negotiation)
                        if pending_resize {
                            pending_resize = false;
                            let actual_w = f.width as u16;
                            let actual_h = f.height as u16;

                            {
                                let mut converter = handler.bitmap_converter.lock().await;
                                *converter = BitmapConverter::new(actual_w, actual_h);
                            }
                            handler
                                .egfx_needs_init
                                .store(true, std::sync::atomic::Ordering::SeqCst);
                            handler.update_size(actual_w, actual_h).await;

                            info!(
                                "Resize finalized from first frame: {}x{} (compositor negotiated)",
                                actual_w, actual_h
                            );
                        }

                        // Report recovery if we previously flagged a stall
                        if video_stall_reported {
                            video_stall_reported = false;
                            if let Some(ref reporter) = *handler.health_reporter.read().await {
                                reporter.report(crate::health::HealthEvent::VideoFrameResumed);
                            }
                        }

                        // Drain PipeWire frames even when no client is connected,
                        // but skip all encoding and sending to avoid wasted work
                        let client_now_active = handler
                            .client_active
                            .load(std::sync::atomic::Ordering::Relaxed);
                        if !client_now_active {
                            was_client_active = false;
                            tokio::time::sleep(tokio::time::Duration::from_millis(1)).await;
                            continue;
                        }

                        // Reset per-connection state on reconnection.
                        // The EGFX gate timeout must count from connection start,
                        // not server start — otherwise after the first 5s of uptime,
                        // every subsequent client bypasses the gate immediately and
                        // gets FastPath bitmaps instead of EGFX.
                        if !was_client_active {
                            was_client_active = true;
                            session_start = std::time::Instant::now();
                            egfx_gate_bypassed = false;
                            first_frame_received = false;
                            zero_frame_reported = false;
                            frames_sent = 0;
                            frames_dropped = 0;
                            frames_paced = 0;
                            egfx_frames_sent = 0;
                            video_encoder = None;
                            egfx_sender = None;
                            compositor_hint_distrusted = false;
                            consecutive_high_divergence = 0;
                            // New client needs fresh EGFX surface setup
                            handler
                                .egfx_needs_init
                                .store(true, std::sync::atomic::Ordering::SeqCst);
                            info!("Pipeline state reset for new client connection");
                        }
                        debug!("Received frame from PipeWire");
                        f
                    }
                    None => {
                        // Stall detection: if we previously received frames (cached_frame
                        // exists) and haven't gotten one for 3+ seconds, the stream may be
                        // stuck. Static desktops normally produce no frames (damage-driven),
                        // so we only flag this after we've seen at least one frame.
                        // Damage-driven capture legitimately delivers no frames
                        // while the desktop is idle; PipeWire pauses the stream in
                        // that case and that pause is already surfaced to health.
                        // Only treat missing frames as a stall while the stream is
                        // still meant to be streaming — otherwise a static desktop
                        // flaps the session between healthy and degraded every time
                        // the user stops interacting.
                        // The compositor is producing buffers and marking every
                        // one unusable: that is not an idle desktop, it is
                        // GNOME/mutter#3903 with a fullscreen or maximised
                        // surface in direct scanout.
                        //
                        // Accumulate rather than reporting one iteration's delta.
                        // This loop turns over in about a millisecond, so the
                        // delta is almost always 1 while hundreds arrive in the
                        // reporting window: the first version of this line said
                        // "1 corrupted buffers" through an episode that logged 800.
                        corrupted_since_report +=
                            corrupted_at_source.saturating_sub(corrupted_total_seen);
                        corrupted_total_seen = corrupted_at_source;
                        {
                            let client_here = handler
                                .client_active
                                .load(std::sync::atomic::Ordering::Relaxed);
                            let starved = last_frame_time.elapsed() > scanout_stall_threshold;
                            let due = last_scanout_report
                                .is_none_or(|t| t.elapsed() > scanout_report_interval);
                            // A rate, not a single buffer. Corrupted chunks also
                            // interleave at a low rate on healthy virtio/Venus
                            // sessions, and an idle desktop legitimately sends no
                            // frames for seconds, so "one stray chunk plus a quiet
                            // moment" must not be reported as a compositor fault.
                            let storming = corrupted_since_report >= scanout_corrupted_floor;
                            if client_here && starved && due && storming {
                                let stalled_ms = last_frame_time.elapsed().as_millis() as u64;
                                if let Some(ref reporter) = *handler.health_reporter.read().await {
                                    reporter.report(
                                        crate::health::HealthEvent::VideoFramesCorrupted {
                                            count: corrupted_since_report.min(u64::from(u32::MAX))
                                                as u32,
                                            window_ms: stalled_ms,
                                        },
                                    );
                                }
                                warn!(
                                    "Compositor delivered {corrupted_since_report} corrupted buffers and no usable frame for {stalled_ms}ms: it has handed this output to direct scanout and stopped recording for us (GNOME/mutter#3903). The picture is frozen until the surface leaves fullscreen or the compositor carries the fix from GNOME/mutter!5276."
                                );
                                last_scanout_report = Some(std::time::Instant::now());
                                corrupted_since_report = 0;
                            } else if due {
                                // Window closed without a storm: start the next one
                                // clean so counts cannot accumulate across minutes
                                // of healthy operation into a false report.
                                last_scanout_report = Some(std::time::Instant::now());
                                corrupted_since_report = 0;
                            }
                        }

                        let stream_streaming = handler
                            .stream_active_flag
                            .read()
                            .as_ref()
                            .is_none_or(|f| f.load(std::sync::atomic::Ordering::Acquire));
                        if stream_streaming && cached_frame.is_some() && !video_stall_reported {
                            let elapsed = last_frame_time.elapsed();
                            if elapsed > stall_threshold {
                                video_stall_reported = true;
                                if let Some(ref reporter) = *handler.health_reporter.read().await {
                                    reporter.report(
                                        crate::health::HealthEvent::VideoFrameStalled {
                                            stall_duration_ms: elapsed.as_millis() as u64,
                                        },
                                    );
                                }
                            }
                        }

                        // Zero-frame detection: if no frame has EVER arrived since session
                        // start, the capture protocol may be non-functional (e.g., ext-capture
                        // on a compositor with incomplete implementation).
                        //
                        // Only while a client is actually connected. The window opens when
                        // one connects, but nothing used to close it when that client left,
                        // so a client that disconnects before the first frame arrives (a
                        // scripted capture can be gone in 60ms) left the window running
                        // against a connection that no longer existed. Ten seconds later the
                        // server reported that capture had never delivered frames and drove
                        // session health to failed, with nobody watching and nothing wrong.
                        // An idle server receiving no frames is damage-driven capture working
                        // as designed, not a fault.
                        let client_present = handler
                            .client_active
                            .load(std::sync::atomic::Ordering::Relaxed);
                        if client_present && !first_frame_received && !zero_frame_reported {
                            let since_start = session_start.elapsed();
                            if since_start > zero_frame_threshold {
                                zero_frame_reported = true;
                                tracing::warn!(
                                    elapsed_ms = since_start.as_millis() as u64,
                                    "No video frames received since session start"
                                );
                                if let Some(ref reporter) = *handler.health_reporter.read().await {
                                    reporter.report(
                                        crate::health::HealthEvent::VideoFrameNeverStarted {
                                            elapsed_ms: since_start.as_millis() as u64,
                                        },
                                    );
                                }
                            }
                        }

                        // DMA-BUF zero-data is fixed at the source in lamco-pipewire
                        // ≥0.4.4 (MOD_LINEAR negotiation, lamco-admin/lamco-wayland#5),
                        // so the old detect-zeros-and-reconnect-with-MemFd fallback is gone.
                        // The virtual-GPU MemFd gate in server/mod.rs remains as defense.

                        // No fresh frame from PipeWire. Check if we should replay
                        // the cached frame for EGFX initialization.
                        //
                        // Portal ScreenCast is damage-driven: on a static desktop,
                        // try_recv_frame() returns None indefinitely. Without this
                        // replay, EGFX-ready clients never receive their first H.264
                        // frame and show a black screen until something moves.
                        let client_waiting = handler
                            .client_active
                            .load(std::sync::atomic::Ordering::Relaxed);

                        // Also reset per-connection state from the None arm,
                        // in case PipeWire hasn't delivered a frame yet
                        if !client_waiting {
                            // Client disconnected while in no-frame path
                            was_client_active = false;
                        } else if client_waiting && !was_client_active {
                            was_client_active = true;
                            session_start = std::time::Instant::now();
                            egfx_gate_bypassed = false;
                            first_frame_received = false;
                            zero_frame_reported = false;
                            frames_sent = 0;
                            frames_dropped = 0;
                            frames_paced = 0;
                            egfx_frames_sent = 0;
                            video_encoder = None;
                            egfx_sender = None;
                            compositor_hint_distrusted = false;
                            consecutive_high_divergence = 0;
                            handler
                                .egfx_needs_init
                                .store(true, std::sync::atomic::Ordering::SeqCst);
                            info!("Pipeline state reset for new client connection (no-frame path)");
                        }

                        let needs_init = handler
                            .egfx_needs_init
                            .load(std::sync::atomic::Ordering::Relaxed);

                        // Log EGFX readiness check periodically during reconnection wait
                        if client_waiting && needs_init && !handler.is_egfx_ready().await {
                            static EGFX_WAIT_COUNTER: std::sync::atomic::AtomicU64 =
                                std::sync::atomic::AtomicU64::new(0);
                            let count = EGFX_WAIT_COUNTER
                                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                            if count.is_multiple_of(200) {
                                // Log every ~1 second (200 * 5ms)
                                let has_tx = handler.server_event_tx.read().await.is_some();
                                let has_handle = handler.gfx_server_handle.read().await.is_some();
                                let state_ready =
                                    handler.gfx_handler_state.as_ref().is_some_and(|s| {
                                        s.is_ready.load(std::sync::atomic::Ordering::Acquire)
                                    });
                                debug!(
                                    "⏳ EGFX not ready (wait #{count}): tx={has_tx}, handle={has_handle}, state_ready={state_ready}"
                                );
                            }
                        }

                        // Replay the cached frame once the EGFX channel is ready.
                        // Codec choice (AVC vs. the V8/uncompressed fallback) is
                        // decided in the same negotiation call that sets is_ready,
                        // so by the time is_egfx_ready() is true the codec is
                        // already settled — this is not a race to wait out.
                        //
                        // A prior version of this gate also required
                        // is_avc_supported(), reasoning that would avoid redundant
                        // replays during codec negotiation. It instead permanently
                        // starved every non-AVC (V8/RemoteFX) client of the initial
                        // frame: those clients never set is_avc_supported(), so on a
                        // desktop idle enough that PipeWire never emits a live frame
                        // within the 10s zero-frame window (damage-driven capture
                        // legitimately produces nothing while nothing changes on
                        // screen), the session was declared dead before anything
                        // downstream ever ran. This was issue #57's real mechanism:
                        // not a stream-teardown race, but this gate silently
                        // excluding non-AVC clients from ever reaching the frame
                        // that would have satisfied the health check.
                        //
                        // needs_init is cleared as soon as this replayed frame
                        // reaches either setup path downstream (the AVC branch or
                        // the V8/uncompressed branch), so a single replay per
                        // connection is enough; this does not reopen the redundant-
                        // replay concern the AVC gate was trying to address.
                        let egfx_ready = handler.is_egfx_ready().await;
                        // A client that never opens the EGFX channel at all (no
                        // DVC, no H.264 decode — plain screenshot/automation
                        // clients) never flips is_egfx_ready, and on an idle
                        // desktop the damage-driven capture gives the FastPath
                        // bitmap path no frame either. Such a client renders
                        // nothing but its own blank framebuffer until the first
                        // real damage. Once the EGFX gate deadline has passed
                        // without readiness, replay the cached frame for it too.
                        // Marking first_frame_received lets the gate's bypass
                        // condition fire on this replayed frame, so it actually
                        // reaches the bitmap path; the V8/legacy path then clears
                        // egfx_needs_init, so this fires once per connection.
                        let legacy_gate_settled =
                            !egfx_ready && session_start.elapsed() > egfx_timeout;
                        if client_waiting && needs_init && (egfx_ready || legacy_gate_settled) {
                            if let Some(ref cached) = cached_frame {
                                if legacy_gate_settled {
                                    first_frame_received = true;
                                    info!(
                                        "📦 Replaying cached frame for legacy (non-EGFX) client init ({}x{}, frame {})",
                                        cached.width, cached.height, cached.frame_id
                                    );
                                } else {
                                    info!(
                                        "📦 Replaying cached frame for EGFX init ({}x{}, frame {})",
                                        cached.width, cached.height, cached.frame_id
                                    );
                                }
                                cached.clone()
                            } else {
                                // No cached frame yet (server just started, PipeWire
                                // hasn't delivered any frames). Wait for first frame.
                                tokio::time::sleep(tokio::time::Duration::from_millis(5)).await;
                                continue;
                            }
                        } else {
                            tokio::time::sleep(tokio::time::Duration::from_millis(5)).await;
                            continue;
                        }
                    }
                };

                let should_process = if adaptive_fps_enabled {
                    adaptive_fps.should_capture_frame()
                } else {
                    frame_regulator.should_send_frame()
                };

                if !should_process {
                    frames_paced += 1;
                    if frames_paced.is_multiple_of(30) {
                        let current_fps = if adaptive_fps_enabled {
                            adaptive_fps.current_fps()
                        } else {
                            30
                        };
                        info!(
                            "Frame rate regulation: paced {} frames, sent {}, target_fps={}",
                            frames_paced, frames_sent, current_fps
                        );
                    }
                    tokio::time::sleep(tokio::time::Duration::from_millis(1)).await;
                    continue;
                }

                // === BUFFER TRANSFORM ===
                // Apply orientation correction to the raw pixel data BEFORE
                // it enters either the EGFX or bitmap/RemoteFX path. Both paths
                // consume the same frame data, so the transform must happen here.
                let mut transform_value =
                    resolve_transform(&self.config.display.frame_transform, frame.transform());
                if transform_value == 0 && frame.is_bottom_up() {
                    transform_value = 6; // vertical flip for negative stride
                }
                if transform_value != 0 {
                    let pixel_data = match &frame.buffer {
                        lamco_pipewire::FrameBuffer::Memory(data) => data,
                        lamco_pipewire::FrameBuffer::DmaBuf(_) => {
                            tracing::warn!("Cannot transform DMA-BUF frame on CPU");
                            continue;
                        }
                    };
                    let (transformed, new_w, new_h, new_stride) = apply_frame_transform(
                        pixel_data,
                        frame.width,
                        frame.height,
                        frame.stride,
                        transform_value,
                        4,
                    );
                    frame = VideoFrame {
                        buffer: lamco_pipewire::FrameBuffer::Memory(std::sync::Arc::new(
                            transformed,
                        )),
                        width: new_w,
                        height: new_h,
                        stride: new_stride,
                        ..frame
                    };
                }

                handler
                    .process_cursor_update(frame.meta.cursor.clone(), frame.monitor_index)
                    .await;

                frames_sent += 1;
                let elapsed_since_log = last_processing_log.elapsed();
                if frames_sent.is_multiple_of(30)
                    || frames_sent < 10
                    || elapsed_since_log >= processing_log_interval
                {
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
                    last_processing_log = std::time::Instant::now();
                }

                // === WAIT FOR EGFX ===
                // Suppress output until EGFX is ready OR timeout expires.
                // Sending bitmap before EGFX establishes can cause display conflicts
                // when ResetGraphics clears the client's framebuffer. However, if EGFX
                // never becomes ready (no DVC, channel failure, etc.), we must fall
                // through to FastPath bitmap — otherwise the client gets zero frames.
                if !egfx_gate_bypassed && !handler.is_egfx_ready().await {
                    let since_first_frame = session_start.elapsed();
                    if first_frame_received && since_first_frame > egfx_timeout {
                        egfx_gate_bypassed = true;
                        warn!(
                            "EGFX not ready after {:.1}s, bypassing gate for FastPath bitmap delivery",
                            since_first_frame.as_secs_f64()
                        );
                    } else {
                        frames_dropped += 1;
                        if frames_dropped.is_multiple_of(30) {
                            let reason = handler.egfx_wait_reason().await;
                            debug!("⏳ {} (dropped {} frames)", reason, frames_dropped);
                        }
                        tokio::time::sleep(tokio::time::Duration::from_millis(1)).await;
                        continue;
                    }
                }

                // === EGFX/H.264 PATH ===
                // Only enter H.264 path when client supports AVC codec AND EGFX is
                // actually ready (not bypassed due to timeout). V8 clients (no AVC)
                // and clients where EGFX timed out skip this block entirely and fall
                // through to the FastPath bitmap path.
                //
                // Load egfx_needs_init but DON'T clear it yet for AVC clients.
                // If encoder or surface creation fails, we need the flag to stay
                // true so the next frame retries initialization. The flag is only
                // cleared on successful setup (egfx_sender populated).
                //
                // Always load the real value, even when the gate is bypassed:
                // every consumer below that cares about `egfx_gate_bypassed` (the
                // AVC setup block, the V8/EGFX-uncompressed block) already gates
                // itself independently on `!egfx_gate_bypassed`, but the
                // non-EGFX-client clear below needs the true flag state to ever
                // fire for a bypassed (legacy, no-EGFX-channel) client. Hardcoding
                // this to `false` when bypassed used to mean that clear could
                // never run, so `egfx_needs_init` stayed stuck true forever and
                // the cached-frame replay above re-fired on every loop iteration
                // — an unbounded busy loop that starved the connection's own I/O.
                let mut needs_init = handler
                    .egfx_needs_init
                    .load(std::sync::atomic::Ordering::SeqCst);

                // L2 FIX: stress-triggered early IDR.
                // When the rolling-window drop rate exceeds threshold and the
                // encoder hasn't emitted an IDR recently, ask it for one. The
                // working theory is that under sustained drop pressure, late-
                // arriving P-slices accumulate decoder-side desync risk; an
                // unprompted IDR breaks the prediction chain and forces a
                // clean reference, reducing the probability that mstsc's
                // decoder gives up and triggers the L1 recovery sequence.
                //
                // Cooldown prevents flapping: after a stress-triggered IDR,
                // we wait stress_cooldown_ms before evaluating again.
                if video_encoder.is_some() {
                    let now = std::time::Instant::now();
                    let window_elapsed_ms =
                        now.duration_since(stress_window_start).as_millis() as u64;
                    if window_elapsed_ms >= stress_window_ms {
                        let dropped_in_window =
                            frames_dropped.saturating_sub(stress_window_dropped_at_start);
                        let sent_in_window =
                            frames_sent.saturating_sub(stress_window_sent_at_start);
                        let cooldown_elapsed_ms =
                            now.duration_since(stress_last_trigger).as_millis() as u64;
                        let ms_since_last_idr = video_encoder
                            .as_ref()
                            .map_or(u64::MAX, VideoEncoder::ms_since_last_idr);

                        let stress_eval = pipeline_decisions::evaluate_stress_idr_trigger(
                            dropped_in_window,
                            sent_in_window,
                            stress_drop_rate_threshold,
                            cooldown_elapsed_ms,
                            stress_cooldown_ms,
                            ms_since_last_idr,
                            stress_min_idr_gap_ms,
                        );
                        let drop_rate = stress_eval.drop_rate;
                        let should_trigger = stress_eval.should_trigger;

                        if should_trigger {
                            if let Some(ref mut enc) = video_encoder {
                                enc.request_idr();
                            }
                            warn!(
                                drop_rate = drop_rate,
                                dropped_in_window,
                                sent_in_window,
                                ms_since_last_idr,
                                cooldown_elapsed_ms,
                                "EGFX L2 STRESS-IDR: drop rate exceeded threshold — early IDR requested to break P-slice chain"
                            );
                            stress_last_trigger = now;

                            // L3: shrink aux-omission interval while stress is active.
                            // Only meaningful for AVC444 (AVC420 has no aux), and
                            // only the first trigger captures the original value to
                            // restore later.
                            if let Some(VideoEncoder::Avc444(ref mut enc444)) = video_encoder
                                && !stress_active
                            {
                                let prev = enc444.aux_max_interval();
                                stress_original_aux_interval = prev;
                                let shrunk = (prev / 2).max(5);
                                enc444.set_aux_max_interval(shrunk);
                                warn!(
                                    from = prev,
                                    to = shrunk,
                                    "EGFX L3 STRESS-AUX: shrinking aux-omission interval until stress clears"
                                );
                                stress_active = true;
                            }
                        }

                        // L3: restore aux-omission interval after recovery period.
                        let since_last_trigger_ms =
                            now.duration_since(stress_last_trigger).as_millis() as u64;
                        if stress_active && since_last_trigger_ms > stress_recovery_ms {
                            if let Some(VideoEncoder::Avc444(ref mut enc444)) = video_encoder
                                && stress_original_aux_interval > 0
                                && enc444.aux_max_interval() != stress_original_aux_interval
                            {
                                enc444.set_aux_max_interval(stress_original_aux_interval);
                                info!(
                                    restored_to = stress_original_aux_interval,
                                    since_last_trigger_ms,
                                    "EGFX L3: stress cleared, aux-omission interval restored to baseline"
                                );
                            }
                            stress_active = false;
                            stress_original_aux_interval = 0;
                        }

                        // Slide the window forward
                        stress_window_start = now;
                        stress_window_dropped_at_start = frames_dropped;
                        stress_window_sent_at_start = frames_sent;
                    }
                }

                // L1 FIX: mid-session CapsAdvertise re-init.
                // If LamcoGraphicsHandler::capabilities_advertise saw a second
                // CapsAdvertise from the client (decoder-recovery sequence),
                // we tear down the surface state via IronRDP's resize() and
                // then proceed through the normal needs_init path to recreate
                // the encoder + surface and emit a fresh IDR.
                let needs_full_reinit = if let Some(ref state) = handler.gfx_handler_state {
                    state
                        .needs_full_reinit
                        .swap(false, std::sync::atomic::Ordering::AcqRel)
                } else {
                    false
                };
                if needs_full_reinit {
                    // L1 RE-INIT — upstream ironrdp-egfx has now silently cleared
                    // server-side surface/frame state and re-armed reset_graphics_sent
                    // inside handle_capabilities_advertise (no DeleteSurface PDU emitted,
                    // because mstsc has already cleared its own state on its side).
                    // All we need to do downstream is force the standard needs_init
                    // path to run: recreate the H.264 encoder and call
                    // setup_egfx_surface() to issue CreateSurface + MapSurfaceToOutput.
                    // ResetGraphics will auto-emit from create_surface() since the
                    // upstream change cleared reset_graphics_sent.
                    //
                    // Previous attempt called server.resize(w, h) here. That emitted
                    // DeleteSurface(id=0) before ResetGraphics — but the client had
                    // already cleared surface 0 from its own state on CapsAdvertise,
                    // so it treated the stray DeleteSurface as a protocol violation
                    // and TCP RST'd within 27ms. Lesson: client-initiated re-init
                    // expects only ResetGraphics + CreateSurface + MapSurfaceToOutput,
                    // not the server-initiated DeleteSurface sequence used by resize().
                    warn!(
                        "EGFX L1 RE-INIT: client re-advertised caps mid-session — \
                         upstream cleared server state; signaling display loop to \
                         recreate encoder + surface and emit fresh IDR"
                    );
                    handler
                        .egfx_needs_init
                        .store(true, std::sync::atomic::Ordering::SeqCst);
                    needs_init = true;

                    // L1+L2 INTERACTION FIX: reset the L2 stress detector's
                    // rolling window to baseline. Without this, the
                    // accumulated frames_dropped from BEFORE the re-init
                    // continues to count, immediately re-triggering L2 on
                    // Frame #1 — yielding two IDRs back-to-back (the L1
                    // forced first-frame IDR and L2's stress-IDR). Observed
                    // in round 5 log: 17 stress IDRs fired in the 19.5s
                    // post-reinit window, contributing to mstsc decoder
                    // giving up.
                    let now = std::time::Instant::now();
                    stress_window_start = now;
                    stress_window_dropped_at_start = frames_dropped;
                    stress_window_sent_at_start = frames_sent;
                    stress_last_trigger = now;
                    if stress_active {
                        info!(
                            "EGFX L1 RE-INIT: also resetting L2 stress state \
                             (was active, would have over-triggered on Frame #1)"
                        );
                        stress_active = false;
                        stress_original_aux_interval = 0;
                    }

                    // Note: SharedHandlerState.has_surface / primary_surface_id are
                    // ALREADY reset by LamcoGraphicsHandler::capabilities_advertise
                    // when the re-advertise was detected — see egfx/handler.rs. No
                    // additional reset needed here.
                }

                let is_avc = !egfx_gate_bypassed && handler.is_avc_supported().await;
                let is_egfx = !egfx_gate_bypassed && handler.is_egfx_ready().await;
                if needs_init && !is_avc && !is_egfx {
                    // Non-EGFX client: clear flag, no EGFX setup needed
                    handler
                        .egfx_needs_init
                        .store(false, std::sync::atomic::Ordering::SeqCst);
                }

                if is_avc {
                    if needs_init {
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

                        // HDR awareness: when the source output is in an HDR mode
                        // its pixels are PQ/HLG-encoded and get squashed to 8-bit
                        // SDR by the RDP path. Surface it here; PQ->SDR
                        // tone-mapping is the encode-side follow-up.
                        #[cfg(feature = "wayland")]
                        if let Some(obs) = self.wayland_observers.color.get()
                            && obs.any_hdr()
                        {
                            warn!(
                                "⚠️  Source output is HDR (PQ/HLG); encoding to 8-bit SDR \
                                 without tone-mapping — colors will be off until tone-mapping lands"
                            );
                        }

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
                            if let Some(ref state) = handler.gfx_handler_state {
                                state
                                    .is_avc444_enabled
                                    .load(std::sync::atomic::Ordering::Acquire)
                            } else {
                                false
                            };

                        // Resolve codec preference from config
                        let codec_pref = self.config.egfx.codec.to_lowercase();
                        let (avc444_enabled, codec_reason) =
                            pipeline_decisions::resolve_avc444_enabled(
                                &codec_pref,
                                client_supports_avc444,
                                self.config.egfx.avc444_enabled,
                            );
                        info!("{codec_reason}");

                        // Build encoder diagnostics ONCE per encoder construction.
                        // Wrapped in Arc so both encoders (AVC444 / AVC420 fallback)
                        // can share a single dump-file and single self-test decoder.
                        // None when both config flags are off — zero per-frame cost.
                        let encoder_diagnostics: Option<
                            std::sync::Arc<crate::egfx::encode_diagnostics::EncodeDiagnostics>,
                        > = {
                            let cfg = &self.config.diagnostics;
                            let need = cfg.dump_h264_to.is_some() || cfg.decode_self_test;
                            if need {
                                match crate::egfx::encode_diagnostics::EncodeDiagnostics::new(
                                    cfg.dump_h264_to.as_deref(),
                                    cfg.decode_self_test,
                                ) {
                                    Ok(d) => {
                                        info!("🔬 Encoder diagnostics: {}", d.summary());
                                        Some(std::sync::Arc::new(d))
                                    }
                                    Err(e) => {
                                        warn!(
                                            "Encoder diagnostics requested but init failed: {e:#} — continuing without"
                                        );
                                        None
                                    }
                                }
                            } else {
                                None
                            }
                        };

                        if avc444_enabled {
                            // Try AVC444 first (premium 4:4:4 chroma), preferring
                            // the VA-API hardware encoder when it is enabled and
                            // available, falling back to software otherwise.
                            #[cfg(feature = "vaapi")]
                            let avc444_result = if self.config.hardware_encoding.enabled {
                                match Avc444Encoder::new_hardware(
                                    config.clone(),
                                    &self.config.hardware_encoding,
                                ) {
                                    Ok(enc) => {
                                        info!(
                                            "✅ AVC444 using VA-API hardware encoder for {}×{}",
                                            aligned_width, aligned_height
                                        );
                                        Ok(enc)
                                    }
                                    Err(e) => {
                                        warn!(
                                            "Hardware AVC444 unavailable ({e:?}); using software AVC444"
                                        );
                                        Avc444Encoder::new(config.clone())
                                    }
                                }
                            } else {
                                Avc444Encoder::new(config.clone())
                            };
                            #[cfg(not(feature = "vaapi"))]
                            let avc444_result = Avc444Encoder::new(config.clone());

                            match avc444_result {
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
                                    encoder.set_diagnostics(encoder_diagnostics.clone());

                                    video_encoder = Some(VideoEncoder::Avc444(encoder));
                                    info!(
                                        "✅ AVC444 encoder initialized for {}×{} (4:4:4 chroma)",
                                        aligned_width, aligned_height
                                    );
                                }
                                Err(e) => {
                                    warn!(
                                        "Failed to create AVC444 encoder: {:?} - falling back to AVC420",
                                        e
                                    );
                                    // Fall through to AVC420
                                    match Avc420Encoder::new(config) {
                                        Ok(mut encoder) => {
                                            encoder.set_diagnostics(encoder_diagnostics.clone());
                                            video_encoder = Some(VideoEncoder::Avc420(encoder));
                                            info!(
                                                "✅ AVC420 encoder initialized for {}×{} (4:2:0 fallback)",
                                                aligned_width, aligned_height
                                            );
                                        }
                                        Err(e) => {
                                            warn!(
                                                "Failed to create AVC420 encoder: {:?} - falling back to RemoteFX",
                                                e
                                            );
                                        }
                                    }
                                }
                            }
                        } else {
                            // Use AVC420 (standard 4:2:0 chroma), preferring the
                            // VA-API hardware encoder when enabled and available.
                            #[cfg(feature = "vaapi")]
                            let avc420_result = if self.config.hardware_encoding.enabled {
                                match Avc420Encoder::new_hardware(
                                    config.clone(),
                                    &self.config.hardware_encoding,
                                ) {
                                    Ok(enc) => {
                                        info!("✅ AVC420 using VA-API hardware encoder");
                                        Ok(enc)
                                    }
                                    Err(e) => {
                                        warn!(
                                            "Hardware AVC420 unavailable ({e:?}); using software AVC420"
                                        );
                                        Avc420Encoder::new(config)
                                    }
                                }
                            } else {
                                Avc420Encoder::new(config)
                            };
                            #[cfg(not(feature = "vaapi"))]
                            let avc420_result = Avc420Encoder::new(config);

                            match avc420_result {
                                Ok(mut encoder) => {
                                    encoder.set_diagnostics(encoder_diagnostics.clone());
                                    video_encoder = Some(VideoEncoder::Avc420(encoder));
                                    info!(
                                        "✅ AVC420 encoder initialized for {}×{} (aligned)",
                                        aligned_width, aligned_height
                                    );
                                }
                                Err(e) => {
                                    warn!(
                                        "Failed to create H.264 encoder: {:?} - falling back to RemoteFX",
                                        e
                                    );
                                }
                            }
                        }
                        drop(encoder_diagnostics);

                        // Create EGFX surface regardless of encoder availability.
                        // Once EGFX is negotiated, ALL frames go through WireToSurface1.
                        // With an encoder: AVC420/AVC444 codec. Without: Uncompressed.
                        // Per MS-RDPEGFX spec, clients in EGFX mode ignore FastPath bitmaps.
                        if video_encoder.is_none() {
                            info!("No H.264 encoder available, using EGFX uncompressed path");
                        }

                        // Publish the active encoder backend to the shared snapshot
                        // so D-Bus/GUI telemetry reports it instead of an empty
                        // string. Runs on encoder (re)creation, not per frame.
                        if let Some(ref enc) = video_encoder {
                            let backend = enc.backend_name();
                            if let Some(handle) = self.encoder_state.read().await.as_ref() {
                                let mut slot = handle.write();
                                match slot.as_mut() {
                                    Some(existing) => backend.clone_into(&mut existing.backend),
                                    None => {
                                        *slot = Some(crate::health::performance::EncoderSnapshot {
                                            backend: backend.to_owned(),
                                            ..Default::default()
                                        });
                                    }
                                }
                            }
                        }
                        if let Some(sender) = handler
                            .setup_egfx_surface(
                                frame.width,
                                frame.height,
                                aligned_width,
                                aligned_height,
                            )
                            .await
                        {
                            egfx_sender = Some(sender);
                            handler
                                .egfx_needs_init
                                .store(false, std::sync::atomic::Ordering::SeqCst);
                            force_first_frame = true;
                            info!("EGFX surface setup complete (AVC path)");
                        }
                    }

                    // Try to send via EGFX H.264 if encoder is available
                    if let (Some(encoder), Some(sender)) = (&mut video_encoder, &egfx_sender) {
                        // CLOSED-LOOP FLOW CONTROL (Layer 4 / GrdRdpGfxFrameController equivalent)
                        // — see src/egfx/flow_controller.rs.
                        //
                        // Before encoding the next frame, ask the flow controller whether
                        // we should pause. The controller engages throttling when unacked
                        // frame count exceeds an RTT-adaptive threshold (avoiding client
                        // decoder saturation that causes mstsc to give up under load).
                        //
                        // Throttle path: drop the frame at the encoder boundary, log
                        // periodically so operators can see flow control engaged.
                        // Break a stalled throttle: if the client has not acked
                        // within frame_ack_timeout, the flow controller drops the
                        // outstanding frames and resumes, and we force an IDR so
                        // the decoder resyncs. Without this a client decode hiccup
                        // (or a dropped ack) freezes the session permanently.
                        let ack_timeout =
                            std::time::Duration::from_millis(handler.config.egfx.frame_ack_timeout);
                        let (recovered_ms, throttle) = handler
                            .gfx_handler_state
                            .as_ref()
                            .and_then(|s| {
                                s.flow_controller.lock().ok().map(|mut fc| {
                                    (fc.check_ack_timeout(ack_timeout), fc.should_throttle())
                                })
                            })
                            .unwrap_or((None, false));
                        if let Some(stalled_ms) = recovered_ms {
                            warn!(
                                stalled_ms,
                                "EGFX: frame-ack stall past timeout — resumed encoder, \
                                 forcing IDR to resync client"
                            );
                            encoder.request_idr();
                            if let Some(ref reporter) = *handler.health_reporter.read().await {
                                reporter.report(crate::health::HealthEvent::VideoAckStalled {
                                    stalled_ms,
                                });
                            }
                        }
                        if throttle {
                            frames_dropped += 1;
                            if frames_dropped.is_multiple_of(60)
                                && let Some(ref s) = handler.gfx_handler_state
                                && let Ok(fc) = s.flow_controller.lock()
                            {
                                info!(
                                    unacked = fc.unacked_count(),
                                    activate_th = fc.activate_th(),
                                    avg_rtt_us = fc.avg_rtt().as_micros() as u64,
                                    state = ?fc.state(),
                                    "EGFX L4 flow controller: throttling encoder \
                                     (waiting for client to drain unacked frames)"
                                );
                            }
                            continue;
                        }

                        use crate::egfx::align_to_16;

                        let timestamp_ms = pipeline_decisions::compute_timestamp_ms(
                            frame.pts,
                            frames_sent,
                            self.config.video.target_fps,
                        );

                        // Extract CPU-resident pixel data.
                        // DMA-BUF passthrough frames (future) will take a different path
                        // through the hardware encoder directly.
                        let pixel_data = match &frame.buffer {
                            lamco_pipewire::FrameBuffer::Memory(data) => {
                                std::sync::Arc::clone(data)
                            }
                            lamco_pipewire::FrameBuffer::DmaBuf(_) => {
                                trace!("Skipping DMA-BUF frame in software encode path");
                                frames_dropped += 1;
                                continue;
                            }
                        };

                        // PipeWire sometimes sends zero-size buffers
                        let expected_size = (frame.width * frame.height * 4) as usize;
                        if pixel_data.len() < expected_size {
                            trace!(
                                "Skipping invalid frame: size={}, expected={} for {}×{}",
                                pixel_data.len(),
                                expected_size,
                                frame.width,
                                frame.height
                            );
                            frames_dropped += 1;
                            continue;
                        }

                        // HDR -> SDR tone mapping (opt-in). When enabled and the
                        // source output is HDR, convert the captured PQ/HLG
                        // pixels to SDR before damage detection and encode, in a
                        // fresh buffer (the PipeWire buffer is shared, so it
                        // cannot be mutated in place).
                        #[cfg(feature = "wayland")]
                        let pixel_data = if self.config.display.hdr_tone_mapping
                            && let Some(obs) = self.wayland_observers.color.get()
                            && let Some((transfer, peak)) = obs.hdr_source()
                            && let Some(src) = hdr_source_transfer(transfer)
                        {
                            let mut toned = pixel_data.as_ref().clone();
                            crate::egfx::tone_map::tonemap_bgrx_in_place(&mut toned, src, peak);
                            std::sync::Arc::new(toned)
                        } else {
                            pixel_data
                        };

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

                        // Decided once per iteration, before the skip/continue points
                        // below, so the compositor-hint probe and the log block it
                        // feeds agree on the same frame. See last_telemetry_log's
                        // declaration for why this is time- not frame-count-gated.
                        let should_log_telemetry =
                            last_telemetry_log.elapsed() >= telemetry_log_interval;

                        // Which source produced damage_regions this frame — logged
                        // periodically below so activity-classification behavior can be
                        // correlated with the Wayland damage protocol in use (compositor
                        // hints differ in reported granularity between e.g. wlr-screencopy
                        // and ext-image-copy-capture-v1; see adaptive_fps.rs module docs).
                        let mut damage_source = "forced";
                        // Periodic pixel-diff cross-check against compositor-hint damage,
                        // populated only on the same 60-frame cadence as the debug log below.
                        let mut compositor_vs_pixel_diff: Option<f32> = None;

                        let damage_regions = if force_full_frame {
                            // Force full frame - either periodic IDR or first frame after init
                            if periodic_idr_due {
                                debug!(
                                    "Forcing full frame for periodic IDR (bypassing damage detection)"
                                );
                            }
                            vec![DamageRegion::full_frame(frame.width, frame.height)]
                        } else if !frame.damage_regions.is_empty() && !compositor_hint_distrusted {
                            // Compositor-provided damage (authoritative, zero CPU cost --
                            // as long as it's trustworthy; see the calibration probe below).
                            // Use PipeWire damage hints from wlr-screencopy or ext-image-copy-capture.
                            damage_source = "compositor-hint";
                            let hint_regions: Vec<DamageRegion> = frame
                                .damage_regions
                                .iter()
                                .map(|r| DamageRegion::from(*r))
                                .collect();
                            if let Some(ref mut detector) = damage_detector_opt {
                                if should_log_telemetry {
                                    // Calibration probe: run the real pixel-diff detector
                                    // instead of the cheap reference-only update, so we can
                                    // measure how far this compositor's damage hints diverge
                                    // from an actual pixel diff. detect() updates the
                                    // reference frame as a side effect, same as
                                    // update_reference() would have -- this is not extra
                                    // drift, just a ~2-3ms probe once every ~2s.
                                    let probe_regions =
                                        detector.detect(&pixel_data, frame.width, frame.height);
                                    let pixel_diff_ratio = pipeline_decisions::compute_damage_ratio(
                                        &probe_regions,
                                        frame.width,
                                        frame.height,
                                    );
                                    let hint_ratio = pipeline_decisions::compute_damage_ratio(
                                        &hint_regions,
                                        frame.width,
                                        frame.height,
                                    );
                                    compositor_vs_pixel_diff = Some(pixel_diff_ratio);

                                    let divergence_pp = (hint_ratio - pixel_diff_ratio) * 100.0;
                                    let eval = pipeline_decisions::evaluate_compositor_trust(
                                        divergence_pp,
                                        consecutive_high_divergence,
                                        self.config
                                            .damage_tracking
                                            .compositor_hint_distrust_threshold_pp,
                                        self.config
                                            .damage_tracking
                                            .compositor_hint_distrust_consecutive_samples,
                                    );
                                    consecutive_high_divergence = eval.new_consecutive_count;
                                    if eval.should_distrust {
                                        compositor_hint_distrusted = true;
                                        warn!(
                                            "🎯 Compositor damage hints distrusted for this connection: \
                                             {} consecutive probes diverged >{:.1}pp from pixel-diff \
                                             (last sample: hint={:.1}% pixel-diff={:.1}%). \
                                             Switching to pixel-diff as primary damage source.",
                                            eval.new_consecutive_count,
                                            self.config
                                                .damage_tracking
                                                .compositor_hint_distrust_threshold_pp,
                                            hint_ratio * 100.0,
                                            pixel_diff_ratio * 100.0,
                                        );
                                        if let Some(ref reporter) =
                                            *handler.health_reporter.read().await
                                        {
                                            reporter.report(
                                                crate::health::HealthEvent::CompositorDamageHintsDistrusted {
                                                    divergence_pp: divergence_pp.abs(),
                                                },
                                            );
                                        }
                                    }
                                } else {
                                    // Keep DamageDetector reference frame synchronized for
                                    // seamless fallback, without the cost of a full diff.
                                    detector.update_reference(&pixel_data);
                                }
                            }
                            hint_regions
                        } else if let Some(ref mut detector) = damage_detector_opt {
                            // Fallback: pixel-diff damage detection (SIMD, ~2-3ms at 1080p).
                            // Reached either because the compositor sent no hint at all, or
                            // because the calibration probe proved this connection's hints
                            // untrustworthy (see compositor_hint_distrusted above).
                            damage_source = if compositor_hint_distrusted {
                                "pixel-diff-distrust"
                            } else {
                                "pixel-diff"
                            };
                            detector.detect(&pixel_data, frame.width, frame.height)
                        } else {
                            // Damage tracking disabled - use full frame
                            damage_source = "disabled";
                            vec![DamageRegion::full_frame(frame.width, frame.height)]
                        };

                        let damage_ratio = pipeline_decisions::compute_damage_ratio(
                            &damage_regions,
                            frame.width,
                            frame.height,
                        );

                        if adaptive_fps_enabled {
                            adaptive_fps.update(damage_ratio);
                        }

                        // Bypass latency governor for forced frames (init IDR, periodic IDR).
                        // The governor may skip based on timing, but forced frames MUST be sent
                        // immediately -- the client has no display content without them.
                        if !force_full_frame {
                            let encoding_decision =
                                latency_governor.should_encode_frame(damage_ratio);
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
                        }

                        if damage_regions.is_empty() {
                            frames_skipped_damage += 1;
                            if frames_skipped_damage.is_multiple_of(100)
                                && let Some(ref detector) = damage_detector_opt
                            {
                                let stats = detector.stats();
                                debug!(
                                    "🎯 Damage tracking: {} frames skipped (no change), {:.1}% bandwidth saved",
                                    frames_skipped_damage,
                                    stats.bandwidth_reduction_percent()
                                );
                            }
                            if adaptive_fps_enabled {
                                adaptive_fps.update(0.0);
                            }
                            continue;
                        }

                        if should_log_telemetry {
                            last_telemetry_log = std::time::Instant::now();
                            if let Some(ref detector) = damage_detector_opt {
                                let stats = detector.stats();
                                debug!(
                                    "🎯 Damage: {} regions, {:.1}% of frame, avg {:.1}ms detection, source={damage_source}",
                                    damage_regions.len(),
                                    damage_ratio * 100.0,
                                    stats.avg_detection_time_ms
                                );
                            }
                            if let Some(pixel_diff_ratio) = compositor_vs_pixel_diff {
                                let delta_pct = (damage_ratio - pixel_diff_ratio) * 100.0;
                                debug!(
                                    "🎯 Damage source cross-check: compositor-hint={:.1}% pixel-diff={:.1}% delta={delta_pct:+.1}pp",
                                    damage_ratio * 100.0,
                                    pixel_diff_ratio * 100.0
                                );
                            }
                            if adaptive_fps_enabled {
                                debug!(
                                    "🎛️ Adaptive FPS: activity={:?}, fps={}, latency_mode={:?}, damage_source={damage_source}",
                                    adaptive_fps.activity_level(),
                                    adaptive_fps.current_fps(),
                                    latency_governor.mode()
                                );
                                if let Some(ref state) = *self.fps_state.read().await {
                                    let fps_stats = adaptive_fps.stats();
                                    let mut snap = state.write();
                                    snap.enabled = true;
                                    snap.current_fps = adaptive_fps.current_fps();
                                    snap.activity_level =
                                        format!("{:?}", adaptive_fps.activity_level());
                                    snap.damage_source = damage_source.to_string();
                                    snap.frames_processed = fps_stats.frames_processed;
                                    snap.frames_skipped = fps_stats.frames_skipped;
                                    snap.time_at_static = fps_stats.time_at_static;
                                    snap.time_at_low = fps_stats.time_at_low;
                                    snap.time_at_medium = fps_stats.time_at_medium;
                                    snap.time_at_high = fps_stats.time_at_high;
                                }
                            }
                        }

                        // MS-RDPEGFX REQUIRES 16-pixel alignment
                        // Frame from PipeWire may not be aligned (e.g., 800×600)
                        // Must align dimensions AND pad frame data
                        // (Transform already applied above, before the EGFX/bitmap fork)
                        let aligned_width = align_to_16(frame.width);
                        let aligned_height = align_to_16(frame.height);

                        let frame_data =
                            if aligned_width != frame.width || aligned_height != frame.height {
                                Self::pad_frame_to_aligned(
                                    &pixel_data,
                                    frame.width,
                                    frame.height,
                                    aligned_width,
                                    aligned_height,
                                )
                            } else {
                                (*pixel_data).clone()
                            };

                        // Update adaptive QP from client feedback before encoding
                        if let Some(ref mut adapt) = encoding_adaptation {
                            let qp = adapt.adapted_qp();
                            sender.set_qp(qp);
                        }

                        // OpenH264's encode() is synchronous and CPU-bound.
                        // On slow hardware (e.g., QEMU VMs) it can block for seconds.
                        // block_in_place tells tokio this thread is occupied so the
                        // runtime can schedule other tasks on remaining threads.
                        //
                        // Time the encode so the operator can see when software
                        // encoding is the throughput bottleneck. Per-frame INFO
                        // when over the soft threshold; DEBUG for routine
                        // (consistent with the Processing-frame log policy).
                        let encode_start = std::time::Instant::now();
                        let encode_result = tokio::task::block_in_place(|| {
                            encoder.encode_bgra(
                                &frame_data,
                                aligned_width,
                                aligned_height,
                                timestamp_ms,
                            )
                        });
                        let encode_elapsed = encode_start.elapsed();
                        // Soft threshold: 100ms is the upper bound for 30fps
                        // delivery (one frame budget). Above that, the encoder
                        // is the limiting factor.
                        if encode_elapsed >= std::time::Duration::from_millis(100) {
                            info!(
                                "⏱ Encoder slow: {} ms for {}x{} {} (force_idr={}) — software encode may be throughput bottleneck",
                                encode_elapsed.as_millis(),
                                aligned_width,
                                aligned_height,
                                encoder.codec_name(),
                                force_full_frame,
                            );
                        } else {
                            debug!(
                                encode_ms = encode_elapsed.as_millis() as u64,
                                width = aligned_width,
                                height = aligned_height,
                                codec = encoder.codec_name(),
                                force_idr = force_full_frame,
                                "Encoder timing"
                            );
                        }
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
                                        // Notify flow controller that this frame is now
                                        // in flight (unacked). Will update state machine
                                        // and may engage throttling on subsequent iterations.
                                        if let Some(ref s) = handler.gfx_handler_state
                                            && let Ok(mut fc) = s.flow_controller.lock()
                                        {
                                            fc.unack_frame(_frame_id, 1);
                                        }
                                        continue; // Frame sent via EGFX, skip RemoteFX path
                                    }
                                    Err(e) => {
                                        // CRITICAL: Once EGFX is active, NEVER fall back to RemoteFX!
                                        // Mixing codecs causes display conflicts - EGFX surface invisible.
                                        //
                                        // Backpressure rejections are expected when the client falls
                                        // behind and need DEBUG visibility — at TRACE they were
                                        // invisible to the operator. Other SendError variants
                                        // (NotReady, NoSurface, ChannelClosed, EncodingFailed) are
                                        // less expected and get WARN.
                                        match &e {
                                            crate::server::egfx_sender::SendError::Backpressure => {
                                                debug!(
                                                    "EGFX send rejected (backpressure): client behind on FrameAcknowledge, frame dropped"
                                                );
                                            }
                                            _ => {
                                                warn!(
                                                    "EGFX send failed: {} - frame dropped (no RemoteFX fallback)",
                                                    e
                                                );
                                            }
                                        }
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
                                trace!(
                                    "H.264 encoding failed: {:?} - dropping frame (no RemoteFX fallback)",
                                    e
                                );
                                frames_dropped += 1;
                                continue; // Drop frame, don't fall through to RemoteFX
                            }
                        }
                    } else if let Some(ref sender) = egfx_sender {
                        // EGFX uncompressed fallback: no H.264 encoder but EGFX surface exists.
                        // Send raw pixels via WireToSurface1 with Codec1Type::Uncompressed.
                        // Per MS-RDPEGFX, clients in EGFX mode ignore FastPath bitmaps,
                        // so all frame delivery must go through WireToSurface1.
                        let pixel_bytes = match &frame.buffer {
                            lamco_pipewire::FrameBuffer::Memory(data) => data,
                            lamco_pipewire::FrameBuffer::DmaBuf(_) => {
                                trace!("Skipping DMA-BUF frame in uncompressed path");
                                frames_dropped += 1;
                                continue;
                            }
                        };

                        // PipeWire BGRx [B,G,R,X] is identical to RDP XRGB_8888 on
                        // little-endian: the 32-bit value 0xXXRRGGBB stores as [BB,GG,RR,XX].
                        // No pixel format conversion needed.
                        let timestamp_ms = (frame.pts / 1_000_000) as u32;
                        match sender
                            .send_uncompressed_frame(
                                pixel_bytes,
                                frame.width as u16,
                                frame.height as u16,
                                timestamp_ms,
                            )
                            .await
                        {
                            Ok(frame_id) => {
                                egfx_frames_sent += 1;
                                last_frame_time = std::time::Instant::now();
                                if !first_frame_received {
                                    first_frame_received = true;
                                    session_start = std::time::Instant::now();
                                }
                                if egfx_frames_sent <= 3 || egfx_frames_sent.is_multiple_of(30) {
                                    info!(
                                        "EGFX uncompressed: sent frame {} ({}x{})",
                                        frame_id, frame.width, frame.height
                                    );
                                }
                            }
                            Err(e) => {
                                debug!("EGFX uncompressed send failed: {}", e);
                                frames_dropped += 1;
                            }
                        }
                        continue;
                    }
                } else if !egfx_gate_bypassed && handler.is_egfx_ready().await {
                    // Non-AVC EGFX client (V8 only): setup surface and send uncompressed.
                    // This path handles clients that negotiate EGFX but don't advertise
                    // AVC420 support (e.g., rdpdo, ironrdp-web, minimal clients).
                    if needs_init {
                        use crate::egfx::align_to_16;
                        let aligned_width = align_to_16(frame.width) as u16;
                        let aligned_height = align_to_16(frame.height) as u16;
                        if let Some(sender) = handler
                            .setup_egfx_surface(
                                frame.width,
                                frame.height,
                                aligned_width,
                                aligned_height,
                            )
                            .await
                        {
                            egfx_sender = Some(sender);
                            handler
                                .egfx_needs_init
                                .store(false, std::sync::atomic::Ordering::SeqCst);
                            force_first_frame = true;
                            info!("EGFX surface setup complete (V8 uncompressed path)");
                        }
                    }

                    if let Some(ref sender) = egfx_sender {
                        let pixel_bytes = match &frame.buffer {
                            lamco_pipewire::FrameBuffer::Memory(data) => data,
                            lamco_pipewire::FrameBuffer::DmaBuf(_) => {
                                trace!("Skipping DMA-BUF frame in uncompressed path");
                                frames_dropped += 1;
                                continue;
                            }
                        };

                        // PipeWire BGRx = RDP XRGB_8888 on little-endian. No conversion needed.
                        let timestamp_ms = (frame.pts / 1_000_000) as u32;
                        match sender
                            .send_uncompressed_frame(
                                pixel_bytes,
                                frame.width as u16,
                                frame.height as u16,
                                timestamp_ms,
                            )
                            .await
                        {
                            Ok(frame_id) => {
                                egfx_frames_sent += 1;
                                last_frame_time = std::time::Instant::now();
                                if !first_frame_received {
                                    first_frame_received = true;
                                    session_start = std::time::Instant::now();
                                }
                                if egfx_frames_sent <= 3 || egfx_frames_sent.is_multiple_of(30) {
                                    info!(
                                        "EGFX V8 uncompressed: sent frame {} ({}x{})",
                                        frame_id, frame.width, frame.height
                                    );
                                }
                            }
                            Err(e) => {
                                debug!("EGFX V8 uncompressed send failed: {}", e);
                                frames_dropped += 1;
                            }
                        }
                        continue;
                    }
                }

                let frame_monitor_index = frame.monitor_index;
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
                let iron_updates = match handler
                    .convert_to_iron_format(&bitmap_update, frame_monitor_index)
                    .await
                {
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
                    let sender = handler.update_sender.lock().await;
                    for iron_bitmap in iron_updates {
                        let update = DisplayUpdate::Bitmap(iron_bitmap);

                        if let Err(e) = sender.send(update).await {
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

    /// Look up a monitor's virtual-desktop position by the PipeWire node id
    /// that produced a given frame (`VideoFrame::monitor_index`), for
    /// mapping that frame's stream-local coordinates into the shared
    /// `CoordinateTransformer`'s space (see `process_cursor_update` and
    /// `convert_to_iron_format`).
    ///
    /// This is a `node_id` lookup, not an array index: `stream_info`'s
    /// order isn't guaranteed to match monitor_index numerically (PipeWire
    /// node ids are arbitrary, globally-assigned object ids, not a 0-based
    /// enumeration), so indexing directly would silently resolve to the
    /// wrong monitor's offset — or an unrelated one — the moment more than
    /// one stream is live. Falls back to `(0, 0)` (no offset) when no
    /// matching stream is found, which is also the correct behavior for
    /// today's single-monitor reality (a `monitor_index` of the one active
    /// stream's node id, offset `(0, 0)` unless the primary is actually
    /// positioned away from the virtual desktop origin).
    fn stream_offset_for(&self, monitor_index: u32) -> (i32, i32) {
        self.stream_info
            .iter()
            .find(|s| s.node_id == monitor_index)
            .map_or((0, 0), |s| s.position)
    }

    /// Reposition a stream-local damage rectangle's origin into RDP
    /// virtual-desktop coordinates via the shared `CoordinateTransformer`,
    /// the same one `process_cursor_update` uses for cursor position.
    ///
    /// Transforms only `(left, top)`, then preserves the rectangle's
    /// original width/height exactly rather than independently
    /// transforming `(right, bottom)` too: `rect_data.data`'s byte length
    /// is fixed by the actual captured pixel buffer, and the RDP Bitmap
    /// Update wire format has no separate scale factor, only a declared
    /// width/height the client reads that many pixels for — if the
    /// transform ever applies real DPI scaling (today's monitor
    /// construction always has `scale_factor: 1.0`, so it doesn't), a
    /// scaled `right`/`bottom` would describe a region the pixel data
    /// doesn't actually match, not a resized one.
    ///
    /// Passes `rect` through unchanged (still correct for today's
    /// single-monitor-at-the-origin reality) if no `CoordinateTransformer`
    /// is wired yet, or if the transformed origin doesn't fit `u16`.
    async fn transform_rectangle_to_rdp(&self, monitor_index: u32, rect: Rectangle) -> Rectangle {
        let Some(transformer) = self
            .input_handler
            .read()
            .await
            .as_ref()
            .map(|h| Arc::clone(&h.coordinate_transformer))
        else {
            return rect;
        };
        let offset = self.stream_offset_for(monitor_index);
        let global_x = f64::from(i32::from(rect.left) + offset.0);
        let global_y = f64::from(i32::from(rect.top) + offset.1);
        let Ok((rdp_x, rdp_y)) = transformer.lock().await.stream_to_rdp(global_x, global_y) else {
            return rect;
        };
        let (Ok(left), Ok(top)) = (u16::try_from(rdp_x), u16::try_from(rdp_y)) else {
            return rect;
        };
        let width = rect.right.saturating_sub(rect.left);
        let height = rect.bottom.saturating_sub(rect.top);
        Rectangle {
            left,
            top,
            right: left.saturating_add(width),
            bottom: top.saturating_add(height),
        }
    }

    /// Handle a frame's `SPA_META_Cursor`: update cursor position/shape and
    /// emit the corresponding IronRDP pointer `DisplayUpdate`s.
    ///
    /// `monitor_index` identifies which capture stream this frame came from
    /// (`VideoFrame::monitor_index`, the stream's real PipeWire node id — not
    /// a 0-based array index, see `stream_offset_for`), used to recover the
    /// stream-space offset for the shared `CoordinateTransformer`. Whether
    /// this ever resolves more than one distinct monitor depends on the RDP
    /// server actually advertising multi-monitor DisplayControl capability
    /// (currently pinned to a single monitor, a separate gap from this
    /// lookup); this call is correct for whatever `stream_info` it's given
    /// and will reflect real multi-monitor layout once that's wired up.
    ///
    /// `CursorMode::Painted` (server-side compositing into the video frame)
    /// is not implemented yet — `CursorMode::requires_compositing()` exists
    /// for a future caller that does the compositing, but nothing calls it
    /// today. Until that lands, Painted falls through to the same
    /// client-side metadata path as Metadata/Predictive rather than sending
    /// no cursor at all; that's a deliberate temporary fallback, not the
    /// intended final behavior for that mode.
    async fn process_cursor_update(
        &self,
        cursor: Option<lamco_pipewire::meta::CursorMeta>,
        monitor_index: u32,
    ) {
        let Some(cursor) = cursor else {
            return;
        };

        let sender = self.get_update_sender();
        let mut strategy = self.cursor_strategy.lock().await;

        // id == 0 is the compositor's "invalid/no cursor" sentinel (e.g. the
        // pointer left the captured surface). Hide once, regardless of mode.
        let mode_hidden = strategy.mode() == crate::cursor::CursorMode::Hidden;
        if cursor.id == 0 || mode_hidden {
            if strategy.needs_hide_update() {
                let sender = sender.lock().await;
                if let Err(e) = sender.send(DisplayUpdate::HidePointer).await {
                    debug!("Failed to send cursor hide update: {e}");
                }
            }
            return;
        }
        strategy.note_visible();

        // Map the frame-local cursor position into RDP virtual-desktop space:
        // add this monitor's stream-space offset, then the shared
        // CoordinateTransformer (already built and maintained per-connection
        // by the input handler) does offset-recovery, monitor lookup, and
        // DPI/virtual-desktop normalization in one step.
        let transformer = self
            .input_handler
            .read()
            .await
            .as_ref()
            .map(|h| Arc::clone(&h.coordinate_transformer));
        if let Some(transformer) = transformer {
            let offset = self.stream_offset_for(monitor_index);
            let global_x = f64::from(cursor.position.0 + offset.0);
            let global_y = f64::from(cursor.position.1 + offset.1);
            match transformer.lock().await.stream_to_rdp(global_x, global_y) {
                Ok((rdp_x, rdp_y)) => strategy.update_position(rdp_x as i32, rdp_y as i32),
                Err(e) => {
                    debug!("Cursor position transform failed, keeping last known position: {e}");
                }
            }
        }

        // Compositors resend the bitmap only on a real shape change; most
        // frames carry a position-only cursor update. The shape cache lets
        // a shape that recurs after other shapes were sent in between (not
        // just the immediately-previous one) skip re-encoding entirely.
        // Lookup is checked before touching cursor.bitmap: a cache hit needs
        // no bitmap at all, and a slot is only ever claimed (cache_shape)
        // once a shape has actually been encoded and is about to be sent —
        // see CursorShapeCache::insert's doc for why that ordering matters.
        if let Some(cache_index) = strategy.lookup_shape_cache(cursor.id) {
            let sender = sender.lock().await;
            if let Err(e) = sender.send(DisplayUpdate::CachedPointer(cache_index)).await {
                debug!("Failed to send cached cursor pointer update: {e}");
            }
        } else if let Some(ref bitmap) = cursor.bitmap {
            match crate::cursor::convert_cursor_bitmap(bitmap, cursor.hotspot) {
                Ok(wire_pointer) => {
                    strategy.update_shape(crate::cursor::CursorShape {
                        width: u32::from(wire_pointer.width),
                        height: u32::from(wire_pointer.height),
                        hotspot_x: u32::from(wire_pointer.hot_x),
                        hotspot_y: u32::from(wire_pointer.hot_y),
                        data: wire_pointer.data.clone(),
                    });
                    let needs_top_down_rows = self.gfx_handler_state.as_ref().is_some_and(|s| {
                        s.needs_android_pointer_updates
                            .load(std::sync::atomic::Ordering::Acquire)
                    });
                    let cache_index = strategy.cache_shape(cursor.id);
                    let update = match wire_pointer
                        .into_display_update(cache_index, needs_top_down_rows)
                    {
                        crate::cursor::WireDisplayUpdate::Rgba(p) => DisplayUpdate::RGBAPointer(p),
                        crate::cursor::WireDisplayUpdate::Large(p) => {
                            DisplayUpdate::LargePointer(p)
                        }
                    };
                    let sender = sender.lock().await;
                    if let Err(e) = sender.send(update).await {
                        debug!("Failed to send cursor shape update: {e}");
                    }
                }
                Err(e) => {
                    debug!("Skipping cursor shape update: {e}");
                }
            }
        } else {
            debug!(
                "Cursor id {} not cached and no bitmap attached this frame; skipping shape update",
                cursor.id
            );
        }

        let (render_x, render_y) = strategy.render_position();
        if render_x >= 0
            && render_y >= 0
            && render_x <= i32::from(u16::MAX)
            && render_y <= i32::from(u16::MAX)
        {
            let position = ironrdp_pdu::pointer::PointerPositionAttribute {
                x: render_x as u16,
                y: render_y as u16,
            };
            let sender = sender.lock().await;
            if let Err(e) = sender.send(DisplayUpdate::PointerPosition(position)).await {
                debug!("Failed to send cursor position update: {e}");
            }
        }
    }

    /// Convert our BitmapUpdate format to IronRDP's BitmapUpdate format
    async fn convert_to_iron_format(
        &self,
        update: &BitmapUpdate,
        monitor_index: u32,
    ) -> Result<Vec<IronBitmapUpdate>> {
        let mut iron_updates = Vec::new();

        for rect_data in &update.rectangles {
            let rectangle = self
                .transform_rectangle_to_rdp(monitor_index, rect_data.rectangle)
                .await;
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

            let width = rectangle.right.saturating_sub(rectangle.left);
            let height = rectangle.bottom.saturating_sub(rectangle.top);

            let bytes_per_pixel = iron_format.bytes_per_pixel() as usize;
            let stride = NonZeroUsize::new(width as usize * bytes_per_pixel)
                .ok_or_else(|| anyhow::anyhow!("Invalid stride calculation: width={width}"))?;

            let iron_bitmap = IronBitmapUpdate {
                x: rectangle.left,
                y: rectangle.top,
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
    async fn updates(&mut self) -> ServerResult<Box<dyn RdpServerDisplayUpdates>> {
        let mut receiver_option = self.update_receiver.lock().await;

        // If receiver was already taken by a previous connection, create a new channel
        if receiver_option.is_none() {
            debug!("Display updates channel exhausted, creating new channel for reconnection");
            let (new_sender, new_receiver) = mpsc::channel(64);
            *self.update_sender.lock().await = new_sender;
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

            // Reset handler state atomics to force waiting for NEW EGFX channel negotiation.
            // The new connection's GfxServerFactory.build_server_with_handle() will
            // update these atomics when the client's EGFX DVC channel is established.
            if let Some(ref state) = self.gfx_handler_state {
                state.reset();
                info!("Reset gfx_handler_state atomics for new EGFX negotiation");
            }

            // NOTE: Do NOT clear gfx_server_handle here. The GfxServerFactory's
            // build_server_with_handle() already replaced it with the new client's
            // handle BEFORE updates() is called. Clearing it here would destroy
            // the new handle, causing is_egfx_ready() to return false indefinitely.
            {
                let handle = self.gfx_server_handle.read().await;
                let has_handle = handle.is_some();
                info!("gfx_server_handle after factory: {has_handle} (preserved for new client)");
            }

            // Reset bitmap converter so the new client gets a full initial frame.
            // The converter caches the last frame hash for dirty-region optimization;
            // without this reset, the replayed cached frame matches the hash and
            // produces an empty update (zero visible bitmap data).
            //
            // Use try_lock to avoid potential deadlock with the pipeline loop.
            // If the lock isn't available, force_full_update will be called when
            // the pipeline processes the next frame.
            match self.bitmap_converter.try_lock() {
                Ok(mut converter) => {
                    let size = self.size.read().await;
                    *converter = BitmapConverter::new(size.width, size.height);
                    debug!("Reset BitmapConverter for {}x{}", size.width, size.height);
                }
                _ => {
                    debug!("BitmapConverter locked by pipeline, will reset on next frame");
                }
            }

            // Reset internal state for reconnection
            if let Some(ref handler) = *self.input_handler.read().await {
                handler.notify_reconnection().await;
            }

            // Connect-start clipboard reset — ONLY on a genuine reconnection.
            // This updates() reset block ALSO runs on a same-connection
            // DeactivationReactivation (display resize): that preserves the
            // CLIPRDR channel and emits no new Ready, so tearing the latch down
            // here would suppress every Linux→Windows paste until the client next
            // copies. saw_real_disconnect (set by on_client_disconnect) gates it,
            // and also re-drives teardown if the disconnect-time emission was lost.
            if self
                .saw_real_disconnect
                .swap(false, std::sync::atomic::Ordering::SeqCst)
            {
                self.notify_clipboard_disconnect().await;
            }
        }

        // Activate deferred input subsystem (EIS) if not already active.
        // This is idempotent — activate_input() returns Ok immediately if already active.
        if let Some(ref handler) = *self.input_handler.read().await
            && let Err(e) = handler.activate_input().await
        {
            warn!("Failed to activate EIS input: {}", e);
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
        if let Ok(current) = self.size.try_read()
            && current.width == new_w
            && current.height == new_h
        {
            debug!("Requested resolution matches current, ignoring");
            return;
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
    // This is hand-written rather than derived because `last_resize_time` and
    // `stream_active_flag` are not `Clone` (they wrap fresh locks). The exhaustive
    // `Self { .. }` literal below — no `..rest` — is the drift guard the audit
    // (L1) asked for: adding a field to the struct without cloning it here is a
    // hard `E0063: missing field` error, so the two cannot silently diverge.
    fn clone(&self) -> Self {
        Self {
            size: Arc::clone(&self.size),
            pipewire_thread: Arc::clone(&self.pipewire_thread),
            bitmap_converter: Arc::clone(&self.bitmap_converter),
            update_sender: Arc::clone(&self.update_sender),
            update_receiver: Arc::clone(&self.update_receiver),
            graphics_tx: self.graphics_tx.clone(),
            stream_info: self.stream_info.clone(),
            cursor_strategy: Arc::clone(&self.cursor_strategy),
            autodetect_rtt: Arc::clone(&self.autodetect_rtt),
            // EGFX fields
            gfx_server_handle: Arc::clone(&self.gfx_server_handle),
            gfx_handler_state: self.gfx_handler_state.as_ref().map(Arc::clone),
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
            capture_node: Arc::clone(&self.capture_node),
            saw_real_disconnect: Arc::clone(&self.saw_real_disconnect),
            health_reporter: Arc::clone(&self.health_reporter),
            pipewire_sensor: Arc::clone(&self.pipewire_sensor),
            egfx_snapshot: Arc::clone(&self.egfx_snapshot),
            fps_state: Arc::clone(&self.fps_state),
            encoder_state: Arc::clone(&self.encoder_state),
            stream_active_flag: parking_lot::RwLock::new(self.stream_active_flag.read().clone()),
            compositor_scale: parking_lot::RwLock::new(*self.compositor_scale.read()),
            direct_channel_mode: self.direct_channel_mode,
            use_dmabuf: self.use_dmabuf,
            #[cfg(feature = "wayland")]
            wayland_observers: self.wayland_observers.clone(),
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
    async fn next_update(&mut self) -> ServerResult<Option<DisplayUpdate>> {
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

// =============================================================================
// BUFFER TRANSFORM SUPPORT
// =============================================================================

/// Resolve the effective transform value from config and PipeWire metadata.
///
/// Config takes precedence: if set to anything other than "auto", it overrides
/// the PipeWire metadata value.
fn resolve_transform(config_value: &str, pw_transform: u32) -> u32 {
    match config_value {
        "auto" => pw_transform,
        "none" => 0,
        "90" => 1,
        "180" => 2,
        "270" => 3,
        "flipped" => 4,
        "flipped-90" => 5,
        "flipped-180" => 6,
        "flipped-270" => 7,
        _ => {
            warn!(
                "Unknown frame_transform config value '{}', using auto",
                config_value
            );
            pw_transform
        }
    }
}

/// Apply a buffer transform to pixel data.
///
/// Implements all 8 values from the SPA/wl_output transform spec using
/// three primitive operations: flip_vertical, flip_horizontal, transpose.
///
/// Returns (transformed_data, new_width, new_height, new_stride).
/// For 90/270 rotations, width and height are swapped.
fn apply_frame_transform(
    data: &[u8],
    width: u32,
    height: u32,
    stride: u32,
    transform: u32,
    bpp: u32,
) -> (Vec<u8>, u32, u32, u32) {
    if transform == 0 {
        return (data.to_vec(), width, height, stride);
    }

    debug!(
        "Applying buffer transform {} to {}x{} frame",
        transform, width, height
    );

    match transform {
        // 180: flip_vertical + flip_horizontal
        2 => {
            let mut buf = data.to_vec();
            flip_vertical(&mut buf, width, height, stride);
            flip_horizontal(&mut buf, width, height, stride, bpp);
            (buf, width, height, stride)
        }
        // Flipped (4): flip_horizontal only
        4 => {
            let mut buf = data.to_vec();
            flip_horizontal(&mut buf, width, height, stride, bpp);
            (buf, width, height, stride)
        }
        // Flipped180 (6): flip_vertical only — single-pass reverse-order copy
        6 => {
            let stride = stride as usize;
            let height = height as usize;
            let mut buf = Vec::with_capacity(stride * height);
            for row in (0..height).rev() {
                let start = row * stride;
                buf.extend_from_slice(&data[start..start + stride]);
            }
            (buf, width, height as u32, stride as u32)
        }
        // 90 CCW (1): transpose + flip_horizontal
        1 => {
            let (transposed, new_stride) = transpose(data, width, height, stride, bpp);
            let mut buf = transposed;
            flip_horizontal(&mut buf, height, width, new_stride, bpp);
            (buf, height, width, new_stride)
        }
        // 270 CCW (3): transpose + flip_vertical
        3 => {
            let (transposed, new_stride) = transpose(data, width, height, stride, bpp);
            let mut buf = transposed;
            flip_vertical(&mut buf, height, width, new_stride);
            (buf, height, width, new_stride)
        }
        // Flipped90 (5): transpose only
        5 => {
            let (transposed, new_stride) = transpose(data, width, height, stride, bpp);
            (transposed, height, width, new_stride)
        }
        // Flipped270 (7): flip_horizontal + transpose
        7 => {
            let mut buf = data.to_vec();
            flip_horizontal(&mut buf, width, height, stride, bpp);
            let (transposed, new_stride) = transpose(&buf, width, height, stride, bpp);
            (transposed, height, width, new_stride)
        }
        _ => {
            warn!(
                "Unknown transform value {}, returning data unchanged",
                transform
            );
            (data.to_vec(), width, height, stride)
        }
    }
}

/// Reverse row order in-place.
fn flip_vertical(data: &mut [u8], _width: u32, height: u32, stride: u32) {
    let stride = stride as usize;
    let height = height as usize;
    let mut top = 0usize;
    let mut bottom = (height - 1) * stride;

    while top < bottom {
        // Swap rows in-place
        for i in 0..stride {
            data.swap(top + i, bottom + i);
        }
        top += stride;
        bottom -= stride;
    }
}

/// Reverse pixel order within each row in-place.
fn flip_horizontal(data: &mut [u8], width: u32, height: u32, stride: u32, bpp: u32) {
    let width = width as usize;
    let height = height as usize;
    let stride = stride as usize;
    let bpp = bpp as usize;

    for row in 0..height {
        let row_start = row * stride;
        let mut left = 0usize;
        let mut right = width - 1;

        while left < right {
            let l_off = row_start + left * bpp;
            let r_off = row_start + right * bpp;

            for b in 0..bpp {
                data.swap(l_off + b, r_off + b);
            }
            left += 1;
            right -= 1;
        }
    }
}

/// Transpose image: swap rows and columns.
/// Output dimensions are height x width (swapped).
/// Returns (new_data, new_stride).
fn transpose(data: &[u8], width: u32, height: u32, stride: u32, bpp: u32) -> (Vec<u8>, u32) {
    let width = width as usize;
    let height = height as usize;
    let stride = stride as usize;
    let bpp = bpp as usize;

    // Output is height-wide and width-tall
    let new_width = height;
    let new_stride = new_width * bpp;
    let new_height = width;

    let mut out = vec![0u8; new_stride * new_height];

    for y in 0..height {
        for x in 0..width {
            let src_off = y * stride + x * bpp;
            let dst_off = x * new_stride + y * bpp;

            out[dst_off..dst_off + bpp].copy_from_slice(&data[src_off..src_off + bpp]);
        }
    }

    (out, new_stride as u32)
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
