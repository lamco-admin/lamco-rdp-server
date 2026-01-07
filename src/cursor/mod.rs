//! Cursor handling strategies (Premium)
//!
//! This module provides advanced cursor handling for different scenarios,
//! including the innovative **predictive cursor** that compensates for
//! network latency by predicting cursor position.
//!
//! # Cursor Modes
//!
//! | Mode | Description | Latency | Use Case |
//! |------|-------------|---------|----------|
//! | Metadata | Client draws cursor | Lowest | Default, LAN |
//! | Painted | Cursor in video | Medium | Compatibility |
//! | Predictive | Predict position | Feels instant | WAN, high latency |
//!
//! # Predictive Cursor (Innovation)
//!
//! The predictive cursor uses physics-based prediction to display the
//! cursor ahead of where it actually is, compensating for network latency.
//! This makes cursor movement feel instant even with 100ms+ latency.
//!
//! **Algorithm:**
//! - Track cursor velocity and acceleration from recent samples
//! - Predict position N milliseconds ahead (based on measured latency)
//! - Apply smoothing to prevent jitter
//! - Quickly converge when cursor stops
//!
//! # Architecture
//!
//! ```text
//! Input Events
//!   └─> CursorPredictor
//!       ├─> Update history
//!       ├─> Calculate velocity/acceleration
//!       └─> Predict future position
//!
//! Cursor Renderer
//!   └─> Apply prediction offset
//!       └─> Render at predicted position
//! ```

mod predictor;
mod strategy;

pub use predictor::{CursorPredictor, PredictorConfig};
pub use strategy::{CursorMode, CursorStrategy, CursorStrategyConfig};

/// Default lookahead for predictive cursor (ms)
pub const DEFAULT_LOOKAHEAD_MS: f32 = 50.0;

/// Latency threshold above which to enable predictive cursor
pub const PREDICTIVE_LATENCY_THRESHOLD_MS: u32 = 100;
