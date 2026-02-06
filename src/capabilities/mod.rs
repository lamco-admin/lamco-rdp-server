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

mod diagnostics;
mod fallback;
mod manager;
pub mod probes;
mod state;

pub use diagnostics::{run_diagnostics, DiagnosticReport};
pub use fallback::{
    AllStrategiesFailed, AttemptResult, FallbackChain, FallbackStrategy, InstantiationError,
    ProbeError, StrategyProbe,
};
pub use manager::CapabilityManager;
pub use state::{
    BlockingIssue, Degradation, MinimumViableConfig, ServiceLevel, Subsystem, SystemCapabilities,
    UserImpact,
};

// Re-export probe results
pub use probes::{
    DisplayCapabilities, EncodingCapabilities, InputCapabilities, NetworkCapabilities,
    RenderingCapabilities, RenderingRecommendation, StorageCapabilities,
};
