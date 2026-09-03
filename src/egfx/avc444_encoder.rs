//! AVC444 H.264 4:4:4 Encoder
//!
//! Encodes BGRA frames to dual YUV420 H.264 bitstreams for AVC444 transmission.
//!
//! # Architecture
//!
//! AVC444 encoding works by splitting YUV444 into two YUV420 streams:
//!
//! ```text
//! BGRA Frame
//!     │
//!     ▼
//! ┌────────────────────┐
//! │ BGRA → YUV444      │  (color_convert.rs)
//! │ BT.709 or BT.601   │
//! └────────────────────┘
//!     │
//!     ▼
//! ┌────────────────────┐
//! │ YUV444 → Dual      │  (yuv444_packing.rs)
//! │   YUV420 Views     │
//! └────────────────────┘
//!     │         │
//!     ▼         ▼
//! ┌───────┐ ┌───────┐
//! │ Main  │ │ Aux   │
//! │ View  │ │ View  │
//! └───────┘ └───────┘
//!     │         │
//!     ▼         ▼
//! ┌───────┐ ┌───────┐
//! │OpenH264│ │OpenH264│  (dual encoders)
//! └───────┘ └───────┘
//!     │         │
//!     ▼         ▼
//! Stream 1   Stream 2
//! (Main)     (Auxiliary)
//! ```
//!
//! # Memory Usage
//!
//! - Two OpenH264 encoder instances: ~10-20MB each
//! - YUV444 buffers: ~6MB per 1080p frame
//! - Total: ~30-40MB for 1080p encoding
//!
//! # Performance
//!
//! With SIMD color conversion and optimized packing:
//! - 1080p: ~15-25ms total encode time
//! - 720p: ~8-12ms total encode time
//!
//! # MS-RDPEGFX Reference
//!
//! See MS-RDPEGFX Section 3.3.8.3.2 for the AVC444 specification.

#[cfg(feature = "h264")]
use tracing::{debug, info, trace};

#[cfg(feature = "h264")]
use super::openh264_compat;
use super::{
    color_convert::{ColorMatrix, bgra_to_yuv444},
    color_space::{ColorRange, ColorSpaceConfig},
    encoder::{EncoderConfig, EncoderError, EncoderResult},
    yuv444_packing::{Yuv420Frame, pack_dual_views},
};

/// AVC444 encoded frame (dual H.264 bitstreams)
///
/// # Phase 1: Auxiliary Stream Omission
///
/// The `stream2_data` field is now Optional to support bandwidth optimization.
/// When `None`, the MS-RDPEGFX LC field is set to 1 (luma only), instructing
/// the client to reuse its previously cached auxiliary stream.
///
/// This implements the FreeRDP-proven pattern for AVC444 bandwidth reduction.
#[derive(Debug)]
pub struct Avc444Frame {
    /// Main view bitstream (full luma + subsampled chroma)
    ///
    /// Always present - contains primary visual information
    pub stream1_data: Vec<u8>,

    /// Auxiliary view bitstream (additional chroma data)
    ///
    /// **Phase 1: Now Optional for bandwidth optimization**
    ///
    /// - `Some(data)`: Auxiliary stream present (LC=0 or LC=2)
    /// - `None`: Auxiliary stream omitted (LC=1, client reuses previous)
    ///
    /// When `None`, client decoder:
    /// 1. Decodes main stream normally
    /// 2. Retrieves previous auxiliary data from cache
    /// 3. Combines to reconstruct YUV444
    ///
    /// Expected omission rate: 60-90% of frames (depending on content)
    pub stream2_data: Option<Vec<u8>>,

    /// Whether this is a keyframe (IDR) in main stream
    pub is_keyframe: bool,

    /// Frame timestamp in milliseconds
    pub timestamp_ms: u64,

    /// Total encoded size (stream1 + stream2 if present)
    ///
    /// When stream2 is omitted, this only reflects main stream size,
    /// providing accurate bandwidth measurement
    pub total_size: usize,

    /// Encoding time breakdown (for performance monitoring)
    pub timing: Avc444Timing,
}

/// Timing breakdown for AVC444 encoding
#[derive(Debug, Clone, Default)]
pub struct Avc444Timing {
    /// Time for BGRA → YUV444 conversion (ms)
    pub color_convert_ms: f32,
    /// Time for YUV444 → dual YUV420 packing (ms)
    pub packing_ms: f32,
    /// Time for dual H.264 encoding (ms)
    pub encoding_ms: f32,
    /// Total time (ms)
    pub total_ms: f32,
}

/// AVC444 encoder statistics
#[derive(Debug, Clone)]
pub struct Avc444Stats {
    /// Total AVC444 frames produced
    pub frames_encoded: u64,
    /// Total bytes encoded (both streams)
    pub bytes_encoded: u64,
    /// Average encoding time (ms)
    pub avg_encode_time_ms: f32,
    /// Configured bitrate (kbps)
    pub bitrate_kbps: u32,
    /// Color matrix in use
    pub color_matrix: ColorMatrix,
}

/// AVC444 Encoder
///
/// Encodes BGRA frames to dual YUV420 H.264 bitstreams for AVC444 transmission.
///
/// # Usage
///
/// ```rust,ignore
/// use lamco_rdp_server::egfx::{Avc444Encoder, EncoderConfig};
///
/// let config = EncoderConfig::default();
/// let mut encoder = Avc444Encoder::new(config)?;
///
/// let frame = encoder.encode_bgra(&bgra_data, 1920, 1080, timestamp)?;
/// if let Some(frame) = frame {
///     // Send frame.stream1_data and frame.stream2_data via EGFX
/// }
/// ```
/// Send-safe proxy to the VA-API AVC444 encoder running on its own thread.
///
/// `VaapiEncoder` is `!Send` (thread-affine VA handles), but the display
/// pipeline is a `tokio::spawn`'d future that must be `Send`, so the encoder
/// lives on a dedicated thread behind [`HardwareEncoderThread`]. Main and aux
/// subframes are issued back to back on that one thread, preserving the
/// single-encoder unified DPB that MS-RDPEGFX § 3.3.8.3.2 requires.
#[cfg(all(feature = "h264", feature = "vaapi"))]
struct HardwareBackendProxy {
    thread: crate::egfx::hardware::HardwareEncoderThread,
    /// Macroblock-aligned encode dimensions. The VA-API backend signals no
    /// frame cropping, so each view is edge-padded to these before upload.
    aligned_width: u32,
    aligned_height: u32,
    /// Set by `force_keyframe`; folded into the next (main) subframe's IDR flag.
    pending_idr: bool,
}

#[cfg(all(feature = "h264", feature = "vaapi"))]
impl HardwareBackendProxy {
    fn encode_view(
        &mut self,
        view: &Yuv420Frame,
        timestamp_ms: i64,
        force_idr: bool,
    ) -> EncoderResult<(Vec<u8>, bool)> {
        let nv12 = i420_to_nv12(
            view,
            self.aligned_width as usize,
            self.aligned_height as usize,
        );
        let force = force_idr || std::mem::take(&mut self.pending_idr);
        match self.thread.encode_nv12(
            nv12,
            self.aligned_width,
            self.aligned_height,
            timestamp_ms as u64,
            force,
        ) {
            Ok(Some(result)) => Ok(result),
            // An encoder skip (rate control) is reported as an empty subframe;
            // the caller treats that as an omitted aux.
            Ok(None) => Ok((Vec::new(), false)),
            Err(msg) => Err(EncoderError::EncodeFailed(format!(
                "hardware encode failed: {msg}"
            ))),
        }
    }
}

/// H.264 backend for the AVC444 dual-view encoder.
///
/// Both subframes MUST be encoded by the same encoder (MS-RDPEGFX § 3.3.8.3.2),
/// so a single backend instance encodes Main then Aux sequentially, sharing one
/// DPB. The software backend is OpenH264; the hardware backend is a single
/// VA-API encoder driven, on its own thread, through its NV12 entry point.
#[cfg(feature = "h264")]
enum Avc444Backend {
    Software(openh264_compat::VersionedEncoder),
    #[cfg(feature = "vaapi")]
    Hardware(HardwareBackendProxy),
}

#[cfg(feature = "h264")]
impl Avc444Backend {
    /// Encode one YUV420 subframe. `force_idr` requests an IDR for this
    /// subframe. Returns `(bitstream, is_keyframe)`; an empty bitstream means the
    /// encoder skipped the frame (rate control), which the caller treats as an
    /// omitted aux so encoder and decoder DPBs stay in sync.
    fn encode_view(
        &mut self,
        view: &Yuv420Frame,
        w: i32,
        h: i32,
        timestamp_ms: i64,
        force_idr: bool,
    ) -> EncoderResult<(Vec<u8>, bool)> {
        match self {
            Avc444Backend::Software(encoder) => {
                if force_idr {
                    encoder.force_intra_frame();
                }
                let strides = view.strides();
                let encoded = encoder
                    .encode(
                        view.y_plane(),
                        view.u_plane(),
                        view.v_plane(),
                        strides.0 as i32,
                        strides.1 as i32,
                        strides.2 as i32,
                        w,
                        h,
                        timestamp_ms,
                    )
                    .map_err(|e| {
                        EncoderError::EncodeFailed(format!("subframe encoding failed: {e}"))
                    })?;
                Ok((encoded.to_vec(), encoded.is_keyframe()))
            }
            #[cfg(feature = "vaapi")]
            Avc444Backend::Hardware(proxy) => proxy.encode_view(view, timestamp_ms, force_idr),
        }
    }

    fn force_keyframe(&mut self) {
        match self {
            Avc444Backend::Software(encoder) => encoder.force_intra_frame(),
            #[cfg(feature = "vaapi")]
            Avc444Backend::Hardware(proxy) => proxy.pending_idr = true,
        }
    }
}

/// Repack planar I420 into contiguous NV12 (Y plane then interleaved UV),
/// edge-padding to `(dst_w, dst_h)` when the source is smaller. The VA-API
/// backend signals no frame cropping, so encode dimensions are macroblock
/// aligned and the last valid row/column is replicated into the padding to
/// avoid hard edges the encoder would otherwise spend bits on.
#[cfg(all(feature = "h264", feature = "vaapi"))]
fn i420_to_nv12(view: &Yuv420Frame, dst_w: usize, dst_h: usize) -> Vec<u8> {
    let (src_w, src_h) = view.dimensions();
    let (y_stride, u_stride, v_stride) = view.strides();
    let y = view.y_plane();
    let u = view.u_plane();
    let v = view.v_plane();

    let cw = dst_w / 2;
    let ch = dst_h / 2;
    let src_cw = src_w / 2;
    let src_ch = src_h / 2;
    let mut nv12 = vec![0u8; dst_w * dst_h + 2 * cw * ch];

    for row in 0..dst_h {
        let src_row = row.min(src_h - 1);
        let src = &y[src_row * y_stride..src_row * y_stride + src_w];
        let dst = &mut nv12[row * dst_w..row * dst_w + dst_w];
        dst[..src_w].copy_from_slice(src);
        let last = src[src_w - 1];
        for px in &mut dst[src_w..] {
            *px = last;
        }
    }

    let uv_off = dst_w * dst_h;
    for row in 0..ch {
        let src_row = row.min(src_ch - 1);
        let u_row = &u[src_row * u_stride..src_row * u_stride + src_cw];
        let v_row = &v[src_row * v_stride..src_row * v_stride + src_cw];
        let dst = &mut nv12[uv_off + row * cw * 2..uv_off + row * cw * 2 + cw * 2];
        for col in 0..cw {
            let sc = col.min(src_cw - 1);
            dst[col * 2] = u_row[sc];
            dst[col * 2 + 1] = v_row[sc];
        }
    }
    nv12
}

#[cfg(feature = "h264")]
pub struct Avc444Encoder {
    /// SINGLE H.264 encoder for BOTH Main and Aux subframes
    ///
    /// MS-RDPEGFX spec requirement: "The two subframe bitstreams MUST be
    /// encoded using the same H.264 encoder" (Section 3.3.8.3.2)
    ///
    /// This ensures unified DPB (Decoded Picture Buffer) timeline between
    /// Main and Aux, preventing cross-stream reference corruption.
    backend: Avc444Backend,

    /// Configuration
    config: EncoderConfig,

    /// Color space configuration (includes matrix + VUI parameters)
    color_space: ColorSpaceConfig,

    /// Color matrix for RGB→YUV conversion (derived from color_space)
    color_matrix: ColorMatrix,

    /// Frame counter
    frame_count: u64,

    /// Total bytes encoded
    bytes_encoded: u64,

    /// Sum of encoding times (for average calculation)
    total_encode_time_ms: f64,

    /// Current H.264 level
    current_level: Option<super::h264_level::H264Level>,

    // === DIAGNOSTIC FLAGS ===
    /// Force all frames to be keyframes (disable P-frames)
    /// Set to true to diagnose P-frame specific color issues
    force_all_keyframes: bool,

    // === PHASE 1: AUX OMISSION (BANDWIDTH OPTIMIZATION) ===
    /// Hash of last encoded auxiliary frame for change detection
    /// None = no aux encoded yet or omission disabled
    last_aux_hash: Option<u64>,

    /// Number of frames since last auxiliary update
    /// Used to enforce max_aux_interval refresh policy
    frames_since_aux: u32,

    /// Maximum frames between auxiliary updates (forced refresh)
    /// Default: 30 frames (1 second @ 30fps)
    /// Range: 1-120 frames
    /// - Lower (10-20): Higher quality, more bandwidth, responsive to color changes
    /// - Medium (30-40): Balanced, recommended for most content
    /// - Higher (60-120): Lower bandwidth, acceptable for static/slow-changing content
    max_aux_interval: u32,

    /// Threshold for detecting auxiliary content changes (0.0-1.0)
    /// Fraction of sampled pixels that must differ to trigger aux update
    /// - 0.0: Any change triggers update (highest quality, most bandwidth)
    /// - 0.05: 5% of pixels changed (balanced, recommended)
    /// - 0.1: 10% of pixels changed (aggressive omission, lowest bandwidth)
    aux_change_threshold: f32,

    /// Force auxiliary stream to IDR when reintroducing after omission
    /// Default: true (safe mode - prevents aux P-frames from referencing stale frames)
    /// - true: Always IDR when aux returns (robust, recommended)
    /// - false: Allow aux P-frames (experimental, may reduce quality)
    force_aux_idr_on_return: bool,

    /// Enable auxiliary stream omission (LC field optimization)
    /// Default: false initially (for gradual rollout)
    /// When true: implements FreeRDP-proven bandwidth optimization
    /// When false: always sends both streams (current all-I behavior)
    enable_aux_omission: bool,

    // === PERIODIC IDR (ARTIFACT RECOVERY) ===
    /// Time of last IDR keyframe (for periodic forced IDR)
    last_idr_time: std::time::Instant,

    /// Interval in seconds for forced IDR keyframes (0 = disabled)
    /// Forces full IDR at regular intervals to clear accumulated artifacts.
    /// Recommended: 5-10 seconds for VDI, 2-3 for unreliable networks.
    periodic_idr_interval_secs: u32,

    /// Flag to force next frame as IDR (set by client PLI or periodic timer)
    force_next_idr: bool,

    /// Flag to force aux inclusion on next frame (set when periodic IDR fires)
    /// This bypasses aux omission to ensure BOTH streams refresh together
    force_aux_on_next_frame: bool,

    /// Optional diagnostics — TRACE NAL hex dump (always-on within encoder),
    /// H.264 file dump (config-gated), and decoder self-test (config-gated).
    /// None when both opt-in flags are off (zero per-frame cost).
    diagnostics: Option<std::sync::Arc<super::encode_diagnostics::EncodeDiagnostics>>,
}

#[cfg(feature = "h264")]
impl Avc444Encoder {
    pub fn new(config: EncoderConfig) -> EncoderResult<Self> {
        // Determine color space configuration:
        // 1. Use explicit config if provided
        // 2. Otherwise, auto-select based on resolution (BT.709 for HD, BT.601 for SD)
        let color_space = config.color_space.unwrap_or({
            match (config.width, config.height) {
                (Some(w), Some(h)) if w >= 1280 && h >= 720 => ColorSpaceConfig::BT709_FULL,
                (Some(_), Some(_)) => ColorSpaceConfig::BT601_LIMITED,
                // Default to BT.709 when dimensions unknown (will be HD in most cases)
                _ => ColorSpaceConfig::BT709_FULL,
            }
        });
        let color_matrix = color_space.matrix;

        // Calculate appropriate H.264 level if dimensions provided
        let level = config
            .width
            .zip(config.height)
            .map(|(w, h)| super::h264_level::H264Level::for_config(w, h, config.max_fps));

        // Build VuiConfig from ColorSpaceConfig for H.264 SPS signaling
        let vui = match (color_space.matrix, color_space.range) {
            (ColorMatrix::BT709, ColorRange::Full) => openh264_compat::VuiConfig::bt709_full(),
            (ColorMatrix::BT709, ColorRange::Limited) => openh264_compat::VuiConfig::bt709(),
            (ColorMatrix::BT601 | ColorMatrix::OpenH264, _) => openh264_compat::VuiConfig::bt601(),
        };

        // High complexity for better text sharpness, scene change detect disabled
        // so IDRs come from our periodic-IDR scheduler rather than OpenH264's heuristic.
        let compat_config = openh264_compat::EncoderConfig {
            bitrate_bps: config.bitrate_kbps * 1000,
            max_frame_rate: config.max_fps,
            usage_type: openh264_compat::ffi_types::SCREEN_CONTENT_REAL_TIME,
            num_threads: config.encoder_threads,
            enable_skip_frame: config.enable_skip_frame,
            max_qp: config.qp_max as i32,
            min_qp: config.qp_min as i32,
            complexity: openh264_compat::HIGH_COMPLEXITY,
            enable_scene_change_detect: false,
            level_idc: level.map(|l| l.to_openh264_level_idc()),
            vui,
            ..openh264_compat::EncoderConfig::default()
        };

        info!(
            "AVC444: High complexity, QP [{}, {}], VUI enabled",
            config.qp_min, config.qp_max
        );

        // Create SINGLE encoder for both Main and Aux (MS-RDPEGFX spec compliant)
        let api = super::encoder::load_openh264_api()?;
        info!("AVC444: {}", api.capabilities);
        let encoder = openh264_compat::VersionedEncoder::new(api, compat_config).map_err(|e| {
            EncoderError::InitFailed(format!("AVC444 single encoder init failed: {e}"))
        })?;

        debug!(
            "Created AVC444 SINGLE encoder: {} color space, {}kbps, level={:?}",
            color_space.description(),
            config.bitrate_kbps,
            level
        );
        info!(
            "AVC444: VUI enabled ({}, primaries={}, transfer={}, matrix={})",
            if color_space.range == ColorRange::Full {
                "full range"
            } else {
                "limited range"
            },
            color_space.vui_colour_primaries(),
            color_space.vui_transfer_characteristics(),
            color_space.vui_matrix_coefficients()
        );

        Ok(Self::assemble(
            config,
            color_space,
            color_matrix,
            level,
            Avc444Backend::Software(encoder),
        ))
    }

    /// Assemble the encoder around a chosen backend. Shared by `new` (OpenH264)
    /// and `new_hardware` (VA-API) so the field defaults live in one place.
    fn assemble(
        config: EncoderConfig,
        color_space: ColorSpaceConfig,
        color_matrix: ColorMatrix,
        level: Option<super::h264_level::H264Level>,
        backend: Avc444Backend,
    ) -> Self {
        Self {
            backend,
            config,
            color_space,
            color_matrix,
            frame_count: 0,
            bytes_encoded: 0,
            total_encode_time_ms: 0.0,
            current_level: level,
            // DIAGNOSTIC FLAG: Force all keyframes to disable P-frame inter-prediction
            force_all_keyframes: false,
            // Phase 1: Aux omission defaults, overridden by configure_aux_omission()
            last_aux_hash: None,
            frames_since_aux: 0,
            max_aux_interval: 30,
            aux_change_threshold: 0.05,
            force_aux_idr_on_return: false,
            enable_aux_omission: false,
            // Periodic IDR defaults, overridden by configure_periodic_idr()
            last_idr_time: std::time::Instant::now(),
            periodic_idr_interval_secs: 5,
            force_next_idr: false,
            force_aux_on_next_frame: false,
            diagnostics: None,
        }
    }

    /// Create an AVC444 encoder backed by a single VA-API H.264 encoder.
    ///
    /// Both subframes run through this one encoder (MS-RDPEGFX § 3.3.8.3.2). It
    /// is built at the config's macroblock-aligned dimensions because the VA-API
    /// backend signals no frame cropping. Requires the VA-API backend; any other
    /// hardware backend is rejected so the caller falls back to software AVC444
    /// rather than failing mid-stream.
    #[cfg(feature = "vaapi")]
    pub fn new_hardware(
        config: EncoderConfig,
        hw_config: &crate::config::HardwareEncodingConfig,
    ) -> EncoderResult<Self> {
        let width = config.width.ok_or_else(|| {
            EncoderError::InitFailed("hardware AVC444 requires known dimensions".to_string())
        })?;
        let height = config.height.ok_or_else(|| {
            EncoderError::InitFailed("hardware AVC444 requires known dimensions".to_string())
        })?;

        let w32 = u32::from(width);
        let h32 = u32::from(height);

        // The encoder is built on its own thread (VA-API handles are !Send).
        // spawn() blocks until construction succeeds or fails, so a failure
        // falls back to software here rather than mid-stream.
        let thread = crate::egfx::hardware::HardwareEncoderThread::spawn(hw_config, w32, h32)
            .map_err(EncoderError::InitFailed)?;

        // Only VA-API implements the dual-view NV12 entry point today.
        if thread.backend_name() != "vaapi" {
            return Err(EncoderError::InitFailed(format!(
                "hardware AVC444 requires the VA-API backend, got {}",
                thread.backend_name()
            )));
        }

        let color_space = config.color_space.unwrap_or({
            match (config.width, config.height) {
                (Some(w), Some(h)) if w >= 1280 && h >= 720 => ColorSpaceConfig::BT709_FULL,
                (Some(_), Some(_)) => ColorSpaceConfig::BT601_LIMITED,
                _ => ColorSpaceConfig::BT709_FULL,
            }
        });
        let color_matrix = color_space.matrix;
        let level = config
            .width
            .zip(config.height)
            .map(|(w, h)| super::h264_level::H264Level::for_config(w, h, config.max_fps));

        info!(
            "AVC444: VA-API hardware encoder at {}×{}, QP [{}, {}]",
            width, height, config.qp_min, config.qp_max
        );

        Ok(Self::assemble(
            config,
            color_space,
            color_matrix,
            level,
            Avc444Backend::Hardware(HardwareBackendProxy {
                thread,
                aligned_width: w32,
                aligned_height: h32,
                pending_idr: false,
            }),
        ))
    }

    /// Attach encoder diagnostics. Called once after construction by the
    /// display handler when the operator has enabled diagnostics in config.
    /// Name of the active encode backend, for telemetry.
    pub fn backend_name(&self) -> &'static str {
        match &self.backend {
            Avc444Backend::Software(_) => "openh264",
            #[cfg(feature = "vaapi")]
            Avc444Backend::Hardware(_) => "vaapi",
        }
    }

    /// Mirrors `Avc420Encoder::set_diagnostics` so both encoders share the
    /// same opt-in surface from the caller's perspective.
    pub fn set_diagnostics(
        &mut self,
        diagnostics: Option<std::sync::Arc<super::encode_diagnostics::EncodeDiagnostics>>,
    ) {
        self.diagnostics = diagnostics;
    }

    /// Create encoder with specific color matrix
    ///
    /// **Deprecated**: Use `EncoderConfig::with_color_space()` instead for full VUI support.
    /// This method only affects the conversion matrix, not VUI signaling.
    #[deprecated(note = "Use EncoderConfig::with_color_space() for full VUI support")]
    pub fn with_color_matrix(config: EncoderConfig, matrix: ColorMatrix) -> EncoderResult<Self> {
        let mut encoder = Self::new(config)?;
        encoder.color_matrix = matrix;
        Ok(encoder)
    }

    pub fn with_color_space(
        mut config: EncoderConfig,
        color_space: ColorSpaceConfig,
    ) -> EncoderResult<Self> {
        config.color_space = Some(color_space);
        Self::new(config)
    }

    /// Configure Phase 1 auxiliary omission parameters
    ///
    /// Call this after `new()` to apply configuration from EgfxConfig.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let mut encoder = Avc444Encoder::new(config)?;
    /// encoder.configure_aux_omission(true, 30, 0.05, true);
    /// ```
    pub fn configure_aux_omission(
        &mut self,
        enable: bool,
        max_interval: u32,
        change_threshold: f32,
        force_idr_on_return: bool,
    ) {
        self.enable_aux_omission = enable;
        self.max_aux_interval = max_interval.clamp(1, 120);
        self.aux_change_threshold = change_threshold.clamp(0.0, 1.0);
        self.force_aux_idr_on_return = force_idr_on_return;

        debug!(
            "AVC444 aux omission configured: enabled={}, max_interval={}, threshold={:.2}, force_idr={}",
            enable, self.max_aux_interval, self.aux_change_threshold, force_idr_on_return
        );

        if enable {
            info!(
                "🎬 Phase 1 AUX OMISSION ENABLED: max_interval={}frames, force_idr_on_return={}",
                self.max_aux_interval, force_idr_on_return
            );
        }
    }

    /// Shrink the aux-omission interval temporarily for stress recovery.
    ///
    /// AVC444's aux-stream omission saves bandwidth when consecutive frames
    /// have similar chroma — by default we omit aux for up to `max_aux_interval`
    /// frames. Under load this delays full-chroma refresh, increasing the
    /// window during which a decoder error in main can persist.
    ///
    /// Under detected stress (called from the display loop's stress-IDR
    /// trigger path), shrink the effective interval so aux refreshes more
    /// frequently. The cap is the original configured value — we never
    /// extend, only shrink.
    ///
    /// Returns the previous interval so the caller can restore later.
    pub fn set_aux_max_interval(&mut self, max_interval: u32) -> u32 {
        let prev = self.max_aux_interval;
        self.max_aux_interval = max_interval.clamp(1, 120);
        if self.max_aux_interval != prev {
            debug!(
                from = prev,
                to = self.max_aux_interval,
                "AVC444: aux max_interval changed"
            );
        }
        prev
    }

    /// Current aux-omission interval (frames). Used by stress logic to decide
    /// whether to shrink or restore.
    pub fn aux_max_interval(&self) -> u32 {
        self.max_aux_interval
    }

    /// Elapsed time since the last IDR, in milliseconds.
    ///
    /// Used by display-loop stress detection to decide whether an early-IDR
    /// request is worthwhile (vs. one already pending or just-emitted).
    pub fn ms_since_last_idr(&self) -> u64 {
        self.last_idr_time.elapsed().as_millis() as u64
    }

    /// Configure periodic IDR keyframe insertion
    ///
    /// Forces a full IDR keyframe at regular intervals to clear accumulated
    /// compression artifacts. This is especially important for VDI where
    /// artifacts from window movement can persist.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let mut encoder = Avc444Encoder::new(config)?;
    /// encoder.configure_periodic_idr(5); // Force IDR every 5 seconds
    /// ```
    pub fn configure_periodic_idr(&mut self, interval_secs: u32) {
        self.periodic_idr_interval_secs = interval_secs;
        self.last_idr_time = std::time::Instant::now();

        if interval_secs > 0 {
            info!(
                "🎬 Periodic IDR ENABLED: interval={}s (clears artifacts automatically)",
                interval_secs
            );
        } else {
            debug!("Periodic IDR disabled");
        }
    }

    /// Request immediate IDR keyframe (for client PLI - Picture Loss Indication)
    ///
    /// Called when the client reports visual artifacts or packet loss.
    /// The next encoded frame will be a full IDR keyframe.
    pub fn request_idr(&mut self) {
        self.force_next_idr = true;
        debug!("IDR requested (PLI or manual trigger)");
    }

    /// Check if periodic IDR is due (non-consuming check)
    ///
    /// This allows callers to know if the next encode will trigger a periodic IDR
    /// WITHOUT actually triggering it. Useful for forcing full-frame damage when
    /// periodic IDR is about to fire, ensuring the entire screen gets refreshed.
    pub fn is_periodic_idr_due(&self) -> bool {
        if self.force_next_idr {
            return true;
        }
        if self.periodic_idr_interval_secs > 0 {
            let elapsed = self.last_idr_time.elapsed();
            return elapsed
                >= std::time::Duration::from_secs(self.periodic_idr_interval_secs as u64);
        }
        false
    }

    /// Check if we should force an IDR frame due to periodic interval or PLI
    ///
    /// When this returns true, it also sets `force_aux_on_next_frame` to ensure
    /// BOTH streams (Main + Aux) get refreshed. This is critical for clearing
    /// artifacts - if we only IDR the Main stream while omitting Aux, the client
    /// reuses its cached aux which may contain artifacts.
    fn should_force_idr(&mut self) -> bool {
        // Check PLI request first
        if self.force_next_idr {
            self.force_next_idr = false;
            self.force_aux_on_next_frame = true; // Force aux to clear ALL artifacts
            self.last_idr_time = std::time::Instant::now();
            info!("Forcing IDR (client PLI request) - both Main and Aux will refresh");
            return true;
        }

        // Check periodic interval. The timer is GATED on a frame arriving at
        // the encoder — if the pipeline is backpressured or PipeWire pauses,
        // the next "fire" only happens when the next frame comes through.
        // Log the delta between configured interval and actual elapsed so
        // operators can tell "5s timer fired late" from "5s timer broken."
        if self.periodic_idr_interval_secs > 0 {
            let elapsed = self.last_idr_time.elapsed();
            let target = std::time::Duration::from_secs(self.periodic_idr_interval_secs as u64);
            if elapsed >= target {
                let drift_ms = elapsed.saturating_sub(target).as_millis();
                self.last_idr_time = std::time::Instant::now();
                self.force_aux_on_next_frame = true; // Force aux to clear ALL artifacts
                info!(
                    "Forcing periodic IDR (elapsed={}ms, target={}ms, drift={}ms over schedule) — BOTH Main and Aux refreshing",
                    elapsed.as_millis(),
                    target.as_millis(),
                    drift_ms,
                );
                return true;
            }
        }

        false
    }

    #[expect(clippy::unwrap_used, reason = "timing subtraction is always valid")]
    pub fn encode_bgra(
        &mut self,
        bgra_data: &[u8],
        width: u32,
        height: u32,
        timestamp_ms: u64,
    ) -> EncoderResult<Option<Avc444Frame>> {
        let start = std::time::Instant::now();

        // Validate dimensions
        if width == 0 || height == 0 || !width.is_multiple_of(2) || !height.is_multiple_of(2) {
            return Err(EncoderError::InvalidDimensions { width, height });
        }

        let expected_size = (width * height * 4) as usize;
        if bgra_data.len() < expected_size {
            return Err(EncoderError::EncodeFailed(format!(
                "BGRA buffer too small: {} < {}",
                bgra_data.len(),
                expected_size
            )));
        }

        // Step 1: BGRA → YUV444
        let yuv444 = bgra_to_yuv444(
            bgra_data,
            width as usize,
            height as usize,
            self.color_matrix,
        );
        let convert_time = start.elapsed();

        // Step 2: YUV444 → Dual YUV420
        let (main_yuv420, aux_yuv420) = pack_dual_views(&yuv444);
        let pack_time = start.elapsed().checked_sub(convert_time).unwrap();

        // Step 3: Encode both views using direct YUV input via version-aware FFI
        //
        // NOTE: We use logical dimensions (width x height) not actual buffer dimensions.
        // The YUV420 frames may have padded chroma planes for macroblock alignment,
        // but OpenH264 expects logical dimensions and will do its own padding internally.
        let (w, h) = (width as i32, height as i32);

        // === IDR DECISION FOR MAIN (periodic/PLI, or diagnostic all-keyframes) ===
        // should_force_idr() is called exactly once (it advances the periodic
        // timer); OR in the diagnostic all-keyframes flag.
        let force_main_idr = self.should_force_idr() || self.force_all_keyframes;
        if self.force_all_keyframes && self.frame_count == 0 {
            debug!("DIAGNOSTIC: force_all_keyframes=true - All frames will be IDR");
        }

        // === SINGLE ENCODER: SEQUENTIAL ENCODING (FreeRDP Pattern) ===
        //
        // MS-RDPEGFX Section 3.3.8.3.2: "The two subframe bitstreams MUST be
        // encoded using the same H.264 encoder". Main is encoded first and
        // updates the unified DPB that Aux then references. This holds for both
        // backends: the OpenH264 single encoder and a single VA-API encoder.
        //
        // 1. Encode Main subframe -> Updates unified DPB
        // 2. Encode Aux subframe -> SAME DPB, sequential call

        // Encode Main subframe FIRST (luma + subsampled chroma)
        let (stream1_data, main_is_keyframe) =
            self.backend
                .encode_view(&main_yuv420, w, h, timestamp_ms as i64, force_main_idr)?;
        let main_frame_type_str = if main_is_keyframe { "IDR" } else { "P" };

        // === PHASE 1: AUXILIARY STREAM (CONDITIONALLY ENCODED) ===
        //
        // CRITICAL IMPLEMENTATION: "Don't encode what you don't send"
        //
        // This is the FreeRDP-proven bandwidth optimization pattern.
        // If aux hasn't changed, we:
        // 1. Don't encode it at all (skip encoder call entirely)
        // 2. Send LC=1 (luma only) to client
        // 3. Client reuses previous aux from its cache
        //
        // This keeps encoder and decoder DPB timelines synchronized!
        //
        // Why this matters:
        // - If we encoded but didn't send: Encoder DPB contains frame decoder never saw
        // - Next aux P-frame would reference missing frame → corruption
        // - By not encoding: Both DPBs stay perfectly in sync

        let should_send_aux = self.should_send_aux(&aux_yuv420, main_is_keyframe);

        // Only meaningful when the aux subframe is actually sent.
        let mut aux_is_keyframe = false;
        let stream2_data_opt = if should_send_aux {
            // Force aux IDR to stay in lockstep with a main IDR (both streams
            // refresh together, clearing artifacts), or when reintroducing aux
            // after omission so it does not reference frames the decoder evicted.
            let force_aux_idr = if main_is_keyframe {
                debug!("Forcing aux IDR to sync with main IDR (artifact clearing)");
                true
            } else if self.force_aux_idr_on_return && self.frames_since_aux > 0 {
                debug!(
                    "Forcing aux IDR on reintroduction (omitted for {} frames)",
                    self.frames_since_aux
                );
                true
            } else {
                false
            };

            // Encode Aux subframe SECOND with the SAME encoder (sequential
            // call), maintaining the unified DPB shared with Main.
            let (aux_data, aux_kf) =
                self.backend
                    .encode_view(&aux_yuv420, w, h, timestamp_ms as i64, force_aux_idr)?;

            // A zero-byte result means the encoder skipped the frame (rate
            // control, common right after a main IDR). Treat it as omitted so
            // encoder and decoder DPBs stay in sync (client keeps cached aux).
            if aux_data.is_empty() {
                trace!("Aux encoder skipped frame (rate control) - treating as omitted");
                self.frames_since_aux += 1;
                None
            } else {
                aux_is_keyframe = aux_kf;
                self.last_aux_hash = Some(Self::hash_yuv420(&aux_yuv420));
                self.frames_since_aux = 0;
                Some(aux_data)
            }
        } else {
            // === AUX OMITTED: Don't encode at all! ===
            // This keeps DPB synchronized with decoder
            // Client will reuse previous aux (LC=1 behavior)
            self.frames_since_aux += 1;
            None
        };

        let encode_time = start
            .elapsed()
            .checked_sub(convert_time)
            .and_then(|d| d.checked_sub(pack_time))
            .unwrap_or_default();

        // Handle empty main bitstream (encoder skip)
        if stream1_data.is_empty() {
            trace!("AVC444 encoder skipped frame (main stream empty)");
            return Ok(None);
        }

        // Diagnostic logging for bandwidth analysis
        if let Some(ref aux_data) = stream2_data_opt {
            let aux_type_str = if aux_is_keyframe { "IDR" } else { "P" };
            debug!(
                "[AVC444 Frame #{}] Main: {} ({}B), Aux: {} ({}B) [BOTH SENT]",
                self.frame_count,
                main_frame_type_str,
                stream1_data.len(),
                aux_type_str,
                aux_data.len()
            );
        } else {
            debug!(
                "[AVC444 Frame #{}] Main: {} ({}B), Aux: OMITTED (LC=1) [BANDWIDTH SAVE]",
                self.frame_count,
                main_frame_type_str,
                stream1_data.len()
            );
        }

        // Option A — always-on per-NAL hex dump at TRACE. Main and Aux both
        // get dumped so SPS/PPS differences between the two are visible.
        super::encode_diagnostics::log_nal_hex_dump(
            stream1_data.as_slice(),
            self.frame_count,
            "AVC444-Main",
        );
        if let Some(ref aux_data) = stream2_data_opt {
            super::encode_diagnostics::log_nal_hex_dump(aux_data, self.frame_count, "AVC444-Aux");
        }

        // Options B + D — config-gated dump file + decoder self-test. Apply
        // ONLY to the Main stream: it carries the luma + half chroma and is
        // a structurally valid H.264 bitstream on its own. The Aux stream
        // carries the remaining chroma planes packed as luma — also valid
        // H.264 syntactically, so we self-test it too for parser sanity,
        // but we DO NOT dump it to the same file (would interleave two
        // unrelated streams). If a separate Aux dump is wanted later, add a
        // second config field.
        if let Some(d) = &self.diagnostics {
            d.dump_frame(stream1_data.as_slice());
            d.self_test(stream1_data.as_slice(), "AVC444-Main");
            if let Some(ref aux_data) = stream2_data_opt {
                d.self_test(aux_data, "AVC444-Aux");
            }
        }

        // SPS/PPS handling: IDR frames contain SPS/PPS as part of Annex B bitstream.
        // Do NOT prepend SPS/PPS to P-frames: MSTSC's MFT decoder interprets
        // unexpected SPS/PPS as parameter changes requiring reinitialization,
        // causing DVC Close during rapid frame sequences (window drag).
        // Aux stream also keeps its own SPS/PPS intact per ITU-H.264 Annex B.
        // See docs/bugs/AVC444-V140-REGRESSION.md for full analysis.

        // Update statistics
        self.frame_count += 1;
        let total_size =
            stream1_data.len() + stream2_data_opt.as_ref().map_or(0, std::vec::Vec::len);
        self.bytes_encoded += total_size as u64;

        let total_time = start.elapsed();
        self.total_encode_time_ms += total_time.as_secs_f64() * 1000.0;

        let timing = Avc444Timing {
            color_convert_ms: convert_time.as_secs_f32() * 1000.0,
            packing_ms: pack_time.as_secs_f32() * 1000.0,
            encoding_ms: encode_time.as_secs_f32() * 1000.0,
            total_ms: total_time.as_secs_f32() * 1000.0,
        };

        // Periodic logging with omission statistics
        if self.frame_count.is_multiple_of(30) {
            let aux_size_display = stream2_data_opt.as_ref().map_or(0, std::vec::Vec::len);
            let omission_status = if stream2_data_opt.is_some() {
                "sent"
            } else {
                "omitted"
            };

            debug!(
                "AVC444 frame {}: {}×{} → {}b (main: {}b, aux: {}b [{}]) in {:.1}ms",
                self.frame_count,
                width,
                height,
                total_size,
                stream1_data.len(),
                aux_size_display,
                omission_status,
                timing.total_ms,
            );
        }

        Ok(Some(Avc444Frame {
            stream1_data,
            stream2_data: stream2_data_opt, // Now Option<Vec<u8>>
            is_keyframe: main_is_keyframe,
            timestamp_ms,
            total_size,
            timing,
        }))
    }

    /// Handle SPS/PPS for main stream (cache on IDR, prepend on P-frame)
    ///
    /// Force next frame to be a keyframe (IDR) in both subframes
    ///
    /// With single encoder, this affects the next encode() call.
    /// Since we encode Main first, then Aux, this will make both IDR.
    pub fn force_keyframe(&mut self) {
        self.backend.force_keyframe();
        debug!("Forced keyframe for next encode (affects both Main and Aux)");
    }

    /// Compute fast hash of YUV420 frame for change detection
    ///
    /// Uses sampled hashing for performance:
    /// - Samples every 16th pixel (reduces 1M pixels to ~4K samples for 1280x800)
    /// - Hashes Y plane only (luma carries most visual information)
    /// - Uses Rust's DefaultHasher (fast, non-cryptographic)
    ///
    /// # Performance
    ///
    /// - 1080p: ~0.5ms
    /// - 1440p: ~0.8ms
    /// - 4K: ~1.5ms
    fn hash_yuv420(frame: &super::yuv444_packing::Yuv420Frame) -> u64 {
        use std::{
            collections::hash_map::DefaultHasher,
            hash::{Hash, Hasher},
        };

        let mut hasher = DefaultHasher::new();

        // Sample every 16th pixel from Y plane for performance
        // For 1280x800: 1,024,000 pixels → 4,000 samples
        // For 1920x1080: 2,073,600 pixels → 8,100 samples
        const SAMPLE_STRIDE: usize = 16;
        const MAX_SAMPLES: usize = 8192; // Cap at 8K samples even for 4K displays

        let y_plane = frame.y_plane();
        let sample_count = (y_plane.len() / SAMPLE_STRIDE).min(MAX_SAMPLES);

        for i in 0..sample_count {
            let idx = i * SAMPLE_STRIDE;
            if idx < y_plane.len() {
                y_plane[idx].hash(&mut hasher);
            }
        }

        hasher.finish()
    }

    /// Determine if auxiliary stream should be encoded and sent
    ///
    /// Implements FreeRDP-style change detection with configurable thresholds.
    ///
    /// # Decision Logic
    ///
    /// Aux is sent when ANY of these conditions are true:
    /// 1. **Omission disabled** - always send (backward compatible)
    /// 2. **First aux frame** - initial frame always sent
    /// 3. **Forced refresh** - exceeded max_aux_interval
    /// 4. **Content changed** - hash differs from previous aux
    ///
    /// # CRITICAL: Main IDR does NOT trigger aux send!
    ///
    /// Previously, we sent aux whenever main was IDR ("sync required"). This
    /// created a FEEDBACK LOOP that prevented P-frames:
    ///
    /// 1. Main IDR → send aux → DPB contains Aux
    /// 2. Next Main references Aux (different content) → forced IDR
    /// 3. Main IDR → send aux → DPB contains Aux
    /// 4. ... loop continues indefinitely
    ///
    /// By NOT sending aux on Main IDR, we break this loop:
    /// 1. Aux refresh (max_interval) → Main becomes IDR (unavoidable)
    /// 2. Next Main: we DON'T send aux → DPB = Main
    /// 3. Next Main: references Main → P-frame works!
    ///
    /// The client handles Main IDR + cached aux correctly (LC=1 mode).
    #[expect(
        clippy::unwrap_used,
        reason = "last_aux_hash is checked for Some before unwrap"
    )]
    fn should_send_aux(
        &mut self, // Changed to &mut self to clear force flag
        aux_frame: &super::yuv444_packing::Yuv420Frame,
        _main_is_keyframe: bool, // IGNORED: See feedback loop documentation above
    ) -> bool {
        // CRITICAL: Forced aux for artifact clearing (periodic IDR or PLI)
        // This bypasses ALL omission logic to ensure client gets fresh aux stream
        if self.force_aux_on_next_frame {
            self.force_aux_on_next_frame = false; // Consume the flag
            info!("Sending aux: FORCED for artifact clearing (bypassing omission)");
            return true;
        }

        // If omission disabled, always send (backward compatible behavior)
        if !self.enable_aux_omission {
            return true;
        }

        // REMOVED: "main_is_keyframe → send aux" rule
        // This was causing a feedback loop that prevented ALL P-frames!
        // See docs/AVC444-AUX-OMISSION-CRITICAL-FINDING.md for details.

        // First aux frame must always be sent
        if self.last_aux_hash.is_none() {
            trace!("Sending aux: first frame");
            return true;
        }

        // Enforce maximum interval (forced refresh for quality)
        if self.frames_since_aux >= self.max_aux_interval {
            debug!(
                "Sending aux: forced refresh ({} frames since last, max={})",
                self.frames_since_aux, self.max_aux_interval
            );
            return true;
        }

        // CRITICAL: Enforce MINIMUM interval between aux sends to prevent DPB pollution!
        // Without this, rapid content changes cause aux on every frame → feedback loop.
        // This ensures Main stream has time to establish P-frame chains between aux refreshes.
        const MIN_AUX_INTERVAL: u32 = 10; // At least 10 frames between aux sends
        if self.frames_since_aux < MIN_AUX_INTERVAL {
            trace!(
                "Skipping aux: rate limited ({} frames since last, min={})",
                self.frames_since_aux, MIN_AUX_INTERVAL
            );
            return false;
        }

        // Check if aux content has changed
        let current_hash = Self::hash_yuv420(aux_frame);
        let previous_hash = self.last_aux_hash.unwrap(); // Safe: checked above

        // Simple hash comparison for Phase 1
        // Phase 2 will add threshold-based pixel difference counting
        let changed = current_hash != previous_hash;

        if changed {
            trace!("Sending aux: content changed (hash mismatch)");
        } else {
            trace!(
                "Skipping aux: no change detected (frame {} since last)",
                self.frames_since_aux
            );
        }

        changed
    }

    pub fn stats(&self) -> Avc444Stats {
        Avc444Stats {
            frames_encoded: self.frame_count,
            bytes_encoded: self.bytes_encoded,
            avg_encode_time_ms: if self.frame_count > 0 {
                (self.total_encode_time_ms / self.frame_count as f64) as f32
            } else {
                0.0
            },
            bitrate_kbps: self.config.bitrate_kbps * 2, // Two streams
            color_matrix: self.color_matrix,
        }
    }

    pub fn color_matrix(&self) -> ColorMatrix {
        self.color_matrix
    }

    pub fn color_space(&self) -> &ColorSpaceConfig {
        &self.color_space
    }

    pub fn level(&self) -> Option<super::h264_level::H264Level> {
        self.current_level
    }
}

// Stub implementation when h264 feature is disabled
#[cfg(not(feature = "h264"))]
pub struct Avc444Encoder;

#[cfg(not(feature = "h264"))]
impl Avc444Encoder {
    pub fn new(_config: EncoderConfig) -> EncoderResult<Self> {
        Err(EncoderError::FeatureDisabled)
    }

    pub fn encode_bgra(
        &mut self,
        _bgra_data: &[u8],
        _width: u32,
        _height: u32,
        _timestamp_ms: u64,
    ) -> EncoderResult<Option<Avc444Frame>> {
        Err(EncoderError::FeatureDisabled)
    }

    pub fn force_keyframe(&mut self) {}

    pub fn stats(&self) -> Avc444Stats {
        Avc444Stats {
            frames_encoded: 0,
            bytes_encoded: 0,
            avg_encode_time_ms: 0.0,
            bitrate_kbps: 0,
            color_matrix: ColorMatrix::BT709,
        }
    }

    pub fn color_matrix(&self) -> ColorMatrix {
        ColorMatrix::BT709
    }

    pub fn color_space(&self) -> &ColorSpaceConfig {
        // Return a static reference for the stub
        &ColorSpaceConfig::BT709_FULL
    }

    pub fn level(&self) -> Option<super::h264_level::H264Level> {
        None
    }

    pub fn configure_aux_omission(
        &mut self,
        _enable: bool,
        _max_interval: u32,
        _change_threshold: f32,
        _force_idr_on_return: bool,
    ) {
    }
    pub fn request_idr(&mut self) {}
    pub fn is_periodic_idr_due(&self) -> bool {
        false
    }
    pub fn configure_periodic_idr(&mut self, _interval_frames: u32) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Try to create an encoder, returning early if OpenH264 library is not installed.
    /// Tests that require the library call this instead of unwrap() so CI environments
    /// without the Cisco binary skip gracefully rather than failing.
    #[cfg(feature = "h264")]
    macro_rules! require_openh264 {
        ($expr:expr) => {
            match $expr {
                Ok(enc) => enc,
                Err(crate::egfx::encoder::EncoderError::InitFailed(_)) => return,
                Err(e) => panic!("unexpected encoder error: {e:?}"),
            }
        };
    }

    #[test]
    #[expect(clippy::float_cmp, reason = "comparing against zero defaults")]
    fn test_avc444_timing_default() {
        let timing = Avc444Timing::default();
        assert_eq!(timing.color_convert_ms, 0.0);
        assert_eq!(timing.packing_ms, 0.0);
        assert_eq!(timing.encoding_ms, 0.0);
        assert_eq!(timing.total_ms, 0.0);
    }

    #[test]
    fn test_avc444_stats_default() {
        let stats = Avc444Stats {
            frames_encoded: 0,
            bytes_encoded: 0,
            avg_encode_time_ms: 0.0,
            bitrate_kbps: 5000,
            color_matrix: ColorMatrix::BT709,
        };
        assert_eq!(stats.frames_encoded, 0);
    }

    #[cfg(feature = "h264")]
    #[test]
    fn test_avc444_encoder_creation() {
        let config = EncoderConfig::default();
        let _encoder = require_openh264!(Avc444Encoder::new(config));
    }

    #[cfg(feature = "h264")]
    #[test]
    fn test_avc444_encoder_with_resolution() {
        let config = EncoderConfig {
            width: Some(1920),
            height: Some(1080),
            ..Default::default()
        };
        let encoder = require_openh264!(Avc444Encoder::new(config));
        assert_eq!(encoder.color_matrix(), ColorMatrix::BT709);
    }

    #[cfg(feature = "h264")]
    #[test]
    fn test_avc444_encoder_sd_resolution() {
        let config = EncoderConfig {
            width: Some(640),
            height: Some(480),
            ..Default::default()
        };
        let encoder = require_openh264!(Avc444Encoder::new(config));
        assert_eq!(encoder.color_matrix(), ColorMatrix::BT601);
    }

    #[cfg(feature = "h264")]
    #[test]
    fn test_encode_black_frame() {
        let config = EncoderConfig::default();
        let mut encoder = require_openh264!(Avc444Encoder::new(config));

        let width = 64u32;
        let height = 64u32;
        let bgra_data = vec![0u8; (width * height * 4) as usize];

        let result = encoder.encode_bgra(&bgra_data, width, height, 0);
        assert!(result.is_ok(), "Encoding failed: {:?}", result.err());

        if let Ok(Some(frame)) = result {
            assert!(!frame.stream1_data.is_empty(), "Stream 1 is empty");
            // stream2_data is Option<Vec<u8>> - may be None with aux omission
            if let Some(ref stream2) = frame.stream2_data {
                assert!(!stream2.is_empty(), "Stream 2 is empty");
                assert_eq!(frame.total_size, frame.stream1_data.len() + stream2.len());
            }
        }
    }

    #[cfg(feature = "h264")]
    #[test]
    fn test_encode_colored_frame() {
        let config = EncoderConfig::default();
        let mut encoder = require_openh264!(Avc444Encoder::new(config));

        let width = 64u32;
        let height = 64u32;
        let mut bgra_data = vec![0u8; (width * height * 4) as usize];

        // Create a gradient pattern
        for y in 0..height {
            for x in 0..width {
                let idx = ((y * width + x) * 4) as usize;
                bgra_data[idx] = ((x * 4) % 256) as u8; // B
                bgra_data[idx + 1] = ((y * 4) % 256) as u8; // G
                bgra_data[idx + 2] = 128; // R
                bgra_data[idx + 3] = 255; // A
            }
        }

        let result = encoder.encode_bgra(&bgra_data, width, height, 0);
        assert!(result.is_ok());
    }

    #[cfg(feature = "h264")]
    #[test]
    fn test_invalid_dimensions() {
        let config = EncoderConfig::default();
        let mut encoder = require_openh264!(Avc444Encoder::new(config));

        // Odd width
        let bgra_data = vec![0u8; 63 * 64 * 4];
        let result = encoder.encode_bgra(&bgra_data, 63, 64, 0);
        assert!(matches!(
            result,
            Err(EncoderError::InvalidDimensions { .. })
        ));

        // Odd height
        let bgra_data = vec![0u8; 64 * 63 * 4];
        let result = encoder.encode_bgra(&bgra_data, 64, 63, 0);
        assert!(matches!(
            result,
            Err(EncoderError::InvalidDimensions { .. })
        ));

        // Zero dimension
        let result = encoder.encode_bgra(&[], 0, 64, 0);
        assert!(matches!(
            result,
            Err(EncoderError::InvalidDimensions { .. })
        ));
    }

    #[cfg(feature = "h264")]
    #[test]
    fn test_buffer_too_small() {
        let config = EncoderConfig::default();
        let mut encoder = require_openh264!(Avc444Encoder::new(config));

        // Buffer smaller than expected
        let bgra_data = vec![0u8; 64 * 32 * 4]; // Only half the expected size
        let result = encoder.encode_bgra(&bgra_data, 64, 64, 0);
        assert!(matches!(result, Err(EncoderError::EncodeFailed(_))));
    }

    #[cfg(feature = "h264")]
    #[test]
    fn test_force_keyframe() {
        let config = EncoderConfig::default();
        let mut encoder = require_openh264!(Avc444Encoder::new(config));

        // Should not panic
        encoder.force_keyframe();
    }

    #[cfg(feature = "h264")]
    #[test]
    fn test_stats() {
        let config = EncoderConfig {
            bitrate_kbps: 5000,
            ..Default::default()
        };
        let encoder = require_openh264!(Avc444Encoder::new(config));
        let stats = encoder.stats();

        assert_eq!(stats.frames_encoded, 0);
        assert_eq!(stats.bytes_encoded, 0);
        assert_eq!(stats.bitrate_kbps, 10000); // 2× for dual streams
    }

    #[cfg(feature = "h264")]
    #[test]
    fn test_multiple_frames() {
        let config = EncoderConfig::default();
        let mut encoder = require_openh264!(Avc444Encoder::new(config));

        let width = 64u32;
        let height = 64u32;
        let bgra_data = vec![128u8; (width * height * 4) as usize];

        // Encode multiple frames
        for i in 0..5 {
            let result = encoder.encode_bgra(&bgra_data, width, height, i * 33);
            assert!(result.is_ok(), "Frame {} failed: {:?}", i, result.err());
        }

        let stats = encoder.stats();
        assert!(stats.frames_encoded >= 1, "No frames encoded");
    }

    #[cfg(feature = "h264")]
    #[test]
    fn test_variable_frame_sizes() {
        // Test that encoder handles different frame sizes correctly
        let config = EncoderConfig::default();
        let mut encoder = require_openh264!(Avc444Encoder::new(config));

        let test_sizes = [(64, 64), (128, 128), (256, 256), (320, 240)];

        for (width, height) in test_sizes {
            let bgra_data = vec![128u8; (width * height * 4) as usize];
            let result = encoder.encode_bgra(&bgra_data, width as u32, height as u32, 0);
            assert!(
                result.is_ok(),
                "Encoding {}×{} failed: {:?}",
                width,
                height,
                result.err()
            );
        }
    }

    #[test]
    fn test_avc444_frame_debug() {
        // Test that Avc444Frame derives Debug
        let frame = Avc444Frame {
            stream1_data: vec![1, 2, 3],
            stream2_data: Some(vec![4, 5, 6]), // Option<Vec<u8>> for aux omission
            is_keyframe: true,
            timestamp_ms: 100,
            total_size: 6,
            timing: Avc444Timing::default(),
        };
        let debug_str = format!("{frame:?}");
        assert!(debug_str.contains("Avc444Frame"));
        assert!(debug_str.contains("is_keyframe: true"));
    }

    #[test]
    #[expect(clippy::float_cmp, reason = "comparing cloned float literals")]
    fn test_avc444_timing_clone() {
        let timing = Avc444Timing {
            color_convert_ms: 1.5,
            packing_ms: 2.5,
            encoding_ms: 10.0,
            total_ms: 14.0,
        };
        let cloned = timing.clone();
        assert_eq!(cloned.color_convert_ms, 1.5);
        assert_eq!(cloned.packing_ms, 2.5);
        assert_eq!(cloned.encoding_ms, 10.0);
        assert_eq!(cloned.total_ms, 14.0);
    }

    #[test]
    #[expect(clippy::float_cmp, reason = "comparing cloned float literals")]
    fn test_avc444_stats_clone() {
        let stats = Avc444Stats {
            frames_encoded: 100,
            bytes_encoded: 50000,
            avg_encode_time_ms: 15.5,
            bitrate_kbps: 5000,
            color_matrix: ColorMatrix::BT709,
        };
        let cloned = stats.clone();
        assert_eq!(cloned.frames_encoded, 100);
        assert_eq!(cloned.bytes_encoded, 50000);
        assert_eq!(cloned.avg_encode_time_ms, 15.5);
        assert_eq!(cloned.color_matrix, ColorMatrix::BT709);
    }
}
