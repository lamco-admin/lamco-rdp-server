//! Performance Configuration Tab
//!
//! Threading, damage tracking, adaptive FPS, latency governor, and metrics
//! exposure settings.

use iced::{
    Alignment, Element, Length,
    widget::{button, column, pick_list, row, slider, text},
};

use crate::gui::{
    message::{DamageTrackingPreset, Message, PerformancePreset},
    state::AppState,
    theme, widgets,
    widgets::space,
};

const LATENCY_MODES: &[&str] = &["interactive", "balanced", "quality"];
const DAMAGE_METHODS: &[&str] = &["diff", "pipewire", "hybrid"];

pub fn view_performance_tab(state: &AppState) -> Element<'_, Message> {
    let mut content = column![
        // Section header
        widgets::section_header("Performance Configuration"),
        space().height(16.0),
    ]
    .spacing(4);

    // Live metrics summary (when available, before config controls)
    if let Some(ref metrics) = state.live_metrics {
        content = content
            .push(row![
                text(format!(
                    "Live: {} FPS | {:.0}ms latency | {} ({}) | queue {}",
                    metrics.fps,
                    metrics.latency_ms,
                    metrics.activity_level,
                    if metrics.damage_source.is_empty() {
                        "unknown"
                    } else {
                        &metrics.damage_source
                    },
                    metrics.queue_depth
                ))
                .size(13)
                .color(crate::gui::theme::colors::TEXT_MUTED),
            ])
            .push(space().height(12.0));
    }

    content = content.push(
        column![
            // Preset buttons
            text("Preset Profiles:").size(14),
            space().height(8.0),
            row![
                button(text("Interactive"))
                    .on_press(Message::PerformancePresetSelected(
                        PerformancePreset::Interactive
                    ))
                    .padding([8, 16])
                    .style(theme::preset_button_style(
                        state.active_preset.as_deref() == Some("interactive")
                    )),
                button(text("Balanced"))
                    .on_press(Message::PerformancePresetSelected(
                        PerformancePreset::Balanced
                    ))
                    .padding([8, 16])
                    .style(theme::preset_button_style(
                        state.active_preset.as_deref() == Some("balanced")
                    )),
                button(text("Quality"))
                    .on_press(Message::PerformancePresetSelected(
                        PerformancePreset::Quality
                    ))
                    .padding([8, 16])
                    .style(theme::preset_button_style(
                        state.active_preset.as_deref() == Some("quality")
                    )),
            ]
            .spacing(8),
            space().height(8.0),
            text("Interactive: <50ms latency | Balanced: <100ms | Quality: Best image quality")
                .size(12)
                .style(|_theme: &iced::Theme| text::Style {
                    color: Some(theme::colors::TEXT_MUTED),
                }),
            space().height(20.0),
            // Threading section
            widgets::subsection_header("Threading"),
            space().height(8.0),
            widgets::labeled_row_with_help(
                "Encoder Threads:",
                150.0,
                widgets::number_input(
                    &state.edit_strings.encoder_threads,
                    "0",
                    80.0,
                    Message::PerformanceEncoderThreadsChanged,
                ),
                "0 = Auto-detect CPU cores, or specify 1-16",
            ),
            space().height(12.0),
            widgets::labeled_row_pending_with_note(
                "Network Threads:",
                150.0,
                widgets::number_input(
                    &state.edit_strings.network_threads,
                    "0",
                    80.0,
                    Message::PerformanceNetworkThreadsChanged,
                ),
                "Tokio runtime uses default multi-threaded executor",
            ),
            space().height(12.0),
            widgets::labeled_row_pending_with_note(
                "Buffer Pool Size:",
                150.0,
                widgets::number_input(
                    &state.edit_strings.buffer_pool_size,
                    "16",
                    80.0,
                    Message::PerformanceBufferPoolSizeChanged,
                ),
                "Frame buffer pool not yet implemented",
            ),
            space().height(12.0),
            widgets::toggle_pending_with_note(
                "Enable Zero-Copy Operations",
                state.config.performance.zero_copy,
                Message::PerformanceZeroCopyToggled,
                "Auto-detected from compositor DMA-BUF support; manual override not yet wired",
            ),
            space().height(20.0),
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
            space().height(16.0),
            // Adaptive FPS section
            widgets::collapsible_header(
                "Adaptive FPS",
                state.adaptive_fps_expanded,
                Message::PerformanceAdaptiveFpsToggleExpanded,
            ),
            if state.adaptive_fps_expanded {
                view_adaptive_fps_config(state)
            } else {
                column![].into()
            },
            space().height(16.0),
            // Latency Governor section
            widgets::collapsible_header(
                "Latency Governor",
                state.latency_expanded,
                Message::PerformanceLatencyToggleExpanded,
            ),
            if state.latency_expanded {
                view_latency_config(state)
            } else {
                column![].into()
            },
            space().height(20.0),
            // Monitoring section
            widgets::subsection_header("Monitoring"),
            space().height(8.0),
            widgets::toggle_with_help(
                "Enable Performance Snapshots",
                state.config.monitoring.enabled,
                "Collect periodic snapshots and emit D-Bus PerformanceUpdated signals",
                Message::MonitoringEnabledToggled,
            ),
            space().height(8.0),
            widgets::labeled_row_with_help(
                "Snapshot Interval:",
                150.0,
                row![
                    widgets::number_input(
                        &state.edit_strings.monitoring_snapshot_interval,
                        "5",
                        60.0,
                        Message::MonitoringSnapshotIntervalChanged,
                    ),
                    text(" seconds"),
                ]
                .align_y(Alignment::Center)
                .into(),
                "How often to publish performance snapshots",
            ),
            space().height(8.0),
            widgets::labeled_row_with_help(
                "Metrics Bind Address:",
                150.0,
                widgets::number_input(
                    &state.config.monitoring.metrics_bind,
                    "127.0.0.1:9100",
                    150.0,
                    Message::MonitoringMetricsBindChanged,
                ),
                "Prometheus + /health HTTP endpoint; only active with the metrics-server feature",
            ),
        ]
        .spacing(4),
    );

    content.padding(20).into()
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
        space().height(8.0),
        widgets::labeled_row_with_help(
            "Hint Distrust Threshold:",
            150.0,
            row![
                widgets::number_input(
                    &state.edit_strings.hint_distrust_threshold_pp,
                    "15",
                    60.0,
                    Message::DamageTrackingHintDistrustThresholdChanged,
                ),
                text(" pp"),
            ]
            .align_y(Alignment::Center)
            .into(),
            "Divergence between compositor damage hints and the pixel-diff \
             calibration probe, in percentage points, above which one sample \
             counts as high divergence",
        ),
        space().height(8.0),
        widgets::labeled_row_with_help(
            "Hint Distrust Samples:",
            150.0,
            widgets::number_input(
                &state.edit_strings.hint_distrust_consecutive_samples,
                "3",
                60.0,
                Message::DamageTrackingHintDistrustConsecutiveSamplesChanged,
            ),
            "Consecutive high-divergence samples before compositor damage \
             hints are distrusted for the rest of the connection",
        ),
    ]
    .padding([0, 16])
    .into()
}

/// Adaptive FPS configuration view
fn view_adaptive_fps_config(state: &AppState) -> Element<'_, Message> {
    let fps_config = &state.config.performance.adaptive_fps;

    column![
        space().height(8.0),
        widgets::toggle_with_help(
            "Enable Adaptive FPS",
            fps_config.enabled,
            "Dynamically adjust FPS based on screen activity",
            Message::AdaptiveFpsEnabledToggled,
        ),
        space().height(12.0),
        widgets::labeled_row(
            "Min FPS:",
            150.0,
            row![
                slider(
                    1..=30,
                    fps_config.min_fps,
                    Message::AdaptiveFpsMinFpsChanged
                )
                .width(Length::Fixed(150.0)),
                space().width(10.0),
                text(format!("{} fps", fps_config.min_fps)),
            ]
            .align_y(Alignment::Center)
            .into(),
        ),
        space().height(8.0),
        widgets::labeled_row(
            "Max FPS:",
            150.0,
            row![
                slider(
                    15..=60,
                    fps_config.max_fps,
                    Message::AdaptiveFpsMaxFpsChanged
                )
                .width(Length::Fixed(150.0)),
                space().width(10.0),
                text(format!("{} fps", fps_config.max_fps)),
            ]
            .align_y(Alignment::Center)
            .into(),
        ),
        space().height(12.0),
        text("Activity Thresholds:").size(13),
        space().height(8.0),
        widgets::labeled_row(
            "High Activity:",
            150.0,
            row![
                widgets::float_slider(
                    fps_config.high_activity_threshold,
                    Message::AdaptiveFpsHighActivityChanged,
                ),
                text(format!(
                    " ({}% changed)",
                    (fps_config.high_activity_threshold * 100.0) as u32
                )),
            ]
            .align_y(Alignment::Center)
            .into(),
        ),
        space().height(8.0),
        widgets::labeled_row(
            "Medium Activity:",
            150.0,
            row![
                widgets::float_slider(
                    fps_config.medium_activity_threshold,
                    Message::AdaptiveFpsMediumActivityChanged,
                ),
                text(format!(
                    " ({}% changed)",
                    (fps_config.medium_activity_threshold * 100.0) as u32
                )),
            ]
            .align_y(Alignment::Center)
            .into(),
        ),
        space().height(8.0),
        widgets::labeled_row(
            "Low Activity:",
            150.0,
            row![
                widgets::float_slider(
                    fps_config.low_activity_threshold,
                    Message::AdaptiveFpsLowActivityChanged,
                ),
                text(format!(
                    " ({}% changed)",
                    (fps_config.low_activity_threshold * 100.0) as u32
                )),
            ]
            .align_y(Alignment::Center)
            .into(),
        ),
    ]
    .padding([0, 16])
    .into()
}

/// Latency governor configuration view
fn view_latency_config(state: &AppState) -> Element<'_, Message> {
    let latency_config = &state.config.performance.latency;

    column![
        space().height(8.0),
        widgets::labeled_row_with_help(
            "Mode:",
            150.0,
            pick_list(
                LATENCY_MODES.to_vec(),
                Some(latency_config.mode.as_str()),
                |s| Message::LatencyModeChanged(s.to_string()),
            )
            .width(Length::Fixed(150.0))
            .into(),
            "Interactive: <50ms | Balanced: <100ms | Quality: <300ms",
        ),
        space().height(12.0),
        text("Mode Descriptions:").size(13),
        space().height(4.0),
        text("• Interactive - <50ms latency (gaming, CAD)")
            .size(12)
            .style(|_theme: &iced::Theme| text::Style {
                color: Some(theme::colors::TEXT_MUTED),
            }),
        text("• Balanced - <100ms latency (general desktop)")
            .size(12)
            .style(|_theme: &iced::Theme| text::Style {
                color: Some(theme::colors::TEXT_MUTED),
            }),
        text("• Quality - <300ms latency (photo/video editing)")
            .size(12)
            .style(|_theme: &iced::Theme| text::Style {
                color: Some(theme::colors::TEXT_MUTED),
            }),
        space().height(12.0),
        // Advanced tuning (optional, could be hidden in expert mode)
        text("Advanced Tuning:").size(13),
        space().height(8.0),
        widgets::labeled_row(
            "Interactive Max Delay:",
            170.0,
            row![
                widgets::number_input(
                    &state.edit_strings.interactive_delay,
                    "16",
                    60.0,
                    Message::LatencyInteractiveDelayChanged,
                ),
                text(" ms"),
            ]
            .align_y(Alignment::Center)
            .into(),
        ),
        space().height(4.0),
        widgets::labeled_row(
            "Balanced Max Delay:",
            170.0,
            row![
                widgets::number_input(
                    &state.edit_strings.balanced_delay,
                    "33",
                    60.0,
                    Message::LatencyBalancedDelayChanged,
                ),
                text(" ms"),
            ]
            .align_y(Alignment::Center)
            .into(),
        ),
        space().height(4.0),
        widgets::labeled_row(
            "Quality Max Delay:",
            170.0,
            row![
                widgets::number_input(
                    &state.edit_strings.quality_delay,
                    "100",
                    60.0,
                    Message::LatencyQualityDelayChanged,
                ),
                text(" ms"),
            ]
            .align_y(Alignment::Center)
            .into(),
        ),
        space().height(8.0),
        widgets::labeled_row(
            "Balanced Threshold:",
            170.0,
            widgets::float_slider(
                latency_config.balanced_damage_threshold,
                Message::LatencyBalancedThresholdChanged,
            ),
        ),
        space().height(4.0),
        widgets::labeled_row(
            "Quality Threshold:",
            170.0,
            widgets::float_slider(
                latency_config.quality_damage_threshold,
                Message::LatencyQualityThresholdChanged,
            ),
        ),
    ]
    .padding([0, 16])
    .into()
}
