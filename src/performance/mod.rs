//! Performance optimization modules
//!
//! This module contains performance-related features:
//! - **Adaptive FPS**: Dynamically adjusts frame rate based on screen activity
//! - **Latency Governor**: Configurable latency vs quality tradeoffs
//!
//! # Architecture
//!
//! Both modules work together to optimize bandwidth and CPU usage while
//! maintaining responsive user experience:
//!
//! ```text
//! Frame Capture Loop
//!   └─> Damage Detection
//!       └─> AdaptiveFpsController (determines if we should capture)
//!           └─> LatencyGovernor (determines if we should encode)
//!               └─> Encoding
//! ```
//!
//! # Usage
//!
//! ```no_run
//! use lamco_rdp_server::performance::{AdaptiveFpsController, AdaptiveFpsConfig};
//!
//! let config = AdaptiveFpsConfig::default();
//! let mut fps_controller = AdaptiveFpsController::new(config);
//!
//! // In frame loop:
//! let damage_ratio = 0.5_f32;
//! fps_controller.update(damage_ratio);
//! if fps_controller.should_capture_frame() {
//!     // Capture and encode frame
//! }
//! ```

mod adaptive_fps;
mod latency_governor;

pub use adaptive_fps::{AdaptiveFpsConfig, AdaptiveFpsController, DamageRatio};
pub use latency_governor::{EncodingDecision, LatencyGovernor, LatencyMode};
