//! Status & Monitoring Tab
//!
//! Server status, service registry display, and live log viewer.

use iced::widget::{button, column, container, pick_list, row, scrollable, space, text};
use iced::{Alignment, Element, Length};

use crate::gui::message::Message;
use crate::gui::state::{AppState, LogLevel, ServerStatus, ServiceLevel};
use crate::gui::theme;
use crate::gui::widgets;

const LOG_LEVELS: &[&str] = &["Trace", "Debug", "Info", "Warn", "Error"];

pub fn view_status_tab(state: &AppState) -> Element<'_, Message> {
    column![
        // Section header
        widgets::section_header("Status & Monitoring"),
        space().height(16.0),
        // Server Status section
        widgets::subsection_header("Server Status"),
        space().height(8.0),
        view_server_status(state),
        space().height(20.0),
        // Detected Capabilities section
        widgets::collapsible_header(
            "Detected Capabilities & Service Registry",
            true,                         // Always expanded
            Message::RefreshCapabilities, // Just use refresh as a placeholder
        ),
        space().height(8.0),
        view_capabilities_section(state),
        space().height(20.0),
        // Live Logs section
        widgets::subsection_header("Live Logs"),
        space().height(8.0),
        view_log_viewer(state),
    ]
    .spacing(4)
    .padding(20)
    .into()
}

fn view_server_status(state: &AppState) -> Element<'_, Message> {
    let (status_text, status_color, is_running) = match &state.server_status {
        ServerStatus::Unknown => ("Unknown", theme::colors::TEXT_MUTED, false),
        ServerStatus::Stopped => ("Stopped", theme::colors::ERROR, false),
        ServerStatus::Starting => ("Starting...", theme::colors::WARNING, false),
        ServerStatus::Running { .. } => ("Running", theme::colors::SUCCESS, true),
        ServerStatus::Error(_) => ("Error", theme::colors::ERROR, false),
    };

    let status_details = match &state.server_status {
        ServerStatus::Running {
            connections,
            uptime,
            address,
        } => {
            let hours = uptime.as_secs() / 3600;
            let minutes = (uptime.as_secs() % 3600) / 60;
            let seconds = uptime.as_secs() % 60;
            format!(
                "Uptime: {}h {}m {}s | Connections: {} | Address: {}",
                hours, minutes, seconds, connections, address
            )
        }
        ServerStatus::Error(msg) => format!("Error: {}", msg),
        _ => String::new(),
    };

    container(
        column![
            row![
                text("●").size(16).style(move |_theme| text::Style {
                    color: Some(status_color),
                }),
                text(format!("Status: {}", status_text)).size(16),
            ]
            .spacing(8)
            .align_y(Alignment::Center),
            if !status_details.is_empty() {
                Element::from(text(status_details).size(13).style(|_theme: &iced::Theme| {
                    text::Style {
                        color: Some(theme::colors::TEXT_SECONDARY),
                    }
                }))
            } else {
                Element::from(space().height(0.0))
            },
            space().height(12.0),
            row![
                if is_running {
                    Element::from(
                        button(text("Stop Server"))
                            .on_press(Message::StopServer)
                            .padding([8, 16])
                            .style(theme::danger_button_style),
                    )
                } else {
                    Element::from(
                        button(text("Start Server"))
                            .on_press(Message::StartServer)
                            .padding([8, 16])
                            .style(theme::primary_button_style),
                    )
                },
                space().width(8.0),
                button(text("Restart Server"))
                    .on_press(Message::RestartServer)
                    .padding([8, 16])
                    .style(theme::secondary_button_style),
            ]
            .spacing(8),
        ]
        .spacing(4)
        .padding(16),
    )
    .style(theme::section_container_style)
    .into()
}

/// Capabilities and service registry view
fn view_capabilities_section(state: &AppState) -> Element<'_, Message> {
    if let Some(ref caps) = state.detected_capabilities {
        // Pre-compute string values to avoid lifetime issues
        let portal_version_str = caps.portal_version.to_string();
        let deployment_str = caps.deployment_context.to_string();
        let xdg_runtime_str = caps.xdg_runtime_dir.display().to_string();
        let screencast_version_str = caps
            .screencast_version
            .map(|v| format!("v{}", v))
            .unwrap_or_else(|| "N/A".to_string());
        let remote_desktop_version_str = caps
            .remote_desktop_version
            .map(|v| format!("v{}", v))
            .unwrap_or_else(|| "N/A".to_string());
        let compositor_full = format!(
            "{} ({})",
            caps.compositor_name,
            caps.compositor_version.as_deref().unwrap_or("unknown")
        );

        container(
            column![
                // System Detection subsection
                text("System Detection").size(14),
                space().height(8.0),

                container(
                    column![
                        labeled_value("Compositor:", &compositor_full),
                        labeled_value("Distribution:", &caps.distribution),
                        labeled_value("Kernel:", &caps.kernel_version),
                        space().height(8.0),
                        labeled_value("Portal Version:", &portal_version_str),
                        labeled_value("Portal Backend:", &caps.portal_backend),
                        labeled_value("ScreenCast:", &screencast_version_str),
                        labeled_value("RemoteDesktop:", &remote_desktop_version_str),
                        space().height(8.0),
                        labeled_value("Deployment:", &deployment_str),
                        labeled_value("XDG_RUNTIME_DIR:", &xdg_runtime_str),
                        space().height(8.0),

                        // Quirks
                        if caps.quirks.is_empty() {
                            Element::from(text("Platform Quirks: None detected ✅").size(13))
                        } else {
                            let mut quirk_elements: Vec<Element<'_, Message>> = vec![
                                Element::from(text("Platform Quirks:").size(13)),
                            ];
                            quirk_elements.extend(caps.quirks.iter().map(|q| {
                                Element::from(
                                    text(format!("  • {} - {}", q.quirk_id, q.description))
                                        .size(12)
                                        .style(|_theme: &iced::Theme| text::Style {
                                            color: Some(theme::colors::WARNING),
                                        })
                                )
                            }));
                            Element::from(column(quirk_elements))
                        },
                        space().height(8.0),
                        labeled_value("Session Persistence:", &caps.persistence_strategy),
                    ]
                    .spacing(2)
                    .padding(12),
                )
                .style(|_theme| container::Style {
                    background: Some(iced::Background::Color(theme::colors::SURFACE_DARK)),
                    border: iced::Border {
                        radius: 4.0.into(),
                        ..Default::default()
                    },
                    ..Default::default()
                }),

                space().height(12.0),

                row![
                    button(text("Refresh Detection"))
                        .on_press(Message::RefreshCapabilities)
                        .padding([6, 12])
                        .style(theme::secondary_button_style),
                    button(text("Export Capabilities"))
                        .on_press(Message::ExportCapabilities)
                        .padding([6, 12])
                        .style(theme::secondary_button_style),
                ]
                .spacing(8),

                space().height(16.0),

                // Service Registry subsection
                text(format!("Service Registry ({} Services)", caps.services.len())).size(14),
                space().height(8.0),

                view_service_registry_table(&caps.services),

                space().height(8.0),

                // Summary
                text(format!(
                    "Summary: ✅ {} Guaranteed │ 🔶 {} BestEffort │ ⚠️ {} Degraded │ ❌ {} Unavailable",
                    caps.guaranteed_count,
                    caps.best_effort_count,
                    caps.degraded_count,
                    caps.unavailable_count
                ))
                .size(13),

                space().height(8.0),

                // Performance hints
                if caps.recommended_fps.is_some() || caps.recommended_codec.is_some() {
                    Element::from(column![
                        text("Performance Hints:").size(13),
                        if let Some(fps) = caps.recommended_fps {
                            Element::from(text(format!("  • Recommended FPS: {}", fps)).size(12))
                        } else {
                            Element::from(space().height(0.0))
                        },
                        if let Some(ref codec) = caps.recommended_codec {
                            Element::from(text(format!("  • Recommended Codec: {}", codec)).size(12))
                        } else {
                            Element::from(space().height(0.0))
                        },
                        text(format!("  • Zero-copy: {}",
                            if caps.zero_copy_available { "Available" } else { "Not available" }
                        )).size(12),
                    ])
                } else {
                    Element::from(space().height(0.0))
                },
            ]
            .spacing(4),
        )
        .padding(12)
        .style(theme::section_container_style)
        .into()
    } else {
        container(
            column![
                text("No capabilities detected yet.").size(14),
                space().height(12.0),
                button(text("Detect Capabilities"))
                    .on_press(Message::RefreshCapabilities)
                    .padding([8, 16])
                    .style(theme::primary_button_style),
            ]
            .spacing(4)
            .padding(16),
        )
        .style(theme::section_container_style)
        .into()
    }
}

/// Helper for labeled values
fn labeled_value(label: impl Into<String>, value: impl Into<String>) -> Element<'static, Message> {
    let label_str: String = label.into();
    let value_str: String = value.into();
    row![
        text(label_str)
            .size(13)
            .width(Length::Fixed(140.0))
            .style(|_theme: &iced::Theme| text::Style {
                color: Some(theme::colors::TEXT_SECONDARY),
            }),
        text(value_str).size(13),
    ]
    .spacing(8)
    .into()
}

/// Service registry table view
fn view_service_registry_table(
    services: &[crate::gui::state::ServiceInfo],
) -> Element<'_, Message> {
    // Header
    let header = row![
        text("Service").width(Length::FillPortion(3)).size(12),
        text("Level").width(Length::FillPortion(2)).size(12),
        text("Wayland Source")
            .width(Length::FillPortion(3))
            .size(12),
        text("RDP Cap").width(Length::FillPortion(2)).size(12),
    ]
    .spacing(8)
    .padding([4, 8]);

    // Rows
    let rows: Vec<Element<'_, Message>> = services
        .iter()
        .map(|service| {
            let level_color = match service.level {
                ServiceLevel::Guaranteed => theme::colors::GUARANTEED,
                ServiceLevel::BestEffort => theme::colors::BEST_EFFORT,
                ServiceLevel::Degraded => theme::colors::DEGRADED,
                ServiceLevel::Unavailable => theme::colors::UNAVAILABLE,
            };

            let service_row: Element<'_, Message> = row![
                text(format!("{} {}", service.level_emoji, service.name))
                    .width(Length::FillPortion(3))
                    .size(12),
                text(service.level.to_string())
                    .width(Length::FillPortion(2))
                    .size(12)
                    .style(move |_theme| text::Style {
                        color: Some(level_color),
                    }),
                text(service.wayland_source.as_deref().unwrap_or("-"))
                    .width(Length::FillPortion(3))
                    .size(12),
                text(service.rdp_capability.as_deref().unwrap_or("-"))
                    .width(Length::FillPortion(2))
                    .size(12),
            ]
            .spacing(8)
            .padding([2, 8])
            .into();

            // Add notes if present
            if service.notes.is_empty() {
                service_row
            } else {
                let notes: Vec<Element<'_, Message>> = service
                    .notes
                    .iter()
                    .map(|note| {
                        text(format!("    ↳ {}", note))
                            .size(11)
                            .style(|_theme| text::Style {
                                color: Some(theme::colors::TEXT_MUTED),
                            })
                            .into()
                    })
                    .collect();

                column![service_row].extend(notes).into()
            }
        })
        .collect();

    container(scrollable(column![header].extend(rows).spacing(2)).height(Length::Fixed(200.0)))
        .style(|_theme| container::Style {
            background: Some(iced::Background::Color(theme::colors::SURFACE_DARK)),
            border: iced::Border {
                radius: 4.0.into(),
                ..Default::default()
            },
            ..Default::default()
        })
        .into()
}

/// Log viewer component
fn view_log_viewer(state: &AppState) -> Element<'_, Message> {
    let filter_level = match state.log_filter_level {
        LogLevel::Trace => "Trace",
        LogLevel::Debug => "Debug",
        LogLevel::Info => "Info",
        LogLevel::Warn => "Warn",
        LogLevel::Error => "Error",
    };

    column![
        // Log viewer controls
        row![
            text("Filter Level:").size(13),
            pick_list(LOG_LEVELS.to_vec(), Some(filter_level), |s| {
                Message::LogFilterLevelChanged(s.to_string())
            },)
            .width(Length::Fixed(100.0)),
            space().width(Length::Fill),
            widgets::toggle_switch("Auto-scroll", state.log_auto_scroll, |_| {
                Message::ToggleLogAutoScroll
            },),
            space().width(16.0),
            button(text("Clear"))
                .on_press(Message::ClearLogs)
                .padding([4, 12])
                .style(theme::secondary_button_style),
            button(text("Export"))
                .on_press(Message::ExportLogs)
                .padding([4, 12])
                .style(theme::secondary_button_style),
        ]
        .spacing(8)
        .align_y(Alignment::Center),
        space().height(8.0),
        // Log content
        container(
            scrollable(
                column(
                    state
                        .filtered_log_lines()
                        .map(|line| {
                            let level_color = match line.level {
                                LogLevel::Error => theme::colors::LOG_ERROR,
                                LogLevel::Warn => theme::colors::LOG_WARN,
                                LogLevel::Info => theme::colors::LOG_INFO,
                                LogLevel::Debug => theme::colors::LOG_DEBUG,
                                LogLevel::Trace => theme::colors::LOG_TRACE,
                            };

                            row![
                                text(&line.timestamp)
                                    .size(11)
                                    .width(Length::Fixed(80.0))
                                    .style(|_theme| text::Style {
                                        color: Some(theme::colors::TEXT_MUTED),
                                    }),
                                text(line.level.to_string())
                                    .size(11)
                                    .width(Length::Fixed(50.0))
                                    .style(move |_theme| text::Style {
                                        color: Some(level_color),
                                    }),
                                text(&line.message).size(11).style(|_theme| text::Style {
                                    color: Some(iced::Color::from_rgb(0.9, 0.9, 0.9)),
                                }),
                            ]
                            .spacing(8)
                            .into()
                        })
                        .collect::<Vec<_>>()
                )
                .spacing(1)
                .padding(8),
            )
            .height(Length::Fixed(250.0)),
        )
        .style(theme::log_viewer_style),
        space().height(4.0),
        text(format!(
            "Showing {} lines (filter: {} and above)",
            state.filtered_log_lines().count(),
            filter_level
        ))
        .size(11)
        .style(|_theme| text::Style {
            color: Some(theme::colors::TEXT_MUTED),
        }),
    ]
    .spacing(4)
    .into()
}
