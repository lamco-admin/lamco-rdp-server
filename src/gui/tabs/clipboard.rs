//! Clipboard Configuration Tab
//!
//! Clipboard synchronization settings, rate limiting, and MIME type filtering.

use iced::{
    widget::{button, column, row, text, text_input},
    Alignment, Element, Length,
};

use crate::gui::{
    message::{ClipboardPreset, Message},
    state::AppState,
    theme, widgets,
    widgets::space,
};

pub fn view_clipboard_tab(state: &AppState) -> Element<'_, Message> {
    // Join allowed types for text area display
    let allowed_types_text = state.config.clipboard.allowed_types.join("\n");

    column![
        // Section header
        widgets::section_header("Clipboard Configuration"),
        space().height(20.0),
        // Enable clipboard toggle
        widgets::toggle_with_help(
            "Enable Clipboard Synchronization",
            state.config.clipboard.enabled,
            "Copy/paste between client and server",
            Message::ClipboardEnabledToggled,
        ),
        space().height(16.0),
        // Maximum clipboard size
        widgets::labeled_row_with_help(
            "Maximum Size:",
            150.0,
            row![
                widgets::number_input(&state.edit_strings.max_size_mb, "10", 80.0, |s| {
                    // Convert MB back to bytes
                    let mb: usize = s.parse().unwrap_or(10);
                    Message::ClipboardMaxSizeChanged((mb * 1024 * 1024).to_string())
                },),
                text(" MB"),
            ]
            .spacing(4)
            .align_y(Alignment::Center)
            .into(),
            "Reject clipboard data larger than this",
        ),
        space().height(16.0),
        // Rate limiting
        widgets::labeled_row_with_help(
            "Rate Limiting:",
            150.0,
            row![
                widgets::number_input(
                    &state.edit_strings.rate_limit,
                    "200",
                    80.0,
                    Message::ClipboardRateLimitChanged,
                ),
                text(" ms between events"),
            ]
            .spacing(4)
            .align_y(Alignment::Center)
            .into(),
            "Prevents clipboard spam attacks (200ms = max 5 events/sec)",
        ),
        space().height(20.0),
        // MIME types section
        text("Allowed MIME Types (one per line, empty = all):").size(14),
        space().height(8.0),
        text_input("text/plain\ntext/html\nimage/png", &allowed_types_text)
            .on_input(Message::ClipboardAllowedTypesChanged)
            .width(Length::Fill)
            .style(theme::text_input_style),
        space().height(8.0),
        text("ⓘ Leave empty to allow all clipboard formats")
            .size(12)
            .style(|_theme| text::Style {
                color: Some(theme::colors::TEXT_MUTED),
            }),
        space().height(12.0),
        // Preset buttons
        text("Quick Presets:").size(13),
        space().height(8.0),
        row![
            button(text("Text Only"))
                .on_press(Message::ClipboardPresetSelected(ClipboardPreset::TextOnly))
                .padding([6, 12])
                .style(theme::secondary_button_style),
            button(text("Text + Images"))
                .on_press(Message::ClipboardPresetSelected(
                    ClipboardPreset::TextAndImages
                ))
                .padding([6, 12])
                .style(theme::secondary_button_style),
            button(text("All Types"))
                .on_press(Message::ClipboardPresetSelected(ClipboardPreset::All))
                .padding([6, 12])
                .style(theme::secondary_button_style),
        ]
        .spacing(8),
        space().height(8.0),
        widgets::info_box(
            "• Text Only: text/plain, text/html, text/uri-list\n\
             • Text + Images: + image/png, image/jpeg\n\
             • All Types: No restrictions (empty list)"
        ),
    ]
    .spacing(4)
    .padding(20)
    .into()
}
