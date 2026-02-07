//! Portal-based Session Factory
//!
//! Implements SessionFactory using XDG Portals for session creation.
//! Handles all quirks related to Portal-based session strategies.

use std::sync::Arc;

use anyhow::{Context, Result};
use async_trait::async_trait;
use tracing::{debug, info, trace, warn};

use crate::portal::PortalManager;
use crate::services::{ServiceId, ServiceRegistry};
use crate::session::strategies::PortalSessionHandleImpl;
use crate::session::strategy::{SessionHandle, SessionType, StreamInfo};
use crate::session::{DeploymentContext, Tokens};

use super::quirks::{InitQuirk, InitQuirkRegistry, SessionStrategyType};
use super::state::{SessionCreationError, SessionCreationFailure};
use super::{SessionFactory, SessionFactoryCapabilities};

/// Portal-based session factory
///
/// Creates sessions using XDG Portals with proper quirk handling for
/// different deployment contexts (Flatpak, native, systemd).
pub struct PortalSessionFactory {
    /// Service registry for capability queries
    registry: Arc<ServiceRegistry>,

    /// Token manager for persistence
    token_manager: Arc<Tokens>,

    /// Quirks that apply to this deployment/compositor combination
    quirks: Vec<InitQuirk>,

    /// Maximum retry attempts
    max_retries: u32,
}

impl PortalSessionFactory {
    pub fn new(registry: Arc<ServiceRegistry>, token_manager: Arc<Tokens>) -> Self {
        let caps = registry.compositor_capabilities();
        let quirks = InitQuirkRegistry::quirks_for(
            caps.deployment,
            &caps.compositor,
            SessionStrategyType::PortalToken,
        );

        info!(
            "PortalSessionFactory created with {} quirks: {:?}",
            quirks.len(),
            quirks.iter().map(|q| q.name()).collect::<Vec<_>>()
        );

        Self {
            registry,
            token_manager,
            quirks,
            max_retries: 2,
        }
    }

    fn expects_persistence_rejection(&self) -> bool {
        self.quirks.contains(&InitQuirk::GnomePersistenceRejected)
    }

    /// Attempt to create a session with the given configuration
    ///
    /// Creates a fresh ClipboardManager for each attempt to ensure it shares
    /// the same D-Bus context as the PortalManager. This is critical because
    /// the Portal backend (especially in Flatpak) requires the clipboard's
    /// D-Bus proxy to be associated with the current session's context.
    async fn attempt_session(
        &self,
        with_persistence: bool,
    ) -> Result<(
        lamco_portal::PortalSessionHandle,
        Option<String>,
        Arc<PortalManager>,
        Option<Arc<lamco_portal::ClipboardManager>>,
    )> {
        let attempt_type = if with_persistence {
            "first (with persistence)"
        } else {
            "retry (no persistence)"
        };
        info!(
            attempt = attempt_type,
            deployment = ?self.registry.compositor_capabilities().deployment,
            "Starting session attempt"
        );

        let restore_token = if with_persistence {
            self.token_manager
                .load_token("default")
                .await
                .context("Failed to load restore token")?
        } else {
            None
        };

        if let Some(ref token) = restore_token {
            info!("Using restore token ({} chars)", token.len());
        } else if with_persistence {
            info!("No restore token, permission dialog will appear");
        }

        let mut portal_config = lamco_portal::PortalConfig::default();
        portal_config.restore_token = restore_token;

        if !with_persistence {
            portal_config.persist_mode = ashpd::desktop::PersistMode::DoNot;
        }

        debug!(
            persist_mode = ?portal_config.persist_mode,
            has_token = portal_config.restore_token.is_some(),
            "Portal configuration"
        );

        trace!("Creating PortalManager (new D-Bus connection)");
        let portal_manager = Arc::new(
            PortalManager::new(portal_config)
                .await
                .context("Failed to create Portal manager")?,
        );
        trace!("PortalManager created successfully");

        // CRITICAL: Check service registry FIRST to enforce clipboard availability decisions
        //
        // The service registry translates compositor quirks (like KdePortalClipboardUnstable)
        // into service levels. If clipboard is marked Unavailable, we MUST NOT create a
        // ClipboardManager - doing so would bypass the quirk system entirely.
        let clipboard_enabled = self
            .registry
            .service_level(ServiceId::Clipboard)
            .is_usable();

        if !clipboard_enabled {
            info!("Clipboard disabled by service registry - skipping ClipboardManager creation");
        }

        // CRITICAL: Only create ClipboardManager on the retry path (with_persistence=false)
        //
        // The archive (wrd-server-specs) documents that creating a ClipboardManager on the
        // first attempt leaves orphaned D-Bus proxy state when that attempt fails. When we
        // then create a second ClipboardManager for the retry, GNOME Shell's Portal backend
        // returns "Invalid state" because it still has state from the first proxy.
        //
        // The working pattern from the archive:
        // - First attempt: Pass None for clipboard (don't create ClipboardManager)
        // - Retry attempt: Create ClipboardManager and pass it (only one ever exists)
        // - BUT: If service registry says clipboard is unavailable, NEVER create it
        let clipboard_mgr = if !clipboard_enabled {
            // Service registry says clipboard is unavailable (e.g., KDE portal instability)
            // Respect this decision - do NOT create ClipboardManager
            None
        } else if with_persistence {
            // First attempt - don't create clipboard, it will fail anyway due to persistence rejection
            debug!(
                attempt = "first",
                "Skipping clipboard creation (persistence attempt - will fail on GNOME)"
            );
            None
        } else {
            // Retry attempt - create clipboard manager now
            debug!(
                attempt = "retry",
                "Creating ClipboardManager for this attempt"
            );
            match lamco_portal::ClipboardManager::new().await {
                Ok(mgr) => {
                    info!(
                        attempt = "retry",
                        "ClipboardManager created - will request clipboard access"
                    );
                    Some(Arc::new(mgr))
                }
                Err(e) => {
                    warn!(
                        error = %e,
                        "ClipboardManager creation failed - clipboard not available"
                    );
                    None
                }
            }
        };

        let session_id = format!("lamco-rdp-{}", uuid::Uuid::new_v4());
        trace!(
            session_id = %session_id,
            has_clipboard = clipboard_mgr.is_some(),
            "Calling portal_manager.create_session()"
        );

        let result = portal_manager
            .create_session(
                session_id.clone(),
                clipboard_mgr.as_ref().map(|c| c.as_ref()),
            )
            .await?;

        info!(
            session_id = %session_id,
            streams = result.0.streams().len(),
            has_restore_token = result.1.is_some(),
            "Session created successfully"
        );

        Ok((result.0, result.1, portal_manager, clipboard_mgr))
    }

    fn parse_failure(&self, error: &anyhow::Error) -> SessionCreationFailure {
        let msg = format!("{:#}", error);

        if msg.contains("cannot persist") || msg.contains("InvalidArgument") {
            SessionCreationFailure::PersistenceRejected { error_message: msg }
        } else if msg.contains("source") || msg.contains("Source") {
            SessionCreationFailure::SourceSelectionFailed { error_message: msg }
        } else if msg.contains("device") || msg.contains("Device") {
            SessionCreationFailure::DeviceSelectionFailed { error_message: msg }
        } else if msg.contains("clipboard") || msg.contains("Clipboard") {
            SessionCreationFailure::ClipboardRequestFailed { error_message: msg }
        } else {
            SessionCreationFailure::Other { error_message: msg }
        }
    }
}

#[async_trait]
impl SessionFactory for PortalSessionFactory {
    async fn create_session(&self) -> Result<Arc<dyn SessionHandle>> {
        info!("PortalSessionFactory: Creating session");
        info!(
            "  Quirks: {:?}",
            self.quirks.iter().map(|q| q.name()).collect::<Vec<_>>()
        );

        // Skip first session if GNOME (always rejects persistence)
        // This avoids a portal daemon state bug where first failed session
        // prevents second session's clipboard.request() from working
        // See: docs/CLIPBOARD-FINAL-ANALYSIS.md
        if self.quirks.contains(&InitQuirk::GnomePersistenceRejected) {
            info!("Skipping persistence attempt (GNOME always rejects it)");
            trace!("Going directly to session without persistence");
            let (portal_handle, new_token, active_manager, clipboard_mgr) =
                self.attempt_session(false).await?;

            // Save token and build handle (same as retry path)
            if let Some(ref token) = new_token {
                info!("Received restore token, saving...");
                self.token_manager
                    .save_token("default", token)
                    .await
                    .context("Failed to save restore token")?;
                info!("Restore token saved");
            }

            let pipewire_fd = portal_handle.pipewire_fd();
            let streams: Vec<StreamInfo> = portal_handle
                .streams()
                .iter()
                .map(|s| StreamInfo {
                    node_id: s.node_id,
                    width: s.size.0,
                    height: s.size.1,
                    position_x: s.position.0,
                    position_y: s.position.1,
                })
                .collect();

            let session = portal_handle.session;
            let handle = PortalSessionHandleImpl {
                pipewire_fd,
                streams,
                remote_desktop: active_manager.remote_desktop().clone(),
                session: Arc::new(tokio::sync::RwLock::new(session)),
                clipboard_manager: clipboard_mgr,
                session_type: SessionType::Portal,
                session_valid: Arc::new(std::sync::atomic::AtomicBool::new(true)),
            };

            info!("Session created successfully via PortalSessionFactory");
            return Ok(Arc::new(handle));
        }

        // Standard two-attempt flow for non-GNOME compositors
        // First attempt: with persistence
        // Clipboard is created inside attempt_session, in the same D-Bus context
        let first_result = self.attempt_session(true).await;

        let (portal_handle, new_token, active_manager, clipboard_mgr) = match first_result {
            Ok(result) => result,
            Err(e) => {
                let failure = self.parse_failure(&e);
                warn!("First session attempt failed: {}", failure.message());

                if !failure.should_retry_without_persistence() {
                    return Err(e).context("Session creation failed");
                }

                info!(
                    quirk = "GnomePersistenceRejected",
                    "Retrying without persistence"
                );

                // Wait for Portal backend to clean up the failed session
                // The AsyncSessionCleanup quirk indicates sessions need time to release resources
                // INVESTIGATION: Increased from 100ms to 500ms to test if first session cleanup affects second
                if self.quirks.contains(&InitQuirk::AsyncSessionCleanup) {
                    let delay_ms = 500;
                    info!(
                        delay_ms = delay_ms,
                        quirk = "AsyncSessionCleanup",
                        "Waiting for session cleanup before retry"
                    );
                    tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
                    trace!("Cleanup delay complete, proceeding with retry");
                }

                // Retry creates a fresh PortalManager AND fresh ClipboardManager
                // Both in the same D-Bus context - this is the key fix
                trace!("Starting retry attempt with fresh D-Bus state");
                let retry_result = self
                    .attempt_session(false)
                    .await
                    .context("Session creation failed (retry without persistence)")?;

                retry_result
            }
        };

        if let Some(ref token) = new_token {
            info!("Received restore token, saving...");
            self.token_manager
                .save_token("default", token)
                .await
                .context("Failed to save restore token")?;
            info!("Restore token saved");
        }

        let pipewire_fd = portal_handle.pipewire_fd();
        let streams: Vec<StreamInfo> = portal_handle
            .streams()
            .iter()
            .map(|s| StreamInfo {
                node_id: s.node_id,
                width: s.size.0,
                height: s.size.1,
                position_x: s.position.0,
                position_y: s.position.1,
            })
            .collect();

        let session = portal_handle.session;

        let handle = PortalSessionHandleImpl {
            pipewire_fd,
            streams,
            remote_desktop: active_manager.remote_desktop().clone(),
            session: Arc::new(tokio::sync::RwLock::new(session)),
            clipboard_manager: clipboard_mgr,
            session_type: SessionType::Portal,
            session_valid: Arc::new(std::sync::atomic::AtomicBool::new(true)),
        };

        info!("Session created successfully via PortalSessionFactory");
        Ok(Arc::new(handle))
    }

    fn capabilities(&self) -> SessionFactoryCapabilities {
        let caps = self.registry.compositor_capabilities();

        // CRITICAL: Use service registry for clipboard, NOT raw portal capabilities
        // The service registry translates quirks (like KdePortalClipboardUnstable) into
        // service levels. Raw caps.portal.supports_clipboard ignores these quirks.
        let clipboard_usable = self
            .registry
            .service_level(ServiceId::Clipboard)
            .is_usable();

        SessionFactoryCapabilities {
            supports_clipboard: clipboard_usable,
            supports_persistence: self.registry.supports_session_persistence()
                && !self.expects_persistence_rejection(),
            supports_input_injection: caps.portal.supports_remote_desktop,
            supports_multi_monitor: true,
            requires_permission_dialog: !self.registry.supports_session_persistence(),
            strategy_type: SessionStrategyType::PortalToken,
        }
    }

    fn deployment_context(&self) -> DeploymentContext {
        self.registry.compositor_capabilities().deployment
    }

    fn quirks(&self) -> &[InitQuirk] {
        &self.quirks
    }

    fn name(&self) -> &'static str {
        "Portal Session Factory"
    }
}

// Make PortalSessionHandleImpl fields pub(crate) for factory access
// This is needed because the factory creates the handle directly
impl PortalSessionHandleImpl {
    pub(crate) fn new(
        pipewire_fd: i32,
        streams: Vec<StreamInfo>,
        remote_desktop: Arc<lamco_portal::RemoteDesktopManager>,
        session: ashpd::desktop::Session<
            'static,
            ashpd::desktop::remote_desktop::RemoteDesktop<'static>,
        >,
        clipboard_manager: Option<Arc<lamco_portal::ClipboardManager>>,
    ) -> Self {
        Self {
            pipewire_fd,
            streams,
            remote_desktop,
            session: Arc::new(tokio::sync::RwLock::new(session)),
            clipboard_manager,
            session_type: SessionType::Portal,
            session_valid: Arc::new(std::sync::atomic::AtomicBool::new(true)),
        }
    }
}
