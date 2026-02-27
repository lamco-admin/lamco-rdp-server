//! Service Advertisement Registry
//!
//! This module bridges Wayland compositor capabilities to RDP clients through
//! a unified registry that translates detected features into RDP-compatible
//! service advertisements.
//!
//! # Architecture
//!
//! ```text
//! CompositorCapabilities ──> ServiceRegistry ──> AdvertisedServices
//!                                                      │
//!                                                      ▼
//!                                              RDP Capability Sets
//! ```
//!
//! # Service Levels
//!
//! Each service has a guarantee level:
//! - **Guaranteed**: Fully supported, tested, optimal path
//! - **BestEffort**: Works but may have limitations
//! - **Degraded**: Known issues, fallbacks in use
//! - **Unavailable**: Not supported on this compositor
//!
//! # Usage
//!
//! ```ignore
//! use lamco_rdp_server::compositor::probe_capabilities;
//! use lamco_rdp_server::services::ServiceRegistry;
//!
//! let caps = probe_capabilities().await?;
//! let registry = ServiceRegistry::from_compositor(caps);
//!
//! // Check service availability
//! if registry.service_level(ServiceId::DamageTracking) >= ServiceLevel::BestEffort {
//!     adaptive_fps.enable_activity_detection();
//! }
//!
//! // Log all services
//! registry.log_summary();
//! ```

pub mod clipboard_manager;
mod rdp_capabilities;
mod registry;
mod service;
mod translation;
mod wayland_features;

// Re-export main types
pub use clipboard_manager::{DetectedSystemClipboardManager, SystemClipboardManagerKind};
pub use rdp_capabilities::RdpCapability;
pub use registry::ServiceRegistry;
pub use service::{AdvertisedService, PerformanceHints, ServiceId, ServiceLevel};
pub use wayland_features::{
    DamageMethod, DataControlProtocol, DrmFormat, HdrTransfer, WaylandFeature,
};
