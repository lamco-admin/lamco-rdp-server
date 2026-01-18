//! Utility Functions and Diagnostics
//!
//! System diagnostics, performance metrics, and user-friendly error formatting.
//!
//! # Overview
//!
//! This module provides three key utilities for operational visibility and debugging:
//!
//! 1. **Diagnostics** - System information and capability detection
//! 2. **Metrics** - Performance monitoring and statistics collection
//! 3. **Error Formatting** - User-friendly error messages with troubleshooting hints
//!
//! ## Diagnostics
//!
//! The [`diagnostics`] module helps understand the runtime environment:
//!
//! ```rust
//! use lamco_rdp_server::utils::{SystemInfo, detect_compositor, get_pipewire_version};
//!
//! // Gather system information
//! let sys_info = SystemInfo::gather();
//! sys_info.log();  // Logs: OS, kernel, CPU count, memory
//!
//! // Detect compositor
//! let compositor = detect_compositor();
//! println!("Running on: {}", compositor);
//!
//! // Check PipeWire version
//! let version = get_pipewire_version().await?;
//! println!("PipeWire: {}", version);
//! ```
//!
//! **CLI access:**
//! ```bash
//! lamco-rdp-server --show-capabilities    # Show compositor and portal info
//! lamco-rdp-server --diagnose             # Full system diagnostics
//! lamco-rdp-server --persistence-status   # Check token availability
//! ```
//!
//! ## Metrics
//!
//! The [`metrics`] module tracks performance statistics:
//!
//! ```rust
//! use lamco_rdp_server::utils::MetricsCollector;
//!
//! let metrics = MetricsCollector::new();
//!
//! // Record measurements
//! metrics.record_frame_time(16.7);  // milliseconds
//! metrics.record_encode_time(8.2);
//!
//! // Get statistics
//! let snapshot = metrics.snapshot();
//! println!("Avg frame time: {:.2}ms", snapshot.avg_frame_time);
//! println!("FPS: {:.1}", snapshot.current_fps);
//! ```
//!
//! Metrics tracked:
//! - Frame processing time (ms)
//! - Encoding time (ms)
//! - Network latency (ms)
//! - Current FPS
//! - Bandwidth usage (Mbps)
//!
//! ## Error Formatting
//!
//! The [`errors`] module provides user-friendly error messages:
//!
//! ```rust
//! use lamco_rdp_server::utils::format_user_error;
//!
//! match operation() {
//!     Err(e) => {
//!         eprintln!("{}", format_user_error(&e));
//!         // Shows:
//!         // - Formatted error with box drawing
//!         // - Context-specific troubleshooting steps
//!         // - Common causes and solutions
//!         // - Technical details
//!     }
//! }
//! ```
//!
//! Error categories with context-aware help:
//! - Portal errors → Check xdg-desktop-portal status, backend installation
//! - PipeWire errors → Check PipeWire version, service status
//! - TLS errors → Certificate validation, file paths
//! - Network errors → Port conflicts, permission issues
//! - Config errors → Syntax validation, missing fields
//!
//! This makes troubleshooting accessible to users unfamiliar with Wayland/Portal internals.

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
