//! Cursor rendering strategies
//!
//! This module defines different strategies for cursor handling,
//! each optimized for different scenarios.

use serde::{Deserialize, Serialize};
use tracing::debug;

use super::predictor::{CursorPredictor, PredictorConfig};

/// Cursor rendering mode
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum CursorMode {
    /// Client-side cursor rendering (lowest latency)
    /// Server sends cursor shape and position metadata.
    #[default]
    Metadata,

    /// Cursor painted into video frames
    /// Works with all clients but has video latency.
    Painted,

    /// Hidden cursor (for touch/pen input)
    Hidden,

    /// Predictive cursor rendering (Premium)
    /// Uses physics-based prediction to compensate for latency.
    Predictive,
}

impl CursorMode {
    /// Get human-readable description
    pub fn description(&self) -> &'static str {
        match self {
            Self::Metadata => "Client-side rendering (lowest latency)",
            Self::Painted => "Painted in video (maximum compatibility)",
            Self::Hidden => "Cursor hidden",
            Self::Predictive => "Predictive rendering (compensates for latency)",
        }
    }

    /// Check if this mode requires server-side cursor compositing
    pub fn requires_compositing(&self) -> bool {
        matches!(self, Self::Painted | Self::Predictive)
    }
}

impl std::fmt::Display for CursorMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Metadata => write!(f, "Metadata"),
            Self::Painted => write!(f, "Painted"),
            Self::Hidden => write!(f, "Hidden"),
            Self::Predictive => write!(f, "Predictive"),
        }
    }
}

impl std::str::FromStr for CursorMode {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "metadata" | "client" | "default" => Ok(Self::Metadata),
            "painted" | "embedded" | "composite" => Ok(Self::Painted),
            "hidden" | "none" | "off" => Ok(Self::Hidden),
            "predictive" | "predict" | "physics" => Ok(Self::Predictive),
            _ => Err(format!("Unknown cursor mode: {s}")),
        }
    }
}

/// Configuration for cursor strategy
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CursorStrategyConfig {
    /// Cursor rendering mode
    #[serde(default)]
    pub mode: CursorMode,

    /// Enable automatic mode selection based on latency
    #[serde(default = "default_true")]
    pub auto_mode: bool,

    /// Latency threshold (ms) above which to enable predictive mode
    #[serde(default = "default_latency_threshold")]
    pub predictive_latency_threshold_ms: u32,

    /// Predictor configuration (for predictive mode)
    #[serde(default)]
    pub predictor: PredictorConfig,

    /// Cursor update rate for separate stream (FPS)
    #[serde(default = "default_cursor_fps")]
    pub cursor_update_fps: u32,
}

fn default_true() -> bool {
    true
}
fn default_latency_threshold() -> u32 {
    100
}
fn default_cursor_fps() -> u32 {
    60
}

impl Default for CursorStrategyConfig {
    fn default() -> Self {
        Self {
            mode: CursorMode::Metadata,
            auto_mode: true,
            predictive_latency_threshold_ms: 100,
            predictor: PredictorConfig::default(),
            cursor_update_fps: 60,
        }
    }
}

/// Cursor strategy manager
///
/// Manages cursor rendering mode and handles automatic
/// mode switching based on measured latency.
pub struct CursorStrategy {
    /// Configuration
    config: CursorStrategyConfig,

    /// Current active mode
    active_mode: CursorMode,

    /// Cursor predictor (for predictive mode)
    predictor: Option<CursorPredictor>,

    /// Measured network latency (ms)
    measured_latency_ms: u32,

    /// Current cursor position
    current_position: (i32, i32),

    /// Current cursor shape (for metadata mode)
    current_shape: Option<CursorShape>,
}

/// Cursor shape information
#[derive(Debug, Clone)]
pub struct CursorShape {
    /// Width in pixels
    pub width: u32,
    /// Height in pixels
    pub height: u32,
    /// Hotspot X offset
    pub hotspot_x: u32,
    /// Hotspot Y offset
    pub hotspot_y: u32,
    /// Pixel data (RGBA)
    pub data: Vec<u8>,
}

impl CursorStrategy {
    pub fn new(config: CursorStrategyConfig) -> Self {
        let predictor = if config.mode == CursorMode::Predictive {
            Some(CursorPredictor::new(config.predictor.clone()))
        } else {
            None
        };

        Self {
            active_mode: config.mode,
            predictor,
            measured_latency_ms: 0,
            current_position: (0, 0),
            current_shape: None,
            config,
        }
    }

    /// Update cursor position
    pub fn update_position(&mut self, x: i32, y: i32) {
        self.current_position = (x, y);

        if let Some(ref mut predictor) = self.predictor {
            predictor.update(x, y);
        }
    }

    /// Update cursor shape
    pub fn update_shape(&mut self, shape: CursorShape) {
        self.current_shape = Some(shape);
    }

    /// Update measured network latency
    pub fn update_latency(&mut self, latency_ms: u32) {
        self.measured_latency_ms = latency_ms;

        // Auto-switch mode if enabled
        if self.config.auto_mode {
            self.auto_select_mode();
        }

        // Update predictor lookahead based on latency
        if let Some(ref mut predictor) = self.predictor {
            // Use 50-100% of measured latency as lookahead
            let lookahead = (latency_ms as f32 * 0.75).clamp(20.0, 150.0);
            predictor.set_lookahead(lookahead);
        }
    }

    /// Get cursor position to render
    ///
    /// Returns predicted position if in predictive mode,
    /// otherwise returns actual position.
    pub fn render_position(&mut self) -> (i32, i32) {
        match self.active_mode {
            CursorMode::Predictive => {
                if let Some(ref mut predictor) = self.predictor {
                    predictor.get_predicted_position()
                } else {
                    self.current_position
                }
            }
            _ => self.current_position,
        }
    }

    /// Get actual cursor position
    pub fn actual_position(&self) -> (i32, i32) {
        self.current_position
    }

    /// Get current cursor shape
    pub fn shape(&self) -> Option<&CursorShape> {
        self.current_shape.as_ref()
    }

    /// Get active cursor mode
    pub fn mode(&self) -> CursorMode {
        self.active_mode
    }

    /// Set cursor mode explicitly
    pub fn set_mode(&mut self, mode: CursorMode) {
        if mode != self.active_mode {
            debug!("Cursor mode changed: {:?} -> {:?}", self.active_mode, mode);
            self.active_mode = mode;

            // Create or destroy predictor as needed
            match mode {
                CursorMode::Predictive => {
                    if self.predictor.is_none() {
                        self.predictor = Some(CursorPredictor::new(self.config.predictor.clone()));
                    }
                }
                _ => {
                    self.predictor = None;
                }
            }
        }
    }

    /// Get measured latency
    pub fn latency(&self) -> u32 {
        self.measured_latency_ms
    }

    /// Check if cursor compositing is needed
    pub fn needs_compositing(&self) -> bool {
        self.active_mode.requires_compositing()
    }

    /// Get cursor predictor (if in predictive mode)
    pub fn predictor(&self) -> Option<&CursorPredictor> {
        self.predictor.as_ref()
    }

    fn auto_select_mode(&mut self) {
        let should_predict = self.measured_latency_ms > self.config.predictive_latency_threshold_ms;

        let new_mode = if should_predict {
            CursorMode::Predictive
        } else {
            self.config.mode // Fall back to configured default
        };

        if new_mode != self.active_mode {
            debug!(
                "Auto-switching cursor mode: {:?} -> {:?} (latency={}ms)",
                self.active_mode, new_mode, self.measured_latency_ms
            );
            self.set_mode(new_mode);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cursor_mode_from_str() {
        assert_eq!(
            "metadata".parse::<CursorMode>().unwrap(),
            CursorMode::Metadata
        );
        assert_eq!(
            "predictive".parse::<CursorMode>().unwrap(),
            CursorMode::Predictive
        );
        assert_eq!("hidden".parse::<CursorMode>().unwrap(), CursorMode::Hidden);
    }

    #[test]
    fn test_default_config() {
        let config = CursorStrategyConfig::default();
        assert_eq!(config.mode, CursorMode::Metadata);
        assert!(config.auto_mode);
        assert_eq!(config.predictive_latency_threshold_ms, 100);
    }

    #[test]
    fn test_auto_mode_switching() {
        let mut config = CursorStrategyConfig::default();
        config.auto_mode = true;
        config.predictive_latency_threshold_ms = 100;

        let mut strategy = CursorStrategy::new(config);

        // Low latency - should stay in metadata mode
        strategy.update_latency(50);
        assert_eq!(strategy.mode(), CursorMode::Metadata);

        // High latency - should switch to predictive
        strategy.update_latency(150);
        assert_eq!(strategy.mode(), CursorMode::Predictive);

        // Low latency again - should switch back
        strategy.update_latency(50);
        assert_eq!(strategy.mode(), CursorMode::Metadata);
    }

    #[test]
    fn test_predictive_mode_creates_predictor() {
        let mut config = CursorStrategyConfig::default();
        config.mode = CursorMode::Predictive;

        let strategy = CursorStrategy::new(config);
        assert!(strategy.predictor().is_some());
    }

    #[test]
    fn test_compositing_required() {
        assert!(!CursorMode::Metadata.requires_compositing());
        assert!(CursorMode::Painted.requires_compositing());
        assert!(CursorMode::Predictive.requires_compositing());
        assert!(!CursorMode::Hidden.requires_compositing());
    }
}
