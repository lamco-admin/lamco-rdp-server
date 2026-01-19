//! lamco-rdp-server-gui entry point
//!
//! GUI binary for configuring the lamco-rdp-server.

use iced::{Font, Size};

use lamco_rdp_server::gui::app::ConfigGuiApp;

/// Professional sans-serif font for the UI
/// Liberation Sans is similar to Arial/Helvetica and widely available on Linux
const FONT: Font = Font::with_name("Liberation Sans");

fn main() -> iced::Result {
    // Run the application with window configuration
    iced::application(ConfigGuiApp::new, ConfigGuiApp::update, ConfigGuiApp::view)
        .title("Lamco RDP Server")
        .window_size(Size::new(1200.0, 800.0))
        .centered()
        .antialiasing(true)
        .default_font(FONT)
        .subscription(ConfigGuiApp::subscription)
        .run()
}
