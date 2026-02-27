//! Server Configuration Tab
//!
//! Basic server settings: listen address, max connections, timeouts, portals.

use iced::{
    widget::{column, row, text},
    Alignment, Element,
};

use crate::gui::{message::Message, state::AppState, widgets, widgets::space};

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
        widgets::labeled_row_pending_with_note(
            "Maximum Connections:",
            150.0,
            widgets::number_input(
                &state.edit_strings.max_connections,
                "10",
                100.0,
                Message::ServerMaxConnectionsChanged,
            ),
            "IronRDP processes one client at a time; queued connections not yet implemented",
        ),
        space().height(16.0),
        // Session Timeout
        widgets::labeled_row_pending_with_note(
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
            "Idle timeout tracking not yet implemented (0 = no timeout)",
        ),
        space().height(16.0),
        // Use XDG Portals
        widgets::toggle_pending_with_note(
            "Use XDG Desktop Portals",
            state.config.server.use_portals,
            Message::ServerUsePortalsToggled,
            "Always enabled - required for Wayland",
        ),
        space().height(16.0),
        // View-only mode
        widgets::toggle_with_help(
            "View-only mode",
            state.config.server.view_only,
            "Video streaming only. Disables input injection and clipboard. \
             Useful for monitoring, presentations, or compositors without RemoteDesktop portal.",
            Message::ServerViewOnlyToggled,
        ),
        space().height(20.0),
        // GUI Behavior section
        widgets::section_header("GUI Behavior"),
        space().height(12.0),
        // Close stops server toggle
        widgets::toggle_with_help(
            "Closing GUI stops server",
            state.close_stops_server,
            "When disabled, closing the GUI leaves the server running in the background",
            Message::ToggleCloseStopsServer,
        ),
    ]
    .spacing(8)
    .padding(20)
    .into()
}
