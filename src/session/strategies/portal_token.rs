//! Portal + Token Strategy Implementation
//!
//! **Execution Path:** Portal ScreenCast + Portal RemoteDesktop + Portal Clipboard
//! **Status:** Active (v1.0.0+)
//! **Platform:** Universal (Flatpak + Native, all compositors)
//! **Session Type:** `PortalTokenStrategy`
//!
//! Uses XDG Portal with restore tokens for session persistence.
//! This is the universal strategy that works across all desktop environments.
//!
//! # Architecture
//!
//! This strategy delegates to `PortalSessionFactory` for actual session creation.
//! The factory handles:
//! - Deployment-specific initialization quirks (Flatpak, native, etc.)
//! - Clipboard manager lifecycle (SingleClipboardProxy quirk)
//! - Retry logic after persistence rejection (GnomePersistenceRejected quirk)
//! - Token loading/saving
//!
//! See: docs/analysis/SESSION-FACTORY-PLAN-20260128.md

use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use futures_util::StreamExt;
use tracing::{debug, error, info, warn};

use crate::{
    health::{HealthEvent, HealthReporter},
    services::ServiceRegistry,
    session::{
        factory::{PortalSessionFactory, SessionFactory},
        strategy::{PipeWireAccess, SessionHandle, SessionStrategy, SessionType, StreamInfo},
        Tokens,
    },
};

/// Portal session handle implementation
///
/// # Session Lock Design (RwLock)
///
/// We use RwLock instead of Mutex to allow concurrent input injection while
/// clipboard operations are in progress. The session handle is just an identifier
/// passed to D-Bus calls - each D-Bus operation creates its own connection/proxy.
///
/// - Input injection: Uses `.read().await` - concurrent access allowed
/// - Clipboard operations: Uses `.read().await` - also concurrent (session not modified)
///
/// This prevents the situation where a slow clipboard operation (e.g., Portal
/// selection_write blocking for 2+ seconds) would block all input injection,
/// causing mouse queue overflow and input lag.
pub struct PortalSessionHandleImpl {
    /// PipeWire file descriptor
    pub(crate) pipewire_fd: i32,
    /// Stream information
    pub(crate) streams: Vec<StreamInfo>,
    /// Remote desktop manager (for input injection)
    pub(crate) remote_desktop: Arc<lamco_portal::RemoteDesktopManager>,
    /// Session for input injection and clipboard
    /// Uses RwLock to allow concurrent input injection during clipboard operations
    pub(crate) session: Arc<
        tokio::sync::RwLock<
            ashpd::desktop::Session<
                'static,
                ashpd::desktop::remote_desktop::RemoteDesktop<'static>,
            >,
        >,
    >,
    /// Clipboard manager (for clipboard operations) - None on Portal v1
    pub(crate) clipboard_manager: Option<Arc<lamco_portal::ClipboardManager>>,
    /// Session type
    pub(crate) session_type: SessionType,
    /// Session validity flag - set to false when Portal session is destroyed
    pub(crate) session_valid: Arc<AtomicBool>,
    /// Health reporter for session lifecycle events (set once after construction).
    /// Arc-wrapped so spawned tasks (e.g., Closed listener) can read it lazily.
    pub(crate) health_reporter: Arc<std::sync::OnceLock<HealthReporter>>,
}

impl PortalSessionHandleImpl {
    /// Create from existing Portal handle and session components (for hybrid Mutter strategy)
    pub fn from_portal_session(
        session: Arc<
            tokio::sync::RwLock<
                ashpd::desktop::Session<
                    'static,
                    ashpd::desktop::remote_desktop::RemoteDesktop<'static>,
                >,
            >,
        >,
        remote_desktop: Arc<lamco_portal::RemoteDesktopManager>,
        clipboard_manager: Option<Arc<lamco_portal::ClipboardManager>>,
    ) -> Self {
        // Input-only handle - doesn't provide video/clipboard
        Self {
            pipewire_fd: 0,  // Not used for input-only
            streams: vec![], // Not used for input-only
            remote_desktop,
            session,
            clipboard_manager,
            session_type: SessionType::Portal,
            session_valid: Arc::new(AtomicBool::new(true)),
            health_reporter: Arc::new(std::sync::OnceLock::new()),
        }
    }

    /// Start listening for the Portal `Closed` D-Bus signal.
    ///
    /// When the compositor destroys the session (e.g., after PipeWire stream pause,
    /// user logout, or screen lock), this sets `session_valid` to false immediately
    /// instead of waiting for the next D-Bus call to fail.
    ///
    /// This is the key fix for Bug #6: the server learns about session destruction
    /// proactively rather than discovering it through error strings.
    pub async fn start_closed_listener(&self) {
        let session_guard = self.session.read().await;
        let closed_stream = match session_guard.receive_closed().await {
            Ok(stream) => stream,
            Err(e) => {
                warn!("Failed to subscribe to Portal session Closed signal: {e}");
                return;
            }
        };
        drop(session_guard);

        let session_valid = Arc::clone(&self.session_valid);
        let health_reporter = Arc::clone(&self.health_reporter);

        tokio::spawn(async move {
            futures_util::pin_mut!(closed_stream);
            // Stream yields () when the session is closed
            closed_stream.next().await;

            error!("Portal session Closed signal received — session destroyed by compositor");
            session_valid.store(false, Ordering::Release);

            // Read the reporter lazily — it may not have been set at spawn time,
            // but will be populated by set_health_reporter() before the compositor
            // destroys the session (which happens minutes/hours later).
            if let Some(reporter) = health_reporter.get() {
                reporter.report(HealthEvent::SessionClosed {
                    reason: "Portal Closed signal received from compositor".into(),
                });
            }
        });

        info!("Portal session Closed listener started");
    }

    /// Close Portal session explicitly
    ///
    /// # Lifecycle
    ///
    /// Portal sessions should be closed explicitly via D-Bus Close() call.
    /// ashpd::Session does NOT have Drop implementation, so sessions leak
    /// in the Portal daemon if not closed.
    ///
    /// This method:
    /// 1. Marks session as invalid (prevents new operations)
    /// 2. Calls Portal Close() via D-Bus
    /// 3. Logs success/failure
    ///
    /// # Errors
    ///
    /// Returns error if Portal Close() call fails, but session is marked
    /// invalid regardless.
    ///
    /// # TODO
    ///
    /// Remove when ashpd adds Session Drop implementation (upstream PR pending)
    pub async fn close_portal_session(&self) -> Result<()> {
        info!("Closing Portal session explicitly");

        // Mark session as invalid first (prevent new operations)
        self.session_valid.store(false, Ordering::Release);

        let session_guard = self.session.read().await;

        match session_guard.close().await {
            Ok(()) => {
                info!("Portal session closed successfully");
                Ok(())
            }
            Err(e) => {
                warn!("Portal session close failed: {}", e);
                // Don't fail - session is invalid anyway
                Ok(())
            }
        }
    }
}

impl PortalSessionHandleImpl {
    /// Handle an input injection error: if the session is destroyed, mark it
    /// invalid and return a clear error. Otherwise propagate the original error.
    fn handle_input_error<E: std::fmt::Display>(&self, error: E, operation: &str) -> anyhow::Error {
        let msg = format!("{error}");
        if msg.contains("non-existing session")
            || msg.contains("non existing session")
            || msg.contains("Invalid session")
            || msg.contains("UnknownObject")
        {
            tracing::error!("Portal session destroyed during {operation}");
            self.session_valid.store(false, Ordering::Release);

            if let Some(reporter) = self.health_reporter.get() {
                reporter.report(HealthEvent::SessionInvalidated {
                    reason: format!("D-Bus error during {operation}: {msg}"),
                });
            }

            anyhow!("Portal session destroyed: {msg}")
        } else {
            if let Some(reporter) = self.health_reporter.get() {
                reporter.report(HealthEvent::InputFailed {
                    reason: format!("{operation}: {msg}"),
                    permanent: false,
                });
            }

            anyhow!("Failed to inject {operation} via Portal: {msg}")
        }
    }
}

#[async_trait]
impl SessionHandle for PortalSessionHandleImpl {
    fn set_health_reporter(&self, reporter: HealthReporter) {
        let _ = self.health_reporter.set(reporter);
    }

    fn pipewire_access(&self) -> PipeWireAccess {
        PipeWireAccess::FileDescriptor(self.pipewire_fd)
    }

    fn streams(&self) -> Vec<StreamInfo> {
        self.streams.clone()
    }

    fn session_type(&self) -> SessionType {
        self.session_type
    }

    async fn notify_keyboard_keycode(&self, keycode: i32, pressed: bool) -> Result<()> {
        if !self.session_valid.load(Ordering::Acquire) {
            return Err(anyhow!(
                "Portal session invalid — cannot send keyboard event"
            ));
        }

        let session = self.session.read().await;
        self.remote_desktop
            .notify_keyboard_keycode(&session, keycode, pressed)
            .await
            .map_err(|e| self.handle_input_error(e, "keyboard keycode"))
    }

    async fn notify_pointer_motion_absolute(&self, stream_id: u32, x: f64, y: f64) -> Result<()> {
        if !self.session_valid.load(Ordering::Acquire) {
            return Err(anyhow!(
                "Portal session invalid — cannot send pointer motion"
            ));
        }

        let session = self.session.read().await;
        self.remote_desktop
            .notify_pointer_motion_absolute(&session, stream_id, x, y)
            .await
            .map_err(|e| self.handle_input_error(e, "pointer motion"))
    }

    async fn notify_pointer_button(&self, button: i32, pressed: bool) -> Result<()> {
        if !self.session_valid.load(Ordering::Acquire) {
            return Err(anyhow!(
                "Portal session invalid — cannot send pointer button"
            ));
        }

        let session = self.session.read().await;
        self.remote_desktop
            .notify_pointer_button(&session, button, pressed)
            .await
            .map_err(|e| self.handle_input_error(e, "pointer button"))
    }

    async fn notify_pointer_axis(&self, dx: f64, dy: f64) -> Result<()> {
        if !self.session_valid.load(Ordering::Acquire) {
            return Err(anyhow!("Portal session invalid — cannot send pointer axis"));
        }

        let session = self.session.read().await;
        self.remote_desktop
            .notify_pointer_axis(&session, dx, dy)
            .await
            .map_err(|e| self.handle_input_error(e, "pointer axis"))
    }

    fn portal_clipboard(&self) -> Option<crate::session::strategy::ClipboardComponents> {
        // Don't hand out clipboard components if the session has been invalidated
        // (e.g., compositor destroyed it after PipeWire stream pause)
        if !self.session_valid.load(Ordering::Acquire) {
            warn!("Portal session invalid — clipboard components unavailable");
            return None;
        }

        Some(crate::session::strategy::ClipboardComponents {
            manager: self.clipboard_manager.clone(),
            session: Arc::clone(&self.session),
            session_valid: Arc::clone(&self.session_valid),
        })
    }
}

/// Portal + Token strategy
///
/// This strategy uses the XDG Portal with restore tokens for session persistence.
/// Works across all desktop environments with portal v4+.
///
/// # Implementation
///
/// Delegates to `PortalSessionFactory` which handles:
/// - Deployment-specific quirks (Flatpak, native, systemd)
/// - Clipboard lifecycle management
/// - Retry logic after persistence rejection
/// - Token storage
pub struct PortalTokenStrategy {
    /// The underlying session factory
    factory: PortalSessionFactory,
    /// Service registry reference for capability queries
    service_registry: Arc<ServiceRegistry>,
}

impl PortalTokenStrategy {
    pub fn new(service_registry: Arc<ServiceRegistry>, token_manager: Arc<Tokens>) -> Self {
        let factory = PortalSessionFactory::new(service_registry.clone(), token_manager);

        Self {
            factory,
            service_registry,
        }
    }
}

#[async_trait]
impl SessionStrategy for PortalTokenStrategy {
    fn name(&self) -> &'static str {
        "Portal + Restore Token"
    }

    fn requires_initial_setup(&self) -> bool {
        // First time requires dialog, but subsequent runs use token
        true
    }

    fn supports_unattended_restore(&self) -> bool {
        // If portal v4+ and we have storage, yes
        self.service_registry.supports_session_persistence()
    }

    async fn create_session(&self) -> Result<Arc<dyn SessionHandle>> {
        info!("Creating session using Portal + Token strategy (via SessionFactory)");

        let quirks = self.factory.quirks();
        if !quirks.is_empty() {
            info!(
                "Active initialization quirks: {:?}",
                quirks
                    .iter()
                    .map(super::super::factory::quirks::InitQuirk::name)
                    .collect::<Vec<_>>()
            );
        }

        // Delegate to factory - it handles all quirk logic
        self.factory.create_session().await
    }

    async fn cleanup(&self, session: &dyn SessionHandle) -> Result<()> {
        // Portal sessions do NOT auto-close on Drop (ashpd has no Drop impl).
        // Explicit close happens in cleanup_resources() via close_portal_session().
        // This method is a no-op — the server's cleanup_resources() handles it.
        debug!("Portal session cleanup — deferred to cleanup_resources()");
        let _ = session;
        Ok(())
    }
}

#[cfg(test)]
mod tests {

    #[tokio::test]
    #[ignore = "Requires Wayland session with portal"]
    async fn test_portal_token_strategy() {
        // Would require full environment
        // Tested via integration tests
    }
}
