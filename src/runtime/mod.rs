//! System diagnostics, performance metrics, and user-friendly error formatting.

pub mod diagnostics;
pub mod errors;
pub mod metrics;

pub use diagnostics::{
    RuntimeStats, SystemInfo, detect_compositor, detect_portal_backend, get_pipewire_version,
    log_startup_diagnostics,
};
pub use errors::format_user_error;
pub use metrics::{HistogramStats, MetricsCollector, MetricsSnapshot, Timer, metric_names};
