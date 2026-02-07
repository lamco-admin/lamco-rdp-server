//! Clipboard Integration Strategy Framework
//!
//! Defines strategies for clipboard integration based on compositor and
//! clipboard manager detection. Implements layered adaptive approach with
//! fallbacks for robustness.
//!
//! # Strategy Tiers (from Strategic Decision)
//!
//! - **Tier 1 (Prevention)**: Use x-kde-syncselection to prevent Klipper processing
//! - **Tier 2 (Cooperation)**: Work WITH Klipper via bidirectional D-Bus sync ⭐ PRIMARY
//! - **Tier 3 (Defensive)**: Detect Klipper takeover and re-announce
//! - **Tier 4 (Workaround)**: Document manual Klipper disable steps
//!
//! # Strategy Selection
//!
//! Automatic selection based on:
//! - Compositor type (GNOME, KDE, Sway, COSMIC)
//! - Clipboard manager detected (Klipper, CopyQ, None)
//! - Deployment mode (Flatpak vs Native)
//! - Protocol availability (Portal, ext-data-control-v1)

use tracing::{debug, info};

use crate::services::{ServiceId, ServiceRegistry};

/// Clipboard integration mode
///
/// Determines how we interact with the system clipboard based on
/// detected environment (compositor, clipboard manager, deployment mode).
///
/// More specific than "Strategy" - describes the integration approach.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClipboardIntegrationMode {
    /// Direct Portal API (no special handling)
    ///
    /// **When used**: No clipboard manager interference expected
    /// **Compositors**: GNOME, Sway (no wl-clipboard), COSMIC
    /// **Behavior**: Normal SetSelection/SelectionOwnerChanged flow
    PortalDirect,

    /// Klipper Cooperation Mode (Tier 2) ⭐ PRIMARY for KDE
    ///
    /// **When used**: KDE Plasma with Klipper detected
    /// **Strategy**: Work WITH Klipper, not against it
    /// **Behavior**:
    /// - Accept that Klipper will take ownership (~1s after SetSelection)
    /// - Monitor clipboardHistoryUpdated signal via D-Bus
    /// - When Klipper updates, read via getClipboardContents()
    /// - Sync Klipper's content back to RDP client
    /// - Bidirectional: Windows ↔ Klipper ↔ Linux apps
    ///
    /// **Benefits**:
    /// - Aligns with Klipper's design (persistence is intentional)
    /// - Robust across versions (no protocol tricks)
    /// - User-friendly (Klipper features continue working)
    /// - Expected success: 90-95%
    KlipperCooperationMode {
        /// Enable Tier 1 prevention (syncselection hint)
        /// If true, adds x-kde-syncselection to prevent Klipper takeover
        /// If false or fails, falls back to cooperation
        use_prevention_tier1: bool,

        /// Enable Tier 3 fallback (re-announce on takeover)
        /// If cooperation fails, detect takeover and re-announce
        use_defensive_tier3: bool,

        /// Maximum reannouncements per copy (Tier 3 loop prevention)
        max_reannounce_per_copy: u32,
    },

    /// Portal with detection (no specific mitigation)
    ///
    /// **When used**: Unknown clipboard manager detected (CopyQ, Diodon, etc.)
    /// **Behavior**: Conservative, watch for ownership changes, no specific mitigation
    PortalWithDetection,

    /// Wayland data-control protocol (Future: v1.4.0+)
    ///
    /// **When used**: Native mode with data-control available
    /// **Benefits**: Full control, lower latency, immediate Klipper reclaim
    /// **Status**: 🚧 UNIMPLEMENTED - Planned for v1.4.0+
    WaylandDataControlMode {
        /// Fall back to Portal if direct mode fails
        fallback_to_portal: bool,

        /// Enable Klipper-specific cooperation in direct mode
        klipper_cooperation: bool,
    },
}

impl ClipboardIntegrationMode {
    /// Select strategy based on environment detection
    pub fn select(
        registry: &ServiceRegistry,
        config: &crate::config::types::ClipboardConfig,
        in_flatpak: bool,
    ) -> Self {
        info!("═══════════════════════════════════════════════════════════════");
        info!("  Clipboard Strategy Selection");
        info!("═══════════════════════════════════════════════════════════════");

        if let Some(ref override_str) = config.strategy_override {
            if let Some(strategy) = Self::from_override_string(override_str) {
                info!("  Strategy: MANUAL OVERRIDE");
                info!("  Selected: {:?}", strategy);
                info!("  ⚠️  User override active - automatic selection bypassed");
                info!("═══════════════════════════════════════════════════════════════");
                return strategy;
            } else {
                tracing::warn!(
                    "Invalid strategy override '{}', using automatic selection",
                    override_str
                );
            }
        }

        let caps = registry.compositor_capabilities();
        let manager_info = caps.clipboard_manager.as_ref();

        info!(
            "  Deployment: {}",
            if in_flatpak { "Flatpak" } else { "Native" }
        );
        info!("  Compositor: {:?}", caps.compositor);

        let strategy = match manager_info {
            Some(info) => {
                info!("  Clipboard Manager: {:?}", info.manager_type);

                match &info.manager_type {
                    crate::services::clipboard_manager::SystemClipboardManagerKind::Klipper {
                        ..
                    } => {
                        // KDE Plasma with Klipper detected
                        info!("  ┌─────────────────────────────────────────────────");
                        info!("  │ Klipper detected - using Cooperation strategy");
                        info!("  │ Tier 2: Work WITH Klipper via bidirectional sync");
                        info!(
                            "  │ Tier 1: Prevention via syncselection = {}",
                            config.kde_syncselection_hint
                        );
                        info!("  │ Tier 3: Re-announce fallback = enabled");
                        info!("  └─────────────────────────────────────────────────");

                        if in_flatpak {
                            // Flatpak MUST use Portal
                            Self::KlipperCooperationMode {
                                use_prevention_tier1: config.kde_syncselection_hint,
                                use_defensive_tier3: true,
                                max_reannounce_per_copy: 2,
                            }
                        } else {
                            // Native: check if direct data-control available (future)
                            // TODO(v1.4.0): Add ServiceId::ClipboardDataControl when implemented
                            // if registry.has_service(ServiceId::ClipboardDataControl) {
                            //     info!("  Note: Direct data-control available but not yet implemented");
                            //     info!("        Using Portal with cooperation for now");
                            // }

                            // For now, use Portal even in native mode
                            // TODO(v1.4.0): Implement DirectDataControl
                            Self::KlipperCooperationMode {
                                use_prevention_tier1: config.kde_syncselection_hint,
                                use_defensive_tier3: true,
                                max_reannounce_per_copy: 2,
                            }
                        }
                    }

                    crate::services::clipboard_manager::SystemClipboardManagerKind::CopyQ {
                        ..
                    }
                    | crate::services::clipboard_manager::SystemClipboardManagerKind::Diodon => {
                        info!("  Third-party clipboard manager detected");
                        info!("  Using conservative manager detection mode");
                        Self::PortalWithDetection
                    }

                    crate::services::clipboard_manager::SystemClipboardManagerKind::GnomeShell
                    | crate::services::clipboard_manager::SystemClipboardManagerKind::None => {
                        info!("  No clipboard manager interference expected");
                        info!("  Using standard Portal strategy");
                        Self::PortalDirect
                    }

                    crate::services::clipboard_manager::SystemClipboardManagerKind::WlClipboard
                    | crate::services::clipboard_manager::SystemClipboardManagerKind::Unknown => {
                        info!("  Unknown/wl-clipboard detected");
                        info!("  Using conservative standard strategy");
                        Self::PortalDirect
                    }
                }
            }

            None => {
                info!("  No clipboard manager info available");
                info!("  Using standard Portal strategy");
                Self::PortalDirect
            }
        };

        info!("  ─────────────────────────────────────────────────");
        info!("  Final Strategy: {:?}", strategy);
        info!("═══════════════════════════════════════════════════════════════");

        strategy
    }

    /// Parse strategy from manual override string
    fn from_override_string(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "portal-standard" => Some(Self::PortalDirect),
            "portal-klipper-cooperation" => Some(Self::KlipperCooperationMode {
                use_prevention_tier1: false,
                use_defensive_tier3: true,
                max_reannounce_per_copy: 2,
            }),
            "portal-with-manager" => Some(Self::PortalWithDetection),
            "direct-data-control" => Some(Self::WaylandDataControlMode {
                fallback_to_portal: true,
                klipper_cooperation: true,
            }),
            _ => None,
        }
    }

    /// Get human-readable strategy name
    pub fn name(&self) -> &'static str {
        match self {
            Self::PortalDirect => "Portal Standard",
            Self::KlipperCooperationMode { .. } => "Portal + Klipper Cooperation",
            Self::PortalWithDetection => "Portal + Manager Detection",
            Self::WaylandDataControlMode { .. } => "Direct Data-Control",
        }
    }

    /// Check if this strategy uses Klipper cooperation
    pub fn uses_klipper_cooperation(&self) -> bool {
        matches!(
            self,
            Self::KlipperCooperationMode { .. }
                | Self::WaylandDataControlMode {
                    klipper_cooperation: true,
                    ..
                }
        )
    }

    /// Check if this strategy should use prevention (Tier 1)
    pub fn uses_prevention(&self) -> bool {
        matches!(
            self,
            Self::KlipperCooperationMode {
                use_prevention_tier1: true,
                ..
            }
        )
    }

    /// Check if this strategy should use defensive re-announce (Tier 3)
    pub fn uses_defensive_reannounce(&self) -> bool {
        matches!(
            self,
            Self::KlipperCooperationMode {
                use_defensive_tier3: true,
                ..
            }
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_strategy_name() {
        let strategy = ClipboardIntegrationMode::PortalDirect;
        assert_eq!(strategy.name(), "Portal Standard");

        let strategy = ClipboardIntegrationMode::KlipperCooperationMode {
            use_prevention_tier1: false,
            use_defensive_tier3: true,
            max_reannounce_per_copy: 2,
        };
        assert_eq!(strategy.name(), "Portal + Klipper Cooperation");
    }

    #[test]
    fn test_uses_klipper_cooperation() {
        let strategy = ClipboardIntegrationMode::PortalDirect;
        assert!(!strategy.uses_klipper_cooperation());

        let strategy = ClipboardIntegrationMode::KlipperCooperationMode {
            use_prevention_tier1: false,
            use_defensive_tier3: true,
            max_reannounce_per_copy: 2,
        };
        assert!(strategy.uses_klipper_cooperation());
    }

    #[test]
    fn test_from_override_string() {
        assert!(matches!(
            ClipboardIntegrationMode::from_override_string("portal-standard"),
            Some(ClipboardIntegrationMode::PortalDirect)
        ));

        assert!(matches!(
            ClipboardIntegrationMode::from_override_string("portal-klipper-cooperation"),
            Some(ClipboardIntegrationMode::KlipperCooperationMode { .. })
        ));

        assert!(ClipboardIntegrationMode::from_override_string("invalid").is_none());
    }
}
