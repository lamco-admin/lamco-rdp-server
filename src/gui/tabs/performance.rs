//! Performance Configuration Tab
//!
//! Threading, adaptive FPS, and latency governor settings.

use iced::{
    widget::{button, column, pick_list, row, slider, text},
    Alignment, Element, Length,
};

use crate::gui::{
    message::{Message, PerformancePreset},
    state::AppState,
    theme, widgets,
    widgets::space,
};

const LATENCY_MODES: &[&str] = &["interactive", "balanced", "quality"];

pub fn view_performance_tab(state: &AppState) -> Element<'_, Message> {
    column![
        // Section header
        widgets::section_header("Performance Configuration"),
        space().height(16.0),
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
        widgets::collapsible_header(
            "Threading",
            true,                                          // Always expanded for this section
            Message::PerformanceAdaptiveFpsToggleExpanded, // Placeholder
        ),
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
    ]
    .spacing(4)
    .padding(20)
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
