//! Encoding Adaptation — closed-loop QP and frame rate control
//!
//! Uses client feedback (EGFX queue_depth and QoE decode+render time)
//! to dynamically adjust encoding quality. Closes the open loop between
//! the encoding pipeline and client performance.
//!
//! # Design
//!
//! The adaptation uses a damped exponential moving average to prevent
//! oscillation. Evaluation happens at configurable intervals (default 500ms),
//! not per-frame, to provide stable quality transitions.
//!
//! # Feedback Signals
//!
//! | Signal | Source | Meaning |
//! |--------|--------|---------|
//! | queue_depth | EGFX FrameAck | Frames waiting in client decode queue |
//! | client_decode_render_us | EGFX QoE (V10.4+) | Client GPU decode+render time |

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use tracing::debug;

use crate::health::performance::EgfxSnapshot;

/// Configuration for encoding adaptation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EncodingAdaptationConfig {
    /// Enable adaptive QP based on client feedback
    #[serde(default)]
    pub enabled: bool,

    /// Base QP value (replaces hardcoded 22)
    #[serde(default = "default_base_qp")]
    pub base_qp: u32,

    /// Minimum QP (highest quality)
    #[serde(default = "default_min_qp")]
    pub min_qp: u32,

    /// Maximum QP (lowest quality)
    #[serde(default = "default_max_qp")]
    pub max_qp: u32,

    /// Minimum interval between adaptation evaluations (ms)
    #[serde(default = "default_evaluation_interval_ms")]
    pub evaluation_interval_ms: u32,

    /// Queue depth threshold for moderate backpressure
    #[serde(default = "default_moderate_queue_threshold")]
    pub moderate_queue_threshold: u32,

    /// Queue depth threshold for severe backpressure
    #[serde(default = "default_severe_queue_threshold")]
    pub severe_queue_threshold: u32,
}

fn default_base_qp() -> u32 {
    22
}
fn default_min_qp() -> u32 {
    18
}
fn default_max_qp() -> u32 {
    42
}
fn default_evaluation_interval_ms() -> u32 {
    500
}
fn default_moderate_queue_threshold() -> u32 {
    3
}
fn default_severe_queue_threshold() -> u32 {
    6
}

impl Default for EncodingAdaptationConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            base_qp: default_base_qp(),
            min_qp: default_min_qp(),
            max_qp: default_max_qp(),
            evaluation_interval_ms: default_evaluation_interval_ms(),
            moderate_queue_threshold: default_moderate_queue_threshold(),
            severe_queue_threshold: default_severe_queue_threshold(),
        }
    }
}

/// Closed-loop encoding adaptation based on EGFX client feedback.
///
/// Reads queue_depth and client decode/render time from the shared
/// EgfxSnapshot and produces a damped QP value for the encoder.
pub struct EncodingAdaptation {
    config: EncodingAdaptationConfig,
    egfx_state: Arc<RwLock<EgfxSnapshot>>,
    current_qp: u32,
    last_evaluation: Instant,
    evaluation_interval: Duration,
}

impl EncodingAdaptation {
    pub fn new(config: EncodingAdaptationConfig, egfx_state: Arc<RwLock<EgfxSnapshot>>) -> Self {
        let evaluation_interval = Duration::from_millis(u64::from(config.evaluation_interval_ms));
        let current_qp = config.base_qp;
        Self {
            config,
            egfx_state,
            current_qp,
            last_evaluation: Instant::now(),
            evaluation_interval,
        }
    }

    /// Get the current adapted QP value.
    ///
    /// When adaptation is disabled, returns the base QP from config.
    /// When enabled, returns a damped QP based on client feedback.
    pub fn adapted_qp(&mut self) -> u32 {
        if !self.config.enabled {
            return self.config.base_qp;
        }

        // Only evaluate at configured intervals to prevent oscillation
        if self.last_evaluation.elapsed() < self.evaluation_interval {
            return self.current_qp;
        }
        self.last_evaluation = Instant::now();

        let egfx = self.egfx_state.read();
        let queue_depth = egfx.queue_depth;

        // Target QP based on queue depth feedback
        let target_qp = if queue_depth > self.config.severe_queue_threshold {
            // Severe backpressure: significant quality reduction
            (self.config.base_qp + 10).min(self.config.max_qp)
        } else if queue_depth > self.config.moderate_queue_threshold {
            // Moderate backpressure: mild quality reduction
            (self.config.base_qp + 4).min(self.config.max_qp)
        } else if queue_depth == 0 {
            // Client caught up: improve quality toward base
            self.config
                .base_qp
                .saturating_sub(2)
                .max(self.config.min_qp)
        } else {
            // Normal operation: use base QP
            self.config.base_qp
        };

        let prev_qp = self.current_qp;

        // Damped exponential moving average: 75% current + 25% target
        // Prevents rapid oscillation while still responding to sustained changes
        self.current_qp =
            ((self.current_qp * 3 + target_qp) / 4).clamp(self.config.min_qp, self.config.max_qp);

        if self.current_qp != prev_qp {
            debug!(
                "QP adaptation: {} → {} (queue_depth={}, target={})",
                prev_qp, self.current_qp, queue_depth, target_qp
            );
        }

        self.current_qp
    }

    /// Check if client decode time suggests reducing frame rate.
    ///
    /// Returns true if the client is consistently taking >30ms to decode+render,
    /// suggesting the GPU is saturated and sending fewer frames would help.
    pub fn should_reduce_fps(&self) -> bool {
        if !self.config.enabled {
            return false;
        }

        let egfx = self.egfx_state.read();
        // 30ms decode+render time threshold (30000 microseconds)
        matches!(egfx.client_decode_render_us, Some(us) if us > 30_000)
    }

    /// Whether adaptation is enabled
    pub fn is_enabled(&self) -> bool {
        self.config.enabled
    }

    /// Current QP without re-evaluating
    pub fn current_qp(&self) -> u32 {
        self.current_qp
    }
}

#[cfg(test)]
#[expect(
    clippy::unchecked_time_subtraction,
    reason = "tests run well after process start; Instant - 1s never underflows the monotonic clock"
)]
mod tests {
    use super::*;

    fn test_state(queue_depth: u32) -> Arc<RwLock<EgfxSnapshot>> {
        Arc::new(RwLock::new(EgfxSnapshot {
            channel_ready: true,
            queue_depth,
            ..EgfxSnapshot::default()
        }))
    }

    #[test]
    fn disabled_returns_base_qp() {
        let state = test_state(10);
        let config = EncodingAdaptationConfig::default(); // enabled: false
        let mut adapt = EncodingAdaptation::new(config, state);
        assert_eq!(adapt.adapted_qp(), 22);
    }

    #[test]
    fn enabled_responds_to_queue_depth() {
        let state = test_state(0);
        let mut config = EncodingAdaptationConfig::default();
        config.enabled = true;
        config.evaluation_interval_ms = 0; // Evaluate every call for testing
        let mut adapt = EncodingAdaptation::new(config, state.clone());

        // Queue depth 0: should decrease toward (base - 2) = 20
        let qp = adapt.adapted_qp();
        assert!(qp <= 22, "Expected QP <= 22 with empty queue, got {qp}");

        // Simulate severe backpressure
        state.write().queue_depth = 8;
        // Need to wait for evaluation interval (0ms in test)
        adapt.last_evaluation = Instant::now() - Duration::from_secs(1);
        let qp = adapt.adapted_qp();
        assert!(qp > 22, "Expected QP > 22 with high queue, got {qp}");
    }

    #[test]
    fn qp_stays_within_bounds() {
        let state = test_state(100); // Extreme backpressure
        let mut config = EncodingAdaptationConfig::default();
        config.enabled = true;
        config.evaluation_interval_ms = 0;
        let mut adapt = EncodingAdaptation::new(config, state);

        // Run many iterations to ensure damped convergence
        for _ in 0..100 {
            adapt.last_evaluation = Instant::now() - Duration::from_secs(1);
            let qp = adapt.adapted_qp();
            assert!(qp >= 18, "QP below min: {qp}");
            assert!(qp <= 42, "QP above max: {qp}");
        }
    }

    #[test]
    fn damping_prevents_oscillation() {
        let state = test_state(0);
        let mut config = EncodingAdaptationConfig::default();
        config.enabled = true;
        config.evaluation_interval_ms = 0;
        let mut adapt = EncodingAdaptation::new(config, state.clone());

        // Alternate between empty and full queue
        let mut qp_values = Vec::new();
        for i in 0..20 {
            state.write().queue_depth = if i % 2 == 0 { 0 } else { 8 };
            adapt.last_evaluation = Instant::now() - Duration::from_secs(1);
            qp_values.push(adapt.adapted_qp());
        }

        // Check that consecutive QP values don't swing wildly
        for window in qp_values.windows(2) {
            let diff = window[0].abs_diff(window[1]);
            assert!(
                diff <= 5,
                "QP swing too large: {} → {} (diff {})",
                window[0],
                window[1],
                diff
            );
        }
    }

    #[test]
    fn should_reduce_fps_with_slow_client() {
        let state = Arc::new(RwLock::new(EgfxSnapshot {
            channel_ready: true,
            client_decode_render_us: Some(40_000), // 40ms
            ..EgfxSnapshot::default()
        }));
        let mut config = EncodingAdaptationConfig::default();
        config.enabled = true;
        let adapt = EncodingAdaptation::new(config, state);
        assert!(adapt.should_reduce_fps());
    }

    #[test]
    fn should_not_reduce_fps_with_fast_client() {
        let state = Arc::new(RwLock::new(EgfxSnapshot {
            channel_ready: true,
            client_decode_render_us: Some(5_000), // 5ms
            ..EgfxSnapshot::default()
        }));
        let mut config = EncodingAdaptationConfig::default();
        config.enabled = true;
        let adapt = EncodingAdaptation::new(config, state);
        assert!(!adapt.should_reduce_fps());
    }
}
