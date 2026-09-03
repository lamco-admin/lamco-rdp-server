//! Input Configuration Tab
//!
//! Keyboard, mouse, touch, and cursor settings.

use iced::{
    Alignment, Element, Length,
    widget::{column, pick_list, row, text},
};

use crate::gui::{message::Message, state::AppState, theme, widgets, widgets::space};

/// Superset of video.rs modes: adds "painted" and "predictive" for advanced use.
const CURSOR_MODES: &[&str] = &["metadata", "painted", "hidden", "predictive"];

const KEYBOARD_LAYOUTS: &[&str] = &[
    "auto", "us", // US English
    "gb", // UK English
    "de", // German
    "fr", // French
    "es", // Spanish
    "it", // Italian
    "pt", // Portuguese
    "nl", // Dutch
    "ru", // Russian
    "ja", // Japanese
    "ko", // Korean
    "zh", // Chinese
];

const INPUT_PROTOCOLS: &[&str] = &["auto", "libei", "wlr"];

pub fn view_input_tab(state: &AppState) -> Element<'_, Message> {
    column![
        // Section header
        widgets::section_header("Input Configuration"),
        space().height(20.0),

        // Input protocol selection
        widgets::labeled_row_with_help(
            "Input Protocol:",
            150.0,
            pick_list(
                INPUT_PROTOCOLS.to_vec(),
                Some(state.config.input.input_protocol.as_str()),
                |s| Message::InputProtocolChanged(s.to_string()),
            )
            .width(Length::Fixed(200.0))
            .into(),
            "auto: GNOME/KDE use libei, wlroots/Smithay use wlr virtual input",
        ),
        space().height(16.0),

        // Keyboard layout
        widgets::labeled_row_with_help(
            "Keyboard Layout:",
            150.0,
            pick_list(
                KEYBOARD_LAYOUTS.to_vec(),
                Some(state.config.input.keyboard_layout.as_str()),
                |s| Message::InputKeyboardLayoutChanged(s.to_string()),
            )
            .width(Length::Fixed(200.0))
            .into(),
            "Auto-detect or specify XKB layout name",
        ),
        space().height(12.0),

        // Layout descriptions
        widgets::info_box("Common Layouts:\n• us - US English (QWERTY)\n• gb - UK English\n• de - German (QWERTZ)\n• fr - French (AZERTY)"),
        space().height(16.0),

        // Enable touch toggle
        widgets::toggle_with_help(
            "Enable Touch Input",
            state.config.input.enable_touch,
            "Support touchscreen devices (if available)",
            Message::InputEnableTouchToggled,
        ),
        space().height(20.0),
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
    .spacing(8)
    .padding(20)
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
