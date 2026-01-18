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

pub use display_handler::LamcoDisplayHandler;
pub use egfx_sender::{EgfxFrameSender, SendError};
pub use gfx_factory::{HandlerState, LamcoGfxFactory, SharedHandlerState};
pub use input_handler::LamcoInputHandler;

use anyhow::{Context, Result};
use ironrdp_pdu::rdp::capability_sets::server_codecs_capabilities;
use ironrdp_server::{Credentials, RdpServer};
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::{Mutex, RwLock};
use tracing::{debug, error, info, warn};

use crate::clipboard::{ClipboardConfig, ClipboardManager, LamcoCliprdrFactory};
use crate::config::Config;
use crate::input::MonitorInfo as InputMonitorInfo;
use crate::portal::PortalManager;
use crate::security::TlsConfig;
use crate::services::{ServiceId, ServiceLevel, ServiceRegistry};
use crate::session::{PipeWireAccess, SessionStrategySelector, SessionType};

/// WRD Server
///
/// Main server struct that orchestrates all subsystems and integrates
/// with IronRDP for RDP protocol handling.
pub struct LamcoRdpServer {
    /// Configuration (kept for future dynamic reconfiguration)
    #[allow(dead_code)]
    config: Arc<Config>,

    /// IronRDP server instance
    rdp_server: RdpServer,

    /// Portal manager for Wayland access (kept for resource cleanup)
    #[allow(dead_code)]
    portal_manager: Arc<PortalManager>,

    /// Display handler (kept for lifecycle management)
    #[allow(dead_code)]
    display_handler: Arc<LamcoDisplayHandler>,
}

impl LamcoRdpServer {
    /// Create a new server instance
    ///
    /// # Arguments
    ///
    /// * `config` - Server configuration
    ///
    /// # Returns
    ///
    /// A new `WrdServer` instance ready to run
    pub async fn new(config: Config) -> Result<Self> {
        info!("Initializing server");
        let config = Arc::new(config);

        // === CAPABILITY PROBING ===
        // Detect compositor and adapt configuration automatically
        info!("Probing compositor capabilities...");
        let capabilities = crate::compositor::probe_capabilities()
            .await
            .context("Failed to probe compositor capabilities")?;

        // Apply quirks based on detected profile
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

        // === SESSION PERSISTENCE SETUP ===
        // Detect deployment context and credential storage
        info!("Detecting deployment context and credential storage...");

        let deployment = crate::session::detect_deployment_context();
        info!("📦 Deployment: {}", deployment);

        let (storage_method, encryption, accessible) =
            crate::session::detect_credential_storage(&deployment).await;
        info!(
            "🔐 Credential Storage: {} (encryption: {}, accessible: {})",
            storage_method, encryption, accessible
        );

        // Create TokenManager for session persistence
        let token_manager = crate::session::TokenManager::new(storage_method)
            .await
            .context("Failed to create TokenManager")?;

        // Try to load existing restore token
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

        // === SERVICE ADVERTISEMENT ===
        // Translate compositor capabilities into advertised services
        let service_registry = Arc::new(ServiceRegistry::from_compositor(capabilities.clone()));
        service_registry.log_summary();

        // Log service-aware premium feature decisions
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

        // === SESSION STRATEGY SELECTION ===
        // Select best strategy based on detected capabilities
        info!("Selecting session strategy based on detected capabilities");

        let strategy_selector =
            SessionStrategySelector::new(service_registry.clone(), Arc::new(token_manager));

        let strategy = strategy_selector
            .select_strategy()
            .await
            .context("Failed to select session strategy")?;

        info!("🎯 Selected strategy: {}", strategy.name());

        // Create session via selected strategy
        info!("Creating session via selected strategy");
        let session_handle = strategy
            .create_session()
            .await
            .context("Failed to create session via strategy")?;

        info!("✅ Session created successfully via {}", strategy.name());

        // Extract session details and handle different PipeWire access methods
        let (pipewire_fd, stream_info) = match session_handle.pipewire_access() {
            PipeWireAccess::FileDescriptor(fd) => {
                // Portal path: FD directly provided
                info!("Using Portal-provided PipeWire file descriptor: {}", fd);

                // Convert strategy StreamInfo to portal StreamInfo format
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
                // Mutter path: Node ID provided, need to connect to PipeWire daemon
                info!("Using Mutter-provided PipeWire node ID: {}", node_id);

                let fd = crate::mutter::get_pipewire_fd_for_mutter()
                    .context("Failed to connect to PipeWire daemon for Mutter")?;

                info!("Connected to PipeWire daemon, FD: {}", fd);

                // Convert strategy StreamInfo to portal StreamInfo format
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

        // Create Portal manager for input+clipboard (needed for both strategies)
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

                let session_id = format!("lamco-rdp-input-clipboard-{}", uuid::Uuid::new_v4());
                let (portal_handle, _) = portal_manager
                    .create_session(session_id, None)
                    .await
                    .context("Failed to create Portal session for input+clipboard")?;

                // Only create clipboard if Portal supports it (v2+)
                let clipboard_mgr = if capabilities.portal.supports_clipboard {
                    Some(Arc::new(
                        lamco_portal::ClipboardManager::new()
                            .await
                            .context("Failed to create Portal clipboard manager")?,
                    ))
                } else {
                    info!(
                        "Skipping clipboard creation - Portal v{} doesn't support clipboard",
                        capabilities.portal.version
                    );
                    None
                };

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

        // Determine initial desktop size from first stream
        let initial_size = stream_info
            .first()
            .map(|s| (s.size.0 as u16, s.size.1 as u16))
            .unwrap_or((1920, 1080)); // Default fallback

        info!(
            "Initial desktop size: {}x{}",
            initial_size.0, initial_size.1
        );

        // Create ALL 4 multiplexer queues (full implementation)
        let (input_tx, input_rx) = tokio::sync::mpsc::channel(32); // Priority 1: Input
        let (_control_tx, control_rx) = tokio::sync::mpsc::channel(16); // Priority 2: Control
        let (_clipboard_tx, clipboard_rx) = tokio::sync::mpsc::channel(8); // Priority 3: Clipboard
        let (graphics_tx, graphics_rx) = tokio::sync::mpsc::channel(64); // Priority 4: Graphics - increased for frame coalescing
        info!("📊 Full multiplexer queues created:");
        info!("   Input queue: 32 (Priority 1 - never starve)");
        info!("   Control queue: 16 (Priority 2 - session critical)");
        info!("   Clipboard queue: 8 (Priority 3 - user operations)");
        info!("   Graphics queue: 64 (Priority 4 - damage region coalescing)");

        // Create EGFX/H.264 factory for video streaming BEFORE display handler
        // This enables hardware-accelerated H.264 encoding when client supports it
        //
        // Check for platform quirks that affect codec selection:
        // - Avc444Unreliable: Force AVC420 only (e.g., RHEL 9 blur issue)
        let force_avc420_only = capabilities
            .profile
            .has_quirk(&crate::compositor::Quirk::Avc444Unreliable);
        let gfx_factory = LamcoGfxFactory::with_quirks(
            initial_size.0 as u16,
            initial_size.1 as u16,
            force_avc420_only,
        );
        // Get shared references BEFORE passing factory to builder
        let gfx_handler_state = gfx_factory.handler_state();
        let gfx_server_handle = gfx_factory.server_handle();
        if force_avc420_only {
            info!("EGFX factory created for H.264/AVC420 streaming (AVC444 disabled by platform quirk)");
        } else {
            info!("EGFX factory created for H.264/AVC420+AVC444 streaming");
        }

        // Create display handler with PipeWire FD, stream info, graphics queue, and EGFX references
        let display_handler = Arc::new(
            LamcoDisplayHandler::new(
                initial_size.0,
                initial_size.1,
                pipewire_fd,
                stream_info.to_vec(), // streams() returns &[StreamInfo], convert to Vec
                Some(graphics_tx),    // Graphics queue for multiplexer
                Some(gfx_server_handle), // EGFX server handle for H.264 frame sending
                Some(gfx_handler_state), // EGFX handler state for readiness checks
                Arc::clone(&config),  // Pass config for feature flags
                Arc::clone(&service_registry), // Service registry for feature decisions
            )
            .await
            .context("Failed to create display handler")?,
        );

        // Start the graphics drain task
        let update_sender = display_handler.get_update_sender();
        let _graphics_drain_handle =
            graphics_drain::start_graphics_drain_task(graphics_rx, update_sender);
        info!("Graphics drain task started");

        // Start the display pipeline
        Arc::clone(&display_handler).start_pipeline();

        // Create input handler for mouse and keyboard injection
        info!("Creating input handler for mouse/keyboard control");

        // Convert stream info to monitor info for coordinate transformation
        let monitors: Vec<InputMonitorInfo> = stream_info
            .iter()
            .enumerate()
            .map(|(idx, stream)| InputMonitorInfo {
                id: idx as u32,
                name: format!("Monitor {}", idx),
                x: stream.position.0 as i32,
                y: stream.position.1 as i32,
                width: stream.size.0 as u32,
                height: stream.size.1 as u32,
                dpi: 96.0,         // Default DPI
                scale_factor: 1.0, // Default scale, Portal doesn't provide this
                stream_x: stream.position.0 as u32,
                stream_y: stream.position.1 as u32,
                stream_width: stream.size.0 as u32,
                stream_height: stream.size.1 as u32,
                is_primary: idx == 0, // First monitor is primary
            })
            .collect();

        // Get the primary stream node ID for Portal input injection
        let primary_stream_id = stream_info.first().map(|s| s.node_id).unwrap_or(0);

        info!(
            "Using PipeWire stream node ID {} for input injection",
            primary_stream_id
        );

        // Create input handler using Portal session handle (works correctly)
        // HYBRID: For Mutter strategy, uses Portal for input while Mutter handles video
        let input_handler = LamcoInputHandler::new(
            portal_input_handle, // Use Portal session for input (works on all DEs)
            monitors.clone(),
            primary_stream_id,
            input_tx.clone(), // Multiplexer input queue sender (for handler callbacks)
            input_rx,         // Multiplexer input queue receiver (for batching task)
        )
        .context("Failed to create input handler")?;

        info!("Input handler created successfully - mouse/keyboard enabled via Portal");

        // Start full multiplexer drain loop
        // Note: Input queue is handled by input_handler's batching task
        // Multiplexer loop handles control/clipboard priorities
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

        // Create TLS acceptor from security config
        info!("Setting up TLS");
        let tls_config =
            TlsConfig::from_files(&config.security.cert_path, &config.security.key_path)
                .context("Failed to load TLS certificates")?;

        let tls_acceptor =
            ironrdp_server::tokio_rustls::TlsAcceptor::from(tls_config.server_config());

        // Configure RemoteFX codec (IronRDP's built-in codec)
        // Server uses "remotefx" string to enable RemoteFX codec (default enabled)
        let codecs = server_codecs_capabilities(&["remotefx"])
            .map_err(|e| anyhow::anyhow!("Failed to create codec capabilities: {}", e))?;

        // Create clipboard manager
        info!("Initializing clipboard manager");
        let clipboard_config = ClipboardConfig::default();
        let mut clipboard_mgr = ClipboardManager::new(clipboard_config)
            .await
            .context("Failed to create clipboard manager")?;

        // Set Portal clipboard reference if available (from session or fallback)
        if let Some(clipboard_mgr_arc) = portal_clipboard_manager {
            clipboard_mgr
                .set_portal_clipboard(clipboard_mgr_arc, Arc::clone(&portal_clipboard_session))
                .await;
            // Note: Success message logged inside set_portal_clipboard
        } else {
            info!("Clipboard disabled - no Portal clipboard manager available");
        }

        // Mount FUSE filesystem for clipboard file transfer
        // This enables on-demand file streaming for Windows → Linux file copy
        if let Err(e) = clipboard_mgr.mount_fuse().await {
            warn!("Failed to mount FUSE clipboard filesystem: {:?}", e);
            warn!("File clipboard will use staging fallback (download files upfront)");
        }

        let clipboard_manager = Arc::new(Mutex::new(clipboard_mgr));

        // Create clipboard factory for IronRDP
        // Factory automatically starts event bridge task internally
        let clipboard_factory = LamcoCliprdrFactory::new(Arc::clone(&clipboard_manager));

        // Note: gfx_factory was created earlier (before display handler)
        // to share references with display handler

        // Build IronRDP server using builder pattern
        info!("Building IronRDP server");
        let listen_addr: SocketAddr = config
            .server
            .listen_addr
            .parse()
            .context("Invalid listen address")?;

        // Build RDP server
        let rdp_server = RdpServer::builder()
            .with_addr(listen_addr)
            .with_tls(tls_acceptor)
            .with_input_handler(input_handler)
            .with_display_handler((*display_handler).clone())
            .with_bitmap_codecs(codecs)
            .with_cliprdr_factory(Some(Box::new(clipboard_factory)))
            .with_gfx_factory(Some(Box::new(gfx_factory)))
            .build();

        // Set server event sender in display handler for EGFX message routing
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
        })
    }

    /// Run the server
    ///
    /// This starts the RDP server and handles incoming connections.
    /// Blocks until the server is shut down.
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

        // Set credentials for RDP authentication
        // Even with auth_method="none", we need to set empty/test credentials
        // for IronRDP to complete the protocol handshake properly
        let credentials = if self.config.security.auth_method == "none" {
            Some(Credentials {
                username: String::new(),
                password: String::new(),
                domain: None,
            })
        } else {
            // For future authentication support
            None
        };

        self.rdp_server.set_credentials(credentials);
        info!(
            "Authentication configured: {}",
            self.config.security.auth_method
        );

        // Run the IronRDP server
        let result = self.rdp_server.run().await.context("RDP server error");

        if let Err(ref e) = result {
            error!("Server stopped with error: {:#}", e);
        } else {
            info!("Server stopped gracefully");
        }

        info!("Server shutdown complete");
        result
    }

    /// Graceful shutdown
    ///
    /// Sends a quit event to stop the server gracefully.
    pub fn shutdown(&self) {
        info!("Initiating graceful shutdown");
        let _ = self
            .rdp_server
            .event_sender()
            .send(ironrdp_server::ServerEvent::Quit(
                "Shutdown requested".to_string(),
            ));
    }
}

impl Drop for LamcoRdpServer {
    fn drop(&mut self) {
        debug!("LamcoRdpServer dropped - cleaning up resources");
        // Resources are automatically cleaned up through Arc<Mutex<>> drops
        // and tokio task cancellation
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
