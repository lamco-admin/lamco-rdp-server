//! Advanced Configuration Tab
//!
//! Combines damage tracking, hardware encoding, display/multimon, video pipeline,
//! logging, advanced video, and cursor settings.

use iced::{
    widget::{button, column, container, pick_list, row, slider, text},
    Alignment, Element, Length,
};

use crate::gui::{
    message::{DamageTrackingPreset, Message, MultimonPreset},
    state::AppState,
    theme, widgets,
    widgets::space,
};

const DAMAGE_METHODS: &[&str] = &["diff", "pipewire", "hybrid"];
const HW_QUALITY_PRESETS: &[&str] = &["speed", "balanced", "quality"];
const LOG_LEVELS: &[&str] = &["trace", "debug", "info", "warn", "error"];

/// Superset of video.rs modes: adds "painted" and "predictive" for advanced use.
const CURSOR_MODES: &[&str] = &["metadata", "painted", "hidden", "predictive"];

const MULTIMON_PRESETS: &[MultimonPreset] = &[
    MultimonPreset::Single,
    MultimonPreset::Dual,
    MultimonPreset::Triple,
    MultimonPreset::Quad,
    MultimonPreset::Custom,
];

pub fn view_advanced_tab(state: &AppState) -> Element<'_, Message> {
    column![
        // Section header
        widgets::section_header("Advanced Configuration"),
        space().height(16.0),
        // Damage Tracking section
        widgets::collapsible_header(
            "Damage Tracking",
            state.damage_tracking_expanded,
            Message::DamageTrackingToggleExpanded,
        ),
        if state.damage_tracking_expanded {
            view_damage_tracking_config(state)
        } else {
            column![].into()
        },
        space().height(12.0),
        // Hardware Encoding section
        widgets::collapsible_header(
            "Hardware Encoding",
            state.hardware_encoding_expanded,
            Message::HardwareEncodingToggleExpanded,
        ),
        if state.hardware_encoding_expanded {
            view_hardware_encoding_config(state)
        } else {
            column![].into()
        },
        space().height(12.0),
        // Display Control section (includes multimon)
        widgets::collapsible_header(
            "Display Control",
            state.display_expanded,
            Message::DisplayToggleExpanded,
        ),
        if state.display_expanded {
            view_display_config(state)
        } else {
            column![].into()
        },
        space().height(12.0),
        // Video Pipeline section
        widgets::collapsible_header(
            "Video Pipeline",
            state.video_pipeline_expanded,
            Message::VideoPipelineToggleExpanded,
        ),
        if state.video_pipeline_expanded {
            view_video_pipeline_config(state)
        } else {
            column![].into()
        },
        space().height(12.0),
        // Advanced Video section
        widgets::collapsible_header(
            "Advanced Video",
            state.advanced_video_expanded,
            Message::AdvancedVideoToggleExpanded,
        ),
        if state.advanced_video_expanded {
            view_advanced_video_config(state)
        } else {
            column![].into()
        },
        space().height(12.0),
        // Cursor Configuration section
        widgets::collapsible_header(
            "Cursor Configuration",
            state.cursor_expanded,
            Message::CursorToggleExpanded,
        ),
        if state.cursor_expanded {
            view_cursor_config(state)
        } else {
            column![].into()
        },
        space().height(12.0),
        // Logging & Diagnostics section
        widgets::collapsible_header(
            "Logging & Diagnostics",
            state.logging_expanded,
            Message::LoggingToggleExpanded,
        ),
        if state.logging_expanded {
            view_logging_config(state)
        } else {
            column![].into()
        },
    ]
    .spacing(4)
    .padding(20)
    .into()
}

/// Damage tracking configuration view
fn view_damage_tracking_config(state: &AppState) -> Element<'_, Message> {
    let damage = &state.config.damage_tracking;

    column![
        space().height(8.0),
        widgets::toggle_with_help(
            "Enable Damage Tracking",
            damage.enabled,
            "Only encode changed regions (significant bandwidth savings)",
            Message::DamageTrackingEnabledToggled,
        ),
        space().height(12.0),
        widgets::labeled_row(
            "Detection Method:",
            150.0,
            pick_list(DAMAGE_METHODS.to_vec(), Some(damage.method.as_str()), |s| {
                Message::DamageTrackingMethodChanged(s.to_string())
            },)
            .width(Length::Fixed(150.0))
            .into(),
        ),
        space().height(4.0),
        text("Diff: CPU pixel comparison | PipeWire: Compositor hints | Hybrid: Both")
            .size(12)
            .style(|_theme| text::Style {
                color: Some(theme::colors::TEXT_MUTED),
            }),
        space().height(12.0),
        // Sensitivity presets
        text("Sensitivity Presets:").size(13),
        space().height(8.0),
        row![
            button(text("Text Work"))
                .on_press(Message::DamageTrackingPresetSelected(
                    DamageTrackingPreset::TextWork
                ))
                .padding([6, 12])
                .style(theme::secondary_button_style),
            button(text("General"))
                .on_press(Message::DamageTrackingPresetSelected(
                    DamageTrackingPreset::General
                ))
                .padding([6, 12])
                .style(theme::secondary_button_style),
            button(text("Video"))
                .on_press(Message::DamageTrackingPresetSelected(
                    DamageTrackingPreset::Video
                ))
                .padding([6, 12])
                .style(theme::secondary_button_style),
        ]
        .spacing(8),
        space().height(12.0),
        widgets::labeled_row_with_help(
            "Tile Size:",
            150.0,
            row![
                widgets::number_input(
                    &state.edit_strings.tile_size,
                    "16",
                    60.0,
                    Message::DamageTrackingTileSizeChanged,
                ),
                text(" pixels"),
            ]
            .align_y(Alignment::Center)
            .into(),
            "16x16 matches FreeRDP (max sensitivity)",
        ),
        space().height(8.0),
        widgets::labeled_row(
            "Diff Threshold:",
            150.0,
            row![
                widgets::float_slider(
                    damage.diff_threshold,
                    Message::DamageTrackingDiffThresholdChanged,
                ),
                text(format!(" ({}%)", (damage.diff_threshold * 100.0) as u32)),
            ]
            .align_y(Alignment::Center)
            .into(),
        ),
        space().height(8.0),
        widgets::labeled_row(
            "Pixel Threshold:",
            150.0,
            widgets::number_input(
                &state.edit_strings.pixel_threshold,
                "1",
                60.0,
                Message::DamageTrackingPixelThresholdChanged,
            ),
        ),
        space().height(8.0),
        widgets::labeled_row(
            "Merge Distance:",
            150.0,
            row![
                widgets::number_input(
                    &state.edit_strings.merge_distance,
                    "16",
                    60.0,
                    Message::DamageTrackingMergeDistanceChanged,
                ),
                text(" pixels"),
            ]
            .align_y(Alignment::Center)
            .into(),
        ),
        space().height(8.0),
        widgets::labeled_row(
            "Min Region Area:",
            150.0,
            row![
                widgets::number_input(
                    &state.edit_strings.min_region_area,
                    "64",
                    60.0,
                    Message::DamageTrackingMinRegionAreaChanged,
                ),
                text(" pixels\u{00B2}"),
            ]
            .align_y(Alignment::Center)
            .into(),
        ),
    ]
    .padding([0, 16])
    .into()
}

/// Hardware encoding configuration view
fn view_hardware_encoding_config(state: &AppState) -> Element<'_, Message> {
    let hw = &state.config.hardware_encoding;

    column![
        space().height(8.0),
        text("Hardware encoding requires display handler integration (OpenH264 software currently used)")
            .size(12)
            .style(|_theme| text::Style {
                color: Some(theme::colors::TEXT_MUTED),
            }),
        space().height(12.0),
        widgets::toggle_pending_with_note(
            "Enable Hardware Acceleration",
            hw.enabled,
            Message::HardwareEncodingEnabledToggled,
            "VA-API/NVENC encoder factory exists but not wired into display pipeline",
        ),
        space().height(12.0),
        // Display detected GPUs
        if !state.detected_gpus.is_empty() {
            let gpu_text: Vec<_> = state
                .detected_gpus
                .iter()
                .map(|gpu| {
                    format!(
                        "{} {} ({})",
                        gpu.vendor,
                        gpu.model,
                        gpu.driver
                    )
                })
                .collect();
            Element::from(column![
                text("Detected GPUs:").size(13),
                text(gpu_text.join("\n")).size(12),
                space().height(12.0),
            ])
        } else {
            Element::from(space().height(0.0))
        },
        widgets::labeled_row_pending_with_note(
            "VA-API Device:",
            150.0,
            pick_list(
                vec!["/dev/dri/renderD128", "/dev/dri/renderD129"],
                Some(state.edit_strings.vaapi_device.as_str()),
                |s| Message::HardwareEncodingVaapiDeviceChanged(s.to_string()),
            )
            .width(Length::Fixed(200.0))
            .into(),
            "Encoder factory supports device selection; awaiting pipeline integration",
        ),
        space().height(12.0),
        widgets::toggle_pending_with_note(
            "Enable DMA-BUF Zero-Copy",
            hw.enable_dmabuf_zerocopy,
            Message::HardwareEncodingDmabufZerocopyToggled,
            "Requires VA-API encoder to be active for zero-copy buffer path",
        ),
        space().height(8.0),
        widgets::toggle_pending_with_note(
            "Fallback to Software",
            hw.fallback_to_software,
            Message::HardwareEncodingFallbackToSoftwareToggled,
            "Active once hardware encoding is integrated; falls back to OpenH264",
        ),
        space().height(8.0),
        widgets::toggle_pending_with_note(
            "Prefer NVENC over VA-API",
            hw.prefer_nvenc,
            Message::HardwareEncodingPreferNvencToggled,
            "Encoder factory supports NVENC; awaiting pipeline integration",
        ),
        space().height(12.0),
        widgets::labeled_row_pending_with_note(
            "Quality Preset:",
            150.0,
            pick_list(
                HW_QUALITY_PRESETS.to_vec(),
                Some(hw.quality_preset.as_str()),
                |s| Message::HardwareEncodingQualityPresetChanged(s.to_string()),
            )
            .width(Length::Fixed(150.0))
            .into(),
            "Maps to VA-API/NVENC quality levels; awaiting pipeline integration",
        ),
    ]
    .padding([0, 16])
    .into()
}

/// Display control configuration view (includes multimon)
fn view_display_config(state: &AppState) -> Element<'_, Message> {
    let display = &state.config.display;

    // Determine current preset based on max_monitors
    let current_preset = match state.config.multimon.max_monitors {
        1 => Some(MultimonPreset::Single),
        2 => Some(MultimonPreset::Dual),
        3 => Some(MultimonPreset::Triple),
        4 => Some(MultimonPreset::Quad),
        _ => Some(MultimonPreset::Custom),
    };

    column![
        space().height(8.0),
        // Multi-monitor subsection
        widgets::subsection_header("Multi-Monitor"),
        space().height(8.0),
        widgets::toggle_with_help(
            "Enable Multi-Monitor Support",
            state.config.multimon.enabled,
            "Allow clients to connect to multiple monitors",
            Message::MultimonEnabledToggled,
        ),
        space().height(12.0),
        widgets::labeled_row(
            "Display Configuration:",
            150.0,
            row![
                pick_list(
                    MULTIMON_PRESETS.to_vec(),
                    current_preset,
                    Message::MultimonPresetSelected,
                )
                .width(Length::Fixed(200.0)),
                space().width(16.0),
                text(format!("{} monitor(s)", state.config.multimon.max_monitors))
                    .size(13)
                    .style(|_theme| text::Style {
                        color: Some(theme::colors::TEXT_SECONDARY),
                    }),
            ]
            .align_y(Alignment::Center)
            .into(),
        ),
        space().height(8.0),
        widgets::labeled_row(
            "Maximum Monitors:",
            150.0,
            row![
                slider(1..=16, state.config.multimon.max_monitors as u8, |v| {
                    Message::MultimonMaxMonitorsChanged(v.to_string())
                },)
                .width(Length::Fixed(200.0)),
                space().width(16.0),
                container(text(&state.edit_strings.max_monitors).size(14))
                    .width(Length::Fixed(40.0))
                    .center_x(Length::Fill),
            ]
            .align_y(Alignment::Center)
            .into(),
        ),
        space().height(4.0),
        text("Portal provides all selected monitors; max setting is advisory")
            .size(11)
            .style(|_theme| text::Style {
                color: Some(theme::colors::TEXT_MUTED),
            }),
        space().height(16.0),
        // Display control subsection
        widgets::subsection_header("Display Control"),
        space().height(8.0),
        widgets::toggle_with_help(
            "Allow Dynamic Resolution",
            display.allow_resize,
            "Clients can request resolution changes via MS-RDPEDISP",
            Message::DisplayAllowResizeToggled,
        ),
        space().height(8.0),
        widgets::toggle_pending_with_note(
            "DPI Aware",
            display.dpi_aware,
            Message::DisplayDpiAwareToggled,
            "Portal doesn't expose DPI; needs per-monitor DPI detection",
        ),
        space().height(8.0),
        widgets::toggle_pending_with_note(
            "Allow Rotation",
            display.allow_rotation,
            Message::DisplayAllowRotationToggled,
            "Rotation transform not yet handled in PipeWire capture pipeline",
        ),
        space().height(12.0),
        text("Allowed Resolutions (empty = all):")
            .size(13)
            .style(|_theme| text::Style {
                color: Some(theme::colors::TEXT_MUTED),
            }),
        space().height(4.0),
        widgets::text_area(
            &state.edit_strings.resolutions_text,
            "1920x1080\n2560x1440\n3840x2160",
            80.0,
            Message::DisplayAllowedResolutionsChanged,
        ),
        text("Resolution filtering not yet implemented")
            .size(11)
            .style(|_theme| text::Style {
                color: Some(theme::colors::TEXT_MUTED),
            }),
    ]
    .padding([0, 16])
    .into()
}

/// Video pipeline configuration view
fn view_video_pipeline_config(state: &AppState) -> Element<'_, Message> {
    column![
        space().height(8.0),
        text("Video pipeline architecture reserved for future use")
            .size(12)
            .style(|_theme| text::Style {
                color: Some(theme::colors::TEXT_MUTED),
            }),
        space().height(12.0),
        // Processor section
        widgets::subsection_header("Frame Processor"),
        space().height(8.0),
        widgets::labeled_row(
            "Max Queue Depth:",
            150.0,
            widgets::number_input(
                &state.edit_strings.max_queue_depth,
                "30",
                80.0,
                Message::ProcessorMaxQueueDepthChanged,
            ),
        ),
        space().height(8.0),
        widgets::toggle_with_help(
            "Adaptive Quality",
            state.config.video_pipeline.processor.adaptive_quality,
            "Adjust quality based on network conditions",
            Message::ProcessorAdaptiveQualityToggled,
        ),
        space().height(8.0),
        widgets::labeled_row(
            "Damage Threshold:",
            150.0,
            widgets::float_slider(
                state.config.video_pipeline.processor.damage_threshold,
                Message::ProcessorDamageThresholdChanged,
            ),
        ),
        space().height(8.0),
        widgets::toggle_with_help(
            "Drop on Full Queue",
            state.config.video_pipeline.processor.drop_on_full_queue,
            "Drop frames when queue is full",
            Message::ProcessorDropOnFullQueueToggled,
        ),
        space().height(8.0),
        widgets::toggle_with_help(
            "Enable Metrics",
            state.config.video_pipeline.processor.enable_metrics,
            "Collect pipeline performance metrics",
            Message::ProcessorEnableMetricsToggled,
        ),
        space().height(16.0),
        // Dispatcher section
        widgets::subsection_header("Frame Dispatcher"),
        space().height(8.0),
        widgets::labeled_row(
            "Channel Size:",
            150.0,
            widgets::number_input(
                &state.edit_strings.channel_size,
                "30",
                80.0,
                Message::DispatcherChannelSizeChanged,
            ),
        ),
        space().height(8.0),
        widgets::toggle_with_help(
            "Priority Dispatch",
            state.config.video_pipeline.dispatcher.priority_dispatch,
            "Prioritize certain frame types",
            Message::DispatcherPriorityDispatchToggled,
        ),
        space().height(8.0),
        widgets::labeled_row(
            "Max Frame Age:",
            150.0,
            row![
                widgets::number_input(
                    &state.edit_strings.max_frame_age,
                    "150",
                    80.0,
                    Message::DispatcherMaxFrameAgeChanged,
                ),
                text(" ms"),
            ]
            .align_y(Alignment::Center)
            .into(),
        ),
        space().height(8.0),
        widgets::toggle_with_help(
            "Enable Backpressure",
            state.config.video_pipeline.dispatcher.enable_backpressure,
            "Slow down when downstream is congested",
            Message::DispatcherEnableBackpressureToggled,
        ),
        space().height(8.0),
        widgets::labeled_row(
            "High Water Mark:",
            150.0,
            widgets::float_slider(
                state.config.video_pipeline.dispatcher.high_water_mark,
                Message::DispatcherHighWaterMarkChanged,
            ),
        ),
        space().height(8.0),
        widgets::labeled_row(
            "Low Water Mark:",
            150.0,
            widgets::float_slider(
                state.config.video_pipeline.dispatcher.low_water_mark,
                Message::DispatcherLowWaterMarkChanged,
            ),
        ),
        space().height(8.0),
        widgets::toggle_with_help(
            "Load Balancing",
            state.config.video_pipeline.dispatcher.load_balancing,
            "Balance load across encoders",
            Message::DispatcherLoadBalancingToggled,
        ),
        space().height(16.0),
        // Converter section
        widgets::subsection_header("Bitmap Converter"),
        space().height(8.0),
        widgets::labeled_row(
            "Buffer Pool Size:",
            150.0,
            widgets::number_input(
                &state.edit_strings.converter_buffer_pool_size,
                "8",
                80.0,
                Message::ConverterBufferPoolSizeChanged,
            ),
        ),
        space().height(8.0),
        widgets::toggle_with_help(
            "Enable SIMD",
            state.config.video_pipeline.converter.enable_simd,
            "Use SIMD acceleration for conversion",
            Message::ConverterEnableSimdToggled,
        ),
        space().height(8.0),
        widgets::labeled_row(
            "Damage Threshold:",
            150.0,
            widgets::float_slider(
                state.config.video_pipeline.converter.damage_threshold,
                Message::ConverterDamageThresholdChanged,
            ),
        ),
        space().height(8.0),
        widgets::toggle_with_help(
            "Enable Statistics",
            state.config.video_pipeline.converter.enable_statistics,
            "Collect conversion statistics",
            Message::ConverterEnableStatisticsToggled,
        ),
    ]
    .spacing(4)
    .padding([0, 16])
    .into()
}

/// Advanced video configuration view
fn view_advanced_video_config(state: &AppState) -> Element<'_, Message> {
    let av = &state.config.advanced_video;

    column![
        space().height(8.0),
        widgets::toggle_with_help(
            "Enable Frame Skip",
            av.enable_frame_skip,
            "Allow OpenH264 encoder to skip frames under load",
            Message::AdvancedVideoEnableFrameSkipToggled,
        ),
        space().height(8.0),
        widgets::labeled_row_pending_with_note(
            "Scene Change Threshold:",
            180.0,
            widgets::float_slider(
                av.scene_change_threshold,
                Message::AdvancedVideoSceneChangeThresholdChanged,
            ),
            "Scene detection not implemented; damage tracking handles this indirectly",
        ),
        space().height(8.0),
        widgets::labeled_row_pending_with_note(
            "Intra Refresh Interval:",
            180.0,
            row![
                widgets::number_input(
                    &state.edit_strings.intra_refresh,
                    "300",
                    60.0,
                    Message::AdvancedVideoIntraRefreshIntervalChanged,
                ),
                text(" frames"),
            ]
            .align_y(Alignment::Center)
            .into(),
            "Use Periodic Keyframe in EGFX tab instead (controls IDR interval)",
        ),
        space().height(8.0),
        widgets::toggle_pending_with_note(
            "Enable Adaptive Quality",
            av.enable_adaptive_quality,
            Message::AdvancedVideoEnableAdaptiveQualityToggled,
            "Needs network bandwidth feedback to dynamically adjust QP",
        ),
    ]
    .padding([0, 16])
    .into()
}

/// Cursor configuration view
fn view_cursor_config(state: &AppState) -> Element<'_, Message> {
    let cursor = &state.config.cursor;

    column![
        space().height(8.0),
        text("Cursor handling uses metadata mode (lowest latency). Advanced modes need implementation.")
            .size(12)
            .style(|_theme| text::Style {
                color: Some(theme::colors::TEXT_MUTED),
            }),
        space().height(12.0),
        widgets::labeled_row_with_help(
            "Cursor Mode:",
            150.0,
            pick_list(CURSOR_MODES.to_vec(), Some(cursor.mode.as_str()), |s| {
                Message::CursorModeChanged(s.to_string())
            },)
            .width(Length::Fixed(150.0))
            .into(),
            "Metadata = client cursor, Painted = composited, Hidden = off, Predictive = physics",
        ),
        space().height(8.0),
        text(
            "Metadata - Client renders cursor (lowest latency)\n\
              Painted - Cursor composited into video\n\
              Hidden - No cursor (touch/pen)\n\
              Predictive - Physics-based prediction"
        )
        .size(12)
        .style(|_theme| text::Style {
            color: Some(theme::colors::TEXT_MUTED),
        }),
        space().height(12.0),
        widgets::toggle_with_help(
            "Auto Mode Selection",
            cursor.auto_mode,
            "Automatically switch cursor mode based on measured latency",
            Message::CursorAutoModeToggled,
        ),
        space().height(8.0),
        widgets::labeled_row_with_help(
            "Predictive Threshold:",
            150.0,
            row![
                widgets::number_input(
                    &state.edit_strings.predictive_threshold,
                    "100",
                    60.0,
                    Message::CursorPredictiveThresholdChanged,
                ),
                text(" ms"),
            ]
            .align_y(Alignment::Center)
            .into(),
            "Latency above this triggers predictive cursor rendering",
        ),
        space().height(8.0),
        widgets::labeled_row_pending_with_note(
            "Cursor Update FPS:",
            150.0,
            widgets::number_input(
                &state.edit_strings.cursor_update_fps,
                "60",
                60.0,
                Message::CursorUpdateFpsChanged,
            ),
            "Painted mode cursor compositing not yet implemented",
        ),
        space().height(12.0),
        // Predictor sub-section
        widgets::collapsible_header(
            "Predictor Configuration (Future)",
            state.cursor_predictor_expanded,
            Message::CursorPredictorToggleExpanded,
        ),
        if state.cursor_predictor_expanded {
            view_cursor_predictor_config(state)
        } else {
            column![].into()
        },
    ]
    .padding([0, 16])
    .into()
}

/// Cursor predictor configuration view
fn view_cursor_predictor_config(state: &AppState) -> Element<'_, Message> {
    let pred = &state.config.cursor.predictor;

    column![
        space().height(8.0),
        text("Predictor requires physics-based cursor mode implementation")
            .size(12)
            .style(|_theme| text::Style {
                color: Some(theme::colors::TEXT_MUTED),
            }),
        space().height(8.0),
        widgets::labeled_row(
            "History Size:",
            180.0,
            widgets::number_input(
                &state.edit_strings.history_size,
                "8",
                60.0,
                Message::PredictorHistorySizeChanged,
            ),
        ),
        space().height(4.0),
        widgets::labeled_row(
            "Lookahead (ms):",
            180.0,
            widgets::number_input(
                &state.edit_strings.lookahead,
                "50.0",
                60.0,
                Message::PredictorLookaheadMsChanged,
            ),
        ),
        space().height(4.0),
        widgets::labeled_row(
            "Velocity Smoothing:",
            180.0,
            widgets::float_slider(
                pred.velocity_smoothing,
                Message::PredictorVelocitySmoothingChanged,
            ),
        ),
        space().height(4.0),
        widgets::labeled_row(
            "Accel Smoothing:",
            180.0,
            widgets::float_slider(
                pred.acceleration_smoothing,
                Message::PredictorAccelerationSmoothingChanged,
            ),
        ),
        space().height(4.0),
        widgets::labeled_row(
            "Max Prediction Dist:",
            180.0,
            row![
                widgets::number_input(
                    &state.edit_strings.max_pred_dist,
                    "100",
                    60.0,
                    Message::PredictorMaxPredictionDistanceChanged,
                ),
                text(" pixels"),
            ]
            .align_y(Alignment::Center)
            .into(),
        ),
        space().height(4.0),
        widgets::labeled_row(
            "Min Velocity Threshold:",
            180.0,
            widgets::number_input(
                &state.edit_strings.min_velocity,
                "50.0",
                60.0,
                Message::PredictorMinVelocityThresholdChanged,
            ),
        ),
        space().height(4.0),
        widgets::labeled_row(
            "Stop Convergence:",
            180.0,
            widgets::float_slider(
                pred.stop_convergence_rate,
                Message::PredictorStopConvergenceRateChanged,
            ),
        ),
    ]
    .padding([0, 16])
    .into()
}

/// Logging & Diagnostics configuration view
fn view_logging_config(state: &AppState) -> Element<'_, Message> {
    // Build the log directory widget before the column to avoid lifetime issues.
    // Flatpak: read-only display with fixed sandbox path.
    // Non-Flatpak: editable path input with browse/clear.
    let log_dir_widget: Element<'_, Message> = if crate::config::is_flatpak() {
        let dir_str = crate::config::resolve_log_dir(&None).display().to_string();
        column![
            container(text(dir_str).size(14))
                .padding([8, 12])
                .width(Length::Fill)
                .style(theme::path_display_style),
            space().height(4.0),
            text("Log directory is fixed in Flatpak (sandbox policy)")
                .size(11)
                .style(|_theme| text::Style {
                    color: Some(theme::colors::TEXT_MUTED),
                }),
        ]
        .into()
    } else {
        column![
            row![
                widgets::path_input(
                    &state.edit_strings.log_dir,
                    "Leave empty for console-only",
                    Message::LoggingLogDirChanged,
                    Message::LoggingBrowseLogDir,
                ),
                space().width(8.0),
                button(text("Clear"))
                    .on_press(Message::LoggingClearLogDir)
                    .padding([6, 12])
                    .style(theme::secondary_button_style),
            ],
            space().height(4.0),
            text("Leave empty for console-only logging")
                .size(11)
                .style(|_theme| text::Style {
                    color: Some(theme::colors::TEXT_MUTED),
                }),
        ]
        .into()
    };
    column![
        space().height(8.0),
        // Log level
        widgets::labeled_row_with_help(
            "Log Level:",
            150.0,
            pick_list(
                LOG_LEVELS.to_vec(),
                Some(state.config.logging.level.as_str()),
                |s| Message::LoggingLevelChanged(s.to_string()),
            )
            .width(Length::Fixed(150.0))
            .into(),
            "Trace: Everything | Debug: Verbose | Info: Normal | Warn/Error: Minimal",
        ),
        space().height(16.0),
        // Log output info
        text("Log Output:").size(13),
        space().height(4.0),
        text("Console output (stdout) is always enabled")
            .size(12)
            .style(|_theme| text::Style {
                color: Some(theme::colors::TEXT_MUTED),
            }),
        space().height(12.0),
        // Log directory (file logging)
        text("Log Directory (for file logging):").size(13),
        space().height(4.0),
        log_dir_widget,
        space().height(16.0),
        // Metrics toggle
        widgets::toggle_pending_with_note(
            "Enable Performance Metrics",
            state.config.logging.metrics,
            Message::LoggingMetricsToggled,
            "Metrics collection not yet implemented",
        ),
    ]
    .padding([0, 16])
    .into()
}
