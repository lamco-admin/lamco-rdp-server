//! Cursor rendering strategies
//!
//! This module defines different strategies for cursor handling,
//! each optimized for different scenarios.

use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

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
    ///
    /// Predictive rendering is client-side, same as Metadata; it only changes
    /// which position gets sent, not who draws the cursor.
    pub fn requires_compositing(&self) -> bool {
        matches!(self, Self::Painted)
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

impl From<&crate::config::types::CursorPredictorConfig> for PredictorConfig {
    fn from(cfg: &crate::config::types::CursorPredictorConfig) -> Self {
        Self {
            history_size: cfg.history_size,
            lookahead_ms: cfg.lookahead_ms,
            velocity_smoothing: cfg.velocity_smoothing,
            acceleration_smoothing: cfg.acceleration_smoothing,
            max_prediction_distance: cfg.max_prediction_distance,
            min_velocity_threshold: cfg.min_velocity_threshold,
            stop_convergence_rate: cfg.stop_convergence_rate,
        }
    }
}

impl From<&crate::config::types::CursorConfig> for CursorStrategyConfig {
    fn from(cfg: &crate::config::types::CursorConfig) -> Self {
        let mode = cfg.mode.parse().unwrap_or_else(|e| {
            warn!(
                "Invalid cursor.mode {:?} in config, falling back to Metadata: {e}",
                cfg.mode
            );
            CursorMode::Metadata
        });
        Self {
            mode,
            auto_mode: cfg.auto_mode,
            predictive_latency_threshold_ms: cfg.predictive_latency_threshold_ms,
            predictor: (&cfg.predictor).into(),
            cursor_update_fps: cfg.cursor_update_fps,
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

    /// Round-robin cache of recently-sent shapes, keyed by `CursorMeta::id`.
    shape_cache: CursorShapeCache,

    /// Whether a `HidePointer` update has already been sent for the current
    /// hidden/no-cursor span, so it's sent exactly once per transition.
    hidden_sent: bool,
}

/// Number of pointer-cache slots this crate assumes it can safely use.
///
/// Chosen without visibility into the client's actual negotiated
/// `pointerCacheSize` (MS-RDPBCGR 2.2.7.1.5): that value is read and
/// enforced internally by `ironrdp-server` (dropping New Pointer Update/
/// CachedPointer emission entirely when it's zero) but isn't exposed back
/// to this crate. A small, conservative slot count is safe regardless of
/// what a real client negotiates — MS-RDPBCGR only ever requires that value
/// be nonzero to enable the cache at all — at the cost of evicting and
/// re-encoding sooner than a larger cache would for a session that cycles
/// through many distinct shapes.
const SHAPE_CACHE_CAPACITY: usize = 8;

/// Round-robin cache of recently-sent cursor shapes, keyed by the
/// compositor's `CursorMeta::id`.
#[derive(Debug)]
struct CursorShapeCache {
    slots: Vec<Option<u32>>,
    next_evict: usize,
}

impl CursorShapeCache {
    fn new(capacity: usize) -> Self {
        Self {
            slots: vec![None; capacity.max(1)],
            next_evict: 0,
        }
    }

    /// Non-mutating lookup: `Some(index)` if `id` is currently cached.
    fn lookup(&self, id: u32) -> Option<u16> {
        self.slots
            .iter()
            .position(|slot| *slot == Some(id))
            .map(|idx| idx as u16)
    }

    /// Claim a cache slot for `id`, evicting the oldest entry if full.
    /// Callers must only call this once they're actually about to send the
    /// full shape at the returned index — inserting speculatively (e.g.
    /// before confirming a shape was successfully encoded) would leave the
    /// cache claiming a slot the client was never actually sent, so a later
    /// `Hit` on that id would reference nothing real.
    fn insert(&mut self, id: u32) -> u16 {
        let idx = self.next_evict;
        self.slots[idx] = Some(id);
        self.next_evict = (self.next_evict + 1) % self.slots.len();
        idx as u16
    }
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
            shape_cache: CursorShapeCache::new(SHAPE_CACHE_CAPACITY),
            hidden_sent: false,
            config,
        }
    }

    /// Look up a compositor cursor id (`CursorMeta::id`) in the shape cache.
    /// `Some(index)` means the caller should send
    /// `DisplayUpdate::CachedPointer(index)` instead of re-encoding;
    /// `None` means the caller must encode the shape and then call
    /// `cache_shape` (only once it actually has something to send).
    ///
    /// Callers must not pass `id == 0` (the compositor's "invalid/no
    /// cursor" sentinel) — that's a hide signal, not a shape to cache.
    pub fn lookup_shape_cache(&self, id: u32) -> Option<u16> {
        debug_assert_ne!(id, 0, "id == 0 is a hide signal, not a cacheable shape");
        self.shape_cache.lookup(id)
    }

    /// Claim a cache slot for `id` and return its index. Call this only
    /// once a shape has actually been encoded and is about to be sent —
    /// see `CursorShapeCache::insert`'s doc for why claiming speculatively
    /// would corrupt the cache.
    pub fn cache_shape(&mut self, id: u32) -> u16 {
        debug_assert_ne!(id, 0, "id == 0 is a hide signal, not a cacheable shape");
        self.shape_cache.insert(id)
    }

    /// Whether a `HidePointer` update still needs to be sent for the
    /// current hidden/no-cursor span. Marks it sent so callers only send it
    /// once; `note_visible` clears the flag on the next real shape/position.
    pub fn needs_hide_update(&mut self) -> bool {
        if self.hidden_sent {
            false
        } else {
            self.hidden_sent = true;
            true
        }
    }

    /// Clear the hidden/no-cursor tracking after sending a real update.
    pub fn note_visible(&mut self) {
        self.hidden_sent = false;
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

    /// Configured cursor update rate (Hz) for the periodic prediction
    /// re-emission loop.
    pub fn update_fps(&self) -> u32 {
        self.config.cursor_update_fps
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
        assert!(!CursorMode::Predictive.requires_compositing());
        assert!(!CursorMode::Hidden.requires_compositing());
    }

    #[test]
    fn test_lookup_shape_cache_unseen_id_is_none() {
        let strategy = CursorStrategy::new(CursorStrategyConfig::default());
        assert_eq!(strategy.lookup_shape_cache(5), None);
    }

    #[test]
    fn test_cache_shape_then_lookup_is_a_hit_at_the_same_index() {
        let mut strategy = CursorStrategy::new(CursorStrategyConfig::default());
        let index = strategy.cache_shape(5);
        assert_eq!(strategy.lookup_shape_cache(5), Some(index));
    }

    #[test]
    fn test_lookup_without_cache_shape_never_reports_a_hit() {
        // A lookup alone must never claim a slot: only cache_shape does.
        let strategy = CursorStrategy::new(CursorStrategyConfig::default());
        assert_eq!(strategy.lookup_shape_cache(5), None);
        assert_eq!(strategy.lookup_shape_cache(5), None);
    }

    #[test]
    fn test_cache_shape_distinguishes_different_ids() {
        let mut strategy = CursorStrategy::new(CursorStrategyConfig::default());
        let idx_a = strategy.cache_shape(5);
        let idx_b = strategy.cache_shape(6);
        assert_ne!(idx_a, idx_b);
        assert_eq!(strategy.lookup_shape_cache(5), Some(idx_a));
        assert_eq!(strategy.lookup_shape_cache(6), Some(idx_b));
    }

    #[test]
    fn test_cache_shape_survives_interleaved_ids() {
        // Cycling between two shapes must hit the cache for both, not just
        // the immediately-previous one.
        let mut strategy = CursorStrategy::new(CursorStrategyConfig::default());
        let idx_a = strategy.cache_shape(1);
        let idx_b = strategy.cache_shape(2);
        assert_eq!(strategy.lookup_shape_cache(1), Some(idx_a));
        assert_eq!(strategy.lookup_shape_cache(2), Some(idx_b));
        assert_eq!(strategy.lookup_shape_cache(1), Some(idx_a));
    }

    #[test]
    fn test_cache_shape_evicts_oldest_slot_when_full() {
        let mut strategy = CursorStrategy::new(CursorStrategyConfig::default());
        for id in 1..=SHAPE_CACHE_CAPACITY as u32 {
            strategy.cache_shape(id);
        }
        // Cache is now full (ids 1..=CAPACITY, one per slot). One more
        // distinct id must evict the oldest (id 1) rather than reuse a
        // still-live slot.
        let evicted_slot = strategy.cache_shape(SHAPE_CACHE_CAPACITY as u32 + 1);
        assert_eq!(evicted_slot, 0);
        // id 1's original slot (0) has been reused: id 1 is no longer cached.
        assert_eq!(strategy.lookup_shape_cache(1), None);
    }

    #[test]
    fn test_needs_hide_update_fires_once_per_span() {
        let mut strategy = CursorStrategy::new(CursorStrategyConfig::default());
        assert!(strategy.needs_hide_update());
        assert!(!strategy.needs_hide_update());
        strategy.note_visible();
        assert!(strategy.needs_hide_update());
    }

    #[test]
    fn test_config_conversion_parses_mode_and_carries_predictor_fields() {
        let config = crate::config::types::CursorConfig {
            mode: "predictive".to_string(),
            auto_mode: false,
            predictive_latency_threshold_ms: 42,
            cursor_update_fps: 30,
            predictor: crate::config::types::CursorPredictorConfig {
                history_size: 3,
                lookahead_ms: 10.0,
                velocity_smoothing: 0.1,
                acceleration_smoothing: 0.2,
                max_prediction_distance: 50,
                min_velocity_threshold: 1.0,
                stop_convergence_rate: 0.5,
            },
        };

        let strategy_config: CursorStrategyConfig = (&config).into();
        assert_eq!(strategy_config.mode, CursorMode::Predictive);
        assert!(!strategy_config.auto_mode);
        assert_eq!(strategy_config.predictive_latency_threshold_ms, 42);
        assert_eq!(strategy_config.cursor_update_fps, 30);
        assert_eq!(strategy_config.predictor.history_size, 3);
        assert_eq!(strategy_config.predictor.max_prediction_distance, 50);
    }

    #[test]
    fn test_config_conversion_falls_back_to_metadata_on_invalid_mode() {
        let mut config = crate::config::types::CursorConfig::default();
        config.mode = "not-a-real-mode".to_string();

        let strategy_config: CursorStrategyConfig = (&config).into();
        assert_eq!(strategy_config.mode, CursorMode::Metadata);
    }
}
