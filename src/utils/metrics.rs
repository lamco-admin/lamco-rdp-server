//! Performance Metrics and Monitoring
//!
//! Provides comprehensive metrics collection and reporting for all system components:
//! - Frame processing metrics
//! - Network throughput metrics
//! - Resource utilization metrics
//! - Latency tracking
//! - Error rate monitoring
//!
//! Metrics can be exported in various formats (JSON, Prometheus-compatible)
//! for integration with monitoring systems.

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

/// Metrics collector for the entire system
pub struct MetricsCollector {
    counters: Arc<RwLock<HashMap<String, u64>>>,
    gauges: Arc<RwLock<HashMap<String, f64>>>,
    histograms: Arc<RwLock<HashMap<String, Histogram>>>,
    start_time: Instant,
}

impl MetricsCollector {
    /// Create a new metrics collector
    pub fn new() -> Self {
        Self {
            counters: Arc::new(RwLock::new(HashMap::new())),
            gauges: Arc::new(RwLock::new(HashMap::new())),
            histograms: Arc::new(RwLock::new(HashMap::new())),
            start_time: Instant::now(),
        }
    }

    /// Increment a counter
    ///
    /// # Arguments
    /// * `name` - Counter name
    /// * `value` - Amount to increment by (default 1)
    pub fn increment_counter(&self, name: &str, value: u64) {
        let mut counters = self.counters.write();
        *counters.entry(name.to_string()).or_insert(0) += value;
    }

    /// Set a gauge value
    ///
    /// # Arguments
    /// * `name` - Gauge name
    /// * `value` - Current value
    pub fn set_gauge(&self, name: &str, value: f64) {
        let mut gauges = self.gauges.write();
        gauges.insert(name.to_string(), value);
    }

    /// Record a histogram value
    ///
    /// # Arguments
    /// * `name` - Histogram name
    /// * `value` - Value to record
    pub fn record_histogram(&self, name: &str, value: f64) {
        let mut histograms = self.histograms.write();
        histograms
            .entry(name.to_string())
            .or_insert_with(Histogram::new)
            .record(value);
    }

    /// Get a counter value
    pub fn get_counter(&self, name: &str) -> Option<u64> {
        self.counters.read().get(name).copied()
    }

    /// Get a gauge value
    pub fn get_gauge(&self, name: &str) -> Option<f64> {
        self.gauges.read().get(name).copied()
    }

    /// Get histogram statistics
    pub fn get_histogram(&self, name: &str) -> Option<HistogramStats> {
        self.histograms.read().get(name).map(|h| h.stats())
    }

    /// Get all metrics as a snapshot
    pub fn snapshot(&self) -> MetricsSnapshot {
        MetricsSnapshot {
            timestamp: SystemTime::now(),
            uptime: self.start_time.elapsed(),
            counters: self.counters.read().clone(),
            gauges: self.gauges.read().clone(),
            histograms: self
                .histograms
                .read()
                .iter()
                .map(|(k, v)| (k.clone(), v.stats()))
                .collect(),
        }
    }

    /// Reset all metrics
    pub fn reset(&self) {
        self.counters.write().clear();
        self.gauges.write().clear();
        self.histograms.write().clear();
    }

    /// Export metrics in Prometheus text format
    pub fn export_prometheus(&self) -> String {
        let mut output = String::new();

        // Export counters
        for (name, value) in self.counters.read().iter() {
            output.push_str(&format!("# TYPE {} counter\n", name));
            output.push_str(&format!("{} {}\n", name, value));
        }

        // Export gauges
        for (name, value) in self.gauges.read().iter() {
            output.push_str(&format!("# TYPE {} gauge\n", name));
            output.push_str(&format!("{} {}\n", name, value));
        }

        // Export histograms
        for (name, histogram) in self.histograms.read().iter() {
            let stats = histogram.stats();
            output.push_str(&format!("# TYPE {} histogram\n", name));
            output.push_str(&format!("{}_count {}\n", name, stats.count));
            output.push_str(&format!("{}_sum {}\n", name, stats.sum));
            output.push_str(&format!("{}_min {}\n", name, stats.min));
            output.push_str(&format!("{}_max {}\n", name, stats.max));
            output.push_str(&format!("{}_avg {}\n", name, stats.mean));
            output.push_str(&format!("{}_p50 {}\n", name, stats.p50));
            output.push_str(&format!("{}_p95 {}\n", name, stats.p95));
            output.push_str(&format!("{}_p99 {}\n", name, stats.p99));
        }

        output
    }

    /// Export metrics as JSON
    pub fn export_json(&self) -> serde_json::Result<String> {
        let snapshot = self.snapshot();
        serde_json::to_string_pretty(&snapshot)
    }
}

impl Default for MetricsCollector {
    fn default() -> Self {
        Self::new()
    }
}

/// Histogram for tracking value distributions
pub struct Histogram {
    values: Vec<f64>,
    min: f64,
    max: f64,
    sum: f64,
}

impl Histogram {
    fn new() -> Self {
        Self {
            values: Vec::new(),
            min: f64::MAX,
            max: f64::MIN,
            sum: 0.0,
        }
    }

    fn record(&mut self, value: f64) {
        self.values.push(value);
        self.min = self.min.min(value);
        self.max = self.max.max(value);
        self.sum += value;
    }

    fn stats(&self) -> HistogramStats {
        if self.values.is_empty() {
            return HistogramStats::default();
        }

        let count = self.values.len();
        let mean = self.sum / count as f64;

        // Calculate variance and stddev
        let variance = self
            .values
            .iter()
            .map(|v| {
                let diff = v - mean;
                diff * diff
            })
            .sum::<f64>()
            / count as f64;
        let stddev = variance.sqrt();

        // Calculate percentiles
        let mut sorted = self.values.clone();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());

        let p50 = percentile(&sorted, 0.50);
        let p95 = percentile(&sorted, 0.95);
        let p99 = percentile(&sorted, 0.99);

        HistogramStats {
            count: count as u64,
            sum: self.sum,
            min: self.min,
            max: self.max,
            mean,
            stddev,
            p50,
            p95,
            p99,
        }
    }
}

/// Calculate percentile from sorted values using the "inclusive" method
/// (equivalent to Excel's PERCENTILE.INC or NumPy's percentile with 'lower' interpolation)
fn percentile(sorted_values: &[f64], p: f64) -> f64 {
    if sorted_values.is_empty() {
        return 0.0;
    }

    // Use (n-1) * p formula for inclusive percentile calculation
    // This maps p=0 to first element and p=1 to last element
    let index = ((sorted_values.len() - 1) as f64 * p) as usize;
    let index = index.min(sorted_values.len() - 1);
    sorted_values[index]
}

/// Histogram statistics computed from recorded observations
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct HistogramStats {
    /// Total number of observations
    pub count: u64,
    /// Sum of all observations
    pub sum: f64,
    /// Minimum observed value
    pub min: f64,
    /// Maximum observed value
    pub max: f64,
    /// Arithmetic mean of observations
    pub mean: f64,
    /// Standard deviation of observations
    pub stddev: f64,
    /// 50th percentile (median)
    pub p50: f64,
    /// 95th percentile
    pub p95: f64,
    /// 99th percentile
    pub p99: f64,
}

impl Default for HistogramStats {
    fn default() -> Self {
        Self {
            count: 0,
            sum: 0.0,
            min: 0.0,
            max: 0.0,
            mean: 0.0,
            stddev: 0.0,
            p50: 0.0,
            p95: 0.0,
            p99: 0.0,
        }
    }
}

/// Point-in-time snapshot of all collected metrics
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetricsSnapshot {
    /// When this snapshot was taken
    pub timestamp: SystemTime,
    /// Server uptime at snapshot time
    pub uptime: Duration,
    /// Counter values (monotonically increasing)
    pub counters: HashMap<String, u64>,
    /// Gauge values (current state)
    pub gauges: HashMap<String, f64>,
    /// Histogram statistics
    pub histograms: HashMap<String, HistogramStats>,
}

pub mod metric_names {
    //! Pre-defined metric names for consistency across the codebase.
    //!
    //! Use these constants instead of string literals to ensure
    //! consistent metric naming across all components.

    /// Total video frames received from PipeWire
    pub const FRAMES_RECEIVED: &str = "frames_received_total";
    /// Total video frames successfully processed
    pub const FRAMES_PROCESSED: &str = "frames_processed_total";
    /// Total video frames dropped (queue full, timeout, etc.)
    pub const FRAMES_DROPPED: &str = "frames_dropped_total";
    /// Frame processing time histogram (milliseconds)
    pub const FRAME_PROCESSING_TIME_MS: &str = "frame_processing_time_ms";

    /// Total format conversions performed
    pub const CONVERSIONS_TOTAL: &str = "conversions_total";
    /// Conversion time histogram (milliseconds)
    pub const CONVERSION_TIME_MS: &str = "conversion_time_ms";
    /// Total bytes converted
    pub const CONVERSION_BYTES: &str = "conversion_bytes_total";

    /// Total frames dispatched to RDP clients
    pub const FRAMES_DISPATCHED: &str = "frames_dispatched_total";
    /// Current frames waiting in dispatch queue
    pub const FRAMES_QUEUED: &str = "frames_queued";
    /// Dispatch time histogram (microseconds)
    pub const DISPATCH_TIME_US: &str = "dispatch_time_us";

    /// Total bytes sent to RDP clients
    pub const BYTES_SENT: &str = "bytes_sent_total";
    /// Total bytes received from RDP clients
    pub const BYTES_RECEIVED: &str = "bytes_received_total";
    /// Total RDP packets sent
    pub const PACKETS_SENT: &str = "packets_sent_total";
    /// Total RDP packets received
    pub const PACKETS_RECEIVED: &str = "packets_received_total";
    /// Total network errors encountered
    pub const NETWORK_ERRORS: &str = "network_errors_total";

    /// Currently active RDP connections
    pub const CONNECTIONS_ACTIVE: &str = "connections_active";
    /// Total RDP connections since server start
    pub const CONNECTIONS_TOTAL: &str = "connections_total";
    /// Total connection errors (auth failures, protocol errors, etc.)
    pub const CONNECTION_ERRORS: &str = "connection_errors_total";

    /// Current CPU usage percentage
    pub const CPU_USAGE: &str = "cpu_usage_percent";
    /// Current memory usage in bytes
    pub const MEMORY_USAGE: &str = "memory_usage_bytes";
    /// Current memory usage as percentage of system total
    pub const MEMORY_USAGE_PERCENT: &str = "memory_usage_percent";

    /// Input event latency histogram (milliseconds)
    pub const INPUT_LATENCY_MS: &str = "input_latency_ms";
    /// Video encoding/transmission latency histogram (milliseconds)
    pub const VIDEO_LATENCY_MS: &str = "video_latency_ms";
    /// End-to-end latency histogram (milliseconds)
    pub const END_TO_END_LATENCY_MS: &str = "end_to_end_latency_ms";
}

/// Timer helper for measuring durations
pub struct Timer {
    start: Instant,
}

impl Timer {
    /// Start a new timer
    pub fn new() -> Self {
        Self {
            start: Instant::now(),
        }
    }

    /// Get elapsed time in milliseconds
    pub fn elapsed_ms(&self) -> f64 {
        self.start.elapsed().as_secs_f64() * 1000.0
    }

    /// Get elapsed time in microseconds
    pub fn elapsed_us(&self) -> f64 {
        self.start.elapsed().as_secs_f64() * 1_000_000.0
    }

    /// Get elapsed time in nanoseconds
    pub fn elapsed_ns(&self) -> u64 {
        self.start.elapsed().as_nanos() as u64
    }
}

impl Default for Timer {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_counter() {
        let metrics = MetricsCollector::new();

        metrics.increment_counter("test_counter", 1);
        assert_eq!(metrics.get_counter("test_counter"), Some(1));

        metrics.increment_counter("test_counter", 5);
        assert_eq!(metrics.get_counter("test_counter"), Some(6));
    }

    #[test]
    fn test_gauge() {
        let metrics = MetricsCollector::new();

        metrics.set_gauge("test_gauge", 42.5);
        assert_eq!(metrics.get_gauge("test_gauge"), Some(42.5));

        metrics.set_gauge("test_gauge", 100.0);
        assert_eq!(metrics.get_gauge("test_gauge"), Some(100.0));
    }

    #[test]
    fn test_histogram() {
        let metrics = MetricsCollector::new();

        metrics.record_histogram("test_histogram", 10.0);
        metrics.record_histogram("test_histogram", 20.0);
        metrics.record_histogram("test_histogram", 30.0);

        let stats = metrics.get_histogram("test_histogram").unwrap();
        assert_eq!(stats.count, 3);
        assert!((stats.min - 10.0).abs() < 0.01);
        assert!((stats.max - 30.0).abs() < 0.01);
        assert!((stats.mean - 20.0).abs() < 0.01);
    }

    #[test]
    fn test_snapshot() {
        let metrics = MetricsCollector::new();

        metrics.increment_counter("counter1", 10);
        metrics.set_gauge("gauge1", 42.0);
        metrics.record_histogram("hist1", 5.0);

        let snapshot = metrics.snapshot();
        assert_eq!(snapshot.counters.get("counter1"), Some(&10));
        assert_eq!(snapshot.gauges.get("gauge1"), Some(&42.0));
        assert!(snapshot.histograms.contains_key("hist1"));
    }

    #[test]
    fn test_reset() {
        let metrics = MetricsCollector::new();

        metrics.increment_counter("test", 1);
        metrics.set_gauge("test", 1.0);
        metrics.record_histogram("test", 1.0);

        metrics.reset();

        assert_eq!(metrics.get_counter("test"), None);
        assert_eq!(metrics.get_gauge("test"), None);
        assert_eq!(metrics.get_histogram("test"), None);
    }

    #[test]
    fn test_prometheus_export() {
        let metrics = MetricsCollector::new();

        metrics.increment_counter("test_counter", 42);
        metrics.set_gauge("test_gauge", 3.14);

        let output = metrics.export_prometheus();
        assert!(output.contains("test_counter 42"));
        assert!(output.contains("test_gauge 3.14"));
    }

    #[test]
    fn test_json_export() {
        let metrics = MetricsCollector::new();

        metrics.increment_counter("test", 1);
        let json = metrics.export_json().unwrap();
        assert!(json.contains("\"test\""));
    }

    #[test]
    fn test_timer() {
        let timer = Timer::new();
        std::thread::sleep(Duration::from_millis(10));

        let elapsed = timer.elapsed_ms();
        assert!(elapsed >= 10.0);
        assert!(elapsed < 50.0); // Allow some overhead
    }

    #[test]
    fn test_percentile() {
        let values = vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0, 10.0];

        assert_eq!(percentile(&values, 0.50), 5.0);
        assert_eq!(percentile(&values, 0.95), 9.0);
        assert_eq!(percentile(&values, 0.99), 9.0);
    }

    #[test]
    fn test_histogram_stats() {
        let mut hist = Histogram::new();

        hist.record(10.0);
        hist.record(20.0);
        hist.record(30.0);
        hist.record(40.0);
        hist.record(50.0);

        let stats = hist.stats();
        assert_eq!(stats.count, 5);
        assert_eq!(stats.sum, 150.0);
        assert_eq!(stats.min, 10.0);
        assert_eq!(stats.max, 50.0);
        assert_eq!(stats.mean, 30.0);
        assert_eq!(stats.p50, 30.0);
    }
}
