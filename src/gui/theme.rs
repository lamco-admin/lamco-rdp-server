//! Professional dark theme for enterprise server management.
//!
//! Designed to convey sophistication and reliability - think Grafana,
//! DataDog, or enterprise admin consoles. Dark backgrounds reduce eye
//! strain during extended use and make status indicators pop.

use iced::widget::{button, container, text_input};
use iced::{Background, Border, Color, Shadow, Theme};

/// Professional dark color palette with cyan/teal accents.
/// Inspired by modern DevOps and infrastructure dashboards.
pub mod colors {
    use iced::Color;

    // Brand accent - sophisticated teal/cyan
    pub const PRIMARY: Color = Color::from_rgb(0.0, 0.75, 0.85); // #00BFD8 - vibrant cyan
    pub const PRIMARY_LIGHT: Color = Color::from_rgb(0.2, 0.85, 0.95); // Lighter for hover
    pub const PRIMARY_DARK: Color = Color::from_rgb(0.0, 0.55, 0.65); // Deeper for pressed

    // Dark slate backgrounds - layered for depth
    pub const BACKGROUND: Color = Color::from_rgb(0.08, 0.09, 0.11); // #14171C - deepest
    pub const SURFACE: Color = Color::from_rgb(0.11, 0.12, 0.14); // #1C1F24 - cards/panels
    pub const SURFACE_DARK: Color = Color::from_rgb(0.14, 0.15, 0.18); // #242630 - elevated
    pub const SURFACE_LIGHT: Color = Color::from_rgb(0.18, 0.19, 0.22); // #2E3138 - hover states

    // Borders - subtle definition
    pub const BORDER: Color = Color::from_rgb(0.22, 0.24, 0.28); // #383D47
    pub const BORDER_LIGHT: Color = Color::from_rgb(0.28, 0.30, 0.35); // Highlighted borders

    // Typography - high contrast for readability
    pub const TEXT_PRIMARY: Color = Color::from_rgb(0.93, 0.94, 0.96); // #EDEFFA - bright white
    pub const TEXT_SECONDARY: Color = Color::from_rgb(0.65, 0.68, 0.75); // #A6AEC0 - muted
    pub const TEXT_MUTED: Color = Color::from_rgb(0.45, 0.48, 0.55); // #737A8C - subtle

    // Semantic status colors - vivid for visibility on dark bg
    pub const SUCCESS: Color = Color::from_rgb(0.2, 0.85, 0.5); // #33D980 - green
    pub const WARNING: Color = Color::from_rgb(1.0, 0.75, 0.2); // #FFBF33 - amber
    pub const ERROR: Color = Color::from_rgb(1.0, 0.35, 0.4); // #FF5966 - red
    pub const INFO: Color = Color::from_rgb(0.4, 0.65, 1.0); // #66A6FF - blue

    // Service registry levels - enterprise dashboard style
    pub const GUARANTEED: Color = Color::from_rgb(0.2, 0.9, 0.55); // Bright green
    pub const BEST_EFFORT: Color = Color::from_rgb(0.4, 0.7, 1.0); // Sky blue
    pub const DEGRADED: Color = Color::from_rgb(1.0, 0.75, 0.3); // Amber
    pub const UNAVAILABLE: Color = Color::from_rgb(0.55, 0.58, 0.65); // Gray

    // Log viewer - terminal aesthetic
    pub const LOG_ERROR: Color = Color::from_rgb(1.0, 0.4, 0.45);
    pub const LOG_WARN: Color = Color::from_rgb(1.0, 0.8, 0.3);
    pub const LOG_INFO: Color = Color::from_rgb(0.7, 0.75, 0.8);
    pub const LOG_DEBUG: Color = Color::from_rgb(0.5, 0.7, 1.0);
    pub const LOG_TRACE: Color = Color::from_rgb(0.5, 0.52, 0.58);

    // Tab bar
    pub const TAB_ACTIVE: Color = PRIMARY;
    pub const TAB_INACTIVE: Color = TEXT_MUTED;
    pub const TAB_HOVER: Color = PRIMARY_LIGHT;

    // Header gradient endpoints (for future gradient support)
    pub const HEADER_BG: Color = Color::from_rgb(0.06, 0.07, 0.09); // Darkest
}

/// Subtle shadow for elevation
fn card_shadow() -> Shadow {
    Shadow {
        color: Color::from_rgba(0.0, 0.0, 0.0, 0.4),
        offset: iced::Vector::new(0.0, 4.0),
        blur_radius: 12.0,
    }
}

/// Glow effect for primary actions
fn accent_glow() -> Shadow {
    Shadow {
        color: Color::from_rgba(0.0, 0.75, 0.85, 0.25),
        offset: iced::Vector::new(0.0, 0.0),
        blur_radius: 8.0,
    }
}

pub fn primary_button_style(_theme: &Theme, status: button::Status) -> button::Style {
    let base = button::Style {
        background: Some(Background::Color(colors::PRIMARY)),
        text_color: Color::from_rgb(0.05, 0.05, 0.08), // Dark text on bright button
        border: Border {
            color: colors::PRIMARY_LIGHT,
            width: 1.0,
            radius: 6.0.into(),
        },
        shadow: accent_glow(),
        snap: false,
    };

    match status {
        button::Status::Active => base,
        button::Status::Hovered => button::Style {
            background: Some(Background::Color(colors::PRIMARY_LIGHT)),
            shadow: Shadow {
                color: Color::from_rgba(0.0, 0.85, 0.95, 0.35),
                offset: iced::Vector::new(0.0, 0.0),
                blur_radius: 12.0,
            },
            ..base
        },
        button::Status::Pressed => button::Style {
            background: Some(Background::Color(colors::PRIMARY_DARK)),
            shadow: Shadow::default(),
            ..base
        },
        button::Status::Disabled => button::Style {
            background: Some(Background::Color(colors::SURFACE_LIGHT)),
            text_color: colors::TEXT_MUTED,
            shadow: Shadow::default(),
            ..base
        },
    }
}

pub fn secondary_button_style(_theme: &Theme, status: button::Status) -> button::Style {
    let base = button::Style {
        background: Some(Background::Color(colors::SURFACE_LIGHT)),
        text_color: colors::TEXT_PRIMARY,
        border: Border {
            color: colors::BORDER,
            width: 1.0,
            radius: 6.0.into(),
        },
        shadow: Shadow::default(),
        snap: false,
    };

    match status {
        button::Status::Active => base,
        button::Status::Hovered => button::Style {
            background: Some(Background::Color(Color::from_rgb(0.22, 0.24, 0.28))),
            border: Border {
                color: colors::PRIMARY,
                ..base.border
            },
            ..base
        },
        button::Status::Pressed => button::Style {
            background: Some(Background::Color(colors::SURFACE)),
            ..base
        },
        button::Status::Disabled => button::Style {
            background: Some(Background::Color(colors::SURFACE)),
            text_color: colors::TEXT_MUTED,
            ..base
        },
    }
}

pub fn danger_button_style(_theme: &Theme, status: button::Status) -> button::Style {
    let base = button::Style {
        background: Some(Background::Color(colors::ERROR)),
        text_color: Color::WHITE,
        border: Border {
            color: Color::from_rgb(1.0, 0.45, 0.5),
            width: 1.0,
            radius: 6.0.into(),
        },
        shadow: Shadow {
            color: Color::from_rgba(1.0, 0.35, 0.4, 0.25),
            offset: iced::Vector::new(0.0, 0.0),
            blur_radius: 8.0,
        },
        snap: false,
    };

    match status {
        button::Status::Active => base,
        button::Status::Hovered => button::Style {
            background: Some(Background::Color(Color::from_rgb(1.0, 0.45, 0.5))),
            shadow: Shadow {
                color: Color::from_rgba(1.0, 0.35, 0.4, 0.4),
                offset: iced::Vector::new(0.0, 0.0),
                blur_radius: 12.0,
            },
            ..base
        },
        button::Status::Pressed => button::Style {
            background: Some(Background::Color(Color::from_rgb(0.85, 0.25, 0.3))),
            shadow: Shadow::default(),
            ..base
        },
        button::Status::Disabled => button::Style {
            background: Some(Background::Color(colors::SURFACE_LIGHT)),
            text_color: colors::TEXT_MUTED,
            shadow: Shadow::default(),
            ..base
        },
    }
}

pub fn success_button_style(_theme: &Theme, status: button::Status) -> button::Style {
    let base = button::Style {
        background: Some(Background::Color(colors::SUCCESS)),
        text_color: Color::from_rgb(0.05, 0.15, 0.08), // Dark text on bright green
        border: Border {
            color: Color::from_rgb(0.3, 0.9, 0.6),
            width: 1.0,
            radius: 6.0.into(),
        },
        shadow: Shadow {
            color: Color::from_rgba(0.2, 0.85, 0.5, 0.25),
            offset: iced::Vector::new(0.0, 0.0),
            blur_radius: 8.0,
        },
        snap: false,
    };

    match status {
        button::Status::Active => base,
        button::Status::Hovered => button::Style {
            background: Some(Background::Color(Color::from_rgb(0.3, 0.9, 0.6))),
            shadow: Shadow {
                color: Color::from_rgba(0.2, 0.85, 0.5, 0.4),
                offset: iced::Vector::new(0.0, 0.0),
                blur_radius: 12.0,
            },
            ..base
        },
        button::Status::Pressed => button::Style {
            background: Some(Background::Color(Color::from_rgb(0.15, 0.7, 0.4))),
            shadow: Shadow::default(),
            ..base
        },
        button::Status::Disabled => button::Style {
            background: Some(Background::Color(colors::SURFACE_LIGHT)),
            text_color: colors::TEXT_MUTED,
            shadow: Shadow::default(),
            ..base
        },
    }
}

pub fn tab_button_style(active: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |_theme: &Theme, status: button::Status| {
        let base = button::Style {
            background: Some(Background::Color(if active {
                colors::SURFACE_LIGHT
            } else {
                Color::TRANSPARENT
            })),
            text_color: if active {
                colors::PRIMARY
            } else {
                colors::TEXT_SECONDARY
            },
            border: Border {
                color: if active {
                    colors::PRIMARY
                } else {
                    Color::TRANSPARENT
                },
                width: if active { 0.0 } else { 0.0 },
                radius: 8.0.into(),
            },
            shadow: Shadow::default(),
            snap: false,
        };

        match status {
            button::Status::Active => base,
            button::Status::Hovered => button::Style {
                background: Some(Background::Color(colors::SURFACE_LIGHT)),
                text_color: colors::PRIMARY_LIGHT,
                ..base
            },
            button::Status::Pressed => button::Style {
                background: Some(Background::Color(colors::SURFACE)),
                text_color: colors::PRIMARY,
                ..base
            },
            button::Status::Disabled => base,
        }
    }
}

pub fn section_container_style(_theme: &Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(colors::SURFACE)),
        border: Border {
            color: colors::BORDER,
            width: 1.0,
            radius: 10.0.into(),
        },
        text_color: Some(colors::TEXT_PRIMARY),
        shadow: card_shadow(),
        snap: false,
    }
}

/// Dark background for terminal-like log viewer
pub fn log_viewer_style(_theme: &Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(Color::from_rgb(0.05, 0.06, 0.08))),
        border: Border {
            color: colors::BORDER,
            width: 1.0,
            radius: 6.0.into(),
        },
        text_color: Some(Color::from_rgb(0.85, 0.87, 0.9)),
        shadow: Shadow {
            color: Color::from_rgba(0.0, 0.0, 0.0, 0.3),
            offset: iced::Vector::new(0.0, 2.0),
            blur_radius: 4.0,
        },
        snap: false,
    }
}

pub fn collapsible_header_style(_theme: &Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(colors::SURFACE_DARK)),
        border: Border {
            color: colors::BORDER,
            width: 1.0,
            radius: 6.0.into(),
        },
        text_color: Some(colors::TEXT_PRIMARY),
        shadow: Shadow::default(),
        snap: false,
    }
}

pub fn preset_button_style(selected: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |_theme: &Theme, status: button::Status| {
        let base = button::Style {
            background: Some(Background::Color(if selected {
                colors::PRIMARY
            } else {
                colors::SURFACE_LIGHT
            })),
            text_color: if selected {
                Color::from_rgb(0.05, 0.05, 0.08)
            } else {
                colors::TEXT_PRIMARY
            },
            border: Border {
                color: if selected {
                    colors::PRIMARY_LIGHT
                } else {
                    colors::BORDER
                },
                width: 1.0,
                radius: 6.0.into(),
            },
            shadow: if selected {
                accent_glow()
            } else {
                Shadow::default()
            },
            snap: false,
        };

        match status {
            button::Status::Active => base,
            button::Status::Hovered => button::Style {
                background: Some(Background::Color(if selected {
                    colors::PRIMARY_LIGHT
                } else {
                    Color::from_rgb(0.22, 0.24, 0.28)
                })),
                ..base
            },
            button::Status::Pressed => button::Style {
                background: Some(Background::Color(colors::PRIMARY_DARK)),
                text_color: Color::WHITE,
                ..base
            },
            button::Status::Disabled => button::Style {
                background: Some(Background::Color(colors::SURFACE)),
                text_color: colors::TEXT_MUTED,
                shadow: Shadow::default(),
                ..base
            },
        }
    }
}

pub fn status_indicator_color(running: bool) -> Color {
    if running {
        colors::SUCCESS
    } else {
        colors::ERROR
    }
}

pub fn service_level_color(level: &str) -> Color {
    match level.to_lowercase().as_str() {
        "guaranteed" => colors::GUARANTEED,
        "besteffort" | "best_effort" => colors::BEST_EFFORT,
        "degraded" => colors::DEGRADED,
        "unavailable" => colors::UNAVAILABLE,
        _ => colors::TEXT_SECONDARY,
    }
}

pub fn log_level_color(level: &str) -> Color {
    match level.to_lowercase().as_str() {
        "error" => colors::LOG_ERROR,
        "warn" | "warning" => colors::LOG_WARN,
        "info" => colors::LOG_INFO,
        "debug" => colors::LOG_DEBUG,
        "trace" => colors::LOG_TRACE,
        _ => colors::TEXT_PRIMARY,
    }
}

/// Header bar style - slightly darker than content for visual separation
pub fn header_style(_theme: &Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(colors::HEADER_BG)),
        border: Border {
            color: colors::BORDER,
            width: 0.0,
            radius: 0.0.into(),
        },
        text_color: Some(colors::TEXT_PRIMARY),
        shadow: Shadow {
            color: Color::from_rgba(0.0, 0.0, 0.0, 0.5),
            offset: iced::Vector::new(0.0, 2.0),
            blur_radius: 8.0,
        },
        snap: false,
    }
}

/// Server status badge container
pub fn status_badge_style(running: bool) -> impl Fn(&Theme) -> container::Style {
    move |_theme: &Theme| {
        let (bg, border) = if running {
            (
                Color::from_rgba(0.2, 0.85, 0.5, 0.15), // Green tint
                Color::from_rgba(0.2, 0.85, 0.5, 0.4),
            )
        } else {
            (
                Color::from_rgba(1.0, 0.35, 0.4, 0.15), // Red tint
                Color::from_rgba(1.0, 0.35, 0.4, 0.4),
            )
        };

        container::Style {
            background: Some(Background::Color(bg)),
            border: Border {
                color: border,
                width: 1.0,
                radius: 20.0.into(), // Pill shape
            },
            text_color: Some(colors::TEXT_PRIMARY),
            shadow: Shadow::default(),
            snap: false,
        }
    }
}

/// Text input style for dark theme - ensures readable text on dark backgrounds
pub fn text_input_style(_theme: &Theme, status: text_input::Status) -> text_input::Style {
    let base = text_input::Style {
        background: Background::Color(colors::SURFACE_LIGHT),
        border: Border {
            color: colors::BORDER,
            width: 1.0,
            radius: 6.0.into(),
        },
        icon: colors::TEXT_MUTED,
        placeholder: colors::TEXT_MUTED,
        value: colors::TEXT_PRIMARY,
        selection: colors::PRIMARY,
    };

    match status {
        text_input::Status::Active => base,
        text_input::Status::Hovered => text_input::Style {
            border: Border {
                color: colors::BORDER_LIGHT,
                ..base.border
            },
            ..base
        },
        text_input::Status::Focused { .. } => text_input::Style {
            border: Border {
                color: colors::PRIMARY,
                width: 2.0,
                ..base.border
            },
            ..base
        },
        text_input::Status::Disabled => text_input::Style {
            background: Background::Color(colors::SURFACE),
            value: colors::TEXT_MUTED,
            ..base
        },
    }
}

/// Category dropdown button style
pub fn category_button_style(
    is_active: bool,
    is_open: bool,
) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |_theme: &Theme, status: button::Status| {
        let bg_color = if is_open {
            colors::SURFACE_LIGHT
        } else if is_active {
            colors::SURFACE
        } else {
            Color::TRANSPARENT
        };

        let text_color = if is_active || is_open {
            colors::PRIMARY
        } else {
            colors::TEXT_SECONDARY
        };

        let base = button::Style {
            background: Some(Background::Color(bg_color)),
            text_color,
            border: Border {
                color: if is_open {
                    colors::PRIMARY
                } else if is_active {
                    colors::BORDER_LIGHT
                } else {
                    Color::TRANSPARENT
                },
                width: if is_open || is_active { 1.0 } else { 0.0 },
                radius: 6.0.into(),
            },
            shadow: Shadow::default(),
            snap: false,
        };

        match status {
            button::Status::Active => base,
            button::Status::Hovered => button::Style {
                background: Some(Background::Color(colors::SURFACE_LIGHT)),
                text_color: colors::PRIMARY_LIGHT,
                border: Border {
                    color: colors::PRIMARY,
                    width: 1.0,
                    ..base.border
                },
                ..base
            },
            button::Status::Pressed => button::Style {
                background: Some(Background::Color(colors::SURFACE)),
                text_color: colors::PRIMARY,
                ..base
            },
            button::Status::Disabled => base,
        }
    }
}

/// Dropdown menu item style
pub fn dropdown_item_style(is_selected: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |_theme: &Theme, status: button::Status| {
        let base = button::Style {
            background: Some(Background::Color(if is_selected {
                colors::SURFACE_LIGHT
            } else {
                Color::TRANSPARENT
            })),
            text_color: if is_selected {
                colors::PRIMARY
            } else {
                colors::TEXT_PRIMARY
            },
            border: Border {
                color: Color::TRANSPARENT,
                width: 0.0,
                radius: 4.0.into(),
            },
            shadow: Shadow::default(),
            snap: false,
        };

        match status {
            button::Status::Active => base,
            button::Status::Hovered => button::Style {
                background: Some(Background::Color(colors::SURFACE_LIGHT)),
                text_color: colors::PRIMARY_LIGHT,
                ..base
            },
            button::Status::Pressed => button::Style {
                background: Some(Background::Color(colors::SURFACE_DARK)),
                text_color: colors::PRIMARY,
                ..base
            },
            button::Status::Disabled => base,
        }
    }
}
