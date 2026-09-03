//! Display & Monitors Configuration Tab
//!
//! Multi-monitor setup and per-output display control (resize, DPI, transform,
//! resolution filtering). Split out of the Advanced tab so it's discoverable on
//! its own rather than buried among unrelated expert settings.

use iced::{
    Alignment, Element, Length,
    widget::{column, container, pick_list, row, slider, text},
};

use crate::gui::{
    message::{Message, MultimonPreset},
    state::AppState,
    theme, widgets,
    widgets::space,
};

const TRANSFORM_OPTIONS: &[&str] = &[
    "auto",
    "none",
    "90",
    "180",
    "270",
    "flipped",
    "flipped-90",
    "flipped-180",
    "flipped-270",
];

const MULTIMON_PRESETS: &[MultimonPreset] = &[
    MultimonPreset::Single,
    MultimonPreset::Dual,
    MultimonPreset::Triple,
    MultimonPreset::Quad,
    MultimonPreset::Custom,
];

pub fn view_display_tab(state: &AppState) -> Element<'_, Message> {
    // Determine current preset based on max_monitors
    let current_preset = match state.config.multimon.max_monitors {
        1 => Some(MultimonPreset::Single),
        2 => Some(MultimonPreset::Dual),
        3 => Some(MultimonPreset::Triple),
        4 => Some(MultimonPreset::Quad),
        _ => Some(MultimonPreset::Custom),
    };

    column![
        widgets::section_header("Display & Monitors"),
        space().height(16.0),
        // Multi-monitor subsection
        widgets::subsection_header("Multi-Monitor"),
        space().height(8.0),
        widgets::toggle_with_help(
            "Enable Multi-Monitor Support",
            state.config.multimon.enabled,
            "Allow clients to connect to multiple monitors",
            Message::MultimonEnabledToggled,
        ),
        space().height(12.0),
        widgets::labeled_row(
            "Display Configuration:",
            150.0,
            row![
                pick_list(
                    MULTIMON_PRESETS.to_vec(),
                    current_preset,
                    Message::MultimonPresetSelected,
                )
                .width(Length::Fixed(200.0)),
                space().width(16.0),
                text(format!("{} monitor(s)", state.config.multimon.max_monitors))
                    .size(13)
                    .style(|_theme| text::Style {
                        color: Some(theme::colors::TEXT_SECONDARY),
                    }),
            ]
            .align_y(Alignment::Center)
            .into(),
        ),
        space().height(8.0),
        widgets::labeled_row(
            "Maximum Monitors:",
            150.0,
            row![
                slider(1..=16, state.config.multimon.max_monitors as u8, |v| {
                    Message::MultimonMaxMonitorsChanged(v.to_string())
                },)
                .width(Length::Fixed(200.0)),
                space().width(16.0),
                container(text(&state.edit_strings.max_monitors).size(14))
                    .width(Length::Fixed(40.0))
                    .center_x(Length::Fill),
            ]
            .align_y(Alignment::Center)
            .into(),
        ),
        space().height(4.0),
        text("Portal provides all selected monitors; max setting is advisory")
            .size(11)
            .style(|_theme| text::Style {
                color: Some(theme::colors::TEXT_MUTED),
            }),
        space().height(20.0),
        // Display control subsection
        widgets::collapsible_header(
            "Display Control",
            state.display_expanded,
            Message::DisplayToggleExpanded,
        ),
        if state.display_expanded {
            view_display_control_config(state)
        } else {
            column![].into()
        },
    ]
    .spacing(4)
    .padding(20)
    .into()
}

fn view_display_control_config(state: &AppState) -> Element<'_, Message> {
    let display = &state.config.display;

    column![
        space().height(8.0),
        widgets::toggle_with_help(
            "Allow Dynamic Resolution",
            display.allow_resize,
            "Clients can request resolution changes via MS-RDPEDISP",
            Message::DisplayAllowResizeToggled,
        ),
        space().height(8.0),
        widgets::toggle_pending_with_note(
            "DPI Aware",
            display.dpi_aware,
            Message::DisplayDpiAwareToggled,
            "Portal doesn't expose DPI; needs per-monitor DPI detection",
        ),
        space().height(8.0),
        widgets::labeled_row_with_help(
            "Frame Transform:",
            150.0,
            pick_list(
                TRANSFORM_OPTIONS.to_vec(),
                Some(display.frame_transform.as_str()),
                |s| Message::DisplayFrameTransformChanged(s.to_string()),
            )
            .width(Length::Fixed(160.0))
            .into(),
            "Auto = read from PipeWire metadata | Others = force specific transform",
        ),
        space().height(12.0),
        text("Allowed Resolutions (empty = all):")
            .size(13)
            .style(|_theme| text::Style {
                color: Some(theme::colors::TEXT_MUTED),
            }),
        space().height(4.0),
        widgets::text_area(
            &state.edit_strings.resolutions_text,
            "1920x1080\n2560x1440\n3840x2160",
            80.0,
            Message::DisplayAllowedResolutionsChanged,
        ),
        text("Resolution filtering not yet implemented")
            .size(11)
            .style(|_theme| text::Style {
                color: Some(theme::colors::TEXT_MUTED),
            }),
    ]
    .padding([0, 16])
    .into()
}
