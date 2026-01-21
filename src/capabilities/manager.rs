//! Capability manager singleton
//!
//! Provides centralized capability state management with singleton access pattern.

use chrono::Utc;
use std::sync::{Arc, OnceLock};

use thiserror::Error;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

use crate::capabilities::probes::{
    DisplayProbe, EncodingProbe, InputProbe, NetworkProbe, RenderingProbe, StorageProbe,
};
use crate::capabilities::state::{
    BlockingIssue, Degradation, MinimumViableConfig, ServiceLevel, Subsystem, SystemCapabilities,
    UserImpact,
};

static INSTANCE: OnceLock<Arc<RwLock<CapabilityManager>>> = OnceLock::new();

/// Error during capability manager initialization
#[derive(Debug, Error)]
pub enum InitializationError {
    /// Manager already initialized
    #[error("Capability manager already initialized")]
    AlreadyInitialized,

    /// Probe failed during initialization
    #[error("Probe failed: {0}")]
    ProbeFailed(String),
}

/// Central capability manager
///
/// Manages the lifecycle of capability detection and provides access to
/// current system capabilities.
pub struct CapabilityManager {
    /// Current capability state
    pub state: SystemCapabilities,

    /// Whether initialization completed successfully
    initialized: bool,
}

impl CapabilityManager {
    /// Initialize the global capability manager
    ///
    /// This should be called once at startup before any capability queries.
    /// It probes all subsystems to detect available capabilities.
    ///
    /// # Errors
    ///
    /// Returns an error if already initialized or if probing fails critically.
    pub async fn initialize() -> Result<(), InitializationError> {
        info!("Initializing capability manager...");

        let manager = Self::probe_all().await?;

        // Log summary
        info!("{}", manager.diagnostic_summary());

        INSTANCE
            .set(Arc::new(RwLock::new(manager)))
            .map_err(|_| InitializationError::AlreadyInitialized)?;

        info!("Capability manager initialized successfully");
        Ok(())
    }

    /// Get reference to the global manager
    ///
    /// # Panics
    ///
    /// Panics if called before `initialize()`. Always call `initialize()` at
    /// program startup.
    pub fn global() -> Arc<RwLock<CapabilityManager>> {
        INSTANCE
            .get()
            .expect("CapabilityManager::initialize() must be called first")
            .clone()
    }

    /// Check if the manager has been initialized
    pub fn is_initialized() -> bool {
        INSTANCE.get().is_some()
    }

    /// Probe all subsystems
    async fn probe_all() -> Result<Self, InitializationError> {
        debug!("Probing display capabilities...");
        let display = DisplayProbe::probe().await;

        debug!("Probing encoding capabilities...");
        let encoding = EncodingProbe::probe().await;

        debug!("Probing input capabilities...");
        let input = InputProbe::probe().await;

        debug!("Probing storage capabilities...");
        let storage = StorageProbe::probe().await;

        debug!("Probing rendering capabilities...");
        let rendering = RenderingProbe::probe().await;

        debug!("Probing network capabilities...");
        let network = NetworkProbe::probe().await;

        // Compute derived state
        let minimum_viable = Self::compute_minimum_viable(&display, &encoding, &input, &rendering);
        let degradations =
            Self::collect_degradations(&display, &encoding, &input, &storage, &rendering);
        let blocking_issues =
            Self::collect_blocking_issues(&display, &encoding, &input, &rendering);

        let state = SystemCapabilities {
            probed_at: Utc::now(),
            display,
            encoding,
            input,
            storage,
            rendering,
            network,
            minimum_viable,
            degradations,
            blocking_issues,
        };

        Ok(Self {
            state,
            initialized: true,
        })
    }

    /// Re-probe a specific subsystem
    ///
    /// Useful when conditions may have changed (e.g., after installing drivers).
    pub async fn reprobe(&mut self, subsystem: Subsystem) {
        info!("Re-probing {:?} subsystem...", subsystem);

        match subsystem {
            Subsystem::Display => self.state.display = DisplayProbe::probe().await,
            Subsystem::Encoding => self.state.encoding = EncodingProbe::probe().await,
            Subsystem::Input => self.state.input = InputProbe::probe().await,
            Subsystem::Storage => self.state.storage = StorageProbe::probe().await,
            Subsystem::Rendering => self.state.rendering = RenderingProbe::probe().await,
            Subsystem::Network => self.state.network = NetworkProbe::probe().await,
        }

        self.recompute_derived_state();
        self.state.probed_at = Utc::now();
    }

    /// Recompute derived state after a reprobe
    fn recompute_derived_state(&mut self) {
        self.state.minimum_viable = Self::compute_minimum_viable(
            &self.state.display,
            &self.state.encoding,
            &self.state.input,
            &self.state.rendering,
        );
        self.state.degradations = Self::collect_degradations(
            &self.state.display,
            &self.state.encoding,
            &self.state.input,
            &self.state.storage,
            &self.state.rendering,
        );
        self.state.blocking_issues = Self::collect_blocking_issues(
            &self.state.display,
            &self.state.encoding,
            &self.state.input,
            &self.state.rendering,
        );
    }

    fn compute_minimum_viable(
        display: &crate::capabilities::probes::DisplayCapabilities,
        encoding: &crate::capabilities::probes::EncodingCapabilities,
        input: &crate::capabilities::probes::InputCapabilities,
        rendering: &crate::capabilities::probes::RenderingCapabilities,
    ) -> MinimumViableConfig {
        MinimumViableConfig {
            can_capture_screen: display.service_level.is_operational(),
            can_encode_video: encoding.service_level.is_operational(),
            can_inject_input: input.service_level.is_operational(),
            can_serve_rdp: display.service_level.is_operational()
                && encoding.service_level.is_operational(),
            can_render_gui: rendering.service_level.is_operational(),
        }
    }

    fn collect_degradations(
        display: &crate::capabilities::probes::DisplayCapabilities,
        encoding: &crate::capabilities::probes::EncodingCapabilities,
        input: &crate::capabilities::probes::InputCapabilities,
        storage: &crate::capabilities::probes::StorageCapabilities,
        rendering: &crate::capabilities::probes::RenderingCapabilities,
    ) -> Vec<Degradation> {
        let mut degradations = Vec::new();

        // Display degradations
        if display.service_level == ServiceLevel::Degraded {
            degradations.push(Degradation {
                subsystem: Subsystem::Display,
                feature: "Screen capture".into(),
                reason: display.degradation_reason.clone().unwrap_or_default(),
                fallback: "Using fallback capture method".into(),
                user_impact: UserImpact::Minor,
            });
        }

        // Encoding degradations
        if encoding.service_level == ServiceLevel::Fallback {
            degradations.push(Degradation {
                subsystem: Subsystem::Encoding,
                feature: "Video encoding".into(),
                reason: "No hardware encoder available".into(),
                fallback: "Using software encoding (OpenH264)".into(),
                user_impact: UserImpact::Moderate,
            });
        }

        // Clipboard degradation (Portal v1)
        if !display.portal.supports_clipboard {
            degradations.push(Degradation {
                subsystem: Subsystem::Display,
                feature: "Clipboard synchronization".into(),
                reason: "Portal version does not support clipboard".into(),
                fallback: "Clipboard disabled".into(),
                user_impact: UserImpact::Moderate,
            });
        }

        // Input degradation
        if input.service_level == ServiceLevel::Fallback {
            degradations.push(Degradation {
                subsystem: Subsystem::Input,
                feature: "Input injection".into(),
                reason: "Using Portal fallback".into(),
                fallback: "Permission dialog on each start".into(),
                user_impact: UserImpact::Minor,
            });
        }

        // Rendering degradation
        if rendering.service_level == ServiceLevel::Fallback {
            degradations.push(Degradation {
                subsystem: Subsystem::Rendering,
                feature: "GUI rendering".into(),
                reason: rendering.fallback_reason.clone().unwrap_or_default(),
                fallback: "Using software rendering".into(),
                user_impact: UserImpact::Minor,
            });
        }

        // Storage degradation
        if storage.service_level == ServiceLevel::Fallback {
            degradations.push(Degradation {
                subsystem: Subsystem::Storage,
                feature: "Credential storage".into(),
                reason: "Secure storage not available".into(),
                fallback: "Using encrypted file storage".into(),
                user_impact: UserImpact::Minimal,
            });
        }

        degradations
    }

    fn collect_blocking_issues(
        display: &crate::capabilities::probes::DisplayCapabilities,
        encoding: &crate::capabilities::probes::EncodingCapabilities,
        input: &crate::capabilities::probes::InputCapabilities,
        rendering: &crate::capabilities::probes::RenderingCapabilities,
    ) -> Vec<BlockingIssue> {
        let mut issues = Vec::new();

        // No display capture
        if display.service_level == ServiceLevel::Unavailable {
            issues.push(BlockingIssue {
                subsystem: Subsystem::Display,
                issue: "Cannot capture screen".into(),
                reason: display.unavailable_reason.clone().unwrap_or("Unknown".into()),
                suggestions: vec![
                    "Ensure a Wayland compositor is running".into(),
                    "Check that XDG Desktop Portal is installed".into(),
                    "Verify portal services are running: systemctl --user status xdg-desktop-portal".into(),
                ],
            });
        }

        // No video encoding
        if encoding.service_level == ServiceLevel::Unavailable {
            issues.push(BlockingIssue {
                subsystem: Subsystem::Encoding,
                issue: "Cannot encode video".into(),
                reason: "No encoder backends available".into(),
                suggestions: vec![
                    "Install VA-API drivers for your GPU".into(),
                    "For NVIDIA: install nvidia-vaapi-driver".into(),
                    "Ensure h264 feature is compiled in".into(),
                ],
            });
        }

        // No input
        if input.service_level == ServiceLevel::Unavailable {
            issues.push(BlockingIssue {
                subsystem: Subsystem::Input,
                issue: "Cannot inject input".into(),
                reason: input.unavailable_reason.clone().unwrap_or("Unknown".into()),
                suggestions: vec![
                    "Ensure XDG Desktop Portal is running".into(),
                    "Check portal RemoteDesktop interface availability".into(),
                ],
            });
        }

        issues
    }

    /// Generate a diagnostic summary
    pub fn diagnostic_summary(&self) -> String {
        let mut summary = String::new();

        summary.push_str("╭───────────────────────────────────────────╮\n");
        summary.push_str("│         Capability Summary                │\n");
        summary.push_str("├───────────────────────────────────────────┤\n");
        summary.push_str(&format!(
            "│  🖥️  Display:   {:12} │\n",
            self.state.display.service_level.name()
        ));
        summary.push_str(&format!(
            "│  🎬 Encoding:  {:12} │\n",
            self.state.encoding.service_level.name()
        ));
        summary.push_str(&format!(
            "│  ⌨️  Input:     {:12} │\n",
            self.state.input.service_level.name()
        ));
        summary.push_str(&format!(
            "│  🔐 Storage:   {:12} │\n",
            self.state.storage.service_level.name()
        ));
        summary.push_str(&format!(
            "│  🎨 Rendering: {:12} │\n",
            self.state.rendering.service_level.name()
        ));
        summary.push_str(&format!(
            "│  🌐 Network:   {:12} │\n",
            self.state.network.service_level.name()
        ));
        summary.push_str("├───────────────────────────────────────────┤\n");

        if !self.state.degradations.is_empty() {
            summary.push_str(&format!(
                "│  ⚠️  {} degradation(s) active               │\n",
                self.state.degradations.len()
            ));
        }

        if !self.state.blocking_issues.is_empty() {
            summary.push_str(&format!(
                "│  ❌ {} blocking issue(s)                   │\n",
                self.state.blocking_issues.len()
            ));
        }

        let can_server = if self.state.minimum_viable.can_operate_server() {
            "✅"
        } else {
            "❌"
        };
        let can_gui = if self.state.minimum_viable.can_operate_gui() {
            "✅"
        } else {
            "❌"
        };
        summary.push_str(&format!(
            "│  Server: {} GUI: {}                       │\n",
            can_server, can_gui
        ));
        summary.push_str("╰───────────────────────────────────────────╯");

        summary
    }

    /// Get JSON representation of capabilities
    pub fn to_json(&self) -> serde_json::Result<String> {
        serde_json::to_string_pretty(&self.state)
    }
}
