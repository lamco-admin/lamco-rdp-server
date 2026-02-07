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
//! Image::create_from (upload NV12 to surface)
//!       │
//!       ▼
//! Picture (typestate: New → Begin → Render → End → Sync)
//!       │
//!       ▼
//! MappedCodedBuffer (read H.264 NAL units)
//! ```
//!
//! # Thread Safety
//!
//! VA-API encoders are NOT thread-safe. The encoder must be created and used
//! on the same thread. For async usage, run encoding on a dedicated thread.

use std::path::Path;
use std::rc::Rc;

use cros_libva::{
    self as libva, BufferType, Config, Context, Display, EncCodedBuffer, EncPictureParameter,
    EncSequenceParameter, EncSliceParameter, MappedCodedBuffer, Picture, Surface, UsageHint,
    VAEntrypoint, VAImageFormat, VAProfile, VA_INVALID_ID, VA_INVALID_SURFACE,
    VA_PICTURE_H264_INVALID, VA_PICTURE_H264_SHORT_TERM_REFERENCE, VA_RT_FORMAT_YUV420,
};
use tracing::{debug, info, trace, warn};

use crate::config::HardwareEncodingConfig;
use crate::egfx::color_space::{ColorRange, ColorSpaceConfig, MatrixCoefficients};

use super::error::VaapiError;
use super::{
    EncodeTimer, H264Frame, HardwareEncoder, HardwareEncoderError, HardwareEncoderResult,
    HardwareEncoderStats, QualityPreset,
};

/// Number of surfaces in the pool for triple buffering
const SURFACE_POOL_SIZE: usize = 3;

/// H.264 slice type constants
const SLICE_TYPE_I: u8 = 2;
const SLICE_TYPE_P: u8 = 0;

/// VA-API H.264 encoder
///
/// Provides GPU-accelerated H.264 encoding for Intel and AMD GPUs.
/// Uses surface pooling and image upload for color conversion.
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

    /// Input surfaces (NV12 format)
    surfaces: Vec<Surface<()>>,

    /// Current surface index (round-robin)
    current_surface: usize,

    /// Coded buffer for output
    coded_buffer: EncCodedBuffer,

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
        if width == 0 || height == 0 || width % 2 != 0 || height % 2 != 0 {
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
            .map_err(|e| VaapiError::DisplayInitFailed(format!("{:?}", e)))?;

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

        // Create surfaces for encoding (NV12 format)
        let surfaces = display
            .create_surfaces(
                VA_RT_FORMAT_YUV420,
                Some(u32::from_ne_bytes(*b"NV12")),
                width,
                height,
                Some(UsageHint::USAGE_HINT_ENCODER),
                vec![(); SURFACE_POOL_SIZE],
            )
            .map_err(|e| VaapiError::SurfaceCreateFailed(e.to_string()))?;

        info!("Created {} encode surfaces", surfaces.len());

        // Create encode context
        let context = display
            .create_context(&config, width, height, Some(&surfaces), true)
            .map_err(|e| VaapiError::ContextCreateFailed(e.to_string()))?;

        // Query NV12 image format
        let image_formats = display.query_image_formats().map_err(|e| {
            HardwareEncoderError::EncodeFailed(format!("Failed to query image formats: {}", e))
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

        // Create coded buffer
        let coded_buffer = context.create_enc_coded(coded_buffer_size).map_err(|e| {
            HardwareEncoderError::EncodeFailed(format!("Failed to create coded buffer: {}", e))
        })?;

        // Calculate bitrate based on preset
        let bitrate_kbps = preset.bitrate_kbps();
        let bitrate_bps = bitrate_kbps * 1000;

        let stats = HardwareEncoderStats::new("vaapi", bitrate_kbps);

        // IDR interval based on preset
        let idr_interval = match preset {
            QualityPreset::Speed => 60,
            QualityPreset::Balanced => 30,
            QualityPreset::Quality => 15,
        };

        // Auto-select color space based on resolution (OpenH264-compatible for consistency)
        let color_space = ColorSpaceConfig::auto_select(width, height, true);

        info!(
            "✅ VA-API encoder initialized: {}x{}, {}kbps, IDR every {} frames, color_space={}",
            width,
            height,
            bitrate_kbps,
            idr_interval,
            color_space.description()
        );

        Ok(Self {
            display,
            context,
            surfaces,
            current_surface: 0,
            coded_buffer,
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
        self.force_idr || self.frame_count % self.idr_interval as u64 == 0
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

        // Get next surface from pool
        let surface_idx = self.current_surface;
        self.current_surface = (self.current_surface + 1) % self.surfaces.len();

        trace!(
            "Encoding frame {} (IDR={}) to surface {}",
            self.frame_count,
            is_idr,
            surface_idx
        );

        // Convert BGRA to NV12 using configured color space
        let nv12_data = bgra_to_nv12(
            bgra_data,
            width as usize,
            height as usize,
            &self.color_space,
        );

        // Upload via Image API - create image, write data, image drop calls vaPutImage
        {
            let mut image = libva::Image::create_from(
                &self.surfaces[surface_idx],
                self.nv12_format,
                (width, height),
                (width, height),
            )
            .map_err(|e| {
                HardwareEncoderError::EncodeFailed(format!("Failed to create image: {}", e))
            })?;

            // Copy NV12 data to image
            let image_data = image.as_mut();
            let copy_len = nv12_data.len().min(image_data.len());
            image_data[..copy_len].copy_from_slice(&nv12_data[..copy_len]);
            // Image dropped here, which triggers vaPutImage to upload to surface
        }

        // Build encoding parameters
        let mb_width = (width + 15) / 16;
        let mb_height = (height + 15) / 16;
        let num_macroblocks = mb_width * mb_height;

        // Create Picture and execute encode pipeline
        let mut picture = Picture::new(
            timestamp_ms,
            Rc::clone(&self.context),
            &self.surfaces[surface_idx],
        );

        // Build and add sequence params for IDR
        if is_idr {
            let seq_param = self.build_sequence_params(mb_width as u16, mb_height as u16);
            let seq_buffer = self
                .context
                .create_buffer(BufferType::EncSequenceParameter(
                    EncSequenceParameter::H264(seq_param),
                ))
                .map_err(|e| {
                    HardwareEncoderError::EncodeFailed(format!(
                        "Failed to create seq buffer: {}",
                        e
                    ))
                })?;
            picture.add_buffer(seq_buffer);
        }

        // Build and add picture params
        let pic_param = self.build_picture_params(
            self.surfaces[surface_idx].id(),
            self.coded_buffer.id(),
            is_idr,
        );
        let pic_buffer = self
            .context
            .create_buffer(BufferType::EncPictureParameter(EncPictureParameter::H264(
                pic_param,
            )))
            .map_err(|e| {
                HardwareEncoderError::EncodeFailed(format!("Failed to create pic buffer: {}", e))
            })?;
        picture.add_buffer(pic_buffer);

        // Build and add slice params
        let slice_param = self.build_slice_params(num_macroblocks, is_idr);
        let slice_buffer = self
            .context
            .create_buffer(BufferType::EncSliceParameter(EncSliceParameter::H264(
                slice_param,
            )))
            .map_err(|e| {
                HardwareEncoderError::EncodeFailed(format!("Failed to create slice buffer: {}", e))
            })?;
        picture.add_buffer(slice_buffer);

        // Execute encoding pipeline: begin → render → end → sync
        let picture = picture.begin().map_err(|e| {
            HardwareEncoderError::EncodeFailed(format!("vaBeginPicture failed: {}", e))
        })?;
        let picture = picture.render().map_err(|e| {
            HardwareEncoderError::EncodeFailed(format!("vaRenderPicture failed: {}", e))
        })?;
        let picture = picture.end().map_err(|e| {
            HardwareEncoderError::EncodeFailed(format!("vaEndPicture failed: {}", e))
        })?;
        let _picture = picture.sync().map_err(|(e, _)| {
            HardwareEncoderError::EncodeFailed(format!("vaSyncSurface failed: {}", e))
        })?;

        // Read encoded data from coded buffer
        let mapped = MappedCodedBuffer::new(&self.coded_buffer).map_err(|e| {
            HardwareEncoderError::EncodeFailed(format!("Failed to map coded buffer: {}", e))
        })?;

        let mut encoded_data = Vec::new();
        for segment in mapped.iter() {
            encoded_data.extend_from_slice(segment.buf);
        }

        // Cache SPS/PPS from IDR frames
        if is_idr {
            if let Some(sps_pps) = Self::extract_sps_pps(&encoded_data) {
                self.cached_sps_pps = Some(sps_pps);
            }
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
        surface_id: u32,
        coded_buf_id: u32,
        is_idr: bool,
    ) -> libva::EncPictureParameterBufferH264 {
        use libva::{EncPictureParameterBufferH264, H264EncPicFields, PictureH264};

        let curr_pic = PictureH264::new(
            surface_id,
            self.frame_count as u32,
            if is_idr {
                VA_PICTURE_H264_SHORT_TERM_REFERENCE
            } else {
                0
            },
            (self.frame_count * 2) as i32, // top_field_order_cnt
            (self.frame_count * 2) as i32, // bottom_field_order_cnt
        );

        // Initialize reference frames (empty for now - simple encoding without B-frames)
        let reference_frames: [PictureH264; 16] = std::array::from_fn(|_| {
            PictureH264::new(VA_INVALID_SURFACE, 0, VA_PICTURE_H264_INVALID, 0, 0)
        });

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

        // QP based on preset
        let qp = match self.preset {
            QualityPreset::Speed => 28,
            QualityPreset::Balanced => 23,
            QualityPreset::Quality => 18,
        };

        EncPictureParameterBufferH264::new(
            curr_pic,
            reference_frames,
            coded_buf_id,
            0,                       // pic_parameter_set_id
            0,                       // seq_parameter_set_id
            0,                       // last_picture
            self.frame_count as u16, // frame_num
            qp,                      // pic_init_qp
            0,                       // num_ref_idx_l0_active_minus1
            0,                       // num_ref_idx_l1_active_minus1
            0,                       // chroma_qp_index_offset
            0,                       // second_chroma_qp_index_offset
            &pic_fields,
        )
    }

    fn build_slice_params(
        &self,
        num_macroblocks: u32,
        is_idr: bool,
    ) -> libva::EncSliceParameterBufferH264 {
        use libva::{EncSliceParameterBufferH264, PictureH264};

        // Initialize empty reference lists
        // Create two separate ref lists since PictureH264 doesn't implement Clone
        let ref_pic_list_0: [PictureH264; 32] = std::array::from_fn(|_| {
            PictureH264::new(VA_INVALID_SURFACE, 0, VA_PICTURE_H264_INVALID, 0, 0)
        });
        let ref_pic_list_1: [PictureH264; 32] = std::array::from_fn(|_| {
            PictureH264::new(VA_INVALID_SURFACE, 0, VA_PICTURE_H264_INVALID, 0, 0)
        });

        let slice_type = if is_idr { SLICE_TYPE_I } else { SLICE_TYPE_P };

        EncSliceParameterBufferH264::new(
            0,                             // macroblock_address
            num_macroblocks,               // num_macroblocks
            VA_INVALID_ID,                 // macroblock_info (not used)
            slice_type,                    // slice_type
            0,                             // pic_parameter_set_id
            self.frame_count as u16,       // idr_pic_id
            (self.frame_count * 2) as u16, // pic_order_cnt_lsb
            0,                             // delta_pic_order_cnt_bottom
            [0, 0],                        // delta_pic_order_cnt
            0,                             // direct_spatial_mv_pred_flag
            0,                             // num_ref_idx_active_override_flag
            0,                             // num_ref_idx_l0_active_minus1
            0,                             // num_ref_idx_l1_active_minus1
            ref_pic_list_0,                // ref_pic_list_0
            ref_pic_list_1,                // ref_pic_list_1
            0,                             // luma_log2_weight_denom
            0,                             // chroma_log2_weight_denom
            0,                             // luma_weight_l0_flag
            [0; 32],                       // luma_weight_l0
            [0; 32],                       // luma_offset_l0
            0,                             // chroma_weight_l0_flag
            [[0; 2]; 32],                  // chroma_weight_l0
            [[0; 2]; 32],                  // chroma_offset_l0
            0,                             // luma_weight_l1_flag
            [0; 32],                       // luma_weight_l1
            [0; 32],                       // luma_offset_l1
            0,                             // chroma_weight_l1_flag
            [[0; 2]; 32],                  // chroma_weight_l1
            [[0; 2]; 32],                  // chroma_offset_l1
            0,                             // cabac_init_idc
            0,                             // slice_qp_delta
            0,                             // disable_deblocking_filter_idc
            0,                             // slice_alpha_c0_offset_div2
            0,                             // slice_beta_offset_div2
        )
    }
}

/// Convert BGRA to NV12 (Y plane + interleaved UV plane) with configurable color space
///
/// NV12 format:
/// - Y plane: width * height bytes
/// - UV plane: width * height / 2 bytes (U and V interleaved, half resolution)
///
/// # Color Space Support
///
/// Uses the matrix coefficients from the color space config to select the
/// appropriate RGB→YCbCr conversion formula. Supports:
/// - BT.709 (HD content, default for ≥720p)
/// - BT.601 (SD content, default for <720p)
/// - BT.2020 (UHD content)
///
/// Range is also configurable (limited 16-235 vs full 0-255).
fn bgra_to_nv12(bgra: &[u8], width: usize, height: usize, config: &ColorSpaceConfig) -> Vec<u8> {
    let y_size = width * height;
    let uv_size = (width / 2) * (height / 2) * 2;
    let mut nv12 = vec![0u8; y_size + uv_size];

    // Split into Y and UV planes
    let (y_plane, uv_plane) = nv12.split_at_mut(y_size);

    // Get luma coefficients from color space config (Kr, Kg, Kb)
    let (kr, kg, kb) = config.matrix.luma_coefficients();

    // Get Y and UV range based on config
    let (y_min, y_max) = config.range.y_range();
    let (uv_min, uv_max) = config.range.uv_range();

    // Calculate range scale factors
    let y_scale = (y_max - y_min) as f32;
    let uv_scale = (uv_max - uv_min) as f32;
    let y_offset = y_min as f32;
    let uv_center = ((uv_min as f32 + uv_max as f32) / 2.0).round();

    // Process Y plane (full resolution)
    for y in 0..height {
        for x in 0..width {
            let src_idx = (y * width + x) * 4;
            let b = bgra[src_idx] as f32;
            let g = bgra[src_idx + 1] as f32;
            let r = bgra[src_idx + 2] as f32;

            // Y = Kr*R + Kg*G + Kb*B, scaled to configured range
            let y_val = y_offset + y_scale * (kr * r + kg * g + kb * b) / 255.0;
            y_plane[y * width + x] = y_val.clamp(y_min as f32, y_max as f32) as u8;
        }
    }

    // Process UV plane (half resolution, interleaved)
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

            // Y component (for chroma calculation, normalized 0-1)
            let y_norm = (kr * r + kg * g + kb * b) / 255.0;

            // Cb = (B' - Y') / (2 * (1 - Kb))
            // Cr = (R' - Y') / (2 * (1 - Kr))
            // Scaled to configured UV range
            let u_val = uv_center + (uv_scale / 2.0) * (b / 255.0 - y_norm) / (1.0 - kb);
            let v_val = uv_center + (uv_scale / 2.0) * (r / 255.0 - y_norm) / (1.0 - kr);

            let uv_idx = (y * uv_width + x) * 2;
            uv_plane[uv_idx] = u_val.clamp(uv_min as f32, uv_max as f32) as u8;
            uv_plane[uv_idx + 1] = v_val.clamp(uv_min as f32, uv_max as f32) as u8;
        }
    }

    nv12
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

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

        // Test with BT.709 (default for HD)
        let config = ColorSpaceConfig::from_preset(ColorSpacePreset::BT709);
        let nv12 = bgra_to_nv12(&bgra, 4, 4, &config);

        // Y plane should be 16 bytes, UV plane should be 8 bytes
        assert_eq!(nv12.len(), 24);

        // Red in BT.709 limited range: Y ≈ 63 (Kr*255 scaled to 16-235)
        // Check Y values are in reasonable range for red
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
        let bt709 = ColorSpaceConfig::from_preset(ColorSpacePreset::BT709);
        let nv12_709 = bgra_to_nv12(&bgra, 4, 4, &bt709);

        // BT.601: Kg = 0.587, so green is still bright but different
        let bt601 = ColorSpaceConfig::from_preset(ColorSpacePreset::BT601);
        let nv12_601 = bgra_to_nv12(&bgra, 4, 4, &bt601);

        // Both should have high Y values for green
        assert!(nv12_709[0] > 150, "BT.709 green Y should be high");
        assert!(nv12_601[0] > 130, "BT.601 green Y should be high");

        // BT.709 should have higher Y for green due to higher Kg coefficient
        // (Green gets more weight in luma calculation)
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
}
