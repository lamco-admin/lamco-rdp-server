//! H.264/AVC420 Encoder for EGFX
//!
//! This module provides H.264 encoding using OpenH264 for use with the
//! EGFX AVC420 codec. OpenH264 handles color conversion internally.
//!
//! # MS-RDPEGFX Compliance
//!
//! MS-RDPEGFX requires length-prefixed NAL units (AVC format per ISO/IEC 14496-15),
//! not Annex B format. OpenH264 outputs Annex B by default, so this module
//! automatically converts the bitstream to length-prefixed format.
//!
//! # OpenH264 Licensing
//!
//! OpenH264 is loaded dynamically at runtime via `libloading`. For patent
//! compliance, the loaded binary must be Cisco's precompiled release,
//! downloaded separately to the device. Compiling from source provides
//! zero patent coverage. See `docs/decisions/H264-CODEC-STRATEGY.md`.
//!
//! # Performance Notes
//!
//! - Hardware encoding (VAAPI/NVENC) is preferred over software OpenH264
//! - Target 30 FPS for typical desktop sharing scenarios

#[cfg(feature = "h264")]
use openh264::formats::{BgraSliceU8, YUVBuffer, YUVSource};
use thiserror::Error;
#[cfg(feature = "h264")]
use tracing::{debug, info, trace, warn};

use super::color_space::ColorSpaceConfig;
#[cfg(feature = "h264")]
use super::openh264_compat;

/// Errors that can occur during H.264 encoding
#[derive(Debug, Error)]
pub enum EncoderError {
    #[error("Encoder initialization failed: {0}")]
    InitFailed(String),

    #[error("Encoding failed: {0}")]
    EncodeFailed(String),

    #[error("Invalid frame dimensions: {width}x{height}")]
    InvalidDimensions { width: u32, height: u32 },

    #[error("H.264 feature not enabled")]
    FeatureDisabled,
}

/// Result type for encoder operations
pub type EncoderResult<T> = Result<T, EncoderError>;

/// Encoder configuration
#[derive(Debug, Clone)]
pub struct EncoderConfig {
    /// Target bitrate in kbps (default: 5000)
    pub bitrate_kbps: u32,

    /// Maximum frame rate (default: 30)
    pub max_fps: f32,

    /// Enable frame skipping for rate control (default: true)
    pub enable_skip_frame: bool,

    /// Resolution for level calculation (optional, auto-detected on first frame)
    pub width: Option<u16>,

    /// Resolution for level calculation (optional, auto-detected on first frame)
    pub height: Option<u16>,

    /// Color space configuration for VUI signaling and conversion matrix
    ///
    /// When set, the encoder will:
    /// 1. Use the specified color matrix for RGB→YUV conversion
    /// 2. Signal the color space via H.264 VUI (Video Usability Information)
    ///
    /// VUI ensures the decoder interprets colors correctly by embedding
    /// color primaries, transfer characteristics, and matrix coefficients
    /// in the SPS (Sequence Parameter Set).
    ///
    /// Default: None (uses OpenH264-compatible limited range for AVC420,
    /// BT.709 for AVC444)
    pub color_space: Option<ColorSpaceConfig>,

    /// Minimum QP value (default: 0, range 0-51)
    /// Lower = better quality, larger frames
    pub qp_min: u8,

    /// Maximum QP value (default: 51, range 0-51)
    /// Higher = worse quality, smaller frames
    pub qp_max: u8,

    /// Number of encoder threads (default: 0 = auto)
    /// OpenH264 will use this for slice-based parallelism
    /// 0 = auto-detect based on CPU cores
    /// 1 = single-threaded
    /// >1 = fixed number of threads
    pub encoder_threads: u16,
}

impl Default for EncoderConfig {
    fn default() -> Self {
        Self {
            bitrate_kbps: 5000,
            max_fps: 30.0,
            enable_skip_frame: true,
            width: None,
            height: None,
            color_space: None,  // Encoder-specific default
            qp_min: 0,          // OpenH264 default
            qp_max: 51,         // OpenH264 default
            encoder_threads: 0, // Auto-detect
        }
    }
}

impl EncoderConfig {
    pub fn for_resolution(width: u16, height: u16) -> Self {
        Self {
            width: Some(width),
            height: Some(height),
            ..Default::default()
        }
    }

    pub fn high_quality() -> Self {
        Self {
            bitrate_kbps: 10000,
            max_fps: 30.0,
            enable_skip_frame: false,
            qp_min: 10, // Better quality range
            qp_max: 25,
            ..Default::default()
        }
    }

    /// Create config for high performance mode (60fps)
    ///
    /// Optimized for powerful systems with hardware encoding:
    /// - 60 FPS for smooth motion
    /// - Higher bitrate to maintain quality at higher framerate
    /// - Requires VAAPI/NVENC for best results
    pub fn high_performance() -> Self {
        Self {
            bitrate_kbps: 8000,
            max_fps: 60.0,
            enable_skip_frame: true,
            ..Default::default()
        }
    }

    pub fn low_bandwidth() -> Self {
        Self {
            bitrate_kbps: 1000,
            max_fps: 15.0,
            enable_skip_frame: true,
            qp_min: 20, // Allow more compression
            qp_max: 45,
            ..Default::default()
        }
    }

    /// Set color space configuration
    ///
    /// This enables VUI signaling in the H.264 stream, ensuring
    /// decoders correctly interpret the color space.
    pub fn with_color_space(mut self, config: ColorSpaceConfig) -> Self {
        self.color_space = Some(config);
        self
    }
}

/// Convert H.264 Annex B format to AVC length-prefixed format
///
/// **DEPRECATED - DO NOT USE FOR MS-RDPEGFX!**
///
/// MS-RDPEGFX specification (Section 2.2.4.4) requires Annex B format (start codes),
/// NOT AVC format (length prefixes). This function was incorrectly used in the past.
///
/// OpenH264 outputs Annex B format, which should be used directly for RDP.
///
/// # Format Conversion (for reference only)
///
/// - **Annex B input**: `[0x00 0x00 0x00 0x01][NAL]` or `[0x00 0x00 0x01][NAL]`
/// - **AVC output**: `[4-byte big-endian length][NAL]`
///
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "kept as reference — MS-RDPEGFX uses Annex B directly"
    )
)]
#[deprecated(note = "MS-RDPEGFX requires Annex B, not AVC. Use Annex B format directly.")]
pub(super) fn annex_b_to_avc(annex_b_data: &[u8]) -> Vec<u8> {
    let mut output = Vec::with_capacity(annex_b_data.len());
    let mut i = 0;

    while i < annex_b_data.len() {
        // Find start code (0x00 0x00 0x01 or 0x00 0x00 0x00 0x01)
        let start_code_len = if i + 4 <= annex_b_data.len()
            && annex_b_data[i] == 0x00
            && annex_b_data[i + 1] == 0x00
            && annex_b_data[i + 2] == 0x00
            && annex_b_data[i + 3] == 0x01
        {
            4
        } else if i + 3 <= annex_b_data.len()
            && annex_b_data[i] == 0x00
            && annex_b_data[i + 1] == 0x00
            && annex_b_data[i + 2] == 0x01
        {
            3
        } else {
            // Not at a start code, skip byte
            i += 1;
            continue;
        };

        // Move past start code
        let nal_start = i + start_code_len;

        // Find next start code or end of data
        let mut nal_end = annex_b_data.len();
        let mut j = nal_start;
        while j + 3 <= annex_b_data.len() {
            if annex_b_data[j] == 0x00
                && annex_b_data[j + 1] == 0x00
                && (annex_b_data[j + 2] == 0x01
                    || (j + 3 < annex_b_data.len()
                        && annex_b_data[j + 2] == 0x00
                        && annex_b_data[j + 3] == 0x01))
            {
                nal_end = j;
                break;
            }
            j += 1;
        }

        // Extract NAL unit
        let nal_data = &annex_b_data[nal_start..nal_end];

        if !nal_data.is_empty() {
            // Write 4-byte big-endian length prefix
            let nal_len = nal_data.len() as u32;
            output.extend_from_slice(&nal_len.to_be_bytes());
            // Write NAL unit data
            output.extend_from_slice(nal_data);
        }

        i = nal_end;
    }

    output
}

/// Encoded H.264 frame
#[derive(Debug)]
pub struct H264Frame {
    /// Encoded NAL units (in AVC length-prefixed format)
    pub data: Vec<u8>,

    /// Whether this is a keyframe (IDR)
    pub is_keyframe: bool,

    /// Frame timestamp in milliseconds
    pub timestamp_ms: u64,

    /// Encoded frame size in bytes
    pub size: usize,
}

// Note: Avc420Region and create_avc420_bitmap_stream are provided by ironrdp-egfx
// See: ironrdp_egfx::pdu::Avc420Region and ironrdp_egfx::pdu::encode_avc420_bitmap_stream

/// Align dimension to multiple of 16 as required by MS-RDPEGFX
///
/// MS-RDPEGFX requires bitmap dimensions to be aligned to 16-pixel boundaries.
/// The encoded area is then cropped to the actual target region.
#[inline]
pub fn align_to_16(dimension: u32) -> u32 {
    (dimension + 15) & !15
}

/// H.264 encoder using OpenH264
///
/// # Feature Gate
///
/// Requires the `h264` feature to be enabled.
#[cfg(feature = "h264")]
pub struct Avc420Encoder {
    encoder: openh264_compat::VersionedEncoder,
    config: EncoderConfig,
    frame_count: u64,
    /// Cached SPS/PPS from last IDR frame (for prepending to P-slices)
    cached_sps_pps: Option<Vec<u8>>,
    /// Current H.264 level (determined from resolution)
    #[expect(dead_code, reason = "used when level-based bitrate scaling is enabled")]
    current_level: Option<super::h264_level::H264Level>,
}

/// Load the OpenH264 library with version detection.
///
/// Delegates to `openh264_compat::load_openh264()` which:
/// 1. Detects the runtime version via `WelsGetCodecVersion()`
/// 2. Selects the correct ABI generation (7 or 8)
/// 3. Returns an API handle with version-appropriate struct layouts
#[cfg(feature = "h264")]
pub(crate) fn load_openh264_api() -> EncoderResult<std::sync::Arc<openh264_compat::OpenH264Api>> {
    openh264_compat::load_openh264()
        .map(std::sync::Arc::new)
        .map_err(EncoderError::InitFailed)
}

#[cfg(feature = "h264")]
impl Avc420Encoder {
    /// Extract SPS and PPS NAL units from Annex B bitstream
    ///
    /// Returns concatenated SPS+PPS with start codes, or None if not found
    fn extract_sps_pps(data: &[u8]) -> Option<Vec<u8>> {
        let mut sps_pps = Vec::new();
        let mut i = 0;

        while i < data.len() {
            // Find start code
            let start_code_len =
                if i + 4 <= data.len() && data[i..i + 4] == [0x00, 0x00, 0x00, 0x01] {
                    4
                } else if i + 3 <= data.len() && data[i..i + 3] == [0x00, 0x00, 0x01] {
                    3
                } else {
                    i += 1;
                    continue;
                };

            let nal_start = i + start_code_len;
            if nal_start >= data.len() {
                break;
            }

            let nal_type = data[nal_start] & 0x1F;

            // Find next start code
            let mut nal_end = data.len();
            let mut j = nal_start + 1;
            while j + 2 < data.len() {
                if (data[j..j + 3] == [0x00, 0x00, 0x01])
                    || (j + 3 < data.len() && data[j..j + 4] == [0x00, 0x00, 0x00, 0x01])
                {
                    nal_end = j;
                    break;
                }
                j += 1;
            }

            // NAL type 7 = SPS, NAL type 8 = PPS
            if nal_type == 7 || nal_type == 8 {
                sps_pps.extend_from_slice(&data[i..nal_end]);
            }

            i = nal_end;
            if i == data.len() {
                break;
            }
        }

        if sps_pps.is_empty() {
            None
        } else {
            Some(sps_pps)
        }
    }

    /// Log detailed NAL unit structure for debugging
    fn log_nal_structure(data: &[u8], frame_num: u64, is_keyframe: bool) {
        let mut nal_types = Vec::new();
        let mut i = 0;

        while i < data.len() {
            // Find start code
            let start_code_len =
                if i + 4 <= data.len() && data[i..i + 4] == [0x00, 0x00, 0x00, 0x01] {
                    4
                } else if i + 3 <= data.len() && data[i..i + 3] == [0x00, 0x00, 0x01] {
                    3
                } else {
                    i += 1;
                    continue;
                };

            let nal_start = i + start_code_len;
            if nal_start >= data.len() {
                break;
            }

            let nal_header = data[nal_start];
            let nal_type = nal_header & 0x1F;
            let _ = (nal_header >> 5) & 0x03; // nal_ref_idc, parsed but unused

            // Find next start code
            let mut nal_end = data.len();
            let mut j = nal_start + 1;
            while j + 2 < data.len() {
                if (data[j..j + 3] == [0x00, 0x00, 0x01])
                    || (j + 3 < data.len() && data[j..j + 4] == [0x00, 0x00, 0x00, 0x01])
                {
                    nal_end = j;
                    break;
                }
                j += 1;
            }

            let nal_size = nal_end - nal_start;
            let type_name = match nal_type {
                1 => "P-slice",
                2 => "B-slice",
                5 => "IDR",
                6 => "SEI",
                7 => "SPS",
                8 => "PPS",
                9 => "AU-delim",
                _ => "Other",
            };

            nal_types.push(format!("{type_name}({nal_size}b)"));

            i = nal_end;
            if i == data.len() {
                break;
            }
        }

        debug!(
            "📦 Frame {}: {} | NALs: [{}] | Total: {}b",
            frame_num,
            if is_keyframe { "IDR" } else { "P" },
            nal_types.join(", "),
            data.len()
        );
    }

    pub fn new(config: EncoderConfig) -> EncoderResult<Self> {
        // Calculate appropriate H.264 level if dimensions provided
        let level = config
            .width
            .zip(config.height)
            .map(|(w, h)| super::h264_level::H264Level::for_config(w, h, config.max_fps));

        // Build version-aware encoder config
        let compat_config = openh264_compat::EncoderConfig {
            bitrate_bps: config.bitrate_kbps * 1000,
            max_frame_rate: config.max_fps,
            usage_type: openh264_compat::ffi_types::SCREEN_CONTENT_REAL_TIME,
            num_threads: config.encoder_threads,
            enable_skip_frame: config.enable_skip_frame,
            max_qp: config.qp_max as i32,
            min_qp: config.qp_min as i32,
            level_idc: level.map(|l| l.to_openh264_level_idc()),
            ..openh264_compat::EncoderConfig::default()
        };

        if let Some(level) = level {
            debug!(
                "Created H.264 encoder: bitrate={}kbps, max_fps={}, level={}",
                config.bitrate_kbps, config.max_fps, level
            );
        } else {
            debug!(
                "Created H.264 encoder: bitrate={}kbps, max_fps={} (level will be auto-detected)",
                config.bitrate_kbps, config.max_fps
            );
        }

        let api = load_openh264_api()?;
        info!("AVC420: {}", api.capabilities);
        let encoder = openh264_compat::VersionedEncoder::new(api, compat_config)
            .map_err(|e| EncoderError::InitFailed(format!("OpenH264 init failed: {e}")))?;

        Ok(Self {
            encoder,
            config,
            frame_count: 0,
            cached_sps_pps: None,
            current_level: level,
        })
    }

    pub fn encode_bgra(
        &mut self,
        bgra_data: &[u8],
        width: u32,
        height: u32,
        timestamp_ms: u64,
    ) -> EncoderResult<Option<H264Frame>> {
        // Validate dimensions (must be multiples of 2 for YUV420)
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

        // Color conversion: BGRA → YUV420 (pure Rust, no FFI)
        let bgra_source = BgraSliceU8::new(bgra_data, (width as usize, height as usize));
        let yuv = YUVBuffer::from_rgb_source(bgra_source);

        // Encode via version-aware FFI (uses correct struct layouts for detected ABI)
        let (w, h) = (width as usize, height as usize);
        let (y_stride, u_stride, v_stride) = yuv.strides();
        let encoded = self
            .encoder
            .encode(
                yuv.y(),
                yuv.u(),
                yuv.v(),
                y_stride as i32,
                u_stride as i32,
                v_stride as i32,
                w as i32,
                h as i32,
                timestamp_ms as i64,
            )
            .map_err(|e| EncoderError::EncodeFailed(format!("OpenH264 encode failed: {e}")))?;

        let annex_b_data = encoded.to_vec();
        if annex_b_data.is_empty() {
            return Ok(None);
        }

        let is_keyframe = encoded.is_keyframe();

        // MS-RDPEGFX requires Annex B format (ITU-H.264 Annex B with start codes)
        // OpenH264 outputs Annex B format directly - use it as-is!
        // CRITICAL: Do NOT convert to AVC format - Windows MFT decoder expects Annex B
        let mut data = annex_b_data;

        if data.is_empty() {
            warn!("Encoded bitstream is empty");
            return Ok(None);
        }

        // HYPOTHESIS 1 TEST: Extract and cache SPS/PPS from IDR frames, prepend to P-slices
        if is_keyframe {
            // IDR frame: Extract SPS/PPS for caching
            let sps_pps = Self::extract_sps_pps(&data);
            if let Some(ref headers) = sps_pps {
                debug!(
                    "🔑 IDR frame: Cached {} bytes of SPS/PPS headers",
                    headers.len()
                );
                self.cached_sps_pps = sps_pps;
            } else {
                warn!("⚠️ IDR frame without SPS/PPS headers!");
            }
        } else {
            // P-slice: Prepend cached SPS/PPS if available
            if let Some(ref sps_pps) = self.cached_sps_pps {
                debug!(
                    "📎 P-slice: Prepending {} bytes of cached SPS/PPS",
                    sps_pps.len()
                );
                let mut combined = sps_pps.clone();
                combined.extend_from_slice(&data);
                data = combined;
            } else {
                warn!("⚠️ P-slice without cached SPS/PPS - may fail on client!");
            }
        }

        self.frame_count += 1;

        // Log detailed NAL structure
        Self::log_nal_structure(&data, self.frame_count, is_keyframe);

        trace!(
            "Encoded frame {}: {} bytes (Annex B format), keyframe={}",
            self.frame_count,
            data.len(),
            is_keyframe
        );

        Ok(Some(H264Frame {
            size: data.len(),
            data,
            is_keyframe,
            timestamp_ms,
        }))
    }

    pub fn force_keyframe(&mut self) {
        self.encoder.force_intra_frame();
        debug!("Forced keyframe on next encode");
    }

    /// Get the detected ABI generation.
    pub fn abi(&self) -> openh264_compat::AbiGeneration {
        self.encoder.abi()
    }

    pub fn stats(&self) -> EncoderStats {
        EncoderStats {
            frames_encoded: self.frame_count,
            bitrate_kbps: self.config.bitrate_kbps,
        }
    }
}

/// Encoder statistics
#[derive(Debug, Clone)]
pub struct EncoderStats {
    /// Total frames encoded
    pub frames_encoded: u64,
    /// Configured bitrate in kbps
    pub bitrate_kbps: u32,
}

// Stub implementation when h264 feature is disabled
#[cfg(not(feature = "h264"))]
pub struct Avc420Encoder;

#[cfg(not(feature = "h264"))]
impl Avc420Encoder {
    pub fn new(_config: EncoderConfig) -> EncoderResult<Self> {
        Err(EncoderError::FeatureDisabled)
    }

    pub fn encode_bgra(
        &mut self,
        _bgra_data: &[u8],
        _width: u32,
        _height: u32,
        _timestamp_ms: u64,
    ) -> EncoderResult<Option<H264Frame>> {
        Err(EncoderError::FeatureDisabled)
    }

    pub fn force_keyframe(&mut self) {}

    pub fn stats(&self) -> EncoderStats {
        EncoderStats {
            frames_encoded: 0,
            bitrate_kbps: 0,
        }
    }
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
                Err(EncoderError::InitFailed(_)) => return,
                Err(e) => panic!("unexpected encoder error: {e:?}"),
            }
        };
    }

    #[test]
    fn test_encoder_config_defaults() {
        let config = EncoderConfig::default();
        assert_eq!(config.bitrate_kbps, 5000);
        assert!((config.max_fps - 30.0).abs() < f32::EPSILON);
    }

    #[test]
    fn test_encoder_config_presets() {
        let hq = EncoderConfig::high_quality();
        assert_eq!(hq.bitrate_kbps, 10000);

        let lb = EncoderConfig::low_bandwidth();
        assert_eq!(lb.bitrate_kbps, 1000);
    }

    #[cfg(feature = "h264")]
    #[test]
    fn test_encoder_creation() {
        let config = EncoderConfig::default();
        let _encoder = require_openh264!(Avc420Encoder::new(config));
    }

    #[cfg(feature = "h264")]
    #[test]
    fn test_encode_small_frame() {
        let config = EncoderConfig::default();
        let mut encoder = require_openh264!(Avc420Encoder::new(config));

        // Create a 64x64 black BGRA frame
        let width = 64u32;
        let height = 64u32;
        let bgra_data = vec![0u8; (width * height * 4) as usize];

        let result = encoder.encode_bgra(&bgra_data, width, height, 0);
        assert!(result.is_ok());
    }

    #[cfg(feature = "h264")]
    #[test]
    fn test_invalid_dimensions() {
        let config = EncoderConfig::default();
        let mut encoder = require_openh264!(Avc420Encoder::new(config));

        // Odd dimensions should fail
        let bgra_data = vec![0u8; 63 * 64 * 4];
        let result = encoder.encode_bgra(&bgra_data, 63, 64, 0);
        assert!(matches!(
            result,
            Err(EncoderError::InvalidDimensions { .. })
        ));
    }

    #[test]
    #[allow(deprecated)]
    fn test_annex_b_to_avc_4byte_start_code() {
        // Single NAL with 4-byte start code: 0x00 0x00 0x00 0x01 + NAL data
        let annex_b = vec![0x00, 0x00, 0x00, 0x01, 0x67, 0x42, 0x00, 0x1e];
        let avc = annex_b_to_avc(&annex_b);

        // Expected: 4-byte length (4) + NAL data
        assert_eq!(avc.len(), 8);
        // Length prefix: 0x00 0x00 0x00 0x04
        assert_eq!(&avc[0..4], &[0x00, 0x00, 0x00, 0x04]);
        // NAL data unchanged
        assert_eq!(&avc[4..8], &[0x67, 0x42, 0x00, 0x1e]);
    }

    #[test]
    #[allow(deprecated)]
    fn test_annex_b_to_avc_3byte_start_code() {
        // Single NAL with 3-byte start code: 0x00 0x00 0x01 + NAL data
        let annex_b = vec![0x00, 0x00, 0x01, 0x68, 0xce, 0x3c, 0x80];
        let avc = annex_b_to_avc(&annex_b);

        // Expected: 4-byte length (4) + NAL data
        assert_eq!(avc.len(), 8);
        assert_eq!(&avc[0..4], &[0x00, 0x00, 0x00, 0x04]);
        assert_eq!(&avc[4..8], &[0x68, 0xce, 0x3c, 0x80]);
    }

    #[test]
    #[allow(deprecated)]
    fn test_annex_b_to_avc_multiple_nals() {
        // Two NALs: SPS + PPS typical pattern
        // Note: NAL data must not contain sequences that look like start codes
        let annex_b = vec![
            // NAL 1: 4-byte start code + 3 bytes data (no 0x00 0x00 sequences)
            0x00, 0x00, 0x00, 0x01, 0x67, 0x42, 0x1e,
            // NAL 2: 3-byte start code + 2 bytes data
            0x00, 0x00, 0x01, 0x68, 0xce,
        ];
        let avc = annex_b_to_avc(&annex_b);

        // First NAL: length 3 + data
        assert_eq!(&avc[0..4], &[0x00, 0x00, 0x00, 0x03]);
        assert_eq!(&avc[4..7], &[0x67, 0x42, 0x1e]);

        // Second NAL: length 2 + data
        assert_eq!(&avc[7..11], &[0x00, 0x00, 0x00, 0x02]);
        assert_eq!(&avc[11..13], &[0x68, 0xce]);
    }

    #[test]
    #[allow(deprecated)]
    fn test_annex_b_to_avc_empty() {
        let annex_b: Vec<u8> = vec![];
        let avc = annex_b_to_avc(&annex_b);
        assert!(avc.is_empty());
    }

    #[test]
    #[allow(deprecated)]
    fn test_annex_b_to_avc_no_start_code() {
        // Data without start code should produce empty output
        let annex_b = vec![0x67, 0x42, 0x00, 0x1e];
        let avc = annex_b_to_avc(&annex_b);
        assert!(avc.is_empty());
    }

    #[test]
    fn test_align_to_16() {
        assert_eq!(align_to_16(0), 0);
        assert_eq!(align_to_16(1), 16);
        assert_eq!(align_to_16(15), 16);
        assert_eq!(align_to_16(16), 16);
        assert_eq!(align_to_16(17), 32);
        assert_eq!(align_to_16(1920), 1920); // Already aligned
        assert_eq!(align_to_16(1080), 1088); // Needs padding
        assert_eq!(align_to_16(1921), 1936);
    }

    // Note: Avc420Region and create_avc420_bitmap_stream tests are in ironrdp-egfx
}
