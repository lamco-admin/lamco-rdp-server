//! Hardware-accelerated video encoding abstraction
//!
//! This module provides a unified interface for GPU-accelerated H.264 encoding,
//! supporting multiple backends:
//!
//! - **VA-API** (`vaapi` feature): Intel and AMD GPUs on Linux
//! - **NVENC** (`nvenc` feature): NVIDIA GPUs via Video Codec SDK
//!
//! # Architecture
//!
//! ```text
//! ┌─────────────────────────────────────────────────────┐
//! │              HardwareEncoder Trait                   │
//! │  encode_bgra() | force_keyframe() | stats()          │
//! └─────────────────────────────────────────────────────┘
//!                          │
//!          ┌───────────────┼───────────────┐
//!          ▼               ▼               ▼
//!    ┌──────────┐   ┌──────────┐   ┌──────────────┐
//!    │  VAAPI   │   │  NVENC   │   │ Future: VK   │
//!    │ Encoder  │   │ Encoder  │   │   Video      │
//!    └──────────┘   └──────────┘   └──────────────┘
//!          │               │
//!          ▼               ▼
//!    Intel/AMD GPU    NVIDIA GPU
//! ```
//!
//! # Usage
//!
//! ```rust,ignore
//! use lamco_rdp_server::egfx::hardware::{create_hardware_encoder, HardwareEncoder};
//!
//! // Create encoder with automatic backend selection
//! let mut encoder = create_hardware_encoder(&config, 1920, 1080)?;
//!
//! // Encode frames
//! if let Some(frame) = encoder.encode_bgra(&bgra_data, 1920, 1080, timestamp)? {
//!     // Send frame.data to RDP client
//! }
//! ```
//!
//! # Feature Flags
//!
//! - `vaapi`: Enable VA-API backend (Intel/AMD)
//! - `nvenc`: Enable NVENC backend (NVIDIA)
//! - `hardware-encoding`: Enable both backends

mod error;
mod factory;
mod stats;

#[cfg(feature = "vaapi")]
pub mod vaapi;

#[cfg(feature = "nvenc")]
pub mod nvenc;

// Re-exports
#[cfg(feature = "nvenc")]
pub use error::NvencError;
#[cfg(feature = "vaapi")]
pub use error::VaapiError;
pub use error::{HardwareEncoderError, HardwareEncoderResult};
pub use factory::create_hardware_encoder;
pub use stats::{EncodeTimer, HardwareEncoderStats};

use crate::config::HardwareEncodingConfig;

/// Encoded H.264 frame from hardware encoder
///
/// Contains the encoded bitstream in Annex B format, ready for
/// transmission via EGFX AVC420/AVC444 codec.
#[derive(Debug, Clone)]
pub struct H264Frame {
    /// Encoded NAL units in Annex B format (with start codes)
    ///
    /// Includes SPS/PPS for keyframes, prepended for P-frames
    pub data: Vec<u8>,

    /// Whether this is a keyframe (IDR)
    pub is_keyframe: bool,

    /// Frame timestamp in milliseconds
    pub timestamp_ms: u64,

    /// Encoded frame size in bytes
    pub size: usize,
}

impl H264Frame {
    pub fn new(data: Vec<u8>, is_keyframe: bool, timestamp_ms: u64) -> Self {
        let size = data.len();
        Self {
            data,
            is_keyframe,
            timestamp_ms,
            size,
        }
    }
}

/// Unified hardware encoder interface
///
/// This trait defines the common API for all hardware encoding backends.
/// Implementations handle:
/// - Color conversion (BGRA → NV12)
/// - H.264 encoding
/// - SPS/PPS management
/// - Rate control
///
/// # Thread Safety
///
/// Implementations are NOT required to be `Send`. Hardware encoders typically
/// use thread-local resources (VA display handles, CUDA contexts) that cannot
/// be safely moved between threads. The encoder should be created and used
/// on the same thread.
///
/// For async usage, wrap the encoder in a dedicated encoding thread and
/// communicate via channels.
///
/// # Error Handling
///
/// All operations return `HardwareEncoderResult` which wraps backend-specific
/// errors into a unified error type.
pub trait HardwareEncoder {
    /// # Performance
    ///
    /// This method performs:
    /// 1. BGRA → NV12 color conversion (GPU-accelerated)
    /// 2. H.264 encoding (GPU-accelerated)
    /// 3. SPS/PPS extraction/caching for IDR frames
    /// 4. SPS/PPS prepending for P-frames
    ///
    /// Typical latency: 1-5ms for 1080p depending on GPU
    fn encode_bgra(
        &mut self,
        bgra_data: &[u8],
        width: u32,
        height: u32,
        timestamp_ms: u64,
    ) -> HardwareEncoderResult<Option<H264Frame>>;

    /// Force the next frame to be a keyframe (IDR)
    ///
    /// Called when:
    /// - Client requests refresh
    /// - Resolution changes
    /// - Long time since last keyframe
    /// - Error recovery needed
    fn force_keyframe(&mut self);

    /// Get encoder statistics
    ///
    /// Returns cumulative statistics since encoder creation.
    /// Statistics are updated after each `encode_bgra()` call.
    fn stats(&self) -> HardwareEncoderStats;

    /// Get the backend name for logging
    ///
    /// Returns a static string identifying the backend:
    /// - `"vaapi"` for VA-API
    /// - `"nvenc"` for NVENC
    fn backend_name(&self) -> &'static str;

    /// Check if encoder supports dynamic resolution changes
    ///
    /// Most hardware encoders require recreation for resolution changes.
    /// Returns `false` by default.
    fn supports_dynamic_resolution(&self) -> bool {
        false
    }

    /// Reconfigure encoder for new resolution
    ///
    /// Only valid if `supports_dynamic_resolution()` returns `true`.
    /// Otherwise returns `ReconfigureNotSupported` error.
    ///
    /// # Arguments
    ///
    /// * `width` - New frame width
    /// * `height` - New frame height
    fn reconfigure(&mut self, _width: u32, _height: u32) -> HardwareEncoderResult<()> {
        Err(HardwareEncoderError::ReconfigureNotSupported {
            backend: self.backend_name(),
        })
    }

    /// Get the VA-API driver name (VA-API only)
    ///
    /// Returns `None` for non-VA-API backends.
    fn driver_name(&self) -> Option<&str> {
        None
    }

    /// Flush pending frames from encoder
    ///
    /// Called before encoder destruction or when switching modes.
    /// Default implementation does nothing.
    fn flush(&mut self) -> HardwareEncoderResult<()> {
        Ok(())
    }
}

/// Quality preset for hardware encoding
///
/// Maps to backend-specific encoder configurations
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum QualityPreset {
    /// Fastest encoding, lowest quality
    /// - VAAPI: QP 20-40, GOP 60
    /// - NVENC: P2, UltraLowLatency tuning
    Speed,

    /// Balanced quality and performance (default)
    /// - VAAPI: QP 18-36, GOP 30
    /// - NVENC: P4, LowLatency tuning
    #[default]
    Balanced,

    /// Highest quality, slower encoding
    /// - VAAPI: QP 15-30, GOP 15
    /// - NVENC: P6, Default tuning
    Quality,
}

impl QualityPreset {
    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "speed" | "fast" => Some(Self::Speed),
            "balanced" | "default" | "medium" => Some(Self::Balanced),
            "quality" | "slow" | "high" => Some(Self::Quality),
            _ => None,
        }
    }

    pub fn bitrate_kbps(&self) -> u32 {
        match self {
            Self::Speed => 3000,
            Self::Balanced => 5000,
            Self::Quality => 10000,
        }
    }

    pub fn gop_size(&self) -> u32 {
        match self {
            Self::Speed => 60,
            Self::Balanced => 30,
            Self::Quality => 15,
        }
    }
}

impl std::fmt::Display for QualityPreset {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Speed => write!(f, "speed"),
            Self::Balanced => write!(f, "balanced"),
            Self::Quality => write!(f, "quality"),
        }
    }
}

#[inline]
pub const fn is_hardware_encoding_available() -> bool {
    cfg!(any(feature = "vaapi", feature = "nvenc"))
}

pub fn available_backends() -> Vec<&'static str> {
    let mut backends = Vec::new();

    #[cfg(feature = "vaapi")]
    backends.push("vaapi");

    #[cfg(feature = "nvenc")]
    backends.push("nvenc");

    backends
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_quality_preset_from_str() {
        assert_eq!(QualityPreset::from_str("speed"), Some(QualityPreset::Speed));
        assert_eq!(
            QualityPreset::from_str("BALANCED"),
            Some(QualityPreset::Balanced)
        );
        assert_eq!(
            QualityPreset::from_str("Quality"),
            Some(QualityPreset::Quality)
        );
        assert_eq!(QualityPreset::from_str("invalid"), None);
    }

    #[test]
    fn test_quality_preset_bitrate() {
        assert_eq!(QualityPreset::Speed.bitrate_kbps(), 3000);
        assert_eq!(QualityPreset::Balanced.bitrate_kbps(), 5000);
        assert_eq!(QualityPreset::Quality.bitrate_kbps(), 10000);
    }

    #[test]
    fn test_h264_frame_new() {
        let data = vec![0x00, 0x00, 0x00, 0x01, 0x67];
        let frame = H264Frame::new(data.clone(), true, 1000);
        assert_eq!(frame.size, 5);
        assert!(frame.is_keyframe);
        assert_eq!(frame.timestamp_ms, 1000);
    }

    #[test]
    fn test_available_backends() {
        let backends = available_backends();
        // At least empty without features, or contains expected backends
        assert!(backends.len() <= 2);
    }
}
