//! Service Registry implementation
//!
//! The central registry that holds all advertised services and provides
//! query methods for runtime feature decisions.

use std::collections::HashMap;

use tracing::info;

use super::{
    service::{AdvertisedService, ServiceId, ServiceLevel},
    translation::translate_capabilities,
};
use crate::compositor::CompositorCapabilities;

/// Central service registry
///
/// Holds the translated services from compositor capabilities and
/// provides efficient lookup methods for runtime decisions.
#[derive(Debug)]
pub struct ServiceRegistry {
    /// Original compositor capabilities
    compositor_caps: CompositorCapabilities,

    /// Advertised services indexed by ID
    services: HashMap<ServiceId, AdvertisedService>,

    /// Sorted list for iteration
    services_list: Vec<AdvertisedService>,

    /// Compositor name for logging
    compositor_name: String,
}

impl ServiceRegistry {
    pub fn from_compositor(caps: CompositorCapabilities) -> Self {
        let compositor_name = caps.compositor.to_string();
        let services_list = translate_capabilities(&caps);

        let mut services = HashMap::new();
        for service in &services_list {
            services.insert(service.id, service.clone());
        }

        Self {
            compositor_caps: caps,
            services,
            services_list,
            compositor_name,
        }
    }

    pub fn has_service(&self, id: ServiceId) -> bool {
        self.services
            .get(&id)
            .is_some_and(|s| s.level > ServiceLevel::Unavailable)
    }

    /// Returns `Unavailable` if service doesn't exist in the registry.
    pub fn service_level(&self, id: ServiceId) -> ServiceLevel {
        self.services
            .get(&id)
            .map_or(ServiceLevel::Unavailable, |s| s.level)
    }

    pub fn get_service(&self, id: ServiceId) -> Option<&AdvertisedService> {
        self.services.get(&id)
    }

    pub fn all_services(&self) -> &[AdvertisedService] {
        &self.services_list
    }

    pub fn services_at_level(&self, min_level: ServiceLevel) -> Vec<&AdvertisedService> {
        self.services_list
            .iter()
            .filter(|s| s.level >= min_level)
            .collect()
    }

    pub fn guaranteed_services(&self) -> Vec<&AdvertisedService> {
        self.services_at_level(ServiceLevel::Guaranteed)
    }

    pub fn usable_services(&self) -> Vec<&AdvertisedService> {
        self.services_at_level(ServiceLevel::Degraded)
    }

    pub fn compositor_capabilities(&self) -> &CompositorCapabilities {
        &self.compositor_caps
    }

    pub fn compositor_name(&self) -> &str {
        &self.compositor_name
    }

    pub fn service_counts(&self) -> ServiceCounts {
        let mut counts = ServiceCounts::default();
        for service in &self.services_list {
            match service.level {
                ServiceLevel::Guaranteed => counts.guaranteed += 1,
                ServiceLevel::BestEffort => counts.best_effort += 1,
                ServiceLevel::Degraded => counts.degraded += 1,
                ServiceLevel::Unavailable => counts.unavailable += 1,
            }
        }
        counts
    }

    pub fn log_summary(&self) {
        info!("╔════════════════════════════════════════════════════════════╗");
        info!("║              Service Advertisement Registry                ║");
        info!("╚════════════════════════════════════════════════════════════╝");
        info!("  Compositor: {}", self.compositor_name);

        let counts = self.service_counts();
        info!(
            "  Services: {} guaranteed, {} best-effort, {} degraded, {} unavailable",
            counts.guaranteed, counts.best_effort, counts.degraded, counts.unavailable
        );

        info!("  ─────────────────────────────────────────────────────────");
        for service in &self.services_list {
            let emoji = service.level.emoji();
            let rdp_info = service
                .rdp_capability
                .as_ref()
                .map(|c| format!(" → {c}"))
                .unwrap_or_default();

            info!(
                "  {} {:20} {:12}{}",
                emoji,
                service.name,
                format!("[{}]", service.level),
                rdp_info
            );

            if let Some(note) = &service.notes {
                info!("      ↳ {}", note);
            }
        }
        info!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    }

    pub fn status_line(&self) -> String {
        let counts = self.service_counts();
        format!(
            "Services: ✅{} 🔶{} ⚠️{} ❌{}",
            counts.guaranteed, counts.best_effort, counts.degraded, counts.unavailable
        )
    }

    pub fn recommended_fps(&self) -> u32 {
        self.compositor_caps.profile.recommended_fps_cap
    }

    pub fn should_enable_adaptive_fps(&self) -> bool {
        self.service_level(ServiceId::DamageTracking) >= ServiceLevel::BestEffort
    }

    pub fn should_use_predictive_cursor(&self) -> bool {
        // Predictive cursor is most valuable when metadata cursor is available
        // but network latency makes raw position updates feel laggy
        self.service_level(ServiceId::MetadataCursor) >= ServiceLevel::BestEffort
    }

    // ========================================================================
    // PHASE 2: Session Persistence Query Methods
    // ========================================================================

    /// Portal v4+ restore tokens and credential storage needed.
    pub fn supports_session_persistence(&self) -> bool {
        self.service_level(ServiceId::SessionPersistence) >= ServiceLevel::BestEffort
    }

    /// Requires restore tokens or direct API to start without user interaction.
    pub fn supports_unattended_access(&self) -> bool {
        self.service_level(ServiceId::UnattendedAccess) >= ServiceLevel::BestEffort
    }

    /// GNOME compositor with Mutter D-Bus interfaces (bypasses portal).
    pub fn has_mutter_direct_api(&self) -> bool {
        self.service_level(ServiceId::DirectCompositorAPI) >= ServiceLevel::BestEffort
    }

    /// wlroots compositor with screencopy protocol (bypasses portal).
    pub fn has_wlr_screencopy(&self) -> bool {
        self.service_level(ServiceId::WlrScreencopy) >= ServiceLevel::Guaranteed
    }

    /// Compositor with the standardized ext-image-copy-capture-v1 protocol
    /// (bypasses portal). Equivalent bypass to wlr-screencopy, but present on
    /// compositors that never had wlr-screencopy at all (Mir, phoc, Jay,
    /// Treeland) or have since migrated off it (Wayfire 0.11+).
    pub fn has_ext_image_copy_capture(&self) -> bool {
        self.service_level(ServiceId::ExtImageCopyCapture) >= ServiceLevel::Guaranteed
    }

    pub fn credential_storage_level(&self) -> ServiceLevel {
        self.service_level(ServiceId::CredentialStorage)
    }

    pub fn recommended_session_strategy(&self) -> &'static str {
        if self.has_wlr_screencopy() {
            "wlr-screencopy (no dialog)"
        } else if self.has_ext_image_copy_capture() {
            "ext-image-copy-capture (no dialog)"
        } else if self.has_mutter_direct_api() {
            "Mutter Direct API (no dialog)"
        } else if self.supports_session_persistence() {
            "Portal + Restore Token (one-time dialog)"
        } else {
            "Basic Portal (dialog each time)"
        }
    }

    // === Authentication Service Queries ===

    /// Not available in Flatpak (sandboxed, no /etc/pam.d/ access).
    pub fn has_pam_auth(&self) -> bool {
        self.service_level(ServiceId::PamAuthentication) >= ServiceLevel::BestEffort
    }

    pub fn pam_auth_level(&self) -> ServiceLevel {
        self.service_level(ServiceId::PamAuthentication)
    }

    pub fn recommended_auth_method(&self) -> &'static str {
        if self.has_pam_auth() { "pam" } else { "none" }
    }

    /// NLA requires a working authentication backend (PAM).
    /// Disabled in Flatpak where PAM is inaccessible.
    pub fn should_enable_nla(&self) -> bool {
        self.has_pam_auth()
    }

    pub fn available_auth_methods(&self) -> Vec<&'static str> {
        let mut methods = vec![];

        if self.service_level(ServiceId::PamAuthentication) >= ServiceLevel::Degraded {
            methods.push("pam");
        }

        // "none" is always available
        methods.push("none");

        methods
    }
}

/// Service counts by level
#[derive(Debug, Clone, Default)]
pub struct ServiceCounts {
    /// Number of guaranteed services
    pub guaranteed: usize,
    /// Number of best-effort services
    pub best_effort: usize,
    /// Number of degraded services
    pub degraded: usize,
    /// Number of unavailable services
    pub unavailable: usize,
}

impl ServiceCounts {
    pub fn usable(&self) -> usize {
        self.guaranteed + self.best_effort + self.degraded
    }

    pub fn reliable(&self) -> usize {
        self.guaranteed + self.best_effort
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compositor::{CompositorType, CursorMode, PortalCapabilities, SourceType};

    fn make_test_caps() -> CompositorCapabilities {
        let compositor = CompositorType::Gnome {
            version: Some("46.0".to_string()),
        };

        let mut portal = PortalCapabilities::default();
        portal.supports_screencast = true;
        portal.supports_remote_desktop = true;
        portal.supports_clipboard = true;
        portal.version = 5;
        portal.available_cursor_modes = vec![CursorMode::Metadata, CursorMode::Embedded];
        portal.available_source_types = vec![SourceType::Monitor, SourceType::Window];

        CompositorCapabilities::new(compositor, portal, vec![])
    }

    #[test]
    fn test_registry_creation() {
        let caps = make_test_caps();
        let registry = ServiceRegistry::from_compositor(caps);

        assert!(!registry.all_services().is_empty());
        assert!(registry.compositor_name().contains("GNOME"));
    }

    #[test]
    fn test_has_service() {
        let caps = make_test_caps();
        let registry = ServiceRegistry::from_compositor(caps);

        // Should have damage tracking
        assert!(registry.has_service(ServiceId::DamageTracking));

        // Should have video capture
        assert!(registry.has_service(ServiceId::VideoCapture));
    }

    #[test]
    fn test_service_level() {
        let caps = make_test_caps();
        let registry = ServiceRegistry::from_compositor(caps);

        // Video capture: BestEffort without H.264 encoder, Guaranteed with
        let level = registry.service_level(ServiceId::VideoCapture);
        // Test caps have encoding: None (no OpenH264 in test), so BestEffort
        assert_eq!(level, ServiceLevel::BestEffort);
    }

    #[test]
    fn test_service_counts() {
        let caps = make_test_caps();
        let registry = ServiceRegistry::from_compositor(caps);
        let counts = registry.service_counts();

        // Should have some guaranteed services
        assert!(counts.guaranteed > 0);

        // Total should match service list
        let total = counts.guaranteed + counts.best_effort + counts.degraded + counts.unavailable;
        assert_eq!(total, registry.all_services().len());
    }

    #[test]
    fn test_services_at_level() {
        let caps = make_test_caps();
        let registry = ServiceRegistry::from_compositor(caps);

        let guaranteed = registry.services_at_level(ServiceLevel::Guaranteed);
        for service in &guaranteed {
            assert_eq!(service.level, ServiceLevel::Guaranteed);
        }
    }
}
