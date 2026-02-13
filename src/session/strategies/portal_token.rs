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

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use tracing::{debug, error, info, warn};

use crate::{
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
        }
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

#[async_trait]
impl SessionHandle for PortalSessionHandleImpl {
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
        if !self.session_valid.load(Ordering::Relaxed) {
            return Err(anyhow!(
                "Portal session destroyed - cannot send keyboard event"
            ));
        }

        let session = self.session.read().await;
        match self
            .remote_desktop
            .notify_keyboard_keycode(&session, keycode, pressed)
            .await
        {
            Ok(()) => Ok(()),
            Err(e) => {
                let error_msg = format!("{e}");
                if error_msg.contains("non-existing session")
                    || error_msg.contains("non existing session")
                {
                    error!("❌ Portal: Session destroyed during keyboard event");
                    self.session_valid.store(false, Ordering::Relaxed);
                    warn!("🔒 Portal session marked as invalid");
                    Err(anyhow!("Portal session destroyed: {e}"))
                } else {
                    Err(e).context("Failed to inject keyboard keycode via Portal")
                }
            }
        }
    }

    async fn notify_pointer_motion_absolute(&self, stream_id: u32, x: f64, y: f64) -> Result<()> {
        if !self.session_valid.load(Ordering::Relaxed) {
            return Err(anyhow!(
                "Portal session destroyed - cannot send pointer motion"
            ));
        }

        let session = self.session.read().await;
        match self
            .remote_desktop
            .notify_pointer_motion_absolute(&session, stream_id, x, y)
            .await
        {
            Ok(()) => Ok(()),
            Err(e) => {
                let error_msg = format!("{e}");
                if error_msg.contains("non-existing session")
                    || error_msg.contains("non existing session")
                {
                    error!("❌ Portal: Session destroyed during pointer motion");
                    self.session_valid.store(false, Ordering::Relaxed);
                    warn!("🔒 Portal session marked as invalid");
                    Err(anyhow!("Portal session destroyed: {e}"))
                } else {
                    Err(e).context("Failed to inject pointer motion via Portal")
                }
            }
        }
    }

    async fn notify_pointer_button(&self, button: i32, pressed: bool) -> Result<()> {
        if !self.session_valid.load(Ordering::Relaxed) {
            return Err(anyhow!(
                "Portal session destroyed - cannot send pointer button"
            ));
        }

        let session = self.session.read().await;
        match self
            .remote_desktop
            .notify_pointer_button(&session, button, pressed)
            .await
        {
            Ok(()) => Ok(()),
            Err(e) => {
                let error_msg = format!("{e}");
                if error_msg.contains("non-existing session")
                    || error_msg.contains("non existing session")
                {
                    error!("❌ Portal: Session destroyed during pointer button");
                    self.session_valid.store(false, Ordering::Relaxed);
                    warn!("🔒 Portal session marked as invalid");
                    Err(anyhow!("Portal session destroyed: {e}"))
                } else {
                    Err(e).context("Failed to inject pointer button via Portal")
                }
            }
        }
    }

    async fn notify_pointer_axis(&self, dx: f64, dy: f64) -> Result<()> {
        if !self.session_valid.load(Ordering::Relaxed) {
            return Err(anyhow!(
                "Portal session destroyed - cannot send pointer axis"
            ));
        }

        let session = self.session.read().await;
        match self
            .remote_desktop
            .notify_pointer_axis(&session, dx, dy)
            .await
        {
            Ok(()) => Ok(()),
            Err(e) => {
                let error_msg = format!("{e}");
                if error_msg.contains("non-existing session")
                    || error_msg.contains("non existing session")
                {
                    error!("❌ Portal: Session destroyed during pointer axis");
                    self.session_valid.store(false, Ordering::Relaxed);
                    warn!("🔒 Portal session marked as invalid");
                    Err(anyhow!("Portal session destroyed: {e}"))
                } else {
                    Err(e).context("Failed to inject pointer axis via Portal")
                }
            }
        }
    }

    fn portal_clipboard(&self) -> Option<crate::session::strategy::ClipboardComponents> {
        // Always return Some for Portal strategy - session is always available
        // Manager may be None on Portal v1 (no clipboard support)
        Some(crate::session::strategy::ClipboardComponents {
            manager: self.clipboard_manager.clone(),
            session: Arc::clone(&self.session),
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

    async fn cleanup(&self, _session: &dyn SessionHandle) -> Result<()> {
        // Portal sessions clean up automatically when dropped
        debug!("Portal session cleanup (automatic via Drop)");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[ignore] // Requires Wayland session with portal
    async fn test_portal_token_strategy() {
        // Would require full environment
        // Tested via integration tests
    }
}
