//! Input Configuration Tab
//!
//! Keyboard, mouse, and touch input settings.

use iced::{
    Element, Length,
    widget::{column, pick_list},
};

use crate::gui::{message::Message, state::AppState, widgets, widgets::space};

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
    ]
    .spacing(8)
    .padding(20)
    .into()
}
