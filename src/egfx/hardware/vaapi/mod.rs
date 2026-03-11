//! VA-API hardware encoder backend
//!
//! This module provides H.264 encoding using the Video Acceleration API (VA-API)
//! for Intel and AMD GPUs on Linux.
//!
//! # Supported Hardware
//!
//! - Intel Gen7+ (Haswell and newer) via iHD or i965 driver
//! - AMD GCN+ via radeonsi driver
//!
//! # Architecture
//!
//! ```text
//! BGRA Frame (PipeWire)
//!       │
//!       ▼
//! bgra_to_nv12 (SIMD-optimized color conversion)
//!       │
//!       ▼
//! Image::create_from (upload NV12 to surface)
//!       │
//!       ▼
//! Picture (typestate: New → Begin → Render → End → Sync)
//!       │
//!       ▼
//! MappedCodedBuffer (read H.264 NAL units)
//! ```
//!
//! # Reference Frame Management
//!
//! The encoder maintains a separate set of reconstructed surfaces for
//! reference frame tracking. P-frames reference the most recently
//! encoded frame via the Decoded Picture Buffer (DPB), enabling true
//! inter-frame prediction for better compression.
//!
//! # Thread Safety
//!
//! VA-API encoders are NOT thread-safe. The encoder must be created and used
//! on the same thread. For async usage, run encoding on a dedicated thread.

#![expect(
    unsafe_code,
    reason = "SIMD intrinsics (AVX2, NEON) for optimized NV12 color conversion"
)]

use std::{path::Path, rc::Rc};

use cros_libva::{
    self as libva, BufferType, Context, Display, EncCodedBuffer, EncMiscParameter,
    EncMiscParameterFrameRate, EncMiscParameterHRD, EncMiscParameterRateControl,
    EncPictureParameter, EncSequenceParameter, EncSliceParameter, MappedCodedBuffer, Picture,
    RcFlags, Surface, UsageHint, VA_INVALID_ID, VA_INVALID_SURFACE, VA_PICTURE_H264_INVALID,
    VA_PICTURE_H264_SHORT_TERM_REFERENCE, VA_RT_FORMAT_YUV420, VAEntrypoint, VAImageFormat,
    VAProfile,
};
use tracing::{debug, info, trace};

use super::{
    EncodeTimer, H264Frame, HardwareEncoder, HardwareEncoderError, HardwareEncoderResult,
    HardwareEncoderStats, QualityPreset, error::VaapiError,
};
use crate::{config::HardwareEncodingConfig, egfx::color_space::ColorSpaceConfig};

/// Number of surfaces in the input pool for triple buffering
const INPUT_SURFACE_POOL_SIZE: usize = 3;

/// Number of reconstructed surfaces for reference frame tracking.
/// We only need 2: one "current" being encoded, one "previous" as reference.
const RECON_SURFACE_POOL_SIZE: usize = 2;

/// Number of coded output buffers for pipelining
const CODED_BUFFER_COUNT: usize = 3;

/// H.264 slice type constants
const SLICE_TYPE_I: u8 = 2;
const SLICE_TYPE_P: u8 = 0;

/// Metadata for a reconstructed reference frame in the DPB
#[derive(Clone)]
struct DpbEntry {
    /// VA surface ID of the reconstructed picture
    surface_id: u32,
    /// H.264 frame_num
    frame_num: u16,
    /// Picture Order Count
    poc: i32,
}

/// VA-API H.264 encoder
///
/// Provides GPU-accelerated H.264 encoding for Intel and AMD GPUs.
/// Uses surface pooling and image upload for color conversion.
///
/// # Reference Frame Tracking
///
/// Maintains a Decoded Picture Buffer (DPB) with the last reconstructed
/// frame as a short-term reference. P-frames predict from this reference,
/// enabling true temporal compression.
///
/// # Thread Safety
///
/// This encoder is NOT `Send` due to VA-API's thread-local design.
/// Create and use on the same thread.
pub struct VaapiEncoder {
    /// VA Display handle
    display: Rc<Display>,

    /// Encode context
    context: Rc<Context>,

    /// Input surfaces (NV12 format, for uploading BGRA→NV12 data)
    input_surfaces: Vec<Surface<()>>,

    /// Reconstructed surfaces (separate from input, used for reference frames)
    recon_surfaces: Vec<Surface<()>>,

    /// Current input surface index (round-robin)
    current_input_surface: usize,

    /// Current reconstructed surface index (alternates 0/1)
    current_recon_surface: usize,

    /// Coded buffers for output (triple-buffered)
    coded_buffers: Vec<EncCodedBuffer>,

    /// Current coded buffer index (round-robin)
    current_coded_buffer: usize,

    /// Decoded Picture Buffer: last reconstructed frame used as reference
    last_ref: Option<DpbEntry>,

    /// Cached SPS/PPS from last IDR frame
    cached_sps_pps: Option<Vec<u8>>,

    /// Frame dimensions
    width: u32,
    height: u32,

    /// Quality preset
    preset: QualityPreset,

    /// Frame counter
    frame_count: u64,

    /// IDR frame interval
    idr_interval: u32,

    /// Force next frame to be IDR
    force_idr: bool,

    /// Encoder statistics
    stats: HardwareEncoderStats,

    /// VA driver name (e.g., "iHD", "i965", "radeonsi")
    driver_name: String,

    /// Device path
    device_path: String,

    /// Target bitrate in bits per second
    bitrate_bps: u32,

    /// NV12 image format for uploads
    nv12_format: VAImageFormat,

    /// Color space configuration for conversion and VUI
    color_space: ColorSpaceConfig,
}

impl VaapiEncoder {
    pub fn new(
        hw_config: &HardwareEncodingConfig,
        width: u32,
        height: u32,
        preset: QualityPreset,
    ) -> HardwareEncoderResult<Self> {
        // Validate dimensions (must be even for H.264)
        if width == 0 || height == 0 || !width.is_multiple_of(2) || !height.is_multiple_of(2) {
            return Err(HardwareEncoderError::InvalidDimensions {
                width,
                height,
                reason: "dimensions must be non-zero and even".to_string(),
            });
        }

        let device_path = hw_config.vaapi_device.to_string_lossy().to_string();

        // Check if device exists
        if !Path::new(&device_path).exists() {
            return Err(HardwareEncoderError::from(VaapiError::DeviceOpenFailed {
                path: hw_config.vaapi_device.clone(),
                source: std::io::Error::new(std::io::ErrorKind::NotFound, "Device not found"),
            }));
        }

        info!(
            "Initializing VA-API encoder: {}x{}, preset={}, device={}",
            width, height, preset, device_path
        );

        // Open VA display from DRM device
        let display = Display::open_drm_display(Path::new(&device_path))
            .map_err(|e| VaapiError::DisplayInitFailed(format!("{e:?}")))?;

        // Get driver info
        let driver_name = display
            .query_vendor_string()
            .unwrap_or_else(|_| "unknown".to_string());
        debug!("VA-API vendor: {}", driver_name);

        // Check for H.264 encode support
        let profiles = display
            .query_config_profiles()
            .map_err(|e| VaapiError::ProfileQueryFailed(e.to_string()))?;

        // Find H.264 encode profile (prefer High, fallback to Main)
        let h264_profile = if profiles.contains(&VAProfile::VAProfileH264High) {
            VAProfile::VAProfileH264High
        } else if profiles.contains(&VAProfile::VAProfileH264Main) {
            VAProfile::VAProfileH264Main
        } else {
            return Err(HardwareEncoderError::from(VaapiError::H264NotSupported));
        };

        debug!("Using H.264 profile: {:?}", h264_profile);

        // Check for encode entrypoint
        let entrypoints = display
            .query_config_entrypoints(h264_profile)
            .map_err(|e| VaapiError::EntrypointQueryFailed(e.to_string()))?;

        if !entrypoints.contains(&VAEntrypoint::VAEntrypointEncSlice) {
            return Err(HardwareEncoderError::from(VaapiError::EncodeNotSupported));
        }

        // Create encode config
        let config = display
            .create_config(
                vec![], // Use default attributes
                h264_profile,
                VAEntrypoint::VAEntrypointEncSlice,
            )
            .map_err(|e| VaapiError::ConfigCreateFailed(e.to_string()))?;

        // Create input surfaces (for uploading NV12 data)
        let input_surfaces = display
            .create_surfaces(
                VA_RT_FORMAT_YUV420,
                Some(u32::from_ne_bytes(*b"NV12")),
                width,
                height,
                Some(UsageHint::USAGE_HINT_ENCODER),
                vec![(); INPUT_SURFACE_POOL_SIZE],
            )
            .map_err(|e| VaapiError::SurfaceCreateFailed(e.to_string()))?;

        // Create reconstructed surfaces (separate pool for reference tracking)
        let recon_surfaces = display
            .create_surfaces(
                VA_RT_FORMAT_YUV420,
                Some(u32::from_ne_bytes(*b"NV12")),
                width,
                height,
                Some(UsageHint::USAGE_HINT_ENCODER),
                vec![(); RECON_SURFACE_POOL_SIZE],
            )
            .map_err(|e| VaapiError::SurfaceCreateFailed(format!("reconstructed surfaces: {e}")))?;

        info!(
            "Created {} input + {} reconstructed surfaces",
            input_surfaces.len(),
            recon_surfaces.len()
        );

        // Create encode context. Surfaces are already allocated; passing None is
        // valid since the context will bind to surfaces during picture creation.
        let context = display
            .create_context(
                &config,
                width,
                height,
                None::<&Vec<Surface<()>>>,
                true, // progressive
            )
            .map_err(|e| VaapiError::ContextCreateFailed(e.to_string()))?;

        // Query NV12 image format
        let image_formats = display.query_image_formats().map_err(|e| {
            HardwareEncoderError::EncodeFailed(format!("Failed to query image formats: {e}"))
        })?;

        let nv12_format = image_formats
            .iter()
            .find(|f| f.fourcc == u32::from_ne_bytes(*b"NV12"))
            .copied()
            .ok_or_else(|| {
                HardwareEncoderError::EncodeFailed("NV12 format not supported".to_string())
            })?;

        // Calculate coded buffer size (estimate: 1.5x raw frame size for worst case)
        let coded_buffer_size = ((width * height * 3) / 2) as usize;

        // Create triple-buffered coded output
        let mut coded_buffers = Vec::with_capacity(CODED_BUFFER_COUNT);
        for i in 0..CODED_BUFFER_COUNT {
            let coded_buffer = context.create_enc_coded(coded_buffer_size).map_err(|e| {
                HardwareEncoderError::EncodeFailed(format!(
                    "Failed to create coded buffer {i}: {e}"
                ))
            })?;
            coded_buffers.push(coded_buffer);
        }

        // Calculate bitrate based on preset
        let bitrate_kbps = preset.bitrate_kbps();
        let bitrate_bps = bitrate_kbps * 1000;

        let stats = HardwareEncoderStats::new("vaapi", bitrate_kbps);

        // IDR interval based on preset
        let idr_interval = preset.gop_size();

        // Auto-select color space based on resolution (OpenH264-compatible for consistency)
        let color_space = ColorSpaceConfig::auto_select(width, height, true);

        info!(
            "VA-API encoder initialized: {}x{}, {}kbps, IDR every {} frames, \
             color_space={}, {} coded buffers",
            width,
            height,
            bitrate_kbps,
            idr_interval,
            color_space.description(),
            coded_buffers.len()
        );

        Ok(Self {
            display,
            context,
            input_surfaces,
            recon_surfaces,
            current_input_surface: 0,
            current_recon_surface: 0,
            coded_buffers,
            current_coded_buffer: 0,
            last_ref: None,
            cached_sps_pps: None,
            width,
            height,
            preset,
            frame_count: 0,
            idr_interval,
            force_idr: true, // First frame is always IDR
            stats,
            driver_name,
            device_path,
            bitrate_bps,
            nv12_format,
            color_space,
        })
    }

    fn is_idr_frame(&self) -> bool {
        self.force_idr || self.frame_count.is_multiple_of(self.idr_interval as u64)
    }

    fn get_h264_level(&self) -> u8 {
        let macroblocks = (self.width / 16) * (self.height / 16);
        let macroblocks_per_sec = macroblocks * 30; // Assume 30fps

        // Select level based on macroblock count and rate
        if macroblocks_per_sec <= 40500 {
            30 // Level 3.0 - up to 720p30
        } else if macroblocks_per_sec <= 108000 {
            31 // Level 3.1 - up to 720p60 or 1080p30
        } else if macroblocks_per_sec <= 245760 {
            40 // Level 4.0 - up to 1080p30
        } else if macroblocks_per_sec <= 589824 {
            41 // Level 4.1 - up to 1080p60
        } else if macroblocks_per_sec <= 983040 {
            50 // Level 5.0 - up to 1080p120 or 4K30
        } else {
            51 // Level 5.1 - up to 4K60
        }
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

    /// Build rate control misc parameter buffers.
    ///
    /// Configures CBR rate control with HRD/VBV compliance and framerate.
    /// These buffers are submitted once per IDR (the driver caches them).
    fn build_rate_control_buffers(&self) -> Vec<BufferType> {
        let mut buffers = Vec::with_capacity(3);

        // Rate control: CBR with per-preset QP bounds
        let (min_qp, max_qp) = match self.preset {
            QualityPreset::Speed => (20, 45),
            QualityPreset::Balanced => (18, 40),
            QualityPreset::Quality => (15, 35),
        };

        let rc = EncMiscParameterRateControl::new(
            self.bitrate_bps,                        // bits_per_second
            100,                                     // target_percentage (100% = CBR)
            1000,                                    // window_size (1 second)
            0,                                       // initial_qp (driver decides)
            min_qp,                                  // min_qp
            0,                                       // basic_unit_size (unused)
            RcFlags::new(0, 1, 0, 0, 0, 0, 0, 0, 0), // disable_frame_skip=1
            0,                                       // icq_quality_factor
            max_qp,                                  // max_qp
            0,                                       // quality_factor
            0,                                       // target_frame_size
        );
        buffers.push(BufferType::EncMiscParameter(EncMiscParameter::RateControl(
            rc,
        )));

        // HRD (Hypothetical Reference Decoder) for VBV compliance.
        // Buffer size = 1 second of data, start half-full.
        let hrd = EncMiscParameterHRD::new(
            self.bitrate_bps / 2, // initial_buffer_fullness (bits)
            self.bitrate_bps,     // buffer_size (bits)
        );
        buffers.push(BufferType::EncMiscParameter(EncMiscParameter::HRD(hrd)));

        // Framerate: 30fps
        let fr = EncMiscParameterFrameRate::new(30, 0);
        buffers.push(BufferType::EncMiscParameter(EncMiscParameter::FrameRate(
            fr,
        )));

        buffers
    }
}

impl HardwareEncoder for VaapiEncoder {
    fn encode_bgra(
        &mut self,
        bgra_data: &[u8],
        width: u32,
        height: u32,
        timestamp_ms: u64,
    ) -> HardwareEncoderResult<Option<H264Frame>> {
        let timer = EncodeTimer::start();

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

        let is_idr = self.is_idr_frame();

        // Get next input surface from pool
        let input_idx = self.current_input_surface;
        self.current_input_surface = (self.current_input_surface + 1) % self.input_surfaces.len();

        // Get next reconstructed surface (alternates between 2)
        let recon_idx = self.current_recon_surface;
        self.current_recon_surface = (self.current_recon_surface + 1) % self.recon_surfaces.len();

        // Get next coded buffer
        let coded_idx = self.current_coded_buffer;
        self.current_coded_buffer = (self.current_coded_buffer + 1) % self.coded_buffers.len();

        trace!(
            "Encoding frame {} (IDR={}) input={} recon={} coded={}",
            self.frame_count, is_idr, input_idx, recon_idx, coded_idx
        );

        // Convert BGRA to NV12 using configured color space
        let nv12_data = bgra_to_nv12(
            bgra_data,
            width as usize,
            height as usize,
            &self.color_space,
        );

        // Upload via Image API
        {
            let mut image = libva::Image::create_from(
                &self.input_surfaces[input_idx],
                self.nv12_format,
                (width, height),
                (width, height),
            )
            .map_err(|e| {
                HardwareEncoderError::EncodeFailed(format!("Failed to create image: {e}"))
            })?;

            let image_data = image.as_mut();
            let copy_len = nv12_data.len().min(image_data.len());
            image_data[..copy_len].copy_from_slice(&nv12_data[..copy_len]);
            // Image dropped here, which triggers vaPutImage to upload to surface
        }

        // Build encoding parameters
        let mb_width = width.div_ceil(16);
        let mb_height = height.div_ceil(16);
        let num_macroblocks = mb_width * mb_height;

        // Create Picture from the INPUT surface (data source)
        let mut picture = Picture::new(
            timestamp_ms,
            Rc::clone(&self.context),
            &self.input_surfaces[input_idx],
        );

        // IDR frames: submit SPS + rate control parameters
        if is_idr {
            // Reset DPB on IDR
            self.last_ref = None;

            let seq_param = self.build_sequence_params(mb_width as u16, mb_height as u16);
            let seq_buffer = self
                .context
                .create_buffer(BufferType::EncSequenceParameter(
                    EncSequenceParameter::H264(seq_param),
                ))
                .map_err(|e| {
                    HardwareEncoderError::EncodeFailed(format!("Failed to create seq buffer: {e}"))
                })?;
            picture.add_buffer(seq_buffer);

            // Submit rate control parameters on IDR (driver caches them)
            for rc_buf_type in self.build_rate_control_buffers() {
                let buf = self.context.create_buffer(rc_buf_type).map_err(|e| {
                    HardwareEncoderError::EncodeFailed(format!(
                        "Failed to create rate control buffer: {e}"
                    ))
                })?;
                picture.add_buffer(buf);
            }
        }

        // Build picture params with reference frame tracking.
        // CurrPic uses the RECONSTRUCTED surface (where the encoder writes its output).
        // reference_frames contains the previous reconstructed frame for P-frame prediction.
        let frame_num = (self.frame_count % 65536) as u16;
        let poc = (self.frame_count * 2) as i32;

        let pic_param = self.build_picture_params(
            self.recon_surfaces[recon_idx].id(),
            self.coded_buffers[coded_idx].id(),
            is_idr,
            frame_num,
            poc,
        );
        let pic_buffer = self
            .context
            .create_buffer(BufferType::EncPictureParameter(EncPictureParameter::H264(
                pic_param,
            )))
            .map_err(|e| {
                HardwareEncoderError::EncodeFailed(format!("Failed to create pic buffer: {e}"))
            })?;
        picture.add_buffer(pic_buffer);

        // Build slice params with reference list
        let slice_param = self.build_slice_params(num_macroblocks, is_idr, frame_num, poc);
        let slice_buffer = self
            .context
            .create_buffer(BufferType::EncSliceParameter(EncSliceParameter::H264(
                slice_param,
            )))
            .map_err(|e| {
                HardwareEncoderError::EncodeFailed(format!("Failed to create slice buffer: {e}"))
            })?;
        picture.add_buffer(slice_buffer);

        // Execute encoding pipeline: begin -> render -> end -> sync
        let picture = picture.begin().map_err(|e| {
            HardwareEncoderError::EncodeFailed(format!("vaBeginPicture failed: {e}"))
        })?;
        let picture = picture.render().map_err(|e| {
            HardwareEncoderError::EncodeFailed(format!("vaRenderPicture failed: {e}"))
        })?;
        let picture = picture
            .end()
            .map_err(|e| HardwareEncoderError::EncodeFailed(format!("vaEndPicture failed: {e}")))?;
        let _picture = picture.sync().map_err(|(e, _)| {
            HardwareEncoderError::EncodeFailed(format!("vaSyncSurface failed: {e}"))
        })?;

        // Update DPB: this reconstructed surface becomes the reference for the next P-frame
        self.last_ref = Some(DpbEntry {
            surface_id: self.recon_surfaces[recon_idx].id(),
            frame_num,
            poc,
        });

        // Read encoded data from coded buffer
        let mapped = MappedCodedBuffer::new(&self.coded_buffers[coded_idx]).map_err(|e| {
            HardwareEncoderError::EncodeFailed(format!("Failed to map coded buffer: {e}"))
        })?;

        let mut encoded_data = Vec::new();
        for segment in mapped.iter() {
            encoded_data.extend_from_slice(segment.buf);
        }

        // Cache SPS/PPS from IDR frames
        if is_idr && let Some(sps_pps) = Self::extract_sps_pps(&encoded_data) {
            self.cached_sps_pps = Some(sps_pps);
        }

        // Update statistics
        let encode_time_ms = timer.elapsed_ms();
        self.stats
            .record_frame(encode_time_ms, encoded_data.len(), is_idr);

        // Reset IDR flag
        if self.force_idr {
            self.force_idr = false;
        }

        let frame_size = encoded_data.len();
        self.frame_count += 1;

        Ok(Some(H264Frame {
            data: encoded_data,
            is_keyframe: is_idr,
            timestamp_ms,
            size: frame_size,
        }))
    }

    fn force_keyframe(&mut self) {
        debug!("VA-API: Forcing IDR on next frame");
        self.force_idr = true;
    }

    fn stats(&self) -> HardwareEncoderStats {
        self.stats.clone()
    }

    fn backend_name(&self) -> &'static str {
        "vaapi"
    }

    fn driver_name(&self) -> Option<&str> {
        Some(&self.driver_name)
    }

    fn supports_dynamic_resolution(&self) -> bool {
        false // VA-API requires context recreation for resolution change
    }
}

impl VaapiEncoder {
    fn build_sequence_params(
        &self,
        mb_width: u16,
        mb_height: u16,
    ) -> libva::EncSequenceParameterBufferH264 {
        use libva::{EncSequenceParameterBufferH264, H264EncSeqFields, H264VuiFields};

        let seq_fields = H264EncSeqFields::new(
            1, // chroma_format_idc (1 = 4:2:0)
            1, // frame_mbs_only_flag
            0, // mb_adaptive_frame_field_flag
            0, // seq_scaling_matrix_present_flag
            1, // direct_8x8_inference_flag
            4, // log2_max_frame_num_minus4
            0, // pic_order_cnt_type
            4, // log2_max_pic_order_cnt_lsb_minus4
            0, // delta_pic_order_always_zero_flag
        );

        let vui_fields = H264VuiFields::new(
            0,  // aspect_ratio_info_present_flag
            1,  // timing_info_present_flag
            0,  // bitstream_restriction_flag
            16, // log2_max_mv_length_horizontal
            16, // log2_max_mv_length_vertical
            0,  // fixed_frame_rate_flag
            0,  // low_delay_hrd_flag
            1,  // motion_vectors_over_pic_boundaries_flag
        );

        EncSequenceParameterBufferH264::new(
            0,                     // seq_parameter_set_id
            self.get_h264_level(), // level_idc
            self.idr_interval,     // intra_period
            self.idr_interval,     // intra_idr_period
            1,                     // ip_period
            self.bitrate_bps,      // bits_per_second
            1,                     // max_num_ref_frames
            mb_width,              // picture_width_in_mbs
            mb_height,             // picture_height_in_mbs
            &seq_fields,
            0,                // bit_depth_luma_minus8
            0,                // bit_depth_chroma_minus8
            0,                // num_ref_frames_in_pic_order_cnt_cycle
            0,                // offset_for_non_ref_pic
            0,                // offset_for_top_to_bottom_field
            [0; 256],         // offset_for_ref_frame
            None,             // frame_crop
            Some(vui_fields), // vui_fields
            0,                // aspect_ratio_idc
            1,                // sar_width
            1,                // sar_height
            1,                // num_units_in_tick
            30,               // time_scale (30 fps)
        )
    }

    fn build_picture_params(
        &self,
        recon_surface_id: u32,
        coded_buf_id: u32,
        is_idr: bool,
        frame_num: u16,
        poc: i32,
    ) -> libva::EncPictureParameterBufferH264 {
        use libva::{EncPictureParameterBufferH264, H264EncPicFields, PictureH264};

        // CurrPic: the reconstructed output surface
        let curr_pic = PictureH264::new(
            recon_surface_id,
            frame_num as u32,
            VA_PICTURE_H264_SHORT_TERM_REFERENCE,
            poc,
            poc,
        );

        // Build reference_frames (Decoded Picture Buffer)
        let mut reference_frames: [PictureH264; 16] = std::array::from_fn(|_| {
            PictureH264::new(VA_INVALID_SURFACE, 0, VA_PICTURE_H264_INVALID, 0, 0)
        });

        // For P-frames, populate slot 0 with the previous reconstructed frame
        let num_ref_l0 = if !is_idr {
            if let Some(ref entry) = self.last_ref {
                reference_frames[0] = PictureH264::new(
                    entry.surface_id,
                    entry.frame_num as u32,
                    VA_PICTURE_H264_SHORT_TERM_REFERENCE,
                    entry.poc,
                    entry.poc,
                );
                1u8
            } else {
                0u8
            }
        } else {
            0u8
        };

        let pic_fields = H264EncPicFields::new(
            if is_idr { 1 } else { 0 }, // idr_pic_flag
            1,                          // reference_pic_flag
            1,                          // entropy_coding_mode_flag (CABAC)
            0,                          // weighted_pred_flag
            0,                          // weighted_bipred_idc
            0,                          // constrained_intra_pred_flag
            1,                          // transform_8x8_mode_flag
            1,                          // deblocking_filter_control_present_flag
            0,                          // redundant_pic_cnt_present_flag
            0,                          // pic_order_present_flag
            0,                          // pic_scaling_matrix_present_flag
        );

        // QP based on preset (used as pic_init_qp, rate control overrides per-MB)
        let qp = match self.preset {
            QualityPreset::Speed => 28,
            QualityPreset::Balanced => 23,
            QualityPreset::Quality => 18,
        };

        EncPictureParameterBufferH264::new(
            curr_pic,
            reference_frames,
            coded_buf_id,
            0,                                               // pic_parameter_set_id
            0,                                               // seq_parameter_set_id
            0,                                               // last_picture
            frame_num,                                       // frame_num
            qp,                                              // pic_init_qp
            if num_ref_l0 > 0 { num_ref_l0 - 1 } else { 0 }, // num_ref_idx_l0_active_minus1
            0,                                               // num_ref_idx_l1_active_minus1
            0,                                               // chroma_qp_index_offset
            0,                                               // second_chroma_qp_index_offset
            &pic_fields,
        )
    }

    fn build_slice_params(
        &self,
        num_macroblocks: u32,
        is_idr: bool,
        frame_num: u16,
        poc: i32,
    ) -> libva::EncSliceParameterBufferH264 {
        use libva::{EncSliceParameterBufferH264, PictureH264};

        let slice_type = if is_idr { SLICE_TYPE_I } else { SLICE_TYPE_P };

        // Build reference picture list 0 (forward references for P-frames)
        let mut ref_pic_list_0: [PictureH264; 32] = std::array::from_fn(|_| {
            PictureH264::new(VA_INVALID_SURFACE, 0, VA_PICTURE_H264_INVALID, 0, 0)
        });
        let ref_pic_list_1: [PictureH264; 32] = std::array::from_fn(|_| {
            PictureH264::new(VA_INVALID_SURFACE, 0, VA_PICTURE_H264_INVALID, 0, 0)
        });

        let (num_ref_override, num_ref_l0) = if !is_idr {
            if let Some(ref entry) = self.last_ref {
                ref_pic_list_0[0] = PictureH264::new(
                    entry.surface_id,
                    entry.frame_num as u32,
                    VA_PICTURE_H264_SHORT_TERM_REFERENCE,
                    entry.poc,
                    entry.poc,
                );
                (1u8, 0u8) // override=1, l0_active_minus1=0 (one ref)
            } else {
                (0u8, 0u8)
            }
        } else {
            (0u8, 0u8)
        };

        EncSliceParameterBufferH264::new(
            0,                 // macroblock_address
            num_macroblocks,   // num_macroblocks
            VA_INVALID_ID,     // macroblock_info (not used)
            slice_type,        // slice_type
            0,                 // pic_parameter_set_id
            frame_num,         // idr_pic_id
            poc as u32 as u16, // pic_order_cnt_lsb
            0,                 // delta_pic_order_cnt_bottom
            [0, 0],            // delta_pic_order_cnt
            0,                 // direct_spatial_mv_pred_flag
            num_ref_override,  // num_ref_idx_active_override_flag
            num_ref_l0,        // num_ref_idx_l0_active_minus1
            0,                 // num_ref_idx_l1_active_minus1
            ref_pic_list_0,    // ref_pic_list_0
            ref_pic_list_1,    // ref_pic_list_1
            0,                 // luma_log2_weight_denom
            0,                 // chroma_log2_weight_denom
            0,                 // luma_weight_l0_flag
            [0; 32],           // luma_weight_l0
            [0; 32],           // luma_offset_l0
            0,                 // chroma_weight_l0_flag
            [[0; 2]; 32],      // chroma_weight_l0
            [[0; 2]; 32],      // chroma_offset_l0
            0,                 // luma_weight_l1_flag
            [0; 32],           // luma_weight_l1
            [0; 32],           // luma_offset_l1
            0,                 // chroma_weight_l1_flag
            [[0; 2]; 32],      // chroma_weight_l1
            [[0; 2]; 32],      // chroma_offset_l1
            0,                 // cabac_init_idc
            0,                 // slice_qp_delta
            0,                 // disable_deblocking_filter_idc
            0,                 // slice_alpha_c0_offset_div2
            0,                 // slice_beta_offset_div2
        )
    }
}

// =============================================================================
// NV12 Color Conversion (SIMD-optimized)
// =============================================================================

/// Convert BGRA to NV12 (Y plane + interleaved UV plane) with configurable color space
///
/// NV12 format:
/// - Y plane: width * height bytes
/// - UV plane: width * height / 2 bytes (U and V interleaved, half resolution)
///
/// Uses SIMD (AVX2 on x86_64, NEON on aarch64) for the Y plane, with scalar
/// fallback for the UV plane's 2x2 chroma subsampling.
fn bgra_to_nv12(bgra: &[u8], width: usize, height: usize, config: &ColorSpaceConfig) -> Vec<u8> {
    let y_size = width * height;
    let uv_size = (width / 2) * (height / 2) * 2;
    let mut nv12 = vec![0u8; y_size + uv_size];

    let (y_plane, uv_plane) = nv12.split_at_mut(y_size);

    let (kr, kg, kb) = config.matrix.luma_coefficients();
    let (y_min, y_max) = config.range.y_range();
    let (uv_min, uv_max) = config.range.uv_range();

    let y_scale = (y_max - y_min) as f32;
    let uv_scale = (uv_max - uv_min) as f32;
    let y_offset = y_min as f32;
    let uv_center = ((uv_min as f32 + uv_max as f32) / 2.0).round();

    // Y plane: full resolution, use SIMD where available
    bgra_to_y_plane(
        bgra, y_plane, width, height, kr, kg, kb, y_scale, y_offset, y_min, y_max,
    );

    // UV plane: half resolution with 2x2 chroma subsampling
    bgra_to_uv_plane(
        bgra, uv_plane, width, height, kr, kg, kb, uv_scale, uv_center, uv_min, uv_max,
    );

    nv12
}

/// Compute Y plane from BGRA. Dispatches to SIMD where available.
#[inline]
fn bgra_to_y_plane(
    bgra: &[u8],
    y_plane: &mut [u8],
    width: usize,
    height: usize,
    kr: f32,
    kg: f32,
    kb: f32,
    y_scale: f32,
    y_offset: f32,
    y_min: u8,
    y_max: u8,
) {
    #[cfg(target_arch = "x86_64")]
    {
        if is_x86_feature_detected!("avx2") {
            // SAFETY: AVX2 detected at runtime
            unsafe {
                bgra_to_y_plane_avx2(
                    bgra, y_plane, width, height, kr, kg, kb, y_scale, y_offset, y_min, y_max,
                );
            }
            return;
        }
    }

    #[cfg(target_arch = "aarch64")]
    {
        // SAFETY: NEON is always available on aarch64
        unsafe {
            bgra_to_y_plane_neon(
                bgra, y_plane, width, height, kr, kg, kb, y_scale, y_offset, y_min, y_max,
            );
        }
        return;
    }

    bgra_to_y_plane_scalar(
        bgra, y_plane, width, height, kr, kg, kb, y_scale, y_offset, y_min, y_max,
    );
}

/// Scalar Y plane computation
fn bgra_to_y_plane_scalar(
    bgra: &[u8],
    y_plane: &mut [u8],
    width: usize,
    height: usize,
    kr: f32,
    kg: f32,
    kb: f32,
    y_scale: f32,
    y_offset: f32,
    y_min: u8,
    y_max: u8,
) {
    for y in 0..height {
        for x in 0..width {
            let src_idx = (y * width + x) * 4;
            let b = bgra[src_idx] as f32;
            let g = bgra[src_idx + 1] as f32;
            let r = bgra[src_idx + 2] as f32;

            let y_val = y_offset + y_scale * (kr * r + kg * g + kb * b) / 255.0;
            y_plane[y * width + x] = y_val.clamp(y_min as f32, y_max as f32) as u8;
        }
    }
}

/// AVX2-optimized Y plane: processes 8 pixels per iteration
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn bgra_to_y_plane_avx2(
    bgra: &[u8],
    y_plane: &mut [u8],
    width: usize,
    height: usize,
    kr: f32,
    kg: f32,
    kb: f32,
    y_scale: f32,
    y_offset: f32,
    y_min: u8,
    y_max: u8,
) {
    use std::arch::x86_64::{
        __m128i, _mm_storeu_si128, _mm256_add_ps, _mm256_castsi256_si128, _mm256_cvtps_epi32,
        _mm256_fmadd_ps, _mm256_loadu_ps, _mm256_max_ps, _mm256_min_ps, _mm256_mul_ps,
        _mm256_packs_epi32, _mm256_packus_epi16, _mm256_set1_ps,
    };

    // SAFETY: caller guarantees AVX2 is available via is_x86_feature_detected!
    // All intrinsics operate on stack-local arrays with bounds-checked indices.
    unsafe {
        let scale_div = y_scale / 255.0;
        let vkr = _mm256_set1_ps(kr * scale_div);
        let vkg = _mm256_set1_ps(kg * scale_div);
        let vkb = _mm256_set1_ps(kb * scale_div);
        let voffset = _mm256_set1_ps(y_offset);
        let vmin = _mm256_set1_ps(y_min as f32);
        let vmax = _mm256_set1_ps(y_max as f32);

        let pixels_per_iter = 8;
        let total_pixels = width * height;
        let simd_end = total_pixels - (total_pixels % pixels_per_iter);

        for i in (0..simd_end).step_by(pixels_per_iter) {
            let base = i * 4;

            // Extract B, G, R channels from 8 BGRA pixels into separate float vectors
            let mut r_arr = [0f32; 8];
            let mut g_arr = [0f32; 8];
            let mut b_arr = [0f32; 8];
            for j in 0..8 {
                b_arr[j] = bgra[base + j * 4] as f32;
                g_arr[j] = bgra[base + j * 4 + 1] as f32;
                r_arr[j] = bgra[base + j * 4 + 2] as f32;
            }

            let vr = _mm256_loadu_ps(r_arr.as_ptr());
            let vg = _mm256_loadu_ps(g_arr.as_ptr());
            let vb = _mm256_loadu_ps(b_arr.as_ptr());

            // Y = offset + scale * (kr*R + kg*G + kb*B) / 255
            let mut vy = _mm256_mul_ps(vkr, vr);
            vy = _mm256_fmadd_ps(vkg, vg, vy);
            vy = _mm256_fmadd_ps(vkb, vb, vy);
            vy = _mm256_add_ps(vy, voffset);

            // Clamp
            vy = _mm256_max_ps(vy, vmin);
            vy = _mm256_min_ps(vy, vmax);

            // Convert to integer and store
            let yi = _mm256_cvtps_epi32(vy);

            // Pack 32-bit ints to 8-bit: epi32 -> epi16 -> epi8
            let yi16 = _mm256_packs_epi32(yi, yi);
            let yi8 = _mm256_packus_epi16(yi16, yi16);

            // Extract the lower 8 bytes we need
            let lo128 = _mm256_castsi256_si128(yi8);
            let mut out_bytes = [0u8; 16];
            _mm_storeu_si128(out_bytes.as_mut_ptr() as *mut __m128i, lo128);

            // Packing puts values at 0,1,2,3,8,9,10,11 due to 128-bit lane crossing
            y_plane[i] = out_bytes[0];
            y_plane[i + 1] = out_bytes[1];
            y_plane[i + 2] = out_bytes[2];
            y_plane[i + 3] = out_bytes[3];
            y_plane[i + 4] = out_bytes[8];
            y_plane[i + 5] = out_bytes[9];
            y_plane[i + 6] = out_bytes[10];
            y_plane[i + 7] = out_bytes[11];
        }

        // Handle remaining pixels with scalar
        for i in simd_end..total_pixels {
            let base = i * 4;
            let b = bgra[base] as f32;
            let g = bgra[base + 1] as f32;
            let r = bgra[base + 2] as f32;
            let y_val = y_offset + (kr * r + kg * g + kb * b) * scale_div;
            y_plane[i] = y_val.clamp(y_min as f32, y_max as f32) as u8;
        }
    }
}

/// NEON-optimized Y plane: processes 8 pixels per iteration
#[cfg(target_arch = "aarch64")]
unsafe fn bgra_to_y_plane_neon(
    bgra: &[u8],
    y_plane: &mut [u8],
    width: usize,
    height: usize,
    kr: f32,
    kg: f32,
    kb: f32,
    y_scale: f32,
    y_offset: f32,
    y_min: u8,
    y_max: u8,
) {
    use std::arch::aarch64::*;

    // SAFETY: NEON is always available on aarch64. All intrinsics operate on
    // stack-local arrays with bounds-checked indices.
    unsafe {
        let scale_div = y_scale / 255.0;
        let vkr = vdupq_n_f32(kr * scale_div);
        let vkg = vdupq_n_f32(kg * scale_div);
        let vkb = vdupq_n_f32(kb * scale_div);
        let voffset = vdupq_n_f32(y_offset);
        let vmin = vdupq_n_f32(y_min as f32);
        let vmax = vdupq_n_f32(y_max as f32);

        let total_pixels = width * height;
        let simd_end = total_pixels - (total_pixels % 4);

        // Process 4 pixels at a time (NEON has 128-bit vectors = 4 floats)
        for i in (0..simd_end).step_by(4) {
            let base = i * 4;

            let mut r_arr = [0f32; 4];
            let mut g_arr = [0f32; 4];
            let mut b_arr = [0f32; 4];
            for j in 0..4 {
                b_arr[j] = bgra[base + j * 4] as f32;
                g_arr[j] = bgra[base + j * 4 + 1] as f32;
                r_arr[j] = bgra[base + j * 4 + 2] as f32;
            }

            let vr = vld1q_f32(r_arr.as_ptr());
            let vg = vld1q_f32(g_arr.as_ptr());
            let vb = vld1q_f32(b_arr.as_ptr());

            // Y = offset + kr*R + kg*G + kb*B (all pre-scaled)
            let mut vy = vfmaq_f32(voffset, vkr, vr);
            vy = vfmaq_f32(vy, vkg, vg);
            vy = vfmaq_f32(vy, vkb, vb);

            // Clamp
            vy = vmaxq_f32(vy, vmin);
            vy = vminq_f32(vy, vmax);

            // Convert to u32, narrow to u16, narrow to u8
            let yi32 = vcvtnq_u32_f32(vy);
            let yi16 = vmovn_u32(yi32);
            let yi8 = vmovn_u16(vcombine_u16(yi16, yi16));

            // Store 4 bytes
            vst1_lane_u32(
                y_plane[i..].as_mut_ptr() as *mut u32,
                vreinterpret_u32_u8(yi8),
                0,
            );
        }

        // Scalar remainder
        for i in simd_end..total_pixels {
            let base = i * 4;
            let b = bgra[base] as f32;
            let g = bgra[base + 1] as f32;
            let r = bgra[base + 2] as f32;
            let y_val = y_offset + (kr * r + kg * g + kb * b) * scale_div;
            y_plane[i] = y_val.clamp(y_min as f32, y_max as f32) as u8;
        }
    }
}

/// UV plane with 2x2 chroma subsampling (scalar, since the 2x2 averaging
/// pattern doesn't vectorize as cleanly and UV is 1/4 the size of Y)
fn bgra_to_uv_plane(
    bgra: &[u8],
    uv_plane: &mut [u8],
    width: usize,
    height: usize,
    kr: f32,
    _kg: f32,
    kb: f32,
    uv_scale: f32,
    uv_center: f32,
    uv_min: u8,
    uv_max: u8,
) {
    let uv_width = width / 2;

    for y in 0..height / 2 {
        for x in 0..uv_width {
            // Average 2x2 block for chroma subsampling
            let mut b_sum = 0f32;
            let mut g_sum = 0f32;
            let mut r_sum = 0f32;

            for dy in 0..2 {
                for dx in 0..2 {
                    let src_idx = ((y * 2 + dy) * width + (x * 2 + dx)) * 4;
                    b_sum += bgra[src_idx] as f32;
                    g_sum += bgra[src_idx + 1] as f32;
                    r_sum += bgra[src_idx + 2] as f32;
                }
            }

            let b = b_sum / 4.0;
            let g = g_sum / 4.0;
            let r = r_sum / 4.0;

            // Normalized luma for chroma offset calculation
            let y_norm = (kr * r + (1.0 - kr - kb) * g + kb * b) / 255.0;

            // Cb = (B' - Y') / (2 * (1 - Kb)), Cr = (R' - Y') / (2 * (1 - Kr))
            let u_val = uv_center + (uv_scale / 2.0) * (b / 255.0 - y_norm) / (1.0 - kb);
            let v_val = uv_center + (uv_scale / 2.0) * (r / 255.0 - y_norm) / (1.0 - kr);

            let uv_idx = (y * uv_width + x) * 2;
            uv_plane[uv_idx] = u_val.clamp(uv_min as f32, uv_max as f32) as u8;
            uv_plane[uv_idx + 1] = v_val.clamp(uv_min as f32, uv_max as f32) as u8;
        }
    }
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
            prefer_nvenc: false,
        }
    }

    #[test]
    fn test_bgra_to_nv12() {
        // Create simple 4x4 test image (red)
        let bgra = vec![
            0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0,
            255, 255, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255,
            255, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255, 255,
        ];

        // Test with BT.709 limited range
        let config = ColorSpaceConfig::BT709_LIMITED;
        let nv12 = bgra_to_nv12(&bgra, 4, 4, &config);

        // Y plane should be 16 bytes, UV plane should be 8 bytes
        assert_eq!(nv12.len(), 24);

        // Red in BT.709 limited range: Y approx 63 (Kr*255 scaled to 16-235)
        for &y in &nv12[0..16] {
            assert!(
                y >= 50 && y <= 100,
                "Y value {} out of range for red in BT.709",
                y
            );
        }
    }

    #[test]
    fn test_bgra_to_nv12_different_color_spaces() {
        // Create simple 4x4 test image (green)
        let bgra = vec![
            0, 255, 0, 255, 0, 255, 0, 255, 0, 255, 0, 255, 0, 255, 0, 255, 0, 255, 0, 255, 0, 255,
            0, 255, 0, 255, 0, 255, 0, 255, 0, 255, 0, 255, 0, 255, 0, 255, 0, 255, 0, 255, 0, 255,
            0, 255, 0, 255, 0, 255, 0, 255, 0, 255, 0, 255, 0, 255, 0, 255, 0, 255, 0, 255,
        ];

        // BT.709: Kg = 0.7152, so green is brightest
        let bt709 = ColorSpaceConfig::BT709_LIMITED;
        let nv12_709 = bgra_to_nv12(&bgra, 4, 4, &bt709);

        // BT.601: Kg = 0.587, so green is still bright but different
        let bt601 = ColorSpaceConfig::BT601_LIMITED;
        let nv12_601 = bgra_to_nv12(&bgra, 4, 4, &bt601);

        // Both should have high Y values for green
        assert!(nv12_709[0] > 150, "BT.709 green Y should be high");
        assert!(nv12_601[0] > 130, "BT.601 green Y should be high");

        // BT.709 should have higher Y for green due to higher Kg coefficient
        assert!(
            nv12_709[0] > nv12_601[0],
            "BT.709 should have higher Y for green than BT.601: {} vs {}",
            nv12_709[0],
            nv12_601[0]
        );
    }

    #[test]
    fn test_extract_sps_pps() {
        // Sample SPS + PPS in Annex B format
        let data = vec![
            0x00, 0x00, 0x00, 0x01, 0x67, 0x42, 0x00, 0x1e, // SPS
            0x00, 0x00, 0x00, 0x01, 0x68, 0xce, 0x3c, 0x80, // PPS
        ];

        let sps_pps = VaapiEncoder::extract_sps_pps(&data);
        assert!(sps_pps.is_some());

        let extracted = sps_pps.unwrap();
        assert_eq!(extracted.len(), 16);
    }

    #[test]
    fn test_dpb_entry_clone() {
        let entry = DpbEntry {
            surface_id: 42,
            frame_num: 7,
            poc: 14,
        };
        let cloned = entry.clone();
        assert_eq!(cloned.surface_id, 42);
        assert_eq!(cloned.frame_num, 7);
        assert_eq!(cloned.poc, 14);
    }

    #[test]
    fn test_nv12_size() {
        // Verify NV12 output size for various resolutions
        let bgra_1080 = vec![128u8; 1920 * 1080 * 4];
        let config = ColorSpaceConfig::BT709_LIMITED;
        let nv12 = bgra_to_nv12(&bgra_1080, 1920, 1080, &config);

        let expected_y = 1920 * 1080;
        let expected_uv = (1920 / 2) * (1080 / 2) * 2;
        assert_eq!(nv12.len(), expected_y + expected_uv);
    }
}
