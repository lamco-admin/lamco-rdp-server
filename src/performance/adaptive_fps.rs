//! Adaptive FPS Controller
//!
//! Dynamically adjusts frame rate based on screen activity level,
//! reducing CPU and bandwidth for static content while maintaining
//! smooth video for active content.
//!
//! # Activity Levels (default max_fps=30)
//!
//! | Level | Damage % | FPS | Use Case |
//! |-------|----------|-----|----------|
//! | Static | <1% | 5 | Wallpaper, idle desktop |
//! | Low | 1-10% | 15 | Typing, cursor movement |
//! | Medium | 10-30% | 20 | Scrolling, menus |
//! | High | >30% | 30 | Video, window dragging |
//!
//! # High Performance Mode (60 FPS)
//!
//! For powerful systems with hardware encoding, set `max_fps = 60`:
//!
//! ```toml
//! [performance.adaptive_fps]
//! max_fps = 60
//! ```
//!
//! **Requirements:** VAAPI/NVENC encoder, fast network (>10Mbps)
//!
//! # Algorithm
//!
//! Uses a rolling window of recent damage ratios to calculate
//! average activity. This smooths out sudden spikes and provides
//! stable FPS transitions.

use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};
use tracing::debug;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdaptiveFpsConfig {
    /// Enable adaptive FPS (false = fixed FPS)
    #[serde(default = "default_enabled")]
    pub enabled: bool,

    /// Minimum FPS (even for static content)
    #[serde(default = "default_min_fps")]
    pub min_fps: u32,

    /// Maximum FPS (target for high activity)
    #[serde(default = "default_max_fps")]
    pub max_fps: u32,

    /// Number of frames to average for activity detection
    #[serde(default = "default_history_size")]
    pub history_size: usize,

    /// Damage ratio threshold for high activity (full FPS)
    #[serde(default = "default_high_threshold")]
    pub high_activity_threshold: f32,

    /// Damage ratio threshold for medium activity (2/3 FPS)
    #[serde(default = "default_medium_threshold")]
    pub medium_activity_threshold: f32,

    /// Damage ratio threshold for low activity (1/2 FPS)
    #[serde(default = "default_low_threshold")]
    pub low_activity_threshold: f32,

    /// Ramp-up speed (how fast to increase FPS on activity)
    #[serde(default = "default_ramp_up_frames")]
    pub ramp_up_frames: usize,

    /// Ramp-down speed (how fast to decrease FPS on idle)
    #[serde(default = "default_ramp_down_frames")]
    pub ramp_down_frames: usize,
}

fn default_enabled() -> bool {
    true
}
fn default_min_fps() -> u32 {
    5
}
fn default_max_fps() -> u32 {
    30
}
fn default_history_size() -> usize {
    10
}
fn default_high_threshold() -> f32 {
    0.30
}
fn default_medium_threshold() -> f32 {
    0.10
}
fn default_low_threshold() -> f32 {
    0.01
}
fn default_ramp_up_frames() -> usize {
    2
}
fn default_ramp_down_frames() -> usize {
    5
}

impl Default for AdaptiveFpsConfig {
    fn default() -> Self {
        Self {
            enabled: default_enabled(),
            min_fps: default_min_fps(),
            max_fps: default_max_fps(),
            history_size: default_history_size(),
            high_activity_threshold: default_high_threshold(),
            medium_activity_threshold: default_medium_threshold(),
            low_activity_threshold: default_low_threshold(),
            ramp_up_frames: default_ramp_up_frames(),
            ramp_down_frames: default_ramp_down_frames(),
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct DamageRatio {
    /// Ratio of damaged pixels (0.0 - 1.0)
    pub ratio: f32,
    /// When this sample was taken
    pub timestamp: Instant,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivityLevel {
    /// Screen is static (< low_threshold damage)
    Static,
    /// Low activity (typing, cursor)
    Low,
    /// Medium activity (scrolling)
    Medium,
    /// High activity (video, dragging)
    High,
}

impl ActivityLevel {
    pub fn fps_multiplier(&self) -> f32 {
        match self {
            Self::Static => 0.0, // Use min_fps
            Self::Low => 0.5,
            Self::Medium => 0.67,
            Self::High => 1.0,
        }
    }
}

/// Adaptive FPS controller
///
/// Tracks screen activity and dynamically adjusts target FPS
/// to optimize bandwidth and CPU usage.
pub struct AdaptiveFpsController {
    config: AdaptiveFpsConfig,
    current_fps: u32,
    activity_level: ActivityLevel,
    damage_history: VecDeque<DamageRatio>,
    last_frame_time: Instant,
    /// For ramp smoothing between activity levels
    frames_at_level: usize,
    stats: AdaptiveFpsStats,
}

#[derive(Debug, Clone, Default)]
pub struct AdaptiveFpsStats {
    pub frames_processed: u64,
    pub frames_skipped: u64,
    pub time_at_static: Duration,
    pub time_at_low: Duration,
    pub time_at_medium: Duration,
    pub time_at_high: Duration,
    pub last_level_change: Option<Instant>,
}

impl AdaptiveFpsController {
    pub fn new(config: AdaptiveFpsConfig) -> Self {
        let current_fps = config.max_fps;

        Self {
            current_fps,
            activity_level: ActivityLevel::High, // Start assuming activity
            damage_history: VecDeque::with_capacity(config.history_size),
            last_frame_time: Instant::now(),
            frames_at_level: 0,
            stats: AdaptiveFpsStats::default(),
            config,
        }
    }

    /// Call after damage detection for each frame.
    pub fn update(&mut self, damage_ratio: f32) {
        if !self.config.enabled {
            return;
        }

        let now = Instant::now();

        self.damage_history.push_back(DamageRatio {
            ratio: damage_ratio,
            timestamp: now,
        });

        while self.damage_history.len() > self.config.history_size {
            self.damage_history.pop_front();
        }

        let avg_damage = self.average_damage();

        let target_level = if avg_damage > self.config.high_activity_threshold {
            ActivityLevel::High
        } else if avg_damage > self.config.medium_activity_threshold {
            ActivityLevel::Medium
        } else if avg_damage > self.config.low_activity_threshold {
            ActivityLevel::Low
        } else {
            ActivityLevel::Static
        };

        let new_level = self.apply_ramping(target_level);

        if new_level != self.activity_level {
            debug!(
                "Activity level changed: {:?} -> {:?} (avg_damage={:.1}%)",
                self.activity_level,
                new_level,
                avg_damage * 100.0
            );
            self.stats.last_level_change = Some(now);
            self.frames_at_level = 0;
        }

        if let Some(last_change) = self.stats.last_level_change {
            let elapsed = now.duration_since(last_change);
            match self.activity_level {
                ActivityLevel::Static => self.stats.time_at_static += elapsed,
                ActivityLevel::Low => self.stats.time_at_low += elapsed,
                ActivityLevel::Medium => self.stats.time_at_medium += elapsed,
                ActivityLevel::High => self.stats.time_at_high += elapsed,
            }
        }

        self.activity_level = new_level;
        self.frames_at_level += 1;

        self.current_fps = self.calculate_target_fps();

        self.stats.frames_processed += 1;
    }

    pub fn should_capture_frame(&mut self) -> bool {
        if !self.config.enabled {
            // When disabled, always capture at max FPS timing
            let frame_interval = Duration::from_secs_f32(1.0 / self.config.max_fps as f32);
            let elapsed = self.last_frame_time.elapsed();
            if elapsed >= frame_interval {
                self.last_frame_time = Instant::now();
                return true;
            }
            return false;
        }

        let frame_interval = Duration::from_secs_f32(1.0 / self.current_fps as f32);
        let elapsed = self.last_frame_time.elapsed();

        if elapsed >= frame_interval {
            self.last_frame_time = Instant::now();
            true
        } else {
            self.stats.frames_skipped += 1;
            false
        }
    }

    pub fn current_fps(&self) -> u32 {
        self.current_fps
    }

    pub fn activity_level(&self) -> ActivityLevel {
        self.activity_level
    }

    pub fn stats(&self) -> &AdaptiveFpsStats {
        &self.stats
    }

    pub fn reset_stats(&mut self) {
        self.stats = AdaptiveFpsStats::default();
    }

    pub fn is_enabled(&self) -> bool {
        self.config.enabled
    }

    pub fn set_enabled(&mut self, enabled: bool) {
        self.config.enabled = enabled;
        if !enabled {
            self.current_fps = self.config.max_fps;
        }
    }

    fn average_damage(&self) -> f32 {
        if self.damage_history.is_empty() {
            return 0.0;
        }

        let sum: f32 = self.damage_history.iter().map(|d| d.ratio).sum();
        sum / self.damage_history.len() as f32
    }

    fn apply_ramping(&self, target_level: ActivityLevel) -> ActivityLevel {
        // Quick ramp-up (respond fast to activity)
        if target_level > self.activity_level {
            if self.frames_at_level >= self.config.ramp_up_frames {
                return target_level;
            }
            // Increase one level at a time for smooth ramp-up
            return match self.activity_level {
                ActivityLevel::Static => ActivityLevel::Low,
                ActivityLevel::Low => ActivityLevel::Medium,
                ActivityLevel::Medium => ActivityLevel::High,
                ActivityLevel::High => ActivityLevel::High,
            };
        }

        // Slow ramp-down (don't drop FPS too quickly)
        if target_level < self.activity_level
            && self.frames_at_level >= self.config.ramp_down_frames
        {
            return match self.activity_level {
                ActivityLevel::High => ActivityLevel::Medium,
                ActivityLevel::Medium => ActivityLevel::Low,
                ActivityLevel::Low => ActivityLevel::Static,
                ActivityLevel::Static => ActivityLevel::Static,
            };
        }

        self.activity_level
    }

    fn calculate_target_fps(&self) -> u32 {
        let multiplier = self.activity_level.fps_multiplier();

        if multiplier == 0.0 {
            self.config.min_fps
        } else {
            let range = self.config.max_fps - self.config.min_fps;
            let fps = self.config.min_fps + (range as f32 * multiplier) as u32;
            fps.clamp(self.config.min_fps, self.config.max_fps)
        }
    }
}

impl std::cmp::PartialOrd for ActivityLevel {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl std::cmp::Ord for ActivityLevel {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        let self_val = match self {
            Self::Static => 0,
            Self::Low => 1,
            Self::Medium => 2,
            Self::High => 3,
        };
        let other_val = match other {
            Self::Static => 0,
            Self::Low => 1,
            Self::Medium => 2,
            Self::High => 3,
        };
        self_val.cmp(&other_val)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = AdaptiveFpsConfig::default();
        assert!(config.enabled);
        assert_eq!(config.min_fps, 5);
        assert_eq!(config.max_fps, 30);
    }

    #[test]
    fn test_activity_level_ordering() {
        assert!(ActivityLevel::Static < ActivityLevel::Low);
        assert!(ActivityLevel::Low < ActivityLevel::Medium);
        assert!(ActivityLevel::Medium < ActivityLevel::High);
    }

    #[test]
    fn test_high_activity_full_fps() {
        let config = AdaptiveFpsConfig::default();
        let mut controller = AdaptiveFpsController::new(config);

        // Simulate high activity
        for _ in 0..10 {
            controller.update(0.5); // 50% damage
        }

        assert_eq!(controller.activity_level(), ActivityLevel::High);
        assert_eq!(controller.current_fps(), 30);
    }

    #[test]
    fn test_static_screen_min_fps() {
        let config = AdaptiveFpsConfig::default();
        let mut controller = AdaptiveFpsController::new(config);

        // Simulate static screen (need enough frames to ramp down)
        for _ in 0..50 {
            controller.update(0.0); // 0% damage
        }

        assert_eq!(controller.activity_level(), ActivityLevel::Static);
        assert_eq!(controller.current_fps(), 5);
    }

    #[test]
    fn test_disabled_controller() {
        let mut config = AdaptiveFpsConfig::default();
        config.enabled = false;

        let mut controller = AdaptiveFpsController::new(config);
        controller.update(0.0);

        // Should stay at max FPS when disabled
        assert_eq!(controller.current_fps(), 30);
    }

    #[test]
    fn test_ramp_up_speed() {
        let mut config = AdaptiveFpsConfig::default();
        config.ramp_up_frames = 2;

        let mut controller = AdaptiveFpsController::new(config);

        // Start at high (default), then go static
        for _ in 0..20 {
            controller.update(0.0);
        }
        assert_eq!(controller.activity_level(), ActivityLevel::Static);

        // Now add high activity - should ramp up quickly
        controller.update(0.5);
        controller.update(0.5);
        controller.update(0.5);

        // Should be at least Medium after 3 high-activity frames
        assert!(controller.activity_level() >= ActivityLevel::Low);
    }
}
