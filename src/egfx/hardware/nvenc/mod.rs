//! NVENC hardware encoder backend
//!
//! This module provides H.264 encoding using NVIDIA's Video Codec SDK (NVENC)
//! for NVIDIA GPUs.
//!
//! # Supported Hardware
//!
//! - NVIDIA Kepler (GTX 600 series) and newer
//! - Turing+ GPUs support B-frames for better compression
//!
//! # Architecture
//!
//! ```text
//! BGRA Frame (PipeWire)
//!       │
//!       ▼
//! NVENC Input Buffer (NV_ENC_BUFFER_FORMAT_ARGB)
//!       │
//!       ▼
//! NVENC Encoder (internal color conversion + H.264 encode)
//!       │
//!       ▼
//! Bitstream Buffer (H.264 NAL units)
//! ```
//!
//! # Usage
//!
//! ```rust,ignore
//! use lamco_rdp_server::egfx::hardware::nvenc::NvencEncoder;
//!
//! let encoder = NvencEncoder::new(&config, 1920, 1080, QualityPreset::Balanced)?;
//! let frame = encoder.encode_bgra(&bgra_data, 1920, 1080, timestamp)?;
//! ```
//!
//! # Presets
//!
//! NVENC uses P1-P7 presets with tuning modes:
//! - P1-P3: Fast/low-latency
//! - P4: Balanced (default)
//! - P5-P7: Quality/slow
#![expect(
    unsafe_code,
    reason = "NVENC GPU API requires unsafe FFI and unsafe Send impl"
)]

use cudarc::driver::CudaContext;
use nvidia_video_codec_sdk::{
    sys::nvEncodeAPI::{
        GUID, NV_ENC_BUFFER_FORMAT, NV_ENC_CODEC_H264_GUID, NV_ENC_CONFIG_VER,
        NV_ENC_H264_PROFILE_HIGH_GUID, NV_ENC_PIC_TYPE, NV_ENC_PRESET_P1_GUID,
        NV_ENC_PRESET_P2_GUID, NV_ENC_PRESET_P3_GUID, NV_ENC_PRESET_P4_GUID, NV_ENC_PRESET_P5_GUID,
        NV_ENC_PRESET_P6_GUID, NV_ENC_PRESET_P7_GUID, NV_ENC_TUNING_INFO,
        NV_ENC_VUI_COLOR_PRIMARIES, NV_ENC_VUI_MATRIX_COEFFS, NV_ENC_VUI_TRANSFER_CHARACTERISTIC,
    },
    Bitstream, Buffer, EncodePictureParams, Encoder, EncoderInitParams, Session,
};
use tracing::{debug, info, warn};

use super::{
    error::NvencError, EncodeTimer, H264Frame, HardwareEncoder, HardwareEncoderError,
    HardwareEncoderResult, HardwareEncoderStats, QualityPreset,
};
use crate::{
    config::HardwareEncodingConfig,
    egfx::color_space::{
        ColorRange, ColorSpaceConfig, ColourPrimaries, MatrixCoefficients, TransferCharacteristics,
    },
};

/// Number of input/output buffers for pipelining
const NUM_BUFFERS: usize = 3;

/// NVENC preset (P1-P7)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NvencPreset {
    /// Fastest encoding, lowest quality
    P1,
    /// Fast
    P2,
    /// Medium-fast
    P3,
    /// Balanced (default)
    P4,
    /// Medium-slow
    P5,
    /// Slow, high quality
    P6,
    /// Slowest, highest quality
    P7,
}

impl NvencPreset {
    pub fn from_quality_preset(preset: QualityPreset) -> Self {
        match preset {
            QualityPreset::Speed => Self::P2,
            QualityPreset::Balanced => Self::P4,
            QualityPreset::Quality => Self::P6,
        }
    }

    fn to_guid(&self) -> GUID {
        match self {
            Self::P1 => NV_ENC_PRESET_P1_GUID,
            Self::P2 => NV_ENC_PRESET_P2_GUID,
            Self::P3 => NV_ENC_PRESET_P3_GUID,
            Self::P4 => NV_ENC_PRESET_P4_GUID,
            Self::P5 => NV_ENC_PRESET_P5_GUID,
            Self::P6 => NV_ENC_PRESET_P6_GUID,
            Self::P7 => NV_ENC_PRESET_P7_GUID,
        }
    }
}

/// NVENC tuning mode
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum NvencTuning {
    /// Balanced quality and performance
    #[default]
    Default,
    /// Low latency for real-time streaming
    LowLatency,
    /// Ultra-low latency for gaming/interactive
    UltraLowLatency,
    /// High quality for offline encoding
    HighQuality,
    /// Lossless mode (highest quality)
    Lossless,
}

impl NvencTuning {
    pub fn from_quality_preset(preset: QualityPreset) -> Self {
        match preset {
            QualityPreset::Speed => Self::UltraLowLatency,
            QualityPreset::Balanced => Self::LowLatency,
            QualityPreset::Quality => Self::Default,
        }
    }

    fn to_nvenc_tuning(&self) -> NV_ENC_TUNING_INFO {
        match self {
            Self::Default => NV_ENC_TUNING_INFO::NV_ENC_TUNING_INFO_HIGH_QUALITY,
            Self::LowLatency => NV_ENC_TUNING_INFO::NV_ENC_TUNING_INFO_LOW_LATENCY,
            Self::UltraLowLatency => NV_ENC_TUNING_INFO::NV_ENC_TUNING_INFO_ULTRA_LOW_LATENCY,
            Self::HighQuality => NV_ENC_TUNING_INFO::NV_ENC_TUNING_INFO_HIGH_QUALITY,
            Self::Lossless => NV_ENC_TUNING_INFO::NV_ENC_TUNING_INFO_LOSSLESS,
        }
    }
}

/// NVENC H.264 encoder
///
/// Provides GPU-accelerated H.264 encoding for NVIDIA GPUs.
/// Accepts BGRA input directly (NVENC handles color conversion internally).
///
/// IMPORTANT: All NVENC types (Session, Buffer, Bitstream) are wrapped in Box
/// to prevent move-invalidation. The nvidia-video-codec-sdk types contain internal
/// pointers that become invalid when the struct is moved in memory. Boxing prevents
/// this by keeping them at a stable heap address.
///
/// Field order matters for drop safety - buffers must be dropped BEFORE session.
pub struct NvencEncoder {
    /// Input buffers (triple buffered) - Option<Box> for explicit drop control
    /// Must drop before session since they reference the encoder.
    input_buffers: Vec<Option<Box<Buffer<'static>>>>,

    /// Output bitstreams (triple buffered) - Option<Box> for explicit drop control
    /// Must drop before session since they reference the encoder.
    output_bitstreams: Vec<Option<Box<Bitstream<'static>>>>,

    /// CUDA context - needed to bind before NVENC operations
    cuda_ctx: std::sync::Arc<cudarc::driver::CudaContext>,

    /// Current buffer index
    current_buffer: usize,

    /// Cached SPS/PPS from last IDR frame
    cached_sps_pps: Option<Vec<u8>>,

    /// Frame dimensions
    width: u32,
    height: u32,

    /// Quality preset
    preset: QualityPreset,

    /// NVENC preset (P1-P7)
    nvenc_preset: NvencPreset,

    /// NVENC tuning mode
    tuning: NvencTuning,

    /// Frame counter
    frame_count: u64,

    /// Force next frame to be IDR
    force_idr: bool,

    /// GOP size (keyframe interval)
    gop_size: u32,

    /// Encoder statistics
    stats: HardwareEncoderStats,

    /// Color space configuration for VUI signaling
    color_space: ColorSpaceConfig,

    /// NVENC session (owns the encoder) - Boxed to prevent move-invalidation.
    /// MUST BE LAST for drop order safety - buffers reference the encoder.
    session: Box<Session>,
}

/// Map ColorSpaceConfig colour primaries to NVENC VUI enum
fn map_colour_primaries(primaries: ColourPrimaries) -> NV_ENC_VUI_COLOR_PRIMARIES {
    match primaries {
        ColourPrimaries::BT709 => NV_ENC_VUI_COLOR_PRIMARIES::NV_ENC_VUI_COLOR_PRIMARIES_BT709,
        ColourPrimaries::Unspecified => {
            NV_ENC_VUI_COLOR_PRIMARIES::NV_ENC_VUI_COLOR_PRIMARIES_UNSPECIFIED
        }
        ColourPrimaries::BT601NTSC => {
            NV_ENC_VUI_COLOR_PRIMARIES::NV_ENC_VUI_COLOR_PRIMARIES_SMPTE170M
        }
        ColourPrimaries::BT601PAL => NV_ENC_VUI_COLOR_PRIMARIES::NV_ENC_VUI_COLOR_PRIMARIES_BT470BG,
        ColourPrimaries::BT2020 => NV_ENC_VUI_COLOR_PRIMARIES::NV_ENC_VUI_COLOR_PRIMARIES_BT2020,
    }
}

/// Map ColorSpaceConfig transfer characteristics to NVENC VUI enum
fn map_transfer_characteristics(
    transfer: TransferCharacteristics,
) -> NV_ENC_VUI_TRANSFER_CHARACTERISTIC {
    match transfer {
        TransferCharacteristics::BT709 => {
            NV_ENC_VUI_TRANSFER_CHARACTERISTIC::NV_ENC_VUI_TRANSFER_CHARACTERISTIC_BT709
        }
        TransferCharacteristics::Unspecified => {
            NV_ENC_VUI_TRANSFER_CHARACTERISTIC::NV_ENC_VUI_TRANSFER_CHARACTERISTIC_UNSPECIFIED
        }
        TransferCharacteristics::BT601 => {
            NV_ENC_VUI_TRANSFER_CHARACTERISTIC::NV_ENC_VUI_TRANSFER_CHARACTERISTIC_SMPTE170M
        }
        TransferCharacteristics::SRGB => {
            NV_ENC_VUI_TRANSFER_CHARACTERISTIC::NV_ENC_VUI_TRANSFER_CHARACTERISTIC_SRGB
        }
        TransferCharacteristics::BT2020_10 => {
            NV_ENC_VUI_TRANSFER_CHARACTERISTIC::NV_ENC_VUI_TRANSFER_CHARACTERISTIC_BT2020_10
        }
        TransferCharacteristics::BT2020_12 => {
            NV_ENC_VUI_TRANSFER_CHARACTERISTIC::NV_ENC_VUI_TRANSFER_CHARACTERISTIC_BT2020_12
        }
    }
}

/// Map ColorSpaceConfig matrix coefficients to NVENC VUI enum
fn map_matrix_coefficients(matrix: MatrixCoefficients) -> NV_ENC_VUI_MATRIX_COEFFS {
    match matrix {
        MatrixCoefficients::BT709 => NV_ENC_VUI_MATRIX_COEFFS::NV_ENC_VUI_MATRIX_COEFFS_BT709,
        MatrixCoefficients::Unspecified => {
            NV_ENC_VUI_MATRIX_COEFFS::NV_ENC_VUI_MATRIX_COEFFS_UNSPECIFIED
        }
        MatrixCoefficients::BT601 => NV_ENC_VUI_MATRIX_COEFFS::NV_ENC_VUI_MATRIX_COEFFS_SMPTE170M,
        MatrixCoefficients::BT2020NCL => {
            NV_ENC_VUI_MATRIX_COEFFS::NV_ENC_VUI_MATRIX_COEFFS_BT2020_NCL
        }
    }
}

// SAFETY: NVENC handles are safe to send between threads.
// The nvidia-video-codec-sdk marks Buffer and Bitstream as Send.
// Session contains Encoder which contains raw pointers, but the NVENC API
// allows calling from different threads as long as calls are serialized.
unsafe impl Send for NvencEncoder {}

impl Drop for NvencEncoder {
    fn drop(&mut self) {
        // CRITICAL: Rebind CUDA context before dropping NVENC resources.
        // CUDA contexts are thread-local, and between the last encode_bgra call
        // and this drop, the context may have been unbound. The Buffer and Bitstream
        // types need a valid CUDA context to destroy their GPU resources.
        if let Err(e) = self.cuda_ctx.bind_to_thread() {
            warn!(
                "Failed to bind CUDA context during NvencEncoder drop: {:?}. \
                 GPU resources may leak.",
                e
            );
            return; // Don't try to drop buffers if context binding failed
        }

        // Explicitly drop buffers BEFORE session by setting Options to None.
        // This ensures proper cleanup order: buffers reference the session's encoder,
        // so they must be destroyed first.
        for buf in &mut self.input_buffers {
            *buf = None;
        }
        for buf in &mut self.output_bitstreams {
            *buf = None;
        }
        // session drops automatically when this function returns
    }
}

impl NvencEncoder {
    pub fn new(
        _config: &HardwareEncodingConfig,
        width: u32,
        height: u32,
        preset: QualityPreset,
    ) -> HardwareEncoderResult<Self> {
        // Validate dimensions
        if width == 0 || height == 0 || width % 2 != 0 || height % 2 != 0 {
            return Err(HardwareEncoderError::InvalidDimensions {
                width,
                height,
                reason: "dimensions must be non-zero and even".to_string(),
            });
        }

        let nvenc_preset = NvencPreset::from_quality_preset(preset);
        let tuning = NvencTuning::from_quality_preset(preset);
        let gop_size = preset.gop_size();

        info!(
            "Initializing NVENC encoder: {}x{}, preset={:?}, tuning={:?}, gop={}",
            width, height, nvenc_preset, tuning, gop_size
        );

        // Initialize CUDA context
        // CudaContext::new already returns Arc<CudaContext>
        let cuda_ctx = CudaContext::new(0).map_err(|e| {
            HardwareEncoderError::from(NvencError::CudaDeviceNotFound(format!(
                "Failed to create CUDA context: {}",
                e
            )))
        })?;

        // Clone the context Arc - we need to keep a reference for binding before operations
        let cuda_ctx_for_encoder = cuda_ctx.clone();

        // Create NVENC encoder with CUDA device
        let encoder = Encoder::initialize_with_cuda(cuda_ctx_for_encoder).map_err(|e| {
            HardwareEncoderError::from(NvencError::ApiInitFailed(format!(
                "Failed to initialize NVENC: {}",
                e
            )))
        })?;

        // Check H.264 encoding support
        let encode_guids = encoder.get_encode_guids().map_err(|e| {
            HardwareEncoderError::from(NvencError::ApiInitFailed(format!(
                "Failed to query encode GUIDs: {}",
                e
            )))
        })?;

        if !encode_guids.contains(&NV_ENC_CODEC_H264_GUID) {
            return Err(HardwareEncoderError::from(NvencError::CodecNotSupported {
                codec: "H.264".to_string(),
                available: vec!["Unknown".to_string()],
            }));
        }

        // Check supported input formats
        let input_formats = encoder
            .get_supported_input_formats(NV_ENC_CODEC_H264_GUID)
            .map_err(|e| {
                HardwareEncoderError::from(NvencError::ApiInitFailed(format!(
                    "Failed to query input formats: {}",
                    e
                )))
            })?;

        // Use ARGB format for BGRA input (NVENC swizzles internally)
        let buffer_format =
            if input_formats.contains(&NV_ENC_BUFFER_FORMAT::NV_ENC_BUFFER_FORMAT_ARGB) {
                NV_ENC_BUFFER_FORMAT::NV_ENC_BUFFER_FORMAT_ARGB
            } else if input_formats.contains(&NV_ENC_BUFFER_FORMAT::NV_ENC_BUFFER_FORMAT_ABGR) {
                NV_ENC_BUFFER_FORMAT::NV_ENC_BUFFER_FORMAT_ABGR
            } else {
                return Err(HardwareEncoderError::from(NvencError::ApiInitFailed(
                    "No compatible RGBA input format supported".to_string(),
                )));
            };

        debug!("Using buffer format: {:?}", buffer_format);

        // Get preset config and modify as needed
        let preset_config = encoder
            .get_preset_config(
                NV_ENC_CODEC_H264_GUID,
                nvenc_preset.to_guid(),
                tuning.to_nvenc_tuning(),
            )
            .map_err(|e| {
                HardwareEncoderError::from(NvencError::ApiInitFailed(format!(
                    "Failed to get preset config: {}",
                    e
                )))
            })?;

        // Customize encoder config
        let mut encode_config = preset_config.presetCfg;
        encode_config.version = NV_ENC_CONFIG_VER;
        encode_config.gopLength = gop_size;
        encode_config.frameIntervalP = 1; // No B-frames for low latency

        // Configure rate control for CBR
        encode_config.rcParams.averageBitRate = preset.bitrate_kbps() * 1000;
        encode_config.rcParams.maxBitRate = preset.bitrate_kbps() * 1500; // 1.5x headroom
        encode_config.rcParams.vbvBufferSize = preset.bitrate_kbps() * 1000 / 8; // 1 second buffer

        // Set H.264 profile to High
        encode_config.profileGUID = NV_ENC_H264_PROFILE_HIGH_GUID;

        // Auto-select color space based on resolution
        let color_space = ColorSpaceConfig::from_resolution(width, height);

        // Configure H.264 VUI parameters for proper color signaling
        // SAFETY: encodeCodecConfig is a union, we access h264Config for H.264 encoding
        unsafe {
            let h264_config = &mut encode_config.encodeCodecConfig.h264Config;
            let vui = &mut h264_config.h264VUIParameters;

            // Enable video signal type and color description
            vui.videoSignalTypePresentFlag = 1;
            vui.colourDescriptionPresentFlag = 1;

            // Set color range (full or limited)
            vui.videoFullRangeFlag = if color_space.range == ColorRange::Full {
                1
            } else {
                0
            };

            // Set color primaries, transfer characteristics, and matrix coefficients
            vui.colourPrimaries = map_colour_primaries(color_space.primaries);
            vui.transferCharacteristics = map_transfer_characteristics(color_space.transfer);
            vui.colourMatrix = map_matrix_coefficients(color_space.matrix_coeff);
        }

        info!(
            "🎨 NVENC VUI configured: primaries={:?}, transfer={:?}, matrix={:?}, range={:?}",
            color_space.primaries,
            color_space.transfer,
            color_space.matrix_coeff,
            color_space.range
        );

        // Initialize encoder params
        let mut init_params = EncoderInitParams::new(NV_ENC_CODEC_H264_GUID, width, height);
        init_params
            .preset_guid(nvenc_preset.to_guid())
            .tuning_info(tuning.to_nvenc_tuning())
            .framerate(30, 1)
            // Note: display_aspect_ratio and enable_picture_type_decision removed for compatibility
            .encode_config(&mut encode_config);

        // Start session - Box IMMEDIATELY to prevent move-invalidation
        // The nvidia-video-codec-sdk types contain internal pointers that become
        // invalid when moved. Boxing keeps them at a stable heap address.
        let session = Box::new(
            encoder
                .start_session(buffer_format, init_params)
                .map_err(|e| {
                    HardwareEncoderError::from(NvencError::SessionCreationFailed(format!(
                        "Failed to start session: {}",
                        e
                    )))
                })?,
        );

        // Create input/output buffers - each wrapped in Option<Box> for move safety
        // SAFETY: We're extending the lifetime of buffers/bitstreams to 'static.
        // This is safe because we ensure they are dropped before the session.
        // The Session owns the Encoder, so the lifetime relationships are:
        // buffers/bitstreams <- session <- encoder
        let mut input_buffers = Vec::with_capacity(NUM_BUFFERS);
        let mut output_bitstreams = Vec::with_capacity(NUM_BUFFERS);

        for i in 0..NUM_BUFFERS {
            let input = session.create_input_buffer().map_err(|e| {
                HardwareEncoderError::from(NvencError::InputBufferError(format!(
                    "Failed to create input buffer {}: {}",
                    i, e
                )))
            })?;
            // SAFETY: buffer lifetime is tied to session which we own
            let input: Buffer<'static> = unsafe { std::mem::transmute(input) };
            // Box to prevent move-invalidation
            input_buffers.push(Some(Box::new(input)));

            let output = session.create_output_bitstream().map_err(|e| {
                HardwareEncoderError::from(NvencError::BitstreamError(format!(
                    "Failed to create output bitstream {}: {}",
                    i, e
                )))
            })?;
            // SAFETY: bitstream lifetime is tied to session which we own
            let output: Bitstream<'static> = unsafe { std::mem::transmute(output) };
            // Box to prevent move-invalidation
            output_bitstreams.push(Some(Box::new(output)));
        }

        info!(
            "✅ NVENC encoder initialized: {} input buffers, {} output bitstreams",
            input_buffers.len(),
            output_bitstreams.len()
        );

        let stats = HardwareEncoderStats::new("nvenc", preset.bitrate_kbps());

        Ok(Self {
            input_buffers,
            output_bitstreams,
            cuda_ctx,
            current_buffer: 0,
            cached_sps_pps: None,
            width,
            height,
            preset,
            nvenc_preset,
            tuning,
            frame_count: 0,
            force_idr: true, // First frame is always IDR
            gop_size,
            stats,
            color_space,
            session, // MUST BE LAST for drop order safety
        })
    }

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

    fn prepend_sps_pps(&self, frame_data: &[u8]) -> Vec<u8> {
        if let Some(ref sps_pps) = self.cached_sps_pps {
            let mut data = Vec::with_capacity(sps_pps.len() + frame_data.len());
            data.extend_from_slice(sps_pps);
            data.extend_from_slice(frame_data);
            data
        } else {
            frame_data.to_vec()
        }
    }
}

impl HardwareEncoder for NvencEncoder {
    fn encode_bgra(
        &mut self,
        bgra_data: &[u8],
        width: u32,
        height: u32,
        timestamp_ms: u64,
    ) -> HardwareEncoderResult<Option<H264Frame>> {
        let timer = EncodeTimer::start();

        // Bind CUDA context to this thread before NVENC operations
        // This is critical - CUDA contexts are thread-local and must be bound
        // before any NVENC API calls, including in Drop
        self.cuda_ctx.bind_to_thread().map_err(|e| {
            HardwareEncoderError::EncodeFailed(format!("Failed to bind CUDA context: {:?}", e))
        })?;

        // Validate dimensions
        if width != self.width || height != self.height {
            return Err(HardwareEncoderError::InvalidDimensions {
                width,
                height,
                reason: format!(
                    "resolution mismatch: encoder configured for {}x{}",
                    self.width, self.height
                ),
            });
        }

        let expected_size = (width * height * 4) as usize;
        if bgra_data.len() < expected_size {
            return Err(HardwareEncoderError::InvalidDimensions {
                width,
                height,
                reason: format!(
                    "buffer too small: {} < {} bytes",
                    bgra_data.len(),
                    expected_size
                ),
            });
        }

        // Determine if this should be an IDR frame
        let is_idr = self.force_idr || (self.frame_count % self.gop_size as u64 == 0);

        // Get current buffer index
        let buf_idx = self.current_buffer;
        self.current_buffer = (self.current_buffer + 1) % NUM_BUFFERS;

        // Get buffer references - unwrap Option and deref Box
        let input_buffer = self.input_buffers[buf_idx].as_mut().ok_or_else(|| {
            HardwareEncoderError::EncodeFailed("Input buffer was dropped".to_string())
        })?;
        let output_bitstream = self.output_bitstreams[buf_idx].as_mut().ok_or_else(|| {
            HardwareEncoderError::EncodeFailed("Output bitstream was dropped".to_string())
        })?;

        // Lock input buffer and write BGRA data
        {
            let mut lock = input_buffer.lock().map_err(|e| {
                HardwareEncoderError::EncodeFailed(format!("Failed to lock input buffer: {}", e))
            })?;
            // SAFETY: We've validated the data size matches the buffer size
            unsafe { lock.write(bgra_data) };
        }

        // Set up encode parameters
        let picture_type = if is_idr {
            NV_ENC_PIC_TYPE::NV_ENC_PIC_TYPE_IDR
        } else {
            NV_ENC_PIC_TYPE::NV_ENC_PIC_TYPE_P
        };

        let params = EncodePictureParams {
            input_timestamp: timestamp_ms,
            picture_type,
            codec_params: None,
        };

        // Encode the frame
        self.session
            .encode_picture(&mut **input_buffer, &mut **output_bitstream, params)
            .map_err(|e| {
                HardwareEncoderError::EncodeFailed(format!("NVENC encode failed: {}", e))
            })?;

        // Lock output bitstream and read encoded data
        // Scoped to release the lock before processing (avoids borrow conflict with self.prepend_sps_pps)
        let (raw_data_copy, actual_is_idr) = {
            let lock = output_bitstream.lock().map_err(|e| {
                HardwareEncoderError::EncodeFailed(format!(
                    "Failed to lock output bitstream: {}",
                    e
                ))
            })?;

            let raw_data = lock.data().to_vec();
            let is_idr = matches!(
                lock.picture_type(),
                NV_ENC_PIC_TYPE::NV_ENC_PIC_TYPE_IDR | NV_ENC_PIC_TYPE::NV_ENC_PIC_TYPE_I
            );
            (raw_data, is_idr)
        }; // lock is dropped here

        // Process encoded data
        let encoded_data = if actual_is_idr {
            // IDR frame - extract and cache SPS/PPS
            if let Some(sps_pps) = Self::extract_sps_pps(&raw_data_copy) {
                debug!(
                    "NVENC: Cached SPS/PPS ({} bytes) from IDR frame",
                    sps_pps.len()
                );
                self.cached_sps_pps = Some(sps_pps);
            }
            raw_data_copy
        } else {
            // P-frame - prepend cached SPS/PPS for RDP compatibility
            self.prepend_sps_pps(&raw_data_copy)
        };

        // Update statistics
        let encode_time_ms = timer.elapsed_ms();
        self.stats
            .record_frame(encode_time_ms, encoded_data.len(), actual_is_idr);

        // Reset IDR flag
        if self.force_idr {
            self.force_idr = false;
        }

        let frame_size = encoded_data.len();
        self.frame_count += 1;

        debug!(
            "NVENC: Encoded frame {} ({}) {} bytes in {:.2}ms",
            self.frame_count,
            if actual_is_idr { "IDR" } else { "P" },
            frame_size,
            encode_time_ms
        );

        Ok(Some(H264Frame {
            data: encoded_data,
            is_keyframe: actual_is_idr,
            timestamp_ms,
            size: frame_size,
        }))
    }

    fn force_keyframe(&mut self) {
        debug!("NVENC: Forcing IDR on next frame");
        self.force_idr = true;
    }

    fn stats(&self) -> HardwareEncoderStats {
        self.stats.clone()
    }

    fn backend_name(&self) -> &'static str {
        "nvenc"
    }

    fn supports_dynamic_resolution(&self) -> bool {
        false // NVENC requires session recreation for resolution change
    }
}

pub fn is_nvidia_available() -> bool {
    use std::path::Path;

    // NVENC requires direct GPU access not available in Flatpak sandbox
    if crate::config::is_flatpak() {
        return false;
    }

    // Check for NVIDIA device nodes
    let nvidia_devices = ["/dev/nvidia0", "/dev/nvidiactl", "/dev/nvidia-uvm"];

    for device in &nvidia_devices {
        if Path::new(device).exists() {
            return true;
        }
    }

    // Check for driver in proc
    if Path::new("/proc/driver/nvidia/version").exists() {
        return true;
    }

    false
}

#[expect(dead_code, reason = "diagnostic utility for NVENC troubleshooting")]
fn get_nvidia_info() -> Option<(String, String)> {
    // Read driver version from /proc
    if let Ok(version) = std::fs::read_to_string("/proc/driver/nvidia/version") {
        // Parse version line like "NVRM version: NVIDIA UNIX x86_64 Kernel Module  535.154.05"
        if let Some(line) = version.lines().next() {
            if let Some(ver_start) = line.find("Module") {
                let version_str = line[ver_start + 7..].trim().to_string();
                return Some(("NVIDIA GPU".to_string(), version_str));
            }
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn test_config() -> HardwareEncodingConfig {
        HardwareEncodingConfig {
            enabled: true,
            vaapi_device: PathBuf::from("/dev/dri/renderD128"),
            enable_dmabuf_zerocopy: false,
            fallback_to_software: true,
            quality_preset: "balanced".to_string(),
            prefer_nvenc: true,
        }
    }

    #[test]
    fn test_nvenc_preset_from_quality() {
        assert_eq!(
            NvencPreset::from_quality_preset(QualityPreset::Speed),
            NvencPreset::P2
        );
        assert_eq!(
            NvencPreset::from_quality_preset(QualityPreset::Balanced),
            NvencPreset::P4
        );
        assert_eq!(
            NvencPreset::from_quality_preset(QualityPreset::Quality),
            NvencPreset::P6
        );
    }

    #[test]
    fn test_nvenc_tuning_from_quality() {
        assert_eq!(
            NvencTuning::from_quality_preset(QualityPreset::Speed),
            NvencTuning::UltraLowLatency
        );
        assert_eq!(
            NvencTuning::from_quality_preset(QualityPreset::Balanced),
            NvencTuning::LowLatency
        );
        assert_eq!(
            NvencTuning::from_quality_preset(QualityPreset::Quality),
            NvencTuning::Default
        );
    }

    #[test]
    fn test_nvidia_detection() {
        // This test may fail without actual hardware
        let available = is_nvidia_available();
        println!("NVIDIA available: {}", available);

        if available {
            if let Some((gpu, version)) = get_nvidia_info() {
                println!("GPU: {}, Driver: {}", gpu, version);
            }
        }
    }

    #[test]
    fn test_extract_sps_pps() {
        // Sample SPS + PPS in Annex B format
        let data = vec![
            0x00, 0x00, 0x00, 0x01, 0x67, 0x42, 0x00, 0x1e, // SPS
            0x00, 0x00, 0x00, 0x01, 0x68, 0xce, 0x3c, 0x80, // PPS
        ];

        let sps_pps = NvencEncoder::extract_sps_pps(&data);
        assert!(sps_pps.is_some());

        let extracted = sps_pps.unwrap();
        assert_eq!(extracted.len(), 16);
    }

    #[test]
    #[ignore = "Requires NVIDIA GPU"]
    fn test_nvenc_encoder_creation() {
        let config = test_config();
        let result = NvencEncoder::new(&config, 1920, 1080, QualityPreset::Balanced);
        assert!(
            result.is_ok(),
            "Failed to create NVENC encoder: {:?}",
            result.err()
        );
    }
}
