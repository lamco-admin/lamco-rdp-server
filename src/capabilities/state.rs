//! Capability state definitions
//!
//! This module defines the data structures used to represent detected capabilities
//! across all subsystems.

use std::fmt;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::capabilities::probes::{
    DisplayCapabilities, EncodingCapabilities, InputCapabilities, NetworkCapabilities,
    RenderingCapabilities, StorageCapabilities,
};

/// Overall system capability state
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemCapabilities {
    /// When capabilities were probed
    pub probed_at: DateTime<Utc>,

    /// Display/compositor capabilities
    pub display: DisplayCapabilities,

    /// Video encoding capabilities
    pub encoding: EncodingCapabilities,

    /// Input injection capabilities
    pub input: InputCapabilities,

    /// Credential storage capabilities
    pub storage: StorageCapabilities,

    /// GUI rendering capabilities
    pub rendering: RenderingCapabilities,

    /// Network/TLS capabilities
    pub network: NetworkCapabilities,

    /// Computed minimum viable configuration
    pub minimum_viable: MinimumViableConfig,

    /// Active degradations (non-fatal capability reductions)
    pub degradations: Vec<Degradation>,

    /// Blocking issues (fatal, prevent operation)
    pub blocking_issues: Vec<BlockingIssue>,
}

/// Service availability levels
///
/// These levels represent the quality/performance tier at which a capability
/// is available. Higher values indicate better service.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, Default)]
pub enum ServiceLevel {
    /// Feature fully available with all capabilities
    Full = 4,
    /// Feature available but with reduced capabilities
    Degraded = 3,
    /// Feature available via fallback mechanism
    Fallback = 2,
    /// Feature explicitly disabled by configuration
    Disabled = 1,
    /// Feature unavailable due to missing requirements
    #[default]
    Unavailable = 0,
}

impl ServiceLevel {
    pub fn is_operational(&self) -> bool {
        matches!(self, Self::Full | Self::Degraded | Self::Fallback)
    }

    pub fn emoji(&self) -> &'static str {
        match self {
            Self::Full => "✅",
            Self::Degraded => "⚠️",
            Self::Fallback => "🔄",
            Self::Disabled => "⛔",
            Self::Unavailable => "❌",
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            Self::Full => "Full",
            Self::Degraded => "Degraded",
            Self::Fallback => "Fallback",
            Self::Disabled => "Disabled",
            Self::Unavailable => "Unavailable",
        }
    }
}

impl fmt::Display for ServiceLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {}", self.emoji(), self.name())
    }
}

/// Subsystem identifier
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Subsystem {
    /// Screen capture capabilities
    Display,
    /// Video encoding capabilities
    Encoding,
    /// Input injection capabilities
    Input,
    /// Credential storage capabilities
    Storage,
    /// GUI rendering capabilities
    Rendering,
    /// Network/TLS capabilities
    Network,
    /// Authentication/security capabilities
    Security,
}

impl Subsystem {
    pub fn emoji(&self) -> &'static str {
        match self {
            Self::Display => "🖥️",
            Self::Encoding => "🎬",
            Self::Input => "⌨️",
            Self::Storage => "🔐",
            Self::Rendering => "🎨",
            Self::Network => "🌐",
            Self::Security => "🛡️",
        }
    }
}

impl fmt::Display for Subsystem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}

/// Represents a non-fatal capability reduction
///
/// Degradations indicate that a feature is working but at reduced capacity.
/// The system can continue operating but some functionality may be limited.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Degradation {
    /// Which subsystem is affected
    pub subsystem: Subsystem,
    /// Which feature is degraded
    pub feature: String,
    /// Why it's degraded
    pub reason: String,
    /// What fallback is being used
    pub fallback: String,
    /// Impact on user experience
    pub user_impact: UserImpact,
}

/// User-facing impact level of a degradation
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum UserImpact {
    /// User won't notice any difference
    Minimal,
    /// Slight reduction in quality/performance
    Minor,
    /// Noticeable reduction in functionality
    Moderate,
    /// Significant functionality missing
    Major,
}

impl UserImpact {
    pub fn emoji(&self) -> &'static str {
        match self {
            Self::Minimal => "ℹ️",
            Self::Minor => "⚠️",
            Self::Moderate => "🟠",
            Self::Major => "🔴",
        }
    }
}

impl fmt::Display for UserImpact {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}

/// Represents a fatal issue that prevents operation
///
/// Blocking issues indicate that the system cannot function in some way.
/// These require user intervention to resolve.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockingIssue {
    /// Which subsystem has the issue
    pub subsystem: Subsystem,
    /// What the issue is
    pub issue: String,
    /// Why it's an issue
    pub reason: String,
    /// How to fix it
    pub suggestions: Vec<String>,
}

/// Minimum configuration needed to operate
///
/// This represents the bare minimum capabilities required for different
/// operational modes.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MinimumViableConfig {
    /// Can we capture the screen?
    pub can_capture_screen: bool,
    /// Can we encode video?
    pub can_encode_video: bool,
    /// Can we inject input?
    pub can_inject_input: bool,
    /// Can we serve RDP connections?
    pub can_serve_rdp: bool,
    /// Can we render the GUI?
    pub can_render_gui: bool,
}

impl MinimumViableConfig {
    pub fn can_operate_server(&self) -> bool {
        self.can_capture_screen && self.can_encode_video && self.can_serve_rdp
    }

    pub fn can_operate_gui(&self) -> bool {
        self.can_render_gui
    }

    pub fn summary(&self) -> String {
        let mut parts = Vec::new();
        if self.can_capture_screen {
            parts.push("capture");
        }
        if self.can_encode_video {
            parts.push("encode");
        }
        if self.can_inject_input {
            parts.push("input");
        }
        if self.can_serve_rdp {
            parts.push("rdp");
        }
        if self.can_render_gui {
            parts.push("gui");
        }
        if parts.is_empty() {
            "nothing available".to_string()
        } else {
            parts.join(", ")
        }
    }
}
