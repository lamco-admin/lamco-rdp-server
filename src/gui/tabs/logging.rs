//! Logging Configuration Tab
//!
//! Log level, output destinations, and metrics settings.

use iced::widget::{button, column, pick_list, row, space, text};
use iced::{Element, Length};

use crate::gui::message::Message;
use crate::gui::state::AppState;
use crate::gui::theme;
use crate::gui::widgets;

const LOG_LEVELS: &[&str] = &["trace", "debug", "info", "warn", "error"];

pub fn view_logging_tab(state: &AppState) -> Element<'_, Message> {
    column![
        // Section header
        widgets::section_header("Logging Configuration"),
        space().height(20.0),
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
        space().height(20.0),
        // Log output section
        text("Log Output:").size(14),
        space().height(8.0),
        // Console is always enabled
        widgets::info_box("Console output (stdout) is always enabled"),
        space().height(12.0),
        // Log directory (file logging)
        text("Log Directory (for file logging):").size(13),
        space().height(4.0),
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
        space().height(8.0),
        text("ⓘ Leave empty for console-only logging")
            .size(12)
            .style(|_theme| text::Style {
                color: Some(theme::colors::TEXT_MUTED),
            }),
        space().height(20.0),
        // Metrics toggle
        widgets::toggle_with_help(
            "Enable Performance Metrics",
            state.config.logging.metrics,
            "Collect FPS, bandwidth, latency statistics",
            Message::LoggingMetricsToggled,
        ),
    ]
    .spacing(4)
    .padding(20)
    .into()
}
