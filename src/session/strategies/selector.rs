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

use anyhow::Result;
use std::sync::Arc;
use tracing::{debug, info, warn};

use crate::services::{ServiceId, ServiceLevel, ServiceRegistry};
use crate::session::strategy::SessionStrategy;
use crate::session::TokenManager;

use super::mutter_direct::MutterDirectStrategy;
use super::portal_token::PortalTokenStrategy;

/// Session strategy selector
///
/// Chooses the optimal session creation strategy based on:
/// - Deployment context (Flatpak, systemd, native)
/// - Compositor type (GNOME, KDE, wlroots)
/// - Available APIs (Portal, Mutter, wlr-screencopy)
/// - Session persistence support
pub struct SessionStrategySelector {
    service_registry: Arc<ServiceRegistry>,
    token_manager: Arc<TokenManager>,
}

impl SessionStrategySelector {
    /// Create a new strategy selector
    ///
    /// # Arguments
    ///
    /// * `service_registry` - For querying available capabilities
    /// * `token_manager` - For token-based strategies
    pub fn new(service_registry: Arc<ServiceRegistry>, token_manager: Arc<TokenManager>) -> Self {
        Self {
            service_registry,
            token_manager,
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

        // Log deployment context
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
                // Flatpak: ONLY portal strategy available (sandbox blocks direct APIs)
                info!("Flatpak deployment: Portal + Token is only available strategy");

                if !self.service_registry.supports_session_persistence() {
                    warn!("Portal version < 4, tokens not supported in Flatpak");
                    warn!("Permission dialog will appear on every server start");
                }

                return Ok(Box::new(PortalTokenStrategy::new(
                    self.service_registry.clone(),
                    self.token_manager.clone(),
                )));
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
            // Verify Mutter API is actually accessible
            if MutterDirectStrategy::is_available().await {
                info!("✅ Selected: Mutter Direct API strategy");
                info!("   Zero permission dialogs (not even first time)");

                // Check if we should use physical monitor or virtual
                let monitor_connector = self.detect_primary_monitor().await;

                return Ok(Box::new(MutterDirectStrategy::new(monitor_connector)));
            } else {
                warn!("Service Registry reports Mutter API available, but connection failed");
                warn!("Falling back to next available strategy");
            }
        }

        // PRIORITY 2: wlr-direct (wlroots compositors, native protocols)
        #[cfg(feature = "wayland")]
        if self
            .service_registry
            .service_level(ServiceId::WlrDirectInput)
            >= ServiceLevel::BestEffort
        {
            use super::wlr_direct::WlrDirectStrategy;

            // Verify protocols are actually accessible
            if WlrDirectStrategy::is_available().await {
                info!("✅ Selected: wlr-direct strategy");
                info!("   Native Wayland protocols for wlroots compositors");
                info!("   Compositor: {}", caps.compositor);
                info!("   Note: Input only (video via Portal ScreenCast)");

                return Ok(Box::new(WlrDirectStrategy::new()));
            } else {
                warn!("Service Registry reports wlr-direct available, but protocol binding failed");
                warn!("Falling back to next available strategy");
            }
        }

        // PRIORITY 3: libei/EIS (wlroots via Portal RemoteDesktop, Flatpak-compatible)
        #[cfg(feature = "libei")]
        if self.service_registry.service_level(ServiceId::LibeiInput) >= ServiceLevel::BestEffort {
            use super::libei::LibeiStrategy;

            // Verify Portal RemoteDesktop with ConnectToEIS is accessible
            if LibeiStrategy::is_available().await {
                info!("✅ Selected: libei strategy");
                info!("   Portal RemoteDesktop + EIS protocol for wlroots");
                info!("   Compositor: {}", caps.compositor);
                info!("   Flatpak-compatible: Yes");
                info!("   Note: Input only (video via Portal ScreenCast)");

                return Ok(Box::new(LibeiStrategy::new(None)));
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
        warn!("⚠️  No session persistence available");
        warn!("   Portal version: {}", caps.portal.version);
        warn!("   Falling back to Portal + Token strategy");
        warn!("   Permission dialog will appear on every server start");

        // Still use Portal + Token strategy (token just won't work)
        Ok(Box::new(PortalTokenStrategy::new(
            self.service_registry.clone(),
            self.token_manager.clone(),
        )))
    }

    /// Detect primary monitor connector for Mutter
    ///
    /// Returns Some(connector) if physical monitor detected, None for virtual
    async fn detect_primary_monitor(&self) -> Option<String> {
        // Try to detect connected physical monitors from DRM subsystem
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

    /// Enumerate connected DRM connectors
    ///
    /// Reads /sys/class/drm/ to find connected displays
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

    /// Get recommended strategy name for logging
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
        use crate::compositor::{CompositorType, PortalCapabilities};
        use crate::services::ServiceRegistry;
        use crate::session::CredentialStorageMethod;

        let compositor = CompositorType::Unknown { session_info: None };
        let portal = PortalCapabilities::default();
        let caps = crate::compositor::CompositorCapabilities::new(compositor, portal, vec![]);

        let registry = Arc::new(ServiceRegistry::from_compositor(caps));

        let token_manager = Arc::new(
            TokenManager::new(CredentialStorageMethod::EncryptedFile)
                .await
                .expect("Failed to create TokenManager"),
        );

        let selector = SessionStrategySelector::new(registry, token_manager);

        // Should not panic
        let _strategy_name = selector.recommended_strategy_name();
    }

    #[test]
    fn test_strategy_selection_logic() {
        use crate::compositor::{CompositorCapabilities, CompositorType, PortalCapabilities};
        use crate::services::ServiceRegistry;
        use crate::session::{CredentialStorageMethod, DeploymentContext};
        use std::sync::Arc;

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

            // GNOME might have DirectCompositorAPI (requires actual D-Bus test)
            // In tests, it will be Unavailable (no D-Bus connection)
            let direct_api_level =
                registry.service_level(crate::services::ServiceId::DirectCompositorAPI);
            // We can't assert it's available without D-Bus, but we can check the logic exists
            assert!(
                direct_api_level == crate::services::ServiceLevel::BestEffort
                    || direct_api_level == crate::services::ServiceLevel::Unavailable,
                "GNOME DirectCompositorAPI should be either BestEffort or Unavailable"
            );
        }
    }
}
