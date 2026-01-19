//! Video Configuration Tab
//!
//! Core video settings: encoder, FPS, bitrate, cursor mode, pipeline configuration.

use iced::widget::{button, column, pick_list, row, slider, space, text};
use iced::{Alignment, Element, Length};

use crate::gui::message::Message;
use crate::gui::state::AppState;
use crate::gui::theme;
use crate::gui::widgets;

const ENCODERS: &[&str] = &["auto", "vaapi", "openh264"];

/// Basic modes for video tab; advanced.rs has more options.
const CURSOR_MODES: &[&str] = &["metadata", "embedded", "hidden"];

pub fn view_video_tab(state: &AppState) -> Element<'_, Message> {
    column![
        // Section header
        widgets::section_header("Video Configuration"),
        space().height(20.0),
        // Basic Settings section
        widgets::subsection_header("Basic Settings"),
        space().height(12.0),
        // Encoder selection with GPU detect button
        widgets::labeled_row_with_help(
            "Encoder:",
            150.0,
            row![
                pick_list(
                    ENCODERS.to_vec(),
                    Some(state.config.video.encoder.as_str()),
                    |s| Message::VideoEncoderChanged(s.to_string()),
                )
                .width(Length::Fixed(150.0)),
                space().width(10.0),
                button(text("Detect GPUs"))
                    .on_press(Message::VideoDetectGpus)
                    .padding([6, 12])
                    .style(theme::secondary_button_style),
            ]
            .align_y(Alignment::Center)
            .into(),
            "Hardware: vaapi (Intel/AMD), Software: openh264",
        ),
        space().height(12.0),
        // Detected GPUs (if any)
        view_detected_gpus(&state.detected_gpus),
        // VA-API Device
        widgets::labeled_row_with_help(
            "VA-API Device:",
            150.0,
            pick_list(
                get_vaapi_device_options(state),
                Some(state.edit_strings.vaapi_device.as_str()),
                |s| Message::VideoVaapiDeviceChanged(s.to_string()),
            )
            .width(Length::Fixed(250.0))
            .into(),
            "GPU device for hardware encoding",
        ),
        space().height(12.0),
        // Target FPS
        widgets::labeled_row_with_help(
            "Target FPS:",
            150.0,
            row![
                slider(
                    5..=60,
                    state.config.video.target_fps,
                    Message::VideoTargetFpsChanged
                )
                .width(Length::Fixed(200.0)),
                space().width(10.0),
                text(format!("{} fps", state.config.video.target_fps)),
            ]
            .align_y(Alignment::Center)
            .into(),
            "5 ←────────────────────────────────→ 60",
        ),
        space().height(12.0),
        // Bitrate
        widgets::labeled_row_with_help(
            "Bitrate:",
            150.0,
            row![
                slider(
                    1000..=20000,
                    state.config.video.bitrate,
                    Message::VideoBitrateChanged
                )
                .width(Length::Fixed(200.0)),
                space().width(10.0),
                text(format!("{} kbps", state.config.video.bitrate)),
            ]
            .align_y(Alignment::Center)
            .into(),
            "1000 ←──────────────────────────→ 20000",
        ),
        space().height(12.0),
        // Damage Tracking toggle
        widgets::toggle_with_help(
            "Enable Damage Tracking",
            state.config.video.damage_tracking,
            "Only encode changed regions (saves bandwidth)",
            Message::VideoDamageTrackingToggled,
        ),
        space().height(12.0),
        // Cursor Mode
        widgets::labeled_row_with_help(
            "Cursor Mode:",
            150.0,
            pick_list(
                CURSOR_MODES.to_vec(),
                Some(state.config.video.cursor_mode.as_str()),
                |s| Message::VideoCursorModeChanged(s.to_string()),
            )
            .width(Length::Fixed(150.0))
            .into(),
            "Metadata = client-side (lowest latency)",
        ),
        space().height(24.0),
        // Advanced Pipeline section (collapsible)
        widgets::collapsible_header(
            "Advanced Pipeline Configuration",
            state.video_pipeline_expanded,
            Message::VideoPipelineToggleExpanded,
        ),
        // Pipeline content (if expanded)
        if state.video_pipeline_expanded {
            view_video_pipeline_config(state)
        } else {
            column![].into()
        },
    ]
    .spacing(4)
    .padding(20)
    .into()
}

/// View detected GPUs
fn view_detected_gpus(gpus: &[crate::gui::state::GpuInfo]) -> Element<'_, Message> {
    if gpus.is_empty() {
        space().height(0.0).into()
    } else {
        let gpu_list: Vec<_> = gpus
            .iter()
            .map(|gpu| {
                let device = gpu
                    .vaapi_device
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| "N/A".to_string());
                text(format!(
                    "• {} {} ({}) - {}",
                    gpu.vendor, gpu.model, gpu.driver, device
                ))
                .size(13)
            })
            .collect();

        column![
            text("Detected GPUs:").size(13),
            column(gpu_list.into_iter().map(|t| t.into()).collect::<Vec<_>>()).spacing(2),
            space().height(12.0),
        ]
        .spacing(4)
        .into()
    }
}

fn get_vaapi_device_options(_state: &AppState) -> Vec<&'static str> {
    // TODO: use state.detected_vaapi_devices when hardware detection is wired up
    vec!["/dev/dri/renderD128", "/dev/dri/renderD129"]
}

/// Video pipeline configuration view
fn view_video_pipeline_config(state: &AppState) -> Element<'_, Message> {
    column![
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
        widgets::toggle_switch(
            "Adaptive Quality",
            state.config.video_pipeline.processor.adaptive_quality,
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
        widgets::toggle_switch(
            "Drop on Full Queue",
            state.config.video_pipeline.processor.drop_on_full_queue,
            Message::ProcessorDropOnFullQueueToggled,
        ),
        space().height(8.0),
        widgets::toggle_switch(
            "Enable Metrics",
            state.config.video_pipeline.processor.enable_metrics,
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
        widgets::toggle_switch(
            "Priority Dispatch",
            state.config.video_pipeline.dispatcher.priority_dispatch,
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
        widgets::toggle_switch(
            "Enable Backpressure",
            state.config.video_pipeline.dispatcher.enable_backpressure,
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
        widgets::toggle_switch(
            "Load Balancing",
            state.config.video_pipeline.dispatcher.load_balancing,
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
        widgets::toggle_switch(
            "Enable SIMD",
            state.config.video_pipeline.converter.enable_simd,
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
        widgets::toggle_switch(
            "Enable Statistics",
            state.config.video_pipeline.converter.enable_statistics,
            Message::ConverterEnableStatisticsToggled,
        ),
    ]
    .spacing(4)
    .padding([0, 20])
    .into()
}
