//! Advanced Configuration Tab
//!
//! Combines damage tracking, hardware encoding, display, advanced video, and cursor settings.

use iced::widget::{button, column, pick_list, row, space, text};
use iced::{Alignment, Element, Length};

use crate::gui::message::{DamageTrackingPreset, Message};
use crate::gui::state::AppState;
use crate::gui::theme;
use crate::gui::widgets;

const DAMAGE_METHODS: &[&str] = &["diff", "pipewire", "hybrid"];
const HW_QUALITY_PRESETS: &[&str] = &["speed", "balanced", "quality"];

/// Superset of video.rs modes: adds "painted" and "predictive" for advanced use.
const CURSOR_MODES: &[&str] = &["metadata", "painted", "hidden", "predictive"];

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
        // Display Control section
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
        text("• Diff: CPU pixel comparison | PipeWire: Compositor hints | Hybrid: Both")
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
                text(" pixels²"),
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
        widgets::toggle_with_help(
            "Enable Hardware Acceleration",
            hw.enabled,
            "Use GPU for H.264 encoding (lower CPU usage)",
            Message::HardwareEncodingEnabledToggled,
        ),
        space().height(12.0),
        // Display detected GPUs
        if !state.detected_gpus.is_empty() {
            let gpu_text: Vec<_> = state
                .detected_gpus
                .iter()
                .map(|gpu| {
                    format!(
                        "• {} {} ({driver})",
                        gpu.vendor,
                        gpu.model,
                        driver = gpu.driver
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
        widgets::labeled_row(
            "VA-API Device:",
            150.0,
            pick_list(
                vec!["/dev/dri/renderD128", "/dev/dri/renderD129"],
                Some(state.edit_strings.vaapi_device.as_str()),
                |s| Message::HardwareEncodingVaapiDeviceChanged(s.to_string()),
            )
            .width(Length::Fixed(200.0))
            .into(),
        ),
        space().height(12.0),
        widgets::toggle_switch(
            "Enable DMA-BUF Zero-Copy",
            hw.enable_dmabuf_zerocopy,
            Message::HardwareEncodingDmabufZerocopyToggled,
        ),
        space().height(8.0),
        widgets::toggle_switch(
            "Fallback to Software",
            hw.fallback_to_software,
            Message::HardwareEncodingFallbackToSoftwareToggled,
        ),
        space().height(8.0),
        widgets::toggle_switch(
            "Prefer NVENC over VA-API",
            hw.prefer_nvenc,
            Message::HardwareEncodingPreferNvencToggled,
        ),
        space().height(12.0),
        widgets::labeled_row(
            "Quality Preset:",
            150.0,
            pick_list(
                HW_QUALITY_PRESETS.to_vec(),
                Some(hw.quality_preset.as_str()),
                |s| Message::HardwareEncodingQualityPresetChanged(s.to_string()),
            )
            .width(Length::Fixed(150.0))
            .into(),
        ),
    ]
    .padding([0, 16])
    .into()
}

/// Display control configuration view
fn view_display_config(state: &AppState) -> Element<'_, Message> {
    let display = &state.config.display;

    column![
        space().height(8.0),
        widgets::toggle_switch(
            "Allow Dynamic Resolution",
            display.allow_resize,
            Message::DisplayAllowResizeToggled,
        ),
        space().height(8.0),
        widgets::toggle_switch(
            "DPI Aware",
            display.dpi_aware,
            Message::DisplayDpiAwareToggled,
        ),
        space().height(8.0),
        widgets::toggle_switch(
            "Allow Rotation",
            display.allow_rotation,
            Message::DisplayAllowRotationToggled,
        ),
        space().height(12.0),
        text("Allowed Resolutions (empty = all):").size(13),
        space().height(4.0),
        widgets::text_area(
            &state.edit_strings.resolutions_text,
            "1920x1080\n2560x1440\n3840x2160",
            80.0,
            Message::DisplayAllowedResolutionsChanged,
        ),
    ]
    .padding([0, 16])
    .into()
}

/// Advanced video configuration view
fn view_advanced_video_config(state: &AppState) -> Element<'_, Message> {
    let av = &state.config.advanced_video;

    column![
        space().height(8.0),
        widgets::toggle_switch(
            "Enable Frame Skip",
            av.enable_frame_skip,
            Message::AdvancedVideoEnableFrameSkipToggled,
        ),
        space().height(8.0),
        widgets::labeled_row(
            "Scene Change Threshold:",
            180.0,
            widgets::float_slider(
                av.scene_change_threshold,
                Message::AdvancedVideoSceneChangeThresholdChanged,
            ),
        ),
        space().height(8.0),
        widgets::labeled_row(
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
        ),
        space().height(8.0),
        widgets::toggle_switch(
            "Enable Adaptive Quality",
            av.enable_adaptive_quality,
            Message::AdvancedVideoEnableAdaptiveQualityToggled,
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
        widgets::labeled_row_with_help(
            "Cursor Mode:",
            150.0,
            pick_list(CURSOR_MODES.to_vec(), Some(cursor.mode.as_str()), |s| {
                Message::CursorModeChanged(s.to_string())
            },)
            .width(Length::Fixed(150.0))
            .into(),
            "Metadata: client-side | Painted: composited | Predictive: physics-based",
        ),
        space().height(8.0),
        text(
            "• Metadata - Client renders cursor (lowest latency)\n\
              • Painted - Cursor composited into video\n\
              • Hidden - No cursor (touch/pen)\n\
              • Predictive - Physics-based prediction"
        )
        .size(12)
        .style(|_theme| text::Style {
            color: Some(theme::colors::TEXT_MUTED),
        }),
        space().height(12.0),
        widgets::toggle_switch(
            "Auto Mode Selection",
            cursor.auto_mode,
            Message::CursorAutoModeToggled,
        ),
        space().height(8.0),
        widgets::labeled_row(
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
        ),
        space().height(8.0),
        widgets::labeled_row(
            "Cursor Update FPS:",
            150.0,
            widgets::number_input(
                &state.edit_strings.cursor_update_fps,
                "60",
                60.0,
                Message::CursorUpdateFpsChanged,
            ),
        ),
        space().height(12.0),
        // Predictor sub-section
        widgets::collapsible_header(
            "Predictor Configuration",
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
