//! GUI Message types for the lamco-rdp-server configuration GUI
//!
//! All user interactions and async events are represented as messages.

use std::path::PathBuf;

use crate::config::Config;
use crate::gui::state::{DetectedCapabilities, GpuInfo, ServerStatus, Tab, ValidationResult};

/// Main application message type
#[derive(Debug, Clone)]
pub enum Message {
    // =========================================================================
    // Tab Navigation
    // =========================================================================
    /// Switch to a different tab
    TabSelected(Tab),

    // =========================================================================
    // Server Configuration (4 fields)
    // =========================================================================
    /// Listen address IP changed
    ServerListenAddrChanged(String),
    /// Listen address port changed
    ServerPortChanged(String),
    /// Maximum connections changed
    ServerMaxConnectionsChanged(String),
    /// Session timeout changed
    ServerSessionTimeoutChanged(String),
    /// Use XDG Portals toggled
    ServerUsePortalsToggled(bool),

    // =========================================================================
    // Security Configuration (5 fields)
    // =========================================================================
    /// Certificate path changed
    SecurityCertPathChanged(String),
    /// Browse for certificate file
    SecurityBrowseCert,
    /// Certificate file selected
    SecurityCertSelected(Option<PathBuf>),
    /// Key path changed
    SecurityKeyPathChanged(String),
    /// Browse for key file
    SecurityBrowseKey,
    /// Key file selected
    SecurityKeySelected(Option<PathBuf>),
    /// Generate self-signed certificate requested
    SecurityGenerateCert,
    /// Certificate generation dialog - common name changed
    CertGenCommonNameChanged(String),
    /// Certificate generation dialog - organization changed
    CertGenOrganizationChanged(String),
    /// Certificate generation dialog - valid days changed
    CertGenValidDaysChanged(String),
    /// Certificate generation confirmed
    CertGenConfirm,
    /// Certificate generation cancelled
    CertGenCancel,
    /// Certificate generation completed
    CertGenCompleted(Result<(), String>),
    /// Enable NLA toggled
    SecurityEnableNlaToggled(bool),
    /// Auth method changed
    SecurityAuthMethodChanged(String),
    /// Require TLS 1.3 toggled
    SecurityRequireTls13Toggled(bool),

    // =========================================================================
    // Video Configuration (6 fields)
    // =========================================================================
    /// Video encoder changed
    VideoEncoderChanged(String),
    /// VA-API device changed
    VideoVaapiDeviceChanged(String),
    /// Target FPS changed (slider)
    VideoTargetFpsChanged(u32),
    /// Bitrate changed (slider)
    VideoBitrateChanged(u32),
    /// Damage tracking toggled
    VideoDamageTrackingToggled(bool),
    /// Cursor mode changed
    VideoCursorModeChanged(String),
    /// Detect GPUs button clicked
    VideoDetectGpus,
    /// GPUs detected
    VideoGpusDetected(Vec<GpuInfo>),

    // =========================================================================
    // Video Pipeline Configuration (16 fields across 3 sub-structs)
    // =========================================================================
    /// Toggle video pipeline section expanded
    VideoPipelineToggleExpanded,

    // Processor config
    ProcessorTargetFpsChanged(String),
    ProcessorMaxQueueDepthChanged(String),
    ProcessorAdaptiveQualityToggled(bool),
    ProcessorDamageThresholdChanged(f32),
    ProcessorDropOnFullQueueToggled(bool),
    ProcessorEnableMetricsToggled(bool),

    // Dispatcher config
    DispatcherChannelSizeChanged(String),
    DispatcherPriorityDispatchToggled(bool),
    DispatcherMaxFrameAgeChanged(String),
    DispatcherEnableBackpressureToggled(bool),
    DispatcherHighWaterMarkChanged(f32),
    DispatcherLowWaterMarkChanged(f32),
    DispatcherLoadBalancingToggled(bool),

    // Converter config
    ConverterBufferPoolSizeChanged(String),
    ConverterEnableSimdToggled(bool),
    ConverterDamageThresholdChanged(f32),
    ConverterEnableStatisticsToggled(bool),

    // =========================================================================
    // Input Configuration (3 fields)
    // =========================================================================
    /// Use libei toggled
    InputUseLibeiToggled(bool),
    /// Keyboard layout changed
    InputKeyboardLayoutChanged(String),
    /// Enable touch toggled
    InputEnableTouchToggled(bool),

    // =========================================================================
    // Clipboard Configuration (4 fields)
    // =========================================================================
    /// Clipboard enabled toggled
    ClipboardEnabledToggled(bool),
    /// Max clipboard size changed
    ClipboardMaxSizeChanged(String),
    /// Rate limit changed
    ClipboardRateLimitChanged(String),
    /// Allowed MIME types changed (newline-separated text)
    ClipboardAllowedTypesChanged(String),
    /// Clipboard preset selected
    ClipboardPresetSelected(ClipboardPreset),

    // =========================================================================
    // Multi-Monitor Configuration (2 fields)
    // =========================================================================
    /// Multi-monitor enabled toggled
    MultimonEnabledToggled(bool),
    /// Max monitors changed
    MultimonMaxMonitorsChanged(String),

    // =========================================================================
    // Performance Configuration (6 fields + 2 sub-structs = 18 fields total)
    // =========================================================================
    /// Performance preset selected
    PerformancePresetSelected(PerformancePreset),

    // Base performance
    PerformanceEncoderThreadsChanged(String),
    PerformanceNetworkThreadsChanged(String),
    PerformanceBufferPoolSizeChanged(String),
    PerformanceZeroCopyToggled(bool),

    // Adaptive FPS section toggle
    PerformanceAdaptiveFpsToggleExpanded,
    AdaptiveFpsEnabledToggled(bool),
    AdaptiveFpsMinFpsChanged(u32),
    AdaptiveFpsMaxFpsChanged(u32),
    AdaptiveFpsHighActivityChanged(f32),
    AdaptiveFpsMediumActivityChanged(f32),
    AdaptiveFpsLowActivityChanged(f32),

    // Latency section toggle
    PerformanceLatencyToggleExpanded,
    LatencyModeChanged(String),
    LatencyInteractiveDelayChanged(String),
    LatencyBalancedDelayChanged(String),
    LatencyQualityDelayChanged(String),
    LatencyBalancedThresholdChanged(f32),
    LatencyQualityThresholdChanged(f32),

    // =========================================================================
    // Logging Configuration (3 fields)
    // =========================================================================
    /// Log level changed
    LoggingLevelChanged(String),
    /// Log directory changed
    LoggingLogDirChanged(String),
    /// Browse for log directory
    LoggingBrowseLogDir,
    /// Log directory selected
    LoggingLogDirSelected(Option<PathBuf>),
    /// Metrics enabled toggled
    LoggingMetricsToggled(bool),
    /// Clear log directory (set to None)
    LoggingClearLogDir,

    // =========================================================================
    // EGFX Configuration (23 fields)
    // =========================================================================
    /// EGFX enabled toggled
    EgfxEnabledToggled(bool),
    /// EGFX quality preset selected
    EgfxPresetSelected(EgfxPreset),
    /// Toggle expert mode
    EgfxToggleExpertMode,

    // Basic EGFX
    EgfxH264LevelChanged(String),
    EgfxH264BitrateChanged(String),
    EgfxZgfxCompressionChanged(String),
    EgfxMaxFramesInFlightChanged(String),
    EgfxFrameAckTimeoutChanged(String),
    EgfxPeriodicIdrIntervalChanged(String),
    EgfxCodecChanged(String),

    // Quality parameters
    EgfxQpMinChanged(String),
    EgfxQpMaxChanged(String),
    EgfxQpDefaultChanged(String),

    // AVC444 configuration
    EgfxAvc444EnabledToggled(bool),
    EgfxAvc444AuxBitrateRatioChanged(f32),
    EgfxColorMatrixChanged(String),
    EgfxColorRangeChanged(String),

    // AVC444 Aux Omission
    EgfxAvc444EnableAuxOmissionToggled(bool),
    EgfxAvc444MaxAuxIntervalChanged(String),
    EgfxAvc444AuxChangeThresholdChanged(f32),
    EgfxAvc444ForceAuxIdrToggled(bool),

    // =========================================================================
    // Damage Tracking Configuration (7 fields)
    // =========================================================================
    /// Toggle damage tracking section expanded
    DamageTrackingToggleExpanded,
    /// Damage tracking preset selected
    DamageTrackingPresetSelected(DamageTrackingPreset),

    DamageTrackingEnabledToggled(bool),
    DamageTrackingMethodChanged(String),
    DamageTrackingTileSizeChanged(String),
    DamageTrackingDiffThresholdChanged(f32),
    DamageTrackingPixelThresholdChanged(String),
    DamageTrackingMergeDistanceChanged(String),
    DamageTrackingMinRegionAreaChanged(String),

    // =========================================================================
    // Hardware Encoding Configuration (6 fields)
    // =========================================================================
    /// Toggle hardware encoding section expanded
    HardwareEncodingToggleExpanded,

    HardwareEncodingEnabledToggled(bool),
    HardwareEncodingVaapiDeviceChanged(String),
    HardwareEncodingDmabufZerocopyToggled(bool),
    HardwareEncodingFallbackToSoftwareToggled(bool),
    HardwareEncodingQualityPresetChanged(String),
    HardwareEncodingPreferNvencToggled(bool),

    // =========================================================================
    // Display Configuration (4 fields)
    // =========================================================================
    /// Toggle display section expanded
    DisplayToggleExpanded,

    DisplayAllowResizeToggled(bool),
    DisplayAllowedResolutionsChanged(String),
    DisplayDpiAwareToggled(bool),
    DisplayAllowRotationToggled(bool),

    // =========================================================================
    // Advanced Video Configuration (4 fields)
    // =========================================================================
    /// Toggle advanced video section expanded
    AdvancedVideoToggleExpanded,

    AdvancedVideoEnableFrameSkipToggled(bool),
    AdvancedVideoSceneChangeThresholdChanged(f32),
    AdvancedVideoIntraRefreshIntervalChanged(String),
    AdvancedVideoEnableAdaptiveQualityToggled(bool),

    // =========================================================================
    // Cursor Configuration (5 fields + sub-struct)
    // =========================================================================
    /// Toggle cursor section expanded
    CursorToggleExpanded,
    /// Toggle predictor sub-section expanded
    CursorPredictorToggleExpanded,

    CursorModeChanged(String),
    CursorAutoModeToggled(bool),
    CursorPredictiveThresholdChanged(String),
    CursorUpdateFpsChanged(String),

    // Predictor config
    PredictorHistorySizeChanged(String),
    PredictorLookaheadMsChanged(String),
    PredictorVelocitySmoothingChanged(f32),
    PredictorAccelerationSmoothingChanged(f32),
    PredictorMaxPredictionDistanceChanged(String),
    PredictorMinVelocityThresholdChanged(String),
    PredictorStopConvergenceRateChanged(f32),

    // =========================================================================
    // File Operations
    // =========================================================================
    /// Load configuration from file
    LoadConfig,
    /// Browse for config file
    BrowseConfigFile,
    /// Config file selected
    ConfigFileSelected(Option<PathBuf>),
    /// Configuration loaded
    ConfigLoaded(Result<Config, String>),
    /// Save configuration
    SaveConfig,
    /// Save configuration as
    SaveConfigAs,
    /// Configuration saved
    ConfigSaved(Result<(), String>),

    // =========================================================================
    // Server Control
    // =========================================================================
    /// Start server
    StartServer,
    /// Stop server
    StopServer,
    /// Restart server
    RestartServer,
    /// Server status updated (from IPC)
    ServerStatusUpdated(ServerStatus),
    /// Server process started (contains PID)
    ServerStarted(u32),
    /// Server process exited
    ServerExited(Option<i32>),
    /// Server log line received
    ServerLogReceived(String, crate::gui::state::LogLevel),
    /// Server start failed
    ServerStartFailed(String),

    // =========================================================================
    // Validation
    // =========================================================================
    /// Validate current configuration
    ValidateConfig,
    /// Validation completed
    ValidationComplete(ValidationResult),

    // =========================================================================
    // Capabilities & Service Registry
    // =========================================================================
    /// Refresh capability detection
    RefreshCapabilities,
    /// Capabilities detected
    CapabilitiesDetected(Result<DetectedCapabilities, String>),
    /// Export capabilities to file
    ExportCapabilities,
    /// Capabilities exported
    CapabilitiesExported(Result<PathBuf, String>),

    // =========================================================================
    // Log Viewer
    // =========================================================================
    /// New log line received
    LogLineReceived(String),
    /// Clear log buffer
    ClearLogs,
    /// Toggle log auto-scroll
    ToggleLogAutoScroll,
    /// Log filter level changed
    LogFilterLevelChanged(String),
    /// Export logs to file
    ExportLogs,

    // =========================================================================
    // UI State
    // =========================================================================
    /// Show info message
    ShowInfo(String),
    /// Show warning message
    ShowWarning(String),
    /// Show error message
    ShowError(String),
    /// Dismiss message
    DismissMessage(usize),
    /// Toggle expert mode globally
    ToggleExpertMode,
    /// Window close requested
    WindowCloseRequested,
    /// Confirm discard changes dialog
    ConfirmDiscardChanges,
    /// Cancel discard changes dialog
    CancelDiscardChanges,

    // =========================================================================
    // Tick / Async Updates
    // =========================================================================
    /// Periodic tick for updates (e.g., log tail, status poll)
    Tick,
    /// Poll server logs from background process
    PollServerLogs,
}

/// Clipboard preset configurations
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClipboardPreset {
    /// Text only: text/plain, text/html, text/uri-list
    TextOnly,
    /// Text and images: + image/png, image/jpeg
    TextAndImages,
    /// All types (empty allowed_types = all)
    All,
}

impl ClipboardPreset {
    /// Convert preset to MIME type list
    pub fn to_mime_types(self) -> Vec<String> {
        match self {
            Self::TextOnly => vec![
                "text/plain".to_string(),
                "text/html".to_string(),
                "text/uri-list".to_string(),
            ],
            Self::TextAndImages => vec![
                "text/plain".to_string(),
                "text/html".to_string(),
                "text/uri-list".to_string(),
                "image/png".to_string(),
                "image/jpeg".to_string(),
            ],
            Self::All => vec![], // Empty = all types allowed
        }
    }
}

/// Performance preset configurations
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PerformancePreset {
    /// Interactive: <50ms latency, 60fps, gaming/CAD
    Interactive,
    /// Balanced: <100ms latency, 30fps, general desktop
    Balanced,
    /// Quality: <300ms latency, best quality
    Quality,
}

impl std::fmt::Display for PerformancePreset {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Interactive => write!(f, "Interactive"),
            Self::Balanced => write!(f, "Balanced"),
            Self::Quality => write!(f, "Quality"),
        }
    }
}

/// EGFX quality preset configurations
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EgfxPreset {
    /// Speed: 3Mbps, fast encoding
    Speed,
    /// Balanced: 5Mbps, good quality
    Balanced,
    /// Quality: 10Mbps, best quality
    Quality,
}

impl std::fmt::Display for EgfxPreset {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Speed => write!(f, "Speed"),
            Self::Balanced => write!(f, "Balanced"),
            Self::Quality => write!(f, "Quality"),
        }
    }
}

/// Damage tracking sensitivity presets
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DamageTrackingPreset {
    /// Text work: Maximum sensitivity for typing detection
    TextWork,
    /// General: Balanced sensitivity
    General,
    /// Video: Less sensitive, prioritize bandwidth
    Video,
}

impl std::fmt::Display for DamageTrackingPreset {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TextWork => write!(f, "Text Work"),
            Self::General => write!(f, "General"),
            Self::Video => write!(f, "Video"),
        }
    }
}
