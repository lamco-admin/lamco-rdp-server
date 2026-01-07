//! Unified statistics for hardware encoders
//!
//! This module provides a common statistics structure that all hardware
//! encoder backends populate, enabling consistent monitoring and logging
//! regardless of the underlying GPU acceleration technology.

use std::time::{Duration, Instant};

/// Unified statistics for hardware encoders
///
/// Provides insight into encoder performance across all backends.
/// Statistics are updated after each successful encode operation.
#[derive(Debug, Clone)]
pub struct HardwareEncoderStats {
    /// Backend identifier ("vaapi", "nvenc", etc.)
    pub backend: &'static str,

    /// Total frames successfully encoded
    pub frames_encoded: u64,

    /// Total bytes of encoded output
    pub bytes_encoded: u64,

    /// Average encoding time per frame in milliseconds
    pub avg_encode_time_ms: f32,

    /// Minimum encoding time observed (ms)
    pub min_encode_time_ms: f32,

    /// Maximum encoding time observed (ms)
    pub max_encode_time_ms: f32,

    /// Current bitrate based on recent output (kbps)
    pub bitrate_kbps: u32,

    /// Target bitrate from configuration (kbps)
    pub target_bitrate_kbps: u32,

    /// Number of keyframes (IDR) encoded
    pub keyframes_encoded: u64,

    /// Number of frames skipped by rate control
    pub frames_skipped: u64,

    /// GPU utilization percentage (0-100), if available
    pub gpu_utilization: Option<f32>,

    /// Video encoder unit utilization (0-100), if available
    /// (separate from 3D/compute GPU usage)
    pub encoder_utilization: Option<f32>,

    /// Time since encoder was created
    pub uptime: Duration,

    /// Timestamp when encoder was created
    pub created_at: Instant,
}

impl HardwareEncoderStats {
    /// Create new stats for a backend
    pub fn new(backend: &'static str, target_bitrate_kbps: u32) -> Self {
        Self {
            backend,
            frames_encoded: 0,
            bytes_encoded: 0,
            avg_encode_time_ms: 0.0,
            min_encode_time_ms: f32::MAX,
            max_encode_time_ms: 0.0,
            bitrate_kbps: 0,
            target_bitrate_kbps,
            keyframes_encoded: 0,
            frames_skipped: 0,
            gpu_utilization: None,
            encoder_utilization: None,
            uptime: Duration::ZERO,
            created_at: Instant::now(),
        }
    }

    /// Update stats after encoding a frame
    pub fn record_frame(&mut self, encode_time_ms: f32, bytes: usize, is_keyframe: bool) {
        self.frames_encoded += 1;
        self.bytes_encoded += bytes as u64;

        // Update timing stats
        if self.frames_encoded == 1 {
            self.avg_encode_time_ms = encode_time_ms;
        } else {
            // Exponential moving average (α = 0.1)
            self.avg_encode_time_ms = self.avg_encode_time_ms * 0.9 + encode_time_ms * 0.1;
        }

        self.min_encode_time_ms = self.min_encode_time_ms.min(encode_time_ms);
        self.max_encode_time_ms = self.max_encode_time_ms.max(encode_time_ms);

        if is_keyframe {
            self.keyframes_encoded += 1;
        }

        // Update uptime
        self.uptime = self.created_at.elapsed();

        // Estimate current bitrate (based on last second of data)
        self.update_bitrate_estimate();
    }

    /// Record a skipped frame
    pub fn record_skip(&mut self) {
        self.frames_skipped += 1;
        self.uptime = self.created_at.elapsed();
    }

    /// Update bitrate estimate based on total bytes and time
    fn update_bitrate_estimate(&mut self) {
        let elapsed_secs = self.uptime.as_secs_f32();
        if elapsed_secs > 0.5 {
            // bits per second / 1000 = kbps
            self.bitrate_kbps = ((self.bytes_encoded * 8) as f32 / elapsed_secs / 1000.0) as u32;
        }
    }

    /// Update GPU utilization (called by backend if available)
    pub fn set_gpu_utilization(&mut self, utilization: f32) {
        self.gpu_utilization = Some(utilization.clamp(0.0, 100.0));
    }

    /// Update encoder-specific utilization (called by backend if available)
    pub fn set_encoder_utilization(&mut self, utilization: f32) {
        self.encoder_utilization = Some(utilization.clamp(0.0, 100.0));
    }

    /// Get frames per second based on total time
    pub fn fps(&self) -> f32 {
        let elapsed_secs = self.uptime.as_secs_f32();
        if elapsed_secs > 0.0 {
            self.frames_encoded as f32 / elapsed_secs
        } else {
            0.0
        }
    }

    /// Get keyframe percentage
    pub fn keyframe_percentage(&self) -> f32 {
        if self.frames_encoded > 0 {
            (self.keyframes_encoded as f32 / self.frames_encoded as f32) * 100.0
        } else {
            0.0
        }
    }

    /// Get skip percentage
    pub fn skip_percentage(&self) -> f32 {
        let total = self.frames_encoded + self.frames_skipped;
        if total > 0 {
            (self.frames_skipped as f32 / total as f32) * 100.0
        } else {
            0.0
        }
    }

    /// Format stats for logging
    pub fn summary(&self) -> String {
        format!(
            "{}: {} frames, {:.1} fps, {} kbps (target {}), avg {:.2}ms/frame",
            self.backend,
            self.frames_encoded,
            self.fps(),
            self.bitrate_kbps,
            self.target_bitrate_kbps,
            self.avg_encode_time_ms
        )
    }
}

impl Default for HardwareEncoderStats {
    fn default() -> Self {
        Self::new("unknown", 5000)
    }
}

/// Timing helper for measuring encode operations
pub struct EncodeTimer {
    start: Instant,
}

impl EncodeTimer {
    /// Start timing an encode operation
    pub fn start() -> Self {
        Self {
            start: Instant::now(),
        }
    }

    /// Get elapsed time in milliseconds
    pub fn elapsed_ms(&self) -> f32 {
        self.start.elapsed().as_secs_f32() * 1000.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_stats_new() {
        let stats = HardwareEncoderStats::new("vaapi", 5000);
        assert_eq!(stats.backend, "vaapi");
        assert_eq!(stats.frames_encoded, 0);
        assert_eq!(stats.target_bitrate_kbps, 5000);
    }

    #[test]
    fn test_stats_record_frame() {
        let mut stats = HardwareEncoderStats::new("nvenc", 5000);

        stats.record_frame(2.5, 10000, true);
        assert_eq!(stats.frames_encoded, 1);
        assert_eq!(stats.keyframes_encoded, 1);
        assert_eq!(stats.bytes_encoded, 10000);
        assert!((stats.avg_encode_time_ms - 2.5).abs() < 0.01);

        stats.record_frame(3.0, 5000, false);
        assert_eq!(stats.frames_encoded, 2);
        assert_eq!(stats.keyframes_encoded, 1);
        assert_eq!(stats.bytes_encoded, 15000);
    }

    #[test]
    fn test_stats_record_skip() {
        let mut stats = HardwareEncoderStats::new("vaapi", 5000);
        stats.record_frame(2.0, 5000, false);
        stats.record_skip();
        stats.record_skip();

        assert_eq!(stats.frames_skipped, 2);
        assert!((stats.skip_percentage() - 66.666).abs() < 1.0);
    }

    #[test]
    fn test_stats_summary() {
        let stats = HardwareEncoderStats::new("nvenc", 8000);
        let summary = stats.summary();
        assert!(summary.contains("nvenc"));
        assert!(summary.contains("8000"));
    }

    #[test]
    fn test_encode_timer() {
        let timer = EncodeTimer::start();
        std::thread::sleep(std::time::Duration::from_millis(10));
        let elapsed = timer.elapsed_ms();
        assert!(elapsed >= 9.0); // Allow some tolerance
    }
}
