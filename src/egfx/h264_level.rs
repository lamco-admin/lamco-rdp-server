//! H.264 Level Management and Constraint Checking
//!
//! This module provides comprehensive H.264 level management to ensure
//! encoder configurations always meet ITU-T H.264 Annex A specifications.
//!
//! # H.264 Levels
//!
//! Levels define decoder capability constraints:
//! - Maximum macroblocks per second (processing throughput)
//! - Maximum frame size in macroblocks (memory)
//! - Maximum bitrate
//!
//! # Level Selection
//!
//! For a given resolution and framerate, select the minimum level that satisfies:
//! ```text
//! (width_in_mbs * height_in_mbs) * fps ≤ level.max_macroblocks_per_second
//! ```

use std::fmt;

/// H.264 level identifier (ITU-T H.264 Annex A)
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum H264Level {
    L1_0 = 10,
    L1_1 = 11,
    L1_2 = 12,
    L1_3 = 13,
    L2_0 = 20,
    L2_1 = 21,
    L2_2 = 22,
    L3_0 = 30,
    L3_1 = 31,
    L3_2 = 32,
    L4_0 = 40,
    L4_1 = 41,
    L4_2 = 42,
    L5_0 = 50,
    L5_1 = 51,
    L5_2 = 52,
}

impl H264Level {
    /// Maximum macroblocks per second for this level
    pub const fn max_macroblocks_per_second(&self) -> u32 {
        match self {
            H264Level::L1_0 => 1_485,
            H264Level::L1_1 => 3_000,
            H264Level::L1_2 => 6_000,
            H264Level::L1_3 => 11_880,
            H264Level::L2_0 => 11_880,
            H264Level::L2_1 => 19_800,
            H264Level::L2_2 => 20_250,
            H264Level::L3_0 => 40_500,
            H264Level::L3_1 => 108_000,
            H264Level::L3_2 => 108_000, // Note: 216,000 if frame ≤ 1620 MBs
            H264Level::L4_0 => 245_760,
            H264Level::L4_1 => 245_760,
            H264Level::L4_2 => 522_240,
            H264Level::L5_0 => 589_824,
            H264Level::L5_1 => 983_040,
            H264Level::L5_2 => 2_073_600,
        }
    }

    /// Maximum frame size in macroblocks for this level
    pub const fn max_frame_macroblocks(&self) -> u32 {
        match self {
            H264Level::L1_0 => 99,
            H264Level::L1_1
            | H264Level::L1_2
            | H264Level::L1_3
            | H264Level::L2_0
            | H264Level::L2_1 => 396,
            H264Level::L2_2 | H264Level::L3_0 => 1_620,
            H264Level::L3_1 => 3_600,
            H264Level::L3_2 => 5_120,
            H264Level::L4_0 | H264Level::L4_1 => 8_192,
            H264Level::L4_2 => 8_704,
            H264Level::L5_0 => 22_080,
            H264Level::L5_1 | H264Level::L5_2 => 36_864,
        }
    }

    /// Get effective max MB/s for Level 3.2 with special frame size handling
    pub const fn effective_max_mbs_per_sec(&self, frame_mbs: u32) -> u32 {
        if matches!(self, H264Level::L3_2) && frame_mbs <= 1_620 {
            216_000 // Higher limit for small frames
        } else {
            self.max_macroblocks_per_second()
        }
    }

    /// Maximum bitrate in bps for this level (Baseline/Main profile)
    pub const fn max_bitrate_bps(&self) -> u32 {
        match self {
            H264Level::L1_0 => 64_000,
            H264Level::L1_1 => 192_000,
            H264Level::L1_2 => 384_000,
            H264Level::L1_3 => 768_000,
            H264Level::L2_0 => 2_000_000,
            H264Level::L2_1 => 4_000_000,
            H264Level::L2_2 => 4_000_000,
            H264Level::L3_0 => 10_000_000,
            H264Level::L3_1 => 14_000_000,
            H264Level::L3_2 => 20_000_000,
            H264Level::L4_0 => 25_000_000,
            H264Level::L4_1 => 50_000_000,
            H264Level::L4_2 => 50_000_000,
            H264Level::L5_0 => 135_000_000,
            H264Level::L5_1 => 240_000_000,
            H264Level::L5_2 => 240_000_000,
        }
    }

    /// Convert to OpenH264 ELevelIdc constant
    pub const fn to_openh264_level_idc(&self) -> i32 {
        *self as i32
    }

    /// Convert to openh264 crate's Level enum
    #[cfg(feature = "h264")]
    pub fn to_openh264_level(&self) -> openh264::encoder::Level {
        use openh264::encoder::Level;
        match self {
            H264Level::L1_0 => Level::Level_1_0,
            H264Level::L1_1 => Level::Level_1_1,
            H264Level::L1_2 => Level::Level_1_2,
            H264Level::L1_3 => Level::Level_1_3,
            H264Level::L2_0 => Level::Level_2_0,
            H264Level::L2_1 => Level::Level_2_1,
            H264Level::L2_2 => Level::Level_2_2,
            H264Level::L3_0 => Level::Level_3_0,
            H264Level::L3_1 => Level::Level_3_1,
            H264Level::L3_2 => Level::Level_3_2,
            H264Level::L4_0 => Level::Level_4_0,
            H264Level::L4_1 => Level::Level_4_1,
            H264Level::L4_2 => Level::Level_4_2,
            H264Level::L5_0 => Level::Level_5_0,
            H264Level::L5_1 => Level::Level_5_1,
            H264Level::L5_2 => Level::Level_5_2,
        }
    }

    /// Select minimum level for given resolution and framerate
    pub fn for_config(width: u16, height: u16, fps: f32) -> Self {
        let mbs = ((width as u32 + 15) / 16) * ((height as u32 + 15) / 16);
        let required_mbs_per_sec = mbs as f32 * fps;

        // Try levels in ascending order
        for level in Self::iter_ascending() {
            // Check frame size constraint
            if mbs > level.max_frame_macroblocks() {
                continue;
            }

            // Check MB/s constraint (with Level 3.2 special handling)
            let max_mbs_per_sec = level.effective_max_mbs_per_sec(mbs);
            if required_mbs_per_sec <= max_mbs_per_sec as f32 {
                return level;
            }
        }

        // If we get here, even Level 5.2 isn't enough
        // Return highest level anyway
        H264Level::L5_2
    }

    /// Iterate levels in ascending order
    pub fn iter_ascending() -> impl Iterator<Item = Self> {
        [
            H264Level::L1_0,
            H264Level::L1_1,
            H264Level::L1_2,
            H264Level::L1_3,
            H264Level::L2_0,
            H264Level::L2_1,
            H264Level::L2_2,
            H264Level::L3_0,
            H264Level::L3_1,
            H264Level::L3_2,
            H264Level::L4_0,
            H264Level::L4_1,
            H264Level::L4_2,
            H264Level::L5_0,
            H264Level::L5_1,
            H264Level::L5_2,
        ]
        .iter()
        .copied()
    }

    /// Human-readable level string (e.g., "3.1", "4.0")
    pub fn as_str(&self) -> &'static str {
        match self {
            H264Level::L1_0 => "1.0",
            H264Level::L1_1 => "1.1",
            H264Level::L1_2 => "1.2",
            H264Level::L1_3 => "1.3",
            H264Level::L2_0 => "2.0",
            H264Level::L2_1 => "2.1",
            H264Level::L2_2 => "2.2",
            H264Level::L3_0 => "3.0",
            H264Level::L3_1 => "3.1",
            H264Level::L3_2 => "3.2",
            H264Level::L4_0 => "4.0",
            H264Level::L4_1 => "4.1",
            H264Level::L4_2 => "4.2",
            H264Level::L5_0 => "5.0",
            H264Level::L5_1 => "5.1",
            H264Level::L5_2 => "5.2",
        }
    }
}

impl fmt::Display for H264Level {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Level {}", self.as_str())
    }
}

/// Level constraint calculator and validator
pub struct LevelConstraints {
    width: u16,
    height: u16,
    macroblocks: u32,
}

impl LevelConstraints {
    /// Create constraints for given resolution
    pub fn new(width: u16, height: u16) -> Self {
        let mbs = ((width as u32 + 15) / 16) * ((height as u32 + 15) / 16);
        Self {
            width,
            height,
            macroblocks: mbs,
        }
    }

    /// Get macroblocks count
    pub const fn macroblocks(&self) -> u32 {
        self.macroblocks
    }

    /// Calculate maximum FPS for a given level
    pub fn max_fps_for_level(&self, level: H264Level) -> f32 {
        let max_mbs_per_sec = level.effective_max_mbs_per_sec(self.macroblocks);
        (max_mbs_per_sec as f32) / (self.macroblocks as f32)
    }

    /// Recommend minimum level for target FPS
    pub fn recommend_level(&self, target_fps: f32) -> H264Level {
        H264Level::for_config(self.width, self.height, target_fps)
    }

    /// Validate that configuration meets level constraints
    pub fn validate(&self, fps: f32, level: H264Level) -> Result<(), ConstraintViolation> {
        // Check frame size constraint
        if self.macroblocks > level.max_frame_macroblocks() {
            return Err(ConstraintViolation::FrameSizeExceeded {
                macroblocks: self.macroblocks,
                max_macroblocks: level.max_frame_macroblocks(),
                level,
            });
        }

        // Check MB/s constraint
        let required_mbs_per_sec = self.macroblocks as f32 * fps;
        let max_mbs_per_sec = level.effective_max_mbs_per_sec(self.macroblocks);

        if required_mbs_per_sec > max_mbs_per_sec as f32 {
            return Err(ConstraintViolation::MacroblocksPerSecondExceeded {
                required: required_mbs_per_sec as u32,
                max: max_mbs_per_sec,
                level,
                resolution: (self.width, self.height),
                fps,
            });
        }

        Ok(())
    }

    /// Adjust FPS to fit within level constraints
    pub fn adjust_fps_for_level(&self, target_fps: f32, level: H264Level) -> f32 {
        let max_fps = self.max_fps_for_level(level);
        if target_fps <= max_fps {
            target_fps
        } else {
            tracing::warn!(
                "Target {:.1}fps exceeds {} constraint, reducing to {:.1}fps",
                target_fps,
                level,
                max_fps
            );
            max_fps
        }
    }
}

/// Level constraint violation error
#[derive(Debug)]
pub enum ConstraintViolation {
    FrameSizeExceeded {
        macroblocks: u32,
        max_macroblocks: u32,
        level: H264Level,
    },
    MacroblocksPerSecondExceeded {
        required: u32,
        max: u32,
        level: H264Level,
        resolution: (u16, u16),
        fps: f32,
    },
}

impl fmt::Display for ConstraintViolation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConstraintViolation::FrameSizeExceeded {
                macroblocks,
                max_macroblocks,
                level,
            } => write!(
                f,
                "Frame size {} MBs exceeds {} max {} MBs",
                macroblocks, level, max_macroblocks
            ),
            ConstraintViolation::MacroblocksPerSecondExceeded {
                required,
                max,
                level,
                resolution,
                fps,
            } => write!(
                f,
                "{}x{} @ {:.1}fps requires {} MB/s but {} only supports {} MB/s",
                resolution.0, resolution.1, fps, required, level, max
            ),
        }
    }
}

impl std::error::Error for ConstraintViolation {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_720p_30fps() {
        let constraints = LevelConstraints::new(1280, 720);
        assert_eq!(constraints.macroblocks(), 3600);

        // Should fit in Level 3.1
        assert!(constraints.validate(30.0, H264Level::L3_1).is_ok());

        // Recommend Level 3.1
        assert_eq!(constraints.recommend_level(30.0), H264Level::L3_1);

        // Max FPS for Level 3.1
        let max_fps = constraints.max_fps_for_level(H264Level::L3_1);
        assert_eq!(max_fps, 30.0); // 108,000 / 3,600 = 30.0
    }

    #[test]
    fn test_1280x800_30fps() {
        let constraints = LevelConstraints::new(1280, 800);
        assert_eq!(constraints.macroblocks(), 4000);

        // Should NOT fit in Level 3.2 at 30fps
        assert!(constraints.validate(30.0, H264Level::L3_2).is_err());

        // Should fit in Level 4.0
        assert!(constraints.validate(30.0, H264Level::L4_0).is_ok());

        // Recommend Level 4.0 for 30fps
        assert_eq!(constraints.recommend_level(30.0), H264Level::L4_0);

        // Max FPS for Level 3.2 (special handling)
        let max_fps_3_2 = constraints.max_fps_for_level(H264Level::L3_2);
        assert_eq!(max_fps_3_2, 27.0); // 108,000 / 4,000 = 27.0

        // Max FPS for Level 4.0
        let max_fps_4_0 = constraints.max_fps_for_level(H264Level::L4_0);
        assert_eq!(max_fps_4_0, 61.44); // 245,760 / 4,000 = 61.44
    }

    #[test]
    fn test_1080p_30fps() {
        let constraints = LevelConstraints::new(1920, 1080);
        // 1920x1080 → ceil(1920/16) × ceil(1080/16) = 120 × 68 = 8160
        assert_eq!(constraints.macroblocks(), 8160);

        // Requires Level 4.0
        assert!(constraints.validate(30.0, H264Level::L3_2).is_err());
        assert!(constraints.validate(30.0, H264Level::L4_0).is_ok());

        assert_eq!(constraints.recommend_level(30.0), H264Level::L4_0);
    }

    #[test]
    fn test_4k_30fps() {
        let constraints = LevelConstraints::new(3840, 2160);
        assert_eq!(constraints.macroblocks(), 32400);

        // Requires Level 5.1
        assert!(constraints.validate(30.0, H264Level::L5_0).is_err());
        assert!(constraints.validate(30.0, H264Level::L5_1).is_ok());

        assert_eq!(constraints.recommend_level(30.0), H264Level::L5_1);
    }

    #[test]
    fn test_fps_adjustment() {
        let constraints = LevelConstraints::new(1280, 800);

        // 30fps doesn't fit in Level 3.2, should reduce to 27fps
        let adjusted = constraints.adjust_fps_for_level(30.0, H264Level::L3_2);
        assert_eq!(adjusted, 27.0);

        // 30fps fits in Level 4.0, should not adjust
        let adjusted = constraints.adjust_fps_for_level(30.0, H264Level::L4_0);
        assert_eq!(adjusted, 30.0);
    }
}
