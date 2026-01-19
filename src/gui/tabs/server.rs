//! Server Configuration Tab
//!
//! Basic server settings: listen address, max connections, timeouts, portals.

use iced::widget::{column, row, space, text};
use iced::{Alignment, Element};

use crate::gui::message::Message;
use crate::gui::state::AppState;
use crate::gui::widgets;

pub fn view_server_tab(state: &AppState) -> Element<'_, Message> {
    column![
        // Section header
        widgets::section_header("Server Configuration"),
        space().height(20.0),
        // Listen Address
        widgets::labeled_row_with_help(
            "Listen Address:",
            150.0,
            widgets::address_input(
                &state.edit_strings.server_ip,
                &state.edit_strings.server_port,
                Message::ServerListenAddrChanged,
                Message::ServerPortChanged,
            ),
            "IP address and port for RDP server",
        ),
        space().height(16.0),
        // Maximum Connections
        widgets::labeled_row_with_help(
            "Maximum Connections:",
            150.0,
            widgets::number_input(
                &state.edit_strings.max_connections,
                "10",
                100.0,
                Message::ServerMaxConnectionsChanged,
            ),
            "Maximum number of simultaneous clients (1-100)",
        ),
        space().height(16.0),
        // Session Timeout
        widgets::labeled_row_with_help(
            "Session Timeout:",
            150.0,
            Element::from(
                row![
                    widgets::number_input(
                        &state.edit_strings.session_timeout,
                        "0",
                        100.0,
                        Message::ServerSessionTimeoutChanged,
                    ),
                    text("seconds"),
                ]
                .spacing(8)
                .align_y(Alignment::Center)
            ),
            "Auto-disconnect idle sessions (0 = never)",
        ),
        space().height(16.0),
        // Use XDG Portals
        widgets::toggle_with_help(
            "Use XDG Desktop Portals",
            state.config.server.use_portals,
            "Required for Wayland screen capture and input injection",
            Message::ServerUsePortalsToggled,
        ),
    ]
    .spacing(8)
    .padding(20)
    .into()
}
