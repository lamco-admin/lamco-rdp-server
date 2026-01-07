//! Service Registry implementation
//!
//! The central registry that holds all advertised services and provides
//! query methods for runtime feature decisions.

use crate::compositor::CompositorCapabilities;
use std::collections::HashMap;
use tracing::debug;

use super::{
    service::{AdvertisedService, ServiceId, ServiceLevel},
    translation::translate_capabilities,
};

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
    /// Create a service registry from compositor capabilities
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

    /// Check if a service is available (at any level)
    pub fn has_service(&self, id: ServiceId) -> bool {
        self.services
            .get(&id)
            .map(|s| s.level > ServiceLevel::Unavailable)
            .unwrap_or(false)
    }

    /// Get the service level for a service ID
    ///
    /// Returns `Unavailable` if service doesn't exist.
    pub fn service_level(&self, id: ServiceId) -> ServiceLevel {
        self.services
            .get(&id)
            .map(|s| s.level)
            .unwrap_or(ServiceLevel::Unavailable)
    }

    /// Get a specific service by ID
    pub fn get_service(&self, id: ServiceId) -> Option<&AdvertisedService> {
        self.services.get(&id)
    }

    /// Get all advertised services
    pub fn all_services(&self) -> &[AdvertisedService] {
        &self.services_list
    }

    /// Get all services at or above a certain level
    pub fn services_at_level(&self, min_level: ServiceLevel) -> Vec<&AdvertisedService> {
        self.services_list
            .iter()
            .filter(|s| s.level >= min_level)
            .collect()
    }

    /// Get all guaranteed services
    pub fn guaranteed_services(&self) -> Vec<&AdvertisedService> {
        self.services_at_level(ServiceLevel::Guaranteed)
    }

    /// Get all usable services (anything above Unavailable)
    pub fn usable_services(&self) -> Vec<&AdvertisedService> {
        self.services_at_level(ServiceLevel::Degraded)
    }

    /// Get the underlying compositor capabilities
    pub fn compositor_capabilities(&self) -> &CompositorCapabilities {
        &self.compositor_caps
    }

    /// Get compositor name
    pub fn compositor_name(&self) -> &str {
        &self.compositor_name
    }

    /// Count services by level
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

    /// Log a summary of the service registry
    pub fn log_summary(&self) {
        let counts = self.service_counts();
        debug!(
            "Services for {}: {} guaranteed, {} best-effort, {} degraded, {} unavailable",
            self.compositor_name,
            counts.guaranteed,
            counts.best_effort,
            counts.degraded,
            counts.unavailable
        );

        for service in &self.services_list {
            let rdp_info = service
                .rdp_capability
                .as_ref()
                .map(|c| format!(" -> {}", c))
                .unwrap_or_default();

            debug!(
                "  {:20} {:12}{}",
                service.name,
                format!("[{}]", service.level),
                rdp_info
            );

            if let Some(note) = &service.notes {
                debug!("    note: {}", note);
            }
        }
    }

    /// Generate a concise status line for logging
    pub fn status_line(&self) -> String {
        let counts = self.service_counts();
        format!(
            "Services: ✅{} 🔶{} ⚠️{} ❌{}",
            counts.guaranteed, counts.best_effort, counts.degraded, counts.unavailable
        )
    }

    /// Get recommended codec list based on service levels
    ///
    /// Returns a list of codec names suitable for IronRDP's `server_codecs_capabilities()`.
    /// The order reflects preference based on available services.
    pub fn recommended_codecs(&self) -> Vec<&'static str> {
        let mut codecs = Vec::new();

        // Check DMA-BUF service for zero-copy path
        let dmabuf_level = self.service_level(ServiceId::DmaBufZeroCopy);
        let damage_level = self.service_level(ServiceId::DamageTracking);

        // If we have guaranteed zero-copy, prefer AVC444 for quality
        // Otherwise, stick with AVC420 which is more compatible
        if dmabuf_level >= ServiceLevel::Guaranteed && damage_level >= ServiceLevel::Guaranteed {
            // Optimal path: zero-copy + good damage tracking
            // Note: In practice, AVC444 requires both main and aux streams working
            // For now, always prefer AVC420 as it's more reliable
            codecs.push("remotefx");
        } else if damage_level >= ServiceLevel::BestEffort {
            // Good damage tracking means we can be efficient
            codecs.push("remotefx");
        } else {
            // Fallback: basic RemoteFX
            codecs.push("remotefx");
        }

        codecs
    }

    /// Check if AVC444 mode should be enabled
    ///
    /// AVC444 requires reliable damage tracking and preferably zero-copy buffers.
    pub fn should_enable_avc444(&self) -> bool {
        let dmabuf_level = self.service_level(ServiceId::DmaBufZeroCopy);
        let damage_level = self.service_level(ServiceId::DamageTracking);

        // AVC444 is more demanding - require guaranteed services
        dmabuf_level >= ServiceLevel::Guaranteed && damage_level >= ServiceLevel::Guaranteed
    }

    /// Get recommended FPS cap based on compositor profile
    pub fn recommended_fps(&self) -> u32 {
        self.compositor_caps.profile.recommended_fps_cap
    }

    /// Check if adaptive FPS should be enabled
    pub fn should_enable_adaptive_fps(&self) -> bool {
        self.service_level(ServiceId::DamageTracking) >= ServiceLevel::BestEffort
    }

    /// Check if predictive cursor should be used
    pub fn should_use_predictive_cursor(&self) -> bool {
        // Predictive cursor is most valuable when metadata cursor is available
        // but network latency makes raw position updates feel laggy
        self.service_level(ServiceId::MetadataCursor) >= ServiceLevel::BestEffort
    }

    // ========================================================================
    // PHASE 2: Session Persistence Query Methods
    // ========================================================================

    /// Check if session persistence is available (portal restore tokens)
    ///
    /// Returns true if portal v4+ and credential storage is available
    pub fn supports_session_persistence(&self) -> bool {
        self.service_level(ServiceId::SessionPersistence) >= ServiceLevel::BestEffort
    }

    /// Check if unattended operation is possible
    ///
    /// Returns true if we can start without user interaction (tokens or direct API)
    pub fn supports_unattended_access(&self) -> bool {
        self.service_level(ServiceId::UnattendedAccess) >= ServiceLevel::BestEffort
    }

    /// Check if Mutter Direct API is available (GNOME bypass)
    ///
    /// Returns true if GNOME compositor with Mutter D-Bus interfaces detected
    pub fn has_mutter_direct_api(&self) -> bool {
        self.service_level(ServiceId::DirectCompositorAPI) >= ServiceLevel::BestEffort
    }

    /// Check if wlr-screencopy is available (wlroots bypass)
    ///
    /// Returns true if wlroots compositor with screencopy protocol detected
    pub fn has_wlr_screencopy(&self) -> bool {
        self.service_level(ServiceId::WlrScreencopy) >= ServiceLevel::Guaranteed
    }

    /// Get credential storage service level
    pub fn credential_storage_level(&self) -> ServiceLevel {
        self.service_level(ServiceId::CredentialStorage)
    }

    /// Check if server can avoid permission dialog (via any method)
    ///
    /// Returns true if one of these is available:
    /// - Portal restore tokens
    /// - Mutter Direct API
    /// - wlr-screencopy
    pub fn can_avoid_permission_dialog(&self) -> bool {
        self.supports_session_persistence()
            || self.has_mutter_direct_api()
            || self.has_wlr_screencopy()
    }

    /// Get the best available session strategy
    ///
    /// Returns a string describing the recommended session persistence strategy
    pub fn recommended_session_strategy(&self) -> &'static str {
        if self.has_wlr_screencopy() {
            "wlr-screencopy (no dialog)"
        } else if self.has_mutter_direct_api() {
            "Mutter Direct API (no dialog)"
        } else if self.supports_session_persistence() {
            "Portal + Restore Token (one-time dialog)"
        } else {
            "Basic Portal (dialog each time)"
        }
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
    /// Total usable services (everything except unavailable)
    pub fn usable(&self) -> usize {
        self.guaranteed + self.best_effort + self.degraded
    }

    /// Total reliable services (guaranteed + best-effort)
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

        // Video capture should be guaranteed on GNOME with portal
        let level = registry.service_level(ServiceId::VideoCapture);
        assert_eq!(level, ServiceLevel::Guaranteed);
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
