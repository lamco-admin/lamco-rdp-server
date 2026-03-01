//! Server Implementation Module
//!
//! This module provides the main server implementation, orchestrating all subsystems
//! to provide complete RDP server functionality for Wayland desktops.
//!
//! # Architecture
//!
//! The server integrates multiple subsystems:
//!
//! ```text
//! WrdServer
//!   ├─> Portal Session (screen capture + input injection permissions)
//!   ├─> PipeWire Thread Manager (video frame capture)
//!   ├─> Display Handler (video streaming to RDP clients)
//!   ├─> Input Handler (keyboard/mouse from RDP clients)
//!   ├─> Clipboard Manager (bidirectional clipboard sync)
//!   └─> IronRDP Server (RDP protocol, TLS, RemoteFX encoding)
//! ```
//!
//! # Data Flow
//!
//! **Video Path:** Portal → PipeWire → Display Handler → IronRDP → Client
//!
//! **Input Path:** Client → IronRDP → Input Handler → Portal → Compositor
//!
//! **Clipboard Path:** Client ↔ IronRDP ↔ Clipboard Manager ↔ Portal ↔ Compositor
//!
//! # Threading Model
//!
//! - **Tokio async runtime:** Main server logic, Portal API calls, frame processing
//! - **PipeWire thread:** Dedicated thread for PipeWire MainLoop (handles non-Send types)
//! - **IronRDP threads:** Managed by IronRDP library for protocol handling
//!
//! # Example
//!
//! ```ignore
//! use lamco_rdp_server::config::Config;
//! use lamco_rdp_server::server::WrdServer;
//!
//! #[tokio::main]
//! async fn main() -> anyhow::Result<()> {
//!     let config = Config::load("config.toml")?;
//!     let server = WrdServer::new(config).await?;
//!     server.run().await?;
//!     Ok(())
//! }
//! ```
//!
//! # Security
//!
//! - TLS 1.3 mandatory for all connections
//! - Certificate-based authentication
//! - Portal-based authorization (user approves screen sharing)
//! - No direct Wayland protocol access
//!
//! # Performance
//!
//! - Target: <100ms end-to-end latency
//! - Target: 30-60 FPS video streaming
//! - RemoteFX compression for efficient bandwidth usage

#![expect(
    unsafe_code,
    reason = "OwnedFd::from_raw_fd() to take ownership of portal-provided PipeWire file descriptor"
)]

mod display_handler;
mod egfx_sender;
#[expect(dead_code, reason = "WIP: not yet integrated into the server pipeline")]
mod event_multiplexer;
mod gfx_factory;
mod graphics_drain;
mod input_handler;
#[expect(dead_code, reason = "WIP: not yet integrated into the server pipeline")]
mod multiplexer_loop;

use std::{net::SocketAddr, sync::Arc};

use anyhow::{Context, Result};
pub use display_handler::LamcoDisplayHandler;
pub use egfx_sender::{EgfxFrameSender, SendError};
pub use gfx_factory::{HandlerState, LamcoGfxFactory, SharedHandlerState};
pub use input_handler::LamcoInputHandler;
use ironrdp_graphics::zgfx::CompressionMode;
use ironrdp_pdu::rdp::capability_sets::server_codecs_capabilities;
use ironrdp_server::{Credentials, RdpServer};
use tokio::sync::{Mutex, RwLock};
use tracing::{debug, error, info, warn};

use crate::{
    audio::factory::create_sound_factory,
    clipboard::{ClipboardOrchestrator, ClipboardOrchestratorConfig, LamcoCliprdrFactory},
    config::{is_flatpak, Config},
    dbus::events::{self, ServerEvent},
    health::{HealthSubscriber, SessionHealthMonitor},
    input::MonitorInfo as InputMonitorInfo,
    portal::PortalManager,
    security::TlsConfig,
    services::{ServiceId, ServiceLevel, ServiceRegistry},
    session::{PipeWireAccess, SessionStrategySelector, SessionType},
};

/// WRD Server
///
/// Main server struct that orchestrates all subsystems and integrates
/// with IronRDP for RDP protocol handling.
pub struct LamcoRdpServer {
    /// Configuration (kept for future dynamic reconfiguration)
    config: Arc<Config>,

    /// IronRDP server instance
    rdp_server: RdpServer,

    /// Portal manager for Wayland access (kept for resource cleanup).
    /// None in ScreenCast-only (view-only) mode where no RemoteDesktop session exists.
    #[expect(
        dead_code,
        reason = "Arc kept alive for portal resource cleanup on drop"
    )]
    portal_manager: Option<Arc<PortalManager>>,

    /// Display handler (kept for lifecycle management)
    display_handler: Arc<LamcoDisplayHandler>,

    /// Service registry for capability/feature decisions
    service_registry: Arc<ServiceRegistry>,

    /// Clipboard manager (for cleanup on shutdown)
    clipboard_manager: Option<Arc<tokio::sync::Mutex<ClipboardOrchestrator>>>,

    /// Portal session for RemoteDesktop (for explicit close on shutdown)
    portal_session: Option<
        Arc<
            tokio::sync::RwLock<
                ashpd::desktop::Session<
                    'static,
                    ashpd::desktop::remote_desktop::RemoteDesktop<'static>,
                >,
            >,
        >,
    >,

    /// Shutdown broadcast for coordinating async task shutdown
    shutdown_broadcast: Arc<tokio::sync::broadcast::Sender<()>>,

    /// Server event channel sender for D-Bus signal emission
    event_tx: tokio::sync::mpsc::UnboundedSender<ServerEvent>,

    /// Server event channel receiver (taken by caller to wire D-Bus relay)
    event_rx: Option<tokio::sync::mpsc::UnboundedReceiver<ServerEvent>>,

    /// Session health subscriber (for health-aware decisions)
    health_subscriber: Option<HealthSubscriber>,

    /// Health monitor task handle
    #[expect(dead_code, reason = "Kept alive to run monitor background task")]
    health_monitor_handle: Option<tokio::task::JoinHandle<()>>,

    /// Prevents double cleanup (run() path + Drop safety net)
    cleanup_done: bool,
}

impl LamcoRdpServer {
    pub async fn new(config: Config) -> Result<Self> {
        info!("Initializing server");
        let config = Arc::new(config);

        info!("Probing compositor capabilities...");
        let capabilities = crate::compositor::probe_capabilities()
            .await
            .context("Failed to probe compositor capabilities")?;

        for quirk in &capabilities.profile.quirks {
            match quirk {
                crate::compositor::Quirk::RequiresWaylandSession => {
                    if !crate::compositor::is_wayland_session() {
                        warn!("⚠️  Not in Wayland session - may have issues");
                    }
                }
                crate::compositor::Quirk::SlowPortalPermissions => {
                    info!(
                        "📋 Slow portal permissions detected ({}ms timeout configured)",
                        capabilities.profile.portal_timeout_ms
                    );
                    // TODO: portal_timeout_ms not yet applied to Portal API calls
                }
                crate::compositor::Quirk::PoorDmaBufSupport => {
                    info!("📋 DMA-BUF support may be limited, using MemFd fallback");
                }
                crate::compositor::Quirk::NeedsExplicitCursorComposite => {
                    info!("📋 Cursor compositing may be needed (no metadata cursor)");
                }
                crate::compositor::Quirk::RestartCaptureOnResize => {
                    info!("📋 Capture will restart on resolution changes");
                }
                crate::compositor::Quirk::MultiMonitorPositionQuirk => {
                    info!("📋 Multi-monitor positions may need adjustment");
                }
                crate::compositor::Quirk::ForceAvc420 => {
                    info!("📋 AVC444 disabled (older driver stack, dual-stream too expensive)");
                }
                crate::compositor::Quirk::ClipboardUnavailable => {
                    info!("📋 Clipboard sync unavailable (Portal v1 limitation)");
                }
                _ => {
                    debug!("Applying quirk: {:?}", quirk);
                }
            }
        }

        info!(
            "✅ Compositor detection complete: {} (profile: {:?} capture, {:?} buffers)",
            capabilities.compositor,
            capabilities.profile.recommended_capture,
            capabilities.profile.recommended_buffer_type
        );

        info!("Detecting deployment context and credential storage...");

        let deployment = crate::session::detect_deployment_context();
        info!("📦 Deployment: {}", deployment);

        let (storage_method, encryption, accessible) =
            crate::session::detect_credential_storage(&deployment).await;
        info!(
            "🔐 Credential Storage: {} (encryption: {}, accessible: {})",
            storage_method, encryption, accessible
        );

        let token_manager = crate::session::Tokens::new(storage_method)
            .await
            .context("Failed to create Tokens")?;

        let restore_token = token_manager
            .load_token("default")
            .await
            .context("Failed to load restore token")?;

        if let Some(ref token) = restore_token {
            info!("🎫 Loaded existing restore token ({} chars)", token.len());
            info!("   Will attempt to restore session without permission dialog");
        } else {
            info!("🎫 No existing restore token found");
            info!("   Permission dialog will appear (one-time grant)");
        }

        let service_registry = Arc::new(ServiceRegistry::from_compositor(capabilities.clone()));
        service_registry.log_summary();

        let pam_level = service_registry.pam_auth_level();
        if pam_level >= ServiceLevel::BestEffort {
            info!("🔐 Authentication: PAM available ({:?})", pam_level);
        } else {
            info!("🔐 Authentication: PAM unavailable (sandboxed environment)");
            info!(
                "   Available methods: {:?}",
                service_registry.available_auth_methods()
            );
            info!(
                "   Recommended: {}",
                service_registry.recommended_auth_method()
            );
        }

        let damage_level = service_registry.service_level(ServiceId::DamageTracking);
        let cursor_level = service_registry.service_level(ServiceId::MetadataCursor);
        let dmabuf_level = service_registry.service_level(ServiceId::DmaBufZeroCopy);

        info!("🎛️ Service-based feature configuration:");
        if damage_level >= ServiceLevel::BestEffort {
            info!(
                "   ✅ Damage tracking: {} - enabling adaptive FPS",
                damage_level
            );
        } else {
            info!(
                "   ⚠️ Damage tracking: {} - using frame diff fallback",
                damage_level
            );
        }

        if cursor_level >= ServiceLevel::BestEffort {
            info!(
                "   ✅ Metadata cursor: {} - client-side rendering",
                cursor_level
            );
        } else {
            info!(
                "   ⚠️ Metadata cursor: {} - painted cursor mode",
                cursor_level
            );
        }

        if dmabuf_level >= ServiceLevel::Guaranteed {
            info!("   ✅ DMA-BUF zero-copy: {} - optimal path", dmabuf_level);
        } else {
            info!("   ⚠️ DMA-BUF: {} - using memory copy path", dmabuf_level);
        }

        // Shared infrastructure created before session — used by all code paths
        let (shutdown_broadcast, _) = tokio::sync::broadcast::channel(16);
        let shutdown_broadcast = Arc::new(shutdown_broadcast);

        // Health monitor must exist before session creation so the reporter
        // can be wired into session handles for proactive death detection
        let (health_monitor, health_reporter, health_subscriber) =
            SessionHealthMonitor::new(shutdown_broadcast.subscribe());
        let health_monitor_handle = tokio::spawn(health_monitor.run());

        let (event_tx, event_rx) = events::event_channel();

        // Bridge health state changes to D-Bus signals so external consumers
        // (GUI, systemd, monitoring) see health transitions in real time
        let _health_bridge_handle = crate::health::start_health_dbus_bridge(
            health_subscriber.clone(),
            event_tx.clone(),
            shutdown_broadcast.subscribe(),
        );

        // View-only mode: bypass strategy selector and use ScreenCast-only directly
        let strategy: Box<dyn crate::session::SessionStrategy> = if config.server.view_only {
            info!("View-only mode requested via configuration");
            Box::new(
                crate::session::strategies::ScreenCastOnlyStrategy::with_cursor_modes(
                    capabilities.portal.available_cursor_modes.clone(),
                ),
            )
        } else {
            info!("Selecting session strategy based on detected capabilities");

            let strategy_selector = SessionStrategySelector::with_keyboard_layout(
                service_registry.clone(),
                Arc::new(token_manager),
                config.input.keyboard_layout.clone(),
            );

            strategy_selector
                .select_strategy()
                .await
                .context("Failed to select session strategy")?
        };

        info!("🎯 Selected strategy: {}", strategy.name());

        info!("Creating session via selected strategy");
        let session_handle: Arc<dyn crate::session::strategy::SessionHandle> =
            match strategy.create_session().await {
                Ok(handle) => handle,
                Err(primary_err) => {
                    warn!(
                        "Primary strategy '{}' failed: {:#}",
                        strategy.name(),
                        primary_err
                    );
                    warn!("Attempting ScreenCast-only fallback (view-only mode)");

                    use crate::session::{
                        strategies::ScreenCastOnlyStrategy, strategy::SessionStrategy as _,
                    };
                    if ScreenCastOnlyStrategy::is_available().await {
                        let fallback = ScreenCastOnlyStrategy::with_cursor_modes(
                            capabilities.portal.available_cursor_modes.clone(),
                        );
                        fallback
                            .create_session()
                            .await
                            .context("ScreenCast-only fallback also failed")?
                    } else {
                        return Err(primary_err)
                            .context("Primary strategy failed and ScreenCast-only unavailable");
                    }
                }
            };

        // Wire health reporter so session handles report lifecycle events
        session_handle.set_health_reporter(health_reporter.clone());

        // Watch for compositor D-Bus name disappearance (crash/restart detection)
        let _compositor_watcher = crate::health::compositor_watcher::start_compositor_watcher(
            session_handle.session_type(),
            health_reporter.clone(),
            shutdown_broadcast.subscribe(),
        )
        .await;

        info!("✅ Session created successfully via {}", strategy.name());

        // wlr-direct: input-only strategy, acquire video via standalone Portal ScreenCast
        let (pipewire_fd, stream_info) = if session_handle.session_type() == SessionType::WlrDirect
        {
            info!("wlr-direct: acquiring video via standalone Portal ScreenCast");

            use ashpd::desktop::{
                screencast::{CursorMode, Screencast, SourceType as ScSourceType},
                PersistMode,
            };

            let screencast = Screencast::new()
                .await
                .context("Failed to connect to ScreenCast portal for wlr-direct video")?;

            let sc_session = screencast
                .create_session()
                .await
                .context("Failed to create ScreenCast session for wlr-direct video")?;

            // Pick best available cursor mode from what the portal actually supports.
            // Hyprland's portal only offers Hidden+Embedded (no Metadata).
            let cursor_mode = if capabilities
                .portal
                .available_cursor_modes
                .contains(&crate::compositor::CursorMode::Metadata)
            {
                CursorMode::Metadata
            } else if capabilities
                .portal
                .available_cursor_modes
                .contains(&crate::compositor::CursorMode::Embedded)
            {
                CursorMode::Embedded
            } else {
                CursorMode::Hidden
            };
            debug!(
                "wlr-direct: using cursor mode {:?} for ScreenCast",
                cursor_mode
            );

            screencast
                .select_sources(
                    &sc_session,
                    cursor_mode,
                    ScSourceType::Monitor.into(),
                    false,
                    None,
                    PersistMode::DoNot,
                )
                .await
                .context("Failed to select ScreenCast sources for wlr-direct video")?;

            let response = screencast
                .start(&sc_session, None)
                .await
                .context("Failed to start ScreenCast for wlr-direct video")?
                .response()
                .context("ScreenCast start rejected by user (wlr-direct video)")?;

            let portal_streams = response.streams();
            if portal_streams.is_empty() {
                return Err(anyhow::anyhow!(
                    "No streams from ScreenCast for wlr-direct video"
                ));
            }

            let streams: Vec<crate::portal::StreamInfo> = portal_streams
                .iter()
                .map(|s| {
                    let (width, height) = s.size().unwrap_or((0, 0));
                    let (x, y) = s.position().unwrap_or((0, 0));
                    crate::portal::StreamInfo {
                        node_id: s.pipe_wire_node_id(),
                        position: (x, y),
                        size: (width as u32, height as u32),
                        source_type: crate::portal::SourceType::Monitor,
                    }
                })
                .collect();

            info!(
                "wlr-direct: ScreenCast started with {} stream(s)",
                streams.len()
            );
            for stream in &streams {
                info!(
                    "  Stream: node_id={}, {}x{} at ({},{})",
                    stream.node_id,
                    stream.size.0,
                    stream.size.1,
                    stream.position.0,
                    stream.position.1
                );
            }

            let fd = screencast
                .open_pipe_wire_remote(&sc_session)
                .await
                .context("Failed to open PipeWire remote for wlr-direct video")?;

            use std::os::fd::AsRawFd;
            let raw_fd = fd.as_raw_fd();
            // Leak the OwnedFd so the PipeWire connection stays alive for the session.
            // Cleaned up when the server process exits.
            std::mem::forget(fd);

            info!("wlr-direct: PipeWire FD: {}", raw_fd);

            // Provide stream dimensions to the session handle so pointer
            // coordinate transformation uses the real resolution.
            let handle_streams: Vec<_> = streams
                .iter()
                .map(|s| crate::session::strategy::StreamInfo {
                    node_id: s.node_id,
                    width: s.size.0,
                    height: s.size.1,
                    position_x: s.position.0,
                    position_y: s.position.1,
                })
                .collect();
            session_handle.set_streams(handle_streams);

            (raw_fd, streams)
        } else {
            match session_handle.pipewire_access() {
                PipeWireAccess::FileDescriptor(fd) => {
                    // Portal path: FD directly provided
                    info!("Using Portal-provided PipeWire file descriptor: {}", fd);

                    let strategy_streams = session_handle.streams();
                    let portal_streams: Vec<crate::portal::StreamInfo> = strategy_streams
                        .iter()
                        .map(|s| crate::portal::StreamInfo {
                            node_id: s.node_id,
                            position: (s.position_x, s.position_y),
                            size: (s.width, s.height),
                            source_type: crate::portal::SourceType::Monitor,
                        })
                        .collect();

                    (fd, portal_streams)
                }
                PipeWireAccess::NodeId(node_id) => {
                    info!("Using Mutter-provided PipeWire node ID: {}", node_id);

                    let fd = crate::mutter::get_pipewire_fd_for_mutter()
                        .context("Failed to connect to PipeWire daemon for Mutter")?;

                    info!("Connected to PipeWire daemon, FD: {}", fd);

                    let strategy_streams = session_handle.streams();
                    let portal_streams: Vec<crate::portal::StreamInfo> = strategy_streams
                        .iter()
                        .map(|s| crate::portal::StreamInfo {
                            node_id: s.node_id,
                            position: (s.position_x, s.position_y),
                            size: (s.width, s.height),
                            source_type: crate::portal::SourceType::Monitor,
                        })
                        .collect();

                    (fd, portal_streams)
                }
            }
        };

        // SAFETY: The file descriptor is provided by the XDG Desktop Portal or
        // PipeWire daemon connection. We take ownership here — this is the only
        // place in the codebase where we convert the raw fd to OwnedFd.
        let pipewire_fd = unsafe {
            use std::os::fd::FromRawFd;
            std::os::fd::OwnedFd::from_raw_fd(pipewire_fd)
        };

        // ScreenCast-only or wlr-direct: skip Portal RemoteDesktop entirely.
        // ScreenCast-only = view-only (no input). WlrDirect = input via native Wayland protocols.
        // Both use standalone Portal ScreenCast for video and bypass the full-featured path.
        if matches!(
            session_handle.session_type(),
            SessionType::ScreenCastOnly | SessionType::WlrDirect
        ) {
            let is_wlr_direct = session_handle.session_type() == SessionType::WlrDirect;

            if is_wlr_direct {
                info!("═══════════════════════════════════════════════════════════");
                info!("  WLR-DIRECT MODE (native Wayland input + Portal video)");
                info!("═══════════════════════════════════════════════════════════");
                info!("Video via Portal ScreenCast, input via wlr virtual-keyboard/pointer.");
                info!("Clipboard not wired in this path (data-control is a separate task).");
                info!("═══════════════════════════════════════════════════════════");
            } else {
                info!("═══════════════════════════════════════════════════════════");
                info!("  VIEW-ONLY MODE (ScreenCast-only)");
                info!("═══════════════════════════════════════════════════════════");
                info!("Video streaming enabled, input and clipboard disabled.");
                info!("Used when Portal RemoteDesktop is unavailable (wlroots Flatpak).");
                info!("═══════════════════════════════════════════════════════════");
            }

            let initial_size = stream_info
                .first()
                .map_or((1920, 1080), |s| (s.size.0 as u16, s.size.1 as u16));

            let (graphics_tx, graphics_rx) = tokio::sync::mpsc::channel(64);

            let force_avc420_only = capabilities
                .profile
                .has_quirk(&crate::compositor::Quirk::ForceAvc420);
            let compression_mode = match config.egfx.zgfx_compression.to_lowercase().as_str() {
                "auto" => CompressionMode::Auto,
                "always" => CompressionMode::Always,
                _ => CompressionMode::Never,
            };
            let gfx_factory = LamcoGfxFactory::with_config(
                initial_size.0,
                initial_size.1,
                force_avc420_only,
                config.egfx.max_frames_in_flight,
                compression_mode,
            );
            let gfx_handler_state = gfx_factory.handler_state();
            let gfx_server_handle = gfx_factory.server_handle();

            let display_handler = Arc::new(
                LamcoDisplayHandler::new(
                    initial_size.0,
                    initial_size.1,
                    pipewire_fd,
                    stream_info.clone(),
                    Some(graphics_tx),
                    Some(gfx_server_handle),
                    Some(gfx_handler_state),
                    Arc::clone(&config),
                    Arc::clone(&service_registry),
                )
                .await
                .context("Failed to create display handler")?,
            );

            display_handler
                .set_health_reporter(health_reporter.clone())
                .await;

            // Report subsystems that aren't wired in this code path
            if !is_wlr_direct {
                // ScreenCastOnly: no input injection at all
                health_reporter.report(crate::health::HealthEvent::SubsystemNotAvailable {
                    subsystem: "input".into(),
                });
                // ScreenCastOnly: no clipboard either
                health_reporter.report(crate::health::HealthEvent::SubsystemNotAvailable {
                    subsystem: "clipboard".into(),
                });
            }
            // wlr-direct clipboard availability depends on whether initialization succeeded
            // (reported after clipboard init below)

            let update_sender = display_handler.get_update_sender();
            let _graphics_drain_handle =
                graphics_drain::start_graphics_drain_task(graphics_rx, update_sender);
            Arc::clone(&display_handler).start_pipeline();

            let tls_config = TlsConfig::from_files_with_options(
                &config.security.cert_path,
                &config.security.key_path,
                config.security.require_tls_13,
            )
            .context("Failed to load TLS certificates")?;
            let tls_acceptor =
                ironrdp_server::tokio_rustls::TlsAcceptor::from(tls_config.server_config());

            let codecs = server_codecs_capabilities(&["remotefx"])
                .map_err(|e| anyhow::anyhow!("Failed to create codec capabilities: {e}"))?;

            let primary_stream_id = stream_info.first().map_or(0, |s| s.node_id);
            let audio_node_id = if primary_stream_id > 0 {
                Some(primary_stream_id)
            } else {
                None
            };
            let sound_factory = create_sound_factory(&config.audio, audio_node_id);

            let listen_addr: SocketAddr = config
                .server
                .listen_addr
                .parse()
                .context("Invalid listen address")?;

            // wlr-direct clipboard via wl-clipboard-rs (data-control protocol)
            type CliprdrFactory = Box<dyn ironrdp_server::CliprdrServerFactory>;
            let (wlr_clipboard_manager, wlr_clipboard_factory): (
                Option<Arc<Mutex<ClipboardOrchestrator>>>,
                Option<CliprdrFactory>,
            ) = if is_wlr_direct && config.clipboard.enabled {
                let all_allowed = config.clipboard.allowed_types.is_empty();
                let has_type = |patterns: &[&str]| {
                    all_allowed
                        || config
                            .clipboard
                            .allowed_types
                            .iter()
                            .any(|t| patterns.iter().any(|p| t.contains(p)))
                };

                let clipboard_config = ClipboardOrchestratorConfig {
                    max_data_size: config.clipboard.max_size,
                    enable_images: has_type(&["image/"]),
                    enable_files: has_type(&["uri-list", "file", "x-special"]),
                    enable_html: has_type(&["text/html"]),
                    enable_rtf: has_type(&["rtf"]),
                    rate_limit_ms: config.clipboard.rate_limit_ms,
                    kde_syncselection_hint: config.clipboard.kde_syncselection_hint,
                    ..ClipboardOrchestratorConfig::default()
                };

                match ClipboardOrchestrator::new(clipboard_config).await {
                    Ok(mut clipboard_mgr) => {
                        clipboard_mgr.set_health_reporter(health_reporter.clone());

                        #[cfg(feature = "wl-clipboard")]
                        {
                            let provider = crate::clipboard::providers::WlClipboardProvider::new();
                            clipboard_mgr
                                .set_clipboard_provider(Arc::new(provider))
                                .await;
                            info!("wlr-direct: clipboard via wl-clipboard-rs (data-control)");
                        }

                        #[cfg(not(feature = "wl-clipboard"))]
                        {
                            warn!("wlr-direct: no clipboard provider compiled in (need wl-clipboard feature)");
                        }

                        let mgr = Arc::new(Mutex::new(clipboard_mgr));
                        let factory = LamcoCliprdrFactory::new(Arc::clone(&mgr));
                        (Some(mgr), Some(Box::new(factory) as CliprdrFactory))
                    }
                    Err(e) => {
                        warn!("Clipboard initialization failed, continuing without: {e}");
                        (None, None)
                    }
                }
            } else {
                (None, None)
            };

            if is_wlr_direct && wlr_clipboard_manager.is_none() {
                health_reporter.report(crate::health::HealthEvent::SubsystemNotAvailable {
                    subsystem: "clipboard".into(),
                });
            }

            let rdp_server = if is_wlr_direct {
                // wlr-direct: video from ScreenCast, input from native Wayland protocols
                let monitors: Vec<InputMonitorInfo> = stream_info
                    .iter()
                    .enumerate()
                    .map(|(idx, stream)| InputMonitorInfo {
                        id: idx as u32,
                        name: format!("Monitor {idx}"),
                        x: stream.position.0,
                        y: stream.position.1,
                        width: stream.size.0,
                        height: stream.size.1,
                        dpi: 96.0,
                        scale_factor: 1.0,
                        stream_x: stream.position.0 as u32,
                        stream_y: stream.position.1 as u32,
                        stream_width: stream.size.0,
                        stream_height: stream.size.1,
                        is_primary: idx == 0,
                    })
                    .collect();

                let (input_tx, input_rx) = tokio::sync::mpsc::channel(256);
                let input_handler = LamcoInputHandler::new(
                    session_handle.clone(),
                    monitors,
                    primary_stream_id,
                    input_tx,
                    input_rx,
                    shutdown_broadcast.subscribe(),
                )
                .context("Failed to create wlr-direct input handler")?;

                display_handler
                    .set_input_handler(Arc::new(input_handler.clone()))
                    .await;

                info!("wlr-direct input handler created (virtual keyboard + pointer)");

                RdpServer::builder()
                    .with_addr(listen_addr)
                    .with_tls(tls_acceptor)
                    .with_input_handler(input_handler)
                    .with_display_handler((*display_handler).clone())
                    .with_bitmap_codecs(codecs)
                    .with_cliprdr_factory(wlr_clipboard_factory)
                    .with_gfx_factory(Some(Box::new(gfx_factory)))
                    .with_sound_factory(Some(Box::new(sound_factory)))
                    .build()
            } else {
                // ScreenCast-only: view-only, no input
                RdpServer::builder()
                    .with_addr(listen_addr)
                    .with_tls(tls_acceptor)
                    .with_no_input()
                    .with_display_handler((*display_handler).clone())
                    .with_bitmap_codecs(codecs)
                    .with_cliprdr_factory(None)
                    .with_gfx_factory(Some(Box::new(gfx_factory)))
                    .with_sound_factory(Some(Box::new(sound_factory)))
                    .build()
            };

            display_handler
                .set_server_event_sender(rdp_server.event_sender().clone())
                .await;

            let _ = event_tx.send(ServerEvent::SessionTypeChanged {
                session_type: session_handle.session_type().to_string(),
            });

            let mode_name = if is_wlr_direct {
                "wlr-direct"
            } else {
                "view-only"
            };
            info!("{} server initialized successfully", mode_name);

            return Ok(Self {
                config,
                rdp_server,
                portal_manager: None,
                display_handler,
                service_registry,
                clipboard_manager: wlr_clipboard_manager,
                portal_session: None,
                shutdown_broadcast,
                event_tx,
                event_rx: Some(event_rx),
                health_subscriber: Some(health_subscriber),
                health_monitor_handle: Some(health_monitor_handle),
                cleanup_done: false,
            });
        }

        // Full-featured path: Portal RemoteDesktop with input + clipboard
        let mut portal_config = config.to_portal_config();
        portal_config.persist_mode = ashpd::desktop::PersistMode::DoNot; // Don't persist (causes errors)
        portal_config.restore_token = None;

        let portal_manager = Arc::new(
            PortalManager::new(portal_config)
                .await
                .context("Failed to create Portal manager for input+clipboard")?,
        );

        // Get clipboard components from session handle, or create fallback Portal session
        let (
            portal_clipboard_manager,
            portal_clipboard_session,
            portal_session_valid,
            portal_input_handle,
        ) = if let Some(clipboard_components) = session_handle.portal_clipboard() {
            // Portal strategy: use session_handle directly (no duplicate sessions)
            info!("Strategy provides clipboard components directly");

            let clipboard_mgr = clipboard_components.manager; // Option<Arc<...>>
            let session = clipboard_components.session; // Always present
            let session_valid = clipboard_components.session_valid;

            (clipboard_mgr, session, session_valid, session_handle)
        } else if session_handle.session_type() == SessionType::MutterDirect {
            // Mutter strategy: input and clipboard handled natively via D-Bus/EIS
            // No separate Portal session needed
            info!("Mutter Direct: input+clipboard handled natively (zero dialogs)");

            // Create a placeholder session for the multiplexer (won't be used for clipboard)
            // The Mutter clipboard manager lives on the MutterSessionHandle and is wired separately
            let session_id = format!("lamco-rdp-mutter-placeholder-{}", uuid::Uuid::new_v4());
            let (portal_handle, _) = portal_manager
                .create_session(session_id, None)
                .await
                .context("Failed to create placeholder Portal session")?;

            let session = Arc::new(RwLock::new(portal_handle.session));

            // Mutter provides its own input, so use session_handle directly
            // Mutter manages session validity through its own D-Bus lifecycle
            let session_valid = Arc::new(std::sync::atomic::AtomicBool::new(true));
            (None, session, session_valid, session_handle)
        } else {
            // Other strategies (wlr-direct, libei): need separate Portal session for clipboard
            info!("Strategy doesn't provide clipboard, creating separate Portal session");

            let clipboard_mgr = if capabilities.portal.supports_clipboard {
                match lamco_portal::ClipboardManager::new().await {
                    Ok(mgr) => {
                        info!("Portal clipboard manager created");
                        Some(Arc::new(mgr))
                    }
                    Err(e) => {
                        warn!("Failed to create clipboard manager: {}", e);
                        None
                    }
                }
            } else {
                info!(
                    "Skipping clipboard creation - Portal v{} doesn't support clipboard",
                    capabilities.portal.version
                );
                None
            };

            let session_id = format!("lamco-rdp-input-clipboard-{}", uuid::Uuid::new_v4());
            let (portal_handle, _) = portal_manager
                .create_session(
                    session_id,
                    clipboard_mgr.as_ref().map(std::convert::AsRef::as_ref),
                )
                .await
                .context("Failed to create Portal session for input+clipboard")?;

            info!("Separate Portal session created for input+clipboard");

            let session = Arc::new(RwLock::new(portal_handle.session));

            let input_handle =
                crate::session::strategies::PortalSessionHandleImpl::from_portal_session(
                    session.clone(),
                    portal_manager.remote_desktop().clone(),
                    clipboard_mgr.clone(),
                );

            // Separate Portal session — validity tracked on the new PortalSessionHandleImpl
            let session_valid = input_handle.session_valid.clone();
            (
                clipboard_mgr,
                session,
                session_valid,
                Arc::new(input_handle) as Arc<dyn crate::session::SessionHandle>,
            )
        };

        info!(
            "Session started with {} streams, PipeWire FD: {:?}",
            stream_info.len(),
            pipewire_fd
        );

        let initial_size = stream_info
            .first()
            .map_or((1920, 1080), |s| (s.size.0 as u16, s.size.1 as u16)); // Default fallback

        info!(
            "Initial desktop size: {}x{}",
            initial_size.0, initial_size.1
        );

        let (input_tx, input_rx) = tokio::sync::mpsc::channel(256); // Priority 1: Input - increased for mouse burst handling
        let (_control_tx, control_rx) = tokio::sync::mpsc::channel(16); // Priority 2: Control
        let (_clipboard_tx, clipboard_rx) = tokio::sync::mpsc::channel(8); // Priority 3: Clipboard
        let (graphics_tx, graphics_rx) = tokio::sync::mpsc::channel(64); // Priority 4: Graphics - increased for frame coalescing
        info!("📊 Full multiplexer queues created:");
        info!("   Input queue: 256 (Priority 1 - handles mouse bursts)");
        info!("   Control queue: 16 (Priority 2 - session critical)");
        info!("   Clipboard queue: 8 (Priority 3 - user operations)");
        info!("   Graphics queue: 64 (Priority 4 - damage region coalescing)");

        // ForceAvc420 quirk: AVC444 dual-stream too expensive on this platform
        let force_avc420_only = capabilities
            .profile
            .has_quirk(&crate::compositor::Quirk::ForceAvc420);

        let compression_mode = match config.egfx.zgfx_compression.to_lowercase().as_str() {
            "auto" => CompressionMode::Auto,
            "always" => CompressionMode::Always,
            _ => CompressionMode::Never, // Default: no compression
        };
        info!("ZGFX compression mode: {:?}", compression_mode);

        let gfx_factory = LamcoGfxFactory::with_config(
            initial_size.0,
            initial_size.1,
            force_avc420_only,
            config.egfx.max_frames_in_flight,
            compression_mode,
        );
        let gfx_handler_state = gfx_factory.handler_state();
        let gfx_server_handle = gfx_factory.server_handle();
        if force_avc420_only {
            info!("EGFX factory created for H.264/AVC420 streaming (AVC444 disabled by platform quirk)");
        } else {
            info!("EGFX factory created for H.264/AVC420+AVC444 streaming");
        }

        let display_handler = Arc::new(
            LamcoDisplayHandler::new(
                initial_size.0,
                initial_size.1,
                pipewire_fd,
                stream_info.clone(), // streams() returns &[StreamInfo], convert to Vec
                Some(graphics_tx),   // Graphics queue for multiplexer
                Some(gfx_server_handle), // EGFX server handle for H.264 frame sending
                Some(gfx_handler_state), // EGFX handler state for readiness checks
                Arc::clone(&config), // Pass config for feature flags
                Arc::clone(&service_registry), // Service registry for feature decisions
            )
            .await
            .context("Failed to create display handler")?,
        );

        display_handler
            .set_health_reporter(health_reporter.clone())
            .await;

        let update_sender = display_handler.get_update_sender();
        let _graphics_drain_handle =
            graphics_drain::start_graphics_drain_task(graphics_rx, update_sender);
        info!("Graphics drain task started");

        Arc::clone(&display_handler).start_pipeline();

        info!("Creating input handler for mouse/keyboard control");

        let monitors: Vec<InputMonitorInfo> = stream_info
            .iter()
            .enumerate()
            .map(|(idx, stream)| InputMonitorInfo {
                id: idx as u32,
                name: format!("Monitor {idx}"),
                x: stream.position.0,
                y: stream.position.1,
                width: stream.size.0,
                height: stream.size.1,
                dpi: 96.0,         // Default DPI
                scale_factor: 1.0, // Default scale, Portal doesn't provide this
                stream_x: stream.position.0 as u32,
                stream_y: stream.position.1 as u32,
                stream_width: stream.size.0,
                stream_height: stream.size.1,
                is_primary: idx == 0, // First monitor is primary
            })
            .collect();

        let primary_stream_id = stream_info.first().map_or(0, |s| s.node_id);

        info!(
            "Using PipeWire stream node ID {} for input injection",
            primary_stream_id
        );

        // HYBRID: For Mutter strategy, uses Portal for input while Mutter handles video
        let session_handle_for_clipboard = Arc::clone(&portal_input_handle);
        let input_handler = LamcoInputHandler::new(
            portal_input_handle, // Use Portal session for input (works on all DEs)
            monitors.clone(),
            primary_stream_id,
            input_tx.clone(), // Multiplexer input queue sender (for handler callbacks)
            input_rx,         // Multiplexer input queue receiver (for batching task)
            shutdown_broadcast.subscribe(), // Shutdown signal for batching task
        )
        .context("Failed to create input handler")?;

        info!("Input handler created successfully - mouse/keyboard enabled via Portal");

        display_handler
            .set_input_handler(Arc::new(input_handler.clone()))
            .await;

        // Input queue is handled by input_handler's batching task;
        // multiplexer loop handles control/clipboard priorities
        let portal_for_mux = portal_manager.remote_desktop().clone();
        let keyboard_handler = input_handler.keyboard_handler.clone();
        let mouse_handler = input_handler.mouse_handler.clone();
        let coord_transformer = input_handler.coordinate_transformer.clone();
        // On Portal v1, portal_clipboard_session may be placeholder - but multiplexer only uses it if clipboard_mgr exists
        let session_for_mux = Arc::clone(&portal_clipboard_session);

        tokio::spawn(multiplexer_loop::run_multiplexer_drain_loop(
            control_rx,
            clipboard_rx,
            portal_for_mux,
            keyboard_handler,
            mouse_handler,
            coord_transformer,
            session_for_mux,
            primary_stream_id,
        ));
        info!("🚀 Full multiplexer drain loop started (control + clipboard priorities)");

        info!("Setting up TLS");
        let tls_config = TlsConfig::from_files_with_options(
            &config.security.cert_path,
            &config.security.key_path,
            config.security.require_tls_13,
        )
        .context("Failed to load TLS certificates")?;

        let tls_acceptor =
            ironrdp_server::tokio_rustls::TlsAcceptor::from(tls_config.server_config());

        let codecs = server_codecs_capabilities(&["remotefx"])
            .map_err(|e| anyhow::anyhow!("Failed to create codec capabilities: {e}"))?;

        // KDE Bug 515465 (Portal clipboard crash) is handled by the
        // KdePortalClipboardUnstable quirk in the service registry and
        // ClipboardIntegrationMode::select(). No separate check needed here.
        let clipboard_manager = if config.clipboard.enabled {
            info!("Initializing clipboard manager");

            // allowed_types: empty = all allowed, otherwise check for specific patterns
            let all_allowed = config.clipboard.allowed_types.is_empty();
            let has_type = |patterns: &[&str]| {
                all_allowed
                    || config
                        .clipboard
                        .allowed_types
                        .iter()
                        .any(|t| patterns.iter().any(|p| t.contains(p)))
            };

            let clipboard_config = ClipboardOrchestratorConfig {
                max_data_size: config.clipboard.max_size,
                enable_images: has_type(&["image/"]),
                enable_files: has_type(&["uri-list", "file", "x-special"]),
                enable_html: has_type(&["text/html"]),
                enable_rtf: has_type(&["rtf"]),
                rate_limit_ms: config.clipboard.rate_limit_ms,
                kde_syncselection_hint: config.clipboard.kde_syncselection_hint,
                ..ClipboardOrchestratorConfig::default()
            };

            let mut clipboard_mgr = ClipboardOrchestrator::new(clipboard_config)
                .await
                .context("Failed to create clipboard manager")?;

            clipboard_mgr.set_health_reporter(health_reporter.clone());

            // Select clipboard strategy first — it drives provider choice
            let clipboard_strategy = crate::clipboard::ClipboardIntegrationMode::select(
                &service_registry,
                &config.clipboard,
                is_flatpak(),
            );

            // Create and set clipboard provider based on strategy + session type
            let uses_data_control = matches!(
                clipboard_strategy,
                crate::clipboard::ClipboardIntegrationMode::WaylandDataControlMode { .. }
            );

            match session_handle_for_clipboard.session_type() {
                SessionType::MutterDirect if !uses_data_control => {
                    // Mutter: use MutterClipboardProvider if clipboard manager is available
                    if let Some(mutter_clipboard) = session_handle_for_clipboard.mutter_clipboard()
                    {
                        match crate::clipboard::providers::MutterClipboardProvider::new(
                            mutter_clipboard,
                        )
                        .await
                        {
                            Ok(provider) => {
                                clipboard_mgr
                                    .set_clipboard_provider(Arc::new(provider))
                                    .await;
                                info!("Clipboard provider: Mutter (D-Bus)");
                            }
                            Err(e) => {
                                warn!("Failed to create Mutter clipboard provider: {e}");
                            }
                        }
                    }
                }
                #[cfg(feature = "portal-generic")]
                SessionType::PortalGeneric if uses_data_control => {
                    // portal-generic with data-control strategy: use DataControlClipboardProvider
                    if let Some(backend) = session_handle_for_clipboard.clipboard_backend() {
                        let provider =
                            crate::clipboard::providers::DataControlClipboardProvider::new(backend);
                        clipboard_mgr
                            .set_clipboard_provider(Arc::new(provider))
                            .await;
                        info!("Clipboard provider: data-control (portal-generic backend)");
                    } else {
                        warn!("portal-generic: No clipboard backend available, falling back to Portal");
                        if let Some(ref clipboard_mgr_arc) = portal_clipboard_manager {
                            let provider =
                                crate::clipboard::providers::PortalClipboardProvider::new(
                                    Arc::clone(clipboard_mgr_arc),
                                    Arc::clone(&portal_clipboard_session),
                                    Arc::clone(&portal_session_valid),
                                    config.clipboard.rate_limit_ms,
                                )
                                .await;
                            clipboard_mgr
                                .set_clipboard_provider(Arc::new(provider))
                                .await;
                            info!("Clipboard provider: Portal (fallback)");
                        }
                    }
                }
                _ if uses_data_control => {
                    // Non-portal-generic session + data-control strategy: try standalone wl-clipboard-rs
                    #[cfg(feature = "wl-clipboard")]
                    let provider_set = {
                        let provider = crate::clipboard::providers::WlClipboardProvider::new();
                        clipboard_mgr
                            .set_clipboard_provider(Arc::new(provider))
                            .await;
                        info!("Clipboard provider: wl-clipboard-rs (standalone data-control)");
                        true
                    };

                    #[cfg(not(feature = "wl-clipboard"))]
                    let provider_set = false;

                    if !provider_set {
                        warn!("WaylandDataControlMode selected but no data-control provider available");
                        warn!("Falling back to Portal clipboard provider");
                        if let Some(ref clipboard_mgr_arc) = portal_clipboard_manager {
                            let provider =
                                crate::clipboard::providers::PortalClipboardProvider::new(
                                    Arc::clone(clipboard_mgr_arc),
                                    Arc::clone(&portal_clipboard_session),
                                    Arc::clone(&portal_session_valid),
                                    config.clipboard.rate_limit_ms,
                                )
                                .await;
                            clipboard_mgr
                                .set_clipboard_provider(Arc::new(provider))
                                .await;
                            info!("Clipboard provider: Portal (fallback)");
                        }
                    }
                }
                #[cfg(feature = "portal-generic")]
                SessionType::PortalGeneric => {
                    // portal-generic without data-control strategy: still use DataControlClipboardProvider
                    // since portal-generic bundles clipboard with its session
                    if let Some(backend) = session_handle_for_clipboard.clipboard_backend() {
                        let provider =
                            crate::clipboard::providers::DataControlClipboardProvider::new(backend);
                        clipboard_mgr
                            .set_clipboard_provider(Arc::new(provider))
                            .await;
                        info!("Clipboard provider: data-control (Wayland)");
                    } else {
                        warn!("portal-generic: No clipboard backend available");
                    }
                }
                _ => {
                    // Portal strategy: wrap the Portal clipboard manager in a provider
                    if let Some(ref clipboard_mgr_arc) = portal_clipboard_manager {
                        let provider = crate::clipboard::providers::PortalClipboardProvider::new(
                            Arc::clone(clipboard_mgr_arc),
                            Arc::clone(&portal_clipboard_session),
                            Arc::clone(&portal_session_valid),
                            config.clipboard.rate_limit_ms,
                        )
                        .await;
                        clipboard_mgr
                            .set_clipboard_provider(Arc::new(provider))
                            .await;
                        info!("Clipboard provider: Portal");
                    }
                }
            }

            // Runtime health check: verify the data-control provider works.
            // Fall back to Portal if it fails and fallback_to_portal is enabled.
            if let crate::clipboard::ClipboardIntegrationMode::WaylandDataControlMode {
                fallback_to_portal,
                ..
            } = &clipboard_strategy
            {
                if let Err(e) = clipboard_mgr.health_check_provider().await {
                    warn!("Data-control clipboard health check failed: {e}");
                    if *fallback_to_portal {
                        warn!("Falling back to Portal clipboard provider");
                        if let Some(ref clipboard_mgr_arc) = portal_clipboard_manager {
                            let provider =
                                crate::clipboard::providers::PortalClipboardProvider::new(
                                    Arc::clone(clipboard_mgr_arc),
                                    Arc::clone(&portal_clipboard_session),
                                    Arc::clone(&portal_session_valid),
                                    config.clipboard.rate_limit_ms,
                                )
                                .await;
                            clipboard_mgr
                                .set_clipboard_provider(Arc::new(provider))
                                .await;
                            info!(
                                "Clipboard provider: Portal (fallback after health check failure)"
                            );
                        }
                    }
                }
            }

            let session_connection = if clipboard_strategy.uses_klipper_cooperation() {
                match zbus::Connection::session().await {
                    Ok(conn) => {
                        info!("D-Bus session connection established for Klipper cooperation");
                        Some(conn)
                    }
                    Err(e) => {
                        warn!("Failed to get D-Bus session connection: {}", e);
                        warn!("Klipper cooperation will be disabled, falling back to Tier 3");
                        None
                    }
                }
            } else {
                None
            };

            if let Err(e) = clipboard_mgr
                .initialize_strategy(clipboard_strategy, session_connection)
                .await
            {
                warn!("Failed to initialize clipboard strategy: {:#}", e);
                warn!("Clipboard may use default strategy");
            }

            // FUSE is not available in Flatpak sandbox (no /dev/fuse access)
            if is_flatpak() {
                info!("Flatpak detected - skipping FUSE mount (using staging fallback for file clipboard)");
            } else if let Err(e) = clipboard_mgr.mount_fuse().await {
                warn!("Failed to mount FUSE clipboard filesystem: {:?}", e);
                warn!("File clipboard will use staging fallback (download files upfront)");
            }

            Arc::new(Mutex::new(clipboard_mgr))
        } else {
            info!("Clipboard disabled by configuration");
            let clipboard_mgr = ClipboardOrchestrator::new(ClipboardOrchestratorConfig::default())
                .await
                .context("Failed to create clipboard manager")?;
            Arc::new(Mutex::new(clipboard_mgr))
        };

        // Set clipboard manager reference in display handler for reconnection cleanup
        // When client reconnects (detected via display channel exhaustion), display handler
        // will clear Portal clipboard to prevent KDE Portal crash (Bug 515465)
        display_handler
            .set_clipboard_manager(Arc::clone(&clipboard_manager))
            .await;

        let clipboard_factory = LamcoCliprdrFactory::new(Arc::clone(&clipboard_manager));

        // Use the primary video stream's PipeWire node ID for audio capture targeting.
        // This connects audio capture to the same session as the screen capture,
        // ensuring we get the correct desktop audio output.
        let audio_node_id = if primary_stream_id > 0 {
            Some(primary_stream_id)
        } else {
            None
        };
        let sound_factory = create_sound_factory(&config.audio, audio_node_id);
        if config.audio.enabled {
            info!(
                "Audio support enabled: codec={}, sample_rate={}, channels={}",
                config.audio.codec, config.audio.sample_rate, config.audio.channels
            );
        } else {
            debug!("Audio support disabled by configuration");
        }

        info!("Building IronRDP server");
        let listen_addr: SocketAddr = config
            .server
            .listen_addr
            .parse()
            .context("Invalid listen address")?;

        let rdp_server = RdpServer::builder()
            .with_addr(listen_addr)
            .with_tls(tls_acceptor)
            .with_input_handler(input_handler)
            .with_display_handler((*display_handler).clone())
            .with_bitmap_codecs(codecs)
            .with_cliprdr_factory(Some(Box::new(clipboard_factory)))
            .with_gfx_factory(Some(Box::new(gfx_factory)))
            .with_sound_factory(Some(Box::new(sound_factory)))
            .build();

        display_handler
            .set_server_event_sender(rdp_server.event_sender().clone())
            .await;
        info!("Server event sender configured in display handler");

        let _ = event_tx.send(ServerEvent::SessionTypeChanged {
            session_type: session_handle_for_clipboard.session_type().to_string(),
        });

        info!("Server initialized successfully");

        Ok(Self {
            config,
            rdp_server,
            portal_manager: Some(portal_manager),
            display_handler,
            service_registry,
            clipboard_manager: Some(clipboard_manager),
            portal_session: Some(portal_clipboard_session),
            shutdown_broadcast,
            event_tx,
            event_rx: Some(event_rx),
            health_subscriber: Some(health_subscriber),
            health_monitor_handle: Some(health_monitor_handle),
            cleanup_done: false,
        })
    }

    /// Run the server, blocking until shutdown.
    pub async fn run(mut self) -> Result<()> {
        info!("╔════════════════════════════════════════════════════════════╗");
        info!("║          Server Starting                                   ║");
        info!("╚════════════════════════════════════════════════════════════╝");
        info!("  Listen Address: {}", self.config.server.listen_addr);
        info!("  TLS: Enabled (rustls 0.23)");
        info!("  Codec: RemoteFX");
        info!("  Max Connections: {}", self.config.server.max_connections);
        info!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");

        // Emit running status
        let _ = self.event_tx.send(ServerEvent::StatusChanged {
            old: "starting".into(),
            new: "running".into(),
            message: format!("Listening on {}", self.config.server.listen_addr),
        });

        info!("Server is ready and listening for RDP connections");
        info!("Waiting for clients to connect...");

        // If config specifies PAM but PAM is unavailable (Flatpak), fall back gracefully
        let configured_auth = &self.config.security.auth_method;
        let effective_auth_method =
            if configured_auth == "pam" && !self.service_registry.has_pam_auth() {
                warn!("⚠️  PAM authentication configured but unavailable in this deployment");
                warn!(
                    "   PAM service level: {:?}",
                    self.service_registry.pam_auth_level()
                );
                warn!(
                    "   Falling back to recommended method: {}",
                    self.service_registry.recommended_auth_method()
                );
                self.service_registry.recommended_auth_method()
            } else {
                configured_auth.as_str()
            };

        // Even with auth_method="none", IronRDP needs credentials
        // to complete the protocol handshake
        let credentials = if effective_auth_method == "none" {
            Some(Credentials {
                username: String::new(),
                password: String::new(),
                domain: None,
            })
        } else {
            // For future authentication support (PAM integration)
            None
        };

        self.rdp_server.set_credentials(credentials);

        if effective_auth_method != configured_auth {
            info!(
                "Authentication: {} (configured: {}, fallback due to deployment)",
                effective_auth_method, configured_auth
            );
        } else {
            info!("Authentication: {}", effective_auth_method);
        }

        // Bind the TCP listener with SO_REUSEADDR to avoid EADDRINUSE after
        // restart. IronRDP's built-in run() uses bare TcpListener::bind() which
        // doesn't set this, so a previous server's TIME_WAIT sockets block rebinding.
        let listen_addr: std::net::SocketAddr = self
            .config
            .server
            .listen_addr
            .parse()
            .context("Invalid listen address")?;

        // Pre-bind check: detect if the port is already in use and identify the holder
        check_port_available(&listen_addr);

        let socket = tokio::net::TcpSocket::new_v4().context("Failed to create TCP socket")?;
        socket
            .set_reuseaddr(true)
            .context("Failed to set SO_REUSEADDR")?;
        if let Err(e) = socket.bind(listen_addr) {
            error!(
                "Failed to bind to {}: {}. Another process may be using this port.",
                listen_addr, e
            );
            // Run the check again after failure for detailed diagnostics
            check_port_available(&listen_addr);
            return Err(anyhow::anyhow!(
                "Failed to bind listen address {listen_addr}: {e}"
            ));
        }
        let listener = socket.listen(128).context("Failed to start TCP listener")?;
        info!(
            "TCP listener bound to {} with SO_REUSEADDR",
            listener.local_addr().unwrap_or(listen_addr)
        );

        // Accept loop: handle connections via IronRDP's run_connection(),
        // with shutdown coordination via broadcast channel.
        let mut shutdown_rx = self.shutdown_broadcast.subscribe();
        let result: anyhow::Result<()> = loop {
            tokio::select! {
                accept_result = listener.accept() => {
                    match accept_result {
                        Ok((stream, peer)) => {
                            debug!("Accepted connection from {peer}");
                            let client_id = format!("rdp-{}", uuid::Uuid::new_v4());
                            let conn_start = std::time::Instant::now();
                            let conn_timestamp = std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap_or_default()
                                .as_secs();

                            let _ = self.event_tx.send(ServerEvent::ClientConnected {
                                client_id: client_id.clone(),
                                peer_address: peer.to_string(),
                                timestamp: conn_timestamp,
                            });

                            if let Err(e) = self.rdp_server.run_connection(stream).await {
                                let duration = conn_start.elapsed();
                                let msg = format!("{e:#}");
                                let is_reset = msg.contains("Connection reset by peer")
                                    || msg.contains("os error 104");

                                if is_reset && duration < std::time::Duration::from_secs(1) {
                                    // mstsc.exe commonly probes with a short-lived
                                    // connection before the real one; not an error.
                                    warn!("Connection from {peer} reset during handshake (likely client probe, lasted {:.0}ms)", duration.as_secs_f64() * 1000.0);
                                } else if is_reset {
                                    // Connection was established and running, then reset.
                                    // This is a real connection failure, not a probe.
                                    error!("Connection from {peer} reset after {:.1}s (active session lost)", duration.as_secs_f64());
                                } else {
                                    error!("Connection error from {peer} after {:.1}s: {msg}", duration.as_secs_f64());
                                }
                            }
                            // Emit disconnect event
                            let duration = conn_start.elapsed().as_secs();
                            let _ = self.event_tx.send(ServerEvent::ClientDisconnected {
                                client_id,
                                reason: "Connection ended".into(),
                                duration_seconds: duration,
                            });

                            // Client disconnected (or failed): clean up transient state
                            // while keeping Portal/PipeWire alive for the next client.
                            // If session health is invalid (compositor destroyed session),
                            // break the accept loop — no point accepting doomed connections.
                            if !self.on_disconnect().await {
                                let _ = self.event_tx.send(ServerEvent::StatusChanged {
                                    old: "running".into(),
                                    new: "stopped".into(),
                                    message: "Session invalidated by compositor".into(),
                                });
                                break Ok(());
                            }
                        }
                        Err(e) => {
                            warn!("Accept failed: {e}");
                        }
                    }
                }
                _ = shutdown_rx.recv() => {
                    info!("Shutdown broadcast received: stopping server");
                    let _ = self.event_tx.send(ServerEvent::StatusChanged {
                        old: "running".into(),
                        new: "stopped".into(),
                        message: "Shutdown requested".into(),
                    });
                    break Ok(());
                }
            }
        };

        if let Err(ref e) = result {
            error!("Server stopped with error: {:#}", e);
            if self.config.notifications.on_error {
                send_portal_notification(
                    "server-error",
                    "RDP Server Error",
                    &format!("{e:#}"),
                    true,
                )
                .await;
            }
        } else {
            info!("Server stopped gracefully");
        }

        info!("Performing post-run cleanup...");
        // Health return value is irrelevant here — we're shutting down regardless
        self.on_disconnect().await;

        if let Err(e) = self.cleanup_resources().await {
            warn!("Resource cleanup failed: {:#}", e);
        }

        result
    }

    /// Take the server event receiver for D-Bus signal relay wiring.
    ///
    /// Call this before `run()` and pass the receiver to `dbus::events::start_signal_relay()`.
    /// If not taken, server events are silently dropped (no receiver on the channel).
    pub fn take_event_receiver(
        &mut self,
    ) -> Option<tokio::sync::mpsc::UnboundedReceiver<ServerEvent>> {
        self.event_rx.take()
    }

    /// Useful for signal handlers that need to trigger shutdown after `run()` consumes self.
    pub fn shutdown_sender(
        &self,
    ) -> tokio::sync::mpsc::UnboundedSender<ironrdp_server::ServerEvent> {
        self.rdp_server.event_sender().clone()
    }

    /// Broadcast sender for coordinating shutdown across all async tasks.
    /// Signal handlers should send on this AND on `shutdown_sender()` —
    /// IronRDP needs the Quit event to close the TLS connection gracefully,
    /// while the broadcast breaks our outer select loop and stops clipboard/PipeWire tasks.
    pub fn shutdown_broadcast(&self) -> Arc<tokio::sync::broadcast::Sender<()>> {
        Arc::clone(&self.shutdown_broadcast)
    }

    /// Signal graceful shutdown. Actual cleanup happens in cleanup_resources().
    pub fn signal_shutdown(&self) {
        info!("Initiating graceful shutdown");
        let _ = self
            .rdp_server
            .event_sender()
            .send(ironrdp_server::ServerEvent::Quit(
                "Shutdown requested".to_string(),
            ));
        let _ = self.shutdown_broadcast.send(());
    }

    /// Explicit cleanup preventing KDE Portal crashes during reconnect.
    /// Portal sessions must be closed and clipboard operations cancelled
    /// before resources are freed. See: docs/COMPREHENSIVE-CLEANUP-PLAN-2026-02-03.md Phase 1
    pub async fn cleanup_resources(&mut self) -> Result<()> {
        if self.cleanup_done {
            debug!("Cleanup already performed, skipping");
            return Ok(());
        }
        self.cleanup_done = true;

        info!("═══════════════════════════════════════════════════════════");
        info!("  Server Shutdown - Cleaning Resources");
        info!("═══════════════════════════════════════════════════════════");

        // Emit stopped status via D-Bus before tearing down subsystems
        let _ = self.event_tx.send(ServerEvent::StatusChanged {
            old: "running".into(),
            new: "stopped".into(),
            message: "Server shutting down".into(),
        });

        info!("  Broadcast shutdown signal to all subsystems...");
        let subscriber_count = self.shutdown_broadcast.receiver_count();
        info!("  Broadcasting to {} subscribers", subscriber_count);
        let _ = self.shutdown_broadcast.send(());
        info!("  ✅ Shutdown broadcast sent");

        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

        if let Some(clipboard_arc) = &self.clipboard_manager {
            info!("  Shutting down clipboard manager...");
            let mut clipboard = clipboard_arc.lock().await;
            clipboard.shutdown().await?;
            info!("  ✅ Clipboard manager stopped");
        }

        // PipeWire is in Arc<Mutex<>> with references from spawned tasks;
        // explicit shutdown ensures immediate cleanup
        info!("  Shutting down PipeWire...");
        self.display_handler.shutdown_pipewire().await;

        if let Some(session_arc) = &self.portal_session {
            info!("  Closing Portal session...");

            let session_guard = session_arc.read().await;

            match session_guard.close().await {
                Ok(()) => {
                    info!("  ✅ Portal session closed successfully");
                }
                Err(e) => {
                    warn!("  ⚠️  Portal session close failed: {}", e);
                    // Best effort cleanup
                }
            }
        }

        info!("  ═══════════════════════════════════════════════════════════");
        info!("  ✅ Server shutdown complete");
        info!("  ═══════════════════════════════════════════════════════════");

        Ok(())
    }

    /// Clears transient state without closing Portal session (reusable for reconnect).
    /// The Portal session, PipeWire stream, and input handler survive for the next client.
    ///
    /// Returns `true` if the session is healthy enough to accept another client,
    /// `false` if the accept loop should break (session is fatally invalid).
    async fn on_disconnect(&self) -> bool {
        info!("Client disconnected - performing cleanup");

        // Stop the pipeline from encoding/sending frames to a dead channel.
        // PipeWire frames are still drained to keep the stream responsive,
        // but no CPU is wasted on encoding or queue pressure.
        self.display_handler.on_client_disconnect();

        // Check health state to decide whether this server instance can accept
        // another client. If the compositor destroyed the session, there's no
        // point looping — the Portal/Mutter session object is gone.
        if let Some(ref subscriber) = self.health_subscriber {
            let health = subscriber.current();
            match health.overall {
                crate::health::OverallHealth::Invalid => {
                    error!(
                        "Session health is invalid after disconnect — cannot accept new clients"
                    );
                    error!("  session: {}", health.session);
                    error!("  video: {}", health.video);
                    error!("  input: {}", health.input);
                    error!("  clipboard: {}", health.clipboard);
                    return false;
                }
                crate::health::OverallHealth::Degraded => {
                    warn!("Session health is degraded — will accept new clients cautiously");
                    warn!("  session: {}", health.session);
                    warn!("  video: {}", health.video);
                    warn!("  input: {}", health.input);
                }
                _ => {
                    info!("Disconnect cleanup complete - ready for next connection");
                }
            }
        } else {
            info!("Disconnect cleanup complete - ready for next connection");
        }

        true
    }
}

impl Drop for LamcoRdpServer {
    fn drop(&mut self) {
        info!("LamcoRdpServer dropping - initiating cleanup");

        // cleanup_resources() is async but Drop is sync
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.block_on(async {
                if let Err(e) = self.cleanup_resources().await {
                    error!("Error during cleanup: {:#}", e);
                } else {
                    info!("✅ Cleanup completed successfully");
                }
            });
        } else {
            warn!("No tokio runtime available for cleanup - resources may leak!");
            warn!("Portal session not closed - will leak in Portal daemon");
        }
    }
}

/// Check if a port is available before attempting to bind.
///
/// Uses a standard TCP connect probe and /proc/net/tcp inspection to detect
/// whether the port is already in use and, if possible, identify the process
/// holding it.
fn check_port_available(addr: &std::net::SocketAddr) {
    let port = addr.port();

    // Probe 1: Try connecting to the port to see if something is listening
    match std::net::TcpStream::connect_timeout(addr, std::time::Duration::from_millis(100)) {
        Ok(_) => {
            warn!(
                "Port {} is already in use: another service is accepting connections",
                port
            );
        }
        Err(e) if e.kind() == std::io::ErrorKind::ConnectionRefused => {
            // Port is free (connection refused = nothing listening)
            debug!("Port {} is available (connection refused on probe)", port);
            return;
        }
        Err(_) => {
            // Timeout or other error: port might be in use, continue checking
        }
    }

    // Probe 2: Check /proc/net/tcp for processes bound to this port
    // Format: local_address (hex ip:port), ... inode
    if let Ok(tcp_data) = std::fs::read_to_string("/proc/net/tcp") {
        let port_hex = format!("{port:04X}");
        for line in tcp_data.lines().skip(1) {
            let fields: Vec<&str> = line.split_whitespace().collect();
            if fields.len() < 10 {
                continue;
            }
            let local_addr = fields[1];
            // local_addr format: "IIIIIIII:PPPP" (hex ip:port)
            if let Some(local_port) = local_addr.split(':').nth(1) {
                if local_port == port_hex {
                    let state = fields[3];
                    let inode = fields[9];
                    let state_name = match state {
                        "0A" => "LISTEN",
                        "01" => "ESTABLISHED",
                        "06" => "TIME_WAIT",
                        "08" => "CLOSE_WAIT",
                        _ => state,
                    };

                    // Try to find the process via /proc/*/fd -> socket inode
                    let process_info = find_process_by_inode(inode);

                    if let Some((pid, name)) = process_info {
                        error!(
                            "Port {} is held by process '{}' (PID {}) in state {}",
                            port, name, pid, state_name
                        );
                    } else {
                        warn!(
                            "Port {} is in use (state: {}, inode: {})",
                            port, state_name, inode
                        );
                    }
                }
            }
        }
    }
}

/// Find a process by socket inode number via /proc/*/fd scanning.
///
/// Returns (pid, process_name) if found.
fn find_process_by_inode(inode: &str) -> Option<(u32, String)> {
    let target = format!("socket:[{inode}]");
    let proc_dir = match std::fs::read_dir("/proc") {
        Ok(d) => d,
        Err(_) => return None,
    };

    for entry in proc_dir.flatten() {
        let name = entry.file_name();
        let pid_str = name.to_string_lossy();
        let pid: u32 = match pid_str.parse() {
            Ok(p) => p,
            Err(_) => continue,
        };

        let fd_dir = format!("/proc/{pid}/fd");
        if let Ok(fds) = std::fs::read_dir(&fd_dir) {
            for fd in fds.flatten() {
                if let Ok(link) = std::fs::read_link(fd.path()) {
                    if link.to_string_lossy() == target {
                        // Found the process - get its name
                        let comm = std::fs::read_to_string(format!("/proc/{pid}/comm"))
                            .unwrap_or_default()
                            .trim()
                            .to_string();
                        return Some((pid, comm));
                    }
                }
            }
        }
    }
    None
}

/// Send a desktop notification via the Notification portal.
///
/// Only fires in Flatpak mode — native installs rely on logs/system tray.
/// Failures are silently ignored since notifications are informational.
async fn send_portal_notification(id: &str, title: &str, body: &str, high_priority: bool) {
    if !crate::config::is_flatpak() {
        return;
    }

    use ashpd::desktop::notification::{Notification, NotificationProxy, Priority};

    let proxy = match NotificationProxy::new().await {
        Ok(p) => p,
        Err(e) => {
            debug!("Notification portal unavailable: {}", e);
            return;
        }
    };

    let priority = if high_priority {
        Priority::High
    } else {
        Priority::Normal
    };

    let notification = Notification::new(title).body(body).priority(priority);

    if let Err(e) = proxy.add_notification(id, notification).await {
        debug!("Failed to send notification: {}", e);
    }
}

#[cfg(test)]
mod tests {

    #[tokio::test]
    #[ignore = "Requires D-Bus and portal access"]
    async fn test_server_initialization() {
        // This test would require a full environment
        // For now, just verify compilation
    }
}
