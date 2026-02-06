//! lamco-rdp-server - Wayland Remote Desktop Server
//!
//! Entry point for the server binary.

use anyhow::Result;
use clap::Parser;
use tracing::info;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

use lamco_rdp_server::config::Config;
use lamco_rdp_server::server::LamcoRdpServer;

/// Command-line arguments for lamco-rdp-server
#[derive(Parser, Debug)]
#[command(name = "lamco-rdp-server")]
#[command(version, about = "Wayland Remote Desktop Server", long_about = None)]
pub struct Args {
    /// Configuration file path
    #[arg(short, long, default_value = "/etc/lamco-rdp-server/config.toml")]
    pub config: String,

    /// Listen address
    #[arg(short, long, env = "LAMCO_RDP_LISTEN_ADDR")]
    pub listen: Option<String>,

    /// Listen port
    #[arg(short, long, env = "LAMCO_RDP_PORT", default_value = "3389")]
    pub port: u16,

    /// Verbose logging (can be specified multiple times)
    #[arg(short, long, action = clap::ArgAction::Count)]
    pub verbose: u8,

    /// Log format (json|pretty|compact)
    #[arg(long, default_value = "pretty")]
    pub log_format: String,

    /// Write logs to file (in addition to stdout)
    #[arg(long)]
    pub log_file: Option<String>,

    /// Grant permission for session persistence and exit (one-time setup)
    ///
    /// Triggers the portal permission dialog, obtains a restore token,
    /// and stores it for future unattended operation. Useful for initial
    /// setup on headless systems via SSH X11 forwarding.
    #[arg(long)]
    pub grant_permission: bool,

    /// Clear all stored session tokens
    #[arg(long)]
    pub clear_tokens: bool,

    /// Show session persistence status and exit
    ///
    /// Displays whether restore tokens are available, what deployment
    /// context is detected, and what credential storage method is in use.
    #[arg(long)]
    pub persistence_status: bool,

    /// Show detected compositor and portal capabilities and exit
    ///
    /// Useful for debugging detection issues and understanding what
    /// session strategies are available.
    #[arg(long)]
    pub show_capabilities: bool,

    /// Output format for --show-capabilities (text|json)
    ///
    /// Default is human-readable text. Use json for machine parsing,
    /// especially for integration with the GUI.
    #[arg(long, default_value = "text")]
    pub format: String,

    /// Run diagnostics and exit
    ///
    /// Tests deployment detection, portal connection, credential storage,
    /// and other components. Helpful for troubleshooting setup issues.
    #[arg(long)]
    pub diagnose: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    // Initialize logging
    init_logging(&args)?;

    info!("════════════════════════════════════════════════════════");
    info!("  lamco-rdp-server v{}", env!("CARGO_PKG_VERSION"));
    info!(
        "  Built: {} {}",
        option_env!("BUILD_DATE").unwrap_or("unknown"),
        option_env!("BUILD_TIME").unwrap_or("")
    );
    info!(
        "  Commit: {}",
        option_env!("GIT_HASH").unwrap_or("vendored")
    );
    info!(
        "  Profile: {}",
        if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        }
    );
    info!("════════════════════════════════════════════════════════");

    if args.show_capabilities {
        return show_capabilities(&args.format).await;
    }

    if args.persistence_status {
        return show_persistence_status().await;
    }

    if args.diagnose {
        return run_diagnostics().await;
    }

    if args.clear_tokens {
        return clear_tokens().await;
    }

    if args.grant_permission {
        return grant_permission_flow().await;
    }

    // Log startup diagnostics
    lamco_rdp_server::utils::log_startup_diagnostics();

    // Load configuration
    let config = Config::load(&args.config).or_else(|e| {
        tracing::warn!("Failed to load config: {}, using defaults", e);
        Config::default_config()
    })?;

    // Override config with CLI args
    let config = config.with_overrides(args.listen.clone(), args.port);

    info!("Configuration loaded successfully");
    tracing::debug!("Config: {:?}", config);

    info!("Initializing server");
    let server = match LamcoRdpServer::new(config).await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("{}", lamco_rdp_server::utils::format_user_error(&e));
            return Err(e);
        }
    };

    info!("Starting server");
    if let Err(e) = server.run().await {
        eprintln!("{}", lamco_rdp_server::utils::format_user_error(&e));
        return Err(e);
    }

    info!("Server shut down");
    Ok(())
}

/// Show detected capabilities
async fn show_capabilities(format: &str) -> Result<()> {
    // Probe capabilities
    let caps = lamco_rdp_server::compositor::probe_capabilities()
        .await
        .unwrap_or_else(|e| {
            eprintln!("Failed to probe capabilities: {}", e);
            std::process::exit(1);
        });

    // Deployment and credential storage
    let deployment = lamco_rdp_server::session::detect_deployment_context();
    let (storage_method, encryption, accessible) =
        lamco_rdp_server::session::detect_credential_storage(&deployment).await;

    // OS release info
    let os_release = lamco_rdp_server::compositor::detect_os_release();

    // Kernel version
    let kernel_version = std::fs::read_to_string("/proc/version")
        .ok()
        .and_then(|v| v.split_whitespace().nth(2).map(String::from))
        .unwrap_or_else(|| "unknown".to_string());

    if format == "json" {
        output_capabilities_json(
            &caps,
            &deployment,
            &storage_method,
            &encryption,
            accessible,
            &os_release,
            &kernel_version,
        );
    } else {
        output_capabilities_text(&caps, &deployment, &storage_method, &encryption, accessible);
    }

    Ok(())
}

/// Output capabilities in JSON format for GUI integration
fn output_capabilities_json(
    caps: &lamco_rdp_server::compositor::CompositorCapabilities,
    deployment: &lamco_rdp_server::session::DeploymentContext,
    storage_method: &lamco_rdp_server::session::CredentialStorageMethod,
    _encryption: &lamco_rdp_server::session::EncryptionType,
    accessible: bool,
    os_release: &Option<lamco_rdp_server::compositor::OsRelease>,
    kernel_version: &str,
) {
    use serde_json::json;

    // Distribution string
    let distribution = os_release
        .as_ref()
        .map(|os| format!("{} {}", os.pretty_name, os.version_id))
        .unwrap_or_else(|| "Unknown".to_string());

    // Build services array based on capabilities
    let mut services = Vec::new();

    // Screen capture service
    services.push(json!({
        "id": "screen_capture",
        "name": "Screen Capture",
        "level": if caps.portal.supports_screencast { "guaranteed" } else { "unavailable" },
        "wayland_source": format!("ScreenCast Portal v{}", caps.portal.version),
        "rdp_capability": "MS-RDPEGFX",
        "notes": []
    }));

    // Keyboard input
    services.push(json!({
        "id": "keyboard_input",
        "name": "Keyboard Input",
        "level": if caps.portal.supports_remote_desktop { "guaranteed" } else { "unavailable" },
        "wayland_source": format!("RemoteDesktop Portal v{}", caps.portal.version),
        "rdp_capability": "Input PDUs",
        "notes": []
    }));

    // Pointer input
    services.push(json!({
        "id": "pointer_input",
        "name": "Pointer Input",
        "level": if caps.portal.supports_remote_desktop { "guaranteed" } else { "unavailable" },
        "wayland_source": format!("RemoteDesktop Portal v{}", caps.portal.version),
        "rdp_capability": "Input PDUs",
        "notes": []
    }));

    // Clipboard
    let clipboard_level = if caps.portal.supports_clipboard {
        "best_effort"
    } else {
        "unavailable"
    };
    services.push(json!({
        "id": "clipboard",
        "name": "Clipboard Sync",
        "level": clipboard_level,
        "wayland_source": if caps.portal.supports_clipboard { Some("Portal Clipboard") } else { None::<&str> },
        "rdp_capability": "CLIPRDR",
        "notes": if caps.portal.version < 46 { vec!["File transfer requires Portal v46+"] } else { vec![] }
    }));

    // Audio (not yet implemented)
    services.push(json!({
        "id": "audio",
        "name": "Audio Playback",
        "level": "unavailable",
        "wayland_source": null,
        "rdp_capability": "RDPSND",
        "notes": ["Not yet implemented"]
    }));

    // Count service levels
    let guaranteed = services
        .iter()
        .filter(|s| s["level"] == "guaranteed")
        .count();
    let best_effort = services
        .iter()
        .filter(|s| s["level"] == "best_effort")
        .count();
    let degraded = services.iter().filter(|s| s["level"] == "degraded").count();
    let unavailable = services
        .iter()
        .filter(|s| s["level"] == "unavailable")
        .count();

    // Build quirks array
    let quirks: Vec<serde_json::Value> = caps
        .profile
        .quirks
        .iter()
        .map(|q| {
            json!({
                "id": format!("{:?}", q),
                "description": q.description(),
                "impact": "workaround"
            })
        })
        .collect();

    // Determine recommended codec based on capture method
    // Portal capture with DmaBuf support indicates EGFX capability
    let recommended_codec = if matches!(
        caps.profile.recommended_buffer_type,
        lamco_rdp_server::compositor::BufferType::DmaBuf
    ) {
        Some("avc420")
    } else {
        Some("bitmap")
    };

    // Deployment context string
    let (deployment_str, linger) = match deployment {
        lamco_rdp_server::session::DeploymentContext::Native => ("native", None),
        lamco_rdp_server::session::DeploymentContext::Flatpak => ("flatpak", None),
        lamco_rdp_server::session::DeploymentContext::SystemdUser { linger_enabled } => {
            ("systemd-user", Some(*linger_enabled))
        }
        lamco_rdp_server::session::DeploymentContext::SystemdSystem => ("systemd-system", None),
        lamco_rdp_server::session::DeploymentContext::InitD => ("initd", None),
    };

    let json = json!({
        "system": {
            "compositor": caps.compositor.to_string(),
            "compositor_version": caps.compositor.version(),
            "distribution": distribution,
            "kernel": kernel_version
        },
        "portals": {
            "version": caps.portal.version,
            "backend": caps.portal.backend,
            "screencast_version": caps.portal.version,
            "remote_desktop_version": caps.portal.version,
            "secret_version": if accessible { Some(1u32) } else { None::<u32> }
        },
        "deployment": {
            "context": deployment_str,
            "xdg_runtime_dir": std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/run/user/1000".to_string()),
            "linger": linger
        },
        "persistence": {
            "strategy": format!("{}", storage_method),
            "notes": if accessible { vec!["Credential storage accessible"] } else { vec!["Credential storage locked or unavailable"] }
        },
        "quirks": quirks,
        "services": services,
        "summary": {
            "guaranteed": guaranteed,
            "best_effort": best_effort,
            "degraded": degraded,
            "unavailable": unavailable
        },
        "hints": {
            "recommended_fps": 30,
            "recommended_codec": recommended_codec,
            "zero_copy": matches!(caps.profile.recommended_buffer_type, lamco_rdp_server::compositor::BufferType::DmaBuf)
        }
    });

    println!("{}", serde_json::to_string_pretty(&json).unwrap());
}

/// Output capabilities in human-readable text format
fn output_capabilities_text(
    caps: &lamco_rdp_server::compositor::CompositorCapabilities,
    deployment: &lamco_rdp_server::session::DeploymentContext,
    storage_method: &lamco_rdp_server::session::CredentialStorageMethod,
    encryption: &lamco_rdp_server::session::EncryptionType,
    accessible: bool,
) {
    println!("╔════════════════════════════════════════════════════════╗");
    println!("║         Capability Detection Report                    ║");
    println!("╚════════════════════════════════════════════════════════╝");
    println!();

    println!("Compositor: {}", caps.compositor);
    println!(
        "  Version: {}",
        caps.compositor.version().unwrap_or("unknown")
    );
    println!();

    println!("Portal: version {}", caps.portal.version);
    println!(
        "  ScreenCast: {}",
        if caps.portal.supports_screencast {
            "✅"
        } else {
            "❌"
        }
    );
    println!(
        "  RemoteDesktop: {}",
        if caps.portal.supports_remote_desktop {
            "✅"
        } else {
            "❌"
        }
    );
    println!(
        "  Clipboard: {}",
        if caps.portal.supports_clipboard {
            "✅"
        } else {
            "❌"
        }
    );
    println!(
        "  Restore tokens: {}",
        if caps.portal.version >= 4 {
            "✅ Supported"
        } else {
            "❌ Not supported (v < 4)"
        }
    );
    println!();

    println!("Deployment: {}", deployment);
    println!();

    println!("Credential Storage: {}", storage_method);
    println!("  Encryption: {}", encryption);
    println!("  Accessible: {}", if accessible { "✅" } else { "❌" });
    println!();
}

/// Show session persistence status
async fn show_persistence_status() -> Result<()> {
    println!("╔════════════════════════════════════════════════════════╗");
    println!("║         Session Persistence Status                     ║");
    println!("╚════════════════════════════════════════════════════════╝");
    println!();

    let deployment = lamco_rdp_server::session::detect_deployment_context();
    let (storage_method, encryption, accessible) =
        lamco_rdp_server::session::detect_credential_storage(&deployment).await;

    let token_manager = lamco_rdp_server::session::TokenManager::new(storage_method).await?;

    let has_token = token_manager.load_token("default").await?.is_some();

    println!("Deployment: {}", deployment);
    println!("Storage: {} ({})", storage_method, encryption);
    println!(
        "Token Status: {}",
        if has_token {
            "✅ Available"
        } else {
            "❌ Not found"
        }
    );
    println!();

    if has_token {
        println!("✅ Server can start without permission dialog");
    } else {
        println!("⚠️  Server will show permission dialog on next start");
        println!("   Run with --grant-permission to obtain token");
    }

    Ok(())
}

/// Clear all stored tokens
async fn clear_tokens() -> Result<()> {
    println!("Clearing all stored session tokens...");

    let deployment = lamco_rdp_server::session::detect_deployment_context();
    let (storage_method, _, _) =
        lamco_rdp_server::session::detect_credential_storage(&deployment).await;

    let token_manager = lamco_rdp_server::session::TokenManager::new(storage_method).await?;

    token_manager.delete_token("default").await?;

    println!("✅ All tokens cleared");
    println!("   Server will show permission dialog on next start");

    Ok(())
}

/// Grant permission flow (interactive)
async fn grant_permission_flow() -> Result<()> {
    println!("╔════════════════════════════════════════════════════════╗");
    println!("║         Permission Grant Flow                          ║");
    println!("╚════════════════════════════════════════════════════════╝");
    println!();
    println!("This will:");
    println!("  1. Trigger portal permission dialog");
    println!("  2. Obtain restore token after you grant permission");
    println!("  3. Store token securely for future use");
    println!("  4. Exit (server will not start)");
    println!();
    println!("When the dialog appears, click 'Allow' to grant permission.");
    println!();

    // Load config (use defaults if not found)
    let config = Config::default_config()?;

    // Create server (this will trigger permission dialog)
    info!("Creating server to obtain permission...");
    let _server = LamcoRdpServer::new(config).await?;

    println!();
    println!("✅ Permission granted and token stored!");
    println!("   Server can now start unattended via:");
    println!("   • systemctl --user start lamco-rdp-server");
    println!("   • Or just: lamco-rdp-server");

    Ok(())
}

/// Run diagnostic checks
async fn run_diagnostics() -> Result<()> {
    println!("╔════════════════════════════════════════════════════════╗");
    println!("║         Diagnostic Report                              ║");
    println!("╚════════════════════════════════════════════════════════╝");
    println!();

    // Test 1: Wayland session
    print!("[  ] Wayland session... ");
    if std::env::var("WAYLAND_DISPLAY").is_ok() {
        println!("✅");
    } else {
        println!("❌ Not in Wayland session");
    }

    // Test 2: D-Bus session
    print!("[  ] D-Bus session bus... ");
    match zbus::Connection::session().await {
        Ok(_) => println!("✅"),
        Err(e) => println!("❌ {}", e),
    }

    // Test 3: Compositor detection
    print!("[  ] Compositor identification... ");
    let compositor = lamco_rdp_server::compositor::identify_compositor();
    if matches!(
        compositor,
        lamco_rdp_server::compositor::CompositorType::Unknown { .. }
    ) {
        println!("⚠️  Unknown (using generic support)");
    } else {
        println!("✅ {}", compositor);
    }

    // Test 4: Portal connection
    print!("[  ] Portal connection... ");
    match lamco_rdp_server::compositor::probe_capabilities().await {
        Ok(caps) => {
            if caps.portal.supports_screencast && caps.portal.supports_remote_desktop {
                println!("✅ v{}", caps.portal.version);
            } else {
                println!("⚠️  Partial support");
            }
        }
        Err(e) => println!("❌ {}", e),
    }

    // Test 5: Deployment detection
    print!("[  ] Deployment context... ");
    let deployment = lamco_rdp_server::session::detect_deployment_context();
    println!("✅ {}", deployment);

    // Test 6: Credential storage
    print!("[  ] Credential storage... ");
    let (method, encryption, accessible) =
        lamco_rdp_server::session::detect_credential_storage(&deployment).await;
    if accessible {
        println!("✅ {} ({})", method, encryption);
    } else {
        println!("⚠️  {} (locked)", method);
    }

    // Test 7: Token availability
    print!("[  ] Restore token... ");
    let token_manager = lamco_rdp_server::session::TokenManager::new(method).await?;
    if token_manager.load_token("default").await?.is_some() {
        println!("✅ Available");
    } else {
        println!("❌ Not found");
    }

    // Test 8: machine-id
    print!("[  ] Machine ID... ");
    if std::path::Path::new("/etc/machine-id").exists() {
        println!("✅ Available");
    } else if std::path::Path::new("/var/lib/dbus/machine-id").exists() {
        println!("✅ Available (fallback location)");
    } else {
        println!("⚠️  Not found (will use hostname)");
    }

    println!();
    println!("SUMMARY:");
    println!("  Run --show-capabilities for detailed capability report");
    println!("  Run --persistence-status for session persistence details");

    Ok(())
}

fn init_logging(args: &Args) -> Result<()> {
    use std::fs::File;

    let log_level = match args.verbose {
        0 => "info",
        1 => "debug",
        _ => "trace",
    };

    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        // Enable lamco crates at requested level, IronRDP protocol at info (debug logs raw packets!)
        // ironrdp_cliprdr/egfx/dvc at debug for channel troubleshooting
        tracing_subscriber::EnvFilter::new(format!(
            "lamco={level},ironrdp_cliprdr={level},ironrdp_egfx={level},ironrdp_dvc={level},ironrdp_server={level},ironrdp=info,ashpd=info,warn",
            level = log_level
        ))
    });

    // If log file is specified, write to both stdout and file
    if let Some(log_file_path) = &args.log_file {
        let file = File::create(log_file_path)?;

        match args.log_format.as_str() {
            "json" => {
                tracing_subscriber::registry()
                    .with(env_filter)
                    .with(
                        tracing_subscriber::fmt::layer()
                            .json()
                            .with_writer(std::io::stdout),
                    )
                    .with(
                        tracing_subscriber::fmt::layer()
                            .json()
                            .with_writer(file)
                            .with_ansi(false),
                    )
                    .init();
            }
            "compact" => {
                tracing_subscriber::registry()
                    .with(env_filter)
                    .with(
                        tracing_subscriber::fmt::layer()
                            .compact()
                            .with_writer(std::io::stdout),
                    )
                    .with(
                        tracing_subscriber::fmt::layer()
                            .compact()
                            .with_writer(file)
                            .with_ansi(false),
                    )
                    .init();
            }
            _ => {
                tracing_subscriber::registry()
                    .with(env_filter)
                    .with(
                        tracing_subscriber::fmt::layer()
                            .pretty()
                            .with_writer(std::io::stdout),
                    )
                    .with(
                        tracing_subscriber::fmt::layer()
                            .with_writer(file)
                            .with_ansi(false),
                    )
                    .init();
            }
        }
        info!("Logging to file: {}", log_file_path);
    } else {
        // Stdout only
        match args.log_format.as_str() {
            "json" => {
                tracing_subscriber::registry()
                    .with(env_filter)
                    .with(tracing_subscriber::fmt::layer().json())
                    .init();
            }
            "compact" => {
                tracing_subscriber::registry()
                    .with(env_filter)
                    .with(tracing_subscriber::fmt::layer().compact())
                    .init();
            }
            _ => {
                tracing_subscriber::registry()
                    .with(env_filter)
                    .with(tracing_subscriber::fmt::layer().pretty())
                    .init();
            }
        }
    }

    Ok(())
}
