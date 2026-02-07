//! System diagnostics, performance metrics, and user-friendly error formatting.

pub mod diagnostics;
pub mod errors;
pub mod metrics;

pub use diagnostics::{
    detect_compositor, detect_portal_backend, get_pipewire_version, log_startup_diagnostics,
    RuntimeStats, SystemInfo,
};
pub use errors::format_user_error;
pub use metrics::{metric_names, HistogramStats, MetricsCollector, MetricsSnapshot, Timer};
