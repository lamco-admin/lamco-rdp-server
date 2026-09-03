//! Advanced Configuration Tab
//!
//! Video pipeline tuning, advanced video (frame skip/scene change/intra
//! refresh), and logging/diagnostics. Damage tracking, hardware encoding,
//! display/multimon, and cursor settings each have their own home now (see
//! tabs/mod.rs) — this tab is expert-only knobs that don't fit elsewhere.

use iced::{
    Alignment, Element, Length,
    widget::{button, column, container, pick_list, row, text},
};

use crate::gui::{message::Message, state::AppState, theme, widgets, widgets::space};

const LOG_LEVELS: &[&str] = &["trace", "debug", "info", "warn", "error"];

pub fn view_advanced_tab(state: &AppState) -> Element<'_, Message> {
    column![
        // Section header
        widgets::section_header("Advanced Configuration"),
        space().height(16.0),
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
