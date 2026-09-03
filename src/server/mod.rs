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
    reason = "OwnedFd::from_raw_fd for Portal/PipeWire file descriptors"
)]

mod deployment;
mod display_handler;
mod egfx_sender;
#[expect(dead_code, reason = "WIP: not yet integrated into the server pipeline")]
mod event_multiplexer;
mod gfx_factory;
mod graphics_drain;
mod input_handler;
#[expect(dead_code, reason = "WIP: not yet integrated into the server pipeline")]
mod multiplexer_loop;
mod pipeline_decisions;
mod rdpei_factory;

use std::{net::SocketAddr, sync::Arc};

use anyhow::{Context, Result};
pub use display_handler::LamcoDisplayHandler;
pub use egfx_sender::{EgfxFrameSender, SendError};
pub use gfx_factory::{LamcoGfxFactory, SharedHandlerState};

/// Interval between server-side NetworkAutoDetect RTT probes.
const AUTODETECT_RTT_PROBE_INTERVAL_MS: u64 = 250;

/// Drive server-side NetworkAutoDetect for the desktop path: fire an RTT probe
/// every [`AUTODETECT_RTT_PROBE_INTERVAL_MS`] while a client session is ready.
/// The server measures RTT from the client's responses and reports it back via a
/// Network Characteristics Result (upstream PR #1470). Gated on EGFX readiness so
/// probes are not queued between connections; the task exits when the server
/// event loop is gone or shutdown fires.
fn spawn_autodetect_probe(
    event_sender: tokio::sync::mpsc::UnboundedSender<ironrdp_server::ServerEvent>,
    handler_state: Arc<SharedHandlerState>,
    mut shutdown_rx: tokio::sync::broadcast::Receiver<()>,
) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(std::time::Duration::from_millis(
            AUTODETECT_RTT_PROBE_INTERVAL_MS,
        ));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                _ = tick.tick() => {
                    if handler_state.is_ready.load(std::sync::atomic::Ordering::Acquire)
                        && event_sender.send(ironrdp_server::ServerEvent::AutoDetectRttRequest).is_err()
                    {
                        break; // server event loop gone — connection ended
                    }
                }
                _ = shutdown_rx.recv() => break,
            }
        }
    });
}
pub use input_handler::{KeyboardLayoutConnectionHandler, LamcoInputHandler};
use ironrdp_graphics::zgfx::CompressionMode;
use ironrdp_pdu::rdp::capability_sets::server_codecs_capabilities;
use ironrdp_server::RdpServer;
pub use rdpei_factory::LamcoRdpeiFactory;
use rdpei_factory::create_rdpei_factory;
use tokio::sync::Mutex;
use tracing::{debug, error, info, warn};

use crate::{
    audio::factory::create_sound_factory,
    clipboard::{ClipboardOrchestrator, ClipboardOrchestratorConfig, LamcoCliprdrFactory},
    config::{Config, is_flatpak},
    dbus::events::{self, ServerEvent},
    health::{HealthSubscriber, SessionHealthMonitor},
    input::MonitorInfo as InputMonitorInfo,
    portal::PortalManager,
    security::TlsConfig,
    services::{ServiceId, ServiceLevel, ServiceRegistry},
    session::{PipeWireAccess, SessionStrategySelector, SessionType},
};

/// Lamco RDP Server
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

    /// Active session handle. Its lifecycle policy drives per-connection
    /// establishment/release via the accept dispatcher (see `run`).
    session_handle: Arc<dyn crate::session::strategy::SessionHandle>,

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
                ashpd::desktop::Session<ashpd::desktop::remote_desktop::RemoteDesktop>,
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

    /// Core metrics collector (shared with EGFX handler and SnapshotCollector)
    metrics: Arc<crate::runtime::metrics::MetricsCollector>,

    /// Performance snapshot collector (aggregates all live data sources)
    snapshot_collector: Arc<crate::health::snapshot_collector::SnapshotCollector>,

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
                crate::compositor::Quirk::NeedsExplicitCursorComposite => {
                    info!("📋 Cursor compositing may be needed (no metadata cursor)");
                }
                crate::compositor::Quirk::RestartCaptureOnResize => {
                    info!("📋 Capture will restart on resolution changes");
                }
                crate::compositor::Quirk::MultiMonitorPositionQuirk => {
                    info!("📋 Multi-monitor positions may need adjustment");
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

        // Wayland observers: color-management (per-output HDR/color state) and
        // output-management (wlroots heads/modes + resolution control). Each
        // returns None when its protocol is absent, so they are no-ops off the
        // relevant compositors.
        #[cfg(feature = "wayland")]
        let color_observer = (config.display.color_management
            && capabilities.has_color_management())
        .then(crate::compositor::ColorObserver::spawn)
        .flatten();
        #[cfg(feature = "wayland")]
        let output_observer = (config.display.output_management
            && capabilities.compositor.is_wlroots_based())
        .then(crate::compositor::OutputObserver::spawn)
        .flatten();

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
        // Shared client-presence flag: the display handler flips it on client
        // connect/disconnect, and the health monitor reads it so a paused capture
        // stream between clients (PerConnection releases on disconnect) reads as
        // idle rather than degraded.
        let client_active_flag = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (health_monitor, health_reporter, health_subscriber) = SessionHealthMonitor::new(
            shutdown_broadcast.subscribe(),
            Arc::clone(&client_active_flag),
        );
        let health_monitor_handle = tokio::spawn(health_monitor.run());

        let (event_tx, event_rx) = events::event_channel();

        // Core metrics collector — shared with EGFX handler and SnapshotCollector.
        // Instantiated early so all subsystems can record from startup.
        let metrics = Arc::new(crate::runtime::metrics::MetricsCollector::new());

        // Sensor registry for version-adaptive health monitoring.
        // Sensors are registered after protocol negotiation (EGFX, encoder, PipeWire).
        // The same Arc is shared with SnapshotCollector for snapshot aggregation.
        let sensor_registry = Arc::new(crate::health::sensors::registry::SensorRegistry::new());

        let snapshot_collector =
            Arc::new(crate::health::snapshot_collector::SnapshotCollector::new(
                Arc::clone(&metrics),
                Arc::clone(&sensor_registry),
            ));

        // Bridge health state changes to D-Bus signals so external consumers
        // (GUI, systemd, monitoring) see health transitions in real time
        let _health_bridge_handle = crate::health::start_health_dbus_bridge(
            health_subscriber.clone(),
            event_tx.clone(),
            shutdown_broadcast.subscribe(),
        );

        // Periodic performance snapshot emitter — pushes live metrics to D-Bus
        // subscribers (GUI, monitoring tools) at the configured interval
        if config.monitoring.enabled {
            let interval_secs = config.monitoring.snapshot_interval_secs;
            let perf_event_tx = event_tx.clone();
            let perf_snapshot = Arc::clone(&snapshot_collector);
            let perf_health_sub = health_subscriber.clone();
            let mut perf_shutdown = shutdown_broadcast.subscribe();
            tokio::spawn(async move {
                let mut interval =
                    tokio::time::interval(std::time::Duration::from_secs(u64::from(interval_secs)));
                // Skip the immediate first tick
                interval.tick().await;
                loop {
                    tokio::select! {
                        _ = interval.tick() => {
                            let snap = perf_snapshot.snapshot();
                            let health = perf_health_sub.current();
                            let _ = perf_event_tx.send(ServerEvent::PerformanceUpdated {
                                fps: snap.fps.current_fps,
                                latency_ms: snap.latency.total_latency_avg_ms,
                                queue_depth: snap.egfx.queue_depth,
                                encoder_backend: snap.encoder.as_ref()
                                    .map(|e| e.backend.clone())
                                    .unwrap_or_default(),
                                activity_level: snap.fps.activity_level.clone(),
                                current_qp: 0, // Updated at runtime by encoding adaptation
                                adaptation_enabled: false, // Updated by config
                                damage_source: snap.fps.damage_source.clone(),
                                sensor_count: snap.sensor_snapshots.len() as u32,
                                bitrate_kbps: snap.encoder.as_ref()
                                    .map_or(0, |e| e.bitrate_kbps),
                                health_video: health.video.to_string(),
                                health_input: health.input.to_string(),
                                health_clipboard: health.clipboard.to_string(),
                                health_session: health.session.to_string(),
                            });
                        }
                        _ = perf_shutdown.recv() => break,
                    }
                }
            });
        }

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

            // Resolve input protocol preference from config + compositor type
            let prefers_libei = config
                .input
                .resolve_for_compositor(&capabilities.compositor);
            info!(
                "Input protocol: {} (config={}, compositor={})",
                if prefers_libei {
                    "libei/EIS"
                } else {
                    "wlr-virtual-input"
                },
                config.input.effective_protocol(),
                capabilities.compositor,
            );

            // Capture-request pacing ceiling for the portal-generic strategy:
            // the higher of the two fps knobs the GUI exposes, so capture
            // pacing never becomes the bottleneck below whichever one the
            // downstream frame-rate regulation actually wants to use.
            let capture_pacing_fps = config
                .video
                .target_fps
                .max(config.performance.adaptive_fps.max_fps);

            let strategy_selector = SessionStrategySelector::with_keyboard_layout(
                service_registry.clone(),
                Arc::new(token_manager),
                config.input.keyboard_layout.clone(),
            )
            .with_input_protocol(prefers_libei)
            .with_capture_pacing_fps(capture_pacing_fps)
            .with_record_mode(crate::mutter::session_manager::RecordMode::from_config(
                &config.capture.gnome_record_mode,
            ))
            .with_virtual_is_platform(config.capture.gnome_virtual_is_platform);

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

        // Keep a clone for the LamcoRdpServer field: the accept dispatcher uses
        // it to drive per-connection session establishment/release. The local
        // `session_handle` is moved into clipboard setup further below.
        let session_handle_field = Arc::clone(&session_handle);

        // Save the stream active flag before session_handle is moved into clipboard setup.
        // This shared AtomicBool is read by Portal input methods and written by the
        // display handler when PipeWire stream state changes.
        let stream_active_flag = session_handle.stream_active_flag();

        // Watch for compositor D-Bus name disappearance (crash/restart detection)
        let _compositor_watcher = crate::health::compositor_watcher::start_compositor_watcher(
            session_handle.session_type(),
            health_reporter.clone(),
            shutdown_broadcast.subscribe(),
        )
        .await;

        info!("✅ Session created successfully via {}", strategy.name());

        // How video frames reach the display handler
        enum PipeWireSource {
            Fd(i32),
            Direct(std::sync::mpsc::Receiver<lamco_pipewire::frame::RawFrameData>),
        }

        // wlr-direct (virtual pointer/keyboard, no RemoteDesktop portal session):
        // acquire video via a standalone Portal ScreenCast. The libei strategy now
        // attaches ScreenCast to its own RemoteDesktop session (#51) and reports the
        // FD via pipewire_access(), so it uses the shared path below.
        let (pipewire_source, stream_info) =
            if matches!(session_handle.session_type(), SessionType::WlrDirect) {
                info!(
                    "{}: acquiring video via standalone Portal ScreenCast",
                    session_handle.session_type()
                );

                use ashpd::desktop::{
                    PersistMode,
                    screencast::{CursorMode, Screencast, SourceType as ScSourceType},
                };

                let screencast = Screencast::new()
                    .await
                    .context("Failed to connect to ScreenCast portal for input-only video")?;

                let sc_session = screencast
                    .create_session(ashpd::desktop::CreateSessionOptions::default())
                    .await
                    .context("Failed to create ScreenCast session for input-only video")?;

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
                debug!("Using cursor mode {:?} for ScreenCast", cursor_mode);

                use ashpd::desktop::screencast::SelectSourcesOptions;
                screencast
                    .select_sources(
                        &sc_session,
                        SelectSourcesOptions::default()
                            .set_cursor_mode(cursor_mode)
                            .set_sources(enumflags2::BitFlags::from(ScSourceType::Monitor))
                            .set_multiple(false)
                            .set_persist_mode(PersistMode::DoNot),
                    )
                    .await
                    .context("Failed to select ScreenCast sources for input-only video")?;

                let response = screencast
                    .start(
                        &sc_session,
                        None,
                        ashpd::desktop::screencast::StartCastOptions::default(),
                    )
                    .await
                    .context("Failed to start ScreenCast for input-only video")?
                    .response()
                    .context("ScreenCast start rejected by user")?;

                let portal_streams = response.streams();
                if portal_streams.is_empty() {
                    return Err(anyhow::anyhow!(
                        "No streams from ScreenCast for input-only video"
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
                            // Portal ScreenCast >= 5 only. Carried through so a
                            // stream can later be matched to the EIS device for
                            // the same monitor; nothing consumes it yet.
                            mapping_id: s.mapping_id().map(ToOwned::to_owned),
                        }
                    })
                    .collect();

                info!("ScreenCast started with {} stream(s)", streams.len());
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
                    .open_pipe_wire_remote(
                        &sc_session,
                        ashpd::desktop::screencast::OpenPipeWireRemoteOptions::default(),
                    )
                    .await
                    .context("Failed to open PipeWire remote for input-only video")?;

                use std::os::fd::AsRawFd;
                let raw_fd = fd.as_raw_fd();
                // Leak the OwnedFd so the PipeWire connection stays alive for the session.
                // Cleaned up when the server process exits.
                std::mem::forget(fd);

                info!("PipeWire FD: {}", raw_fd);

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

                (PipeWireSource::Fd(raw_fd), streams)
            } else {
                let strategy_streams = session_handle.streams();
                let portal_streams: Vec<crate::portal::StreamInfo> = strategy_streams
                    .iter()
                    .map(|s| crate::portal::StreamInfo {
                        node_id: s.node_id,
                        position: (s.position_x, s.position_y),
                        size: (s.width, s.height),
                        source_type: crate::portal::SourceType::Monitor,
                        // The strategy-level StreamInfo these come from does not
                        // carry a mapping id: the non-portal backends (Mutter
                        // direct, wlr) have no portal stream to read one from.
                        mapping_id: None,
                    })
                    .collect();

                match session_handle.pipewire_access() {
                    PipeWireAccess::FileDescriptor(fd) => {
                        info!("Using Portal-provided PipeWire file descriptor: {}", fd);
                        (PipeWireSource::Fd(fd), portal_streams)
                    }
                    PipeWireAccess::NodeId(node_id) => {
                        info!("Using Mutter-provided PipeWire node ID: {}", node_id);

                        let fd = crate::mutter::get_pipewire_fd_for_mutter()
                            .context("Failed to connect to PipeWire daemon for Mutter")?;

                        info!("Connected to PipeWire daemon, FD: {}", fd);
                        (PipeWireSource::Fd(fd), portal_streams)
                    }
                    PipeWireAccess::DirectChannel(rx) => {
                        info!("Using direct frame channel (bypassing PipeWire transport)");
                        (PipeWireSource::Direct(rx), portal_streams)
                    }
                }
            };

        // Self-sufficient strategies: skip Portal RemoteDesktop entirely.
        // ScreenCast-only = view-only (no input). WlrDirect = input via native Wayland protocols.
        // PortalGeneric = embedded wlroots video + input + clipboard (no Portal daemon needed).
        // All bypass the full-featured Portal RemoteDesktop path.
        if matches!(
            session_handle.session_type(),
            SessionType::ScreenCastOnly | SessionType::WlrDirect | SessionType::PortalGeneric
        ) {
            let is_wlr_direct = session_handle.session_type() == SessionType::WlrDirect;
            let is_portal_generic = session_handle.session_type() == SessionType::PortalGeneric;

            if is_portal_generic {
                info!("═══════════════════════════════════════════════════════════");
                info!("  PORTAL-GENERIC MODE (embedded wlroots backend)");
                info!("═══════════════════════════════════════════════════════════");
                info!("Native Wayland video + input + clipboard via portal-generic.");
                info!("No external Portal daemon required.");
                info!("═══════════════════════════════════════════════════════════");
            } else if is_wlr_direct {
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

            let force_avc420_only = false;
            let compression_mode = match config.egfx.zgfx_compression.to_lowercase().as_str() {
                "auto" => CompressionMode::Auto,
                "always" => CompressionMode::Always,
                _ => CompressionMode::Never,
            };
            let mut gfx_factory = LamcoGfxFactory::with_config(
                initial_size.0,
                initial_size.1,
                force_avc420_only,
                config.egfx.max_frames_in_flight,
                compression_mode,
            );
            gfx_factory.set_monitoring(Arc::clone(&metrics), snapshot_collector.egfx_state());
            gfx_factory.set_health_reporter(health_reporter.clone());

            // Register EGFX sensor with base signals. Version-gated QoE signal
            // becomes available after on_ready() updates EgfxSnapshot.negotiated_version.
            // The sensor reads live data from the shared EgfxSnapshot state.
            sensor_registry.register(Arc::new(crate::health::sensors::egfx::EgfxSensor::new(
                "pending",
                snapshot_collector.egfx_state(),
            )));

            // Register encoder sensor — reads from the shared encoder state.
            // Snapshot values populate after encoder creation in the pipeline loop.
            // Backend name updates when EncoderSnapshot is written by the active encoder.
            sensor_registry.register(Arc::new(
                crate::health::sensors::encoder::EncoderSensor::new(
                    "pending",
                    snapshot_collector.encoder_state(),
                ),
            ));

            let gfx_handler_state = gfx_factory.handler_state();
            let gfx_server_handle = gfx_factory.server_handle();

            // Server-side NetworkAutoDetect: the RDP server writes measured RTT
            // here; the EGFX FlowController reads it as a freshness floor, and the
            // server reports it to the client via a Network Characteristics Result.
            let autodetect_rtt = Arc::new(std::sync::atomic::AtomicU32::new(u32::MAX));
            if let Ok(mut fc) = gfx_handler_state.flow_controller.lock() {
                fc.set_autodetect_rtt_handle(Arc::clone(&autodetect_rtt));
            }
            let autodetect_probe_state = Arc::clone(&gfx_handler_state);

            let display_handler = Arc::new(match pipewire_source {
                PipeWireSource::Fd(raw_fd) => {
                    // SAFETY: fd from XDG Desktop Portal or PipeWire daemon.
                    // We take ownership here — only place we convert raw fd to OwnedFd.
                    let pipewire_fd = unsafe {
                        use std::os::fd::FromRawFd;
                        std::os::fd::OwnedFd::from_raw_fd(raw_fd)
                    };
                    // Request DMA-BUF only when compositor recommends it AND
                    // the GPU can actually provide CPU-readable DMA-BUF data.
                    // Non-Venus virtio-gpu (plain 2D, or virgl-3D without
                    // blob) returns all-zero mmap data because GPU memory
                    // uses non-linear tiling that CPU can't read. Venus-
                    // capable virtio-gpu (venus=on,blob=on on the host) can
                    // genuinely back CPU-readable DMA-BUF, so only force
                    // MemFd when the actual capset query says Venus is
                    // absent, not on driver name alone.
                    let rendering_recommends_software =
                        crate::capabilities::probes::rendering::is_display_gpu_virgl()
                            && !crate::capabilities::probes::rendering::is_display_gpu_venus_capable();
                    let use_dmabuf = !matches!(
                        capabilities.profile.recommended_buffer_type,
                        crate::compositor::BufferType::MemFd
                    ) && !rendering_recommends_software;
                    if rendering_recommends_software {
                        info!(
                            "Virtual GPU without Venus detected — forcing MemFd buffers (DMA-BUF mmap returns zeros)"
                        );
                    }
                    info!(
                        "Buffer type: {:?} (use_dmabuf={})",
                        capabilities.profile.recommended_buffer_type, use_dmabuf
                    );

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
                        use_dmabuf,
                        Arc::clone(&client_active_flag),
                    )
                    .await
                    .context("Failed to create display handler")?
                }
                PipeWireSource::Direct(raw_rx) => LamcoDisplayHandler::new_direct(
                    initial_size.0,
                    initial_size.1,
                    raw_rx,
                    stream_info.clone(),
                    Some(graphics_tx),
                    Some(gfx_server_handle),
                    Some(gfx_handler_state),
                    Arc::clone(&config),
                    Arc::clone(&service_registry),
                    Arc::clone(&client_active_flag),
                )
                .await
                .context("Failed to create display handler (direct channel)")?,
            });

            display_handler
                .set_health_reporter(health_reporter.clone())
                .await;
            display_handler
                .set_autodetect_rtt_handle(Arc::clone(&autodetect_rtt))
                .await;

            // Wire PipeWire sensor for version-adaptive health monitoring
            let pw_version = crate::runtime::diagnostics::get_pipewire_version()
                .and_then(|v| {
                    let parts: Vec<&str> = v.split('.').collect();
                    if parts.len() >= 3 {
                        Some((
                            parts[0].parse::<u32>().unwrap_or(0),
                            parts[1].parse::<u32>().unwrap_or(0),
                            parts[2].parse::<u32>().unwrap_or(0),
                        ))
                    } else {
                        None
                    }
                })
                .unwrap_or((0, 3, 0));
            let pw_sensor = Arc::new(crate::health::sensors::pipewire::PipeWireSensor::new(
                pw_version,
            ));
            sensor_registry
                .register(Arc::clone(&pw_sensor) as Arc<dyn crate::health::sensors::HealthSensor>);
            display_handler.set_pipewire_sensor(pw_sensor).await;

            // Wire EGFX snapshot for encoding adaptation feedback loop
            display_handler
                .set_egfx_snapshot(snapshot_collector.egfx_state())
                .await;

            // Wire FPS snapshot for D-Bus/GUI live-metrics reporting
            display_handler
                .set_fps_state(snapshot_collector.fps_state())
                .await;

            // Wire encoder-state sink so telemetry reports the active backend
            // (openh264/vaapi) once the pipeline creates the encoder.
            display_handler
                .set_encoder_state(snapshot_collector.encoder_state())
                .await;

            // Wire stream active flag for Portal input coupling
            if let Some(ref flag) = stream_active_flag {
                display_handler.set_stream_active_flag(Arc::clone(flag));
            }

            // Report subsystems that aren't wired in this code path
            if !is_wlr_direct && !is_portal_generic {
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
            let tls_acceptor = tokio_rustls::TlsAcceptor::from(tls_config.server_config());
            let tls_pub_key = tls_config.public_key().ok();

            let codecs = server_codecs_capabilities(&["remotefx"])
                .map_err(|e| anyhow::anyhow!("Failed to create codec capabilities: {e}"))?;

            let primary_stream_id = stream_info.first().map_or(0, |s| s.node_id);
            // Audio capture targets the default sink monitor (desktop output),
            // which lamco-pipewire selects via stream.capture.sink=true when no
            // node is given. The ScreenCast video node is not an audio node, so
            // it must not be passed here.
            let sound_factory = create_sound_factory(&config.audio, None);

            let listen_addr: SocketAddr = config
                .server
                .listen_addr
                .parse()
                .context("Invalid listen address")?;

            // Clipboard for self-sufficient strategies:
            // - wlr-direct: wl-clipboard-rs (data-control protocol)
            // - portal-generic: embedded DataControl backend from session handle
            type CliprdrFactory = Box<dyn ironrdp_server::CliprdrServerFactory>;
            let (wlr_clipboard_manager, wlr_clipboard_factory): (
                Option<Arc<Mutex<ClipboardOrchestrator>>>,
                Option<CliprdrFactory>,
            ) = if (is_wlr_direct || is_portal_generic) && config.clipboard.enabled {
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

                        // Wire the clipboard provider the strategy backs:
                        // portal-generic → its embedded data-control backend;
                        // wlr-direct → wl-clipboard data-control. Both are
                        // produced by the strategy's build_clipboard().
                        if let Some(provider) = session_handle
                            .build_clipboard(None, config.clipboard.rate_limit_ms)
                            .await
                        {
                            clipboard_mgr.set_clipboard_provider(provider).await;
                            info!("Clipboard provider wired from strategy");
                        } else {
                            warn!("Strategy provides no clipboard provider");
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

            if (is_wlr_direct || is_portal_generic) && wlr_clipboard_manager.is_none() {
                health_reporter.report(crate::health::HealthEvent::SubsystemNotAvailable {
                    subsystem: "clipboard".into(),
                });
            }

            let mut rdp_server = if is_wlr_direct || is_portal_generic {
                // wlr-direct/portal-generic: input via session handle (native Wayland protocols)
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

                let keyboard_layout_handler = KeyboardLayoutConnectionHandler::new(Arc::clone(
                    &input_handler.keyboard_handler,
                ));
                type RdpeiFactory = Box<dyn ironrdp_server::RdpeiServerFactory>;
                let rdpei_factory: Option<RdpeiFactory> = config.input.enable_touch.then(|| {
                    Box::new(create_rdpei_factory(input_handler.input_sender())) as RdpeiFactory
                });

                info!("wlr-direct input handler created (virtual keyboard + pointer)");

                // Resolve security. Standard RDP Security (no TLS) is required by
                // Hyper-V Enhanced Session Mode; otherwise Hybrid if configured and a
                // public key is available, else TLS.
                let addr_builder = RdpServer::builder().with_addr(listen_addr);
                let handler_builder = if is_standard_rdp_security(&config.security.security_mode) {
                    warn!(
                        "Security: Standard RDP Security (no TLS, no encryption) — accept only \
                         over hypervisor-isolated transports (vsock / Hyper-V ESM)"
                    );
                    addr_builder.with_no_security()
                } else if config.security.security_mode == "hybrid" {
                    if let Some(pub_key) = tls_pub_key {
                        info!("Configuring Hybrid security (NLA/CredSSP)");
                        addr_builder.with_hybrid(tls_acceptor, pub_key)
                    } else {
                        warn!("Hybrid requested but public key extraction failed, using TLS");
                        addr_builder.with_tls(tls_acceptor)
                    }
                } else {
                    addr_builder.with_tls(tls_acceptor)
                };

                handler_builder
                    .with_input_handler(input_handler)
                    .with_display_handler((*display_handler).clone())
                    .with_bitmap_codecs(codecs)
                    .with_cliprdr_factory(wlr_clipboard_factory)
                    .with_gfx_factory(Some(Box::new(gfx_factory)))
                    .with_sound_factory(Some(Box::new(sound_factory)))
                    .with_rdpei_factory(rdpei_factory)
                    .with_autodetect_rtt_handle(Arc::clone(&autodetect_rtt))
                    .with_connection_handler(Some(Box::new(keyboard_layout_handler)))
                    .build()
            } else {
                // ScreenCast-only: view-only, no input
                let addr_builder = RdpServer::builder().with_addr(listen_addr);
                let handler_builder = if is_standard_rdp_security(&config.security.security_mode) {
                    warn!(
                        "Security: Standard RDP Security (no TLS, no encryption) — accept only \
                         over hypervisor-isolated transports (vsock / Hyper-V ESM)"
                    );
                    addr_builder.with_no_security()
                } else if config.security.security_mode == "hybrid" {
                    if let Some(pub_key) = tls_pub_key {
                        info!("Configuring Hybrid security (NLA/CredSSP)");
                        addr_builder.with_hybrid(tls_acceptor, pub_key)
                    } else {
                        warn!("Hybrid requested but public key extraction failed, using TLS");
                        addr_builder.with_tls(tls_acceptor)
                    }
                } else {
                    addr_builder.with_tls(tls_acceptor)
                };

                handler_builder
                    .with_no_input()
                    .with_display_handler((*display_handler).clone())
                    .with_bitmap_codecs(codecs)
                    .with_cliprdr_factory(None)
                    .with_gfx_factory(Some(Box::new(gfx_factory)))
                    .with_sound_factory(Some(Box::new(sound_factory)))
                    .with_autodetect_rtt_handle(Arc::clone(&autodetect_rtt))
                    .build()
            };

            display_handler
                .set_server_event_sender(rdp_server.event_sender().clone())
                .await;

            // Phase 3: drive server-side auto-detect for this session.
            rdp_server.enable_autodetect();
            spawn_autodetect_probe(
                rdp_server.event_sender().clone(),
                autodetect_probe_state,
                shutdown_broadcast.subscribe(),
            );

            let _ = event_tx.send(ServerEvent::SessionTypeChanged {
                session_type: session_handle.session_type().to_string(),
            });

            let mode_name = if is_portal_generic {
                "portal-generic"
            } else if is_wlr_direct {
                "wlr-direct"
            } else {
                "view-only"
            };
            info!("{} server initialized successfully", mode_name);

            #[cfg(feature = "wayland")]
            display_handler.set_wayland_observers(color_observer.clone(), output_observer.clone());

            return Ok(Self {
                config,
                rdp_server,
                portal_manager: None,
                display_handler,
                session_handle: Arc::clone(&session_handle_field),
                service_registry,
                clipboard_manager: wlr_clipboard_manager,
                portal_session: None,
                shutdown_broadcast,
                event_tx,
                event_rx: Some(event_rx),
                health_subscriber: Some(health_subscriber),
                health_monitor_handle: Some(health_monitor_handle),
                metrics,
                snapshot_collector,
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

        // Wire clipboard based on what the strategy provides.
        // Strategies that bundle their own clipboard (Portal, Mutter, DataControl) need
        // no extra Portal session. Only strategies with ClipboardSource::None that still
        // want Portal clipboard (e.g., libei) get a separate Portal session here.
        use crate::session::strategy::ClipboardSource;

        let (
            portal_clipboard_manager,
            portal_clipboard_session,
            portal_session_valid,
            portal_input_handle,
        ) = match session_handle.clipboard_source() {
            ClipboardSource::Portal(components) => {
                // Strategy provides its own Portal session (input + clipboard).
                info!("Strategy provides Portal clipboard directly");
                (
                    components.manager,
                    Some(components.session),
                    components.session_valid,
                    Arc::clone(&session_handle),
                )
            }
            _ => {
                // Self-sufficient: Mutter (native clipboard), libei (data-control
                // clipboard + EIS input), ScreenCast (view-only). Input goes
                // through the strategy's own handle; the clipboard provider is
                // built later via build_clipboard(). No separate Portal session —
                // libei no longer mints a second one (wlr-direct and
                // portal-generic early-return before reaching this path).
                info!(
                    "Strategy '{}' is self-sufficient — no separate Portal session",
                    session_handle.session_type()
                );
                let session_valid = Arc::new(std::sync::atomic::AtomicBool::new(true));
                (None, None, session_valid, Arc::clone(&session_handle))
            }
        };

        // Portal RemoteDesktop path always uses fd-based PipeWire
        let pipewire_fd = match pipewire_source {
            PipeWireSource::Fd(raw_fd) => unsafe {
                use std::os::fd::FromRawFd;
                std::os::fd::OwnedFd::from_raw_fd(raw_fd)
            },
            PipeWireSource::Direct(_) => {
                unreachable!("DirectChannel only used with self-sufficient strategies")
            }
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

        let force_avc420_only = false;

        let compression_mode = match config.egfx.zgfx_compression.to_lowercase().as_str() {
            "auto" => CompressionMode::Auto,
            "always" => CompressionMode::Always,
            _ => CompressionMode::Never, // Default: no compression
        };
        info!("ZGFX compression mode: {:?}", compression_mode);

        let mut gfx_factory = LamcoGfxFactory::with_config(
            initial_size.0,
            initial_size.1,
            force_avc420_only,
            config.egfx.max_frames_in_flight,
            compression_mode,
        );
        gfx_factory.set_monitoring(Arc::clone(&metrics), snapshot_collector.egfx_state());
        gfx_factory.set_health_reporter(health_reporter.clone());

        // Register sensors for the Mutter direct path (same pattern as Portal path)
        sensor_registry.register(Arc::new(crate::health::sensors::egfx::EgfxSensor::new(
            "pending",
            snapshot_collector.egfx_state(),
        )));
        sensor_registry.register(Arc::new(
            crate::health::sensors::encoder::EncoderSensor::new(
                "pending",
                snapshot_collector.encoder_state(),
            ),
        ));

        let gfx_handler_state = gfx_factory.handler_state();
        let gfx_server_handle = gfx_factory.server_handle();

        // Server-side NetworkAutoDetect: the RDP server writes measured RTT here;
        // the EGFX FlowController reads it as a freshness floor, and the server
        // reports it to the client via a Network Characteristics Result.
        let autodetect_rtt = Arc::new(std::sync::atomic::AtomicU32::new(u32::MAX));
        if let Ok(mut fc) = gfx_handler_state.flow_controller.lock() {
            fc.set_autodetect_rtt_handle(Arc::clone(&autodetect_rtt));
        }
        let autodetect_probe_state = Arc::clone(&gfx_handler_state);

        if force_avc420_only {
            info!(
                "EGFX factory created for H.264/AVC420 streaming (AVC444 disabled by platform quirk)"
            );
        } else {
            info!("EGFX factory created for H.264/AVC420+AVC444 streaming");
        }

        let rendering_recommends_software =
            crate::capabilities::probes::rendering::is_display_gpu_virgl()
                && !crate::capabilities::probes::rendering::is_display_gpu_venus_capable();
        let use_dmabuf = !matches!(
            capabilities.profile.recommended_buffer_type,
            crate::compositor::BufferType::MemFd
        ) && !rendering_recommends_software;
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
                use_dmabuf,
                Arc::clone(&client_active_flag),
            )
            .await
            .context("Failed to create display handler")?,
        );

        display_handler
            .set_health_reporter(health_reporter.clone())
            .await;
        display_handler
            .set_autodetect_rtt_handle(Arc::clone(&autodetect_rtt))
            .await;

        // Wire PipeWire sensor for Mutter direct path
        {
            let pw_version = crate::runtime::diagnostics::get_pipewire_version()
                .and_then(|v| {
                    let parts: Vec<&str> = v.split('.').collect();
                    if parts.len() >= 3 {
                        Some((
                            parts[0].parse::<u32>().unwrap_or(0),
                            parts[1].parse::<u32>().unwrap_or(0),
                            parts[2].parse::<u32>().unwrap_or(0),
                        ))
                    } else {
                        None
                    }
                })
                .unwrap_or((0, 3, 0));
            let pw_sensor = Arc::new(crate::health::sensors::pipewire::PipeWireSensor::new(
                pw_version,
            ));
            sensor_registry
                .register(Arc::clone(&pw_sensor) as Arc<dyn crate::health::sensors::HealthSensor>);
            display_handler.set_pipewire_sensor(pw_sensor).await;

            // Wire EGFX snapshot for encoding adaptation feedback loop
            display_handler
                .set_egfx_snapshot(snapshot_collector.egfx_state())
                .await;

            // Wire FPS snapshot for D-Bus/GUI live-metrics reporting
            display_handler
                .set_fps_state(snapshot_collector.fps_state())
                .await;

            // Wire encoder-state sink so telemetry reports the active backend
            // (openh264/vaapi) once the pipeline creates the encoder.
            display_handler
                .set_encoder_state(snapshot_collector.encoder_state())
                .await;
        }

        // Wire stream active flag for Portal input coupling (reconnection path)
        if let Some(ref flag) = stream_active_flag {
            display_handler.set_stream_active_flag(Arc::clone(flag));
        }

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

        let keyboard_layout_handler =
            KeyboardLayoutConnectionHandler::new(Arc::clone(&input_handler.keyboard_handler));
        type RdpeiFactory = Box<dyn ironrdp_server::RdpeiServerFactory>;
        let rdpei_factory: Option<RdpeiFactory> = config
            .input
            .enable_touch
            .then(|| Box::new(create_rdpei_factory(input_handler.input_sender())) as RdpeiFactory);

        info!("Input handler created successfully");

        display_handler
            .set_input_handler(Arc::new(input_handler.clone()))
            .await;

        // Input is handled by input_handler's batching task;
        // multiplexer loop handles control/clipboard priorities
        tokio::spawn(multiplexer_loop::run_multiplexer_drain_loop(
            control_rx,
            clipboard_rx,
        ));
        info!("🚀 Full multiplexer drain loop started (control + clipboard priorities)");

        info!("Setting up TLS");
        let tls_config = TlsConfig::from_files_with_options(
            &config.security.cert_path,
            &config.security.key_path,
            config.security.require_tls_13,
        )
        .context("Failed to load TLS certificates")?;

        let tls_acceptor = tokio_rustls::TlsAcceptor::from(tls_config.server_config());
        let tls_pub_key = tls_config.public_key().ok();

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

            // Build the clipboard provider from the strategy's own source. The
            // WaylandDataControlMode IntegrationMode forces wl-clipboard
            // (data-control) regardless of the strategy's native source.
            let uses_data_control = matches!(
                clipboard_strategy,
                crate::clipboard::ClipboardIntegrationMode::WaylandDataControlMode { .. }
            );

            let provider: Option<Arc<dyn crate::clipboard::provider::ClipboardProvider>> =
                if uses_data_control {
                    #[cfg(feature = "wl-clipboard")]
                    {
                        info!("Clipboard provider: wl-clipboard-rs (data-control override)");
                        Some(
                            Arc::new(crate::clipboard::providers::WlClipboardProvider::new())
                                as Arc<dyn crate::clipboard::provider::ClipboardProvider>,
                        )
                    }
                    #[cfg(not(feature = "wl-clipboard"))]
                    {
                        session_handle_for_clipboard
                            .build_clipboard(None, config.clipboard.rate_limit_ms)
                            .await
                    }
                } else {
                    session_handle_for_clipboard
                        .build_clipboard(None, config.clipboard.rate_limit_ms)
                        .await
                };

            match provider {
                Some(p) => {
                    clipboard_mgr.set_clipboard_provider(p).await;
                    info!("Clipboard provider wired from strategy build_clipboard()");
                }
                None => info!("Strategy provides no clipboard provider"),
            }

            // Runtime health check: verify the data-control provider works.
            // Fall back to Portal if it fails and fallback_to_portal is enabled.
            if let crate::clipboard::ClipboardIntegrationMode::WaylandDataControlMode {
                fallback_to_portal,
                ..
            } = &clipboard_strategy
                && let Err(e) = clipboard_mgr.health_check_provider().await
            {
                warn!("Data-control clipboard health check failed: {e}");
                if *fallback_to_portal {
                    warn!("Falling back to Portal clipboard provider");
                    if let (Some(clipboard_mgr_arc), Some(session)) =
                        (&portal_clipboard_manager, &portal_clipboard_session)
                    {
                        let provider = crate::clipboard::providers::PortalClipboardProvider::new(
                            Arc::clone(clipboard_mgr_arc),
                            Arc::clone(session),
                            Arc::clone(&portal_session_valid),
                            config.clipboard.rate_limit_ms,
                        )
                        .await;
                        clipboard_mgr
                            .set_clipboard_provider(Arc::new(provider))
                            .await;
                        info!("Clipboard provider: Portal (fallback after health check failure)");
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

            // File transfer backend (FUSE/Staging) is selected and initialized
            // inside ClipboardOrchestrator::new() based on FileTransferMode::select().
            // No manual FUSE mount needed here.

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

        // Audio capture targets the default sink monitor (desktop output),
        // which lamco-pipewire selects via stream.capture.sink=true when no node
        // is given. The ScreenCast video node is not an audio node, so it must
        // not be passed here.
        let sound_factory = create_sound_factory(&config.audio, None);
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

        let addr_builder = RdpServer::builder().with_addr(listen_addr);
        let handler_builder = if is_standard_rdp_security(&config.security.security_mode) {
            warn!(
                "Security: Standard RDP Security (no TLS, no encryption) — accept only \
                 over hypervisor-isolated transports (vsock / Hyper-V ESM)"
            );
            addr_builder.with_no_security()
        } else if config.security.security_mode == "hybrid" {
            if let Some(pub_key) = tls_pub_key {
                info!("Configuring Hybrid security (NLA/CredSSP)");
                addr_builder.with_hybrid(tls_acceptor, pub_key)
            } else {
                warn!("Hybrid requested but public key extraction failed, using TLS");
                addr_builder.with_tls(tls_acceptor)
            }
        } else {
            addr_builder.with_tls(tls_acceptor)
        };

        let mut rdp_server = handler_builder
            .with_input_handler(input_handler)
            .with_display_handler((*display_handler).clone())
            .with_bitmap_codecs(codecs)
            .with_cliprdr_factory(Some(Box::new(clipboard_factory)))
            .with_gfx_factory(Some(Box::new(gfx_factory)))
            .with_sound_factory(Some(Box::new(sound_factory)))
            .with_rdpei_factory(rdpei_factory)
            .with_autodetect_rtt_handle(Arc::clone(&autodetect_rtt))
            .with_connection_handler(Some(Box::new(keyboard_layout_handler)))
            .build();

        display_handler
            .set_server_event_sender(rdp_server.event_sender().clone())
            .await;
        info!("Server event sender configured in display handler");

        // Phase 3: drive server-side auto-detect for this session.
        rdp_server.enable_autodetect();
        spawn_autodetect_probe(
            rdp_server.event_sender().clone(),
            autodetect_probe_state,
            shutdown_broadcast.subscribe(),
        );

        let _ = event_tx.send(ServerEvent::SessionTypeChanged {
            session_type: session_handle_for_clipboard.session_type().to_string(),
        });

        info!("Server initialized successfully");

        #[cfg(feature = "wayland")]
        display_handler.set_wayland_observers(color_observer.clone(), output_observer.clone());

        Ok(Self {
            config,
            rdp_server,
            portal_manager: Some(portal_manager),
            display_handler,
            session_handle: Arc::clone(&session_handle_field),
            service_registry,
            clipboard_manager: Some(clipboard_manager),
            portal_session: portal_clipboard_session,
            shutdown_broadcast,
            event_tx,
            event_rx: Some(event_rx),
            health_subscriber: Some(health_subscriber),
            health_monitor_handle: Some(health_monitor_handle),
            metrics,
            snapshot_collector,
            cleanup_done: false,
        })
    }

    /// Get the performance snapshot collector for monitoring consumers.
    ///
    /// Returns an Arc that can be shared with D-Bus manager, HTTP metrics
    /// server, and other consumers that need performance data.
    pub fn snapshot_collector(&self) -> Arc<crate::health::snapshot_collector::SnapshotCollector> {
        Arc::clone(&self.snapshot_collector)
    }

    /// Clone the health subscriber for external consumers (D-Bus manager).
    pub fn health_subscriber(&self) -> Option<HealthSubscriber> {
        self.health_subscriber.clone()
    }

    /// Get the core metrics collector for subsystems that need to record metrics.
    pub fn metrics(&self) -> Arc<crate::runtime::metrics::MetricsCollector> {
        Arc::clone(&self.metrics)
    }

    /// Run the server, blocking until shutdown.
    pub async fn run(mut self) -> Result<()> {
        let security_label = match self.config.security.security_mode.as_str() {
            "hybrid" => "Hybrid (NLA/CredSSP)",
            "auto" => "Auto",
            "rdp" | "none" => "Standard RDP (no TLS/encryption)",
            _ => "TLS",
        };

        if std::env::var("WAYLAND_DISPLAY").is_err() {
            warn!("WAYLAND_DISPLAY is not set - screen capture will not work");
            warn!(
                "Start the server from a Wayland graphical session or set WAYLAND_DISPLAY manually"
            );
        }

        info!("╔════════════════════════════════════════════════════════════╗");
        info!("║          Server Starting                                   ║");
        info!("╚════════════════════════════════════════════════════════════╝");
        info!("  Listen Address: {}", self.config.server.listen_addr);
        info!("  Security: {} (rustls 0.23)", security_label);
        let codec_label = if self.config.egfx.enabled {
            "EGFX (H.264 AVC420/AVC444) + RemoteFX fallback"
        } else {
            "RemoteFX"
        };
        info!("  Codec: {codec_label}");
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

        // IronRDP needs credentials for the protocol handshake.
        // For Hybrid/NLA mode, CredSSP requires valid credentials to complete
        // the NTLM challenge-response exchange — the acceptor errors out with
        // "no credentials while doing credssp" if creds are None.
        let use_hybrid =
            resolve_security_mode(&self.config.security.security_mode, effective_auth_method);

        // Credential resolution:
        //   credssp_credentials present → use them (required for hybrid without PAM)
        //   auth_method=pam → PamValidator (set below) handles validation post-CredSSP
        //   otherwise → None (only valid for tls-only mode)
        let initial_creds = self.config.security.credssp_credentials.as_ref().map(|c| {
            ironrdp_server::Credentials {
                username: c.username.clone(),
                password: c.password.clone(),
                domain: c.domain.clone(),
            }
        });
        if let Some(creds) = self.config.security.credssp_credentials.as_ref() {
            info!(
                "Pre-loaded CredSSP credentials from config (user: {})",
                creds.username
            );
        }
        self.rdp_server.set_credentials(initial_creds);

        // Set up PAM credential validator if auth_method=pam
        let pam_validator = if effective_auth_method == "pam" {
            let validator = std::sync::Arc::new(crate::security::PamValidator::new(None));
            self.rdp_server
                .set_credential_validator(Some(validator.clone()));
            info!("PAM credential validator attached to RDP server");
            Some(validator)
        } else {
            None
        };

        if use_hybrid {
            info!("Security mode: Hybrid (NLA/CredSSP)");
            if self.config.security.credssp_credentials.is_none() && effective_auth_method != "pam"
            {
                warn!(
                    "Hybrid mode active but no credssp_credentials configured — \
                     clients will fail with 'no credentials while doing credssp'. \
                     Set [security].credssp_credentials in config, or use D-Bus/GUI \
                     to set credentials before clients connect."
                );
            }
        } else if is_standard_rdp_security(&self.config.security.security_mode) {
            info!("Security mode: Standard RDP Security (no TLS/encryption)");
        } else {
            info!("Security mode: TLS");
        }

        if effective_auth_method != configured_auth {
            info!(
                "Authentication: {} (configured: {}, fallback due to deployment)",
                effective_auth_method, configured_auth
            );
        } else {
            info!("Authentication: {}", effective_auth_method);
        }

        // Exposure guard (defense-in-depth, mirrors the qemu console's startup
        // refusal): an unauthenticated listener on a routable address serves RDP
        // to anyone who can reach the port. The desktop product still gates
        // capture interactively via the Portal, so this warns loudly rather than
        // refusing — but auth_method=none on a non-loopback bind is rarely
        // intended outside a trusted network.
        if effective_auth_method == "none"
            && let Ok(addr) = self.config.server.listen_addr.parse::<SocketAddr>()
            && !addr.ip().is_loopback()
        {
            warn!(
                "⚠️  Unauthenticated RDP (auth_method=none) on routable address {} — anyone who \
                 can reach this port can connect. Set auth_method=pam, configure \
                 credssp_credentials, or bind to localhost unless this is a trusted network.",
                addr
            );
        }

        // Phase 1 of the unified transport accept layer, retrofit 2026-05-16
        // to use the AcceptDeployment trait pattern.
        //
        // WlrDirectDeployment encapsulates the per-binary differences (TOML
        // transports config, mpsc D-Bus event sink, PAM validator, broadcast
        // shutdown, Portal-validity closure). AcceptDispatcher consumes the
        // trait and stays binary-agnostic.
        //
        // See:
        // - docs/design/transport/TRANSPORT-PHASE-1-SDS-2026-05-16.md
        // - docs/design/transport/TRANSPORT-PHASE-1-RETROFIT-SDS-2026-05-16.md
        let deployment = deployment::WlrDirectDeployment::new(
            self.config.clone(),
            self.display_handler.clone(),
            self.health_subscriber.clone(),
            self.event_tx.clone(),
            pam_validator.clone(),
            self.shutdown_broadcast.clone(),
            Arc::clone(&self.session_handle),
        );

        let result =
            crate::transport::AcceptDispatcher::run(deployment, &mut self.rdp_server).await;

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
    /// Returns `true` if the server can accept another client. Video/input failures
    /// return `true` because the display pipeline reinitializes per-connection.
    /// Returns `false` only when the Portal session itself was destroyed by the
    /// compositor — the D-Bus session object is gone and can't be recreated.
    async fn on_disconnect(&self) -> bool {
        perform_disconnect_cleanup(&self.display_handler, self.health_subscriber.as_ref(), true)
            .await
    }
}

/// Standalone cleanup logic shared between `LamcoRdpServer::on_disconnect` and
/// the closure captured by `transport::LamcoConnectionHandler` (which needs to
/// call this without holding a reference to the full server struct).
///
/// Returns `false` only when the Portal session was destroyed by the compositor
/// — the D-Bus session object is gone and can't be recreated.
pub(crate) async fn perform_disconnect_cleanup(
    display_handler: &LamcoDisplayHandler,
    health_subscriber: Option<&HealthSubscriber>,
    served: bool,
) -> bool {
    if served {
        info!("Client disconnected - performing cleanup");

        // Stop the pipeline from encoding/sending frames to a dead channel.
        // PipeWire frames are still drained to keep the stream responsive,
        // but no CPU is wasted on encoding or queue pressure.
        display_handler.on_client_disconnect();

        // Drive the clipboard connection-lifecycle teardown: clear the Ready
        // latch, drop per-connection state, and release any local clipboard
        // ownership held on the now-gone remote's behalf.
        display_handler.notify_clipboard_disconnect().await;
    } else {
        // A connection that never served (a fast handshake-failure client probe)
        // must NOT pause the pipeline or tear down clipboard. The real client can
        // be actively served on an overlapping connection, and pausing it here is
        // exactly what left frame processing stuck (frames captured, none sent)
        // after a reconnect.
        debug!("Unserved/probe disconnect — skipping pipeline pause and clipboard teardown");
    }

    // Check health state to decide whether this server instance can accept
    // another client. Only session destruction (compositor closed the Portal
    // session) is truly fatal — the D-Bus session object is gone and can't be
    // recreated without user interaction. Video/input failures are recoverable:
    // a new client connection restarts the display pipeline.
    if let Some(subscriber) = health_subscriber {
        let health = subscriber.current();

        if health.session.is_failed() {
            // Session destroyed by compositor — irrecoverable without restart
            error!("Portal session destroyed — cannot accept new clients");
            error!("  session: {}", health.session);
            error!("  video: {}", health.video);
            error!("  input: {}", health.input);
            error!("  clipboard: {}", health.clipboard);
            return false;
        }

        match health.overall {
            crate::health::OverallHealth::Invalid => {
                // Subsystem failure (video/input) but session is alive.
                // The next client connection will reinitialize the display
                // pipeline, so we can accept another connection.
                warn!(
                    "Session health is invalid (subsystem failure) but Portal session is alive — accepting new clients"
                );
                warn!("  video: {}", health.video);
                warn!("  input: {}", health.input);
                warn!("  clipboard: {}", health.clipboard);
            }
            crate::health::OverallHealth::Degraded => {
                warn!("Session health is degraded — will accept new clients cautiously");
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

impl Drop for LamcoRdpServer {
    fn drop(&mut self) {
        info!("LamcoRdpServer dropping - initiating cleanup");

        // cleanup_resources() is async but Drop is sync. block_in_place moves this
        // thread out of the tokio worker pool so block_on won't panic.
        match tokio::runtime::Handle::try_current() {
            Ok(handle) => {
                tokio::task::block_in_place(|| {
                    handle.block_on(async {
                        match self.cleanup_resources().await {
                            Err(e) => {
                                error!("Error during cleanup: {:#}", e);
                            }
                            _ => {
                                info!("Cleanup completed successfully");
                            }
                        }
                    });
                });
            }
            _ => {
                warn!("No tokio runtime available for cleanup - resources may leak");
            }
        }
    }
}

/// Standard RDP Security ("rdp"/"none"): no TLS and no RDP-layer encryption.
///
/// Hyper-V Enhanced Session Mode requires this — VMConnect speaks plaintext RDP
/// (`PROTOCOL_RDP`) over the hypervisor-isolated vsock transport and never sends a
/// TLS ClientHello. Only safe on isolated transports (vsock) or loopback.
pub(crate) fn is_standard_rdp_security(security_mode: &str) -> bool {
    matches!(security_mode, "rdp" | "none")
}

/// Resolve the effective security mode from config.
///
/// "auto" resolves to "hybrid" when credentials are available (auth != "none"),
/// "tls" otherwise. Explicit "hybrid" or "tls" pass through.
fn resolve_security_mode(security_mode: &str, effective_auth_method: &str) -> bool {
    match security_mode {
        "hybrid" => true,
        "auto" => effective_auth_method != "none",
        _ => false, // "tls" or unknown
    }
}

/// Check if a port is available before attempting to bind.
///
/// Uses a standard TCP connect probe and /proc/net/tcp inspection to detect
/// whether the port is already in use and, if possible, identify the process
/// holding it.
pub(crate) fn check_port_available(addr: &std::net::SocketAddr) {
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
            if let Some(local_port) = local_addr.split(':').nth(1)
                && local_port == port_hex
            {
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
                if let Ok(link) = std::fs::read_link(fd.path())
                    && link.to_string_lossy() == target
                {
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
