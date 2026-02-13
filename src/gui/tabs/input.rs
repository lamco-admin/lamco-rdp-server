//! Input Configuration Tab
//!
//! Keyboard, mouse, and touch input settings.

use iced::{
    widget::{column, pick_list},
    Element, Length,
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

pub fn view_input_tab(state: &AppState) -> Element<'_, Message> {
    column![
        // Section header
        widgets::section_header("Input Configuration"),
        space().height(20.0),

        // Use libei toggle
        widgets::toggle_with_help(
            "Use libei for Input Injection",
            state.config.input.use_libei,
            "Modern input method via Portal RemoteDesktop (recommended)",
            Message::InputUseLibeiToggled,
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
