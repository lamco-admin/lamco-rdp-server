//! Main iced Application implementation for lamco-rdp-server-gui
//!
//! Implements the Elm Architecture pattern: State -> View -> Message -> Update -> State

use std::{path::PathBuf, sync::Arc, time::Duration};

use iced::{
    Alignment, Element, Length, Subscription, Task,
    widget::{button, column, container, image, row, scrollable, text},
};
use tracing::debug;

use crate::gui::widgets::space;

/// Lamb head logo icon (48x48 PNG)
static LOGO_ICON: &[u8] = include_bytes!("../../data/icons/io.lamco.rdp-server-48.png");
use parking_lot::Mutex;
use tokio::sync::mpsc;

use crate::{
    config::Config,
    gui::{
        message::{DamageTrackingPreset, EgfxPreset, Message, PerformancePreset},
        server_connection::{ConnectionMode, ServerConnection},
        server_process::ServerLogLine,
        state::{AppState, CertGenState, LogLevel, LogLine, MessageLevel, Tab, TabCategory},
        tabs, theme as app_theme,
    },
};

pub struct ConfigGuiApp {
    pub state: AppState,
    pub current_tab: Tab,
    /// Server connection (None if not connected)
    /// Can be either D-Bus connection or spawned process
    server_connection: Option<ServerConnection>,
    /// Log receiver channel from server process
    log_receiver: Option<Arc<Mutex<mpsc::UnboundedReceiver<ServerLogLine>>>>,
    /// Current connection mode
    connection_mode: ConnectionMode,
    /// Shared slot for async connection tasks to deposit a ServerConnection.
    /// Needed because ServerConnection isn't Clone (can't pass through iced Messages).
    pending_connection: Arc<Mutex<Option<ServerConnection>>>,
    /// Incremented on each D-Bus reconnect to restart the signal subscription
    dbus_connection_generation: u64,
}

impl Default for ConfigGuiApp {
    fn default() -> Self {
        Self {
            state: AppState::load_or_default(),
            current_tab: Tab::Server,
            server_connection: None,
            log_receiver: None,
            connection_mode: ConnectionMode::Disconnected,
            pending_connection: Arc::new(Mutex::new(None)),
            dbus_connection_generation: 0,
        }
    }
}

/// Probe whether the desktop exposes an `org.freedesktop.portal.FileChooser`
/// portal, i.e. whether `rfd` file-browse dialogs can actually open. On wlroots
/// compositors that ship only `xdg-desktop-portal-wlr` (ScreenCast-only), the
/// interface is absent and browse buttons would silently do nothing — the GUI
/// uses this to fall back to manual path entry with a clear message.
async fn file_chooser_portal_available() -> bool {
    async fn probe() -> zbus::Result<bool> {
        let conn = zbus::Connection::session().await?;
        let proxy = zbus::fdo::IntrospectableProxy::builder(&conn)
            .destination("org.freedesktop.portal.Desktop")?
            .path("/org/freedesktop/portal/desktop")?
            .build()
            .await?;
        Ok(proxy
            .introspect()
            .await?
            .contains("org.freedesktop.portal.FileChooser"))
    }
    probe().await.unwrap_or(false)
}

impl ConfigGuiApp {
    pub fn new() -> (Self, Task<Message>) {
        let app = Self::default();

        let tasks = Task::batch([
            Task::perform(async {}, |()| Message::RefreshCapabilities),
            Task::perform(async {}, |()| Message::VideoDetectGpus),
            Task::perform(async {}, |()| Message::CheckCertificates),
            // Try to detect existing D-Bus server on startup
            Task::perform(async {}, |()| Message::TryDbusConnect),
            // Probe whether a FileChooser portal exists (browse buttons work)
            Task::perform(
                file_chooser_portal_available(),
                Message::FileDialogPortalChecked,
            ),
        ]);

        (app, tasks)
    }

    pub fn title(&self) -> String {
        let dirty_indicator = if self.state.is_dirty { " *" } else { "" };
        format!("lamco-rdp-server Configuration{dirty_indicator}")
    }

    /// Perform application exit with proper server handling.
    ///
    /// This method centralizes all exit logic to ensure consistent behavior:
    /// - If `close_stops_server` is false and server is running, detach it first
    /// - If `close_stops_server` is true, server will be stopped by Drop impl
    fn perform_exit(&mut self) -> Task<Message> {
        // If user wants server to keep running, detach it before exit
        if !self.state.close_stops_server
            && let Some(ref mut connection) = self.server_connection
            && matches!(
                self.state.server_status,
                crate::gui::state::ServerStatus::Running { .. }
            )
        {
            // Server will continue running after GUI closes
            connection.detach();
        }

        iced::exit()
    }

    pub fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Noop => Task::none(),
            Message::TabSelected(tab) => {
                self.current_tab = tab;
                Task::none()
            }
            Message::CategorySelected(category) => {
                if let Some(&first_tab) = category.tabs().first() {
                    self.current_tab = first_tab;
                }
                Task::none()
            }

            Message::ServerListenAddrChanged(addr) => {
                self.state.edit_strings.server_ip = addr;
                let composed = crate::gui::state::compose_listen_addr(
                    &self.state.edit_strings.server_ip,
                    &self.state.edit_strings.server_port,
                );
                self.state.config.server.listen_addr = composed;
                self.state.mark_dirty();
                Task::none()
            }
            Message::ServerPortChanged(port) => {
                self.state.edit_strings.server_port = port;
                let composed = crate::gui::state::compose_listen_addr(
                    &self.state.edit_strings.server_ip,
                    &self.state.edit_strings.server_port,
                );
                self.state.config.server.listen_addr = composed;
                self.state.mark_dirty();
                Task::none()
            }
            Message::ServerMaxConnectionsChanged(val) => {
                self.state.edit_strings.max_connections.clone_from(&val);
                if let Ok(v) = val.parse() {
                    self.state.config.server.max_connections = v;
                    self.state.mark_dirty();
                }
                Task::none()
            }
            Message::ServerSessionTimeoutChanged(val) => {
                self.state.edit_strings.session_timeout.clone_from(&val);
                if let Ok(v) = val.parse() {
                    self.state.config.server.session_timeout = v;
                    self.state.mark_dirty();
                }
                Task::none()
            }
            Message::ServerUsePortalsToggled(val) => {
                self.state.config.server.use_portals = val;
                self.state.mark_dirty();
                Task::none()
            }
            Message::ServerViewOnlyToggled(val) => {
                self.state.config.server.view_only = val;
                self.state.mark_dirty();
                Task::none()
            }

            Message::SecurityCertPathChanged(path) => {
                self.state.edit_strings.cert_path.clone_from(&path);
                self.state.config.security.cert_path = PathBuf::from(path);
                self.state.mark_dirty();
                Task::none()
            }
            Message::SecurityBrowseCert => {
                if !self.state.file_dialog_available {
                    self.state.add_message(
                        MessageLevel::Warning,
                        "No file-picker portal on this desktop — type the certificate \
                         path directly, or install xdg-desktop-portal-gtk to browse."
                            .to_string(),
                    );
                    return Task::none();
                }
                Task::perform(
                    async {
                        let file = rfd::AsyncFileDialog::new()
                            .add_filter("Certificate", &["pem", "crt", "cert"])
                            .pick_file()
                            .await;
                        file.map(|f| f.path().to_path_buf())
                    },
                    Message::SecurityCertSelected,
                )
            }
            Message::SecurityCertSelected(path) => {
                if let Some(p) = path {
                    // Update both config and edit_strings so UI displays the selection
                    self.state.edit_strings.cert_path = p.display().to_string();
                    self.state.config.security.cert_path = p;
                    self.state.mark_dirty();
                }
                Task::none()
            }
            Message::SecurityKeyPathChanged(path) => {
                self.state.edit_strings.key_path.clone_from(&path);
                self.state.config.security.key_path = PathBuf::from(path);
                self.state.mark_dirty();
                Task::none()
            }
            Message::SecurityBrowseKey => {
                if !self.state.file_dialog_available {
                    self.state.add_message(
                        MessageLevel::Warning,
                        "No file-picker portal on this desktop — type the private key \
                         path directly, or install xdg-desktop-portal-gtk to browse."
                            .to_string(),
                    );
                    return Task::none();
                }
                Task::perform(
                    async {
                        let file = rfd::AsyncFileDialog::new()
                            .add_filter("Private Key", &["pem", "key"])
                            .pick_file()
                            .await;
                        file.map(|f| f.path().to_path_buf())
                    },
                    Message::SecurityKeySelected,
                )
            }
            Message::SecurityKeySelected(path) => {
                if let Some(p) = path {
                    // Update both config and edit_strings so UI displays the selection
                    self.state.edit_strings.key_path = p.display().to_string();
                    self.state.config.security.key_path = p;
                    self.state.mark_dirty();
                }
                Task::none()
            }
            Message::SecurityGenerateCert => {
                self.state.cert_gen_dialog = Some(CertGenState::default());
                Task::none()
            }
            Message::CertGenCommonNameChanged(name) => {
                if let Some(ref mut dialog) = self.state.cert_gen_dialog {
                    dialog.common_name = name;
                }
                Task::none()
            }
            Message::CertGenOrganizationChanged(org) => {
                if let Some(ref mut dialog) = self.state.cert_gen_dialog {
                    dialog.organization = org;
                }
                Task::none()
            }
            Message::CertGenValidDaysChanged(days) => {
                if let Some(ref mut dialog) = self.state.cert_gen_dialog {
                    dialog.valid_days_str.clone_from(&days);
                    if let Ok(d) = days.parse() {
                        dialog.valid_days = d;
                    }
                }
                Task::none()
            }
            Message::CertGenConfirm => {
                if let Some(ref mut dialog) = self.state.cert_gen_dialog {
                    dialog.generating = true;
                    let cert_path = self.state.config.security.cert_path.clone();
                    let key_path = self.state.config.security.key_path.clone();
                    let common_name = dialog.common_name.clone();
                    let organization = dialog.organization.clone();
                    let valid_days = dialog.valid_days;

                    return Task::perform(
                        async move {
                            crate::gui::certificates::generate_self_signed_certificate(
                                cert_path,
                                key_path,
                                common_name,
                                Some(organization),
                                valid_days,
                            )
                        },
                        Message::CertGenCompleted,
                    );
                }
                Task::none()
            }
            Message::CertGenCancel => {
                self.state.cert_gen_dialog = None;
                Task::none()
            }
            Message::CertGenCompleted(result) => {
                self.state.cert_gen_dialog = None;
                match result {
                    Ok(()) => {
                        self.state.add_message(
                            MessageLevel::Success,
                            "Certificate generated successfully".to_string(),
                        );
                    }
                    Err(e) => {
                        self.state.add_message(MessageLevel::Error, e);
                    }
                }
                Task::none()
            }
            Message::SecurityEnableNlaToggled(val) => {
                // Legacy handler: map to security_mode for backward compat
                self.state.config.security.enable_nla = val;
                if val {
                    self.state.config.security.security_mode = "hybrid".to_string();
                }
                self.state.mark_dirty();
                Task::none()
            }
            Message::SecurityModeChanged(mode) => {
                self.state.config.security.security_mode = mode;
                self.state.mark_dirty();
                Task::none()
            }
            Message::SecurityAuthMethodChanged(method) => {
                self.state.config.security.auth_method = method;
                self.state.mark_dirty();
                Task::none()
            }
            Message::SecurityRequireTls13Toggled(val) => {
                self.state.config.security.require_tls_13 = val;
                self.state.mark_dirty();
                Task::none()
            }

            Message::VideoTargetFpsChanged(fps) => {
                self.state.config.video.target_fps = fps;
                self.state.mark_dirty();
                Task::none()
            }
            Message::VideoCursorModeChanged(mode) => {
                self.state.config.video.cursor_mode = mode;
                self.state.mark_dirty();
                Task::none()
            }
            Message::CaptureProtocolChanged(protocol) => {
                self.state.config.capture.protocol = protocol;
                self.state.mark_dirty();
                Task::none()
            }
            Message::CaptureAllowFallbackToggled(val) => {
                self.state.config.capture.allow_fallback = val;
                self.state.mark_dirty();
                Task::none()
            }
            Message::CaptureHandshakeTimeoutChanged(val) => {
                self.state
                    .edit_strings
                    .capture_handshake_timeout
                    .clone_from(&val);
                if let Ok(v) = val.parse() {
                    self.state.config.capture.handshake_timeout_ms = v;
                    self.state.mark_dirty();
                }
                Task::none()
            }
            Message::VideoPipelineToggleExpanded => {
                self.state.video_pipeline_expanded = !self.state.video_pipeline_expanded;
                Task::none()
            }

            Message::ProcessorTargetFpsChanged(val) => {
                if let Ok(v) = val.parse() {
                    self.state.config.video_pipeline.processor.target_fps = v;
                    self.state.mark_dirty();
                }
                Task::none()
            }
            Message::ProcessorMaxQueueDepthChanged(val) => {
                self.state.edit_strings.max_queue_depth.clone_from(&val);
                if let Ok(v) = val.parse() {
                    self.state.config.video_pipeline.processor.max_queue_depth = v;
                    self.state.mark_dirty();
                }
                Task::none()
            }
            Message::ProcessorAdaptiveQualityToggled(val) => {
                self.state.config.video_pipeline.processor.adaptive_quality = val;
                self.state.mark_dirty();
                Task::none()
            }
            Message::ProcessorDamageThresholdChanged(val) => {
                self.state.config.video_pipeline.processor.damage_threshold = val;
                self.state.mark_dirty();
                Task::none()
            }
            Message::ProcessorDropOnFullQueueToggled(val) => {
                self.state
                    .config
                    .video_pipeline
                    .processor
                    .drop_on_full_queue = val;
                self.state.mark_dirty();
                Task::none()
            }
            Message::ProcessorEnableMetricsToggled(val) => {
                self.state.config.video_pipeline.processor.enable_metrics = val;
                self.state.mark_dirty();
                Task::none()
            }

            Message::DispatcherChannelSizeChanged(val) => {
                self.state.edit_strings.channel_size.clone_from(&val);
                if let Ok(v) = val.parse() {
                    self.state.config.video_pipeline.dispatcher.channel_size = v;
                    self.state.mark_dirty();
                }
                Task::none()
            }
            Message::DispatcherPriorityDispatchToggled(val) => {
                self.state
                    .config
                    .video_pipeline
                    .dispatcher
                    .priority_dispatch = val;
                self.state.mark_dirty();
                Task::none()
            }
            Message::DispatcherMaxFrameAgeChanged(val) => {
                self.state.edit_strings.max_frame_age.clone_from(&val);
                if let Ok(v) = val.parse() {
                    self.state.config.video_pipeline.dispatcher.max_frame_age_ms = v;
                    self.state.mark_dirty();
                }
                Task::none()
            }
            Message::DispatcherEnableBackpressureToggled(val) => {
                self.state
                    .config
                    .video_pipeline
                    .dispatcher
                    .enable_backpressure = val;
                self.state.mark_dirty();
                Task::none()
            }
            Message::DispatcherHighWaterMarkChanged(val) => {
                self.state.config.video_pipeline.dispatcher.high_water_mark = val;
                self.state.mark_dirty();
                Task::none()
            }
            Message::DispatcherLowWaterMarkChanged(val) => {
                self.state.config.video_pipeline.dispatcher.low_water_mark = val;
                self.state.mark_dirty();
                Task::none()
            }
            Message::DispatcherLoadBalancingToggled(val) => {
                self.state.config.video_pipeline.dispatcher.load_balancing = val;
                self.state.mark_dirty();
                Task::none()
            }

            Message::ConverterBufferPoolSizeChanged(val) => {
                self.state
                    .edit_strings
                    .converter_buffer_pool_size
                    .clone_from(&val);
                if let Ok(v) = val.parse() {
                    self.state.config.video_pipeline.converter.buffer_pool_size = v;
                    self.state.mark_dirty();
                }
                Task::none()
            }
            Message::ConverterEnableSimdToggled(val) => {
                self.state.config.video_pipeline.converter.enable_simd = val;
                self.state.mark_dirty();
                Task::none()
            }
            Message::ConverterDamageThresholdChanged(val) => {
                self.state.config.video_pipeline.converter.damage_threshold = val;
                self.state.mark_dirty();
                Task::none()
            }
            Message::ConverterEnableStatisticsToggled(val) => {
                self.state.config.video_pipeline.converter.enable_statistics = val;
                self.state.mark_dirty();
                Task::none()
            }

            Message::InputProtocolChanged(protocol) => {
                self.state.config.input.input_protocol = protocol;
                self.state.mark_dirty();
                Task::none()
            }
            Message::InputKeyboardLayoutChanged(layout) => {
                self.state.config.input.keyboard_layout = layout;
                self.state.mark_dirty();
                Task::none()
            }
            Message::InputEnableTouchToggled(val) => {
                self.state.config.input.enable_touch = val;
                self.state.mark_dirty();
                Task::none()
            }

            Message::ClipboardEnabledToggled(val) => {
                self.state.config.clipboard.enabled = val;
                self.state.mark_dirty();
                Task::none()
            }
            Message::ClipboardMaxSizeChanged(val) => {
                self.state.edit_strings.max_size_mb.clone_from(&val);
                if let Ok(mb) = val.parse::<usize>() {
                    self.state.config.clipboard.max_size = mb * 1024 * 1024;
                    self.state.mark_dirty();
                }
                Task::none()
            }
            Message::ClipboardRateLimitChanged(val) => {
                self.state.edit_strings.rate_limit.clone_from(&val);
                if let Ok(v) = val.parse() {
                    self.state.config.clipboard.rate_limit_ms = v;
                    self.state.mark_dirty();
                }
                Task::none()
            }
            Message::ClipboardAllowedTypesChanged(types) => {
                self.state.config.clipboard.allowed_types = types
                    .lines()
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
                self.state.mark_dirty();
                Task::none()
            }
            Message::ClipboardPresetSelected(preset) => {
                self.state.config.clipboard.allowed_types = preset.to_mime_types();
                self.state.mark_dirty();
                Task::none()
            }

            Message::AudioEnabledToggled(val) => {
                self.state.config.audio.enabled = val;
                self.state.mark_dirty();
                Task::none()
            }
            Message::AudioCodecChanged(codec) => {
                self.state.config.audio.codec = codec;
                self.state.mark_dirty();
                Task::none()
            }
            Message::AudioSampleRateChanged(rate) => {
                self.state.config.audio.sample_rate = rate;
                self.state.mark_dirty();
                Task::none()
            }
            Message::AudioChannelsChanged(channels) => {
                self.state.config.audio.channels = channels;
                self.state.mark_dirty();
                Task::none()
            }
            Message::AudioFrameMsChanged(ms) => {
                self.state.config.audio.frame_ms = ms;
                self.state.mark_dirty();
                Task::none()
            }
            Message::AudioOpusBitrateChanged(val) => {
                self.state.edit_strings.audio_opus_bitrate.clone_from(&val);
                if let Ok(kbps) = val.parse::<u32>() {
                    self.state.config.audio.opus_bitrate = kbps * 1000; // Convert kbps to bps
                    self.state.mark_dirty();
                }
                Task::none()
            }

            Message::MultimonEnabledToggled(val) => {
                self.state.config.multimon.enabled = val;
                self.state.mark_dirty();
                Task::none()
            }
            Message::MultimonMaxMonitorsChanged(val) => {
                if let Ok(v) = val.parse::<usize>() {
                    self.state.config.multimon.max_monitors = v;
                    self.state.edit_strings.max_monitors = val;
                    self.state.mark_dirty();
                }
                Task::none()
            }
            Message::MultimonPresetSelected(preset) => {
                use crate::gui::message::MultimonPreset;
                let max = match preset {
                    MultimonPreset::Single => 1,
                    MultimonPreset::Dual => 2,
                    MultimonPreset::Triple => 3,
                    MultimonPreset::Quad => 4,
                    MultimonPreset::Custom => self.state.config.multimon.max_monitors, // Keep current
                };
                self.state.config.multimon.max_monitors = max;
                self.state.edit_strings.max_monitors = max.to_string();
                self.state.mark_dirty();
                Task::none()
            }

            Message::PerformancePresetSelected(preset) => {
                apply_performance_preset(&mut self.state.config.performance, preset);
                self.state.active_preset = Some(preset.to_string().to_lowercase());
                self.state.mark_dirty();
                Task::none()
            }
            Message::PerformanceEncoderThreadsChanged(val) => {
                self.state.edit_strings.encoder_threads.clone_from(&val);
                if let Ok(v) = val.parse() {
                    self.state.config.performance.encoder_threads = v;
                    self.state.mark_dirty();
                }
                Task::none()
            }
            Message::PerformanceNetworkThreadsChanged(val) => {
                self.state.edit_strings.network_threads.clone_from(&val);
                if let Ok(v) = val.parse() {
                    self.state.config.performance.network_threads = v;
                    self.state.mark_dirty();
                }
                Task::none()
            }
            Message::PerformanceBufferPoolSizeChanged(val) => {
                self.state.edit_strings.buffer_pool_size.clone_from(&val);
                if let Ok(v) = val.parse() {
                    self.state.config.performance.buffer_pool_size = v;
                    self.state.mark_dirty();
                }
                Task::none()
            }
            Message::PerformanceZeroCopyToggled(val) => {
                self.state.config.performance.zero_copy = val;
                self.state.mark_dirty();
                Task::none()
            }
            Message::MonitoringEnabledToggled(val) => {
                self.state.config.monitoring.enabled = val;
                self.state.mark_dirty();
                Task::none()
            }
            Message::MonitoringSnapshotIntervalChanged(val) => {
                self.state
                    .edit_strings
                    .monitoring_snapshot_interval
                    .clone_from(&val);
                if let Ok(v) = val.parse() {
                    self.state.config.monitoring.snapshot_interval_secs = v;
                    self.state.mark_dirty();
                }
                Task::none()
            }
            Message::MonitoringMetricsBindChanged(val) => {
                self.state.config.monitoring.metrics_bind = val;
                self.state.mark_dirty();
                Task::none()
            }
            Message::PerformanceAdaptiveFpsToggleExpanded => {
                self.state.adaptive_fps_expanded = !self.state.adaptive_fps_expanded;
                Task::none()
            }
            Message::AdaptiveFpsEnabledToggled(val) => {
                self.state.config.performance.adaptive_fps.enabled = val;
                self.state.mark_dirty();
                Task::none()
            }
            Message::AdaptiveFpsMinFpsChanged(val) => {
                self.state.config.performance.adaptive_fps.min_fps = val;
                self.state.mark_dirty();
                Task::none()
            }
            Message::AdaptiveFpsMaxFpsChanged(val) => {
                self.state.config.performance.adaptive_fps.max_fps = val;
                self.state.mark_dirty();
                Task::none()
            }
            Message::AdaptiveFpsHighActivityChanged(val) => {
                self.state
                    .config
                    .performance
                    .adaptive_fps
                    .high_activity_threshold = val;
                self.state.mark_dirty();
                Task::none()
            }
            Message::AdaptiveFpsMediumActivityChanged(val) => {
                self.state
                    .config
                    .performance
                    .adaptive_fps
                    .medium_activity_threshold = val;
                self.state.mark_dirty();
                Task::none()
            }
            Message::AdaptiveFpsLowActivityChanged(val) => {
                self.state
                    .config
                    .performance
                    .adaptive_fps
                    .low_activity_threshold = val;
                self.state.mark_dirty();
                Task::none()
            }
            Message::PerformanceLatencyToggleExpanded => {
                self.state.latency_expanded = !self.state.latency_expanded;
                Task::none()
            }
            Message::LatencyModeChanged(mode) => {
                self.state.config.performance.latency.mode = mode;
                self.state.mark_dirty();
                Task::none()
            }
            Message::LatencyInteractiveDelayChanged(val) => {
                self.state.edit_strings.interactive_delay.clone_from(&val);
                if let Ok(v) = val.parse() {
                    self.state
                        .config
                        .performance
                        .latency
                        .interactive_max_delay_ms = v;
                    self.state.mark_dirty();
                }
                Task::none()
            }
            Message::LatencyBalancedDelayChanged(val) => {
                self.state.edit_strings.balanced_delay.clone_from(&val);
                if let Ok(v) = val.parse() {
                    self.state.config.performance.latency.balanced_max_delay_ms = v;
                    self.state.mark_dirty();
                }
                Task::none()
            }
            Message::LatencyQualityDelayChanged(val) => {
                self.state.edit_strings.quality_delay.clone_from(&val);
                if let Ok(v) = val.parse() {
                    self.state.config.performance.latency.quality_max_delay_ms = v;
                    self.state.mark_dirty();
                }
                Task::none()
            }
            Message::LatencyBalancedThresholdChanged(val) => {
                self.state
                    .config
                    .performance
                    .latency
                    .balanced_damage_threshold = val;
                self.state.mark_dirty();
                Task::none()
            }
            Message::LatencyQualityThresholdChanged(val) => {
                self.state
                    .config
                    .performance
                    .latency
                    .quality_damage_threshold = val;
                self.state.mark_dirty();
                Task::none()
            }

            Message::LoggingToggleExpanded => {
                self.state.logging_expanded = !self.state.logging_expanded;
                Task::none()
            }
            Message::LoggingLevelChanged(level) => {
                self.state.config.logging.level = level;
                self.state.mark_dirty();
                Task::none()
            }
            Message::LoggingLogDirChanged(dir) => {
                self.state.edit_strings.log_dir.clone_from(&dir);
                self.state.config.logging.log_dir = if dir.is_empty() {
                    None
                } else {
                    Some(PathBuf::from(dir))
                };
                self.state.mark_dirty();
                Task::none()
            }
            Message::LoggingBrowseLogDir => {
                if !self.state.file_dialog_available {
                    self.state.add_message(
                        MessageLevel::Warning,
                        "No file-picker portal on this desktop (e.g. wlroots/sway with \
                         only portal-wlr). Type the log directory path directly, or \
                         install xdg-desktop-portal-gtk to enable browsing."
                            .to_string(),
                    );
                    return Task::none();
                }
                Task::perform(
                    async {
                        let folder = rfd::AsyncFileDialog::new().pick_folder().await;
                        folder.map(|f| f.path().to_path_buf())
                    },
                    Message::LoggingLogDirSelected,
                )
            }
            Message::FileDialogPortalChecked(available) => {
                self.state.file_dialog_available = available;
                Task::none()
            }
            Message::LoggingLogDirSelected(path) => {
                if let Some(p) = path {
                    // Update both config and edit_strings so UI displays the selection
                    self.state.edit_strings.log_dir = p.display().to_string();
                    self.state.config.logging.log_dir = Some(p);
                    self.state.mark_dirty();
                }
                Task::none()
            }
            Message::LoggingMetricsToggled(val) => {
                self.state.config.logging.metrics = val;
                self.state.mark_dirty();
                Task::none()
            }
            Message::LoggingClearLogDir => {
                // Clear both config and edit_strings
                self.state.edit_strings.log_dir.clear();
                self.state.config.logging.log_dir = None;
                self.state.mark_dirty();
                Task::none()
            }

            Message::EgfxEnabledToggled(val) => {
                self.state.config.egfx.enabled = val;
                self.state.mark_dirty();
                Task::none()
            }
            Message::EgfxPresetSelected(preset) => {
                apply_egfx_preset(&mut self.state.config.egfx, preset);
                self.state.active_preset =
                    Some(format!("egfx_{}", preset.to_string().to_lowercase()));
                self.state.mark_dirty();
                Task::none()
            }
            Message::EgfxToggleExpertMode => {
                self.state.egfx_expert_mode = !self.state.egfx_expert_mode;
                Task::none()
            }
            Message::EgfxH264LevelChanged(level) => {
                self.state.config.egfx.h264_level = level;
                self.state.mark_dirty();
                Task::none()
            }
            Message::EgfxH264BitrateChanged(val) => {
                self.state.edit_strings.h264_bitrate.clone_from(&val);
                if let Ok(v) = val.parse() {
                    self.state.config.egfx.h264_bitrate = v;
                    self.state.mark_dirty();
                }
                Task::none()
            }
            Message::EgfxZgfxCompressionChanged(mode) => {
                self.state.config.egfx.zgfx_compression = mode;
                self.state.mark_dirty();
                Task::none()
            }
            Message::EgfxMaxFramesInFlightChanged(val) => {
                self.state.edit_strings.max_frames.clone_from(&val);
                if let Ok(v) = val.parse() {
                    self.state.config.egfx.max_frames_in_flight = v;
                    self.state.mark_dirty();
                }
                Task::none()
            }
            Message::EgfxFrameAckTimeoutChanged(val) => {
                self.state.edit_strings.frame_ack_timeout.clone_from(&val);
                if let Ok(v) = val.parse() {
                    self.state.config.egfx.frame_ack_timeout = v;
                    self.state.mark_dirty();
                }
                Task::none()
            }
            Message::EgfxPeriodicIdrIntervalChanged(val) => {
                self.state.edit_strings.periodic_idr.clone_from(&val);
                if let Ok(v) = val.parse() {
                    self.state.config.egfx.periodic_idr_interval = v;
                    self.state.mark_dirty();
                }
                Task::none()
            }
            Message::EgfxCodecChanged(codec) => {
                self.state.config.egfx.codec = codec;
                self.state.mark_dirty();
                Task::none()
            }
            Message::EgfxQpMinChanged(val) => {
                self.state.edit_strings.qp_min.clone_from(&val);
                if let Ok(v) = val.parse() {
                    self.state.config.egfx.qp_min = v;
                    self.state.mark_dirty();
                }
                Task::none()
            }
            Message::EgfxQpMaxChanged(val) => {
                self.state.edit_strings.qp_max.clone_from(&val);
                if let Ok(v) = val.parse() {
                    self.state.config.egfx.qp_max = v;
                    self.state.mark_dirty();
                }
                Task::none()
            }
            Message::EgfxQpDefaultChanged(val) => {
                self.state.edit_strings.qp_default.clone_from(&val);
                if let Ok(v) = val.parse() {
                    self.state.config.egfx.qp_default = v;
                    self.state.mark_dirty();
                }
                Task::none()
            }
            Message::EgfxAvc444EnabledToggled(val) => {
                self.state.config.egfx.avc444_enabled = val;
                self.state.mark_dirty();
                Task::none()
            }
            Message::EgfxAvc444AuxBitrateRatioChanged(val) => {
                self.state.config.egfx.avc444_aux_bitrate_ratio = val;
                self.state.mark_dirty();
                Task::none()
            }
            Message::EgfxColorMatrixChanged(matrix) => {
                self.state.config.egfx.color_matrix = matrix;
                self.state.mark_dirty();
                Task::none()
            }
            Message::EgfxColorRangeChanged(range) => {
                self.state.config.egfx.color_range = range;
                self.state.mark_dirty();
                Task::none()
            }
            Message::EgfxAvc444EnableAuxOmissionToggled(val) => {
                self.state.config.egfx.avc444_enable_aux_omission = val;
                self.state.mark_dirty();
                Task::none()
            }
            Message::EgfxAvc444MaxAuxIntervalChanged(val) => {
                self.state.edit_strings.max_aux_interval.clone_from(&val);
                if let Ok(v) = val.parse() {
                    self.state.config.egfx.avc444_max_aux_interval = v;
                    self.state.mark_dirty();
                }
                Task::none()
            }
            Message::EgfxAvc444AuxChangeThresholdChanged(val) => {
                self.state.config.egfx.avc444_aux_change_threshold = val;
                self.state.mark_dirty();
                Task::none()
            }
            Message::EgfxAvc444ForceAuxIdrToggled(val) => {
                self.state.config.egfx.avc444_force_aux_idr_on_return = val;
                self.state.mark_dirty();
                Task::none()
            }

            Message::DamageTrackingToggleExpanded => {
                self.state.damage_tracking_expanded = !self.state.damage_tracking_expanded;
                Task::none()
            }
            Message::DamageTrackingPresetSelected(preset) => {
                apply_damage_tracking_preset(&mut self.state.config.damage_tracking, preset);
                self.state.mark_dirty();
                Task::none()
            }
            Message::DamageTrackingEnabledToggled(val) => {
                self.state.config.damage_tracking.enabled = val;
                self.state.mark_dirty();
                Task::none()
            }
            Message::DamageTrackingMethodChanged(method) => {
                self.state.config.damage_tracking.method = method;
                self.state.mark_dirty();
                Task::none()
            }
            Message::DamageTrackingTileSizeChanged(val) => {
                self.state.edit_strings.tile_size.clone_from(&val);
                if let Ok(v) = val.parse() {
                    self.state.config.damage_tracking.tile_size = v;
                    self.state.mark_dirty();
                }
                Task::none()
            }
            Message::DamageTrackingDiffThresholdChanged(val) => {
                self.state.config.damage_tracking.diff_threshold = val;
                self.state.mark_dirty();
                Task::none()
            }
            Message::DamageTrackingPixelThresholdChanged(val) => {
                self.state.edit_strings.pixel_threshold.clone_from(&val);
                if let Ok(v) = val.parse() {
                    self.state.config.damage_tracking.pixel_threshold = v;
                    self.state.mark_dirty();
                }
                Task::none()
            }
            Message::DamageTrackingMergeDistanceChanged(val) => {
                self.state.edit_strings.merge_distance.clone_from(&val);
                if let Ok(v) = val.parse() {
                    self.state.config.damage_tracking.merge_distance = v;
                    self.state.mark_dirty();
                }
                Task::none()
            }
            Message::DamageTrackingMinRegionAreaChanged(val) => {
                self.state.edit_strings.min_region_area.clone_from(&val);
                if let Ok(v) = val.parse() {
                    self.state.config.damage_tracking.min_region_area = v;
                    self.state.mark_dirty();
                }
                Task::none()
            }
            Message::DamageTrackingHintDistrustThresholdChanged(val) => {
                self.state
                    .edit_strings
                    .hint_distrust_threshold_pp
                    .clone_from(&val);
                if let Ok(v) = val.parse() {
                    self.state
                        .config
                        .damage_tracking
                        .compositor_hint_distrust_threshold_pp = v;
                    self.state.mark_dirty();
                }
                Task::none()
            }
            Message::DamageTrackingHintDistrustConsecutiveSamplesChanged(val) => {
                self.state
                    .edit_strings
                    .hint_distrust_consecutive_samples
                    .clone_from(&val);
                if let Ok(v) = val.parse() {
                    self.state
                        .config
                        .damage_tracking
                        .compositor_hint_distrust_consecutive_samples = v;
                    self.state.mark_dirty();
                }
                Task::none()
            }

            Message::HardwareEncodingToggleExpanded => {
                self.state.hardware_encoding_expanded = !self.state.hardware_encoding_expanded;
                Task::none()
            }
            Message::HardwareEncodingEnabledToggled(val) => {
                self.state.config.hardware_encoding.enabled = val;
                self.state.mark_dirty();
                Task::none()
            }
            Message::HardwareEncodingVaapiDeviceChanged(device) => {
                self.state.config.hardware_encoding.vaapi_device = PathBuf::from(device);
                self.state.mark_dirty();
                Task::none()
            }
            Message::HardwareEncodingDmabufZerocopyToggled(val) => {
                self.state.config.hardware_encoding.enable_dmabuf_zerocopy = val;
                self.state.mark_dirty();
                Task::none()
            }
            Message::HardwareEncodingFallbackToSoftwareToggled(val) => {
                self.state.config.hardware_encoding.fallback_to_software = val;
                self.state.mark_dirty();
                Task::none()
            }
            Message::HardwareEncodingQualityPresetChanged(preset) => {
                self.state.config.hardware_encoding.quality_preset = preset;
                self.state.mark_dirty();
                Task::none()
            }
            Message::HardwareEncodingPreferNvencToggled(val) => {
                self.state.config.hardware_encoding.prefer_nvenc = val;
                self.state.mark_dirty();
                Task::none()
            }

            Message::DisplayToggleExpanded => {
                self.state.display_expanded = !self.state.display_expanded;
                Task::none()
            }
            Message::MultimonToggleExpanded => {
                self.state.multimon_expanded = !self.state.multimon_expanded;
                Task::none()
            }
            Message::DisplayAllowResizeToggled(val) => {
                self.state.config.display.allow_resize = val;
                self.state.mark_dirty();
                Task::none()
            }
            Message::DisplayAllowedResolutionsChanged(resolutions) => {
                self.state
                    .edit_strings
                    .resolutions_text
                    .clone_from(&resolutions);
                self.state.config.display.allowed_resolutions = resolutions
                    .lines()
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
                self.state.mark_dirty();
                Task::none()
            }
            Message::DisplayDpiAwareToggled(val) => {
                self.state.config.display.dpi_aware = val;
                self.state.mark_dirty();
                Task::none()
            }
            Message::DisplayFrameTransformChanged(val) => {
                self.state.config.display.frame_transform = val;
                self.state.mark_dirty();
                Task::none()
            }

            Message::AdvancedVideoToggleExpanded => {
                self.state.advanced_video_expanded = !self.state.advanced_video_expanded;
                Task::none()
            }
            Message::AdvancedVideoEnableFrameSkipToggled(val) => {
                self.state.config.advanced_video.enable_frame_skip = val;
                self.state.mark_dirty();
                Task::none()
            }
            Message::AdvancedVideoSceneChangeThresholdChanged(val) => {
                self.state.config.advanced_video.scene_change_threshold = val;
                self.state.mark_dirty();
                Task::none()
            }
            Message::AdvancedVideoIntraRefreshIntervalChanged(val) => {
                self.state.edit_strings.intra_refresh.clone_from(&val);
                if let Ok(v) = val.parse() {
                    self.state.config.advanced_video.intra_refresh_interval = v;
                    self.state.mark_dirty();
                }
                Task::none()
            }
            Message::AdvancedVideoEnableAdaptiveQualityToggled(val) => {
                self.state.config.advanced_video.enable_adaptive_quality = val;
                self.state.mark_dirty();
                Task::none()
            }

            Message::CursorToggleExpanded => {
                self.state.cursor_expanded = !self.state.cursor_expanded;
                Task::none()
            }
            Message::CursorPredictorToggleExpanded => {
                self.state.cursor_predictor_expanded = !self.state.cursor_predictor_expanded;
                Task::none()
            }
            Message::CursorModeChanged(mode) => {
                self.state.config.cursor.mode = mode;
                self.state.mark_dirty();
                Task::none()
            }
            Message::CursorAutoModeToggled(val) => {
                self.state.config.cursor.auto_mode = val;
                self.state.mark_dirty();
                Task::none()
            }
            Message::CursorPredictiveThresholdChanged(val) => {
                self.state
                    .edit_strings
                    .predictive_threshold
                    .clone_from(&val);
                if let Ok(v) = val.parse() {
                    self.state.config.cursor.predictive_latency_threshold_ms = v;
                    self.state.mark_dirty();
                }
                Task::none()
            }
            Message::CursorUpdateFpsChanged(val) => {
                self.state.edit_strings.cursor_update_fps.clone_from(&val);
                if let Ok(v) = val.parse() {
                    self.state.config.cursor.cursor_update_fps = v;
                    self.state.mark_dirty();
                }
                Task::none()
            }
            Message::PredictorHistorySizeChanged(val) => {
                self.state.edit_strings.history_size.clone_from(&val);
                if let Ok(v) = val.parse() {
                    self.state.config.cursor.predictor.history_size = v;
                    self.state.mark_dirty();
                }
                Task::none()
            }
            Message::PredictorLookaheadMsChanged(val) => {
                self.state.edit_strings.lookahead.clone_from(&val);
                if let Ok(v) = val.parse() {
                    self.state.config.cursor.predictor.lookahead_ms = v;
                    self.state.mark_dirty();
                }
                Task::none()
            }
            Message::PredictorVelocitySmoothingChanged(val) => {
                self.state.config.cursor.predictor.velocity_smoothing = val;
                self.state.mark_dirty();
                Task::none()
            }
            Message::PredictorAccelerationSmoothingChanged(val) => {
                self.state.config.cursor.predictor.acceleration_smoothing = val;
                self.state.mark_dirty();
                Task::none()
            }
            Message::PredictorMaxPredictionDistanceChanged(val) => {
                self.state.edit_strings.max_pred_dist.clone_from(&val);
                if let Ok(v) = val.parse() {
                    self.state.config.cursor.predictor.max_prediction_distance = v;
                    self.state.mark_dirty();
                }
                Task::none()
            }
            Message::PredictorMinVelocityThresholdChanged(val) => {
                self.state.edit_strings.min_velocity.clone_from(&val);
                if let Ok(v) = val.parse() {
                    self.state.config.cursor.predictor.min_velocity_threshold = v;
                    self.state.mark_dirty();
                }
                Task::none()
            }
            Message::PredictorStopConvergenceRateChanged(val) => {
                self.state.config.cursor.predictor.stop_convergence_rate = val;
                self.state.mark_dirty();
                Task::none()
            }

            Message::LoadConfig => Task::perform(
                async {
                    let file = rfd::AsyncFileDialog::new()
                        .add_filter("TOML Config", &["toml"])
                        .pick_file()
                        .await;
                    file.map(|f| f.path().to_path_buf())
                },
                Message::ConfigFileSelected,
            ),
            Message::BrowseConfigFile => Task::perform(
                async {
                    let file = rfd::AsyncFileDialog::new()
                        .add_filter("TOML Config", &["toml"])
                        .pick_file()
                        .await;
                    file.map(|f| f.path().to_path_buf())
                },
                Message::ConfigFileSelected,
            ),
            Message::ConfigFileSelected(path) => {
                if let Some(p) = path {
                    let path_str = p.to_string_lossy().to_string();
                    return Task::perform(
                        async move { Config::load(&path_str).map_err(|e| e.to_string()) },
                        Message::ConfigLoaded,
                    );
                }
                Task::none()
            }
            Message::ConfigLoaded(result) => {
                match result {
                    Ok(config) => {
                        self.state.config = config;
                        self.state.edit_strings =
                            crate::gui::state::EditStrings::from_config(&self.state.config);
                        self.state.mark_clean();
                        self.state.add_message(
                            MessageLevel::Success,
                            "Configuration loaded successfully".to_string(),
                        );
                    }
                    Err(e) => {
                        self.state.add_message(
                            MessageLevel::Error,
                            format!("Failed to load config: {e}"),
                        );
                    }
                }
                Task::none()
            }
            Message::SaveConfig => {
                // Sync GUI state to config before saving
                self.state.sync_gui_state_to_config();
                let config = self.state.config.clone();
                let path = self.state.config_path.clone();
                Task::perform(
                    async move { crate::gui::file_ops::save_config(&config, &path) },
                    Message::ConfigSaved,
                )
            }
            Message::SaveConfigAs => {
                // Sync GUI state to config before saving
                self.state.sync_gui_state_to_config();
                let config = self.state.config.clone();
                Task::perform(
                    async move {
                        let file = rfd::AsyncFileDialog::new()
                            .add_filter("TOML Config", &["toml"])
                            .set_file_name("config.toml")
                            .save_file()
                            .await;

                        if let Some(f) = file {
                            crate::gui::file_ops::save_config(&config, f.path())
                        } else {
                            Ok(())
                        }
                    },
                    Message::ConfigSaved,
                )
            }
            Message::ConfigSaved(result) => {
                match result {
                    Ok(()) => {
                        self.state.mark_clean();
                        self.state.add_message(
                            MessageLevel::Success,
                            "Configuration saved successfully".to_string(),
                        );
                    }
                    Err(e) => {
                        self.state.add_message(MessageLevel::Error, e);
                    }
                }
                Task::none()
            }
            Message::RestoreDefaults => {
                debug!("Restoring configuration to defaults");
                Task::perform(
                    async {
                        Config::default_config()
                            .map_err(|e| format!("Failed to generate defaults: {e}"))
                    },
                    Message::DefaultsRestored,
                )
            }
            Message::DefaultsRestored(result) => {
                match result {
                    Ok(default_config) => {
                        // Keep GUI state (window position, etc.) but restore all other settings
                        let gui_state = self.state.config.gui_state.clone();
                        self.state.config = default_config;
                        self.state.config.gui_state = gui_state;

                        self.state.edit_strings =
                            crate::gui::state::EditStrings::from_config(&self.state.config);

                        self.state.mark_dirty(); // Need to save to persist
                        self.state.add_message(
                            MessageLevel::Success,
                            "Settings restored to defaults. Save to keep changes.".to_string(),
                        );
                    }
                    Err(e) => {
                        self.state.add_message(MessageLevel::Error, e);
                    }
                }
                Task::none()
            }

            Message::StartServer => {
                if self.server_connection.is_some() {
                    self.state.add_message(
                        MessageLevel::Warning,
                        "Server is already running".to_string(),
                    );
                    return Task::none();
                }

                self.state.server_status = crate::gui::state::ServerStatus::Starting;
                self.state
                    .add_message(MessageLevel::Info, "Starting server...".to_string());

                let (tx, rx) = mpsc::unbounded_channel();
                self.log_receiver = Some(Arc::new(Mutex::new(rx)));

                let config = self.state.config.clone();
                let pending = self.pending_connection.clone();

                Task::perform(
                    async move {
                        let result = ServerConnection::connect(&config, tx, true).await;
                        match result {
                            Ok(conn) => {
                                *pending.lock() = Some(conn);
                                Ok(())
                            }
                            Err(e) => Err(e),
                        }
                    },
                    Message::ServerConnectResult,
                )
            }
            Message::ServerConnectResult(result) => {
                match result {
                    Ok(()) => {
                        if let Some(connection) = self.pending_connection.lock().take() {
                            let mode = connection.mode();
                            let address = match &connection {
                                ServerConnection::Process(p) => p.address().to_string(),
                                ServerConnection::DBus(_) | ServerConnection::External(_) => {
                                    self.state.config.server.listen_addr.clone()
                                }
                            };
                            let pid = connection.pid();

                            if mode == ConnectionMode::DBus {
                                self.dbus_connection_generation += 1;
                            }
                            self.server_connection = Some(connection);
                            self.connection_mode = mode;

                            self.state.server_status = crate::gui::state::ServerStatus::Running {
                                connections: 0,
                                uptime: Duration::from_secs(0),
                                address,
                            };

                            let msg = match (mode, pid) {
                                (ConnectionMode::DBus, _) => {
                                    "Connected to server via D-Bus".to_string()
                                }
                                (ConnectionMode::Process, Some(p)) => {
                                    format!("Server started (PID: {p})")
                                }
                                _ => "Server started".to_string(),
                            };
                            self.state.add_message(MessageLevel::Success, msg);

                            // In Flatpak, register with Background portal so the server
                            // survives GUI close (GNOME 43+ kills unregistered bg processes)
                            if crate::config::is_flatpak() {
                                return Task::perform(
                                    crate::gui::server_process::register_background_portal(),
                                    |()| Message::Noop,
                                );
                            }
                        } else {
                            self.state.server_status = crate::gui::state::ServerStatus::Error(
                                "Connection lost during handoff".to_string(),
                            );
                        }
                    }
                    Err(e) => {
                        self.state.server_status =
                            crate::gui::state::ServerStatus::Error(e.clone());
                        self.state.add_message(
                            MessageLevel::Error,
                            format!("Failed to start server: {e}"),
                        );
                        self.log_receiver = None;
                    }
                }
                Task::none()
            }
            Message::TryDbusConnect => {
                // Try to detect and connect to an existing D-Bus server on startup
                if self.server_connection.is_some() {
                    return Task::none();
                }

                let pending = self.pending_connection.clone();
                Task::perform(
                    async move {
                        match ServerConnection::try_dbus().await {
                            Some(conn) => {
                                *pending.lock() = Some(conn);
                                Ok(())
                            }
                            None => Err("No D-Bus server found".to_string()),
                        }
                    },
                    Message::DbusConnectResult,
                )
            }
            Message::DbusConnectResult(result) => {
                match result {
                    Ok(()) => {
                        // Extract the connection stored by the async task
                        if let Some(connection) = self.pending_connection.lock().take() {
                            let address = self.state.config.server.listen_addr.clone();
                            if connection.mode() == ConnectionMode::DBus {
                                self.dbus_connection_generation += 1;
                            }
                            self.connection_mode = connection.mode();
                            self.server_connection = Some(connection);
                            self.state.server_status = crate::gui::state::ServerStatus::Running {
                                connections: 0,
                                uptime: Duration::from_secs(0),
                                address,
                            };
                            self.state.add_message(
                                MessageLevel::Success,
                                "Connected to running server via D-Bus".to_string(),
                            );
                        }
                    }
                    Err(_) => {
                        // No D-Bus server found on startup - check PID file as fallback.
                        // Wrap the discovered PID in a ServerConnection::External so the
                        // GUI's existing control surfaces (Stop button, status display)
                        // work uniformly regardless of how the server was launched. Stop
                        // sends SIGTERM (then SIGKILL after grace) to the discovered PID.
                        if let Some(pid) = crate::gui::server_process::check_pid_file() {
                            let address = self.state.config.server.listen_addr.clone();
                            let connection = ServerConnection::external(pid, address.clone());
                            self.connection_mode = connection.mode();
                            self.server_connection = Some(connection);
                            self.state.server_status = crate::gui::state::ServerStatus::Running {
                                connections: 0,
                                uptime: Duration::from_secs(0),
                                address,
                            };
                            self.state.add_message(
                                MessageLevel::Info,
                                format!(
                                    "Connected to running server (PID {pid}, externally launched) — Stop will SIGTERM the process"
                                ),
                            );
                        }
                    }
                }
                Task::none()
            }
            Message::StopServer => {
                if let Some(mut connection) = self.server_connection.take() {
                    let mode = connection.mode();
                    self.state
                        .add_message(MessageLevel::Info, "Stopping server...".to_string());

                    if let Err(e) = connection.stop() {
                        self.state.add_message(
                            MessageLevel::Error,
                            format!("Error stopping server: {e}"),
                        );
                    } else {
                        let msg = match mode {
                            ConnectionMode::DBus => "Server stopped",
                            ConnectionMode::Process => "Server stopped",
                            ConnectionMode::External => "Server stopped (SIGTERM to external PID)",
                            ConnectionMode::Disconnected => "Disconnected",
                        };
                        self.state
                            .add_message(MessageLevel::Success, msg.to_string());
                    }

                    self.server_connection = None;
                    self.log_receiver = None;
                    self.connection_mode = ConnectionMode::Disconnected;
                    self.state.server_status = crate::gui::state::ServerStatus::Stopped;
                } else {
                    self.state
                        .add_message(MessageLevel::Warning, "Server is not running".to_string());
                }
                Task::none()
            }
            Message::RestartServer => {
                // Stop if running, then delegate to StartServer
                if self.server_connection.is_some() {
                    self.state
                        .add_message(MessageLevel::Info, "Restarting server...".to_string());

                    if let Some(mut connection) = self.server_connection.take() {
                        let _ = connection.stop();
                    }
                    self.log_receiver = None;
                    self.connection_mode = ConnectionMode::Disconnected;
                    self.state.server_status = crate::gui::state::ServerStatus::Stopped;
                }
                self.update(Message::StartServer)
            }
            Message::ServerStatusUpdated(status) => {
                self.state.server_status = status;
                Task::none()
            }
            Message::ServerExited(exit_code) => {
                self.server_connection = None;
                self.log_receiver = None;
                self.connection_mode = ConnectionMode::Disconnected;

                let msg = if let Some(code) = exit_code {
                    format!("Server exited with code {code}")
                } else {
                    "Server exited".to_string()
                };

                self.state.server_status = crate::gui::state::ServerStatus::Stopped;
                self.state.add_message(MessageLevel::Info, msg);
                Task::none()
            }
            Message::ServerLogReceived(message, level) => {
                let log_line = LogLine {
                    timestamp: chrono::Local::now().format("%H:%M:%S").to_string(),
                    level,
                    message: message.clone(),
                    raw: message,
                };
                self.state.add_log_line(log_line);
                Task::none()
            }
            Message::ValidateConfig => {
                let result = crate::gui::validation::validate_config(&self.state.config);
                Task::perform(async move { result }, Message::ValidationComplete)
            }
            Message::ValidationComplete(result) => {
                self.state.validation = result.into();
                Task::none()
            }

            Message::RefreshCapabilities => Task::perform(
                async { crate::gui::capabilities::detect_capabilities() },
                Message::CapabilitiesDetected,
            ),
            Message::CapabilitiesDetected(caps) => {
                self.state.detected_capabilities = caps.ok();
                Task::none()
            }
            Message::VideoDetectGpus => {
                Task::perform(async { crate::gui::hardware::detect_gpus() }, |gpus| {
                    Message::GpusDetected(gpus.into_iter().map(|g| g.to_state_gpu_info()).collect())
                })
            }
            Message::GpusDetected(gpus) => {
                self.state.detected_gpus = gpus;
                Task::none()
            }
            Message::ExportCapabilities => {
                if let Some(ref caps) = self.state.detected_capabilities {
                    let caps_clone = caps.clone();
                    return Task::perform(
                        async move {
                            let file = rfd::AsyncFileDialog::new()
                                .add_filter("JSON", &["json"])
                                .set_file_name("capabilities.json")
                                .save_file()
                                .await;

                            if let Some(f) = file {
                                let path = f.path().to_path_buf();
                                crate::gui::capabilities::export_capabilities(&caps_clone, &path)
                                    .map(|()| path)
                            } else {
                                Err("Export cancelled".to_string())
                            }
                        },
                        Message::CapabilitiesExported,
                    );
                }
                Task::none()
            }
            Message::CapabilitiesExported(result) => {
                match result {
                    Ok(path) => {
                        self.state.add_message(
                            MessageLevel::Success,
                            format!("Capabilities exported to: {}", path.display()),
                        );
                    }
                    Err(e) => {
                        self.state.add_message(MessageLevel::Error, e);
                    }
                }
                Task::none()
            }

            Message::NotificationsOnErrorToggled(val) => {
                self.state.config.notifications.on_error = val;
                self.state.mark_dirty();
                Task::none()
            }
            Message::NotificationsOnCertExpiryToggled(val) => {
                self.state.config.notifications.on_cert_expiry = val;
                self.state.mark_dirty();
                Task::none()
            }
            Message::LogLineReceived(line) => {
                self.state.add_log_line(LogLine::parse(&line));
                Task::none()
            }
            Message::ClearLogs => {
                self.state.log_buffer.clear();
                Task::none()
            }
            Message::ToggleLogAutoScroll => {
                self.state.log_auto_scroll = !self.state.log_auto_scroll;
                Task::none()
            }
            Message::LogFilterLevelChanged(level) => {
                self.state.log_filter_level = match level.to_lowercase().as_str() {
                    "trace" => crate::gui::state::LogLevel::Trace,
                    "debug" => crate::gui::state::LogLevel::Debug,
                    "info" => crate::gui::state::LogLevel::Info,
                    "warn" => crate::gui::state::LogLevel::Warn,
                    "error" => crate::gui::state::LogLevel::Error,
                    _ => crate::gui::state::LogLevel::Info,
                };
                Task::none()
            }
            Message::ExportLogs => {
                let logs: Vec<(String, String, String)> = self
                    .state
                    .log_buffer
                    .iter()
                    .map(|l| {
                        (
                            l.timestamp.clone(),
                            format!("{:?}", l.level),
                            l.message.clone(),
                        )
                    })
                    .collect();

                Task::perform(
                    async move {
                        let file = rfd::AsyncFileDialog::new()
                            .set_file_name("lamco-rdp-server-logs.txt")
                            .add_filter("Log files", &["txt", "log"])
                            .save_file()
                            .await;
                        match file {
                            Some(handle) => {
                                let mut content = format!(
                                    "# Lamco RDP Server Logs\n# Exported at: {}\n\n",
                                    chrono::Local::now().format("%Y-%m-%d %H:%M:%S")
                                );
                                for (ts, level, msg) in &logs {
                                    content.push_str(&format!("{ts} [{level}] {msg}\n"));
                                }
                                handle
                                    .write(content.as_bytes())
                                    .await
                                    .map(|()| format!("Logs exported ({})", handle.file_name()))
                                    .map_err(|e| format!("Failed to write: {e}"))
                            }
                            None => Err("Export cancelled".to_string()),
                        }
                    },
                    |result| match result {
                        Ok(msg) => Message::ShowInfo(msg),
                        Err(msg) => Message::ShowError(msg),
                    },
                )
            }

            Message::ShowInfo(msg) => {
                self.state.add_message(MessageLevel::Info, msg);
                Task::none()
            }
            Message::ShowWarning(msg) => {
                self.state.add_message(MessageLevel::Warning, msg);
                Task::none()
            }
            Message::ShowError(msg) => {
                self.state.add_message(MessageLevel::Error, msg);
                Task::none()
            }
            Message::DismissMessage(idx) => {
                if idx < self.state.messages.len() {
                    self.state.messages.remove(idx);
                }
                Task::none()
            }
            Message::ToggleExpertMode => {
                self.state.expert_mode = !self.state.expert_mode;
                Task::none()
            }
            Message::ToggleCloseStopsServer(val) => {
                self.state.close_stops_server = val;
                self.state.mark_dirty();
                Task::none()
            }
            Message::WindowCloseRequested => {
                debug!("WindowCloseRequested received");
                if self.state.is_dirty {
                    debug!("Unsaved changes detected, showing confirm dialog");
                    self.state.confirm_discard_dialog = true;
                    Task::none()
                } else {
                    // No unsaved changes - exit with proper server handling
                    self.perform_exit()
                }
            }
            Message::ConfirmDiscardChanges => {
                debug!("ConfirmDiscardChanges - user chose to discard");
                self.state.confirm_discard_dialog = false;
                self.perform_exit()
            }
            Message::SaveAndExit => {
                debug!("SaveAndExit - saving config then exiting");
                // Sync GUI state and save config, then exit
                self.state.confirm_discard_dialog = false;
                self.state.sync_gui_state_to_config();
                let config = self.state.config.clone();
                let path = self.state.config_path.clone();
                match crate::gui::file_ops::save_config(&config, &path) {
                    Ok(()) => {
                        self.state.is_dirty = false;
                        self.perform_exit()
                    }
                    Err(e) => {
                        self.state.add_message(
                            MessageLevel::Error,
                            format!("Failed to save configuration: {e}"),
                        );
                        Task::none()
                    }
                }
            }
            Message::CancelDiscardChanges => {
                self.state.confirm_discard_dialog = false;
                Task::none()
            }
            Message::LiveMetricsUpdated {
                fps,
                latency_ms,
                queue_depth,
                encoder_backend,
                activity_level,
                current_qp,
                adaptation_enabled,
                damage_source,
                sensor_count,
                bitrate_kbps,
                health_video,
                health_input,
                health_clipboard,
                health_session,
            } => {
                self.state.live_metrics = Some(crate::gui::state::LiveMetrics {
                    fps,
                    latency_ms,
                    queue_depth,
                    encoder_backend,
                    activity_level,
                    current_qp,
                    adaptation_enabled,
                    damage_source,
                    sensor_count,
                    bitrate_kbps,
                    last_updated: std::time::Instant::now(),
                });
                // Update per-subsystem health from performance signal
                if let Some(ref mut health) = self.state.session_health {
                    health.video = health_video;
                    health.input = health_input;
                    health.clipboard = health_clipboard;
                    health.session = health_session;
                    health.last_updated = std::time::Instant::now();
                }
                Task::none()
            }
            Message::Tick => {
                // When connected via D-Bus, periodically check the server is still alive
                if self.connection_mode == ConnectionMode::DBus && self.server_connection.is_some()
                {
                    return Task::perform(
                        async { crate::gui::dbus_client::DbusClient::is_service_available().await },
                        Message::DbusHealthCheck,
                    );
                }
                Task::none()
            }
            Message::DbusHealthCheck(alive) => {
                if !alive && self.connection_mode == ConnectionMode::DBus {
                    // D-Bus server disappeared - clean up
                    self.server_connection = None;
                    self.log_receiver = None;
                    self.connection_mode = ConnectionMode::Disconnected;
                    self.state.server_status = crate::gui::state::ServerStatus::Stopped;
                    self.state.add_message(
                        MessageLevel::Warning,
                        "Server D-Bus service disappeared - connection lost".to_string(),
                    );
                }
                Task::none()
            }
            Message::DbusServerStateChanged(_old, new, detail) => {
                // Parse health-prefixed states: "running-healthy", "running-degraded", "running-invalid"
                let (base_state, health) = if let Some(suffix) = new.strip_prefix("running-") {
                    ("running", Some(suffix.to_string()))
                } else {
                    (new.as_str(), None)
                };

                if let Some(health_level) = health {
                    // Preserve existing per-subsystem states if already populated
                    let existing = self.state.session_health.as_ref();
                    self.state.session_health = Some(crate::gui::state::SessionHealth {
                        overall: health_level,
                        detail: detail.clone(),
                        video: existing.map_or_else(String::new, |h| h.video.clone()),
                        input: existing.map_or_else(String::new, |h| h.input.clone()),
                        clipboard: existing.map_or_else(String::new, |h| h.clipboard.clone()),
                        session: existing.map_or_else(String::new, |h| h.session.clone()),
                        last_updated: std::time::Instant::now(),
                    });
                }

                match base_state {
                    "stopped" | "error" => {
                        self.server_connection = None;
                        self.log_receiver = None;
                        self.connection_mode = ConnectionMode::Disconnected;
                        self.state.server_status = crate::gui::state::ServerStatus::Stopped;
                        self.state.session_health = None;
                        self.state.add_message(
                            MessageLevel::Warning,
                            format!("Server state changed to: {new}"),
                        );
                    }
                    "running" => {
                        let msg = if detail.is_empty() {
                            "Server is now running".to_string()
                        } else {
                            format!("Server running: {detail}")
                        };
                        self.state.add_message(MessageLevel::Success, msg);
                    }
                    _ => {}
                }
                Task::none()
            }
            Message::DbusClientConnected(client_id, peer_addr) => {
                self.state.add_message(
                    MessageLevel::Info,
                    format!("Client connected: {client_id} from {peer_addr}"),
                );
                // Update connection count if we're tracking it
                if let crate::gui::state::ServerStatus::Running {
                    ref mut connections,
                    ..
                } = self.state.server_status
                {
                    *connections += 1;
                }
                Task::none()
            }
            Message::DbusClientDisconnected(client_id, reason) => {
                self.state.add_message(
                    MessageLevel::Info,
                    format!("Client disconnected: {client_id} ({reason})"),
                );
                if let crate::gui::state::ServerStatus::Running {
                    ref mut connections,
                    ..
                } = self.state.server_status
                {
                    *connections = connections.saturating_sub(1);
                }
                Task::none()
            }
            Message::DbusConfigReloaded(success) => {
                if success {
                    self.state.add_message(
                        MessageLevel::Info,
                        "Server configuration reloaded".to_string(),
                    );
                } else {
                    self.state.add_message(
                        MessageLevel::Warning,
                        "Server config reload failed".to_string(),
                    );
                }
                Task::none()
            }
            Message::PollServerLogs => {
                self.poll_server_logs();

                // For D-Bus mode, poll uptime/connections asynchronously
                if self.connection_mode == ConnectionMode::DBus
                    && let Some(ServerConnection::DBus(ref client)) = self.server_connection
                {
                    let proxy = client.clone_proxy();
                    let address = match &self.state.server_status {
                        crate::gui::state::ServerStatus::Running { address, .. } => address.clone(),
                        _ => String::new(),
                    };
                    return Task::perform(
                        async move {
                            let uptime = Duration::from_secs(proxy.uptime().await.unwrap_or(0));
                            let connections =
                                proxy.active_connections().await.unwrap_or(0) as usize;
                            crate::gui::state::ServerStatus::Running {
                                connections,
                                uptime,
                                address,
                            }
                        },
                        Message::ServerStatusUpdated,
                    );
                }
                Task::none()
            }

            Message::CheckCertificates => {
                match self.state.config.check_certificates() {
                    Ok(true) => Task::none(),
                    Ok(false) => {
                        self.state.first_run_cert_dialog = true;
                        self.state.add_message(
                            MessageLevel::Warning,
                            "TLS certificates not found. Please generate or provide certificates to start the server.".to_string(),
                        );
                        Task::none()
                    }
                    Err(e) => {
                        // Mismatched state (one exists, other doesn't)
                        self.state.add_message(
                            MessageLevel::Error,
                            format!("Certificate configuration error: {e}"),
                        );
                        Task::none()
                    }
                }
            }
            Message::FirstRunGenerateCerts => {
                self.state.first_run_cert_generating = true;

                // Get paths from config (already set to deployment-appropriate defaults)
                let cert_path = self.state.config.security.cert_path.clone();
                let key_path = self.state.config.security.key_path.clone();

                let hostname = hostname::get().map_or_else(
                    |_| "localhost".to_string(),
                    |h| h.to_string_lossy().to_string(),
                );

                Task::perform(
                    async move {
                        crate::gui::certificates::generate_self_signed_certificate(
                            cert_path,
                            key_path,
                            hostname,
                            Some("Lamco RDP Server".to_string()),
                            365,
                        )
                    },
                    Message::FirstRunCertsGenerated,
                )
            }
            Message::FirstRunProvideCerts => {
                // User wants to provide their own - just dismiss dialog and go to Security tab
                self.state.first_run_cert_dialog = false;
                self.current_tab = Tab::Security;
                self.state.add_message(
                    MessageLevel::Info,
                    "Please configure certificate paths in the Security tab.".to_string(),
                );
                Task::none()
            }
            Message::FirstRunDismiss => {
                self.state.first_run_cert_dialog = false;
                Task::none()
            }
            Message::FirstRunCertsGenerated(result) => {
                self.state.first_run_cert_generating = false;
                self.state.first_run_cert_dialog = false;

                match result {
                    Ok(()) => {
                        self.state.edit_strings.cert_path =
                            self.state.config.security.cert_path.display().to_string();
                        self.state.edit_strings.key_path =
                            self.state.config.security.key_path.display().to_string();

                        self.state.add_message(
                            MessageLevel::Success,
                            format!(
                                "Self-signed certificate generated successfully at: {}",
                                self.state.config.security.cert_path.display()
                            ),
                        );
                    }
                    Err(e) => {
                        self.state.add_message(
                            MessageLevel::Error,
                            format!("Failed to generate certificate: {e}"),
                        );
                    }
                }
                Task::none()
            }
        }
    }

    /// Render the main view
    /// Render the notification banner for queued user messages. These are added
    /// via `add_message` (server start/stop results, browse-portal warnings,
    /// errors); before this existed they were pushed to state but never drawn,
    /// so the whole notification system was invisible. Shows the most recent
    /// few, each dismissible.
    fn view_messages(&self) -> Element<'_, Message> {
        if self.state.messages.is_empty() {
            return space().height(0.0).into();
        }

        let start = self.state.messages.len().saturating_sub(5);
        let mut banner = column![].spacing(4);
        for (idx, msg) in self.state.messages.iter().enumerate().skip(start) {
            let accent = match msg.level {
                MessageLevel::Info => app_theme::colors::INFO,
                MessageLevel::Warning => app_theme::colors::WARNING,
                MessageLevel::Error => app_theme::colors::ERROR,
                MessageLevel::Success => app_theme::colors::SUCCESS,
            };
            let icon = match msg.level {
                MessageLevel::Info => "ℹ",
                MessageLevel::Warning => "⚠",
                MessageLevel::Error => "✗",
                MessageLevel::Success => "✓",
            };

            let entry = container(
                row![
                    text(icon).size(14).style(move |_theme| text::Style {
                        color: Some(accent),
                    }),
                    text(msg.text.as_str())
                        .size(13)
                        .style(|_theme| text::Style {
                            color: Some(app_theme::colors::TEXT_PRIMARY),
                        }),
                    space().width(Length::Fill),
                    button(text("✕").size(12))
                        .on_press(Message::DismissMessage(idx))
                        .style(button::text),
                ]
                .spacing(8)
                .align_y(Alignment::Center),
            )
            .padding(8)
            .width(Length::Fill)
            .style(move |_theme| container::Style {
                background: Some(iced::Background::Color(app_theme::colors::SURFACE)),
                border: iced::Border {
                    color: accent,
                    width: 1.0,
                    radius: 4.0.into(),
                },
                ..Default::default()
            });
            banner = banner.push(entry);
        }

        container(banner)
            .padding([8, 12])
            .width(Length::Fill)
            .into()
    }

    pub fn view(&self) -> Element<'_, Message> {
        let header = self.view_header();
        let tab_bar = self.view_tab_bar();
        let content = self.view_tab_content();
        let footer = self.view_footer();

        let main_content = scrollable(content).height(Length::Fill);

        let mut main_layout =
            column![header, tab_bar, self.view_messages(), main_content, footer,].spacing(0);

        if self.state.first_run_cert_dialog {
            main_layout = column![
                self.view_first_run_cert_dialog(),
                container(main_layout).style(|_theme| container::Style {
                    // Dim the background when dialog is shown
                    background: Some(iced::Background::Color(iced::Color::from_rgba(
                        0.0, 0.0, 0.0, 0.3
                    ))),
                    ..Default::default()
                })
            ];
        }

        if self.state.confirm_discard_dialog {
            main_layout = column![
                self.view_unsaved_changes_dialog(),
                container(main_layout).style(|_theme| container::Style {
                    // Dim the background when dialog is shown
                    background: Some(iced::Background::Color(iced::Color::from_rgba(
                        0.0, 0.0, 0.0, 0.5
                    ))),
                    ..Default::default()
                })
            ];
        }

        container(main_layout)
            .width(Length::Fill)
            .height(Length::Fill)
            .style(|_theme| container::Style {
                background: Some(iced::Background::Color(app_theme::colors::BACKGROUND)),
                ..Default::default()
            })
            .into()
    }

    /// Render the first-run certificate setup dialog
    fn view_first_run_cert_dialog(&self) -> Element<'_, Message> {
        let is_flatpak = crate::config::is_flatpak();
        let cert_dir = crate::config::get_cert_config_dir();

        let location_text = if is_flatpak {
            format!(
                "Running in Flatpak sandbox. Certificates will be stored in:\n{}",
                cert_dir.display()
            )
        } else {
            format!("Certificates will be stored in:\n{}", cert_dir.display())
        };

        let dialog_content = column![
            text("TLS Certificate Setup Required")
                .size(20)
                .style(|_theme| text::Style {
                    color: Some(app_theme::colors::TEXT_PRIMARY),
                }),
            space().height(12.0),
            text("The RDP server requires TLS certificates to accept connections securely.")
                .size(14)
                .style(|_theme| text::Style {
                    color: Some(app_theme::colors::TEXT_SECONDARY),
                }),
            space().height(8.0),
            text(location_text)
                .size(12)
                .style(|_theme| text::Style {
                    color: Some(app_theme::colors::TEXT_MUTED),
                }),
            space().height(20.0),
            text("Choose an option:")
                .size(14)
                .style(|_theme| text::Style {
                    color: Some(app_theme::colors::TEXT_PRIMARY),
                }),
            space().height(12.0),
            button(
                row![
                    text("Generate Self-Signed Certificate").size(14),
                    if self.state.first_run_cert_generating {
                        text(" (Generating...)").size(12)
                    } else {
                        text(" (Recommended)").size(12)
                    },
                ]
                .spacing(4)
            )
            .on_press_maybe(if self.state.first_run_cert_generating {
                None
            } else {
                Some(Message::FirstRunGenerateCerts)
            })
            .padding([12, 24])
            .width(Length::Fill)
            .style(app_theme::success_button_style),
            space().height(8.0),
            text("Creates a self-signed certificate valid for 1 year. Suitable for personal use and testing.")
                .size(11)
                .style(|_theme| text::Style {
                    color: Some(app_theme::colors::TEXT_MUTED),
                }),
            space().height(16.0),
            button(text("I'll Provide My Own Certificates").size(14))
                .on_press(Message::FirstRunProvideCerts)
                .padding([10, 20])
                .width(Length::Fill)
                .style(app_theme::secondary_button_style),
            space().height(8.0),
            text("For production use with CA-signed certificates. You'll configure the paths manually.")
                .size(11)
                .style(|_theme| text::Style {
                    color: Some(app_theme::colors::TEXT_MUTED),
                }),
        ]
        .spacing(2)
        .padding(24)
        .width(Length::Fill);

        // Center dialog at ~60% width using fill-portion spacers
        let dialog_row = row![
            space().width(Length::FillPortion(1)),
            container(dialog_content)
                .style(|_theme| container::Style {
                    background: Some(iced::Background::Color(app_theme::colors::SURFACE)),
                    border: iced::Border {
                        color: app_theme::colors::BORDER,
                        width: 1.0,
                        radius: 8.0.into(),
                    },
                    shadow: iced::Shadow {
                        color: iced::Color::from_rgba(0.0, 0.0, 0.0, 0.5),
                        offset: iced::Vector::new(0.0, 4.0),
                        blur_radius: 16.0,
                    },
                    ..Default::default()
                })
                .width(Length::FillPortion(3)),
            space().width(Length::FillPortion(1)),
        ];

        container(dialog_row)
            .width(Length::Fill)
            .height(Length::Shrink)
            .padding(iced::Padding {
                top: 40.0,
                right: 0.0,
                bottom: 0.0,
                left: 0.0,
            })
            .into()
    }

    /// Render the unsaved changes confirmation dialog
    fn view_unsaved_changes_dialog(&self) -> Element<'_, Message> {
        let dialog_content = column![
            text("Unsaved Changes")
                .size(18)
                .style(|_theme| text::Style {
                    color: Some(app_theme::colors::TEXT_PRIMARY),
                }),
            space().height(12.0),
            text("You have unsaved changes. What would you like to do?")
                .size(14)
                .style(|_theme| text::Style {
                    color: Some(app_theme::colors::TEXT_SECONDARY),
                }),
            space().height(20.0),
            row![
                button(text("Save & Exit").size(14))
                    .on_press(Message::SaveAndExit)
                    .padding([10, 20])
                    .style(app_theme::primary_button_style),
                space().width(12.0),
                button(text("Discard").size(14))
                    .on_press(Message::ConfirmDiscardChanges)
                    .padding([10, 20])
                    .style(app_theme::danger_button_style),
                space().width(12.0),
                button(text("Cancel").size(14))
                    .on_press(Message::CancelDiscardChanges)
                    .padding([10, 20])
                    .style(app_theme::secondary_button_style),
            ]
            .spacing(0),
        ]
        .spacing(2)
        .padding(24)
        .width(Length::Fill);

        // Center dialog at ~60% width using fill-portion spacers
        let dialog_row = row![
            space().width(Length::FillPortion(1)),
            container(dialog_content)
                .style(|_theme| container::Style {
                    background: Some(iced::Background::Color(app_theme::colors::SURFACE)),
                    border: iced::Border {
                        color: app_theme::colors::BORDER,
                        width: 1.0,
                        radius: 8.0.into(),
                    },
                    shadow: iced::Shadow {
                        color: iced::Color::from_rgba(0.0, 0.0, 0.0, 0.5),
                        offset: iced::Vector::new(0.0, 4.0),
                        blur_radius: 16.0,
                    },
                    ..Default::default()
                })
                .width(Length::FillPortion(3)),
            space().width(Length::FillPortion(1)),
        ];

        container(dialog_row)
            .width(Length::Fill)
            .height(Length::Shrink)
            .padding(iced::Padding {
                top: 100.0,
                right: 0.0,
                bottom: 0.0,
                left: 0.0,
            })
            .into()
    }

    /// Render the header
    fn view_header(&self) -> Element<'_, Message> {
        let (status_text, status_color, is_running) = match &self.state.server_status {
            crate::gui::state::ServerStatus::Unknown => {
                ("Offline", app_theme::colors::TEXT_MUTED, false)
            }
            crate::gui::state::ServerStatus::Stopped => {
                ("Stopped", app_theme::colors::ERROR, false)
            }
            crate::gui::state::ServerStatus::Starting => {
                ("Starting...", app_theme::colors::WARNING, false)
            }
            crate::gui::state::ServerStatus::Running { .. } => {
                ("Running", app_theme::colors::SUCCESS, true)
            }
            crate::gui::state::ServerStatus::Error(_) => ("Error", app_theme::colors::ERROR, false),
        };

        let status_badge = container(
            row![
                text("●").size(12).style(move |_theme| text::Style {
                    color: Some(status_color),
                }),
                text(status_text).size(12).style(move |_theme| text::Style {
                    color: Some(status_color),
                }),
            ]
            .spacing(6)
            .align_y(Alignment::Center),
        )
        .padding([4, 12])
        .style(app_theme::status_badge_style(is_running));

        let server_controls = row![
            status_badge,
            space().width(8.0),
            if is_running {
                button(text("Stop").size(12))
                    .on_press(Message::StopServer)
                    .padding([6, 14])
                    .style(app_theme::danger_button_style)
            } else {
                button(text("Start Server").size(12))
                    .on_press(Message::StartServer)
                    .padding([6, 14])
                    .style(app_theme::success_button_style)
            },
            if is_running {
                Element::from(
                    button(text("Restart").size(12))
                        .on_press(Message::RestartServer)
                        .padding([6, 12])
                        .style(app_theme::secondary_button_style),
                )
            } else {
                Element::from(space().width(0.0))
            },
        ]
        .spacing(6)
        .align_y(Alignment::Center);

        container(
            row![
                row![
                    image(image::Handle::from_bytes(LOGO_ICON))
                        .width(36)
                        .height(36),
                    space().width(10.0),
                    column![
                        text("Lamco").size(20).style(|_theme| text::Style {
                            color: Some(app_theme::colors::TEXT_PRIMARY),
                        }),
                        text("RDP Server").size(11).style(|_theme| text::Style {
                            color: Some(app_theme::colors::TEXT_MUTED),
                        }),
                    ]
                    .spacing(0),
                ]
                .align_y(Alignment::Center),
                space().width(30.0),
                server_controls,
                space().width(Length::Fill),
                button(text("Import").size(12))
                    .on_press(Message::LoadConfig)
                    .padding([6, 14])
                    .style(app_theme::secondary_button_style),
                button(text("Defaults").size(12))
                    .on_press(Message::RestoreDefaults)
                    .padding([6, 14])
                    .style(app_theme::secondary_button_style),
                button(text("Save").size(12))
                    .on_press(Message::SaveConfig)
                    .padding([6, 14])
                    .style(app_theme::primary_button_style),
                button(text("Export").size(12))
                    .on_press(Message::SaveConfigAs)
                    .padding([6, 14])
                    .style(app_theme::secondary_button_style),
            ]
            .spacing(10)
            .align_y(Alignment::Center)
            .padding([14, 24]),
        )
        .style(app_theme::header_style)
        .width(Length::Fill)
        .into()
    }

    /// Render the tab bar with category selector and tab buttons
    fn view_tab_bar(&self) -> Element<'_, Message> {
        let current_category = self.current_tab.category();

        let category_buttons: Vec<Element<'_, Message>> = TabCategory::all()
            .iter()
            .map(|&category| {
                let is_active = current_category == category;
                button(
                    row![
                        text(category.icon()).size(14),
                        text(category.display_name()).size(13),
                    ]
                    .spacing(6)
                    .align_y(Alignment::Center),
                )
                .on_press(Message::CategorySelected(category))
                .padding([6, 12])
                .style(app_theme::category_button_style(is_active, false))
                .into()
            })
            .collect();

        let category_row = container(
            row(category_buttons)
                .spacing(4)
                .padding([8, 16])
                .align_y(Alignment::Center),
        )
        .style(|_theme| container::Style {
            background: Some(iced::Background::Color(app_theme::colors::SURFACE_DARK)),
            border: iced::Border {
                color: app_theme::colors::BORDER,
                width: 0.0,
                radius: 0.0.into(),
            },
            ..Default::default()
        })
        .width(Length::Fill);

        let tab_buttons: Vec<Element<'_, Message>> = current_category
            .tabs()
            .iter()
            .map(|&tab| {
                let is_active = self.current_tab == tab;
                button(
                    row![text(tab.icon()).size(13), text(tab.display_name()).size(12),]
                        .spacing(4)
                        .align_y(Alignment::Center),
                )
                .on_press(Message::TabSelected(tab))
                .padding([6, 10])
                .style(app_theme::tab_button_style(is_active))
                .into()
            })
            .collect();

        let tab_row = container(
            row(tab_buttons)
                .spacing(2)
                .padding([6, 16])
                .align_y(Alignment::Center),
        )
        .style(|_theme| container::Style {
            background: Some(iced::Background::Color(app_theme::colors::SURFACE)),
            border: iced::Border {
                color: app_theme::colors::BORDER,
                width: 1.0,
                radius: 0.0.into(),
            },
            ..Default::default()
        })
        .width(Length::Fill);

        column![category_row, tab_row]
            .spacing(0)
            .width(Length::Fill)
            .into()
    }

    /// Render the current tab content
    fn view_tab_content(&self) -> Element<'_, Message> {
        let content = match self.current_tab {
            Tab::Server => tabs::view_server_tab(&self.state),
            Tab::Security => tabs::view_security_tab(&self.state),
            Tab::Video => tabs::view_video_tab(&self.state),
            Tab::Display => tabs::view_display_tab(&self.state),
            Tab::Audio => tabs::view_audio_tab(&self.state),
            Tab::Input => tabs::view_input_tab(&self.state),
            Tab::Clipboard => tabs::view_clipboard_tab(&self.state),
            Tab::Performance => tabs::view_performance_tab(&self.state),
            Tab::Egfx => tabs::view_egfx_tab(&self.state),
            Tab::Advanced => tabs::view_advanced_tab(&self.state),
            Tab::Status => tabs::view_status_tab(&self.state),
        };

        container(content)
            .style(|_theme| container::Style {
                background: Some(iced::Background::Color(app_theme::colors::BACKGROUND)),
                text_color: Some(app_theme::colors::TEXT_PRIMARY),
                ..Default::default()
            })
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    }

    /// Render the footer with status and validation
    fn view_footer(&self) -> Element<'_, Message> {
        let dirty_indicator = if self.state.is_dirty {
            text("● Unsaved changes")
                .size(12)
                .style(|_theme| text::Style {
                    color: Some(app_theme::colors::WARNING),
                })
        } else {
            text("● Saved").size(12).style(|_theme| text::Style {
                color: Some(app_theme::colors::SUCCESS),
            })
        };

        let validation_status = if self.state.validation.is_valid {
            text("✓ Valid configuration")
                .size(12)
                .style(|_theme| text::Style {
                    color: Some(app_theme::colors::SUCCESS),
                })
        } else {
            text(format!("✗ {} errors", self.state.validation.errors.len()))
                .size(12)
                .style(|_theme| text::Style {
                    color: Some(app_theme::colors::ERROR),
                })
        };

        let config_path = text(format!("Config: {}", self.state.config_path.display()))
            .size(12)
            .style(|_theme| text::Style {
                color: Some(app_theme::colors::TEXT_MUTED),
            });

        container(
            row![
                dirty_indicator,
                space().width(20.0),
                validation_status,
                space().width(Length::Fill),
                config_path,
            ]
            .spacing(8)
            .align_y(Alignment::Center)
            .padding([10, 24]),
        )
        .style(|_theme| container::Style {
            background: Some(iced::Background::Color(app_theme::colors::SURFACE)),
            border: iced::Border {
                color: app_theme::colors::BORDER,
                width: 1.0,
                radius: 0.0.into(),
            },
            shadow: iced::Shadow {
                color: iced::Color::from_rgba(0.0, 0.0, 0.0, 0.3),
                offset: iced::Vector::new(0.0, -2.0),
                blur_radius: 6.0,
            },
            ..Default::default()
        })
        .width(Length::Fill)
        .into()
    }

    /// Subscriptions for async events
    pub fn subscription(&self) -> Subscription<Message> {
        let mut subscriptions = vec![
            // Periodic tick for log updates, status polling, etc.
            iced::time::every(Duration::from_secs(1)).map(|_| Message::Tick),
            // Window close events - needed for "close GUI only" behavior
            iced::window::close_requests().map(|_id| Message::WindowCloseRequested),
        ];

        // If server is running, add a faster tick for log polling and uptime updates
        if self.server_connection.is_some() {
            subscriptions.push(
                iced::time::every(Duration::from_millis(100)).map(|_| Message::PollServerLogs),
            );
        }

        // D-Bus signal subscription: restarts on each reconnect (generation change)
        if self.connection_mode == ConnectionMode::DBus && self.server_connection.is_some() {
            subscriptions.push(Subscription::run_with(
                self.dbus_connection_generation,
                dbus_signal_stream,
            ));
        }

        Subscription::batch(subscriptions)
    }

    /// Poll server logs from the receiver channel
    fn poll_server_logs(&mut self) {
        if let Some(ref receiver) = self.log_receiver {
            let mut receiver_guard = receiver.lock();

            while let Ok(log_line) = receiver_guard.try_recv() {
                let level = match log_line.level {
                    crate::gui::server_process::LogLevel::Trace => LogLevel::Trace,
                    crate::gui::server_process::LogLevel::Debug => LogLevel::Debug,
                    crate::gui::server_process::LogLevel::Info => LogLevel::Info,
                    crate::gui::server_process::LogLevel::Warn => LogLevel::Warn,
                    crate::gui::server_process::LogLevel::Error => LogLevel::Error,
                };

                let gui_log_line = LogLine {
                    timestamp: log_line.timestamp,
                    level,
                    message: log_line.message.clone(),
                    raw: log_line.message,
                };

                self.state.add_log_line(gui_log_line);
            }
        }

        // Liveness + uptime tick. Process and External both support sync probes
        // (the child handle / signal-0 to the discovered PID). D-Bus uses async
        // polling in the PollServerLogs handler instead.
        if let Some(ref connection) = self.server_connection {
            match connection {
                ServerConnection::Process(process) => {
                    if !process.is_running() {
                        self.server_connection = None;
                        self.log_receiver = None;
                        self.connection_mode = ConnectionMode::Disconnected;
                        self.state.server_status = crate::gui::state::ServerStatus::Stopped;
                        self.state.add_message(
                            MessageLevel::Warning,
                            "Server process exited unexpectedly".to_string(),
                        );
                    } else if let crate::gui::state::ServerStatus::Running {
                        ref mut uptime,
                        connections: _,
                        address: _,
                    } = self.state.server_status
                    {
                        *uptime = process.uptime();
                    }
                }
                ServerConnection::External(ext) => {
                    if !ext.is_running() {
                        // External server went away (operator killed it elsewhere,
                        // or it crashed). Drop our handle.
                        self.server_connection = None;
                        self.connection_mode = ConnectionMode::Disconnected;
                        self.state.server_status = crate::gui::state::ServerStatus::Stopped;
                        self.state.add_message(
                            MessageLevel::Info,
                            "Externally-managed server is no longer running".to_string(),
                        );
                    } else if let crate::gui::state::ServerStatus::Running {
                        ref mut uptime,
                        connections: _,
                        address: _,
                    } = self.state.server_status
                    {
                        *uptime = ext.uptime();
                    }
                }
                ServerConnection::DBus(_) => {
                    // D-Bus mode: uptime/connections polled via Task::perform
                    // in the PollServerLogs handler (async D-Bus property reads)
                }
            }
        }
    }
}

/// Multiplexed D-Bus signal stream for iced subscription.
///
/// Connects to the session bus independently and subscribes to all 5 server
/// signals. The stream ends when the D-Bus connection drops, which triggers
/// iced to restart the subscription on the next generation increment.
fn dbus_signal_stream(
    _generation: &u64,
) -> std::pin::Pin<Box<dyn futures::Stream<Item = Message> + Send>> {
    Box::pin(iced::stream::channel(32, async move |mut output| {
        use futures::{SinkExt, StreamExt};

        let Ok(connection) = zbus::Connection::session().await else {
            return;
        };
        let Ok(proxy) = crate::gui::dbus_client::ManagerProxy::new(&connection).await else {
            return;
        };

        let Ok(mut state_stream) = proxy.receive_server_state_changed().await else {
            return;
        };
        let Ok(mut perf_stream) = proxy.receive_performance_updated().await else {
            return;
        };
        let Ok(mut connect_stream) = proxy.receive_client_connected().await else {
            return;
        };
        let Ok(mut disconnect_stream) = proxy.receive_client_disconnected().await else {
            return;
        };
        let Ok(mut config_stream) = proxy.receive_config_reloaded().await else {
            return;
        };

        loop {
            tokio::select! {
                Some(signal) = state_stream.next() => {
                    if let Ok(args) = signal.args() {
                        let _ = output.send(Message::DbusServerStateChanged(
                            args.old_status.to_string(),
                            args.new_status.to_string(),
                            args.message.to_string(),
                        )).await;
                    }
                }
                Some(signal) = perf_stream.next() => {
                    if let Ok(args) = signal.args() {
                        let _ = output.send(Message::LiveMetricsUpdated {
                            fps: args.fps,
                            latency_ms: args.latency_ms,
                            queue_depth: args.queue_depth,
                            encoder_backend: args.encoder_backend.to_string(),
                            activity_level: args.activity_level.to_string(),
                            current_qp: args.current_qp,
                            adaptation_enabled: args.adaptation_enabled,
                            damage_source: args.damage_source.to_string(),
                            sensor_count: args.sensor_count,
                            bitrate_kbps: args.bitrate_kbps,
                            health_video: args.health_video.to_string(),
                            health_input: args.health_input.to_string(),
                            health_clipboard: args.health_clipboard.to_string(),
                            health_session: args.health_session.to_string(),
                        }).await;
                    }
                }
                Some(signal) = connect_stream.next() => {
                    if let Ok(args) = signal.args() {
                        let _ = output.send(Message::DbusClientConnected(
                            args.client_id.to_string(),
                            args.peer_address.to_string(),
                        )).await;
                    }
                }
                Some(signal) = disconnect_stream.next() => {
                    if let Ok(args) = signal.args() {
                        let _ = output.send(Message::DbusClientDisconnected(
                            args.client_id.to_string(),
                            args.reason.to_string(),
                        )).await;
                    }
                }
                Some(signal) = config_stream.next() => {
                    if let Ok(_args) = signal.args() {
                        let _ = output.send(Message::DbusConfigReloaded(true)).await;
                    }
                }
                else => break,
            }
        }
    }))
}

/// Apply performance preset to config
fn apply_performance_preset(
    config: &mut crate::config::types::PerformanceConfig,
    preset: PerformancePreset,
) {
    match preset {
        PerformancePreset::Interactive => {
            config.encoder_threads = 0;
            config.network_threads = 0;
            config.buffer_pool_size = 32;
            config.zero_copy = true;
            config.adaptive_fps.enabled = true;
            config.adaptive_fps.min_fps = 15;
            config.adaptive_fps.max_fps = 60;
            config.adaptive_fps.high_activity_threshold = 0.20;
            config.adaptive_fps.medium_activity_threshold = 0.08;
            config.adaptive_fps.low_activity_threshold = 0.01;
            config.latency.mode = "interactive".to_string();
            config.latency.interactive_max_delay_ms = 16;
        }
        PerformancePreset::Balanced => {
            config.encoder_threads = 0;
            config.network_threads = 0;
            config.buffer_pool_size = 16;
            config.zero_copy = true;
            config.adaptive_fps.enabled = true;
            config.adaptive_fps.min_fps = 5;
            config.adaptive_fps.max_fps = 30;
            config.adaptive_fps.high_activity_threshold = 0.30;
            config.adaptive_fps.medium_activity_threshold = 0.10;
            config.adaptive_fps.low_activity_threshold = 0.01;
            config.latency.mode = "balanced".to_string();
            config.latency.balanced_max_delay_ms = 33;
        }
        PerformancePreset::Quality => {
            config.encoder_threads = 0;
            config.network_threads = 0;
            config.buffer_pool_size = 8;
            config.zero_copy = false;
            config.adaptive_fps.enabled = false;
            config.latency.mode = "quality".to_string();
            config.latency.quality_max_delay_ms = 100;
        }
    }
}

/// Apply EGFX quality preset to config
fn apply_egfx_preset(config: &mut crate::config::types::EgfxConfig, preset: EgfxPreset) {
    match preset {
        EgfxPreset::Speed => {
            config.h264_bitrate = 3000;
            config.qp_min = 20;
            config.qp_default = 28;
            config.qp_max = 40;
            config.periodic_idr_interval = 10;
            config.avc444_aux_bitrate_ratio = 0.3;
        }
        EgfxPreset::Balanced => {
            config.h264_bitrate = 5000;
            config.qp_min = 18;
            config.qp_default = 23;
            config.qp_max = 36;
            config.periodic_idr_interval = 5;
            config.avc444_aux_bitrate_ratio = 0.5;
        }
        EgfxPreset::Quality => {
            config.h264_bitrate = 10000;
            config.qp_min = 15;
            config.qp_default = 20;
            config.qp_max = 30;
            config.periodic_idr_interval = 3;
            config.avc444_aux_bitrate_ratio = 1.0;
        }
    }
}

/// Apply damage tracking preset to config
fn apply_damage_tracking_preset(
    config: &mut crate::config::types::DamageTrackingConfig,
    preset: DamageTrackingPreset,
) {
    match preset {
        DamageTrackingPreset::TextWork => {
            config.tile_size = 16;
            config.diff_threshold = 0.01;
            config.pixel_threshold = 1;
            config.merge_distance = 16;
            config.min_region_area = 64;
        }
        DamageTrackingPreset::General => {
            config.tile_size = 32;
            config.diff_threshold = 0.05;
            config.pixel_threshold = 4;
            config.merge_distance = 32;
            config.min_region_area = 256;
        }
        DamageTrackingPreset::Video => {
            config.tile_size = 128;
            config.diff_threshold = 0.10;
            config.pixel_threshold = 8;
            config.merge_distance = 64;
            config.min_region_area = 1024;
        }
    }
}
