//! lamco-rdp-server-gui entry point
//!
//! GUI binary for configuring the lamco-rdp-server.
//!
//! # Capability Detection & Software Rendering
//!
//! Before launching the GUI, this binary performs capability detection to
//! determine if the system can render the GUI properly. If software rendering
//! is needed, the process re-executes itself with the appropriate environment
//! variables set. This is necessary because Mesa/OpenGL reads environment
//! variables at process startup, before main() runs.

use std::{env, os::unix::process::CommandExt, panic, process::Command};

use iced::{Font, Size};
use lamco_rdp_server::{
    capabilities::{Capabilities, RenderingRecommendation},
    gui::app::ConfigGuiApp,
};
use tracing::{error, info, warn};

/// Professional sans-serif font for the UI
/// Liberation Sans is similar to Arial/Helvetica and widely available on Linux
const FONT: Font = Font::with_name("Liberation Sans");

/// Environment variable marker indicating we've already configured software rendering
const SOFTWARE_RENDERING_CONFIGURED: &str = "LAMCO_SOFTWARE_RENDERING_CONFIGURED";

fn main() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            env::var("RUST_LOG")
                .unwrap_or_else(|_| "lamco=info,iced=warn,wgpu=warn,cosmic_text=warn".into()),
        )
        .try_init();

    if let Err(e) = run() {
        error!("GUI failed: {}", e);
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let software_configured = env::var(SOFTWARE_RENDERING_CONFIGURED).is_ok();
    let force_software = env::var("LAMCO_GUI_SOFTWARE").is_ok();

    if software_configured || env::var("LIBGL_ALWAYS_SOFTWARE").is_ok() {
        info!("Software rendering environment already configured");
        setup_panic_handler();
        return run_gui();
    }

    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async { Capabilities::initialize().await })?;

    let caps = rt.block_on(async { Capabilities::global().read().await.state.clone() });

    match &caps.rendering.recommendation {
        RenderingRecommendation::NoGui { reason, suggestion } => {
            print_no_gui_message(reason, suggestion);
            return Err("GUI unavailable".into());
        }

        RenderingRecommendation::UseSoftware { reason } => {
            warn!("Software rendering needed: {}", reason);
            // Re-exec with software rendering environment
            reexec_with_software_rendering();
        }

        RenderingRecommendation::UseGpu { reason } => {
            info!("Using GPU rendering: {}", reason);
        }
    }

    if force_software {
        info!("Forced software rendering via LAMCO_GUI_SOFTWARE environment variable");
        reexec_with_software_rendering();
    }

    setup_panic_handler();
    run_gui()
}

/// Re-execute the current process with software rendering environment variables set.
/// This is necessary because Mesa reads these variables at process startup.
fn reexec_with_software_rendering() -> ! {
    info!("Re-executing with software rendering environment...");

    let exe = env::current_exe().expect("Failed to get current executable path");
    let args: Vec<String> = env::args().skip(1).collect();

    let err = Command::new(&exe)
        .args(&args)
        .env("LIBGL_ALWAYS_SOFTWARE", "1")
        .env("GALLIUM_DRIVER", "llvmpipe")
        .env("MESA_GL_VERSION_OVERRIDE", "4.5")
        // Force OpenGL backend - llvmpipe Vulkan lacks SHADER_FLOAT16_IN_FLOAT32
        .env("WGPU_BACKEND", "gl")
        .env(SOFTWARE_RENDERING_CONFIGURED, "1")
        .exec();

    // exec() only returns on error
    eprintln!("Failed to re-exec: {}", err);
    std::process::exit(1);
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
        .exit_on_close_request(false)
        .run()?;

    Ok(())
}
