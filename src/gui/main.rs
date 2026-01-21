//! lamco-rdp-server-gui entry point
//!
//! GUI binary for configuring the lamco-rdp-server.
//!
//! # Capability Detection
//!
//! Before launching the GUI, this binary performs capability detection to
//! determine if the system can render the GUI properly. This prevents crashes
//! on VMs without GPU passthrough.

use iced::{Font, Size};
use std::panic;
use tracing::{error, info, warn};

use lamco_rdp_server::capabilities::{CapabilityManager, RenderingRecommendation};
use lamco_rdp_server::gui::app::ConfigGuiApp;

/// Professional sans-serif font for the UI
/// Liberation Sans is similar to Arial/Helvetica and widely available on Linux
const FONT: Font = Font::with_name("Liberation Sans");

fn main() {
    // Initialize logging
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            std::env::var("RUST_LOG")
                .unwrap_or_else(|_| "lamco=info,iced=warn,wgpu=warn,cosmic_text=warn".into()),
        )
        .try_init();

    // Run with proper error handling
    if let Err(e) = run() {
        error!("GUI failed: {}", e);
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize capability manager
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        CapabilityManager::initialize().await
    })?;

    // Get capabilities
    let caps = rt.block_on(async {
        CapabilityManager::global().read().await.state.clone()
    });

    // Check rendering capability
    match &caps.rendering.recommendation {
        RenderingRecommendation::NoGui { reason, suggestion } => {
            print_no_gui_message(reason, suggestion);
            return Err("GUI unavailable".into());
        }

        RenderingRecommendation::UseSoftware { reason } => {
            warn!("Using software rendering: {}", reason);
            configure_software_rendering();
        }

        RenderingRecommendation::UseGpu { reason } => {
            info!("Using GPU rendering: {}", reason);
        }
    }

    // Check for forced software rendering via environment
    if std::env::var("LAMCO_GUI_SOFTWARE").is_ok() {
        info!("Forced software rendering via LAMCO_GUI_SOFTWARE environment variable");
        configure_software_rendering();
    }

    // Set up panic handler for wgpu errors
    setup_panic_handler();

    // Run the GUI
    run_gui()
}

/// Configure environment for software rendering via llvmpipe
fn configure_software_rendering() {
    std::env::set_var("LIBGL_ALWAYS_SOFTWARE", "1");
    std::env::set_var("GALLIUM_DRIVER", "llvmpipe");
    std::env::set_var("MESA_GL_VERSION_OVERRIDE", "4.5");
}

/// Set up panic handler to catch wgpu/GPU errors and print helpful messages
fn setup_panic_handler() {
    let default_hook = panic::take_hook();
    panic::set_hook(Box::new(move |panic_info| {
        let payload = panic_info.payload();
        let msg = if let Some(s) = payload.downcast_ref::<&str>() {
            s.to_string()
        } else if let Some(s) = payload.downcast_ref::<String>() {
            s.clone()
        } else {
            "Unknown panic".to_string()
        };

        // Check if this is a wgpu/rendering error
        if msg.contains("wgpu")
            || msg.contains("Shader")
            || msg.contains("GPU")
            || msg.contains("Vulkan")
            || msg.contains("FLOAT16")
            || msg.contains("Capabilities")
        {
            print_gpu_error_message(&msg);
        } else {
            // Not a GPU error, use default handler
            default_hook(panic_info);
        }
    }));
}

/// Print a user-friendly message when GUI is unavailable
fn print_no_gui_message(reason: &str, suggestion: &str) {
    eprintln!();
    eprintln!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    eprintln!("  GUI Unavailable");
    eprintln!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    eprintln!();
    eprintln!("  Reason: {}", reason);
    eprintln!();
    eprintln!("  Suggestion: {}", suggestion);
    eprintln!();
    eprintln!("  For CLI usage:");
    eprintln!("    lamco-rdp-server --help");
    eprintln!("    lamco-rdp-server --show-capabilities");
    eprintln!();
    eprintln!("  To force software rendering (may work on some systems):");
    eprintln!("    LAMCO_GUI_SOFTWARE=1 lamco-rdp-server-gui");
    eprintln!();
    eprintln!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
}

/// Print a user-friendly message when GPU rendering fails
fn print_gpu_error_message(msg: &str) {
    eprintln!();
    eprintln!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    eprintln!("  GPU Rendering Error");
    eprintln!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    eprintln!();
    eprintln!("  Your system's GPU does not support required features.");
    eprintln!();
    eprintln!("  Error: {}", msg);
    eprintln!();
    eprintln!("  Solutions:");
    eprintln!("    1. Enable GPU passthrough in VM settings");
    eprintln!("    2. Install updated graphics drivers");
    eprintln!("    3. Force software rendering:");
    eprintln!("       LAMCO_GUI_SOFTWARE=1 lamco-rdp-server-gui");
    eprintln!("    4. Use CLI instead: lamco-rdp-server");
    eprintln!();
    eprintln!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    std::process::exit(1);
}

/// Run the iced GUI application
fn run_gui() -> Result<(), Box<dyn std::error::Error>> {
    iced::application(ConfigGuiApp::new, ConfigGuiApp::update, ConfigGuiApp::view)
        .title("Lamco RDP Server")
        .window_size(Size::new(1200.0, 800.0))
        .centered()
        .antialiasing(true)
        .default_font(FONT)
        .subscription(ConfigGuiApp::subscription)
        .run()?;

    Ok(())
}
