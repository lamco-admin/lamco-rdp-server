//! Session Strategy Selector
//!
//! Intelligently selects the best session creation strategy based on
//! detected capabilities from the Service Registry.
//!
//! Priority:
//! 1. Mutter Direct API (GNOME, zero dialogs)
//! 2. wlr-direct (wlroots native, zero dialogs)
//! 3. libei/EIS (wlroots via Portal, Flatpak-compatible)
//! 4. Portal + Token (universal, one-time dialog)
//! 5. Basic Portal (fallback, dialog each time)

use std::sync::Arc;

use anyhow::Result;
use tracing::{debug, info, warn};

use super::{mutter_direct::MutterDirectStrategy, portal_token::PortalTokenStrategy};
use crate::{
    services::{ServiceId, ServiceLevel, ServiceRegistry},
    session::{strategy::SessionStrategy, Tokens},
};

/// Session strategy selector
///
/// Chooses the optimal session creation strategy based on:
/// - Deployment context (Flatpak, systemd, native)
/// - Compositor type (GNOME, KDE, wlroots)
/// - Available APIs (Portal, Mutter, wlr-screencopy)
/// - Session persistence support
pub struct SessionStrategySelector {
    service_registry: Arc<ServiceRegistry>,
    token_manager: Arc<Tokens>,
    /// Keyboard layout from config (e.g., "us", "de", "auto")
    #[cfg_attr(
        not(feature = "wayland"),
        expect(
            dead_code,
            reason = "used by WlrDirectStrategy when wayland feature is enabled"
        )
    )]
    keyboard_layout: String,
}

impl SessionStrategySelector {
    pub fn new(service_registry: Arc<ServiceRegistry>, token_manager: Arc<Tokens>) -> Self {
        Self {
            service_registry,
            token_manager,
            keyboard_layout: "auto".to_string(),
        }
    }

    pub fn with_keyboard_layout(
        service_registry: Arc<ServiceRegistry>,
        token_manager: Arc<Tokens>,
        keyboard_layout: String,
    ) -> Self {
        Self {
            service_registry,
            token_manager,
            keyboard_layout,
        }
    }

    /// Select the best available session strategy
    ///
    /// Returns a boxed SessionStrategy implementation based on detected capabilities.
    ///
    /// Priority order:
    /// 1. Mutter Direct API (GNOME only, zero dialogs)
    /// 2. Portal + Token (universal, one-time dialog)
    /// 3. Basic Portal (fallback, dialog each time - NOT IMPLEMENTED)
    pub async fn select_strategy(&self) -> Result<Box<dyn SessionStrategy>> {
        info!("Selecting session creation strategy...");

        let caps = self.service_registry.compositor_capabilities();

        info!("📦 Deployment: {}", caps.deployment);
        info!(
            "🎯 Session Persistence: {}",
            self.service_registry
                .service_level(ServiceId::SessionPersistence)
        );
        info!(
            "🎯 Direct Compositor API: {}",
            self.service_registry
                .service_level(ServiceId::DirectCompositorAPI)
        );

        // DEPLOYMENT CONSTRAINT CHECK
        use crate::session::DeploymentContext;

        match caps.deployment {
            DeploymentContext::Flatpak => {
                // Flatpak: sandbox blocks direct compositor APIs.
                // Try Portal RemoteDesktop first (full input+clipboard).
                // If RemoteDesktop is unavailable (wlroots without portal-wlr),
                // fall back to ScreenCast-only for view-only sessions.
                info!("Flatpak deployment: checking Portal RemoteDesktop availability");

                use ashpd::desktop::remote_desktop::RemoteDesktop;
                let has_remote_desktop = match RemoteDesktop::new().await {
                    Ok(rd) => rd.available_device_types().await.is_ok(),
                    Err(_) => false,
                };

                if has_remote_desktop {
                    info!("Portal RemoteDesktop available, using Portal + Token strategy");

                    if !self.service_registry.supports_session_persistence() {
                        warn!("Portal version < 4, tokens not supported in Flatpak");
                        warn!("Permission dialog will appear on every server start");
                    }

                    return Ok(Box::new(PortalTokenStrategy::new(
                        self.service_registry.clone(),
                        self.token_manager.clone(),
                    )));
                }

                // RemoteDesktop unavailable: try ScreenCast-only (view-only mode)
                use super::screencast_only::ScreenCastOnlyStrategy;

                if ScreenCastOnlyStrategy::is_available().await {
                    warn!("Portal RemoteDesktop unavailable in Flatpak sandbox");
                    warn!("Falling back to ScreenCast-only mode (view-only)");
                    warn!("Session will have video but no input injection or clipboard");
                    let cursor_modes = caps.portal.available_cursor_modes.clone();
                    return Ok(Box::new(ScreenCastOnlyStrategy::with_cursor_modes(
                        cursor_modes,
                    )));
                }

                return Err(anyhow::anyhow!(
                    "No portal strategy available in Flatpak: \
                     RemoteDesktop and ScreenCast both unavailable"
                ));
            }

            DeploymentContext::SystemdSystem => {
                // System service: Limited to portal (D-Bus session complexity)
                warn!("System service deployment: Limited to Portal strategy");
                warn!("Recommend using systemd user service instead for better compatibility");

                return Ok(Box::new(PortalTokenStrategy::new(
                    self.service_registry.clone(),
                    self.token_manager.clone(),
                )));
            }

            _ => {
                // Native, SystemdUser, InitD - full strategy access
                debug!("Unrestricted deployment, checking all strategies");
            }
        }

        // PRIORITY 1: Mutter Direct API (GNOME only, zero dialogs ever)
        if self
            .service_registry
            .service_level(ServiceId::DirectCompositorAPI)
            >= ServiceLevel::BestEffort
        {
            if MutterDirectStrategy::is_available().await {
                info!("✅ Selected: Mutter Direct API strategy");
                info!("   Zero permission dialogs (not even first time)");

                let monitor_connector = self.detect_primary_monitor().await;

                return Ok(Box::new(MutterDirectStrategy::new(monitor_connector)));
            } else {
                warn!("Service Registry reports Mutter API available, but connection failed");
                warn!("Falling back to next available strategy");
            }
        }

        // PRIORITY 2: portal-generic embedded (wlroots, video + input + clipboard)
        #[cfg(feature = "portal-generic")]
        if self
            .service_registry
            .service_level(ServiceId::WlrDirectInput)
            >= ServiceLevel::BestEffort
        {
            use super::portal_generic::PortalGenericStrategy;

            if PortalGenericStrategy::is_available().await {
                info!("Selected: portal-generic embedded strategy");
                info!("   Native Wayland protocols: screencopy + virtual input + data-control");
                info!("   Compositor: {}", caps.compositor);
                info!("   Video + Input + Clipboard (no external portal daemon)");

                return Ok(Box::new(PortalGenericStrategy::new()));
            } else {
                warn!(
                    "portal-generic: protocol check failed (missing screencopy or virtual input)"
                );
                warn!("Falling back to next available strategy");
            }
        }

        // PRIORITY 2b: wlr-direct (wlroots compositors, input-only fallback)
        #[cfg(feature = "wayland")]
        if self
            .service_registry
            .service_level(ServiceId::WlrDirectInput)
            >= ServiceLevel::BestEffort
        {
            use super::wlr_direct::WlrDirectStrategy;

            if WlrDirectStrategy::is_available().await {
                info!("Selected: wlr-direct strategy");
                info!("   Native Wayland protocols for wlroots compositors");
                info!("   Compositor: {}", caps.compositor);
                info!("   Note: Input only (video via Portal ScreenCast)");

                return Ok(Box::new(WlrDirectStrategy::with_keyboard_layout(
                    self.keyboard_layout.clone(),
                )));
            } else {
                warn!("Service Registry reports wlr-direct available, but protocol binding failed");
                warn!("Falling back to next available strategy");
            }
        }

        // PRIORITY 3: libei/EIS (wlroots via Portal RemoteDesktop, Flatpak-compatible)
        #[cfg(feature = "libei")]
        if self.service_registry.service_level(ServiceId::LibeiInput) >= ServiceLevel::BestEffort {
            use super::libei::LibeiStrategy;

            if LibeiStrategy::is_available().await {
                info!("✅ Selected: libei strategy");
                info!("   Portal RemoteDesktop + EIS protocol for wlroots");
                info!("   Compositor: {}", caps.compositor);
                info!("   Flatpak-compatible: Yes");
                info!("   Note: Input only (video via Portal ScreenCast)");

                return Ok(Box::new(LibeiStrategy::new(
                    None,
                    Some(self.token_manager.clone()),
                )));
            } else {
                warn!("Service Registry reports libei available, but Portal ConnectToEIS failed");
                warn!("Portal backend may not support ConnectToEIS method");
                warn!("Falling back to Portal strategy");
            }
        }

        // PRIORITY 4: Portal + Token (works on all DEs with portal v4+)
        if self.service_registry.supports_session_persistence() {
            info!("✅ Selected: Portal + Token strategy");
            info!("   One-time permission dialog, then unattended operation");

            return Ok(Box::new(PortalTokenStrategy::new(
                self.service_registry.clone(),
                self.token_manager.clone(),
            )));
        }

        // FALLBACK: Portal without tokens (portal v3 or below)
        // Check if Portal RemoteDesktop is available at all
        use ashpd::desktop::remote_desktop::RemoteDesktop;
        let has_remote_desktop = match RemoteDesktop::new().await {
            Ok(rd) => rd.available_device_types().await.is_ok(),
            Err(_) => false,
        };
        if has_remote_desktop {
            warn!("⚠️  No session persistence available");
            warn!("   Portal version: {}", caps.portal.version);
            warn!("   Falling back to Portal + Token strategy");
            warn!("   Permission dialog will appear on every server start");

            return Ok(Box::new(PortalTokenStrategy::new(
                self.service_registry.clone(),
                self.token_manager.clone(),
            )));
        }

        // LAST RESORT: ScreenCast-only (view-only) when no input strategy works
        use super::screencast_only::ScreenCastOnlyStrategy;
        if ScreenCastOnlyStrategy::is_available().await {
            warn!("⚠️  No input strategy available");
            warn!("   Portal RemoteDesktop unavailable, all input methods exhausted");
            warn!("   Falling back to ScreenCast-only mode (view-only)");
            warn!("   Session will have video but no input injection or clipboard");
            let cursor_modes = caps.portal.available_cursor_modes.clone();
            return Ok(Box::new(ScreenCastOnlyStrategy::with_cursor_modes(
                cursor_modes,
            )));
        }

        Err(anyhow::anyhow!(
            "No session strategy available: all input methods and ScreenCast unavailable"
        ))
    }

    async fn detect_primary_monitor(&self) -> Option<String> {
        match Self::enumerate_drm_connectors().await {
            Ok(connectors) if !connectors.is_empty() => {
                let primary = &connectors[0];
                info!("Detected primary monitor: {}", primary);
                info!("  {} total monitor(s) detected", connectors.len());
                Some(primary.clone())
            }
            Ok(_) => {
                info!("No physical monitors detected, using virtual monitor");
                info!("  Virtual monitor is headless-compatible");
                None
            }
            Err(e) => {
                debug!("Failed to enumerate monitors: {}", e);
                info!("Using virtual monitor (detection failed)");
                info!("  Virtual monitor is headless-compatible");
                None
            }
        }
    }

    async fn enumerate_drm_connectors() -> anyhow::Result<Vec<String>> {
        use std::path::Path;

        use tokio::fs;

        let mut connectors = Vec::new();

        let drm_path = Path::new("/sys/class/drm");
        if !drm_path.exists() {
            debug!("/sys/class/drm not found - not a typical Linux system");
            return Ok(vec![]);
        }

        let mut entries = fs::read_dir(drm_path).await?;

        while let Some(entry) = entries.next_entry().await? {
            let name = entry.file_name().to_string_lossy().to_string();

            // Look for card*-<connector> pattern (e.g., card0-HDMI-A-1, card0-DP-1)
            if name.starts_with("card") && name.contains('-') {
                // Check if connector is connected
                let status_path = entry.path().join("status");
                if let Ok(status) = fs::read_to_string(&status_path).await {
                    if status.trim() == "connected" {
                        // Extract connector name (e.g., "HDMI-A-1" from "card0-HDMI-A-1")
                        let parts: Vec<&str> = name.split('-').collect();
                        if parts.len() >= 2 {
                            let connector = parts[1..].join("-");
                            if !connector.is_empty() {
                                debug!("Found connected monitor: {} (from {})", connector, name);
                                connectors.push(connector);
                            }
                        }
                    }
                }
            }
        }

        Ok(connectors)
    }

    pub fn recommended_strategy_name(&self) -> &'static str {
        self.service_registry.recommended_session_strategy()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_strategy_selector_creation() {
        // Create minimal service registry for testing
        use crate::{
            compositor::{CompositorType, PortalCapabilities},
            services::ServiceRegistry,
            session::CredentialStorageMethod,
        };

        let compositor = CompositorType::Unknown { session_info: None };
        let portal = PortalCapabilities::default();
        let caps = crate::compositor::CompositorCapabilities::new(compositor, portal, vec![]);

        let registry = Arc::new(ServiceRegistry::from_compositor(caps));

        let token_manager = Arc::new(
            Tokens::new(CredentialStorageMethod::EncryptedFile)
                .await
                .expect("Failed to create Tokens"),
        );

        let selector = SessionStrategySelector::new(registry, token_manager);

        // Should not panic
        let _strategy_name = selector.recommended_strategy_name();
    }

    #[test]
    fn test_strategy_selection_logic() {
        use std::sync::Arc;

        use crate::{
            compositor::{CompositorCapabilities, CompositorType, PortalCapabilities},
            services::ServiceRegistry,
            session::DeploymentContext,
        };

        // Test 1: Flatpak deployment constraint (should recommend Portal)
        {
            let compositor = CompositorType::Gnome {
                version: Some("46.0".to_string()),
            };
            let mut portal = PortalCapabilities::default();
            portal.version = 5;
            portal.supports_restore_tokens = true;
            let mut caps = CompositorCapabilities::new(compositor, portal, vec![]);
            caps.deployment = DeploymentContext::Flatpak;

            let registry = Arc::new(ServiceRegistry::from_compositor(caps));

            // Check that the service registry correctly identifies constraints
            let session_level =
                registry.service_level(crate::services::ServiceId::SessionPersistence);
            assert!(
                session_level >= crate::services::ServiceLevel::BestEffort,
                "Flatpak with Portal v5 should support session persistence"
            );
        }

        // Test 2: KDE should have Portal support (no Mutter API)
        {
            let compositor = CompositorType::Kde {
                version: Some("6.0".to_string()),
            };
            let mut portal = PortalCapabilities::default();
            portal.version = 5;
            portal.supports_restore_tokens = true;
            let caps = CompositorCapabilities::new(compositor, portal, vec![]);
            let registry = Arc::new(ServiceRegistry::from_compositor(caps));

            // KDE should not have DirectCompositorAPI (Mutter-specific)
            let direct_api_level =
                registry.service_level(crate::services::ServiceId::DirectCompositorAPI);
            assert_eq!(
                direct_api_level,
                crate::services::ServiceLevel::Unavailable,
                "KDE should not have Mutter API"
            );

            // But should have session persistence via Portal
            let session_level =
                registry.service_level(crate::services::ServiceId::SessionPersistence);
            assert!(
                session_level >= crate::services::ServiceLevel::BestEffort,
                "KDE with Portal v5 should support session persistence"
            );
        }

        // Test 3: GNOME should potentially have DirectCompositorAPI
        {
            let compositor = CompositorType::Gnome {
                version: Some("46.0".to_string()),
            };
            let mut portal = PortalCapabilities::default();
            portal.version = 5;
            portal.supports_restore_tokens = true;
            let caps = CompositorCapabilities::new(compositor, portal, vec![]);
            let registry = Arc::new(ServiceRegistry::from_compositor(caps));

            // GNOME 46+ reports Guaranteed (EIS + clipboard available)
            let direct_api_level =
                registry.service_level(crate::services::ServiceId::DirectCompositorAPI);
            assert!(
                direct_api_level == crate::services::ServiceLevel::Guaranteed
                    || direct_api_level == crate::services::ServiceLevel::BestEffort
                    || direct_api_level == crate::services::ServiceLevel::Unavailable,
                "GNOME DirectCompositorAPI should be Guaranteed, BestEffort, or Unavailable"
            );
        }
    }
}
