//! Latency Governor
//!
//! Configurable latency vs quality tradeoffs for different use cases.
//! Provides three professional modes optimized for different workflows.
//!
//! # Modes
//!
//! | Mode | Target Latency | Use Case | Tradeoff |
//! |------|----------------|----------|----------|
//! | Interactive | <50ms | Gaming, CAD | Higher bandwidth |
//! | Balanced | <100ms | General desktop | Default |
//! | Quality | <300ms | Photo/video editing | Better compression |
//!
//! # Algorithm
//!
//! The governor decides when to encode frames based on:
//! 1. Accumulated damage since last encode
//! 2. Time since first damage (prevents starvation)
//! 3. Mode-specific thresholds

use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};
use tracing::debug;

/// Latency mode selection
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum LatencyMode {
    /// Low latency for interactive applications (<50ms)
    Interactive,

    /// Balanced latency/quality tradeoff (<100ms)
    #[default]
    Balanced,

    /// High quality with acceptable latency (<300ms)
    Quality,
}

impl LatencyMode {
    /// Get target latency for this mode
    pub fn target_latency_ms(&self) -> u32 {
        match self {
            Self::Interactive => 50,
            Self::Balanced => 100,
            Self::Quality => 300,
        }
    }

    /// Get human-readable description
    pub fn description(&self) -> &'static str {
        match self {
            Self::Interactive => "Low latency (<50ms) - Gaming, CAD, interactive design",
            Self::Balanced => "Balanced (<100ms) - General desktop, office work",
            Self::Quality => "High quality (<300ms) - Photo/video editing, color work",
        }
    }
}

impl std::fmt::Display for LatencyMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Interactive => write!(f, "Interactive"),
            Self::Balanced => write!(f, "Balanced"),
            Self::Quality => write!(f, "Quality"),
        }
    }
}

impl std::str::FromStr for LatencyMode {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "interactive" | "low" | "fast" => Ok(Self::Interactive),
            "balanced" | "default" | "normal" => Ok(Self::Balanced),
            "quality" | "high" | "slow" => Ok(Self::Quality),
            _ => Err(format!("Unknown latency mode: {}", s)),
        }
    }
}

/// Mode-specific settings
#[derive(Debug, Clone)]
struct ModeSettings {
    /// Maximum delay before forcing frame encode (ms)
    max_frame_delay_ms: f32,
    /// Damage threshold to trigger immediate encode
    damage_threshold: f32,
    /// Whether to use adaptive FPS with this mode
    use_adaptive_fps: bool,
    /// Encode timeout (how long to wait for encoder)
    encode_timeout_ms: u32,
}

impl ModeSettings {
    fn for_mode(mode: LatencyMode) -> Self {
        match mode {
            LatencyMode::Interactive => Self {
                max_frame_delay_ms: 16.0, // ~60fps timing
                damage_threshold: 0.0,    // Encode ANY change immediately
                use_adaptive_fps: false,  // Always max FPS
                encode_timeout_ms: 10,
            },
            LatencyMode::Balanced => Self {
                max_frame_delay_ms: 33.0, // ~30fps timing
                damage_threshold: 0.02,   // 2% damage threshold
                use_adaptive_fps: true,
                encode_timeout_ms: 20,
            },
            LatencyMode::Quality => Self {
                max_frame_delay_ms: 100.0, // Can batch more
                damage_threshold: 0.05,    // 5% damage threshold
                use_adaptive_fps: true,
                encode_timeout_ms: 50,
            },
        }
    }
}

/// Encoding decision from the governor
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EncodingDecision {
    /// Encode frame immediately (damage threshold met)
    EncodeNow,

    /// Encode as keepalive (no damage but timeout reached)
    EncodeKeepalive,

    /// Encode accumulated batch (quality mode)
    EncodeBatch,

    /// Encode due to timeout (max delay reached)
    EncodeTimeout,

    /// Skip this frame (accumulate more damage)
    Skip,

    /// Wait for more damage (quality mode batching)
    WaitForMore,
}

impl EncodingDecision {
    /// Check if this decision means we should encode
    pub fn should_encode(&self) -> bool {
        matches!(
            self,
            Self::EncodeNow | Self::EncodeKeepalive | Self::EncodeBatch | Self::EncodeTimeout
        )
    }
}

/// Frame accumulator for batching
#[derive(Debug, Default)]
struct FrameAccumulator {
    /// Total damage accumulated since last encode
    pending_damage: f32,
    /// Time of first damage in current batch
    first_damage_time: Option<Instant>,
    /// Number of frames accumulated
    frame_count: u32,
}

impl FrameAccumulator {
    fn reset(&mut self) {
        self.pending_damage = 0.0;
        self.first_damage_time = None;
        self.frame_count = 0;
    }

    fn add_damage(&mut self, damage: f32) {
        if self.first_damage_time.is_none() && damage > 0.0 {
            self.first_damage_time = Some(Instant::now());
        }
        self.pending_damage += damage;
        self.frame_count += 1;
    }

    fn elapsed_ms(&self) -> f32 {
        self.first_damage_time
            .map(|t| t.elapsed().as_secs_f32() * 1000.0)
            .unwrap_or(0.0)
    }
}

/// Latency metrics for monitoring
#[derive(Debug, Clone, Default)]
pub struct LatencyMetrics {
    /// Rolling average: capture to encode start
    pub capture_to_encode_avg_ms: f32,
    /// Rolling average: encode duration
    pub encode_duration_avg_ms: f32,
    /// Rolling average: total latency
    pub total_latency_avg_ms: f32,
    /// Frames encoded
    pub frames_encoded: u64,
    /// Frames skipped
    pub frames_skipped: u64,
    /// Batches (quality mode)
    pub batches_encoded: u64,
}

/// Latency Governor
///
/// Decides when to encode frames based on damage accumulation
/// and mode-specific latency targets.
pub struct LatencyGovernor {
    /// Current mode
    mode: LatencyMode,
    /// Mode-specific settings
    settings: ModeSettings,
    /// Frame accumulator
    accumulator: FrameAccumulator,
    /// Last encode time
    last_encode_time: Instant,
    /// Metrics
    metrics: LatencyMetrics,
}

impl LatencyGovernor {
    /// Create a new latency governor with the specified mode
    pub fn new(mode: LatencyMode) -> Self {
        Self {
            settings: ModeSettings::for_mode(mode),
            mode,
            accumulator: FrameAccumulator::default(),
            last_encode_time: Instant::now(),
            metrics: LatencyMetrics::default(),
        }
    }

    /// Determine if we should encode this frame
    ///
    /// Call this after damage detection to get an encoding decision.
    pub fn should_encode_frame(&mut self, damage_ratio: f32) -> EncodingDecision {
        self.accumulator.add_damage(damage_ratio);

        let elapsed = self.accumulator.elapsed_ms();
        let pending = self.accumulator.pending_damage;

        let decision = match self.mode {
            LatencyMode::Interactive => {
                // Interactive: encode immediately on ANY damage
                if damage_ratio > self.settings.damage_threshold {
                    EncodingDecision::EncodeNow
                } else if elapsed > self.settings.max_frame_delay_ms {
                    EncodingDecision::EncodeKeepalive
                } else {
                    EncodingDecision::Skip
                }
            }

            LatencyMode::Balanced => {
                // Balanced: encode when threshold met or timeout
                if pending >= self.settings.damage_threshold {
                    EncodingDecision::EncodeNow
                } else if elapsed > self.settings.max_frame_delay_ms {
                    EncodingDecision::EncodeTimeout
                } else {
                    EncodingDecision::Skip
                }
            }

            LatencyMode::Quality => {
                // Quality: batch changes for better compression
                if pending >= self.settings.damage_threshold {
                    EncodingDecision::EncodeBatch
                } else if elapsed > self.settings.max_frame_delay_ms {
                    EncodingDecision::EncodeTimeout
                } else if pending > 0.0 {
                    EncodingDecision::WaitForMore
                } else {
                    EncodingDecision::Skip
                }
            }
        };

        // Update metrics and reset accumulator if encoding
        if decision.should_encode() {
            self.record_encode();
        } else {
            self.metrics.frames_skipped += 1;
        }

        debug!(
            "LatencyGovernor ({:?}): damage={:.1}%, pending={:.1}%, elapsed={:.1}ms -> {:?}",
            self.mode,
            damage_ratio * 100.0,
            pending * 100.0,
            elapsed,
            decision
        );

        decision
    }

    /// Record that an encode was performed
    fn record_encode(&mut self) {
        self.metrics.frames_encoded += 1;
        if self.accumulator.frame_count > 1 {
            self.metrics.batches_encoded += 1;
        }
        self.last_encode_time = Instant::now();
        self.accumulator.reset();
    }

    /// Record encode timing for metrics
    pub fn record_encode_timing(&mut self, capture_to_encode_ms: f32, encode_duration_ms: f32) {
        // Simple exponential moving average
        const ALPHA: f32 = 0.1;

        self.metrics.capture_to_encode_avg_ms =
            self.metrics.capture_to_encode_avg_ms * (1.0 - ALPHA) + capture_to_encode_ms * ALPHA;

        self.metrics.encode_duration_avg_ms =
            self.metrics.encode_duration_avg_ms * (1.0 - ALPHA) + encode_duration_ms * ALPHA;

        self.metrics.total_latency_avg_ms =
            self.metrics.capture_to_encode_avg_ms + self.metrics.encode_duration_avg_ms;
    }

    /// Get current mode
    pub fn mode(&self) -> LatencyMode {
        self.mode
    }

    /// Set latency mode
    pub fn set_mode(&mut self, mode: LatencyMode) {
        if mode != self.mode {
            debug!("Latency mode changed: {:?} -> {:?}", self.mode, mode);
            self.mode = mode;
            self.settings = ModeSettings::for_mode(mode);
            self.accumulator.reset();
        }
    }

    /// Check if adaptive FPS should be used with current mode
    pub fn should_use_adaptive_fps(&self) -> bool {
        self.settings.use_adaptive_fps
    }

    /// Get encode timeout for current mode
    pub fn encode_timeout(&self) -> Duration {
        Duration::from_millis(self.settings.encode_timeout_ms as u64)
    }

    /// Get metrics
    pub fn metrics(&self) -> &LatencyMetrics {
        &self.metrics
    }

    /// Reset metrics
    pub fn reset_metrics(&mut self) {
        self.metrics = LatencyMetrics::default();
    }

    /// Get time since last encode
    pub fn time_since_last_encode(&self) -> Duration {
        self.last_encode_time.elapsed()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mode_from_str() {
        assert_eq!(
            "interactive".parse::<LatencyMode>().unwrap(),
            LatencyMode::Interactive
        );
        assert_eq!(
            "balanced".parse::<LatencyMode>().unwrap(),
            LatencyMode::Balanced
        );
        assert_eq!(
            "quality".parse::<LatencyMode>().unwrap(),
            LatencyMode::Quality
        );
        assert_eq!(
            "fast".parse::<LatencyMode>().unwrap(),
            LatencyMode::Interactive
        );
    }

    #[test]
    fn test_interactive_immediate_encode() {
        let mut gov = LatencyGovernor::new(LatencyMode::Interactive);

        // Any damage should trigger immediate encode
        let decision = gov.should_encode_frame(0.01);
        assert_eq!(decision, EncodingDecision::EncodeNow);
    }

    #[test]
    fn test_balanced_threshold() {
        let mut gov = LatencyGovernor::new(LatencyMode::Balanced);

        // Below threshold - skip
        let decision = gov.should_encode_frame(0.01);
        assert_eq!(decision, EncodingDecision::Skip);

        // Above threshold (accumulated) - encode
        let decision = gov.should_encode_frame(0.02);
        assert!(decision.should_encode());
    }

    #[test]
    fn test_quality_batching() {
        let mut gov = LatencyGovernor::new(LatencyMode::Quality);

        // Small damage - wait for more
        let decision = gov.should_encode_frame(0.01);
        assert_eq!(decision, EncodingDecision::WaitForMore);

        // More damage - still waiting
        let decision = gov.should_encode_frame(0.02);
        assert_eq!(decision, EncodingDecision::WaitForMore);

        // Enough accumulated - encode batch
        let decision = gov.should_encode_frame(0.03);
        assert_eq!(decision, EncodingDecision::EncodeBatch);
    }

    #[test]
    fn test_encoding_decision_should_encode() {
        assert!(EncodingDecision::EncodeNow.should_encode());
        assert!(EncodingDecision::EncodeKeepalive.should_encode());
        assert!(EncodingDecision::EncodeBatch.should_encode());
        assert!(EncodingDecision::EncodeTimeout.should_encode());
        assert!(!EncodingDecision::Skip.should_encode());
        assert!(!EncodingDecision::WaitForMore.should_encode());
    }

    #[test]
    fn test_adaptive_fps_mode_setting() {
        let gov_interactive = LatencyGovernor::new(LatencyMode::Interactive);
        let gov_balanced = LatencyGovernor::new(LatencyMode::Balanced);

        assert!(!gov_interactive.should_use_adaptive_fps());
        assert!(gov_balanced.should_use_adaptive_fps());
    }
}
