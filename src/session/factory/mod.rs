//! Session Factory Module
//!
//! Provides a unified interface for session creation that:
//! - Handles initialization quirks per deployment context
//! - Manages retry logic with proper resource reuse
//! - Supports all current and planned session strategies
//!
//! # Design
//!
//! The Session Factory pattern separates:
//! - **WHAT is available** → Service Registry
//! - **HOW to initialize** → Session Factory + InitQuirks
//!
//! This prevents the "fix one thing, break another" pattern where
//! deployment-specific initialization logic is scattered across
//! strategy implementations.
//!
//! # Architecture
//!
//! ```text
//! ServiceRegistry (WHAT)           SessionFactory (HOW)
//!   - Clipboard: Guaranteed    →   - SingleClipboardProxy quirk
//!   - Auth: Unavailable        →   - PamBlockedBySandbox quirk
//!   - Persistence: BestEffort  →   - GnomePersistenceRejected quirk
//! ```
//!
//! See: docs/analysis/SESSION-FACTORY-PLAN-20260128.md

pub mod portal;
pub mod quirks;
pub mod state;

use std::sync::Arc;

use async_trait::async_trait;

use crate::session::strategy::SessionHandle;
use crate::session::DeploymentContext;

pub use portal::PortalSessionFactory;
pub use quirks::{InitQuirk, InitQuirkRegistry, SessionStrategyType};
pub use state::{SessionCreationError, SessionCreationFailure, SessionCreationState};

/// Capabilities provided by a session factory
#[derive(Debug, Clone)]
pub struct SessionFactoryCapabilities {
    /// Can the factory provide clipboard support?
    pub supports_clipboard: bool,

    /// Can sessions persist across restarts (restore tokens)?
    pub supports_persistence: bool,

    /// Can the factory inject keyboard/mouse input?
    pub supports_input_injection: bool,

    /// Can the factory capture multiple monitors?
    pub supports_multi_monitor: bool,

    /// Does session creation require user interaction (permission dialog)?
    pub requires_permission_dialog: bool,

    /// Strategy type for this factory
    pub strategy_type: SessionStrategyType,
}

/// Session Factory trait
///
/// Provides a unified interface for creating sessions across different
/// deployment contexts and strategies.
#[async_trait]
pub trait SessionFactory: Send + Sync {
    /// Create a session with full retry and quirk handling
    ///
    /// The factory handles:
    /// - Resource allocation (PortalManager, ClipboardManager)
    /// - First attempt with persistence (if supported)
    /// - Retry without persistence (if rejected)
    /// - Clipboard manager reuse (if quirk applies)
    /// - Proper cleanup on failure
    async fn create_session(&self) -> anyhow::Result<Arc<dyn SessionHandle>>;

    fn capabilities(&self) -> SessionFactoryCapabilities;

    fn deployment_context(&self) -> DeploymentContext;

    fn quirks(&self) -> &[InitQuirk];

    fn name(&self) -> &'static str;
}

/// Select the appropriate session factory for the given context
///
/// This replaces the previous SessionStrategySelector logic with
/// factory-based selection.
pub fn select_session_factory(
    registry: &Arc<crate::services::ServiceRegistry>,
    token_manager: Arc<crate::session::Tokens>,
) -> Arc<dyn SessionFactory> {
    // Currently all deployment contexts use Portal-based factory
    // Future: Add WlrSessionFactory for native wlroots without portal
    Arc::new(PortalSessionFactory::new(
        Arc::clone(registry),
        token_manager,
    ))
}
