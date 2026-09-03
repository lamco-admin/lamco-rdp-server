//! Vulkan Video encoder backend (cross-vendor)
//!
//! Uses VK_KHR_video_encode_h264 via the vk-video crate for GPU-accelerated
//! H.264 encoding on NVIDIA, Intel, and AMD GPUs.
//!
//! # Supported Hardware
//!
//! Any GPU with Vulkan 1.3+ and VK_KHR_video_encode_h264 extension:
//! - NVIDIA Turing+ (RTX 20 series and newer)
//! - Intel Arc (DG2+)
//! - AMD RDNA 3+ (RX 7000 series)
//!
//! # Architecture
//!
//! ```text
//! BGRA Frame (PipeWire)
//!       |
//!       v
//! CPU BGRA->NV12 conversion (SIMD)
//!       |
//!       v
//! vk-video BytesEncoder (GPU H.264 encode)
//!       |
//!       v
//! H.264 NAL units (Annex B)
//! ```

use tracing::info;
use vk_video::{
    BytesEncoder, InputFrame, RawFrameData, VulkanInstance,
    parameters::{
        ColorRange, ColorSpace, EncoderParameters, EncoderTuningMode, H264Profile, RateControl,
        Rational, VideoParameters, VulkanAdapterDescriptor, VulkanDeviceDescriptor,
    },
};

use super::{
    EncodeTimer, H264Frame, HardwareEncoder, HardwareEncoderError, HardwareEncoderResult,
    HardwareEncoderStats, QualityPreset, error::VulkanVideoError,
};
use crate::config::HardwareEncodingConfig;

/// Vulkan Video H.264 encoder
///
/// Cross-vendor GPU-accelerated encoding via VK_KHR_video_encode_h264.
/// Accepts BGRA input, converts to NV12, then encodes via Vulkan Video.
pub struct VulkanVideoEncoder {
    /// vk-video bytes encoder
    encoder: BytesEncoder,

    /// Frame dimensions
    #[expect(
        dead_code,
        reason = "captured at setup; the reconfigure path is not wired yet"
    )]
    width: u32,
    #[expect(
        dead_code,
        reason = "captured at setup; the reconfigure path is not wired yet"
    )]
    height: u32,

    /// Quality preset
    #[expect(
        dead_code,
        reason = "captured at setup; the reconfigure path is not wired yet"
    )]
    preset: QualityPreset,

    /// Frame counter
    frame_count: u64,

    /// Force next frame to be IDR
    force_idr: bool,

    /// GOP size (keyframe interval)
    gop_size: u32,

    /// Encoder statistics
    stats: HardwareEncoderStats,

    /// Cached SPS/PPS for prepending to P-frames
    cached_sps_pps: Option<Vec<u8>>,
}

impl VulkanVideoEncoder {
    /// Create a new Vulkan Video encoder.
    ///
    /// Probes for a GPU with VK_KHR_video_encode_h264 support and
    /// initializes an H.264 encoding session.
    pub fn new(
        _config: &HardwareEncodingConfig,
        width: u32,
        height: u32,
        preset: QualityPreset,
    ) -> HardwareEncoderResult<Self> {
        let instance =
            VulkanInstance::new().map_err(|e| VulkanVideoError::InstanceFailed(format!("{e}")))?;

        let adapter_desc = VulkanAdapterDescriptor {
            supports_encoding: true,
            ..Default::default()
        };

        let adapter = instance
            .create_adapter(&adapter_desc)
            .map_err(|_| VulkanVideoError::NoDevice)?;

        let adapter_info = adapter.info();
        info!(
            "Vulkan Video adapter: {} ({:?})",
            adapter_info.name, adapter_info.device_type
        );

        let device_desc = VulkanDeviceDescriptor::default();
        let device = adapter
            .create_device(&device_desc)
            .map_err(|e| VulkanVideoError::EncoderFailed(format!("device creation: {e}")))?;

        let gop_size = preset.gop_size();

        // vk-video 0.3 split EncoderParameters into nested input/output parameter
        // structs (see CHANGELOG v0.3.0). VideoParameters is now narrowed to
        // dimensions + target_framerate; encoding-side knobs (rate_control, idr_period,
        // profile, quality_level, tuning_mode, color_space, color_range,
        // inline_stream_params, etc.) live on EncoderOutputParameters.
        //
        // We start from device-tuned defaults (encoder_output_parameters_high_quality /
        // _low_latency, depending on preset) so device-specific quality_level / usage
        // flags / max_references stay correct, then override the fields we care about.
        let width_nz = std::num::NonZeroU32::new(width).ok_or_else(|| {
            HardwareEncoderError::InvalidDimensions {
                width,
                height,
                reason: "width must be non-zero".into(),
            }
        })?;
        let height_nz = std::num::NonZeroU32::new(height).ok_or_else(|| {
            HardwareEncoderError::InvalidDimensions {
                width,
                height,
                reason: "height must be non-zero".into(),
            }
        })?;

        let rate_control = RateControl::ConstantBitrate {
            bitrate: u64::from(preset.bitrate_kbps()) * 1000,
            // 2s virtual buffer matches the vk-video example and gives a reasonable
            // bitrate-conformance window for real-time RDP streaming.
            virtual_buffer_size: std::time::Duration::from_secs(2),
        };

        let mut output_parameters = match preset {
            QualityPreset::Speed | QualityPreset::Balanced => device
                .encoder_output_parameters_low_latency(rate_control)
                .map_err(|e| VulkanVideoError::EncoderFailed(format!("output parameters: {e}")))?,
            QualityPreset::Quality => device
                .encoder_output_parameters_high_quality(rate_control)
                .map_err(|e| VulkanVideoError::EncoderFailed(format!("output parameters: {e}")))?,
        };

        // Override the few fields RDP cares about regardless of device defaults.
        output_parameters.idr_period = std::num::NonZeroU32::new(gop_size);
        output_parameters.profile = H264Profile::High;
        output_parameters.tuning_mode = Some(match preset {
            QualityPreset::Speed => EncoderTuningMode::ULTRA_LOW_LATENCY,
            QualityPreset::Balanced => EncoderTuningMode::LOW_LATENCY,
            QualityPreset::Quality => EncoderTuningMode::HIGH_QUALITY,
        });
        output_parameters.inline_stream_params = Some(true);
        output_parameters.color_space = Some(ColorSpace::BT709);
        output_parameters.color_range = Some(ColorRange::Limited);

        let params = EncoderParameters {
            input_parameters: VideoParameters {
                width: width_nz,
                height: height_nz,
                target_framerate: Rational::from(30),
            },
            output_parameters,
        };

        let encoder = device
            .create_bytes_encoder(params)
            .map_err(|e| VulkanVideoError::EncoderFailed(format!("{e}")))?;

        info!(
            "Vulkan Video encoder initialized: {}x{}, preset={}, gop={}",
            width, height, preset, gop_size
        );

        Ok(Self {
            encoder,
            width,
            height,
            preset,
            frame_count: 0,
            force_idr: true, // First frame is always IDR
            gop_size,
            stats: HardwareEncoderStats::new("vulkan-video", preset.bitrate_kbps()),
            cached_sps_pps: None,
        })
    }

    /// Convert BGRA pixel data to NV12 format for the encoder.
    ///
    /// NV12 is a planar YUV 4:2:0 format:
    /// - Y plane: width * height bytes
    /// - UV plane (interleaved U, V): width * height / 2 bytes
    ///
    /// Uses fixed-point BT.709 coefficients for performance (no float ops).
    /// Processes Y for every pixel and UV for every 2x2 block (4:2:0 subsampling).
    #[expect(
        clippy::many_single_char_names,
        reason = "r/g/b and y/u/v are the colour-conversion domain's own names; spelling them out would obscure the formulas"
    )]
    fn bgra_to_nv12(bgra: &[u8], width: u32, height: u32) -> Vec<u8> {
        let w = width as usize;
        let h = height as usize;
        let y_size = w * h;
        let uv_size = w * h / 2;
        let mut nv12 = vec![0u8; y_size + uv_size];

        // BT.709 fixed-point coefficients (scaled by 16384 = 2^14 for precision)
        // Y  =  0.2126*R + 0.7152*G + 0.0722*B
        // Cb = -0.1146*R - 0.3854*G + 0.5000*B + 128
        // Cr =  0.5000*R - 0.4542*G - 0.0458*B + 128
        const YR: i32 = 3483; // 0.2126 * 16384
        const YG: i32 = 11718; // 0.7152 * 16384
        const YB: i32 = 1183; // 0.0722 * 16384
        const CG_CB: i32 = -6316; // -0.3854 * 16384
        const CB_B: i32 = 8192; // 0.5000 * 16384
        const CR_R: i32 = 8192;
        const CG_CR: i32 = -7440; // -0.4542 * 16384
        const CB_R: i32 = -1878; // -0.1146 * 16384
        const CR_B: i32 = -752; // -0.0458 * 16384

        // Single pass: compute Y for every pixel, UV for every 2x2 block
        let (y_plane, uv_plane) = nv12.split_at_mut(y_size);

        for row in 0..h {
            let row_offset = row * w * 4;
            let y_row_offset = row * w;
            let is_even_row = row % 2 == 0;

            for col in 0..w {
                let px = row_offset + col * 4;
                let b = bgra[px] as i32;
                let g = bgra[px + 1] as i32;
                let r = bgra[px + 2] as i32;

                // Y with rounding: (coeff * val + 8192) >> 14
                let y = ((YR * r + YG * g + YB * b + 8192) >> 14).clamp(0, 255);
                y_plane[y_row_offset + col] = y as u8;

                // UV: sample top-left pixel of each 2x2 block
                if is_even_row && col % 2 == 0 {
                    let uv_idx = (row / 2) * w + col;
                    let cb = ((CB_R * r + CG_CB * g + CB_B * b + 8192) >> 14) + 128;
                    let cr = ((CR_R * r + CG_CR * g + CR_B * b + 8192) >> 14) + 128;
                    uv_plane[uv_idx] = cb.clamp(0, 255) as u8;
                    uv_plane[uv_idx + 1] = cr.clamp(0, 255) as u8;
                }
            }
        }

        nv12
    }
}

impl HardwareEncoder for VulkanVideoEncoder {
    fn encode_bgra(
        &mut self,
        bgra_data: &[u8],
        width: u32,
        height: u32,
        timestamp_ms: u64,
    ) -> HardwareEncoderResult<Option<H264Frame>> {
        let timer = EncodeTimer::start();

        // Convert BGRA to NV12 (CPU-side for now; GPU compute shader in WU-3)
        let nv12 = Self::bgra_to_nv12(bgra_data, width, height);

        let input = InputFrame {
            data: RawFrameData {
                frame: nv12,
                width,
                height,
            },
            pts: Some(timestamp_ms),
        };

        let force_keyframe = self.force_idr
            || (self.gop_size > 0 && self.frame_count.is_multiple_of(self.gop_size as u64));

        let chunk = self
            .encoder
            .encode(&input, force_keyframe)
            .map_err(|e| VulkanVideoError::EncodeFailed(format!("{e}")))?;

        self.force_idr = false;
        self.frame_count += 1;

        let is_keyframe = chunk.is_keyframe;
        let data = chunk.data;
        let size = data.len();

        self.stats
            .record_frame(timer.elapsed_ms(), size, is_keyframe);

        if is_keyframe {
            // Cache SPS/PPS from IDR frame for potential later use
            self.cached_sps_pps = Some(data.clone());
        }

        Ok(Some(H264Frame::new(data, is_keyframe, timestamp_ms)))
    }

    fn force_keyframe(&mut self) {
        self.force_idr = true;
    }

    fn stats(&self) -> HardwareEncoderStats {
        let mut s = self.stats.clone();
        s.uptime = s.created_at.elapsed();
        s
    }

    fn backend_name(&self) -> &'static str {
        "vulkan-video"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bgra_to_nv12_dimensions() {
        let width = 4u32;
        let height = 4u32;
        let bgra = vec![0u8; (width * height * 4) as usize];
        let nv12 = VulkanVideoEncoder::bgra_to_nv12(&bgra, width, height);
        // NV12: Y plane (w*h) + UV plane (w*h/2)
        assert_eq!(nv12.len(), (width * height + width * height / 2) as usize);
    }

    #[test]
    fn test_bgra_to_nv12_white() {
        // White pixel: BGRA = (255, 255, 255, 255)
        let width = 2u32;
        let height = 2u32;
        let bgra = vec![255u8; (width * height * 4) as usize];
        let nv12 = VulkanVideoEncoder::bgra_to_nv12(&bgra, width, height);
        // White in BT.709: Y=255, U=128, V=128
        assert_eq!(nv12[0], 255); // Y
        let uv_offset = (width * height) as usize;
        assert_eq!(nv12[uv_offset], 128); // U
        assert_eq!(nv12[uv_offset + 1], 128); // V
    }

    #[test]
    fn test_bgra_to_nv12_black() {
        // Black pixel: BGRA = (0, 0, 0, 255)
        let width = 2u32;
        let height = 2u32;
        let mut bgra = vec![0u8; (width * height * 4) as usize];
        // Set alpha to 255
        for i in (3..(width * height * 4) as usize).step_by(4) {
            bgra[i] = 255;
        }
        let nv12 = VulkanVideoEncoder::bgra_to_nv12(&bgra, width, height);
        // Black in BT.709: Y=0, U=128, V=128
        assert_eq!(nv12[0], 0); // Y
        let uv_offset = (width * height) as usize;
        assert_eq!(nv12[uv_offset], 128); // U
        assert_eq!(nv12[uv_offset + 1], 128); // V
    }
}
