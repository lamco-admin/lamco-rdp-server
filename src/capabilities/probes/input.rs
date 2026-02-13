//! Input capability probe
//!
//! Detects available input injection methods.

use std::path::Path;

use serde::{Deserialize, Serialize};
use tracing::{debug, info};

use crate::capabilities::{fallback::AttemptResult, state::ServiceLevel};

/// Input capabilities
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InputCapabilities {
    /// Available input strategies
    pub strategies: Vec<InputStrategy>,
    /// Selected strategy
    pub selected: Option<InputStrategy>,
    /// Deployment type
    pub deployment: DeploymentType,
    /// Overall service level
    pub service_level: ServiceLevel,
    /// Fallback chain attempts
    pub fallback_chain: Vec<AttemptResult>,
    /// Reason for unavailability
    pub unavailable_reason: Option<String>,
}

/// An input injection strategy
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InputStrategy {
    /// Type of strategy
    pub strategy_type: InputStrategyType,
    /// Service level this strategy provides
    pub service_level: ServiceLevel,
}

/// Type of input injection strategy
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum InputStrategyType {
    /// Mutter direct D-Bus API (GNOME, no dialogs)
    MutterApi,
    /// wlr-virtual-keyboard/pointer (wlroots native)
    WlrDirect,
    /// libei for EIS/EI input (Flatpak compatible)
    LibEi,
    /// Portal with restore token (minimal dialogs)
    PortalToken,
    /// Portal fallback (dialog each session)
    Portal,
}

impl InputStrategyType {
    pub fn name(&self) -> &str {
        match self {
            Self::MutterApi => "Mutter D-Bus API",
            Self::WlrDirect => "wlr-virtual-keyboard/pointer",
            Self::LibEi => "libei (EIS/EI)",
            Self::PortalToken => "Portal with restore token",
            Self::Portal => "Portal",
        }
    }
}

/// Deployment type detection
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DeploymentType {
    /// Native package installation
    Native,
    /// Flatpak sandbox
    Flatpak,
    /// Systemd user service
    SystemdUser,
    /// Systemd system service
    SystemdSystem,
    /// Container (Docker, Podman)
    Container,
    /// Unknown deployment
    Unknown,
}

/// Input probe
pub struct InputProbe;

impl InputProbe {
    pub async fn probe() -> InputCapabilities {
        info!("Probing input capabilities...");

        let deployment = Self::detect_deployment();
        debug!("Deployment type: {:?}", deployment);

        let mut strategies = Vec::new();
        let mut fallback_chain = Vec::new();

        match deployment {
            DeploymentType::Flatpak => {
                let libei_available = Self::check_libei();
                if libei_available {
                    strategies.push(InputStrategy {
                        strategy_type: InputStrategyType::LibEi,
                        service_level: ServiceLevel::Full,
                    });
                    fallback_chain.push(AttemptResult::success("libei", std::time::Duration::ZERO));
                } else {
                    fallback_chain.push(AttemptResult::failure(
                        "libei",
                        "libei feature not available",
                        std::time::Duration::ZERO,
                    ));
                }

                // Portal always available in Flatpak
                strategies.push(InputStrategy {
                    strategy_type: InputStrategyType::Portal,
                    service_level: ServiceLevel::Fallback,
                });
                fallback_chain.push(AttemptResult::success("Portal", std::time::Duration::ZERO));
            }
            DeploymentType::Native | DeploymentType::SystemdUser => {
                let compositor = Self::detect_compositor_type();

                if compositor == "mutter" {
                    strategies.push(InputStrategy {
                        strategy_type: InputStrategyType::MutterApi,
                        service_level: ServiceLevel::Full,
                    });
                    fallback_chain.push(AttemptResult::success(
                        "Mutter API",
                        std::time::Duration::ZERO,
                    ));
                } else if compositor == "wlroots" {
                    strategies.push(InputStrategy {
                        strategy_type: InputStrategyType::WlrDirect,
                        service_level: ServiceLevel::Full,
                    });
                    fallback_chain.push(AttemptResult::success(
                        "wlr-direct",
                        std::time::Duration::ZERO,
                    ));
                }

                if Self::check_libei() {
                    strategies.push(InputStrategy {
                        strategy_type: InputStrategyType::LibEi,
                        service_level: ServiceLevel::Degraded,
                    });
                    fallback_chain.push(AttemptResult::success("libei", std::time::Duration::ZERO));
                }

                strategies.push(InputStrategy {
                    strategy_type: InputStrategyType::Portal,
                    service_level: ServiceLevel::Fallback,
                });
                fallback_chain.push(AttemptResult::success("Portal", std::time::Duration::ZERO));
            }
            _ => {
                strategies.push(InputStrategy {
                    strategy_type: InputStrategyType::Portal,
                    service_level: ServiceLevel::Fallback,
                });
                fallback_chain.push(AttemptResult::success("Portal", std::time::Duration::ZERO));
            }
        }

        let selected = strategies.first().cloned();
        let service_level = selected
            .as_ref()
            .map_or(ServiceLevel::Unavailable, |s| s.service_level);

        let unavailable_reason = if strategies.is_empty() {
            Some("No input injection method available".into())
        } else {
            None
        };

        info!(
            "Input service level: {:?}, strategy: {:?}",
            service_level, selected
        );

        InputCapabilities {
            strategies,
            selected,
            deployment,
            service_level,
            fallback_chain,
            unavailable_reason,
        }
    }

    fn detect_deployment() -> DeploymentType {
        if std::env::var("FLATPAK_ID").is_ok() {
            return DeploymentType::Flatpak;
        }

        if Path::new("/.dockerenv").exists() {
            return DeploymentType::Container;
        }
        if let Ok(cgroup) = std::fs::read_to_string("/proc/1/cgroup") {
            if cgroup.contains("docker")
                || cgroup.contains("podman")
                || cgroup.contains("containerd")
            {
                return DeploymentType::Container;
            }
        }

        // INVOCATION_ID is set by systemd for service units
        if std::env::var("INVOCATION_ID").is_ok() {
            if std::env::var("USER").map(|u| u == "root").unwrap_or(false) {
                return DeploymentType::SystemdSystem;
            }
            return DeploymentType::SystemdUser;
        }

        DeploymentType::Native
    }

    fn detect_compositor_type() -> &'static str {
        let desktop = std::env::var("XDG_CURRENT_DESKTOP")
            .unwrap_or_default()
            .to_lowercase();

        if desktop.contains("gnome") {
            "mutter"
        } else if desktop.contains("sway")
            || desktop.contains("hyprland")
            || desktop.contains("wlroots")
        {
            "wlroots"
        } else if desktop.contains("kde") || desktop.contains("plasma") {
            "kwin"
        } else {
            "unknown"
        }
    }

    fn check_libei() -> bool {
        #[cfg(feature = "libei")]
        {
            true
        }
        #[cfg(not(feature = "libei"))]
        {
            false
        }
    }
}
