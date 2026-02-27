//! lamco-rdp-server - Wayland Remote Desktop Server
//!
//! Entry point for the server binary.

use anyhow::Result;
use clap::Parser;
use lamco_rdp_server::{config::Config, server::LamcoRdpServer};
use tracing::{error, info, warn};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

/// Command-line arguments for lamco-rdp-server
#[derive(Parser, Debug)]
#[command(name = "lamco-rdp-server")]
#[command(version, about = "Wayland Remote Desktop Server", long_about = None)]
pub struct Args {
    /// Configuration file path
    #[arg(short, long)]
    pub config: Option<String>,

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

    /// Run as D-Bus service
    ///
    /// Registers the management interface on the session bus (or system bus
    /// with --system). The GUI and other tools can connect to manage the
    /// server remotely. Enables:
    /// - Status queries via D-Bus properties
    /// - Configuration management via D-Bus methods
    /// - Real-time notifications via D-Bus signals
    #[arg(long)]
    pub dbus_service: bool,

    /// Use system bus instead of session bus (requires root or polkit)
    ///
    /// Only valid with --dbus-service. For system-level services managed
    /// by systemd as a system unit.
    #[arg(long)]
    pub system: bool,
}

#[tokio::main]
#[expect(
    clippy::expect_used,
    reason = "top-level entry point: panics on infallible config defaults"
)]
async fn main() -> Result<()> {
    let args = Args::parse();

    // Resolve config path: CLI flag, then Flatpak-aware default, then /etc fallback
    let config_path = args.config.clone().unwrap_or_else(|| {
        let dir = lamco_rdp_server::config::get_cert_config_dir();
        let candidate = dir.join("config.toml");
        if candidate.exists() {
            candidate.display().to_string()
        } else {
            "/etc/lamco-rdp-server/config.toml".to_string()
        }
    });

    // Load configuration first (needed for logging settings)
    // Silently fall back to defaults if config doesn't exist yet
    let config = Config::load(&config_path)
        .unwrap_or_else(|_| Config::default_config().expect("Default config should always work"));

    // Initialize logging (uses config.logging, CLI args override)
    init_logging(&args, &config.logging)?;

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

    lamco_rdp_server::runtime::log_startup_diagnostics();

    // Apply CLI overrides to config (config already loaded above for logging)
    let config = config.with_overrides(args.listen.clone(), args.port);

    info!("Configuration loaded successfully");
    tracing::debug!("Config: {:?}", config);

    let _dbus_connection = if args.dbus_service {
        info!("Starting D-Bus management interface");
        let state = lamco_rdp_server::dbus::new_shared_state();

        {
            let mut s = state.write().await;
            s.config_path.clone_from(&config_path);
        }

        match lamco_rdp_server::dbus::start_service(args.system, state).await {
            Ok(conn) => {
                info!(
                    "D-Bus service registered: {}",
                    if args.system {
                        lamco_rdp_server::dbus::SYSTEM_SERVICE_NAME
                    } else {
                        lamco_rdp_server::dbus::SERVICE_NAME
                    }
                );
                Some(conn)
            }
            Err(e) => {
                tracing::error!("Failed to start D-Bus service: {}", e);
                return Err(anyhow::anyhow!("D-Bus service failed: {e}"));
            }
        }
    } else {
        None
    };

    info!("Initializing server");
    let mut server = match LamcoRdpServer::new(config).await {
        Ok(s) => s,
        Err(e) => {
            error!("Server initialization failed: {e:#}");
            eprintln!("{}", lamco_rdp_server::runtime::format_user_error(&e));
            return Err(e);
        }
    };

    // Wire D-Bus signal relay if D-Bus service is active
    if let Some(ref dbus_conn) = _dbus_connection {
        if let Some(event_rx) = server.take_event_receiver() {
            let dbus_state = lamco_rdp_server::dbus::new_shared_state();
            let _relay_handle = lamco_rdp_server::dbus::events::start_signal_relay(
                dbus_conn.clone(),
                event_rx,
                dbus_state,
            );
            info!("D-Bus signal relay started");
        }
    }

    info!("Starting server");

    // Get shutdown channels BEFORE run() consumes the server.
    // Quit event closes the active RDP connection gracefully (TLS CloseNotify).
    // Broadcast breaks the outer accept loop and stops clipboard/PipeWire tasks.
    let shutdown_sender = server.shutdown_sender();
    let shutdown_broadcast = server.shutdown_broadcast();

    tokio::spawn(async move {
        if let Ok(()) = tokio::signal::ctrl_c().await {
            warn!("═══════════════════════════════════════════════════════════");
            warn!("  Ctrl-C received - Initiating graceful shutdown");
            warn!("═══════════════════════════════════════════════════════════");
            warn!("Shutdown sequence:");
            warn!("  1. Stop accepting new connections");
            warn!("  2. Shutdown clipboard manager");
            warn!("  3. Shutdown PipeWire thread");
            warn!("  4. Close Portal session");
            warn!("═══════════════════════════════════════════════════════════");
            let _ = shutdown_sender.send(ironrdp_server::ServerEvent::Quit(
                "Ctrl-C received".to_string(),
            ));
            let _ = shutdown_broadcast.send(());
        }
    });

    if let Err(e) = server.run().await {
        error!("Server exited with error: {e:#}");
        eprintln!("{}", lamco_rdp_server::runtime::format_user_error(&e));
        return Err(e);
    }

    info!("Server shut down");
    Ok(())
}

/// Show detected capabilities
async fn show_capabilities(format: &str) -> Result<()> {
    let caps = lamco_rdp_server::compositor::probe_capabilities()
        .await
        .unwrap_or_else(|e| {
            eprintln!("Failed to probe capabilities: {e}");
            std::process::exit(1);
        });

    let deployment = lamco_rdp_server::session::detect_deployment_context();
    let (storage_method, encryption, accessible) =
        lamco_rdp_server::session::detect_credential_storage(&deployment).await;

    let os_release = lamco_rdp_server::compositor::detect_os_release();

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
            os_release.as_ref(),
            &kernel_version,
        );
    } else {
        output_capabilities_text(&caps, &deployment, &storage_method, &encryption, accessible);
    }

    Ok(())
}

/// Output capabilities in JSON format for GUI integration
#[expect(
    clippy::expect_used,
    reason = "JSON serialization of known-valid struct"
)]
fn output_capabilities_json(
    caps: &lamco_rdp_server::compositor::CompositorCapabilities,
    deployment: &lamco_rdp_server::session::DeploymentContext,
    storage_method: &lamco_rdp_server::session::CredentialStorageMethod,
    _encryption: &lamco_rdp_server::session::EncryptionType,
    accessible: bool,
    os_release: Option<&lamco_rdp_server::compositor::OsRelease>,
    kernel_version: &str,
) {
    use serde_json::json;

    let distribution = os_release.as_ref().map_or_else(
        || "Unknown".to_string(),
        |os| format!("{} {}", os.pretty_name, os.version_id),
    );

    let mut services = Vec::new();

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

    services.push(json!({
        "id": "audio",
        "name": "Audio Playback",
        "level": "unavailable",
        "wayland_source": null,
        "rdp_capability": "RDPSND",
        "notes": ["Not yet implemented"]
    }));

    // PAM Authentication - unavailable in Flatpak sandbox
    let pam_available = !matches!(
        deployment,
        lamco_rdp_server::session::DeploymentContext::Flatpak
    );
    services.push(json!({
        "id": "PamAuthentication",
        "name": "PAM Authentication",
        "level": if pam_available { "guaranteed" } else { "unavailable" },
        "wayland_source": null,
        "rdp_capability": "NLA",
        "notes": if pam_available {
            vec![]
        } else {
            vec!["PAM blocked by Flatpak sandbox"]
        }
    }));

    // No Authentication - always available
    services.push(json!({
        "id": "NoAuthentication",
        "name": "No Authentication",
        "level": "guaranteed",
        "wayland_source": null,
        "rdp_capability": null,
        "notes": ["Always available, no password required"]
    }));

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

    println!(
        "{}",
        serde_json::to_string_pretty(&json).expect("JSON serialization of capabilities")
    );
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

    println!("Deployment: {deployment}");
    println!();

    println!("Credential Storage: {storage_method}");
    println!("  Encryption: {encryption}");
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
    let (storage_method, encryption, _accessible) =
        lamco_rdp_server::session::detect_credential_storage(&deployment).await;

    let token_manager = lamco_rdp_server::session::Tokens::new(storage_method).await?;

    let has_token = token_manager.load_token("default").await?.is_some();

    println!("Deployment: {deployment}");
    println!("Storage: {storage_method} ({encryption})");
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

    let token_manager = lamco_rdp_server::session::Tokens::new(storage_method).await?;

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

    let config = Config::default_config()?;

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
        Err(e) => println!("❌ {e}"),
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
        println!("✅ {compositor}");
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
        Err(e) => println!("❌ {e}"),
    }

    // Test 5: Deployment detection
    print!("[  ] Deployment context... ");
    let deployment = lamco_rdp_server::session::detect_deployment_context();
    println!("✅ {deployment}");

    // Test 6: Credential storage
    print!("[  ] Credential storage... ");
    let (method, encryption, accessible) =
        lamco_rdp_server::session::detect_credential_storage(&deployment).await;
    if accessible {
        println!("✅ {method} ({encryption})");
    } else {
        println!("⚠️  {method} (locked)");
    }

    // Test 7: Token availability
    print!("[  ] Restore token... ");
    let token_manager = lamco_rdp_server::session::Tokens::new(method).await?;
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

fn init_logging(
    args: &Args,
    logging_config: &lamco_rdp_server::config::types::LoggingConfig,
) -> Result<()> {
    use std::fs::{self, File};

    // CLI -v flag overrides config
    let log_level = if args.verbose > 0 {
        match args.verbose {
            1 => "debug",
            _ => "trace",
        }
    } else {
        match logging_config.level.as_str() {
            "trace" | "debug" | "info" | "warn" | "error" => logging_config.level.as_str(),
            _ => "info", // Invalid value, fallback to info
        }
    };

    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        // Logging levels by crate:
        // - lamco_* crates: User-controlled via -v flag or config
        // - ironrdp_cliprdr/egfx/dvc/server: Same as lamco (channel troubleshooting)
        // - ironrdp (main): Forced to info (debug logs raw packets - very verbose!)
        // - ashpd: Same as lamco for portal debugging
        // - zbus: info level for D-Bus troubleshooting without flooding
        // - Everything else: warn
        tracing_subscriber::EnvFilter::new(format!(
            "lamco={log_level},lamco_portal={log_level},lamco_rdp={log_level},lamco_video={log_level},\
             ironrdp_cliprdr={log_level},ironrdp_egfx={log_level},ironrdp_dvc={log_level},ironrdp_server={log_level},\
             ironrdp=info,ashpd={log_level},zbus=info,warn"
        ))
    });

    // CLI --log-file overrides config.log_dir.
    // In Flatpak, always log to the sandbox data directory even if log_dir isn't
    // explicitly configured — the GUI shows this path as the fixed log location.
    let log_file_path: Option<String> = if let Some(cli_path) = &args.log_file {
        Some(cli_path.clone())
    } else {
        let resolved = if logging_config.log_dir.is_some() || lamco_rdp_server::config::is_flatpak()
        {
            Some(lamco_rdp_server::config::resolve_log_dir(
                &logging_config.log_dir,
            ))
        } else {
            None
        };

        if let Some(log_dir) = resolved {
            if let Err(e) = fs::create_dir_all(&log_dir) {
                eprintln!(
                    "Warning: Cannot create log directory {}: {e}",
                    log_dir.display()
                );
                None
            } else {
                let timestamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
                Some(
                    log_dir
                        .join(format!("lamco-rdp-server-{timestamp}.log"))
                        .display()
                        .to_string(),
                )
            }
        } else {
            None
        }
    };

    // If log file is specified, write to both stdout and file
    // Gracefully fall back to stdout-only if file creation fails (e.g. read-only filesystem in Flatpak)
    let log_file = log_file_path
        .as_ref()
        .and_then(|path| match File::create(path) {
            Ok(f) => Some((f, path.clone())),
            Err(e) => {
                eprintln!(
                    "Warning: Cannot create log file {path:?}: {e} — logging to console only"
                );
                None
            }
        });

    if let Some((file, ref log_file_path)) = log_file {
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
