//! Diagnostic reporting for capability detection
//!
//! Provides detailed diagnostic reports for troubleshooting capability issues.

use serde::{Deserialize, Serialize};

use crate::capabilities::{
    BlockingIssue, Degradation, ServiceLevel, Subsystem, SystemCapabilities,
};

/// Comprehensive diagnostic report
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiagnosticReport {
    /// Overall system health
    pub health: SystemHealth,
    /// Per-subsystem reports
    pub subsystems: Vec<SubsystemReport>,
    /// All degradations
    pub degradations: Vec<Degradation>,
    /// All blocking issues
    pub blocking_issues: Vec<BlockingIssue>,
    /// Recommendations
    pub recommendations: Vec<Recommendation>,
}

/// Overall system health status
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SystemHealth {
    /// All systems operational at full capacity
    Healthy,
    /// Some degradations but fully functional
    Degraded,
    /// Limited functionality available
    Limited,
    /// System cannot operate
    Critical,
}

impl SystemHealth {
    pub fn emoji(&self) -> &'static str {
        match self {
            Self::Healthy => "💚",
            Self::Degraded => "💛",
            Self::Limited => "🟠",
            Self::Critical => "🔴",
        }
    }
}

/// Report for a single subsystem
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubsystemReport {
    /// Which subsystem
    pub subsystem: Subsystem,
    /// Service level
    pub service_level: ServiceLevel,
    /// Detailed status
    pub status: String,
    /// What's working
    pub working: Vec<String>,
    /// What's not working
    pub not_working: Vec<String>,
}

/// A recommendation for improving capability
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Recommendation {
    /// Priority (higher = more important)
    pub priority: u8,
    /// Which subsystem this affects
    pub subsystem: Subsystem,
    /// What to do
    pub action: String,
    /// Expected improvement
    pub benefit: String,
}

pub fn run_diagnostics(caps: &SystemCapabilities) -> DiagnosticReport {
    let mut subsystems = Vec::new();
    let mut recommendations = Vec::new();

    let display_report = SubsystemReport {
        subsystem: Subsystem::Display,
        service_level: caps.display.service_level,
        status: format!(
            "Compositor: {}, Portal v{}",
            caps.display.compositor.name, caps.display.portal.version
        ),
        working: {
            let mut w = Vec::new();
            if caps.display.portal.supports_screencast {
                w.push("Screen capture".into());
            }
            if caps.display.portal.supports_remote_desktop {
                w.push("Remote desktop".into());
            }
            if caps.display.portal.supports_clipboard {
                w.push("Clipboard".into());
            }
            w
        },
        not_working: {
            let mut nw = Vec::new();
            if !caps.display.portal.supports_clipboard {
                nw.push("Clipboard (requires portal v4+)".into());
            }
            if !caps.display.portal.supports_restore_tokens {
                nw.push("Session restore tokens".into());
            }
            nw
        },
    };
    subsystems.push(display_report);

    if !caps.display.portal.supports_clipboard {
        recommendations.push(Recommendation {
            priority: 3,
            subsystem: Subsystem::Display,
            action: "Upgrade to GNOME 44+ or KDE 5.27+ for clipboard support".into(),
            benefit: "Enable bidirectional clipboard sync".into(),
        });
    }

    let encoding_report = SubsystemReport {
        subsystem: Subsystem::Encoding,
        service_level: caps.encoding.service_level,
        status: if let Some(selected) = &caps.encoding.selected {
            format!("Using: {}", selected.backend_type.name())
        } else {
            "No encoder available".into()
        },
        working: caps
            .encoding
            .backends
            .iter()
            .map(|b| format!("{} ({})", b.backend_type.name(), b.service_level.name()))
            .collect(),
        not_working: caps
            .encoding
            .fallback_chain
            .iter()
            .filter(|a| !a.success)
            .map(|a| {
                format!(
                    "{}: {}",
                    a.strategy_name,
                    a.error.as_deref().unwrap_or("unknown")
                )
            })
            .collect(),
    };
    subsystems.push(encoding_report);

    if !caps.encoding.hardware_available {
        recommendations.push(Recommendation {
            priority: 2,
            subsystem: Subsystem::Encoding,
            action: "Install VA-API or NVENC drivers for hardware encoding".into(),
            benefit: "Reduce CPU usage and improve quality".into(),
        });
    }

    let input_report = SubsystemReport {
        subsystem: Subsystem::Input,
        service_level: caps.input.service_level,
        status: if let Some(selected) = &caps.input.selected {
            format!("Using: {:?}", selected.strategy_type)
        } else {
            "No input method available".into()
        },
        working: caps
            .input
            .strategies
            .iter()
            .map(|s| format!("{:?}", s.strategy_type))
            .collect(),
        not_working: Vec::new(),
    };
    subsystems.push(input_report);

    let storage_report = SubsystemReport {
        subsystem: Subsystem::Storage,
        service_level: caps.storage.service_level,
        status: if let Some(selected) = &caps.storage.selected {
            format!("Using: {}", selected.name())
        } else {
            "No storage available".into()
        },
        working: caps
            .storage
            .backends
            .iter()
            .map(|b| b.name().to_string())
            .collect(),
        not_working: Vec::new(),
    };
    subsystems.push(storage_report);

    let rendering_report = SubsystemReport {
        subsystem: Subsystem::Rendering,
        service_level: caps.rendering.service_level,
        status: match &caps.rendering.recommendation {
            crate::capabilities::RenderingRecommendation::UseGpu { reason } => {
                format!("GPU: {reason}")
            }
            crate::capabilities::RenderingRecommendation::UseSoftware { reason } => {
                format!("Software: {reason}")
            }
            crate::capabilities::RenderingRecommendation::NoGui { reason, .. } => {
                format!("Unavailable: {reason}")
            }
        },
        working: {
            let mut w = Vec::new();
            if caps.rendering.gpu_available {
                w.push("GPU detected".into());
            }
            if caps.rendering.software_available {
                w.push("Software rendering".into());
            }
            if caps.rendering.wgpu_supported {
                w.push("wgpu compatible".into());
            }
            w
        },
        not_working: {
            let mut nw = Vec::new();
            if !caps.rendering.gpu_available {
                nw.push("No GPU".into());
            }
            if caps.rendering.gpu_available && !caps.rendering.wgpu_supported {
                nw.push("GPU incompatible with wgpu".into());
            }
            nw
        },
    };
    subsystems.push(rendering_report);

    if caps.rendering.service_level == ServiceLevel::Fallback
        && caps.rendering.virtualization.is_some()
    {
        recommendations.push(Recommendation {
            priority: 1,
            subsystem: Subsystem::Rendering,
            action: "Enable GPU passthrough in VM settings".into(),
            benefit: "Better GUI performance".into(),
        });
    }

    let network_report = SubsystemReport {
        subsystem: Subsystem::Network,
        service_level: caps.network.service_level,
        status: if caps.network.tls_available {
            "TLS available".into()
        } else {
            "TLS unavailable".into()
        },
        working: if caps.network.tls_available {
            vec!["TLS/SSL".into()]
        } else {
            Vec::new()
        },
        not_working: Vec::new(),
    };
    subsystems.push(network_report);

    let health = if caps.blocking_issues.is_empty() && caps.degradations.is_empty() {
        SystemHealth::Healthy
    } else if caps.blocking_issues.is_empty() {
        SystemHealth::Degraded
    } else if caps.minimum_viable.can_operate_server() {
        SystemHealth::Limited
    } else {
        SystemHealth::Critical
    };

    recommendations.sort_by(|a, b| b.priority.cmp(&a.priority));

    DiagnosticReport {
        health,
        subsystems,
        degradations: caps.degradations.clone(),
        blocking_issues: caps.blocking_issues.clone(),
        recommendations,
    }
}

impl DiagnosticReport {
    pub fn format_text(&self) -> String {
        let mut output = String::new();

        output.push_str(&format!(
            "\n{} System Health: {:?}\n",
            self.health.emoji(),
            self.health
        ));
        output.push_str("\n=== Subsystem Status ===\n\n");

        for report in &self.subsystems {
            output.push_str(&format!(
                "{} {} - {}\n",
                report.subsystem.emoji(),
                report.subsystem,
                report.service_level
            ));
            output.push_str(&format!("   Status: {}\n", report.status));

            if !report.working.is_empty() {
                output.push_str("   Working:\n");
                for item in &report.working {
                    output.push_str(&format!("     ✅ {item}\n"));
                }
            }

            if !report.not_working.is_empty() {
                output.push_str("   Not working:\n");
                for item in &report.not_working {
                    output.push_str(&format!("     ❌ {item}\n"));
                }
            }
            output.push('\n');
        }

        if !self.degradations.is_empty() {
            output.push_str("=== Active Degradations ===\n\n");
            for deg in &self.degradations {
                output.push_str(&format!(
                    "  {} {} - {}\n",
                    deg.user_impact.emoji(),
                    deg.feature,
                    deg.reason
                ));
                output.push_str(&format!("    Fallback: {}\n", deg.fallback));
            }
            output.push('\n');
        }

        if !self.blocking_issues.is_empty() {
            output.push_str("=== Blocking Issues ===\n\n");
            for issue in &self.blocking_issues {
                output.push_str(&format!("  ❌ {} - {}\n", issue.issue, issue.reason));
                output.push_str("    Suggestions:\n");
                for sug in &issue.suggestions {
                    output.push_str(&format!("      • {sug}\n"));
                }
            }
            output.push('\n');
        }

        if !self.recommendations.is_empty() {
            output.push_str("=== Recommendations ===\n\n");
            for rec in &self.recommendations {
                output.push_str(&format!(
                    "  [{}] {} {}\n",
                    rec.priority,
                    rec.subsystem.emoji(),
                    rec.action
                ));
                output.push_str(&format!("      Benefit: {}\n", rec.benefit));
            }
        }

        output
    }
}
