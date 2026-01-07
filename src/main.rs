//! WRD-Server - Wayland Remote Desktop Server
//!
//! Entry point for the server binary.

use anyhow::Result;
use clap::Parser;
use tracing::{debug, info};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

use lamco_rdp_server::config::Config;
use lamco_rdp_server::server::WrdServer;

/// Command-line arguments for wrd-server
#[derive(Parser, Debug)]
#[command(name = "wrd-server")]
#[command(version, about = "Wayland Remote Desktop Server", long_about = None)]
pub struct Args {
    /// Configuration file path
    #[arg(short, long, default_value = "/etc/wrd-server/config.toml")]
    pub config: String,

    /// Listen address
    #[arg(short, long, env = "WRD_LISTEN_ADDR")]
    pub listen: Option<String>,

    /// Listen port
    #[arg(short, long, env = "WRD_PORT", default_value = "3389")]
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

    info!("lamco-rdp-server v{}", env!("CARGO_PKG_VERSION"));
    debug!(
        "Build: {} {} ({}) commit {}",
        env!("BUILD_DATE"),
        env!("BUILD_TIME"),
        if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        },
        env!("GIT_HASH")
    );

    // Handle diagnostic commands (exit after running)
    if args.show_capabilities {
        return show_capabilities().await;
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
    lamco_rdp_server::telemetry::log_startup_diagnostics();

    // Load configuration
    let config = Config::load(&args.config).or_else(|e| {
        tracing::warn!("Failed to load config: {}, using defaults", e);
        Config::default_config()
    })?;

    // Override config with CLI args
    let config = config.with_overrides(args.listen.clone(), args.port);

    info!("Configuration loaded successfully");
    tracing::debug!("Config: {:?}", config);

    // Create and start WRD server
    info!("Initializing WRD Server");
    let server = match WrdServer::new(config).await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("{}", lamco_rdp_server::telemetry::format_user_error(&e));
            return Err(e);
        }
    };

    info!("Starting WRD Server");
    if let Err(e) = server.run().await {
        eprintln!("{}", lamco_rdp_server::telemetry::format_user_error(&e));
        return Err(e);
    }

    info!("WRD Server shut down");
    Ok(())
}

/// Show detected capabilities
async fn show_capabilities() -> Result<()> {
    println!("╔════════════════════════════════════════════════════════╗");
    println!("║         Capability Detection Report                    ║");
    println!("╚════════════════════════════════════════════════════════╝");
    println!();

    // Probe capabilities
    let caps = lamco_rdp_server::compositor::probe_capabilities()
        .await
        .unwrap_or_else(|e| {
            eprintln!("Failed to probe capabilities: {}", e);
            std::process::exit(1);
        });

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

    // Deployment detection
    let deployment = lamco_rdp_server::session::detect_deployment_context();
    println!("Deployment: {}", deployment);
    println!();

    // Credential storage
    let (storage_method, encryption, accessible) =
        lamco_rdp_server::session::detect_credential_storage(&deployment).await;
    println!("Credential Storage: {}", storage_method);
    println!("  Encryption: {}", encryption);
    println!("  Accessible: {}", if accessible { "✅" } else { "❌" });
    println!();

    Ok(())
}

/// Show session persistence status
async fn show_persistence_status() -> Result<()> {
    println!("╔════════════════════════════════════════════════════════╗");
    println!("║         Session Persistence Status                     ║");
    println!("╚════════════════════════════════════════════════════════╝");
    println!();

    let deployment = lamco_rdp_server::session::detect_deployment_context();
    let (storage_method, encryption, _accessible) =
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
    let _server = WrdServer::new(config).await?;

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
