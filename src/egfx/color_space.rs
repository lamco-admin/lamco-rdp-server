//! Unified Color Space Configuration
//!
//! This module provides a single source of truth for color space handling
//! across all encoder backends (OpenH264, VA-API, NVENC).
//!
//! # Problem Solved
//!
//! Different encoding paths historically handled color differently:
//! - AVC420: OpenH264 internal BT.601 limited range
//! - AVC444: Our conversion with BT.709 full range
//! - VA-API: Hardcoded BT.709
//!
//! This module unifies color handling by:
//! 1. Defining VUI (Video Usability Information) parameters
//! 2. Providing consistent coefficients across all paths
//! 3. Ensuring decoder receives correct color metadata
//!
//! # H.264 VUI Reference
//!
//! VUI parameters are embedded in SPS (Sequence Parameter Set) and tell
//! decoders how to interpret color data. Key fields:
//!
//! | Field | Purpose |
//! |-------|---------|
//! | `colour_primaries` | Which RGB primaries (BT.709=1, BT.601=6) |
//! | `transfer_characteristics` | Gamma/OETF (BT.709=1, sRGB=13) |
//! | `matrix_coefficients` | RGB↔YUV conversion matrix |
//! | `video_full_range_flag` | Limited (0: 16-235) vs Full (1: 0-255) |

use crate::egfx::color_convert::ColorMatrix;
use serde::{Deserialize, Serialize};

/// Color range for Y'CbCr encoding
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ColorRange {
    /// Limited/TV range: Y 16-235, Cb/Cr 16-240
    /// Compatible with broadcast standards
    #[default]
    Limited,
    /// Full/PC range: Y 0-255, Cb/Cr 0-255
    /// Maximum dynamic range for computer graphics
    Full,
}

impl ColorRange {
    /// H.264 VUI video_full_range_flag value
    pub const fn vui_flag(&self) -> u8 {
        match self {
            ColorRange::Limited => 0,
            ColorRange::Full => 1,
        }
    }
}

/// H.264 VUI colour_primaries values (ITU-T H.264 Table E-3)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ColourPrimaries {
    /// BT.709 (HD content)
    BT709 = 1,
    /// Unspecified (default)
    Unspecified = 2,
    /// BT.601 NTSC (SMPTE 170M)
    BT601NTSC = 6,
    /// BT.601 PAL (BT.470 BG)
    BT601PAL = 5,
    /// BT.2020 (UHD/HDR)
    BT2020 = 9,
}

/// H.264 VUI transfer_characteristics values (ITU-T H.264 Table E-4)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum TransferCharacteristics {
    /// BT.709 transfer
    BT709 = 1,
    /// Unspecified (default)
    Unspecified = 2,
    /// BT.601 (same as SMPTE 170M)
    BT601 = 6,
    /// sRGB (IEC 61966-2-1)
    SRGB = 13,
    /// BT.2020 10-bit
    BT2020_10 = 14,
    /// BT.2020 12-bit
    BT2020_12 = 15,
}

/// H.264 VUI matrix_coefficients values (ITU-T H.264 Table E-5)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum MatrixCoefficients {
    /// BT.709
    BT709 = 1,
    /// Unspecified (default)
    Unspecified = 2,
    /// BT.601 (SMPTE 170M)
    BT601 = 6,
    /// BT.2020 non-constant luminance
    BT2020NCL = 9,
}

/// Complete color space configuration for H.264 encoding
///
/// This struct provides:
/// 1. The color matrix for RGB↔YUV conversion
/// 2. The value range (full vs limited)
/// 3. H.264 VUI parameters for decoder signaling
///
/// # Presets
///
/// Use the provided presets for common scenarios:
/// - [`ColorSpaceConfig::OPENH264_COMPATIBLE`] - Match OpenH264 internal conversion
/// - [`ColorSpaceConfig::BT709_LIMITED`] - HD content with broadcast range
/// - [`ColorSpaceConfig::BT709_FULL`] - HD content with full range
/// - [`ColorSpaceConfig::BT601_LIMITED`] - SD content with broadcast range
///
/// # Example
///
/// ```
/// use lamco_rdp_server::egfx::color_space::ColorSpaceConfig;
///
/// // For consistency with AVC420 (OpenH264):
/// let config = ColorSpaceConfig::OPENH264_COMPATIBLE;
///
/// // For high-quality 4:4:4 (when VUI is properly signaled):
/// let config = ColorSpaceConfig::BT709_FULL;
/// ```
#[derive(Debug, Clone, Copy)]
pub struct ColorSpaceConfig {
    /// Color matrix for RGB↔YUV conversion
    pub matrix: ColorMatrix,
    /// Value range (full vs limited)
    pub range: ColorRange,
    /// H.264 VUI colour_primaries
    pub primaries: ColourPrimaries,
    /// H.264 VUI transfer_characteristics
    pub transfer: TransferCharacteristics,
    /// H.264 VUI matrix_coefficients
    pub matrix_coeff: MatrixCoefficients,
}

impl ColorSpaceConfig {
    /// OpenH264-compatible preset (BT.601 limited range)
    ///
    /// **Use this for AVC444 to match AVC420 output.**
    ///
    /// This matches OpenH264's internal RGB→YUV conversion exactly,
    /// ensuring visual consistency between AVC420 and AVC444 modes.
    pub const OPENH264_COMPATIBLE: Self = Self {
        matrix: ColorMatrix::OpenH264,
        range: ColorRange::Limited,
        primaries: ColourPrimaries::BT601NTSC,
        transfer: TransferCharacteristics::BT601,
        matrix_coeff: MatrixCoefficients::BT601,
    };

    /// BT.709 with limited range (HD broadcast standard)
    ///
    /// Use for HD content (≥720p) with broadcast compatibility.
    pub const BT709_LIMITED: Self = Self {
        matrix: ColorMatrix::BT709,
        range: ColorRange::Limited,
        primaries: ColourPrimaries::BT709,
        transfer: TransferCharacteristics::BT709,
        matrix_coeff: MatrixCoefficients::BT709,
    };

    /// BT.709 with full range (HD computer graphics)
    ///
    /// Use for HD content when maximum dynamic range is needed
    /// and the decoder supports full range.
    pub const BT709_FULL: Self = Self {
        matrix: ColorMatrix::BT709,
        range: ColorRange::Full,
        primaries: ColourPrimaries::BT709,
        transfer: TransferCharacteristics::BT709,
        matrix_coeff: MatrixCoefficients::BT709,
    };

    /// BT.601 with limited range (SD broadcast standard)
    ///
    /// Use for SD content (≤720p) with broadcast compatibility.
    pub const BT601_LIMITED: Self = Self {
        matrix: ColorMatrix::BT601,
        range: ColorRange::Limited,
        primaries: ColourPrimaries::BT601NTSC,
        transfer: TransferCharacteristics::BT601,
        matrix_coeff: MatrixCoefficients::BT601,
    };

    /// sRGB preset (computer graphics)
    ///
    /// Use for desktop capture where content is sRGB.
    /// Uses BT.709 matrix with sRGB transfer function.
    pub const SRGB_FULL: Self = Self {
        matrix: ColorMatrix::BT709,
        range: ColorRange::Full,
        primaries: ColourPrimaries::BT709,
        transfer: TransferCharacteristics::SRGB,
        matrix_coeff: MatrixCoefficients::BT709,
    };

    /// Auto-select configuration based on resolution and compatibility mode
    ///
    /// # Arguments
    ///
    /// * `width` - Frame width in pixels
    /// * `height` - Frame height in pixels
    /// * `openh264_compat` - If true, use OpenH264-compatible settings
    ///
    /// # Returns
    ///
    /// Appropriate ColorSpaceConfig for the given parameters
    pub fn auto_select(width: u32, height: u32, openh264_compat: bool) -> Self {
        if openh264_compat {
            // Always use OpenH264-compatible for consistency with AVC420
            Self::OPENH264_COMPATIBLE
        } else if width >= 1280 && height >= 720 {
            // HD content: BT.709
            Self::BT709_LIMITED
        } else {
            // SD content: BT.601
            Self::BT601_LIMITED
        }
    }

    /// Create from string configuration values
    ///
    /// # Arguments
    ///
    /// * `color_space` - "auto", "openh264", "bt709", "bt601", "srgb"
    /// * `color_range` - "auto", "limited", "full"
    /// * `width` - Frame width for auto selection
    /// * `height` - Frame height for auto selection
    pub fn from_config(color_space: &str, color_range: &str, width: u32, height: u32) -> Self {
        let base = match color_space.to_lowercase().as_str() {
            "openh264" => Self::OPENH264_COMPATIBLE,
            "bt709" => Self::BT709_LIMITED,
            "bt601" => Self::BT601_LIMITED,
            "srgb" => Self::SRGB_FULL,
            _ => Self::auto_select(width, height, true), // Default: OpenH264 compat
        };

        // Override range if specified
        let range = match color_range.to_lowercase().as_str() {
            "limited" => ColorRange::Limited,
            "full" => ColorRange::Full,
            _ => base.range, // Keep base range for "auto"
        };

        Self { range, ..base }
    }

    /// Get VUI video_full_range_flag value
    #[inline]
    pub const fn vui_full_range_flag(&self) -> u8 {
        self.range.vui_flag()
    }

    /// Get VUI colour_primaries value
    #[inline]
    pub const fn vui_colour_primaries(&self) -> u8 {
        self.primaries as u8
    }

    /// Get VUI transfer_characteristics value
    #[inline]
    pub const fn vui_transfer_characteristics(&self) -> u8 {
        self.transfer as u8
    }

    /// Get VUI matrix_coefficients value
    #[inline]
    pub const fn vui_matrix_coefficients(&self) -> u8 {
        self.matrix_coeff as u8
    }

    /// Check if this config uses limited range
    #[inline]
    pub const fn is_limited_range(&self) -> bool {
        matches!(self.range, ColorRange::Limited)
    }

    /// Get a human-readable description
    pub fn description(&self) -> String {
        let matrix_name = match self.matrix {
            ColorMatrix::BT601 => "BT.601",
            ColorMatrix::BT709 => "BT.709",
            ColorMatrix::OpenH264 => "OpenH264",
        };
        let range_name = match self.range {
            ColorRange::Limited => "limited",
            ColorRange::Full => "full",
        };
        format!("{} {}", matrix_name, range_name)
    }
}

impl Default for ColorSpaceConfig {
    /// Default to OpenH264-compatible for maximum consistency
    fn default() -> Self {
        Self::OPENH264_COMPATIBLE
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_color_range_vui() {
        assert_eq!(ColorRange::Limited.vui_flag(), 0);
        assert_eq!(ColorRange::Full.vui_flag(), 1);
    }

    #[test]
    fn test_openh264_preset() {
        let config = ColorSpaceConfig::OPENH264_COMPATIBLE;
        assert_eq!(config.matrix, ColorMatrix::OpenH264);
        assert_eq!(config.range, ColorRange::Limited);
        assert_eq!(config.vui_colour_primaries(), 6); // BT.601 NTSC
        assert_eq!(config.vui_matrix_coefficients(), 6);
        assert_eq!(config.vui_full_range_flag(), 0);
    }

    #[test]
    fn test_bt709_preset() {
        let config = ColorSpaceConfig::BT709_LIMITED;
        assert_eq!(config.matrix, ColorMatrix::BT709);
        assert_eq!(config.range, ColorRange::Limited);
        assert_eq!(config.vui_colour_primaries(), 1); // BT.709
        assert_eq!(config.vui_matrix_coefficients(), 1);
    }

    #[test]
    fn test_auto_select_hd() {
        // HD resolution, not OpenH264 compat
        let config = ColorSpaceConfig::auto_select(1920, 1080, false);
        assert_eq!(config.matrix, ColorMatrix::BT709);

        // HD resolution, OpenH264 compat
        let config = ColorSpaceConfig::auto_select(1920, 1080, true);
        assert_eq!(config.matrix, ColorMatrix::OpenH264);
    }

    #[test]
    fn test_auto_select_sd() {
        // SD resolution, not OpenH264 compat
        let config = ColorSpaceConfig::auto_select(800, 600, false);
        assert_eq!(config.matrix, ColorMatrix::BT601);

        // SD resolution, OpenH264 compat
        let config = ColorSpaceConfig::auto_select(800, 600, true);
        assert_eq!(config.matrix, ColorMatrix::OpenH264);
    }

    #[test]
    fn test_from_config() {
        let config = ColorSpaceConfig::from_config("bt709", "full", 1920, 1080);
        assert_eq!(config.matrix, ColorMatrix::BT709);
        assert_eq!(config.range, ColorRange::Full);

        let config = ColorSpaceConfig::from_config("openh264", "limited", 1920, 1080);
        assert_eq!(config.matrix, ColorMatrix::OpenH264);
        assert_eq!(config.range, ColorRange::Limited);

        let config = ColorSpaceConfig::from_config("auto", "auto", 1920, 1080);
        assert_eq!(config.matrix, ColorMatrix::OpenH264); // Default to compat
    }

    #[test]
    fn test_description() {
        assert_eq!(
            ColorSpaceConfig::OPENH264_COMPATIBLE.description(),
            "OpenH264 limited"
        );
        assert_eq!(ColorSpaceConfig::BT709_FULL.description(), "BT.709 full");
    }

    #[test]
    fn test_default() {
        let config = ColorSpaceConfig::default();
        assert_eq!(config.matrix, ColorMatrix::OpenH264);
    }
}
