//! Core service types
//!
//! Defines the service identifiers, levels, and advertised service structure.

use serde::{Deserialize, Serialize};

use super::{rdp_capabilities::RdpCapability, wayland_features::WaylandFeature};

/// Unique identifier for each known service
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ServiceId {
    /// Damage region tracking for bandwidth optimization
    DamageTracking,

    /// Zero-copy DMA-BUF GPU buffer access
    DmaBufZeroCopy,

    /// Explicit sync for tear-free display
    ExplicitSync,

    /// Fractional scaling support (HiDPI)
    FractionalScaling,

    /// Metadata cursor (client-side rendering)
    MetadataCursor,

    /// Multi-monitor support
    MultiMonitor,

    /// Per-window capture capability
    WindowCapture,

    /// HDR color space passthrough
    HdrColorSpace,

    /// Clipboard integration
    Clipboard,

    /// Clipboard manager detection (Klipper, CopyQ, etc.)
    ClipboardManager,

    /// Remote input injection (keyboard/mouse)
    RemoteInput,

    /// PipeWire video capture
    VideoCapture,

    // === Session Persistence Services ===
    // Added in Phase 2 for unattended operation support
    /// Session persistence capability (portal restore tokens)
    /// Indicates whether permission dialogs can be avoided on reconnect
    SessionPersistence,

    /// Direct compositor API availability (bypasses portal)
    /// Currently only available on GNOME via Mutter D-Bus interfaces
    DirectCompositorAPI,

    /// Secure credential storage capability
    /// Varies by environment: Secret Service, TPM 2.0, or encrypted file
    CredentialStorage,

    /// Unattended access readiness (aggregate capability)
    /// Indicates if server can start without user interaction
    UnattendedAccess,

    /// wlr-screencopy protocol availability (wlroots bypass)
    /// Enables portal-free capture on Sway, Hyprland, Labwc
    WlrScreencopy,

    /// wlr-direct input protocols (virtual keyboard/pointer)
    /// Enables portal-free input injection on wlroots compositors
    WlrDirectInput,

    /// libei/EIS input via Portal RemoteDesktop
    /// Flatpak-compatible wlroots input injection
    LibeiInput,

    // === Authentication Services ===
    // Added for proper auth method detection across deployment contexts
    /// PAM authentication availability
    /// Unavailable in Flatpak (sandboxed), available in native installs
    PamAuthentication,

    /// No-authentication mode
    /// Always available, used when PAM unavailable or for testing
    NoAuthentication,
}

impl ServiceId {
    pub fn name(&self) -> &'static str {
        match self {
            Self::DamageTracking => "Damage Tracking",
            Self::DmaBufZeroCopy => "DMA-BUF Zero-Copy",
            Self::ExplicitSync => "Explicit Sync",
            Self::FractionalScaling => "Fractional Scaling",
            Self::MetadataCursor => "Metadata Cursor",
            Self::MultiMonitor => "Multi-Monitor",
            Self::WindowCapture => "Window Capture",
            Self::HdrColorSpace => "HDR Color Space",
            Self::Clipboard => "Clipboard",
            Self::ClipboardManager => "Clipboard Manager",
            Self::RemoteInput => "Remote Input",
            Self::VideoCapture => "Video Capture",
            // Session persistence services
            Self::SessionPersistence => "Session Persistence",
            Self::DirectCompositorAPI => "Direct Compositor API",
            Self::CredentialStorage => "Credential Storage",
            Self::UnattendedAccess => "Unattended Access",
            Self::WlrScreencopy => "wlr-screencopy",
            Self::WlrDirectInput => "wlr-direct Input",
            Self::LibeiInput => "libei/EIS Input",
            // Authentication services
            Self::PamAuthentication => "PAM Authentication",
            Self::NoAuthentication => "No Authentication",
        }
    }

    pub fn all() -> &'static [ServiceId] {
        &[
            // Video and display services
            Self::DamageTracking,
            Self::DmaBufZeroCopy,
            Self::ExplicitSync,
            Self::FractionalScaling,
            Self::MetadataCursor,
            Self::MultiMonitor,
            Self::WindowCapture,
            Self::HdrColorSpace,
            // I/O services
            Self::Clipboard,
            Self::ClipboardManager,
            Self::RemoteInput,
            Self::VideoCapture,
            // Session persistence services
            Self::SessionPersistence,
            Self::DirectCompositorAPI,
            Self::CredentialStorage,
            Self::UnattendedAccess,
            Self::WlrScreencopy,
            Self::WlrDirectInput,
            Self::LibeiInput,
            // Authentication services
            Self::PamAuthentication,
            Self::NoAuthentication,
        ]
    }
}

impl std::fmt::Display for ServiceId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.name())
    }
}

/// Service availability guarantee level
///
/// Implements `Ord` so levels can be compared:
/// `Guaranteed > BestEffort > Degraded > Unavailable`
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum ServiceLevel {
    /// Feature not available on this compositor
    Unavailable = 0,

    /// Feature detected but known issues exist
    Degraded = 1,

    /// Feature works but may have limitations
    BestEffort = 2,

    /// Feature is fully supported and tested
    Guaranteed = 3,
}

impl ServiceLevel {
    pub fn is_usable(&self) -> bool {
        *self > Self::Unavailable
    }

    pub fn is_reliable(&self) -> bool {
        *self >= Self::BestEffort
    }

    pub fn emoji(&self) -> &'static str {
        match self {
            Self::Guaranteed => "✅",
            Self::BestEffort => "🔶",
            Self::Degraded => "⚠️",
            Self::Unavailable => "❌",
        }
    }
}

impl std::fmt::Display for ServiceLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Guaranteed => write!(f, "Guaranteed"),
            Self::BestEffort => write!(f, "BestEffort"),
            Self::Degraded => write!(f, "Degraded"),
            Self::Unavailable => write!(f, "Unavailable"),
        }
    }
}

/// Performance hints for service optimization
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PerformanceHints {
    /// Recommended maximum FPS for this service
    pub recommended_fps: Option<u32>,

    /// Expected latency overhead in milliseconds
    pub latency_overhead_ms: Option<u32>,

    /// Whether zero-copy path is available
    pub zero_copy_available: bool,

    /// Recommended buffer count for pipelining
    pub buffer_count: Option<u32>,

    /// Whether SIMD acceleration is available
    pub simd_available: bool,
}

impl PerformanceHints {
    pub fn zero_copy() -> Self {
        Self {
            zero_copy_available: true,
            latency_overhead_ms: Some(1),
            buffer_count: Some(2),
            simd_available: true,
            ..Default::default()
        }
    }

    pub fn memcpy() -> Self {
        Self {
            zero_copy_available: false,
            latency_overhead_ms: Some(5),
            buffer_count: Some(3),
            simd_available: true,
            ..Default::default()
        }
    }
}

/// An advertised service with its source, target, and performance hints
#[derive(Debug, Clone)]
pub struct AdvertisedService {
    /// Unique service identifier
    pub id: ServiceId,

    /// Human-readable service name
    pub name: String,

    /// Wayland feature this service is based on
    pub wayland_source: Option<WaylandFeature>,

    /// RDP capability this maps to (if any)
    pub rdp_capability: Option<RdpCapability>,

    /// Service availability level
    pub level: ServiceLevel,

    /// Performance optimization hints
    pub performance: PerformanceHints,

    /// Additional notes about this service
    pub notes: Option<String>,
}

impl AdvertisedService {
    pub fn guaranteed(id: ServiceId, source: WaylandFeature) -> Self {
        Self {
            id,
            name: id.name().to_string(),
            wayland_source: Some(source),
            rdp_capability: None,
            level: ServiceLevel::Guaranteed,
            performance: PerformanceHints::default(),
            notes: None,
        }
    }

    pub fn best_effort(id: ServiceId, source: WaylandFeature) -> Self {
        Self {
            id,
            name: id.name().to_string(),
            wayland_source: Some(source),
            rdp_capability: None,
            level: ServiceLevel::BestEffort,
            performance: PerformanceHints::default(),
            notes: None,
        }
    }

    pub fn degraded(id: ServiceId, source: WaylandFeature, note: &str) -> Self {
        Self {
            id,
            name: id.name().to_string(),
            wayland_source: Some(source),
            rdp_capability: None,
            level: ServiceLevel::Degraded,
            performance: PerformanceHints::default(),
            notes: Some(note.to_string()),
        }
    }

    pub fn unavailable(id: ServiceId) -> Self {
        Self {
            id,
            name: id.name().to_string(),
            wayland_source: None,
            rdp_capability: None,
            level: ServiceLevel::Unavailable,
            performance: PerformanceHints::default(),
            notes: None,
        }
    }

    pub fn with_rdp_capability(mut self, cap: RdpCapability) -> Self {
        self.rdp_capability = Some(cap);
        self
    }

    pub fn with_performance(mut self, hints: PerformanceHints) -> Self {
        self.performance = hints;
        self
    }

    pub fn with_note(mut self, note: &str) -> Self {
        self.notes = Some(note.to_string());
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_service_level_ordering() {
        assert!(ServiceLevel::Guaranteed > ServiceLevel::BestEffort);
        assert!(ServiceLevel::BestEffort > ServiceLevel::Degraded);
        assert!(ServiceLevel::Degraded > ServiceLevel::Unavailable);
    }

    #[test]
    fn test_service_level_usability() {
        assert!(ServiceLevel::Guaranteed.is_usable());
        assert!(ServiceLevel::Degraded.is_usable());
        assert!(!ServiceLevel::Unavailable.is_usable());
    }

    #[test]
    fn test_service_level_reliability() {
        assert!(ServiceLevel::Guaranteed.is_reliable());
        assert!(ServiceLevel::BestEffort.is_reliable());
        assert!(!ServiceLevel::Degraded.is_reliable());
    }

    #[test]
    fn test_service_id_all() {
        let all = ServiceId::all();
        assert!(all.contains(&ServiceId::DamageTracking));
        assert!(all.contains(&ServiceId::MetadataCursor));
    }
}
