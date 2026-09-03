//! Application state management for the lamco-rdp-server GUI
//!
//! Manages configuration state, validation, server status, and UI state.

use std::{
    path::PathBuf,
    time::{Duration, SystemTime},
};

use crate::config::Config;

/// Tab categories for organized navigation
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TabCategory {
    #[default]
    Core,
    System,
    Media,
}

impl TabCategory {
    pub fn all() -> &'static [TabCategory] {
        &[TabCategory::Core, TabCategory::System, TabCategory::Media]
    }

    pub fn display_name(&self) -> &'static str {
        match self {
            TabCategory::Core => "Core",
            TabCategory::System => "System",
            TabCategory::Media => "Media & I/O",
        }
    }

    pub fn icon(&self) -> &'static str {
        match self {
            TabCategory::Core => "🖥",
            TabCategory::System => "⚙",
            TabCategory::Media => "🎬",
        }
    }

    pub fn tabs(&self) -> &'static [Tab] {
        match self {
            TabCategory::Core => &[Tab::Server, Tab::Security],
            TabCategory::System => &[Tab::Performance, Tab::Advanced, Tab::Status],
            TabCategory::Media => &[
                Tab::Video,
                Tab::Display,
                Tab::Egfx,
                Tab::Audio,
                Tab::Input,
                Tab::Clipboard,
            ],
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Tab {
    #[default]
    Server,
    Security,
    Video,
    Display,
    Audio,
    Input,
    Clipboard,
    Performance,
    Egfx,
    Advanced,
    Status,
}

impl Tab {
    pub fn all() -> &'static [Tab] {
        &[
            Tab::Server,
            Tab::Security,
            Tab::Video,
            Tab::Display,
            Tab::Audio,
            Tab::Input,
            Tab::Clipboard,
            Tab::Performance,
            Tab::Egfx,
            Tab::Advanced,
            Tab::Status,
        ]
    }

    pub fn display_name(&self) -> &'static str {
        match self {
            Tab::Server => "Server",
            Tab::Security => "Security",
            Tab::Video => "Video",
            Tab::Display => "Display",
            Tab::Audio => "Audio",
            Tab::Input => "Input",
            Tab::Clipboard => "Clipboard",
            Tab::Performance => "Performance",
            Tab::Egfx => "EGFX",
            Tab::Advanced => "Advanced",
            Tab::Status => "Status",
        }
    }

    pub fn icon(&self) -> &'static str {
        match self {
            Tab::Server => "🖥",
            Tab::Security => "🔒",
            Tab::Video => "🎬",
            Tab::Display => "🪟",
            Tab::Audio => "🔊",
            Tab::Input => "⌨",
            Tab::Clipboard => "📋",
            Tab::Performance => "⚡",
            Tab::Egfx => "🎨",
            Tab::Advanced => "⚙",
            Tab::Status => "📊",
        }
    }

    pub fn category(&self) -> TabCategory {
        match self {
            Tab::Server | Tab::Security => TabCategory::Core,
            Tab::Video | Tab::Display | Tab::Audio | Tab::Input | Tab::Clipboard | Tab::Egfx => {
                TabCategory::Media
            }
            Tab::Performance | Tab::Advanced | Tab::Status => TabCategory::System,
        }
    }
}

/// String buffers for text inputs to avoid iced lifetime issues.
/// Synced with Config on load/save.
#[derive(Debug, Clone, Default)]
pub struct EditStrings {
    // Server tab
    pub server_ip: String,
    pub server_port: String,
    pub max_connections: String,
    pub session_timeout: String,

    // Security tab
    pub cert_path: String,
    pub key_path: String,
    pub valid_days: String,

    // Video tab
    pub vaapi_device: String,
    pub capture_handshake_timeout: String,

    // Clipboard tab
    pub max_size_mb: String,
    pub rate_limit: String,

    // Audio tab
    pub audio_sample_rate: String,
    pub audio_frame_ms: String,
    pub audio_opus_bitrate: String,

    // Logging tab
    pub log_dir: String,

    // Performance tab
    pub buffer_pool_size: String,
    pub network_threads: String,
    pub encoder_threads: String,
    pub quality_delay: String,
    pub balanced_delay: String,
    pub interactive_delay: String,
    pub monitoring_snapshot_interval: String,

    // Video Pipeline
    pub max_frame_age: String,
    pub channel_size: String,
    pub max_queue_depth: String,
    pub converter_buffer_pool_size: String,
    pub frame_ack_timeout: String,
    pub max_frames: String,

    // Egfx tab
    pub h264_bitrate: String,
    pub qp_min: String,
    pub qp_default: String,
    pub qp_max: String,
    pub periodic_idr: String,
    pub aux_threshold_pct: String,
    pub max_aux_interval: String,

    // Advanced tab - Damage Tracking
    pub tile_size: String,
    pub pixel_threshold: String,
    pub merge_distance: String,
    pub min_region_area: String,
    pub hint_distrust_threshold_pp: String,
    pub hint_distrust_consecutive_samples: String,

    // Advanced tab - Display
    pub resolutions_text: String,

    // Advanced tab - Advanced Video
    pub intra_refresh: String,

    // Advanced tab - Cursor
    pub cursor_update_fps: String,
    pub predictive_threshold: String,
    pub history_size: String,
    pub lookahead: String,
    pub max_pred_dist: String,
    pub min_velocity: String,

    // Multimon tab
    pub max_monitors: String,
}

impl EditStrings {
    pub fn from_config(config: &Config) -> Self {
        let (ip, port) = Self::parse_listen_addr(&config.server.listen_addr);
        Self {
            // Server
            server_ip: ip,
            server_port: port,
            max_connections: config.server.max_connections.to_string(),
            session_timeout: config.server.session_timeout.to_string(),

            // Security
            cert_path: config.security.cert_path.display().to_string(),
            key_path: config.security.key_path.display().to_string(),
            valid_days: "365".to_string(),

            // Hardware Encoding
            vaapi_device: config.hardware_encoding.vaapi_device.display().to_string(),
            capture_handshake_timeout: config.capture.handshake_timeout_ms.to_string(),

            // Clipboard (convert bytes to MB for display)
            max_size_mb: (config.clipboard.max_size / (1024 * 1024)).to_string(),
            rate_limit: config.clipboard.rate_limit_ms.to_string(),

            // Audio
            audio_sample_rate: config.audio.sample_rate.to_string(),
            audio_frame_ms: config.audio.frame_ms.to_string(),
            audio_opus_bitrate: (config.audio.opus_bitrate / 1000).to_string(), // Display as kbps

            // Logging
            log_dir: config
                .logging
                .log_dir
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_default(),

            // Performance
            buffer_pool_size: config.performance.buffer_pool_size.to_string(),
            network_threads: config.performance.network_threads.to_string(),
            encoder_threads: config.performance.encoder_threads.to_string(),
            quality_delay: config.performance.latency.quality_max_delay_ms.to_string(),
            balanced_delay: config.performance.latency.balanced_max_delay_ms.to_string(),
            interactive_delay: config
                .performance
                .latency
                .interactive_max_delay_ms
                .to_string(),
            monitoring_snapshot_interval: config.monitoring.snapshot_interval_secs.to_string(),

            // Video Pipeline
            max_frame_age: config
                .video_pipeline
                .dispatcher
                .max_frame_age_ms
                .to_string(),
            channel_size: config.video_pipeline.dispatcher.channel_size.to_string(),
            max_queue_depth: config.video_pipeline.processor.max_queue_depth.to_string(),
            converter_buffer_pool_size: config
                .video_pipeline
                .converter
                .buffer_pool_size
                .to_string(),
            frame_ack_timeout: config.egfx.frame_ack_timeout.to_string(),
            max_frames: config.egfx.max_frames_in_flight.to_string(),

            // Egfx
            h264_bitrate: config.egfx.h264_bitrate.to_string(),
            qp_min: config.egfx.qp_min.to_string(),
            qp_default: config.egfx.qp_default.to_string(),
            qp_max: config.egfx.qp_max.to_string(),
            periodic_idr: config.egfx.periodic_idr_interval.to_string(),
            aux_threshold_pct: format!("{:.0}", config.egfx.avc444_aux_change_threshold * 100.0),
            max_aux_interval: config.egfx.avc444_max_aux_interval.to_string(),

            // Advanced - Damage Tracking
            tile_size: config.damage_tracking.tile_size.to_string(),
            pixel_threshold: config.damage_tracking.pixel_threshold.to_string(),
            merge_distance: config.damage_tracking.merge_distance.to_string(),
            min_region_area: config.damage_tracking.min_region_area.to_string(),
            hint_distrust_threshold_pp: config
                .damage_tracking
                .compositor_hint_distrust_threshold_pp
                .to_string(),
            hint_distrust_consecutive_samples: config
                .damage_tracking
                .compositor_hint_distrust_consecutive_samples
                .to_string(),

            // Advanced - Display
            resolutions_text: config.display.allowed_resolutions.join("\n"),

            // Advanced - Video
            intra_refresh: config.advanced_video.intra_refresh_interval.to_string(),

            // Advanced - Cursor
            cursor_update_fps: config.cursor.cursor_update_fps.to_string(),
            predictive_threshold: config.cursor.predictive_latency_threshold_ms.to_string(),
            history_size: config.cursor.predictor.history_size.to_string(),
            lookahead: format!("{:.1}", config.cursor.predictor.lookahead_ms),
            max_pred_dist: config.cursor.predictor.max_prediction_distance.to_string(),
            min_velocity: format!("{:.1}", config.cursor.predictor.min_velocity_threshold),

            // Multimon
            max_monitors: config.multimon.max_monitors.to_string(),
        }
    }

    fn parse_listen_addr(addr: &str) -> (String, String) {
        if let Ok(sock_addr) = addr.parse::<std::net::SocketAddr>() {
            match sock_addr {
                std::net::SocketAddr::V4(v4) => (v4.ip().to_string(), v4.port().to_string()),
                std::net::SocketAddr::V6(v6) => (format!("[{}]", v6.ip()), v6.port().to_string()),
            }
        } else {
            (addr.to_string(), "3389".to_string())
        }
    }
}

/// Compose a `listen_addr` string from separate IP and port fields, bracketing
/// IPv6 so the result is a valid `SocketAddr`. The dual-stack default `[::]` and
/// bare IPv6 entry both round-trip; IPv4 and partial input are combined as-is so
/// validation can surface a clear error.
#[must_use]
pub(crate) fn compose_listen_addr(ip: &str, port: &str) -> String {
    let ip = ip.trim();
    let port = port.trim();
    let bare = ip
        .strip_prefix('[')
        .and_then(|s| s.strip_suffix(']'))
        .unwrap_or(ip);
    if bare.parse::<std::net::Ipv6Addr>().is_ok() {
        format!("[{bare}]:{port}")
    } else {
        format!("{bare}:{port}")
    }
}

#[cfg(test)]
mod compose_tests {
    use std::net::SocketAddr;

    use super::compose_listen_addr;

    #[test]
    fn compose_brackets_ipv6_and_round_trips() {
        for (ip, port, expected) in [
            ("[::]", "3389", "[::]:3389"), // dual-stack default (bracketed)
            ("::", "3389", "[::]:3389"),   // bare IPv6 gets bracketed
            ("2001:db8::1", "3390", "[2001:db8::1]:3390"),
            ("[2001:db8::1]", "3390", "[2001:db8::1]:3390"),
            ("0.0.0.0", "3389", "0.0.0.0:3389"), // IPv4 unchanged
            ("127.0.0.1", "3389", "127.0.0.1:3389"),
        ] {
            let addr = compose_listen_addr(ip, port);
            assert_eq!(addr, expected, "compose({ip:?}, {port:?})");
            assert!(
                addr.parse::<SocketAddr>().is_ok(),
                "composed address must parse as SocketAddr: {addr}"
            );
        }
    }
}

#[derive(Debug, Clone)]
pub struct AppState {
    // Configuration being edited
    pub config: Config,

    // Edit strings for text inputs
    pub edit_strings: EditStrings,

    // File state
    pub config_path: PathBuf,
    pub is_dirty: bool,
    pub last_saved: Option<SystemTime>,

    // Validation state
    pub validation: ValidationState,

    // Server state (from IPC)
    pub server_status: ServerStatus,

    // Hardware detection
    pub detected_gpus: Vec<GpuInfo>,
    pub detected_vaapi_devices: Vec<PathBuf>,

    // Capabilities (from --show-capabilities)
    pub detected_capabilities: Option<DetectedCapabilities>,

    // UI state
    pub active_preset: Option<String>,
    pub expert_mode: bool,

    // Expanded section states
    pub video_pipeline_expanded: bool,
    pub adaptive_fps_expanded: bool,
    pub latency_expanded: bool,
    pub damage_tracking_expanded: bool,
    pub hardware_encoding_expanded: bool,
    pub display_expanded: bool,
    pub advanced_video_expanded: bool,
    pub cursor_expanded: bool,
    pub cursor_predictor_expanded: bool,
    pub egfx_expert_mode: bool,
    pub multimon_expanded: bool,
    pub logging_expanded: bool,

    // Certificate generation dialog state
    pub cert_gen_dialog: Option<CertGenState>,

    // Log viewer state
    pub log_buffer: Vec<LogLine>,
    pub log_auto_scroll: bool,
    pub log_filter_level: LogLevel,
    pub max_log_lines: usize,

    // User messages (info/warning/error notifications)
    pub messages: Vec<UserMessage>,

    // Whether a FileChooser portal is available (file-browse dialogs work).
    // False on e.g. wlroots compositors that ship only portal-wlr (ScreenCast).
    pub file_dialog_available: bool,

    // Dialog states
    pub confirm_discard_dialog: bool,
    pub pending_action: Option<PendingAction>,

    // First-run certificate setup
    pub first_run_cert_dialog: bool,
    pub first_run_cert_generating: bool,

    // Close behavior: true = closing GUI stops server, false = GUI closes but server keeps running
    pub close_stops_server: bool,

    // Live performance metrics (from D-Bus PerformanceUpdated signal)
    pub live_metrics: Option<LiveMetrics>,

    // Session health (from D-Bus ServerStateChanged signals with health prefixes)
    pub session_health: Option<SessionHealth>,
}

/// Session health state from D-Bus signals
#[derive(Debug, Clone)]
pub struct SessionHealth {
    /// "healthy", "degraded", or "invalid"
    pub overall: String,
    /// Detail message from the server_state_changed signal
    pub detail: String,
    /// Per-subsystem health states (from PerformanceUpdated signal)
    pub video: String,
    pub input: String,
    pub clipboard: String,
    pub session: String,
    pub last_updated: std::time::Instant,
}

/// Live performance metrics from the server, updated periodically
#[derive(Debug, Clone)]
pub struct LiveMetrics {
    pub fps: u32,
    pub latency_ms: f32,
    pub queue_depth: u32,
    pub encoder_backend: String,
    pub activity_level: String,
    /// Encoding adaptation: current QP value (0 = not active)
    pub current_qp: u32,
    /// Whether encoding adaptation is enabled
    pub adaptation_enabled: bool,
    /// Damage detection source: "compositor", "pixel-diff", or ""
    pub damage_source: String,
    /// Number of registered health sensors
    pub sensor_count: u32,
    /// Current encoder bitrate in kbps
    pub bitrate_kbps: u32,
    pub last_updated: std::time::Instant,
}

impl AppState {
    /// Create new state with default or loaded config
    pub fn load_or_default() -> Self {
        let config_path = Self::default_config_path();
        let config = Config::load(config_path.to_str().unwrap_or_default())
            .unwrap_or_else(|_| Config::default_config().unwrap_or_default());
        let edit_strings = EditStrings::from_config(&config);

        // Extract gui_state values before moving config into the struct
        let gui_state = &config.gui_state;
        let expert_mode = gui_state.expert_mode;
        let egfx_expert_mode = gui_state.egfx_expert_mode;
        let video_pipeline_expanded = gui_state.video_pipeline_expanded;
        let adaptive_fps_expanded = gui_state.adaptive_fps_expanded;
        let latency_expanded = gui_state.latency_expanded;
        let damage_tracking_expanded = gui_state.damage_tracking_expanded;
        let hardware_encoding_expanded = gui_state.hardware_encoding_expanded;
        let display_expanded = gui_state.display_expanded;
        let advanced_video_expanded = gui_state.advanced_video_expanded;
        let cursor_expanded = gui_state.cursor_expanded;
        let cursor_predictor_expanded = gui_state.cursor_predictor_expanded;
        let log_auto_scroll = gui_state.log_auto_scroll;
        let log_filter_level = match gui_state.log_filter_level.as_str() {
            "trace" => LogLevel::Trace,
            "debug" => LogLevel::Debug,
            "info" => LogLevel::Info,
            "warn" => LogLevel::Warn,
            "error" => LogLevel::Error,
            _ => LogLevel::Info,
        };
        let close_stops_server = gui_state.close_stops_server;
        let multimon_expanded = gui_state.multimon_expanded;
        let logging_expanded = gui_state.logging_expanded;

        Self {
            config,
            edit_strings,
            config_path,
            is_dirty: false,
            last_saved: None,
            validation: ValidationState::default(),
            server_status: ServerStatus::Unknown,
            detected_gpus: Vec::new(),
            detected_vaapi_devices: Vec::new(),
            detected_capabilities: None,
            active_preset: None,
            expert_mode,
            video_pipeline_expanded,
            adaptive_fps_expanded,
            latency_expanded,
            damage_tracking_expanded,
            hardware_encoding_expanded,
            display_expanded,
            advanced_video_expanded,
            cursor_expanded,
            cursor_predictor_expanded,
            egfx_expert_mode,
            multimon_expanded,
            logging_expanded,
            cert_gen_dialog: None,
            log_buffer: Vec::new(),
            log_auto_scroll,
            log_filter_level,
            max_log_lines: 1000,
            messages: Vec::new(),
            // Assume available until the startup probe reports otherwise.
            file_dialog_available: true,
            confirm_discard_dialog: false,
            pending_action: None,
            first_run_cert_dialog: false,
            first_run_cert_generating: false,
            close_stops_server,
            live_metrics: None,
            session_health: None,
        }
    }

    /// Mark configuration as modified
    pub fn mark_dirty(&mut self) {
        self.is_dirty = true;
        self.active_preset = None; // Clear preset when manually modified
    }

    /// Mark configuration as saved
    pub fn mark_clean(&mut self) {
        self.is_dirty = false;
        self.last_saved = Some(SystemTime::now());
    }

    /// Sync GUI state back to config before saving
    ///
    /// This ensures UI preferences (expanded sections, expert mode, etc.)
    /// are persisted when the config file is saved.
    pub fn sync_gui_state_to_config(&mut self) {
        self.config.gui_state.expert_mode = self.expert_mode;
        self.config.gui_state.egfx_expert_mode = self.egfx_expert_mode;
        self.config.gui_state.video_pipeline_expanded = self.video_pipeline_expanded;
        self.config.gui_state.adaptive_fps_expanded = self.adaptive_fps_expanded;
        self.config.gui_state.latency_expanded = self.latency_expanded;
        self.config.gui_state.damage_tracking_expanded = self.damage_tracking_expanded;
        self.config.gui_state.hardware_encoding_expanded = self.hardware_encoding_expanded;
        self.config.gui_state.display_expanded = self.display_expanded;
        self.config.gui_state.advanced_video_expanded = self.advanced_video_expanded;
        self.config.gui_state.cursor_expanded = self.cursor_expanded;
        self.config.gui_state.cursor_predictor_expanded = self.cursor_predictor_expanded;
        self.config.gui_state.multimon_expanded = self.multimon_expanded;
        self.config.gui_state.logging_expanded = self.logging_expanded;
        self.config.gui_state.log_auto_scroll = self.log_auto_scroll;
        self.config.gui_state.log_filter_level = match self.log_filter_level {
            LogLevel::Trace => "trace".to_string(),
            LogLevel::Debug => "debug".to_string(),
            LogLevel::Info => "info".to_string(),
            LogLevel::Warn => "warn".to_string(),
            LogLevel::Error => "error".to_string(),
        };
        self.config.gui_state.close_stops_server = self.close_stops_server;
    }

    /// Get default config file path
    ///
    /// Location depends on deployment context:
    /// - Flatpak: ~/.var/app/io.lamco.rdp-server/config/config.toml
    /// - Native: ~/.config/lamco-rdp-server/config.toml or /etc/lamco-rdp-server/config.toml
    fn default_config_path() -> PathBuf {
        use crate::config::is_flatpak;

        if is_flatpak() {
            // In Flatpak, only use the sandboxed config directory
            // dirs::config_dir() returns ~/.var/app/<app-id>/config in Flatpak
            return dirs::config_dir()
                .unwrap_or_else(|| PathBuf::from("/app/config"))
                .join("config.toml");
        }

        // Native: Try in order:
        // 1. $XDG_CONFIG_HOME/lamco-rdp-server/config.toml
        // 2. ~/.config/lamco-rdp-server/config.toml
        // 3. /etc/lamco-rdp-server/config.toml
        if let Some(config_dir) = dirs::config_dir() {
            let user_config = config_dir.join("lamco-rdp-server").join("config.toml");
            if user_config.exists() {
                return user_config;
            }
        }

        let etc_config = PathBuf::from("/etc/lamco-rdp-server/config.toml");
        if etc_config.exists() {
            return etc_config;
        }

        // Default to user config location even if doesn't exist
        dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("/etc"))
            .join("lamco-rdp-server")
            .join("config.toml")
    }

    /// Add a user message
    pub fn add_message(&mut self, level: MessageLevel, text: String) {
        self.messages.push(UserMessage {
            level,
            text,
            timestamp: SystemTime::now(),
        });
    }

    /// Add log line to buffer
    pub fn add_log_line(&mut self, line: LogLine) {
        self.log_buffer.push(line);
        if self.log_buffer.len() > self.max_log_lines {
            self.log_buffer.remove(0);
        }
    }

    /// Get filtered log lines based on current filter level
    pub fn filtered_log_lines(&self) -> impl Iterator<Item = &LogLine> {
        let filter_level = self.log_filter_level;
        self.log_buffer
            .iter()
            .filter(move |line| line.level as u8 >= filter_level as u8)
    }
}

#[derive(Debug, Clone, Default)]
pub struct ValidationState {
    pub is_valid: bool,
    pub errors: Vec<ValidationError>,
    pub warnings: Vec<ValidationWarning>,
}

#[derive(Debug, Clone)]
pub struct ValidationError {
    pub field: String,
    pub message: String,
}

#[derive(Debug, Clone)]
pub struct ValidationWarning {
    pub field: String,
    pub message: String,
}

#[derive(Debug, Clone)]
pub struct ValidationResult {
    pub is_valid: bool,
    pub errors: Vec<ValidationError>,
    pub warnings: Vec<ValidationWarning>,
}

impl From<ValidationResult> for ValidationState {
    fn from(result: ValidationResult) -> Self {
        Self {
            is_valid: result.is_valid,
            errors: result.errors,
            warnings: result.warnings,
        }
    }
}

/// Server status from IPC
#[derive(Debug, Clone, PartialEq, Default)]
pub enum ServerStatus {
    #[default]
    Unknown,
    Stopped,
    Starting,
    Running {
        connections: usize,
        uptime: Duration,
        address: String,
    },
    Error(String),
}

impl ServerStatus {
    /// Check if server is running
    pub fn is_running(&self) -> bool {
        matches!(self, ServerStatus::Running { .. })
    }

    /// Get status display text
    pub fn display_text(&self) -> &str {
        match self {
            ServerStatus::Unknown => "Unknown",
            ServerStatus::Stopped => "Stopped",
            ServerStatus::Starting => "Starting...",
            ServerStatus::Running { .. } => "Running",
            ServerStatus::Error(_) => "Error",
        }
    }
}

/// GPU information for display in the UI
#[derive(Debug, Clone)]
pub struct GpuInfo {
    pub vendor: String,
    pub model: String,
    pub driver: String,
    pub vaapi_device: Option<PathBuf>,
    pub nvenc_available: bool,
    pub supports_h264: bool,
}

/// Detected system capabilities
#[derive(Debug, Clone)]
pub struct DetectedCapabilities {
    // System information
    pub compositor_name: String,
    pub compositor_version: Option<String>,
    pub distribution: String,
    pub kernel_version: String,

    // Portal information
    pub portal_version: u32,
    pub portal_backend: String,
    pub screencast_version: Option<u32>,
    pub remote_desktop_version: Option<u32>,
    pub secret_portal_version: Option<u32>,

    // Deployment
    pub deployment_context: DeploymentContext,
    pub xdg_runtime_dir: PathBuf,

    // Platform quirks
    pub quirks: Vec<PlatformQuirk>,

    // Session persistence
    pub persistence_strategy: String,
    pub persistence_notes: Vec<String>,

    // Service Registry (18 services)
    pub services: Vec<ServiceInfo>,

    // Counts
    pub guaranteed_count: usize,
    pub best_effort_count: usize,
    pub degraded_count: usize,
    pub unavailable_count: usize,

    // Performance hints
    pub recommended_fps: Option<u32>,
    pub recommended_codec: Option<String>,
    pub zero_copy_available: bool,

    // Authentication (derived from services)
    /// Available authentication methods based on deployment context
    /// Derived from PamAuthentication/NoAuthentication service levels
    pub available_auth_methods: Vec<String>,

    // Timestamp
    pub detected_at: SystemTime,
}

/// Service information from registry
#[derive(Debug, Clone)]
pub struct ServiceInfo {
    pub id: String,
    pub name: String,
    pub level: ServiceLevel,
    pub level_emoji: String,
    pub wayland_source: Option<String>,
    pub rdp_capability: Option<String>,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceLevel {
    Guaranteed,
    BestEffort,
    Degraded,
    Unavailable,
}

impl std::fmt::Display for ServiceLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ServiceLevel::Guaranteed => write!(f, "Guaranteed"),
            ServiceLevel::BestEffort => write!(f, "BestEffort"),
            ServiceLevel::Degraded => write!(f, "Degraded"),
            ServiceLevel::Unavailable => write!(f, "Unavailable"),
        }
    }
}

impl ServiceLevel {
    pub fn emoji(&self) -> &'static str {
        match self {
            ServiceLevel::Guaranteed => "✅",
            ServiceLevel::BestEffort => "🔶",
            ServiceLevel::Degraded => "⚠️",
            ServiceLevel::Unavailable => "❌",
        }
    }
}

#[derive(Debug, Clone)]
pub struct PlatformQuirk {
    pub quirk_id: String,
    pub description: String,
    pub impact: String,
}

#[derive(Debug, Clone)]
pub enum DeploymentContext {
    Native,
    Flatpak,
    SystemdUser { linger: bool },
    SystemdSystem,
    InitD,
    Unknown,
}

impl std::fmt::Display for DeploymentContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DeploymentContext::Native => write!(f, "Native"),
            DeploymentContext::Flatpak => write!(f, "Flatpak"),
            DeploymentContext::SystemdUser { linger } => {
                write!(f, "systemd-user (linger: {linger})")
            }
            DeploymentContext::SystemdSystem => write!(f, "systemd-system"),
            DeploymentContext::InitD => write!(f, "init.d"),
            DeploymentContext::Unknown => write!(f, "Unknown"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct CertGenState {
    pub common_name: String,
    pub organization: String,
    pub valid_days: u32,
    pub valid_days_str: String,
    pub generating: bool,
}

impl Default for CertGenState {
    fn default() -> Self {
        Self {
            common_name: "localhost".to_string(),
            organization: "My Organization".to_string(),
            valid_days: 365,
            valid_days_str: "365".to_string(),
            generating: false,
        }
    }
}

/// Log line from server output
#[derive(Debug, Clone)]
pub struct LogLine {
    pub timestamp: String,
    pub level: LogLevel,
    pub message: String,
    pub raw: String,
}

impl LogLine {
    pub fn parse(raw: &str) -> Self {
        // Try to parse: "2026-01-19 14:23:45 [INFO] Server listening..."
        let parts: Vec<&str> = raw.splitn(4, ' ').collect();

        let timestamp = if parts.len() >= 2 {
            format!("{} {}", parts[0], parts[1])
        } else {
            "??:??:??".to_string()
        };

        let level = if parts.len() >= 3 {
            match parts[2]
                .trim_matches(|c| c == '[' || c == ']')
                .to_uppercase()
                .as_str()
            {
                "TRACE" => LogLevel::Trace,
                "DEBUG" => LogLevel::Debug,
                "INFO" => LogLevel::Info,
                "WARN" | "WARNING" => LogLevel::Warn,
                "ERROR" => LogLevel::Error,
                _ => LogLevel::Info,
            }
        } else {
            LogLevel::Info
        };

        let message = parts.get(3).unwrap_or(&"").to_string();

        Self {
            timestamp,
            level,
            message,
            raw: raw.to_string(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum LogLevel {
    Trace = 0,
    Debug = 1,
    Info = 2,
    Warn = 3,
    Error = 4,
}

impl std::fmt::Display for LogLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LogLevel::Trace => write!(f, "TRACE"),
            LogLevel::Debug => write!(f, "DEBUG"),
            LogLevel::Info => write!(f, "INFO"),
            LogLevel::Warn => write!(f, "WARN"),
            LogLevel::Error => write!(f, "ERROR"),
        }
    }
}

impl LogLevel {
    pub fn all() -> &'static [LogLevel] {
        &[
            LogLevel::Trace,
            LogLevel::Debug,
            LogLevel::Info,
            LogLevel::Warn,
            LogLevel::Error,
        ]
    }
}

#[derive(Debug, Clone)]
pub struct UserMessage {
    pub level: MessageLevel,
    pub text: String,
    pub timestamp: SystemTime,
}

#[derive(Debug, Clone, Copy)]
pub enum MessageLevel {
    Info,
    Warning,
    Error,
    Success,
}

/// Tracks unsaved-changes prompts: what the user was trying to do.
#[derive(Debug, Clone)]
pub enum PendingAction {
    CloseWindow,
    SwitchTab(Tab),
    LoadConfig(PathBuf),
}
