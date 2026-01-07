//! Server telemetry, diagnostics, and metrics
//!
//! Provides system diagnostics, performance metrics collection,
//! and user-facing error formatting.

pub mod diagnostics;
pub mod errors;
pub mod metrics;

// Re-export key types
pub use diagnostics::{
    detect_compositor, detect_portal_backend, get_pipewire_version, log_startup_diagnostics,
    RuntimeStats, SystemInfo,
};
pub use errors::format_user_error;
pub use metrics::{metric_names, HistogramStats, MetricsCollector, MetricsSnapshot, Timer};
