//! Performance Snapshot — unified point-in-time performance data
//!
//! **Execution Path:** Cross-cutting (all session strategies)
//! **Status:** Active (v1.5.0+)
//!
//! Aggregates all performance data sources into a single serializable
//! struct. This is the canonical API boundary for all monitoring
//! consumers (D-Bus, Prometheus, HTTP /health, GUI).

use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};

use super::sensors::SensorSnapshot;

/// Point-in-time snapshot of all server performance metrics.
///
/// Every monitoring consumer (D-Bus, Prometheus, HTTP, GUI) projects
/// from this struct rather than querying data sources independently.
/// This prevents inconsistency across tiers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PerformanceSnapshot {
    /// When this snapshot was captured
    pub timestamp: SystemTime,

    /// Server uptime
    pub uptime: Duration,

    /// Adaptive FPS controller state
    pub fps: FpsSnapshot,

    /// Latency governor state
    pub latency: LatencySnapshot,

    /// EGFX pipeline state
    pub egfx: EgfxSnapshot,

    /// Hardware encoder state (None if software encoding)
    pub encoder: Option<EncoderSnapshot>,

    /// Raw metrics from MetricsCollector
    pub metrics: MetricsSnapshot,

    /// Version-adaptive sensor snapshots from all registered protocol sensors
    #[serde(default)]
    pub sensor_snapshots: Vec<SensorSnapshot>,
}

/// Adaptive FPS controller state
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FpsSnapshot {
    /// Whether adaptive FPS is enabled
    pub enabled: bool,
    /// Current target FPS
    pub current_fps: u32,
    /// Current activity level
    pub activity_level: String,
    /// Which source produced the damage ratio driving the current activity
    /// level: "compositor-hint", "pixel-diff", "forced", or "disabled". See
    /// `performance::adaptive_fps` module docs — this matters because
    /// different Wayland damage protocols report damage at different
    /// granularity for the same visual change.
    #[serde(default)]
    pub damage_source: String,
    /// Total frames processed by the FPS controller
    pub frames_processed: u64,
    /// Total frames skipped by the FPS controller
    pub frames_skipped: u64,
    /// Time spent at each activity level
    pub time_at_static: Duration,
    pub time_at_low: Duration,
    pub time_at_medium: Duration,
    pub time_at_high: Duration,
}

impl Default for FpsSnapshot {
    fn default() -> Self {
        Self {
            enabled: true,
            current_fps: 30,
            activity_level: "High".into(),
            damage_source: String::new(),
            frames_processed: 0,
            frames_skipped: 0,
            time_at_static: Duration::ZERO,
            time_at_low: Duration::ZERO,
            time_at_medium: Duration::ZERO,
            time_at_high: Duration::ZERO,
        }
    }
}

/// Latency governor state
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LatencySnapshot {
    /// Current latency mode name
    pub mode: String,
    /// Average capture-to-encode time (ms)
    pub capture_to_encode_avg_ms: f32,
    /// Average encode duration (ms)
    pub encode_duration_avg_ms: f32,
    /// Average total latency (ms)
    pub total_latency_avg_ms: f32,
    /// Total frames encoded
    pub frames_encoded: u64,
    /// Total frames skipped
    pub frames_skipped: u64,
    /// Total batches encoded (Quality mode)
    pub batches_encoded: u64,
}

impl Default for LatencySnapshot {
    fn default() -> Self {
        Self {
            mode: "Balanced".into(),
            capture_to_encode_avg_ms: 0.0,
            encode_duration_avg_ms: 0.0,
            total_latency_avg_ms: 0.0,
            frames_encoded: 0,
            frames_skipped: 0,
            batches_encoded: 0,
        }
    }
}

/// EGFX pipeline state
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EgfxSnapshot {
    /// Whether the EGFX channel is ready
    pub channel_ready: bool,
    /// Negotiated EGFX capability version (e.g., "V10.5")
    pub negotiated_version: Option<String>,
    /// Whether AVC420 (H.264) is negotiated
    pub avc420_enabled: bool,
    /// Whether AVC444 (H.264 4:4:4) is negotiated
    pub avc444_enabled: bool,
    /// Current frame acknowledgement queue depth
    pub queue_depth: u32,
    /// Total frame acknowledgements received
    pub frame_acks: u64,
    /// Most recent client decode+render time (microseconds, from QoE)
    pub client_decode_render_us: Option<u64>,
}

/// Hardware encoder state
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EncoderSnapshot {
    /// Backend name ("vaapi", "nvenc", "openh264")
    pub backend: String,
    /// Total frames encoded
    pub frames_encoded: u64,
    /// Total bytes of encoded output
    pub bytes_encoded: u64,
    /// Average encoding time per frame (ms)
    pub avg_encode_time_ms: f32,
    /// Current bitrate (kbps)
    pub bitrate_kbps: u32,
    /// Target bitrate (kbps)
    pub target_bitrate_kbps: u32,
    /// Current FPS
    pub fps: f32,
    /// Keyframes encoded
    pub keyframes_encoded: u64,
    /// Frames skipped by rate control
    pub frames_skipped: u64,
    /// GPU utilization (0-100), if available
    pub gpu_utilization: Option<f32>,
}

impl Default for EncoderSnapshot {
    fn default() -> Self {
        Self {
            backend: "openh264".into(),
            frames_encoded: 0,
            bytes_encoded: 0,
            avg_encode_time_ms: 0.0,
            bitrate_kbps: 0,
            target_bitrate_kbps: 5000,
            fps: 0.0,
            keyframes_encoded: 0,
            frames_skipped: 0,
            gpu_utilization: None,
        }
    }
}

/// Aggregated raw metrics from MetricsCollector
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MetricsSnapshot {
    /// Active RDP connections
    pub active_connections: u64,
    /// Total bytes sent
    pub bytes_sent: u64,
    /// Total bytes received
    pub bytes_received: u64,
    /// Total frames received from PipeWire
    pub frames_received: u64,
    /// Total frames dropped
    pub frames_dropped: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn performance_snapshot_is_serializable() {
        let snapshot = PerformanceSnapshot {
            timestamp: SystemTime::now(),
            uptime: Duration::from_hours(1),
            fps: FpsSnapshot::default(),
            latency: LatencySnapshot::default(),
            egfx: EgfxSnapshot::default(),
            encoder: None,
            metrics: MetricsSnapshot::default(),
            sensor_snapshots: Vec::new(),
        };

        let json = serde_json::to_string(&snapshot).expect("should serialize");
        assert!(json.contains("\"uptime\""));
        assert!(json.contains("\"fps\""));
        assert!(json.contains("\"egfx\""));
    }

    #[test]
    fn performance_snapshot_clone_is_independent() {
        let snapshot = PerformanceSnapshot {
            timestamp: SystemTime::now(),
            uptime: Duration::from_mins(1),
            fps: FpsSnapshot {
                current_fps: 25,
                ..FpsSnapshot::default()
            },
            latency: LatencySnapshot::default(),
            egfx: EgfxSnapshot {
                channel_ready: true,
                queue_depth: 2,
                ..EgfxSnapshot::default()
            },
            encoder: Some(EncoderSnapshot {
                backend: "vaapi".into(),
                fps: 29.5,
                ..EncoderSnapshot::default()
            }),
            metrics: MetricsSnapshot::default(),
            sensor_snapshots: Vec::new(),
        };

        let cloned = snapshot.clone();
        assert_eq!(cloned.fps.current_fps, 25);
        assert!(cloned.egfx.channel_ready);
        assert_eq!(cloned.encoder.as_ref().unwrap().backend, "vaapi");
    }
}
