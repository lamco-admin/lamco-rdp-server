//! Main iced Application implementation for lamco-rdp-server-gui
//!
//! Implements the Elm Architecture pattern: State -> View -> Message -> Update -> State

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use iced::widget::{button, column, container, row, scrollable, space, text};
use iced::{Alignment, Element, Length, Subscription, Task};
use parking_lot::Mutex;
use tokio::sync::mpsc;

use crate::config::Config;
use crate::gui::message::{DamageTrackingPreset, EgfxPreset, Message, PerformancePreset};
use crate::gui::server_process::{ServerLogLine, ServerProcess};
use crate::gui::state::{AppState, CertGenState, LogLevel, LogLine, MessageLevel, Tab};
use crate::gui::tabs;
use crate::gui::theme as app_theme;

pub struct ConfigGuiApp {
    pub state: AppState,
    pub current_tab: Tab,
    /// Server process handle (None if not running)
    server_process: Option<ServerProcess>,
    /// Log receiver channel from server process
    log_receiver: Option<Arc<Mutex<mpsc::UnboundedReceiver<ServerLogLine>>>>,
}

impl Default for ConfigGuiApp {
    fn default() -> Self {
        Self {
            state: AppState::load_or_default(),
            current_tab: Tab::Server,
            server_process: None,
            log_receiver: None,
        }
    }
}

impl ConfigGuiApp {
    pub fn new() -> (Self, Task<Message>) {
        let app = Self::default();

        // Initial tasks: detect capabilities and GPUs
        let tasks = Task::batch([
            Task::perform(async {}, |_| Message::RefreshCapabilities),
            Task::perform(async {}, |_| Message::VideoDetectGpus),
        ]);

        (app, tasks)
    }

    pub fn title(&self) -> String {
        let dirty_indicator = if self.state.is_dirty { " *" } else { "" };
        format!("lamco-rdp-server Configuration{}", dirty_indicator)
    }

    pub fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            // =================================================================
            // Tab Navigation
            // =================================================================
            Message::TabSelected(tab) => {
                self.current_tab = tab;
                Task::none()
            }

            // =================================================================
            // Server Configuration
            // =================================================================
            Message::ServerListenAddrChanged(addr) => {
                // Reconstruct full address with existing port
                let port = self
                    .state
                    .config
                    .server
                    .listen_addr
                    .rsplit(':')
                    .next()
                    .unwrap_or("3389");
                self.state.config.server.listen_addr = format!("{}:{}", addr, port);
                self.state.mark_dirty();
                Task::none()
            }
            Message::ServerPortChanged(port) => {
                // Reconstruct full address with existing IP
                let ip = self
                    .state
                    .config
                    .server
                    .listen_addr
                    .rsplit_once(':')
                    .map(|(ip, _)| ip)
                    .unwrap_or("0.0.0.0");
                self.state.config.server.listen_addr = format!("{}:{}", ip, port);
                self.state.mark_dirty();
                Task::none()
            }
            Message::ServerMaxConnectionsChanged(val) => {
                if let Ok(v) = val.parse() {
                    self.state.config.server.max_connections = v;
                    self.state.mark_dirty();
                }
                Task::none()
            }
            Message::ServerSessionTimeoutChanged(val) => {
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

            // =================================================================
            // Security Configuration
            // =================================================================
            Message::SecurityCertPathChanged(path) => {
                self.state.config.security.cert_path = PathBuf::from(path);
                self.state.mark_dirty();
                Task::none()
            }
            Message::SecurityBrowseCert => Task::perform(
                async {
                    let file = rfd::AsyncFileDialog::new()
                        .add_filter("Certificate", &["pem", "crt", "cert"])
                        .pick_file()
                        .await;
                    file.map(|f| f.path().to_path_buf())
                },
                Message::SecurityCertSelected,
            ),
            Message::SecurityCertSelected(path) => {
                if let Some(p) = path {
                    self.state.config.security.cert_path = p;
                    self.state.mark_dirty();
                }
                Task::none()
            }
            Message::SecurityKeyPathChanged(path) => {
                self.state.config.security.key_path = PathBuf::from(path);
                self.state.mark_dirty();
                Task::none()
            }
            Message::SecurityBrowseKey => Task::perform(
                async {
                    let file = rfd::AsyncFileDialog::new()
                        .add_filter("Private Key", &["pem", "key"])
                        .pick_file()
                        .await;
                    file.map(|f| f.path().to_path_buf())
                },
                Message::SecurityKeySelected,
            ),
            Message::SecurityKeySelected(path) => {
                if let Some(p) = path {
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
                    dialog.valid_days_str = days.clone();
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
                self.state.config.security.enable_nla = val;
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

            // =================================================================
            // Video Configuration
            // =================================================================
            Message::VideoEncoderChanged(encoder) => {
                self.state.config.video.encoder = encoder;
                self.state.mark_dirty();
                Task::none()
            }
            Message::VideoVaapiDeviceChanged(device) => {
                self.state.config.video.vaapi_device = PathBuf::from(device);
                self.state.mark_dirty();
                Task::none()
            }
            Message::VideoTargetFpsChanged(fps) => {
                self.state.config.video.target_fps = fps;
                self.state.mark_dirty();
                Task::none()
            }
            Message::VideoBitrateChanged(bitrate) => {
                self.state.config.video.bitrate = bitrate;
                self.state.mark_dirty();
                Task::none()
            }
            Message::VideoDamageTrackingToggled(val) => {
                self.state.config.video.damage_tracking = val;
                self.state.mark_dirty();
                Task::none()
            }
            Message::VideoCursorModeChanged(mode) => {
                self.state.config.video.cursor_mode = mode;
                self.state.mark_dirty();
                Task::none()
            }
            Message::VideoDetectGpus => Task::perform(
                async {
                    crate::gui::hardware::detect_gpus()
                        .into_iter()
                        .map(|gpu| gpu.to_state_gpu_info())
                        .collect()
                },
                Message::VideoGpusDetected,
            ),
            Message::VideoGpusDetected(gpus) => {
                self.state.detected_gpus = gpus;
                Task::none()
            }
            Message::VideoPipelineToggleExpanded => {
                self.state.video_pipeline_expanded = !self.state.video_pipeline_expanded;
                Task::none()
            }

            // Video Pipeline - Processor
            Message::ProcessorTargetFpsChanged(val) => {
                if let Ok(v) = val.parse() {
                    self.state.config.video_pipeline.processor.target_fps = v;
                    self.state.mark_dirty();
                }
                Task::none()
            }
            Message::ProcessorMaxQueueDepthChanged(val) => {
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

            // Video Pipeline - Dispatcher
            Message::DispatcherChannelSizeChanged(val) => {
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

            // Video Pipeline - Converter
            Message::ConverterBufferPoolSizeChanged(val) => {
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

            // =================================================================
            // Input Configuration
            // =================================================================
            Message::InputUseLibeiToggled(val) => {
                self.state.config.input.use_libei = val;
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

            // =================================================================
            // Clipboard Configuration
            // =================================================================
            Message::ClipboardEnabledToggled(val) => {
                self.state.config.clipboard.enabled = val;
                self.state.mark_dirty();
                Task::none()
            }
            Message::ClipboardMaxSizeChanged(val) => {
                if let Ok(v) = val.parse() {
                    self.state.config.clipboard.max_size = v;
                    self.state.mark_dirty();
                }
                Task::none()
            }
            Message::ClipboardRateLimitChanged(val) => {
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

            // =================================================================
            // Audio Configuration
            // =================================================================
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
                self.state.edit_strings.audio_opus_bitrate = val.clone();
                if let Ok(kbps) = val.parse::<u32>() {
                    self.state.config.audio.opus_bitrate = kbps * 1000; // Convert kbps to bps
                    self.state.mark_dirty();
                }
                Task::none()
            }

            // =================================================================
            // Multi-Monitor Configuration
            // =================================================================
            Message::MultimonEnabledToggled(val) => {
                self.state.config.multimon.enabled = val;
                self.state.mark_dirty();
                Task::none()
            }
            Message::MultimonMaxMonitorsChanged(val) => {
                if let Ok(v) = val.parse() {
                    self.state.config.multimon.max_monitors = v;
                    self.state.mark_dirty();
                }
                Task::none()
            }

            // =================================================================
            // Performance Configuration
            // =================================================================
            Message::PerformancePresetSelected(preset) => {
                apply_performance_preset(&mut self.state.config.performance, preset);
                self.state.active_preset = Some(preset.to_string().to_lowercase());
                self.state.mark_dirty();
                Task::none()
            }
            Message::PerformanceEncoderThreadsChanged(val) => {
                if let Ok(v) = val.parse() {
                    self.state.config.performance.encoder_threads = v;
                    self.state.mark_dirty();
                }
                Task::none()
            }
            Message::PerformanceNetworkThreadsChanged(val) => {
                if let Ok(v) = val.parse() {
                    self.state.config.performance.network_threads = v;
                    self.state.mark_dirty();
                }
                Task::none()
            }
            Message::PerformanceBufferPoolSizeChanged(val) => {
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
                if let Ok(v) = val.parse() {
                    self.state.config.performance.latency.balanced_max_delay_ms = v;
                    self.state.mark_dirty();
                }
                Task::none()
            }
            Message::LatencyQualityDelayChanged(val) => {
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

            // =================================================================
            // Logging Configuration
            // =================================================================
            Message::LoggingLevelChanged(level) => {
                self.state.config.logging.level = level;
                self.state.mark_dirty();
                Task::none()
            }
            Message::LoggingLogDirChanged(dir) => {
                self.state.config.logging.log_dir = if dir.is_empty() {
                    None
                } else {
                    Some(PathBuf::from(dir))
                };
                self.state.mark_dirty();
                Task::none()
            }
            Message::LoggingBrowseLogDir => Task::perform(
                async {
                    let folder = rfd::AsyncFileDialog::new().pick_folder().await;
                    folder.map(|f| f.path().to_path_buf())
                },
                Message::LoggingLogDirSelected,
            ),
            Message::LoggingLogDirSelected(path) => {
                if let Some(p) = path {
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
                self.state.config.logging.log_dir = None;
                self.state.mark_dirty();
                Task::none()
            }

            // =================================================================
            // EGFX Configuration (continued in next part due to size)
            // =================================================================
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
                if let Ok(v) = val.parse() {
                    self.state.config.egfx.max_frames_in_flight = v;
                    self.state.mark_dirty();
                }
                Task::none()
            }
            Message::EgfxFrameAckTimeoutChanged(val) => {
                if let Ok(v) = val.parse() {
                    self.state.config.egfx.frame_ack_timeout = v;
                    self.state.mark_dirty();
                }
                Task::none()
            }
            Message::EgfxPeriodicIdrIntervalChanged(val) => {
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
                if let Ok(v) = val.parse() {
                    self.state.config.egfx.qp_min = v;
                    self.state.mark_dirty();
                }
                Task::none()
            }
            Message::EgfxQpMaxChanged(val) => {
                if let Ok(v) = val.parse() {
                    self.state.config.egfx.qp_max = v;
                    self.state.mark_dirty();
                }
                Task::none()
            }
            Message::EgfxQpDefaultChanged(val) => {
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

            // =================================================================
            // Damage Tracking Configuration
            // =================================================================
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
                if let Ok(v) = val.parse() {
                    self.state.config.damage_tracking.pixel_threshold = v;
                    self.state.mark_dirty();
                }
                Task::none()
            }
            Message::DamageTrackingMergeDistanceChanged(val) => {
                if let Ok(v) = val.parse() {
                    self.state.config.damage_tracking.merge_distance = v;
                    self.state.mark_dirty();
                }
                Task::none()
            }
            Message::DamageTrackingMinRegionAreaChanged(val) => {
                if let Ok(v) = val.parse() {
                    self.state.config.damage_tracking.min_region_area = v;
                    self.state.mark_dirty();
                }
                Task::none()
            }

            // =================================================================
            // Hardware Encoding Configuration
            // =================================================================
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

            // =================================================================
            // Display Configuration
            // =================================================================
            Message::DisplayToggleExpanded => {
                self.state.display_expanded = !self.state.display_expanded;
                Task::none()
            }
            Message::DisplayAllowResizeToggled(val) => {
                self.state.config.display.allow_resize = val;
                self.state.mark_dirty();
                Task::none()
            }
            Message::DisplayAllowedResolutionsChanged(resolutions) => {
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
            Message::DisplayAllowRotationToggled(val) => {
                self.state.config.display.allow_rotation = val;
                self.state.mark_dirty();
                Task::none()
            }

            // =================================================================
            // Advanced Video Configuration
            // =================================================================
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

            // =================================================================
            // Cursor Configuration
            // =================================================================
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
                if let Ok(v) = val.parse() {
                    self.state.config.cursor.predictive_latency_threshold_ms = v;
                    self.state.mark_dirty();
                }
                Task::none()
            }
            Message::CursorUpdateFpsChanged(val) => {
                if let Ok(v) = val.parse() {
                    self.state.config.cursor.cursor_update_fps = v;
                    self.state.mark_dirty();
                }
                Task::none()
            }
            Message::PredictorHistorySizeChanged(val) => {
                if let Ok(v) = val.parse() {
                    self.state.config.cursor.predictor.history_size = v;
                    self.state.mark_dirty();
                }
                Task::none()
            }
            Message::PredictorLookaheadMsChanged(val) => {
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
                if let Ok(v) = val.parse() {
                    self.state.config.cursor.predictor.max_prediction_distance = v;
                    self.state.mark_dirty();
                }
                Task::none()
            }
            Message::PredictorMinVelocityThresholdChanged(val) => {
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

            // =================================================================
            // File Operations
            // =================================================================
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
                        self.state.mark_clean();
                        self.state.add_message(
                            MessageLevel::Success,
                            "Configuration loaded successfully".to_string(),
                        );
                    }
                    Err(e) => {
                        self.state.add_message(
                            MessageLevel::Error,
                            format!("Failed to load config: {}", e),
                        );
                    }
                }
                Task::none()
            }
            Message::SaveConfig => {
                let config = self.state.config.clone();
                let path = self.state.config_path.clone();
                Task::perform(
                    async move { crate::gui::file_ops::save_config(&config, &path) },
                    Message::ConfigSaved,
                )
            }
            Message::SaveConfigAs => {
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

            // =================================================================
            // Server Control
            // =================================================================
            Message::StartServer => {
                // Check if already running
                if self.server_process.is_some() {
                    self.state.add_message(
                        MessageLevel::Warning,
                        "Server is already running".to_string(),
                    );
                    return Task::none();
                }

                self.state.server_status = crate::gui::state::ServerStatus::Starting;
                self.state
                    .add_message(MessageLevel::Info, "Starting server...".to_string());

                // Create log channel
                let (tx, rx) = mpsc::unbounded_channel();
                self.log_receiver = Some(Arc::new(Mutex::new(rx)));

                // Start the server process
                let config = self.state.config.clone();
                match ServerProcess::start(&config, tx) {
                    Ok(process) => {
                        let pid = process.pid();
                        let address = process.address().to_string();
                        self.server_process = Some(process);

                        self.state.server_status = crate::gui::state::ServerStatus::Running {
                            connections: 0,
                            uptime: Duration::from_secs(0),
                            address,
                        };

                        self.state.add_message(
                            MessageLevel::Success,
                            format!("Server started (PID: {})", pid),
                        );
                    }
                    Err(e) => {
                        self.state.server_status =
                            crate::gui::state::ServerStatus::Error(e.to_string());
                        self.state.add_message(
                            MessageLevel::Error,
                            format!("Failed to start server: {}", e),
                        );
                        self.log_receiver = None;
                    }
                }
                Task::none()
            }
            Message::StopServer => {
                if let Some(mut process) = self.server_process.take() {
                    self.state
                        .add_message(MessageLevel::Info, "Stopping server...".to_string());

                    if let Err(e) = process.stop() {
                        self.state.add_message(
                            MessageLevel::Error,
                            format!("Error stopping server: {}", e),
                        );
                    } else {
                        self.state
                            .add_message(MessageLevel::Success, "Server stopped".to_string());
                    }

                    self.server_process = None;
                    self.log_receiver = None;
                    self.state.server_status = crate::gui::state::ServerStatus::Stopped;
                } else {
                    self.state
                        .add_message(MessageLevel::Warning, "Server is not running".to_string());
                }
                Task::none()
            }
            Message::RestartServer => {
                // Stop then start
                if self.server_process.is_some() {
                    self.state
                        .add_message(MessageLevel::Info, "Restarting server...".to_string());

                    // Stop first
                    if let Some(mut process) = self.server_process.take() {
                        let _ = process.stop();
                    }
                    self.log_receiver = None;

                    // Then start
                    let (tx, rx) = mpsc::unbounded_channel();
                    self.log_receiver = Some(Arc::new(Mutex::new(rx)));

                    let config = self.state.config.clone();
                    match ServerProcess::start(&config, tx) {
                        Ok(process) => {
                            let pid = process.pid();
                            let address = process.address().to_string();
                            self.server_process = Some(process);

                            self.state.server_status = crate::gui::state::ServerStatus::Running {
                                connections: 0,
                                uptime: Duration::from_secs(0),
                                address,
                            };

                            self.state.add_message(
                                MessageLevel::Success,
                                format!("Server restarted (PID: {})", pid),
                            );
                        }
                        Err(e) => {
                            self.state.server_status =
                                crate::gui::state::ServerStatus::Error(e.to_string());
                            self.state.add_message(
                                MessageLevel::Error,
                                format!("Failed to restart server: {}", e),
                            );
                            self.log_receiver = None;
                        }
                    }
                } else {
                    // Not running, just start
                    return self.update(Message::StartServer);
                }
                Task::none()
            }
            Message::ServerStatusUpdated(status) => {
                self.state.server_status = status;
                Task::none()
            }
            Message::ServerStarted(pid) => {
                self.state.add_message(
                    MessageLevel::Success,
                    format!("Server process started (PID: {})", pid),
                );
                Task::none()
            }
            Message::ServerExited(exit_code) => {
                self.server_process = None;
                self.log_receiver = None;

                let msg = if let Some(code) = exit_code {
                    format!("Server exited with code {}", code)
                } else {
                    "Server exited".to_string()
                };

                self.state.server_status = crate::gui::state::ServerStatus::Stopped;
                self.state.add_message(MessageLevel::Info, msg);
                Task::none()
            }
            Message::ServerLogReceived(message, level) => {
                // Add to log buffer
                let log_line = LogLine {
                    timestamp: chrono::Local::now().format("%H:%M:%S").to_string(),
                    level,
                    message: message.clone(),
                    raw: message,
                };
                self.state.add_log_line(log_line);
                Task::none()
            }
            Message::ServerStartFailed(error) => {
                self.server_process = None;
                self.log_receiver = None;
                self.state.server_status = crate::gui::state::ServerStatus::Error(error.clone());
                self.state
                    .add_message(MessageLevel::Error, format!("Server failed: {}", error));
                Task::none()
            }

            // =================================================================
            // Validation
            // =================================================================
            Message::ValidateConfig => {
                let result = crate::gui::validation::validate_config(&self.state.config);
                Task::perform(async move { result }, Message::ValidationComplete)
            }
            Message::ValidationComplete(result) => {
                self.state.validation = result.into();
                Task::none()
            }

            // =================================================================
            // Capabilities
            // =================================================================
            Message::RefreshCapabilities => Task::perform(
                async { crate::gui::capabilities::detect_capabilities() },
                Message::CapabilitiesDetected,
            ),
            Message::CapabilitiesDetected(caps) => {
                self.state.detected_capabilities = caps.ok();
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
                                    .map(|_| path)
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

            // =================================================================
            // Log Viewer
            // =================================================================
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
                // TODO: Implement log export
                self.state.add_message(
                    MessageLevel::Info,
                    "Log export not yet implemented".to_string(),
                );
                Task::none()
            }

            // =================================================================
            // UI State
            // =================================================================
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
            Message::WindowCloseRequested => {
                if self.state.is_dirty {
                    self.state.confirm_discard_dialog = true;
                    Task::none()
                } else {
                    iced::exit()
                }
            }
            Message::ConfirmDiscardChanges => {
                self.state.confirm_discard_dialog = false;
                iced::exit()
            }
            Message::CancelDiscardChanges => {
                self.state.confirm_discard_dialog = false;
                Task::none()
            }
            Message::Tick => {
                // Periodic updates (log refresh, status poll, etc.)
                Task::none()
            }
            Message::PollServerLogs => {
                // Poll logs from server process and update status
                self.poll_server_logs();
                Task::none()
            }
        }
    }

    /// Render the main view
    pub fn view(&self) -> Element<'_, Message> {
        let header = self.view_header();
        let tab_bar = self.view_tab_bar();
        let content = self.view_tab_content();
        let footer = self.view_footer();

        // Wrap content in scrollable
        let main_content = scrollable(content).height(Length::Fill);

        // Main layout with dark background
        container(column![header, tab_bar, main_content, footer,].spacing(0))
            .width(Length::Fill)
            .height(Length::Fill)
            .style(|_theme| container::Style {
                background: Some(iced::Background::Color(app_theme::colors::BACKGROUND)),
                ..Default::default()
            })
            .into()
    }

    /// Render the header
    fn view_header(&self) -> Element<'_, Message> {
        // Server status indicator and controls
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

        // Server status badge with pill shape
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

        // Server control buttons
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
                // Brand logo area
                row![
                    text("🐑").size(28),
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
                // Server controls
                server_controls,
                space().width(Length::Fill),
                // File operations
                button(text("Import").size(12))
                    .on_press(Message::LoadConfig)
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

    /// Render the tab bar
    fn view_tab_bar(&self) -> Element<'_, Message> {
        let tabs: Vec<Element<'_, Message>> = Tab::all()
            .iter()
            .map(|&tab| {
                let is_active = self.current_tab == tab;
                button(
                    row![text(tab.icon()).size(14), text(tab.display_name()),]
                        .spacing(6)
                        .align_y(Alignment::Center),
                )
                .on_press(Message::TabSelected(tab))
                .padding([8, 16])
                .style(app_theme::tab_button_style(is_active))
                .into()
            })
            .collect();

        container(
            row(tabs)
                .spacing(4)
                .padding([8, 20])
                .align_y(Alignment::Center),
        )
        .style(|_theme| container::Style {
            background: Some(iced::Background::Color(app_theme::colors::SURFACE_DARK)),
            ..Default::default()
        })
        .width(Length::Fill)
        .into()
    }

    /// Render the current tab content
    fn view_tab_content(&self) -> Element<'_, Message> {
        let content = match self.current_tab {
            Tab::Server => tabs::view_server_tab(&self.state),
            Tab::Security => tabs::view_security_tab(&self.state),
            Tab::Video => tabs::view_video_tab(&self.state),
            Tab::Audio => tabs::view_audio_tab(&self.state),
            Tab::Input => tabs::view_input_tab(&self.state),
            Tab::Clipboard => tabs::view_clipboard_tab(&self.state),
            Tab::Logging => tabs::view_logging_tab(&self.state),
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
        ];

        // If server is running, add a faster tick for log polling and uptime updates
        if self.server_process.is_some() {
            subscriptions.push(
                iced::time::every(Duration::from_millis(100)).map(|_| Message::PollServerLogs),
            );
        }

        Subscription::batch(subscriptions)
    }

    /// Poll server logs from the receiver channel
    fn poll_server_logs(&mut self) {
        if let Some(ref receiver) = self.log_receiver {
            let mut receiver_guard = receiver.lock();

            // Drain all available log lines
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

        // Check if server process is still running
        if let Some(ref process) = self.server_process {
            if !process.is_running() {
                // Process exited
                self.server_process = None;
                self.log_receiver = None;
                self.state.server_status = crate::gui::state::ServerStatus::Stopped;
                self.state.add_message(
                    MessageLevel::Warning,
                    "Server process exited unexpectedly".to_string(),
                );
            } else {
                // Update uptime
                if let crate::gui::state::ServerStatus::Running {
                    ref mut uptime,
                    connections: _,
                    address: _,
                } = self.state.server_status
                {
                    *uptime = process.uptime();
                }
            }
        }
    }
}

// =============================================================================
// Preset Application Helpers
// =============================================================================

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
