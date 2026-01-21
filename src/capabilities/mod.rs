//! Unified capability detection and fallback system
//!
//! This module provides comprehensive capability detection across all subsystems
//! with consistent fallback behavior and diagnostic reporting.
//!
//! # Overview
//!
//! The capability system detects available hardware and software resources at
//! startup, determines the best approach for each subsystem, and provides
//! graceful fallbacks when optimal methods aren't available.
//!
//! # Subsystems
//!
//! - **Display**: Screen capture via Portal/Wayland
//! - **Encoding**: Video encoding (VA-API, NVENC, OpenH264 software)
//! - **Input**: Input injection (Mutter API, wlr-direct, libei, Portal)
//! - **Storage**: Credential storage (Secret Service, TPM, encrypted file)
//! - **Rendering**: GUI rendering (GPU, software)
//! - **Network**: TLS and networking capabilities
//!
//! # Usage
//!
//! ```ignore
//! use lamco_rdp_server::capabilities::CapabilityManager;
//!
//! // Initialize at startup
//! CapabilityManager::initialize().await?;
//!
//! // Check capabilities
//! let mgr = CapabilityManager::global();
//! let state = mgr.read().await;
//!
//! if state.state.minimum_viable.can_operate_server() {
//!     // Start server
//! }
//!
//! if state.state.minimum_viable.can_render_gui() {
//!     // Start GUI
//! }
//! ```

mod manager;
mod state;
mod fallback;
mod diagnostics;
pub mod probes;

pub use manager::CapabilityManager;
pub use state::{
    SystemCapabilities,
    ServiceLevel,
    Subsystem,
    Degradation,
    BlockingIssue,
    MinimumViableConfig,
    UserImpact,
};
pub use fallback::{FallbackChain, FallbackStrategy, AttemptResult, StrategyProbe, ProbeError, InstantiationError, AllStrategiesFailed};
pub use diagnostics::{DiagnosticReport, run_diagnostics};

// Re-export probe results
pub use probes::{
    DisplayCapabilities,
    EncodingCapabilities,
    InputCapabilities,
    StorageCapabilities,
    RenderingCapabilities,
    NetworkCapabilities,
    RenderingRecommendation,
};
