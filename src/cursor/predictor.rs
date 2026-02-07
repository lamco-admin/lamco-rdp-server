//! Predictive Cursor
//!
//! Physics-based cursor prediction to compensate for network latency.
//! Uses velocity and acceleration tracking to predict where the cursor
//! will be N milliseconds in the future.
//!
//! # Physics Model
//!
//! ```text
//! position(t) = position(0) + velocity * t + 0.5 * acceleration * t²
//! ```
//!
//! Where t is the lookahead time (typically 50-100ms based on latency).
//!
//! # Smoothing
//!
//! Velocity and acceleration are smoothed using exponential moving average
//! to prevent jitter from input noise:
//!
//! ```text
//! velocity_smooth = α * velocity_new + (1 - α) * velocity_old
//! ```

use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::time::Instant;
use tracing::trace;

/// Configuration for cursor predictor
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PredictorConfig {
    /// Number of samples to keep in history
    #[serde(default = "default_history_size")]
    pub history_size: usize,

    /// Default lookahead time (ms)
    #[serde(default = "default_lookahead_ms")]
    pub lookahead_ms: f32,

    /// Velocity smoothing factor (0.0-1.0, higher = more responsive)
    #[serde(default = "default_velocity_smoothing")]
    pub velocity_smoothing: f32,

    /// Acceleration smoothing factor (0.0-1.0)
    #[serde(default = "default_accel_smoothing")]
    pub acceleration_smoothing: f32,

    /// Maximum prediction distance (pixels)
    #[serde(default = "default_max_prediction")]
    pub max_prediction_distance: i32,

    /// Minimum velocity to apply prediction (pixels/second)
    #[serde(default = "default_min_velocity")]
    pub min_velocity_threshold: f32,

    /// Convergence rate when cursor stops (0.0-1.0)
    #[serde(default = "default_convergence")]
    pub stop_convergence_rate: f32,
}

fn default_history_size() -> usize {
    8
}
fn default_lookahead_ms() -> f32 {
    50.0
}
fn default_velocity_smoothing() -> f32 {
    0.4
}
fn default_accel_smoothing() -> f32 {
    0.2
}
fn default_max_prediction() -> i32 {
    100
}
fn default_min_velocity() -> f32 {
    50.0
}
fn default_convergence() -> f32 {
    0.5
}

impl Default for PredictorConfig {
    fn default() -> Self {
        Self {
            history_size: default_history_size(),
            lookahead_ms: default_lookahead_ms(),
            velocity_smoothing: default_velocity_smoothing(),
            acceleration_smoothing: default_accel_smoothing(),
            max_prediction_distance: default_max_prediction(),
            min_velocity_threshold: default_min_velocity(),
            stop_convergence_rate: default_convergence(),
        }
    }
}

/// Cursor position sample with timestamp
#[derive(Debug, Clone, Copy)]
struct CursorSample {
    x: i32,
    y: i32,
    timestamp: Instant,
}

/// Cursor predictor using physics-based prediction
pub struct CursorPredictor {
    /// Configuration
    config: PredictorConfig,

    /// Current actual position
    position: (i32, i32),

    /// Current predicted position
    predicted_position: (i32, i32),

    /// Smoothed velocity (pixels/second)
    velocity: (f32, f32),

    /// Smoothed acceleration (pixels/second²)
    acceleration: (f32, f32),

    /// Position history for velocity calculation
    history: VecDeque<CursorSample>,

    /// Is cursor currently moving?
    is_moving: bool,

    /// Frames since last movement
    frames_since_move: u32,
}

impl CursorPredictor {
    pub fn new(config: PredictorConfig) -> Self {
        Self {
            history: VecDeque::with_capacity(config.history_size),
            config,
            position: (0, 0),
            predicted_position: (0, 0),
            velocity: (0.0, 0.0),
            acceleration: (0.0, 0.0),
            is_moving: false,
            frames_since_move: 0,
        }
    }

    /// Update with new cursor position
    pub fn update(&mut self, x: i32, y: i32) {
        let now = Instant::now();
        let moved = x != self.position.0 || y != self.position.1;

        // Track movement state
        if moved {
            self.is_moving = true;
            self.frames_since_move = 0;
        } else {
            self.frames_since_move += 1;
            if self.frames_since_move > 3 {
                self.is_moving = false;
            }
        }

        // Update position
        self.position = (x, y);

        // Add to history
        self.history.push_back(CursorSample {
            x,
            y,
            timestamp: now,
        });

        // Limit history size
        while self.history.len() > self.config.history_size {
            self.history.pop_front();
        }

        // Update velocity and acceleration
        self.update_velocity();
        self.update_acceleration();

        trace!(
            "Cursor update: pos=({}, {}), vel=({:.1}, {:.1}), moving={}",
            x,
            y,
            self.velocity.0,
            self.velocity.1,
            self.is_moving
        );
    }

    /// Predict cursor position at given lookahead time
    pub fn predict(&self, lookahead_ms: f32) -> (i32, i32) {
        // If not moving, converge to actual position
        if !self.is_moving {
            let rate = self.config.stop_convergence_rate;
            let dx = (self.position.0 - self.predicted_position.0) as f32 * rate;
            let dy = (self.position.1 - self.predicted_position.1) as f32 * rate;
            return (
                (self.predicted_position.0 as f32 + dx) as i32,
                (self.predicted_position.1 as f32 + dy) as i32,
            );
        }

        // Check if velocity is above threshold
        let speed = (self.velocity.0.powi(2) + self.velocity.1.powi(2)).sqrt();
        if speed < self.config.min_velocity_threshold {
            return self.position;
        }

        // Physics-based prediction: pos + vel*t + 0.5*acc*t²
        let dt = lookahead_ms / 1000.0;

        let pred_x =
            self.position.0 as f32 + self.velocity.0 * dt + 0.5 * self.acceleration.0 * dt * dt;

        let pred_y =
            self.position.1 as f32 + self.velocity.1 * dt + 0.5 * self.acceleration.1 * dt * dt;

        // Clamp to maximum prediction distance
        let dx = pred_x - self.position.0 as f32;
        let dy = pred_y - self.position.1 as f32;
        let dist = (dx * dx + dy * dy).sqrt();

        if dist > self.config.max_prediction_distance as f32 {
            let scale = self.config.max_prediction_distance as f32 / dist;
            (
                (self.position.0 as f32 + dx * scale) as i32,
                (self.position.1 as f32 + dy * scale) as i32,
            )
        } else {
            (pred_x as i32, pred_y as i32)
        }
    }

    /// Get predicted position using configured lookahead
    pub fn get_predicted_position(&mut self) -> (i32, i32) {
        self.predicted_position = self.predict(self.config.lookahead_ms);
        self.predicted_position
    }

    /// Get current actual position
    pub fn actual_position(&self) -> (i32, i32) {
        self.position
    }

    /// Get current velocity (pixels/second)
    pub fn velocity(&self) -> (f32, f32) {
        self.velocity
    }

    /// Get current speed (magnitude of velocity)
    pub fn speed(&self) -> f32 {
        (self.velocity.0.powi(2) + self.velocity.1.powi(2)).sqrt()
    }

    /// Check if cursor is currently moving
    pub fn is_moving(&self) -> bool {
        self.is_moving
    }

    /// Set lookahead time (for dynamic adjustment based on latency)
    pub fn set_lookahead(&mut self, lookahead_ms: f32) {
        self.config.lookahead_ms = lookahead_ms;
    }

    /// Get current lookahead time
    pub fn lookahead(&self) -> f32 {
        self.config.lookahead_ms
    }

    /// Reset predictor state
    pub fn reset(&mut self) {
        self.history.clear();
        self.velocity = (0.0, 0.0);
        self.acceleration = (0.0, 0.0);
        self.is_moving = false;
        self.frames_since_move = 0;
    }

    fn update_velocity(&mut self) {
        if self.history.len() < 2 {
            return;
        }

        let recent = &self.history[self.history.len() - 1];
        let prev = &self.history[self.history.len() - 2];

        let dt = recent
            .timestamp
            .duration_since(prev.timestamp)
            .as_secs_f32();
        if dt <= 0.0 {
            return;
        }

        // Calculate instantaneous velocity
        let vx = (recent.x - prev.x) as f32 / dt;
        let vy = (recent.y - prev.y) as f32 / dt;

        // Apply exponential smoothing
        let alpha = self.config.velocity_smoothing;
        self.velocity.0 = alpha * vx + (1.0 - alpha) * self.velocity.0;
        self.velocity.1 = alpha * vy + (1.0 - alpha) * self.velocity.1;
    }

    fn update_acceleration(&mut self) {
        if self.history.len() < 3 {
            return;
        }

        let n = self.history.len();
        let recent = &self.history[n - 1];
        let mid = &self.history[n - 2];
        let prev = &self.history[n - 3];

        let dt1 = recent.timestamp.duration_since(mid.timestamp).as_secs_f32();
        let dt2 = mid.timestamp.duration_since(prev.timestamp).as_secs_f32();

        if dt1 <= 0.0 || dt2 <= 0.0 {
            return;
        }

        // Calculate velocity at two points
        let v1x = (recent.x - mid.x) as f32 / dt1;
        let v1y = (recent.y - mid.y) as f32 / dt1;
        let v2x = (mid.x - prev.x) as f32 / dt2;
        let v2y = (mid.y - prev.y) as f32 / dt2;

        // Calculate acceleration
        let dt = (dt1 + dt2) / 2.0;
        let ax = (v1x - v2x) / dt;
        let ay = (v1y - v2y) / dt;

        // Apply smoothing
        let alpha = self.config.acceleration_smoothing;
        self.acceleration.0 = alpha * ax + (1.0 - alpha) * self.acceleration.0;
        self.acceleration.1 = alpha * ay + (1.0 - alpha) * self.acceleration.1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread::sleep;
    use std::time::Duration;

    #[test]
    fn test_default_config() {
        let config = PredictorConfig::default();
        assert_eq!(config.history_size, 8);
        assert_eq!(config.lookahead_ms, 50.0);
    }

    #[test]
    fn test_stationary_cursor() {
        let config = PredictorConfig::default();
        let mut predictor = CursorPredictor::new(config);

        // Update with same position and let prediction converge
        for _ in 0..20 {
            predictor.update(100, 100);
            // Call get_predicted_position to update internal predicted_position state
            let _ = predictor.get_predicted_position();
            sleep(Duration::from_millis(16));
        }

        // Prediction should converge to actual after multiple iterations
        let predicted = predictor.get_predicted_position();
        // Allow more tolerance since convergence is exponential
        assert!(
            (predicted.0 - 100).abs() < 20,
            "X prediction {} not near 100",
            predicted.0
        );
        assert!(
            (predicted.1 - 100).abs() < 20,
            "Y prediction {} not near 100",
            predicted.1
        );
    }

    #[test]
    fn test_moving_cursor() {
        let config = PredictorConfig::default();
        let mut predictor = CursorPredictor::new(config);

        // Simulate cursor moving right at ~600 pixels/second
        for i in 0..10 {
            predictor.update(100 + i * 10, 100);
            sleep(Duration::from_millis(16));
        }

        // Prediction should be ahead of actual position
        let actual = predictor.actual_position();
        let predicted = predictor.predict(50.0);

        // Predicted X should be greater than actual (moving right)
        assert!(
            predicted.0 > actual.0,
            "Predicted {} should be > actual {}",
            predicted.0,
            actual.0
        );
    }

    #[test]
    fn test_max_prediction_distance() {
        let mut config = PredictorConfig::default();
        config.max_prediction_distance = 20;

        let mut predictor = CursorPredictor::new(config);

        // Very fast movement
        for i in 0..10 {
            predictor.update(i * 100, 0);
            sleep(Duration::from_millis(16));
        }

        let actual = predictor.actual_position();
        let predicted = predictor.predict(100.0);

        // Distance should be clamped
        let dist = ((predicted.0 - actual.0).pow(2) + (predicted.1 - actual.1).pow(2)) as f32;
        let dist = dist.sqrt();

        assert!(
            dist <= 25.0, // Allow small margin for rounding
            "Distance {} should be <= 25",
            dist
        );
    }

    #[test]
    fn test_velocity_calculation() {
        let config = PredictorConfig::default();
        let mut predictor = CursorPredictor::new(config);

        // Move at consistent speed
        for i in 0..10 {
            predictor.update(i * 10, 0);
            sleep(Duration::from_millis(16));
        }

        // Velocity should be positive X, zero Y
        let vel = predictor.velocity();
        assert!(vel.0 > 0.0, "X velocity should be positive: {}", vel.0);
        assert!(
            vel.1.abs() < 10.0,
            "Y velocity should be near zero: {}",
            vel.1
        );
    }
}
