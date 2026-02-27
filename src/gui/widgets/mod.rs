//! Widget helpers that compose iced primitives into consistent form controls.
//!
//! Centralizes styling decisions so tabs focus purely on layout.

pub use iced::widget::space;
use iced::{
    widget::{button, column, container, pick_list, row, slider, text, text_input, toggler},
    Alignment, Element, Length,
};

use crate::gui::{message::Message, theme};

pub fn labeled_row<'a>(
    label: &'a str,
    label_width: f32,
    widget: Element<'a, Message>,
) -> Element<'a, Message> {
    row![text(label).width(Length::Fixed(label_width)), widget,]
        .spacing(10)
        .align_y(Alignment::Center)
        .into()
}

pub fn labeled_row_with_help<'a>(
    label: &'a str,
    label_width: f32,
    widget: Element<'a, Message>,
    help_text: &'a str,
) -> Element<'a, Message> {
    column![
        row![text(label).width(Length::Fixed(label_width)), widget,]
            .spacing(10)
            .align_y(Alignment::Center),
        row![
            space().width(label_width),
            text(format!("ⓘ {}", help_text))
                .size(12)
                .style(|_theme| text::Style {
                    color: Some(theme::colors::TEXT_MUTED),
                }),
        ],
    ]
    .spacing(4)
    .into()
}

pub fn section_header<'a>(title: &'a str) -> Element<'a, Message> {
    text(title)
        .size(20)
        .style(|_theme| text::Style {
            color: Some(theme::colors::PRIMARY),
        })
        .into()
}

pub fn subsection_header<'a>(title: &'a str) -> Element<'a, Message> {
    text(title)
        .size(16)
        .style(|_theme| text::Style {
            color: Some(theme::colors::TEXT_PRIMARY),
        })
        .into()
}

pub fn collapsible_header<'a>(
    title: &'a str,
    expanded: bool,
    on_toggle: Message,
) -> Element<'a, Message> {
    let icon = if expanded { "▼" } else { "▶" };

    button(
        row![text(icon).size(14), text(title).size(16),]
            .spacing(8)
            .align_y(Alignment::Center),
    )
    .on_press(on_toggle)
    .padding([8, 12])
    .width(Length::Fill)
    .style(|theme, status| {
        let mut style = theme::collapsible_header_style(theme);
        if matches!(status, button::Status::Hovered) {
            style.background = Some(iced::Background::Color(theme::colors::SURFACE_DARK));
        }
        button::Style {
            background: style.background,
            text_color: theme::colors::TEXT_PRIMARY,
            border: style.border,
            shadow: style.shadow,
            snap: false,
        }
    })
    .into()
}

pub fn toggle_switch<'a>(
    label: &'a str,
    value: bool,
    on_toggle: impl Fn(bool) -> Message + 'a,
) -> Element<'a, Message> {
    row![
        text(label).width(Length::Fill),
        toggler(value).on_toggle(on_toggle),
    ]
    .spacing(10)
    .align_y(Alignment::Center)
    .into()
}

pub fn toggle_with_help<'a>(
    label: &'a str,
    value: bool,
    help_text: &'a str,
    on_toggle: impl Fn(bool) -> Message + 'a,
) -> Element<'a, Message> {
    column![
        row![
            text(label).width(Length::Fill),
            toggler(value).on_toggle(on_toggle),
        ]
        .spacing(10)
        .align_y(Alignment::Center),
        text(format!("ⓘ {}", help_text))
            .size(12)
            .style(|_theme| text::Style {
                color: Some(theme::colors::TEXT_MUTED),
            }),
    ]
    .spacing(4)
    .into()
}

pub fn number_input<'a>(
    value: &'a str,
    placeholder: &'a str,
    width: f32,
    on_change: impl Fn(String) -> Message + 'a,
) -> Element<'a, Message> {
    text_input(placeholder, value)
        .on_input(on_change)
        .width(Length::Fixed(width))
        .style(theme::text_input_style)
        .into()
}

pub fn slider_with_value<'a>(
    value: u32,
    min: u32,
    max: u32,
    unit: &'a str,
    on_change: impl Fn(u32) -> Message + 'a,
) -> Element<'a, Message> {
    row![
        slider(min..=max, value, on_change).width(Length::FillPortion(3)),
        text(format!("{} {}", value, unit))
            .width(Length::FillPortion(1))
            .align_x(iced::alignment::Horizontal::Right),
    ]
    .spacing(10)
    .align_y(Alignment::Center)
    .into()
}

/// Slider over [0.0, 1.0], backed by integer steps for iced slider compatibility.
pub fn float_slider<'a>(
    value: f32,
    on_change: impl Fn(f32) -> Message + 'a,
) -> Element<'a, Message> {
    let int_value = (value * 100.0) as u32;

    row![
        slider(0..=100, int_value, move |v| on_change(v as f32 / 100.0))
            .width(Length::FillPortion(3)),
        text(format!("{:.2}", value))
            .width(Length::FillPortion(1))
            .align_x(iced::alignment::Horizontal::Right),
    ]
    .spacing(10)
    .align_y(Alignment::Center)
    .into()
}

pub fn dropdown<'a, T>(
    options: impl IntoIterator<Item = T> + 'a,
    selected: Option<T>,
    on_select: impl Fn(T) -> Message + 'a,
) -> Element<'a, Message>
where
    T: ToString + PartialEq + Clone + 'a,
{
    pick_list(options.into_iter().collect::<Vec<_>>(), selected, on_select)
        .placeholder("Select...")
        .width(Length::Fixed(200.0))
        .into()
}

pub fn path_input<'a>(
    value: &'a str,
    placeholder: &'a str,
    on_change: impl Fn(String) -> Message + 'a,
    on_browse: Message,
) -> Element<'a, Message> {
    row![
        text_input(placeholder, value)
            .on_input(on_change)
            .width(Length::Fill)
            .style(theme::text_input_style),
        button(text("Browse..."))
            .on_press(on_browse)
            .style(theme::secondary_button_style),
    ]
    .spacing(10)
    .into()
}

/// Read-only path display with Browse button for sandbox mode.
///
/// In Flatpak, users cannot type arbitrary paths — the FileChooser portal
/// grants per-file access when the user selects via the Browse dialog.
/// The text field is non-editable, showing the selected path or placeholder.
pub fn path_display<'a>(
    value: &'a str,
    placeholder: &'a str,
    on_browse: Message,
) -> Element<'a, Message> {
    let display_text = if value.is_empty() { placeholder } else { value };
    row![
        container(text(display_text).size(14))
            .padding([8, 12])
            .width(Length::Fill)
            .style(theme::path_display_style),
        button(text("Browse..."))
            .on_press(on_browse)
            .style(theme::secondary_button_style),
    ]
    .spacing(10)
    .into()
}

pub fn preset_buttons<'a, T>(
    presets: &'a [(T, &'a str)],
    selected: Option<T>,
    on_select: impl Fn(T) -> Message + Clone + 'a,
) -> Element<'a, Message>
where
    T: Clone + PartialEq + 'a,
{
    let buttons: Vec<_> = presets
        .iter()
        .map(|(preset, label)| {
            let is_selected = selected.as_ref() == Some(preset);
            let preset = preset.clone();
            let on_select = on_select.clone();
            button(text(*label))
                .on_press(on_select(preset))
                .padding([6, 12])
                .style(theme::preset_button_style(is_selected))
                .into()
        })
        .collect();

    row(buttons).spacing(8).into()
}

/// Simple help text displayed in muted style
pub fn help_text<'a>(text_content: &'a str) -> Element<'a, Message> {
    text(format!("ⓘ {}", text_content))
        .size(12)
        .style(|_theme| text::Style {
            color: Some(theme::colors::TEXT_MUTED),
        })
        .into()
}

pub fn info_box<'a>(text_content: &'a str) -> Element<'a, Message> {
    container(
        row![text("ⓘ").size(16), text(text_content).size(13),]
            .spacing(8)
            .align_y(Alignment::Center),
    )
    .padding([8, 12])
    .style(|_theme| container::Style {
        background: Some(iced::Background::Color(
            theme::colors::INFO.scale_alpha(0.1),
        )),
        border: iced::Border {
            color: theme::colors::INFO.scale_alpha(0.3),
            width: 1.0,
            radius: 4.0.into(),
        },
        ..Default::default()
    })
    .into()
}

pub fn warning_box<'a>(text_content: &'a str) -> Element<'a, Message> {
    container(
        row![text("⚠").size(16), text(text_content).size(13),]
            .spacing(8)
            .align_y(Alignment::Center),
    )
    .padding([8, 12])
    .style(|_theme| container::Style {
        background: Some(iced::Background::Color(
            theme::colors::WARNING.scale_alpha(0.1),
        )),
        border: iced::Border {
            color: theme::colors::WARNING.scale_alpha(0.3),
            width: 1.0,
            radius: 4.0.into(),
        },
        ..Default::default()
    })
    .into()
}

pub fn error_box<'a>(text_content: &'a str) -> Element<'a, Message> {
    container(
        row![text("✗").size(16), text(text_content).size(13),]
            .spacing(8)
            .align_y(Alignment::Center),
    )
    .padding([8, 12])
    .style(|_theme| container::Style {
        background: Some(iced::Background::Color(
            theme::colors::ERROR.scale_alpha(0.1),
        )),
        border: iced::Border {
            color: theme::colors::ERROR.scale_alpha(0.3),
            width: 1.0,
            radius: 4.0.into(),
        },
        ..Default::default()
    })
    .into()
}

pub fn success_box<'a>(text_content: &'a str) -> Element<'a, Message> {
    container(
        row![text("✓").size(16), text(text_content).size(13),]
            .spacing(8)
            .align_y(Alignment::Center),
    )
    .padding([8, 12])
    .style(|_theme| container::Style {
        background: Some(iced::Background::Color(
            theme::colors::SUCCESS.scale_alpha(0.1),
        )),
        border: iced::Border {
            color: theme::colors::SUCCESS.scale_alpha(0.3),
            width: 1.0,
            radius: 4.0.into(),
        },
        ..Default::default()
    })
    .into()
}

pub fn section_container<'a>(content: Element<'a, Message>) -> Element<'a, Message> {
    container(content)
        .padding(20)
        .width(Length::Fill)
        .style(theme::section_container_style)
        .into()
}

pub fn vertical_spacing(height: f32) -> iced::widget::Space {
    space().height(height)
}

pub fn horizontal_spacing(width: f32) -> iced::widget::Space {
    space().width(width)
}

/// Creates a labeled row with a "pending implementation" indicator.
/// The widget is shown but with muted styling and a note that it needs wiring.
/// Use for config options that exist in the GUI but aren't yet connected to the server.
pub fn labeled_row_pending<'a>(
    label: &'a str,
    label_width: f32,
    widget: Element<'a, Message>,
) -> Element<'a, Message> {
    column![
        row![
            text(label)
                .width(Length::Fixed(label_width))
                .style(|_theme| text::Style {
                    color: Some(theme::colors::TEXT_MUTED),
                }),
            container(widget).style(|_theme| container::Style {
                // Slightly transparent to indicate disabled state
                ..Default::default()
            }),
        ]
        .spacing(10)
        .align_y(Alignment::Center),
        row![
            space().width(label_width),
            text("⚠ Not yet wired to server")
                .size(11)
                .style(|_theme| text::Style {
                    color: Some(theme::colors::WARNING.scale_alpha(0.7)),
                }),
        ],
    ]
    .spacing(2)
    .into()
}

/// Creates a labeled row with custom pending message.
pub fn labeled_row_pending_with_note<'a>(
    label: &'a str,
    label_width: f32,
    widget: Element<'a, Message>,
    note: &'a str,
) -> Element<'a, Message> {
    column![
        row![
            text(label)
                .width(Length::Fixed(label_width))
                .style(|_theme| text::Style {
                    color: Some(theme::colors::TEXT_MUTED),
                }),
            widget,
        ]
        .spacing(10)
        .align_y(Alignment::Center),
        row![
            space().width(label_width),
            text(format!("⚠ {}", note))
                .size(11)
                .style(|_theme| text::Style {
                    color: Some(theme::colors::WARNING.scale_alpha(0.7)),
                }),
        ],
    ]
    .spacing(2)
    .into()
}

/// Toggle switch with pending implementation note.
pub fn toggle_pending_with_note<'a>(
    label: &'a str,
    value: bool,
    on_toggle: impl Fn(bool) -> Message + 'a,
    note: &'a str,
) -> Element<'a, Message> {
    column![
        row![
            text(label).width(Length::Fill).style(|_theme| text::Style {
                color: Some(theme::colors::TEXT_MUTED),
            }),
            toggler(value).on_toggle(on_toggle),
        ]
        .spacing(10)
        .align_y(Alignment::Center),
        text(format!("⚠ {}", note))
            .size(11)
            .style(|_theme| text::Style {
                color: Some(theme::colors::WARNING.scale_alpha(0.7)),
            }),
    ]
    .spacing(2)
    .into()
}

/// iced lacks native multi-line input; wraps single-line as temporary workaround.
pub fn text_area<'a>(
    value: &'a str,
    placeholder: &'a str,
    _height: f32, // Reserved for when iced adds multi-line support
    on_change: impl Fn(String) -> Message + 'a,
) -> Element<'a, Message> {
    text_input(placeholder, value)
        .on_input(on_change)
        .width(Length::Fill)
        .style(theme::text_input_style)
        .into()
}

pub fn address_input<'a>(
    ip: &'a str,
    port: &'a str,
    on_ip_change: impl Fn(String) -> Message + 'a,
    on_port_change: impl Fn(String) -> Message + 'a,
) -> Element<'a, Message> {
    row![
        text_input("0.0.0.0", ip)
            .on_input(on_ip_change)
            .width(Length::Fixed(150.0))
            .style(theme::text_input_style),
        text(":").size(20).style(|_theme| text::Style {
            color: Some(theme::colors::TEXT_PRIMARY),
        }),
        text_input("3389", port)
            .on_input(on_port_change)
            .width(Length::Fixed(70.0))
            .style(theme::text_input_style),
    ]
    .spacing(4)
    .align_y(Alignment::Center)
    .into()
}

pub fn status_indicator<'a>(running: bool, status_text: &'a str) -> Element<'a, Message> {
    let color = theme::status_indicator_color(running);
    row![
        text("●")
            .size(16)
            .style(move |_theme| text::Style { color: Some(color) }),
        text(status_text),
    ]
    .spacing(8)
    .align_y(Alignment::Center)
    .into()
}

pub fn service_level_badge<'a>(level: &'a str, emoji: &'a str) -> Element<'a, Message> {
    let color = theme::service_level_color(level);
    row![
        text(emoji).size(14),
        text(level)
            .size(13)
            .style(move |_theme| text::Style { color: Some(color) }),
    ]
    .spacing(4)
    .align_y(Alignment::Center)
    .into()
}
