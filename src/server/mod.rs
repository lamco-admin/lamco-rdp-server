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
//! ```no_run
//! use wrd_server::config::Config;
//! use wrd_server::server::WrdServer;
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

mod display_handler;
mod egfx_sender;
mod event_multiplexer;
mod gfx_factory;
mod graphics_drain;
mod input_handler;
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

    /// Portal manager for Wayland access (kept for resource cleanup)
    #[expect(
        dead_code,
        reason = "Arc kept alive for portal resource cleanup on drop"
    )]
    portal_manager: Arc<PortalManager>,

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
                    info!("📋 Increasing portal timeout for slow permissions");
                    // Note: Portal timeout is handled via capabilities.profile.portal_timeout_ms
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
                crate::compositor::Quirk::Avc444Unreliable => {
                    info!("📋 AVC444 disabled due to platform quirk (forcing AVC420 only)");
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

        info!("Selecting session strategy based on detected capabilities");

        let strategy_selector = SessionStrategySelector::with_keyboard_layout(
            service_registry.clone(),
            Arc::new(token_manager),
            config.input.keyboard_layout.clone(),
        );

        let strategy = strategy_selector
            .select_strategy()
            .await
            .context("Failed to select session strategy")?;

        info!("🎯 Selected strategy: {}", strategy.name());

        info!("Creating session via selected strategy");
        let session_handle = strategy
            .create_session()
            .await
            .context("Failed to create session via strategy")?;

        info!("✅ Session created successfully via {}", strategy.name());

        let (pipewire_fd, stream_info) = match session_handle.pipewire_access() {
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
        };

        let mut portal_config = config.to_portal_config();
        portal_config.persist_mode = ashpd::desktop::PersistMode::DoNot; // Don't persist (causes errors)
        portal_config.restore_token = None;

        let portal_manager = Arc::new(
            PortalManager::new(portal_config)
                .await
                .context("Failed to create Portal manager for input+clipboard")?,
        );

        // Get clipboard components from session handle, or create fallback Portal session
        // HYBRID STRATEGY: For Mutter, we also use Portal session for input (Mutter input broken on GNOME 46)
        let (portal_clipboard_manager, portal_clipboard_session, portal_input_handle) =
            if session_handle.session_type() == SessionType::Portal {
                // Portal strategy: use session_handle directly (no duplicate sessions)
                info!("Portal strategy: using session_handle directly");

                // Portal strategy always provides ClipboardComponents
                // manager may be None on Portal v1, but session is always present
                let clipboard_components = session_handle
                    .portal_clipboard()
                    .expect("Portal strategy always provides ClipboardComponents");

                let clipboard_mgr = clipboard_components.manager; // Option<Arc<...>>
                let session = clipboard_components.session; // Always present

                (clipboard_mgr, session, session_handle)
            } else {
                // Mutter strategy: Need separate Portal session for input AND clipboard (one dialog)
                // HYBRID: Mutter provides video (zero dialogs), Portal provides input+clipboard (one dialog)
                info!("Strategy doesn't provide clipboard, creating separate Portal session for input+clipboard");
                info!("HYBRID MODE: Mutter for video (zero dialogs), Portal for input+clipboard (one dialog)");

                // CRITICAL: Create clipboard manager BEFORE session creation
                // Clipboard must be requested during session setup, not after
                let clipboard_mgr = if capabilities.portal.supports_clipboard {
                    match lamco_portal::ClipboardManager::new().await {
                        Ok(mgr) => {
                            info!("Portal clipboard manager created (will be used during session creation)");
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

                info!("Separate Portal session created for input+clipboard (non-persistent)");

                let session = Arc::new(RwLock::new(portal_handle.session));

                // Create PortalSessionHandleImpl for input
                // If no clipboard (Portal v1), just use Mutter session_handle for input instead
                // Create Portal input handle regardless of clipboard availability
                let input_handle =
                    crate::session::strategies::PortalSessionHandleImpl::from_portal_session(
                        session.clone(),
                        portal_manager.remote_desktop().clone(),
                        clipboard_mgr.clone(), // Pass Option directly
                    );

                (
                    clipboard_mgr,
                    session,
                    Arc::new(input_handle) as Arc<dyn crate::session::SessionHandle>,
                )
            };

        info!(
            "Session started with {} streams, PipeWire FD: {}",
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

        // Avc444Unreliable quirk: Force AVC420 only (e.g., RHEL 9 blur issue)
        let force_avc420_only = capabilities
            .profile
            .has_quirk(&crate::compositor::Quirk::Avc444Unreliable);

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

        let (shutdown_broadcast, _) = tokio::sync::broadcast::channel(16);
        let shutdown_broadcast = Arc::new(shutdown_broadcast);

        // HYBRID: For Mutter strategy, uses Portal for input while Mutter handles video
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

        // Check for KDE Portal Clipboard bug (Bug 515465)
        // All current KDE versions (6.3.90-6.5.x) have threading bugs in Portal Clipboard
        // Disable clipboard entirely on KDE until upstream fix (v6.6+, NOT backported to 6.5.x)
        let kde_clipboard_disabled = matches!(
            capabilities.compositor,
            crate::compositor::CompositorType::Kde { .. }
        );

        let clipboard_manager = if config.clipboard.enabled && !kde_clipboard_disabled {
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

            if let Some(clipboard_mgr_arc) = portal_clipboard_manager {
                clipboard_mgr
                    .set_portal_clipboard(clipboard_mgr_arc, Arc::clone(&portal_clipboard_session))
                    .await;
            } else {
                info!("Clipboard: no Portal clipboard manager available");
            }

            let clipboard_strategy = crate::clipboard::ClipboardIntegrationMode::select(
                &service_registry,
                &config.clipboard,
                is_flatpak(),
            );

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
            if kde_clipboard_disabled {
                warn!("═══════════════════════════════════════════════════════════");
                warn!("  Clipboard DISABLED on KDE (Portal Bug 515465)");
                warn!("═══════════════════════════════════════════════════════════");
                warn!("KDE Portal Clipboard has critical threading bugs causing crashes.");
                warn!("Bug: https://bugs.kde.org/show_bug.cgi?id=515465");
                warn!("Status: Fix landed in KDE 6.6 (releases Feb 17, 2026)");
                warn!("Note: Fix NOT backported to 6.5.x branch.");
                warn!("");
                warn!("Video and input work perfectly - only clipboard affected.");
                warn!("Clipboard will be available after upgrading to KDE 6.6+.");
                warn!("═══════════════════════════════════════════════════════════");
            } else {
                info!("Clipboard disabled by configuration");
            }
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

        // TODO: Get audio node ID from portal session for targeted capture
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

        info!("Server initialized successfully");

        Ok(Self {
            config,
            rdp_server,
            portal_manager,
            display_handler,
            service_registry,
            clipboard_manager: Some(clipboard_manager),
            portal_session: Some(portal_clipboard_session),
            shutdown_broadcast,
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

        // Race rdp_server.run() against our shutdown broadcast.
        // IronRDP's Quit event only closes the active connection — its outer accept
        // loop continues waiting for new clients. The broadcast breaks our select.
        let mut shutdown_rx = self.shutdown_broadcast.subscribe();
        let result = tokio::select! {
            result = self.rdp_server.run() => result.context("RDP server error"),
            _ = shutdown_rx.recv() => {
                info!("Shutdown broadcast received — stopping server");
                Ok(())
            }
        };

        if let Err(ref e) = result {
            error!("Server stopped with error: {:#}", e);
        } else {
            info!("Server stopped gracefully");
        }

        info!("Performing post-run cleanup...");
        if let Err(e) = self.on_disconnect().await {
            warn!("Disconnect cleanup failed: {:#}", e);
        }

        if let Err(e) = self.cleanup_resources().await {
            warn!("Resource cleanup failed: {:#}", e);
        }

        result
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
    async fn on_disconnect(&self) -> Result<()> {
        info!("Client disconnected - performing cleanup");

        if let Some(clipboard_arc) = &self.clipboard_manager {
            info!("  Clearing Portal clipboard...");
            let clipboard = clipboard_arc.lock().await;
            if let Err(e) = clipboard.clear_portal_clipboard().await {
                warn!("  Failed to clear clipboard: {}", e);
            } else {
                info!("  ✅ Portal clipboard cleared");
            }
        }

        info!("✅ Disconnect cleanup complete");
        Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[ignore] // Requires D-Bus and portal access
    async fn test_server_initialization() {
        // This test would require a full environment
        // For now, just verify compilation
    }
}
