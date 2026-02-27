//! Clipboard Orchestrator
//!
//! **Execution Path:** ClipboardProvider trait + optional Klipper D-Bus cooperation
//! **Status:** Active (v1.0.0+)
//! **Platform:** Universal (Flatpak + Native)
//!
//! Main clipboard synchronization coordinator that manages bidirectional
//! clipboard sharing between RDP client and Wayland compositor.
//!
//! # Architecture
//!
//! The orchestrator uses library types from the lamco crate ecosystem:
//! - `lamco-clipboard-core` - Format conversion, transfer engine
//! - `ClipboardProvider` trait - Backend-agnostic clipboard access
//!
//! Server-specific types from this crate:
//! - `SyncManager` - State machine with echo protection
//! - `ClipboardEvent` - Server event routing
//!
//! # See Also
//!
//! - [`ClipboardIntegrationMode`] - Strategy selection
//! - [`KlipperCooperationCoordinator`] - KDE-specific integration

use std::{
    collections::HashMap,
    fs::File,
    io::{Read, Seek, SeekFrom, Write},
    path::PathBuf,
    sync::Arc,
};

use lamco_clipboard_core::{
    sanitize::{
        parse_file_uris, sanitize_filename_for_linux, sanitize_text_for_linux,
        sanitize_text_for_windows,
    },
    ClipboardFormat, FormatConverter, LoopDetectionConfig, TransferConfig, TransferEngine,
};
use tokio::sync::{mpsc, RwLock};
use tracing::{debug, error, info, trace, warn};

use crate::clipboard::{
    error::{ClipboardError, Result},
    sync::SyncManager,
    FormatConverterExt,
};

/// Shared clipboard provider reference (used by multiple handlers)
type SharedClipboardProvider =
    Arc<RwLock<Option<Arc<dyn crate::clipboard::provider::ClipboardProvider>>>>;

/// Pending portal request queue (format_id, mime_type, timestamp)
type PendingPortalRequests =
    Arc<RwLock<std::collections::VecDeque<(u32, String, std::time::Instant)>>>;

/// Server event sender for RDP clipboard messages
type ServerEventSender = Arc<RwLock<Option<mpsc::UnboundedSender<ironrdp_server::ServerEvent>>>>;

/// Runtime configuration for the clipboard orchestrator
///
/// This is the internal implementation config, separate from the user-facing
/// `crate::config::types::ClipboardConfig` which defines what users can configure.
/// The server maps user settings to this runtime config at startup.
#[derive(Debug, Clone)]
pub struct ClipboardOrchestratorConfig {
    /// Maximum data size in bytes
    pub max_data_size: usize,

    /// Enable image format support
    pub enable_images: bool,

    /// Enable file transfer support
    pub enable_files: bool,

    /// Enable HTML format support
    pub enable_html: bool,

    /// Enable RTF format support
    pub enable_rtf: bool,

    /// Chunk size for transfers
    pub chunk_size: usize,

    /// Transfer timeout in milliseconds
    pub timeout_ms: u64,

    /// Loop detection window in milliseconds
    pub loop_detection_window_ms: u64,

    /// Minimum milliseconds between forwarded clipboard events (rate limiting)
    /// Prevents rapid-fire D-Bus signals from overwhelming Portal. Set to 0 to disable.
    pub rate_limit_ms: u64,

    /// [EXPERIMENTAL] Include x-kde-syncselection hint for Klipper
    ///
    /// See `crate::config::types::ClipboardConfig::kde_syncselection_hint` for details.
    /// Default: false (disabled)
    pub kde_syncselection_hint: bool,
}

impl Default for ClipboardOrchestratorConfig {
    fn default() -> Self {
        Self {
            max_data_size: 16 * 1024 * 1024, // 16MB
            enable_images: true,
            enable_files: true,
            enable_html: true,
            enable_rtf: true,
            chunk_size: 64 * 1024, // 64KB chunks
            timeout_ms: 5000,
            loop_detection_window_ms: 500,
            rate_limit_ms: 200,            // Max 5 events/second
            kde_syncselection_hint: false, // Disabled by default
        }
    }
}

/// Response callback for sending data back to RDP
pub type RdpResponseCallback = Arc<dyn Fn(Vec<u8>) + Send + Sync>;

/// Clipboard events from RDP or Portal
#[derive(Clone)]
pub enum ClipboardEvent {
    /// RDP clipboard channel is ready - should re-announce Linux clipboard
    RdpReady,

    /// RDP client announced available formats
    RdpFormatList(Vec<ClipboardFormat>),

    /// RDP client requests data in specific format (with callback to send response)
    RdpDataRequest(u32, Option<RdpResponseCallback>),

    /// RDP client provides requested data
    RdpDataResponse(Vec<u8>),

    /// RDP client returned error for data request (need to cancel Portal transfer)
    RdpDataError,

    /// RDP client requests file contents (Windows wants file from Linux)
    RdpFileContentsRequest {
        stream_id: u32,
        list_index: u32,
        position: u64,
        size: u32,
        is_size_request: bool,
    },

    /// RDP client provides file contents (Linux receives file from Windows)
    RdpFileContentsResponse {
        stream_id: u32,
        data: Vec<u8>,
        is_error: bool,
    },

    /// Portal announced available MIME types
    /// The bool indicates if this is from D-Bus extension (true = authoritative, force sync)
    /// vs Portal echo (false = may be blocked if RDP owns clipboard)
    PortalFormatsAvailable(Vec<String>, bool),

    /// Portal requests data in specific MIME type
    PortalDataRequest(String),

    /// Portal provides requested data
    PortalDataResponse(Vec<u8>),
}

impl std::fmt::Debug for ClipboardEvent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::RdpReady => write!(f, "RdpReady"),
            Self::RdpFormatList(formats) => write!(f, "RdpFormatList({} formats)", formats.len()),
            Self::RdpDataRequest(id, _) => write!(f, "RdpDataRequest({id})"),
            Self::RdpDataResponse(data) => write!(f, "RdpDataResponse({} bytes)", data.len()),
            Self::RdpDataError => write!(f, "RdpDataError"),
            Self::RdpFileContentsRequest {
                stream_id,
                list_index,
                size,
                is_size_request,
                ..
            } => {
                write!(
                    f,
                    "RdpFileContentsRequest(stream={stream_id}, index={list_index}, size={size}, size_req={is_size_request})"
                )
            }
            Self::RdpFileContentsResponse {
                stream_id,
                data,
                is_error,
            } => {
                write!(
                    f,
                    "RdpFileContentsResponse(stream={}, {} bytes, error={})",
                    stream_id,
                    data.len(),
                    is_error
                )
            }
            Self::PortalFormatsAvailable(mimes, force) => {
                write!(f, "PortalFormatsAvailable({mimes:?}, force={force})")
            }
            Self::PortalDataRequest(mime) => write!(f, "PortalDataRequest({mime})"),
            Self::PortalDataResponse(data) => write!(f, "PortalDataResponse({} bytes)", data.len()),
        }
    }
}

/// Clipboard manager coordinates all clipboard operations
/// Coordinates bidirectional clipboard sync between RDP client and system clipboard
///
/// **Role:** Primary clipboard orchestrator for the server
/// **Integrates:** IronRDP (RDP side), Portal/Klipper (system side), format conversion
/// **Not to be confused with:** `DetectedSystemClipboardManager` (detection metadata)
///
/// # Architecture
///
/// Routes clipboard events between:
/// - RDP client (via `LamcoCliprdrFactory`)
/// - System clipboard (via `ClipboardProvider` trait)
/// - Klipper (via `KlipperCooperationCoordinator` when detected)
///
/// # See Also
///
/// - [`ClipboardIntegrationMode`] - Strategy selection
/// - [`KlipperCooperationCoordinator`] - KDE-specific integration
pub struct ClipboardOrchestrator {
    /// Configuration
    config: ClipboardOrchestratorConfig,

    /// Format converter
    converter: Arc<FormatConverter>,

    /// Transfer engine
    transfer_engine: Arc<TransferEngine>,

    /// Synchronization manager
    sync_manager: Arc<RwLock<SyncManager>>,

    /// Event sender
    event_tx: mpsc::Sender<ClipboardEvent>,

    /// Shutdown signal (mpsc for single event processor task)
    shutdown_tx: Option<mpsc::Sender<()>>,

    /// Shutdown broadcast (for all other async tasks)
    shutdown_broadcast: Arc<tokio::sync::broadcast::Sender<()>>,

    /// Task handles (for cleanup verification)
    task_handles: Arc<tokio::sync::Mutex<Vec<tokio::task::JoinHandle<()>>>>,

    /// Pending Portal SelectionTransfer requests (FIFO queue)
    /// Each entry: (serial, mime_type, request_time)
    /// Used to correlate SelectionTransfer signals with RDP FormatDataResponse in order
    pending_portal_requests:
        Arc<RwLock<std::collections::VecDeque<(u32, String, std::time::Instant)>>>,

    /// Server event sender for sending clipboard requests to IronRDP
    /// Set by LamcoCliprdrFactory after ServerEvent sender is available
    server_event_sender: Arc<RwLock<Option<mpsc::UnboundedSender<ironrdp_server::ServerEvent>>>>,

    /// Clipboard provider (trait-abstracted backend).
    clipboard_provider: Arc<RwLock<Option<Arc<dyn crate::clipboard::provider::ClipboardProvider>>>>,

    /// File transfer state (for handling file clipboard operations)
    file_transfer_state: Arc<RwLock<FileTransferState>>,

    /// FUSE filesystem manager for on-demand file transfer
    fuse_manager: Arc<RwLock<Option<crate::clipboard::fuse::FuseMount>>>,

    /// Channel sender for FUSE file content requests
    #[expect(dead_code, reason = "wired when FUSE file transfer is activated")]
    fuse_request_tx: Option<mpsc::Sender<crate::clipboard::fuse::FileContentsRequest>>,

    /// Pending FUSE responses (stream_id -> response channel)
    /// Used to deliver RDP FileContentsResponse back to FUSE read() calls
    pending_fuse_responses: Arc<
        RwLock<
            HashMap<
                u32,
                tokio::sync::oneshot::Sender<crate::clipboard::fuse::FileContentsResponse>,
            >,
        >,
    >,

    /// Current RDP format list from Windows (for format ID lookup)
    /// Windows registered format IDs (like FileGroupDescriptorW) vary per session,
    /// so we store the actual list to look up the correct ID when requesting data.
    current_rdp_formats: Arc<RwLock<Vec<ClipboardFormat>>>,

    /// Formats we've advertised TO Windows (for Linux → Windows data requests)
    /// When Windows requests data by format ID, we look up the format name here.
    local_advertised_formats: Arc<RwLock<Vec<ClipboardFormat>>>,

    /// Klipper (KDE clipboard manager) info for compositor-aware behavior
    klipper_info: Arc<RwLock<crate::clipboard::klipper::KlipperInfo>>,

    /// Guard: timestamp of last reannounce operation (Klipper mitigation)
    /// Used to prevent rapid reannouncement loops
    last_reannounce_time: Arc<RwLock<Option<std::time::SystemTime>>>,

    /// Guard: count reannouncements per RDP format list (prevent loops)
    /// Key: sorted format IDs, Value: reannounce count
    /// Used to limit reannouncements to max 2 per RDP copy operation
    reannounce_count: Arc<RwLock<HashMap<Vec<u32>, u32>>>,

    /// Health reporter for clipboard subsystem events
    health_reporter: Option<crate::health::HealthReporter>,

    /// Clipboard integration strategy (determined from service registry)
    ///
    /// Determines how we interact with clipboard manager (if any).
    /// Selected at initialization based on compositor, manager, deployment mode.
    strategy: crate::clipboard::ClipboardIntegrationMode,

    /// Klipper cooperation coordinator (Tier 2 strategy)
    ///
    /// When strategy is KlipperCooperationMode, this handles bidirectional
    /// sync with Klipper clipboard manager. None for other strategies.
    cooperation_coordinator: Arc<RwLock<Option<crate::clipboard::KlipperCooperationCoordinator>>>,

    /// Cooperation content cache
    ///
    /// Stores content received from Klipper cooperation mode.
    /// When KlipperContentUpdated fires, we store the text here.
    /// When client requests data, we serve from this cache.
    cooperation_content_cache: Arc<RwLock<Option<Vec<u8>>>>,
}

/// State for managing file transfers between Windows and Linux
#[derive(Debug)]
struct FileTransferState {
    /// Incoming files (Windows → Linux) - stream_id → file state
    incoming_files: HashMap<u32, IncomingFile>,

    /// Outgoing files (Linux → Windows) - from current clipboard
    outgoing_files: Vec<OutgoingFile>,

    /// Pending file descriptors from Windows (FileGroupDescriptorW)
    /// These describe files Windows has available for transfer
    pending_descriptors: Vec<lamco_clipboard_core::FileDescriptor>,

    /// Directory for downloaded files
    download_dir: PathBuf,

    /// Portal serial for current incoming transfer (to deliver URIs when complete)
    portal_serial: Option<u32>,

    /// Next stream ID to use for FileContentsRequest (incremented per request)
    next_stream_id: u32,

    /// Completed files ready for delivery (final paths after rename from temp)
    completed_files: Vec<PathBuf>,
}

/// File being received from Windows
#[derive(Debug)]
struct IncomingFile {
    #[expect(dead_code, reason = "retained for debug logging of file transfers")]
    stream_id: u32,
    filename: String,
    total_size: u64,
    received_size: u64,
    temp_path: PathBuf,
    file_handle: File,
    /// Index in the FileGroupDescriptorW list (needed for continuation requests)
    file_index: u32,
    /// Clipboard data lock ID (needed for continuation requests)
    clip_data_id: u32,
}

/// File being sent to Windows
#[derive(Debug)]
struct OutgoingFile {
    #[expect(dead_code, reason = "needed for multi-file transfer tracking")]
    list_index: u32,
    path: PathBuf,
    size: u64,
    filename: String,
}

impl FileTransferState {
    fn new(download_dir: PathBuf) -> Self {
        Self {
            incoming_files: HashMap::new(),
            outgoing_files: Vec::new(),
            pending_descriptors: Vec::new(),
            download_dir,
            portal_serial: None,
            next_stream_id: 1,
            completed_files: Vec::new(),
        }
    }

    fn clear_incoming(&mut self) {
        self.incoming_files.clear();
        self.portal_serial = None;
        self.completed_files.clear();
    }

    fn clear_outgoing(&mut self) {
        self.outgoing_files.clear();
    }

    fn set_pending_descriptors(&mut self, descriptors: Vec<lamco_clipboard_core::FileDescriptor>) {
        self.pending_descriptors = descriptors;
    }

    #[expect(dead_code, reason = "WIP: file transfer cleanup path")]
    fn clear_pending_descriptors(&mut self) {
        self.pending_descriptors.clear();
    }

    fn allocate_stream_id(&mut self) -> u32 {
        let id = self.next_stream_id;
        self.next_stream_id = self.next_stream_id.wrapping_add(1);
        id
    }

    /// Check if all incoming files are complete
    #[expect(dead_code, reason = "WIP: file transfer completion check")]
    fn all_files_complete(&self) -> bool {
        !self.incoming_files.is_empty()
            && self
                .incoming_files
                .values()
                .all(|f| f.received_size >= f.total_size && f.total_size > 0)
    }
}

/// Look up the actual RDP format ID for a MIME type from the stored format list.
///
/// Windows registered format IDs (like FileGroupDescriptorW) vary per session,
/// so we need to look them up from the actual format list sent by Windows.
fn lookup_format_id_for_mime(formats: &[ClipboardFormat], mime_type: &str) -> Option<u32> {
    use super::format_name_to_mime;

    // For text/plain, prefer CF_UNICODETEXT (13) over CF_TEXT (1)
    // CF_UNICODETEXT is UTF-16LE (full Unicode), CF_TEXT is ANSI (limited to Windows-1252)
    if mime_type == "text/plain;charset=utf-8" || mime_type == "text/plain" {
        if formats.iter().any(|f| f.id == 13) {
            debug!(
                "Preferring CF_UNICODETEXT (13) for {} (full Unicode support)",
                mime_type
            );
            return Some(13);
        }
        // Fall back to CF_TEXT if CF_UNICODETEXT not available
        if formats.iter().any(|f| f.id == 1) {
            debug!("Using CF_TEXT (1) for {} (ANSI fallback)", mime_type);
            return Some(1);
        }
    }

    // For all other MIME types, use normal lookup
    for format in formats {
        // First check if this format's ID maps to the requested MIME type
        if let Some(mapped_mime) = super::lib_rdp_format_to_mime(format.id) {
            if mapped_mime == mime_type {
                return Some(format.id);
            }
        }

        // For registered formats, check by name
        if let Some(ref name) = format.name {
            if let Some(mapped_mime) = format_name_to_mime(name) {
                // Direct match
                if mapped_mime == mime_type {
                    debug!(
                        "Found format ID {} for MIME {} via format name {:?}",
                        format.id, mime_type, name
                    );
                    return Some(format.id);
                }
                // For file formats: x-special/gnome-copied-files and text/uri-list are equivalent
                // GNOME Nautilus requests gnome-copied-files, but RDP file formats map to uri-list
                if mapped_mime == "text/uri-list" && mime_type == "x-special/gnome-copied-files" {
                    debug!(
                        "Found format ID {} for MIME {} via equivalent file format {:?}",
                        format.id, mime_type, name
                    );
                    return Some(format.id);
                }
            }
        }
    }

    None
}

impl std::fmt::Debug for ClipboardOrchestrator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClipboardOrchestrator")
            .field("config", &self.config)
            .field(
                "has_clipboard_provider",
                &self
                    .clipboard_provider
                    .try_read()
                    .map(|g| g.is_some())
                    .unwrap_or(false),
            )
            .finish_non_exhaustive()
    }
}

impl ClipboardOrchestrator {
    pub async fn new(config: ClipboardOrchestratorConfig) -> Result<Self> {
        let converter = Arc::new(FormatConverter::new());

        let transfer_config = TransferConfig {
            chunk_size: config.chunk_size,
            max_size: config.max_data_size,
            timeout_ms: config.timeout_ms,
            verify_integrity: true,
        };
        let transfer_engine = Arc::new(TransferEngine::with_config(transfer_config));

        let loop_config = LoopDetectionConfig {
            window_ms: config.loop_detection_window_ms,
            max_history: 10,
            enable_content_hashing: true,
            rate_limit_ms: if config.rate_limit_ms > 0 {
                Some(config.rate_limit_ms)
            } else {
                None
            },
        };
        let sync_manager = Arc::new(RwLock::new(SyncManager::with_config(loop_config)));

        let (event_tx, event_rx) = mpsc::channel(100);

        // Use XDG_DOWNLOAD_DIR for proper Flatpak sandbox compatibility
        let download_dir = std::env::var("XDG_DOWNLOAD_DIR")
            .ok()
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var("HOME")
                    .ok()
                    .map(|h| PathBuf::from(h).join("Downloads"))
            })
            .unwrap_or_else(|| PathBuf::from("/tmp"));

        let file_transfer_state = Arc::new(RwLock::new(FileTransferState::new(download_dir)));

        let (fuse_request_tx, fuse_request_rx) =
            mpsc::channel::<crate::clipboard::fuse::FileContentsRequest>(32);

        let fuse_manager = match crate::clipboard::fuse::FuseMount::new(fuse_request_tx.clone()) {
            Ok(fm) => {
                debug!("FUSE manager created");
                Some(fm)
            }
            Err(e) => {
                warn!(
                    "FUSE manager creation failed (file transfer may not work): {:?}",
                    e
                );
                None
            }
        };

        let fuse_manager = Arc::new(RwLock::new(fuse_manager));
        let pending_fuse_responses = Arc::new(RwLock::new(HashMap::new()));

        let klipper_info = crate::clipboard::klipper::KlipperMonitor::detect().await;
        let klipper_info = Arc::new(RwLock::new(klipper_info));

        let (shutdown_broadcast, _) = tokio::sync::broadcast::channel(16);
        let shutdown_broadcast = Arc::new(shutdown_broadcast);

        let task_handles = Arc::new(tokio::sync::Mutex::new(Vec::new()));

        let mut manager = Self {
            config,
            converter,
            transfer_engine,
            sync_manager,
            event_tx,
            shutdown_tx: None,
            pending_portal_requests: Arc::new(RwLock::new(std::collections::VecDeque::new())),
            server_event_sender: Arc::new(RwLock::new(None)), // Set by WrdCliprdrFactory
            clipboard_provider: Arc::new(RwLock::new(None)),
            file_transfer_state,
            fuse_manager: Arc::clone(&fuse_manager),
            fuse_request_tx: Some(fuse_request_tx),
            pending_fuse_responses: Arc::clone(&pending_fuse_responses),
            current_rdp_formats: Arc::new(RwLock::new(Vec::new())),
            local_advertised_formats: Arc::new(RwLock::new(Vec::new())),
            klipper_info,
            last_reannounce_time: Arc::new(RwLock::new(None)),
            reannounce_count: Arc::new(RwLock::new(HashMap::new())),
            strategy: crate::clipboard::ClipboardIntegrationMode::PortalDirect, // Default, will be set by initialize_strategy
            health_reporter: None,
            cooperation_coordinator: Arc::new(RwLock::new(None)),
            cooperation_content_cache: Arc::new(RwLock::new(None)),
            shutdown_broadcast: Arc::clone(&shutdown_broadcast),
            task_handles: Arc::clone(&task_handles),
        };

        manager
            .start_fuse_request_handler(fuse_request_rx, Arc::clone(&pending_fuse_responses))
            .await;
        manager.start_event_processor(event_rx);

        debug!("Clipboard manager initialized");

        Ok(manager)
    }

    pub fn event_sender(&self) -> mpsc::Sender<ClipboardEvent> {
        self.event_tx.clone()
    }

    /// Initialize clipboard strategy and cooperation mode
    ///
    /// Should be called after `new()` once environment detection is complete.
    pub async fn initialize_strategy(
        &mut self,
        strategy: crate::clipboard::ClipboardIntegrationMode,
        session_connection: Option<zbus::Connection>,
    ) -> Result<()> {
        info!("═══════════════════════════════════════════════════════════════");
        info!("  Initializing Clipboard Strategy");
        info!("═══════════════════════════════════════════════════════════════");
        info!("  Strategy: {}", strategy.name());

        self.strategy = strategy.clone();

        if strategy.uses_klipper_cooperation() {
            info!("  Klipper cooperation mode ENABLED");

            if let Some(conn) = session_connection {
                let (coordinator, event_rx) =
                    crate::clipboard::KlipperCooperationCoordinator::new(conn, 1000).await?;

                coordinator.start_monitoring().await?;
                *self.cooperation_coordinator.write().await = Some(coordinator);

                self.start_cooperation_event_handler(event_rx).await;

                info!("  ✅ Cooperation coordinator active and monitoring");
            } else {
                warn!("  ⚠️  No D-Bus connection - cooperation disabled");
                warn!("     Falling back to Tier 3 (re-announce) strategy");
            }
        } else {
            info!("  Standard strategy - no cooperation needed");
        }

        info!("═══════════════════════════════════════════════════════════════");

        Ok(())
    }

    /// Handle cooperation events from Klipper coordinator
    ///
    /// Spawns a task that processes cooperation events and syncs content
    /// between Klipper and RDP client.
    ///
    /// # Phase 2: Shutdown Signal
    ///
    /// Task subscribes to shutdown broadcast and exits cleanly when signaled.
    async fn start_cooperation_event_handler(
        &self,
        mut event_rx: tokio::sync::mpsc::UnboundedReceiver<crate::clipboard::CooperationEvent>,
    ) {
        let _converter = Arc::clone(&self.converter);
        let server_event_sender = Arc::clone(&self.server_event_sender);
        let sync_manager = Arc::clone(&self.sync_manager);
        let cooperation_content_cache = Arc::clone(&self.cooperation_content_cache);

        let mut shutdown_rx = self.shutdown_broadcast.subscribe();

        let handle = tokio::spawn(async move {
            info!("🎧 Cooperation event handler started");

            loop {
                tokio::select! {
                    Some(event) = event_rx.recv() => {
                match event {
                    crate::clipboard::CooperationEvent::KlipperContentUpdated {
                        content,
                        timestamp_ms,
                    } => {
                        debug!("📨 Cooperation: Klipper content updated ({}ms)", timestamp_ms);

                        // Klipper's D-Bus API only provides text
                        let formats = [
                            ClipboardFormat {
                                id: 13, // CF_UNICODETEXT
                                name: None,
                            },
                            ClipboardFormat {
                                id: 1, // CF_TEXT
                                name: None,
                            },
                        ];

                        {
                            let mut mgr = sync_manager.write().await;
                            mgr.handle_portal_formats(
                                vec!["text/plain".to_string()],
                                true, // force=true, this is authoritative from Klipper
                            );
                        }

                        if let Some(ref sender) = *server_event_sender.read().await {
                            use ironrdp_cliprdr::backend::ClipboardMessage;

                            let ironrdp_formats: Vec<ironrdp_cliprdr::pdu::ClipboardFormat> =
                                formats
                                    .iter()
                                    .map(|f| {
                                        ironrdp_cliprdr::pdu::ClipboardFormat {
                                            id: ironrdp_cliprdr::pdu::ClipboardFormatId(f.id),
                                            name: None,
                                        }
                                    })
                                    .collect();

                            if sender
                                .send(ironrdp_server::ServerEvent::Clipboard(
                                    ClipboardMessage::SendInitiateCopy(ironrdp_formats),
                                ))
                                .is_ok()
                            {
                                info!("✅ Cooperation: Sent FormatList to client (text from Klipper)");

                                // Convert to UTF-16 for CF_UNICODETEXT format
                                let utf16_data: Vec<u16> = content
                                    .encode_utf16()
                                    .chain(std::iter::once(0)) // Null terminator
                                    .collect();
                                let bytes: Vec<u8> = utf16_data
                                    .iter()
                                    .flat_map(|&c| c.to_le_bytes())
                                    .collect();

                                *cooperation_content_cache.write().await = Some(bytes.clone());
                                debug!(
                                    "Stored {} bytes in cooperation cache (UTF-16 text)",
                                    bytes.len()
                                );
                            } else {
                                warn!("Cooperation: Failed to send FormatList (channel closed)");
                            }
                        } else {
                            debug!("Cooperation: No server event sender (not ready yet)");
                        }
                    }

                    crate::clipboard::CooperationEvent::CooperationFailed { reason, retry } => {
                        if retry {
                            warn!("⚠️  Cooperation failed (retrying): {}", reason);
                        } else {
                            error!("❌ Cooperation failed (permanent): {}", reason);
                            error!("   Falling back to Tier 3 (re-announce) strategy");
                        }
                    }
                }
                    }

                    // Shutdown signal received
                    _ = shutdown_rx.recv() => {
                        info!("🛑 Cooperation event handler received shutdown signal");
                        break;
                    }
                }
            }

            info!("Cooperation event handler stopped");
        });

        self.task_handles.lock().await.push(handle);
    }

    /// Set server event sender (called by LamcoCliprdrFactory after initialization)
    pub async fn set_server_event_sender(
        &self,
        sender: mpsc::UnboundedSender<ironrdp_server::ServerEvent>,
    ) {
        *self.server_event_sender.write().await = Some(sender);
        debug!(" ServerEvent sender registered with clipboard manager");
    }

    /// Wire a health reporter so clipboard operations emit health events.
    pub fn set_health_reporter(&mut self, reporter: crate::health::HealthReporter) {
        self.health_reporter = Some(reporter);
    }

    /// Mount FUSE filesystem for clipboard file transfer
    ///
    /// Should be called once during session setup.
    pub async fn mount_fuse(&self) -> Result<()> {
        let mut fuse = self.fuse_manager.write().await;
        if let Some(ref mut manager) = *fuse {
            manager.mount()?;
            info!(
                "FUSE clipboard filesystem mounted at {:?}",
                manager.mount_point()
            );
        } else {
            warn!("FUSE manager not available - file transfer will use fallback staging");
        }
        Ok(())
    }

    /// Unmount FUSE filesystem
    pub async fn unmount_fuse(&self) -> Result<()> {
        let mut fuse = self.fuse_manager.write().await;
        if let Some(ref mut manager) = *fuse {
            manager.unmount()?;
            info!("FUSE clipboard filesystem unmounted");
        }
        Ok(())
    }

    pub async fn create_fuse_virtual_files(
        &self,
        descriptors: Vec<crate::clipboard::fuse::FileDescriptor>,
        clip_data_id: Option<u32>,
    ) -> Option<Vec<PathBuf>> {
        let fuse = self.fuse_manager.read().await;
        if let Some(ref manager) = *fuse {
            if manager.is_mounted() {
                let paths = manager.set_files(descriptors, clip_data_id);
                debug!("Created {} virtual files in FUSE", paths.len());
                return Some(paths);
            }
        }
        None
    }

    /// Generate gnome-copied-files content from FUSE virtual file paths
    pub fn generate_fuse_uri_content(paths: &[PathBuf]) -> String {
        crate::clipboard::fuse::generate_gnome_copied_files_content(paths)
    }

    /// Check if FUSE is available and mounted
    pub async fn is_fuse_available(&self) -> bool {
        let fuse = self.fuse_manager.read().await;
        fuse.as_ref()
            .is_some_and(super::fuse::FuseMount::is_mounted)
    }

    /// Set the clipboard provider (trait-abstracted backend).
    ///
    /// The provider manages its own listener tasks internally; this method
    /// subscribes to the provider's event stream and forwards events to
    /// the orchestrator's main event channel.
    pub async fn set_clipboard_provider(
        &mut self,
        provider: Arc<dyn crate::clipboard::provider::ClipboardProvider>,
    ) {
        info!("Setting clipboard provider: {}", provider.name());

        *self.clipboard_provider.write().await = Some(Arc::clone(&provider));

        // Subscribe to provider events and forward to our event channel
        let mut provider_rx = provider.subscribe();
        let event_tx = self.event_tx.clone();
        let pending_requests = Arc::clone(&self.pending_portal_requests);
        let mut shutdown_rx = self.shutdown_broadcast.subscribe();

        let handle = tokio::spawn(async move {
            loop {
                tokio::select! {
                    Some(event) = provider_rx.recv() => {
                        match event {
                            crate::clipboard::provider::ClipboardProviderEvent::SelectionChanged {
                                mime_types,
                                force,
                            } => {
                                if let Err(e) = event_tx
                                    .send(ClipboardEvent::PortalFormatsAvailable(mime_types, force))
                                    .await
                                {
                                    error!("Failed to forward SelectionChanged to orchestrator: {e}");
                                    break;
                                }
                            }
                            crate::clipboard::provider::ClipboardProviderEvent::SelectionTransfer {
                                serial,
                                mime_type,
                            } => {
                                // Track in pending requests queue for FIFO correlation
                                pending_requests.write().await.push_back((
                                    serial,
                                    mime_type.clone(),
                                    std::time::Instant::now(),
                                ));

                                if let Err(e) = event_tx
                                    .send(ClipboardEvent::PortalDataRequest(mime_type))
                                    .await
                                {
                                    error!("Failed to forward SelectionTransfer to orchestrator: {e}");
                                    break;
                                }
                            }
                        }
                    }
                    _ = shutdown_rx.recv() => {
                        info!("Provider event forwarder received shutdown");
                        break;
                    }
                }
            }
        });

        self.task_handles.lock().await.push(handle);

        debug!("Clipboard provider event forwarder started");
    }

    /// Run a health check on the active clipboard provider.
    ///
    /// Returns Ok if provider is healthy or no provider is set.
    /// Returns Err if the provider's health check fails.
    pub async fn health_check_provider(&self) -> crate::clipboard::error::Result<()> {
        let provider_opt = self.clipboard_provider.read().await;
        if let Some(ref provider) = *provider_opt {
            provider.health_check().await
        } else {
            Ok(())
        }
    }

    /// Start FUSE request handler
    ///
    /// This bridges synchronous FUSE read() calls to async RDP FileContentsRequests.
    /// When the Linux file manager reads a virtual file, FUSE blocks on a channel
    /// while we fetch the data from Windows via RDP.
    async fn start_fuse_request_handler(
        &self,
        mut request_rx: mpsc::Receiver<crate::clipboard::fuse::FileContentsRequest>,
        pending_responses: Arc<
            RwLock<
                HashMap<
                    u32,
                    tokio::sync::oneshot::Sender<crate::clipboard::fuse::FileContentsResponse>,
                >,
            >,
        >,
    ) {
        use crate::clipboard::fuse::FileContentsResponse;

        let server_event_sender = Arc::clone(&self.server_event_sender);
        let file_transfer_state = Arc::clone(&self.file_transfer_state);
        let mut shutdown_rx4 = self.shutdown_broadcast.subscribe();

        let handle4 = tokio::spawn(async move {
            debug!("FUSE request handler started");

            loop {
                tokio::select! {
                    Some(request) = request_rx.recv() => {
                let stream_id = {
                    let mut state = file_transfer_state.write().await;
                    state.allocate_stream_id()
                };

                debug!(
                    "FUSE request: file_index={} offset={} size={} -> stream_id={}",
                    request.file_index, request.offset, request.size, stream_id
                );

                {
                    let mut pending = pending_responses.write().await;
                    pending.insert(stream_id, request.response_tx);
                }

                if let Some(sender) = server_event_sender.read().await.as_ref() {
                    use ironrdp_cliprdr::backend::ClipboardMessage;
                    use ironrdp_cliprdr::pdu::{
                        FileContentsFlags, FileContentsRequest as RdpFileContentsRequest,
                    };

                    let rdp_request = RdpFileContentsRequest {
                        stream_id,
                        index: request.file_index,
                        flags: FileContentsFlags::DATA,
                        position: request.offset,
                        requested_size: request.size,
                        data_id: request.clip_data_id,
                    };

                    if let Err(e) = sender.send(ironrdp_server::ServerEvent::Clipboard(
                        ClipboardMessage::SendFileContentsRequest(rdp_request),
                    )) {
                        error!("Failed to send FileContentsRequest to RDP: {:?}", e);
                        if let Some(response_tx) =
                            pending_responses.write().await.remove(&stream_id)
                        {
                            let _ = response_tx.send(FileContentsResponse::Error(
                                "Failed to send RDP request".to_string(),
                            ));
                        }
                    }
                } else {
                    warn!("ServerEvent sender not available for FUSE request");
                    if let Some(response_tx) = pending_responses.write().await.remove(&stream_id) {
                        let _ = response_tx
                            .send(FileContentsResponse::Error("RDP not connected".to_string()));
                    }
                }
                    }

                    _ = shutdown_rx4.recv() => {
                        info!("🛑 FUSE request handler received shutdown signal");
                        break;
                    }
                }
            }

            info!("FUSE request handler stopped");
        });

        self.task_handles.lock().await.push(handle4);
    }

    /// Deliver FUSE file contents response from RDP
    ///
    /// Called when we receive a FileContentsResponse from Windows.
    /// This delivers the data back to the blocked FUSE read() call.
    pub async fn deliver_fuse_response(&self, stream_id: u32, data: Vec<u8>, is_error: bool) {
        use crate::clipboard::fuse::FileContentsResponse;

        if let Some(response_tx) = self.pending_fuse_responses.write().await.remove(&stream_id) {
            let response = if is_error {
                FileContentsResponse::Error("RDP error".to_string())
            } else {
                FileContentsResponse::Data(data)
            };

            if response_tx.send(response).is_err() {
                warn!("FUSE response channel closed for stream_id={}", stream_id);
            } else {
                trace!("Delivered FUSE response for stream_id={}", stream_id);
            }
        } else {
            // This may be a response for the old staging-based transfer, not FUSE
            trace!(
                "No pending FUSE request for stream_id={} (may be staging transfer)",
                stream_id
            );
        }
    }

    /// Start event processing loop
    fn start_event_processor(&mut self, mut event_rx: mpsc::Receiver<ClipboardEvent>) {
        let converter = self.converter.clone();
        let sync_manager = self.sync_manager.clone();
        let transfer_engine = self.transfer_engine.clone();
        let config = self.config.clone();
        let clipboard_provider = Arc::clone(&self.clipboard_provider);
        let pending_portal_requests = Arc::clone(&self.pending_portal_requests);
        let server_event_sender = Arc::clone(&self.server_event_sender);
        let file_transfer_state = Arc::clone(&self.file_transfer_state);
        let fuse_manager = Arc::clone(&self.fuse_manager);
        let current_rdp_formats = Arc::clone(&self.current_rdp_formats);
        let local_advertised_formats = Arc::clone(&self.local_advertised_formats);
        let last_reannounce_time = Arc::clone(&self.last_reannounce_time);
        let reannounce_count = Arc::clone(&self.reannounce_count);
        let klipper_info = Arc::clone(&self.klipper_info);
        let cooperation_coordinator = Arc::clone(&self.cooperation_coordinator);
        let cooperation_content_cache = Arc::clone(&self.cooperation_content_cache);
        let health_reporter = self.health_reporter.clone();

        let (shutdown_tx, mut shutdown_rx) = mpsc::channel::<()>(1);
        self.shutdown_tx = Some(shutdown_tx);

        tokio::spawn(async move {
            let mut consecutive_errors: u32 = 0;

            loop {
                tokio::select! {
                    Some(event) = event_rx.recv() => {
                        if let Err(e) = Self::handle_event(
                            event,
                            &converter,
                            &sync_manager,
                            &transfer_engine,
                            &config,
                            &clipboard_provider,
                            &pending_portal_requests,
                            &server_event_sender,
                            &file_transfer_state,
                            &fuse_manager,
                            &current_rdp_formats,
                            &local_advertised_formats,
                            &last_reannounce_time,
                            &reannounce_count,
                            &klipper_info,
                            &cooperation_coordinator,
                            &cooperation_content_cache,
                        ).await {
                            let err_msg = format!("{e}");
                            error!("Error handling clipboard event: {err_msg}");
                            consecutive_errors += 1;

                            // Session-invalid errors are fatal — report immediately
                            let is_session_invalid = err_msg.contains("session invalid")
                                || err_msg.contains("Session invalid");
                            if is_session_invalid || consecutive_errors >= 3 {
                                if let Some(ref reporter) = health_reporter {
                                    reporter.report(crate::health::HealthEvent::ClipboardFailed {
                                        reason: format!("{consecutive_errors} consecutive errors: {e}"),
                                    });
                                }
                            }
                        } else if consecutive_errors > 0 {
                            // Recovered after errors
                            if consecutive_errors >= 3 {
                                if let Some(ref reporter) = health_reporter {
                                    reporter.report(crate::health::HealthEvent::ClipboardRecovered);
                                }
                            }
                            consecutive_errors = 0;
                        }
                    }
                    _ = shutdown_rx.recv() => {
                        debug!("Clipboard manager shutting down");
                        break;
                    }
                }
            }
        });
    }

    /// Handle a clipboard event
    #[expect(
        clippy::too_many_arguments,
        reason = "orchestrator dispatches with shared state refs"
    )]
    async fn handle_event(
        event: ClipboardEvent,
        converter: &FormatConverter,
        sync_manager: &Arc<RwLock<SyncManager>>,
        transfer_engine: &TransferEngine,
        _config: &ClipboardOrchestratorConfig,
        clipboard_provider: &SharedClipboardProvider,
        pending_portal_requests: &PendingPortalRequests,
        server_event_sender: &ServerEventSender,
        file_transfer_state: &Arc<RwLock<FileTransferState>>,
        fuse_manager: &Arc<RwLock<Option<crate::clipboard::fuse::FuseMount>>>,
        current_rdp_formats: &Arc<RwLock<Vec<ClipboardFormat>>>,
        local_advertised_formats: &Arc<RwLock<Vec<ClipboardFormat>>>,
        last_reannounce_time: &Arc<RwLock<Option<std::time::SystemTime>>>,
        reannounce_count: &Arc<RwLock<HashMap<Vec<u32>, u32>>>,
        klipper_info: &Arc<RwLock<crate::clipboard::klipper::KlipperInfo>>,
        cooperation_coordinator: &Arc<
            RwLock<Option<crate::clipboard::KlipperCooperationCoordinator>>,
        >,
        cooperation_content_cache: &Arc<RwLock<Option<Vec<u8>>>>,
    ) -> Result<()> {
        match event {
            ClipboardEvent::RdpReady => {
                debug!("RDP clipboard channel ready - checking for pending Linux clipboard to announce");
                // When RDP becomes ready, re-announce any cached Linux clipboard formats
                // This handles the case where Linux clipboard changed before RDP connected
                let advertised = local_advertised_formats.read().await;
                if !advertised.is_empty() {
                    info!(
                        "Re-announcing {} cached Linux clipboard formats to RDP",
                        advertised.len()
                    );
                    let formats_to_send = advertised.clone();
                    drop(advertised);

                    let sender_opt = server_event_sender.read().await.clone();
                    if let Some(sender) = sender_opt {
                        use ironrdp_cliprdr::backend::ClipboardMessage;

                        let rdp_formats: Vec<ironrdp_cliprdr::pdu::ClipboardFormat> =
                            formats_to_send
                                .iter()
                                .map(|f| {
                                    let name = f.name.as_ref().map(|n| {
                                        ironrdp_cliprdr::pdu::ClipboardFormatName::new(n.clone())
                                    });
                                    ironrdp_cliprdr::pdu::ClipboardFormat {
                                        id: ironrdp_cliprdr::pdu::ClipboardFormatId(f.id),
                                        name,
                                    }
                                })
                                .collect();

                        info!(
                            "Re-sending FormatList to RDP client with {} formats",
                            rdp_formats.len()
                        );
                        if let Err(e) = sender.send(ironrdp_server::ServerEvent::Clipboard(
                            ClipboardMessage::SendInitiateCopy(rdp_formats),
                        )) {
                            error!("Failed to re-send FormatList: {:?}", e);
                        }
                    }
                } else {
                    debug!("No cached Linux clipboard formats to announce");
                }
                Ok(())
            }

            ClipboardEvent::RdpFormatList(formats) => {
                Self::handle_rdp_format_list(
                    formats,
                    converter,
                    sync_manager,
                    clipboard_provider,
                    current_rdp_formats,
                    _config,
                    klipper_info,
                    cooperation_coordinator,
                )
                .await
            }

            ClipboardEvent::RdpDataRequest(format_id, _response_callback) => {
                Self::handle_rdp_data_request(
                    format_id,
                    converter,
                    sync_manager,
                    clipboard_provider,
                    server_event_sender,
                    local_advertised_formats,
                    file_transfer_state,
                    cooperation_content_cache,
                )
                .await
            }

            ClipboardEvent::RdpDataResponse(data) => {
                Self::handle_rdp_data_response(
                    data,
                    sync_manager,
                    transfer_engine,
                    clipboard_provider,
                    pending_portal_requests,
                    file_transfer_state,
                    fuse_manager,
                    server_event_sender,
                )
                .await
            }

            ClipboardEvent::RdpDataError => {
                Self::handle_rdp_data_error(clipboard_provider, pending_portal_requests).await
            }

            ClipboardEvent::RdpFileContentsRequest {
                stream_id,
                list_index,
                position,
                size,
                is_size_request,
            } => {
                Self::handle_rdp_file_contents_request(
                    stream_id,
                    list_index,
                    position,
                    size,
                    is_size_request,
                    server_event_sender,
                    file_transfer_state,
                )
                .await
            }

            ClipboardEvent::RdpFileContentsResponse {
                stream_id,
                data,
                is_error,
            } => {
                Self::handle_rdp_file_contents_response(
                    stream_id,
                    data,
                    is_error,
                    file_transfer_state,
                    clipboard_provider,
                    server_event_sender,
                )
                .await
            }

            ClipboardEvent::PortalFormatsAvailable(mime_types, force) => {
                Self::handle_portal_formats(
                    mime_types,
                    force,
                    converter,
                    sync_manager,
                    server_event_sender,
                    local_advertised_formats,
                    current_rdp_formats,
                    clipboard_provider,
                    last_reannounce_time,
                    reannounce_count,
                )
                .await
            }

            ClipboardEvent::PortalDataRequest(mime_type) => {
                Self::handle_portal_data_request(
                    mime_type,
                    converter,
                    sync_manager,
                    server_event_sender,
                    current_rdp_formats,
                )
                .await
            }

            ClipboardEvent::PortalDataResponse(_) => {
                // PortalDataResponse is unused — data flows through
                // handle_rdp_data_request → Portal read_data → SendFormatData
                Ok(())
            }
        }
    }

    /// Handle RDP format list announcement
    #[expect(
        clippy::too_many_arguments,
        reason = "orchestrator handler with shared state refs"
    )]
    async fn handle_rdp_format_list(
        formats: Vec<ClipboardFormat>,
        converter: &FormatConverter,
        sync_manager: &Arc<RwLock<SyncManager>>,
        clipboard_provider: &SharedClipboardProvider,
        current_rdp_formats: &Arc<RwLock<Vec<ClipboardFormat>>>,
        config: &ClipboardOrchestratorConfig,
        klipper_info: &Arc<RwLock<crate::clipboard::klipper::KlipperInfo>>,
        cooperation_coordinator: &Arc<
            RwLock<Option<crate::clipboard::KlipperCooperationCoordinator>>,
        >,
    ) -> Result<()> {
        debug!("RDP format list received: {:?}", formats);

        // Registered format IDs vary per session, store for later lookup
        {
            let mut stored_formats = current_rdp_formats.write().await;
            stored_formats.clone_from(&formats);
            debug!(
                "Stored {} RDP formats for format ID lookup",
                stored_formats.len()
            );
        }

        {
            let coordinator_opt = cooperation_coordinator.read().await;
            if let Some(ref coordinator) = *coordinator_opt {
                coordinator.update_rdp_formats(formats.clone()).await;
                debug!(
                    "Updated cooperation coordinator with {} RDP formats",
                    formats.len()
                );
            }
        }

        let should_sync = {
            let mut mgr = sync_manager.write().await;
            mgr.handle_rdp_formats(formats.clone())
        };

        if !should_sync {
            debug!("Skipping RDP format list due to loop detection");
            return Ok(());
        }

        let mut mime_types = converter.rdp_to_mime_types(&formats)?;

        debug!("Converted to MIME types: {:?}", mime_types);

        if config.kde_syncselection_hint {
            let klipper_detected = {
                let info = klipper_info.read().await;
                info.detected && info.responsive
            };

            if klipper_detected {
                warn!("⚠️  EXPERIMENTAL: Adding x-kde-syncselection hint");
                warn!("   This tells Klipper to completely ignore our clipboard");
                warn!("   This MIME type is intended for Klipper's internal use only");

                const KDE_SYNCSELECTION: &str = "application/x-kde-syncselection";

                if !mime_types.contains(&KDE_SYNCSELECTION.to_string()) {
                    mime_types.push(KDE_SYNCSELECTION.to_string());
                    debug!("   Added {} to MIME types", KDE_SYNCSELECTION);
                }
            } else {
                debug!("kde_syncselection_hint enabled but Klipper not detected - skipping hint");
            }
        }

        debug!("Final MIME types for SetSelection: {:?}", mime_types);

        // Delayed rendering: announce format availability WITHOUT transferring data
        info!("┌─ SetSelection (RDP → Provider) ──────────────────────────────");
        info!(
            "│ Announcing {} MIME types: {:?}",
            mime_types.len(),
            mime_types
        );
        info!(
            "│ Echo protection window starts NOW ({}ms)",
            2000 // ECHO_PROTECTION_WINDOW_MS from sync.rs
        );
        info!("│ Any SelectionOwnerChanged within this window will be blocked");
        info!("└────────────────────────────────────────────────────────────────");

        // Announce via clipboard provider
        let provider_opt = clipboard_provider.read().await;
        if let Some(ref provider) = *provider_opt {
            provider.announce_formats(mime_types).await.map_err(|e| {
                ClipboardError::PortalError(format!("Provider announce_formats failed: {e}"))
            })?;
            debug!(
                "RDP clipboard formats announced via {} provider",
                provider.name()
            );
        } else {
            debug!("No clipboard provider available (normal during startup)");
        }

        Ok(())
    }

    /// Handle RDP data request (Linux → Windows paste)
    #[expect(
        clippy::too_many_arguments,
        reason = "orchestrator handler with shared state refs"
    )]
    async fn handle_rdp_data_request(
        format_id: u32,
        converter: &FormatConverter,
        _sync_manager: &Arc<RwLock<SyncManager>>,
        clipboard_provider: &SharedClipboardProvider,
        server_event_sender: &ServerEventSender,
        local_advertised_formats: &Arc<RwLock<Vec<ClipboardFormat>>>,
        file_transfer_state: &Arc<RwLock<FileTransferState>>,
        cooperation_content_cache: &Arc<RwLock<Option<Vec<u8>>>>,
    ) -> Result<()> {
        info!(
            "RDP data request for format ID: {} (Linux → Windows paste)",
            format_id
        );

        // PRIORITY 1: Check cooperation content cache (from Klipper sync)
        // If we recently synced from Klipper, serve that content
        if let Some(cached_data) = cooperation_content_cache.read().await.as_ref() {
            // Check if format_id matches what we cached (CF_UNICODETEXT=13 or CF_TEXT=1)
            if format_id == 13 || format_id == 1 {
                info!(
                    "✅ Serving from cooperation cache: {} bytes (Klipper sync)",
                    cached_data.len()
                );

                let sender_opt = server_event_sender.read().await.clone();
                if let Some(sender) = sender_opt {
                    use ironrdp_cliprdr::{backend::ClipboardMessage, pdu::FormatDataResponse};
                    use ironrdp_pdu::IntoOwned;

                    let data_to_send = if format_id == 1 {
                        // CF_TEXT: client wants ANSI text, cache is UTF-16LE
                        let text = ironrdp_pdu::utils::from_utf16_bytes(cached_data);
                        let trimmed = text.trim_end_matches('\0');
                        let mut bytes = trimmed.as_bytes().to_vec();
                        bytes.push(0); // CF_TEXT null terminator
                        debug!(
                            "Converted cooperation cache UTF-16LE ({} bytes) to CF_TEXT ({} bytes)",
                            cached_data.len(),
                            bytes.len()
                        );
                        bytes
                    } else {
                        // CF_UNICODETEXT: cache is already UTF-16LE
                        cached_data.clone()
                    };

                    let response = FormatDataResponse::new_data(data_to_send.clone());
                    let owned_response = response.into_owned();

                    if sender
                        .send(ironrdp_server::ServerEvent::Clipboard(
                            ClipboardMessage::SendFormatData(owned_response),
                        ))
                        .is_ok()
                    {
                        info!(
                            "Sent {} bytes from cooperation cache to RDP client",
                            data_to_send.len()
                        );
                        return Ok(());
                    }
                } else {
                    warn!("ServerEvent sender not available");
                }
            }
        }

        // Normal path: read from Portal clipboard
        let advertised = local_advertised_formats.read().await;
        let format_name = advertised
            .iter()
            .find(|f| f.id == format_id || (format_id == 0 && f.name.is_some()))
            .and_then(|f| f.name.clone());
        drop(advertised);

        if let Some(ref name) = format_name {
            if name == "FileGroupDescriptorW" {
                debug!("Windows requests FileGroupDescriptorW - sending file list from Linux clipboard");
                return Self::handle_file_descriptor_request(
                    clipboard_provider,
                    server_event_sender,
                    file_transfer_state,
                )
                .await;
            }
        }

        let mime_type = match converter.format_id_to_mime(format_id) {
            Ok(m) => m,
            Err(e) => {
                warn!("Unknown format ID {}: {:?}", format_id, e);
                Self::send_format_data_error(server_event_sender).await;
                return Ok(());
            }
        };
        debug!("Format {} maps to MIME: {}", format_id, mime_type);

        let portal_data = {
            let provider_opt = clipboard_provider.read().await;
            if let Some(ref provider) = *provider_opt {
                match provider.read_data(&mime_type).await {
                    Ok(data) => {
                        info!(
                            "Read {} bytes from {} provider ({})",
                            data.len(),
                            provider.name(),
                            mime_type
                        );
                        data
                    }
                    Err(e) => {
                        error!("Failed to read from {} provider: {:#}", provider.name(), e);
                        Self::send_format_data_error(server_event_sender).await;
                        return Ok(());
                    }
                }
            } else {
                warn!("No clipboard provider available for RDP data request");
                Self::send_format_data_error(server_event_sender).await;
                return Ok(());
            }
        };

        let rdp_data = if format_id == 13 {
            // CF_UNICODETEXT - Convert UTF-8 to UTF-16LE with line ending conversion
            let text = String::from_utf8_lossy(&portal_data);
            // Sanitize text for Windows: LF → CRLF, remove null bytes
            let sanitized = sanitize_text_for_windows(&text);
            let utf16: Vec<u16> = sanitized.encode_utf16().collect();
            let mut bytes = Vec::with_capacity(utf16.len() * 2 + 2);
            for c in utf16 {
                bytes.extend_from_slice(&c.to_le_bytes());
            }
            bytes.extend_from_slice(&[0, 0]); // Null terminator
            debug!(
                "Converted UTF-8 ({} bytes) to UTF-16LE ({} bytes) with CRLF line endings",
                portal_data.len(),
                bytes.len()
            );
            bytes
        } else if format_id == 8 {
            // CF_DIB - Windows wants DIB, Portal has image format
            if mime_type.starts_with("image/png") {
                trace!(" Converting PNG to DIB for Windows");
                lamco_clipboard_core::image::png_to_dib(&portal_data)
                    .map_err(ClipboardError::Core)?
            } else if mime_type.starts_with("image/jpeg") {
                trace!(" Converting JPEG to DIB for Windows");
                lamco_clipboard_core::image::jpeg_to_dib(&portal_data)
                    .map_err(ClipboardError::Core)?
            } else if mime_type.starts_with("image/bmp") || mime_type.starts_with("image/x-bmp") {
                trace!(" Converting BMP to DIB for Windows");
                lamco_clipboard_core::image::bmp_to_dib(&portal_data)
                    .map_err(ClipboardError::Core)?
            } else {
                debug!("Unknown image MIME for DIB: {}, passing through", mime_type);
                portal_data
            }
        } else if format_id == 17 {
            // CF_DIBV5 - Windows wants DIBV5 with alpha channel support
            if mime_type.starts_with("image/png") {
                trace!(" Converting PNG to DIBV5 for Windows (with alpha)");
                lamco_clipboard_core::image::png_to_dibv5(&portal_data)
                    .map_err(ClipboardError::Core)?
            } else if mime_type.starts_with("image/jpeg") {
                trace!(" Converting JPEG to DIBV5 for Windows");
                lamco_clipboard_core::image::jpeg_to_dibv5(&portal_data)
                    .map_err(ClipboardError::Core)?
            } else {
                // Unsupported MIME for DIBV5, fall back to raw data
                debug!(
                    "Unknown image MIME for DIBV5: {}, passing through",
                    mime_type
                );
                portal_data
            }
        } else if format_id == 0xD011 {
            // CF_PNG - Windows wants PNG
            if mime_type.starts_with("image/png") {
                debug!("PNG to PNG - pass through");
                portal_data
            } else {
                debug!("Unsupported conversion to PNG from {}", mime_type);
                portal_data
            }
        } else {
            debug!(
                "Format {} - pass through {} bytes",
                format_id,
                portal_data.len()
            );
            portal_data
        };

        let data_len = rdp_data.len();
        debug!("Converted to RDP format: {} bytes", data_len);

        let sender_opt = server_event_sender.read().await.clone();
        if let Some(sender) = sender_opt {
            use ironrdp_cliprdr::{backend::ClipboardMessage, pdu::FormatDataResponse};
            use ironrdp_pdu::IntoOwned;

            let response = FormatDataResponse::new_data(rdp_data);
            let owned_response = response.into_owned();

            if let Err(e) = sender.send(ironrdp_server::ServerEvent::Clipboard(
                ClipboardMessage::SendFormatData(owned_response),
            )) {
                error!("Failed to send FormatDataResponse via ServerEvent: {:?}", e);
            } else {
                info!(
                    "Sent {} bytes to RDP client for format {} (Linux → Windows)",
                    data_len, format_id
                );
            }
        } else {
            warn!("ServerEvent sender not available - cannot send clipboard data to RDP");
        }

        Ok(())
    }

    /// Handle FileGroupDescriptorW request from Windows (Linux → Windows file transfer)
    ///
    /// Reads file URIs from Portal clipboard and converts to Windows FILEDESCRIPTORW format.
    async fn handle_file_descriptor_request(
        clipboard_provider: &SharedClipboardProvider,
        server_event_sender: &ServerEventSender,
        file_transfer_state: &Arc<RwLock<FileTransferState>>,
    ) -> Result<()> {
        // Read file URIs: prefer x-special/gnome-copied-files, fall back to text/uri-list
        let uri_data = {
            let provider_opt = clipboard_provider.read().await;
            if let Some(ref provider) = *provider_opt {
                match provider.read_data("x-special/gnome-copied-files").await {
                    Ok(data) if !data.is_empty() => {
                        info!(
                            "Read {} bytes from {} provider (x-special/gnome-copied-files)",
                            data.len(),
                            provider.name()
                        );
                        data
                    }
                    _ => match provider.read_data("text/uri-list").await {
                        Ok(data) => {
                            info!(
                                "Read {} bytes from {} provider (text/uri-list)",
                                data.len(),
                                provider.name()
                            );
                            data
                        }
                        Err(e) => {
                            error!(
                                "Failed to read file URIs from {} provider: {:#}",
                                provider.name(),
                                e
                            );
                            Self::send_format_data_error(server_event_sender).await;
                            return Ok(());
                        }
                    },
                }
            } else {
                warn!("No clipboard provider available for file descriptor request");
                Self::send_format_data_error(server_event_sender).await;
                return Ok(());
            }
        };

        let file_paths = parse_file_uris(&uri_data);

        for path in &file_paths {
            trace!("Found file: {:?}", path);
        }

        if file_paths.is_empty() {
            warn!("No valid file paths found in clipboard");
            Self::send_format_data_error(server_event_sender).await;
            return Ok(());
        }

        {
            let mut state = file_transfer_state.write().await;
            state.clear_outgoing();
            for (idx, path) in file_paths.iter().enumerate() {
                if let Ok(metadata) = std::fs::metadata(path) {
                    let filename = path
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("unknown")
                        .to_string();
                    state.outgoing_files.push(OutgoingFile {
                        list_index: idx as u32,
                        path: path.clone(),
                        size: metadata.len(),
                        filename,
                    });
                }
            }
            info!(
                "Stored {} outgoing files for transfer",
                state.outgoing_files.len()
            );
        }

        let descriptor_data = match lamco_clipboard_core::build_file_group_descriptor_w(&file_paths)
        {
            Ok(data) => {
                info!(
                    "Built FileGroupDescriptorW ({} bytes) for {} files",
                    data.len(),
                    file_paths.len()
                );
                data
            }
            Err(e) => {
                error!("Failed to build FileGroupDescriptorW: {:?}", e);
                Self::send_format_data_error(server_event_sender).await;
                return Ok(());
            }
        };

        let sender_opt = server_event_sender.read().await.clone();
        if let Some(sender) = sender_opt {
            use ironrdp_cliprdr::{backend::ClipboardMessage, pdu::FormatDataResponse};
            use ironrdp_pdu::IntoOwned;

            let response = FormatDataResponse::new_data(descriptor_data);
            let owned_response = response.into_owned();

            if let Err(e) = sender.send(ironrdp_server::ServerEvent::Clipboard(
                ClipboardMessage::SendFormatData(owned_response),
            )) {
                error!("Failed to send FileGroupDescriptorW response: {:?}", e);
            } else {
                debug!(" Sent FileGroupDescriptorW to Windows (Linux → Windows file transfer)");
            }
        }

        Ok(())
    }

    /// Send error response for FormatDataRequest
    async fn send_format_data_error(server_event_sender: &ServerEventSender) {
        let sender_opt = server_event_sender.read().await.clone();
        if let Some(sender) = sender_opt {
            use ironrdp_cliprdr::{backend::ClipboardMessage, pdu::FormatDataResponse};
            use ironrdp_pdu::IntoOwned;

            let response = FormatDataResponse::new_error();
            let owned_response = response.into_owned();

            if let Err(e) = sender.send(ironrdp_server::ServerEvent::Clipboard(
                ClipboardMessage::SendFormatData(owned_response),
            )) {
                error!("Failed to send error FormatDataResponse: {:?}", e);
            } else {
                debug!("Sent error FormatDataResponse to RDP client");
            }
        }
    }

    /// Handle RDP data response (Windows → Linux paste completion)
    #[expect(
        clippy::too_many_arguments,
        reason = "orchestrator handler with shared state refs"
    )]
    #[expect(
        clippy::expect_used,
        reason = "provider existence verified by caller before this path"
    )]
    async fn handle_rdp_data_response(
        data: Vec<u8>,
        sync_manager: &Arc<RwLock<SyncManager>>,
        _transfer_engine: &TransferEngine,
        clipboard_provider: &SharedClipboardProvider,
        pending_portal_requests: &PendingPortalRequests,
        file_transfer_state: &Arc<RwLock<FileTransferState>>,
        fuse_manager: &Arc<RwLock<Option<crate::clipboard::fuse::FuseMount>>>,
        server_event_sender: &ServerEventSender,
    ) -> Result<()> {
        debug!("RDP data response received: {} bytes", data.len());

        let should_transfer = sync_manager.write().await.check_content(&data, true);
        if !should_transfer {
            debug!("Skipping RDP data due to content loop detection");
            return Ok(());
        }

        let provider_opt = clipboard_provider.read().await.clone();
        if provider_opt.is_none() {
            warn!("No clipboard provider available - cannot deliver clipboard data");
            return Ok(());
        }

        // Get FIRST pending request (FIFO order)
        // IronRDP doesn't correlate requests/responses, so we use FIFO queue
        let mut pending = pending_portal_requests.write().await;
        let request_opt = pending.pop_front();
        drop(pending);

        let (serial, requested_mime, _request_time) = match request_opt {
            Some(req) => req,
            None => {
                warn!("No pending request - FormatDataResponse arrived with no matching request");
                return Ok(());
            }
        };

        info!(
            "Matched FormatDataResponse to serial {} (FIFO queue)",
            serial
        );
        debug!(
            "Requested MIME: {}, received {} bytes from Windows",
            requested_mime,
            data.len()
        );

        // Special handling for file transfer formats
        if requested_mime == "text/uri-list" || requested_mime == "x-special/gnome-copied-files" {
            info!(
                "Received FileGroupDescriptorW data ({} bytes) - parsing file list",
                data.len()
            );

            match lamco_clipboard_core::FileDescriptor::parse_list(&data) {
                Ok(descriptors) => {
                    info!(
                        "Parsed {} file descriptor(s) from Windows",
                        descriptors.len()
                    );

                    for (idx, desc) in descriptors.iter().enumerate() {
                        info!(
                            "  File {}: {} ({} bytes)",
                            idx,
                            desc.name,
                            desc.size.unwrap_or(0)
                        );
                    }

                    // Check for duplicate file transfer (apps send both text/uri-list and
                    // x-special/gnome-copied-files for the same paste)
                    {
                        let state = file_transfer_state.read().await;
                        if !state.incoming_files.is_empty() {
                            info!(
                                "Skipping duplicate file transfer request ({}) - transfer already in progress with {} file(s)",
                                requested_mime, state.incoming_files.len()
                            );
                            drop(state);

                            if let Some(ref provider) = provider_opt {
                                let _ = provider
                                    .complete_transfer(serial, &requested_mime, vec![], false)
                                    .await;
                            }
                            return Ok(());
                        }
                    }

                    let fuse_available = {
                        let fuse = fuse_manager.read().await;
                        fuse.as_ref()
                            .is_some_and(super::fuse::FuseMount::is_mounted)
                    };

                    if fuse_available {
                        info!("Using FUSE on-demand file transfer (no upfront download)");

                        let clip_data_id = 1u32;
                        if let Some(sender) = server_event_sender.read().await.as_ref() {
                            use ironrdp_cliprdr::backend::ClipboardMessage;
                            if let Err(e) = sender.send(ironrdp_server::ServerEvent::Clipboard(
                                ClipboardMessage::SendLockClipboard { clip_data_id },
                            )) {
                                warn!("Failed to send Lock PDU for FUSE transfer: {:?}", e);
                            }
                        }

                        let fuse_descriptors: Vec<crate::clipboard::fuse::FileDescriptor> =
                            descriptors
                                .iter()
                                .map(|d| {
                                    let filename = sanitize_filename_for_linux(&d.name);
                                    crate::clipboard::fuse::FileDescriptor::new(
                                        filename,
                                        d.size.unwrap_or(0),
                                    )
                                })
                                .collect();

                        let paths = {
                            let fuse = fuse_manager.read().await;
                            if let Some(ref manager) = *fuse {
                                manager.set_files(fuse_descriptors, Some(clip_data_id))
                            } else {
                                Vec::new()
                            }
                        };

                        if !paths.is_empty() {
                            let uri_content =
                                crate::clipboard::fuse::generate_gnome_copied_files_content(&paths);
                            let uri_bytes = uri_content.into_bytes();

                            info!(
                                "Delivering {} FUSE virtual file URI(s) via provider (serial={})",
                                paths.len(),
                                serial
                            );

                            if let Some(ref provider) = provider_opt {
                                match provider
                                    .complete_transfer(
                                        serial,
                                        "x-special/gnome-copied-files",
                                        uri_bytes,
                                        true,
                                    )
                                    .await
                                {
                                    Ok(()) => {
                                        info!(
                                            "FUSE file URIs delivered - files available for on-demand read"
                                        );
                                    }
                                    Err(e) => {
                                        error!("Failed to deliver FUSE URIs via provider: {:?}", e);
                                    }
                                }
                            }

                            return Ok(());
                        }
                        error!("FUSE failed to create virtual files - falling back to staging");
                    }

                    // Staging fallback: download files upfront (when FUSE not available)
                    info!("Using staging file transfer (FUSE not available)");

                    let sender_opt = server_event_sender.read().await.clone();
                    let sender = match sender_opt {
                        Some(s) => s,
                        None => {
                            error!(
                                "ServerEvent sender not available - cannot request file contents"
                            );
                            if let Some(ref provider) = provider_opt {
                                let _ = provider
                                    .complete_transfer(serial, &requested_mime, vec![], false)
                                    .await;
                            }
                            return Ok(());
                        }
                    };

                    {
                        let mut state = file_transfer_state.write().await;

                        state.clear_incoming();
                        state.set_pending_descriptors(descriptors.clone());
                        state.portal_serial = Some(serial);

                        use ironrdp_cliprdr::{
                            backend::ClipboardMessage,
                            pdu::{FileContentsFlags, FileContentsRequest},
                        };

                        let clip_data_id = 1u32;
                        info!("Sending Lock PDU (clip_data_id={})", clip_data_id);
                        if let Err(e) = sender.send(ironrdp_server::ServerEvent::Clipboard(
                            ClipboardMessage::SendLockClipboard { clip_data_id },
                        )) {
                            error!("Failed to send Lock PDU: {:?}", e);
                        }

                        for (idx, desc) in descriptors.iter().enumerate() {
                            let stream_id = state.allocate_stream_id();
                            let original_name = &desc.name;
                            let filename = sanitize_filename_for_linux(original_name);
                            let total_size = desc.size.unwrap_or(0);

                            if &filename != original_name {
                                info!("Requesting file {}/{}: '{}' -> '{}' (sanitized, {} bytes, stream_id={})",
                                    idx + 1, descriptors.len(), original_name, filename, total_size, stream_id);
                            } else {
                                info!(
                                    "Requesting file {}/{}: '{}' ({} bytes, stream_id={})",
                                    idx + 1,
                                    descriptors.len(),
                                    filename,
                                    total_size,
                                    stream_id
                                );
                            }

                            let temp_path = state
                                .download_dir
                                .join(format!(".{filename}.{stream_id}.tmp"));

                            if let Err(e) = std::fs::create_dir_all(&state.download_dir) {
                                error!("Failed to create download directory: {}", e);
                                continue;
                            }

                            let file_handle = match File::create(&temp_path) {
                                Ok(f) => f,
                                Err(e) => {
                                    error!(
                                        "Failed to create temp file '{}': {}",
                                        temp_path.display(),
                                        e
                                    );
                                    continue;
                                }
                            };

                            let incoming = IncomingFile {
                                stream_id,
                                filename: filename.clone(),
                                total_size,
                                received_size: 0,
                                temp_path,
                                file_handle,
                                file_index: idx as u32,
                                clip_data_id,
                            };
                            state.incoming_files.insert(stream_id, incoming);

                            let request_size = if total_size > 0 {
                                total_size.min(64 * 1024 * 1024) as u32
                            } else {
                                64 * 1024 * 1024
                            };

                            if let Err(e) = sender.send(ironrdp_server::ServerEvent::Clipboard(
                                ClipboardMessage::SendFileContentsRequest(FileContentsRequest {
                                    stream_id,
                                    index: idx as u32,
                                    flags: FileContentsFlags::DATA,
                                    position: 0,
                                    requested_size: request_size,
                                    data_id: Some(clip_data_id),
                                }),
                            )) {
                                error!(
                                    "Failed to send FileContentsRequest for '{}': {:?}",
                                    filename, e
                                );
                            } else {
                                info!("Sent FileContentsRequest for '{}' (stream={}, {} bytes, clip_data_id={})",
                                    filename, stream_id, request_size, clip_data_id);
                            }
                        }

                        info!(
                            "Initiated staging transfer for {} file(s), waiting for responses...",
                            state.incoming_files.len()
                        );
                    }

                    return Ok(());
                }
                Err(e) => {
                    error!("Failed to parse FileGroupDescriptorW: {:?}", e);
                    // Fall through to generic handling
                }
            }
        }

        let portal_data = if requested_mime.starts_with("image/png") {
            // Portal wants PNG, Windows sent DIB or DIBV5
            // Auto-detect format based on header size
            if data.len() >= 4 {
                let header_size = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
                match header_size {
                    124 => {
                        // DIBV5 format with alpha channel
                        trace!(" Converting DIBV5 to PNG for Portal (with alpha)");
                        lamco_clipboard_core::image::dibv5_to_png(&data).map_err(|e| {
                            error!("DIBV5 to PNG conversion failed: {}", e);
                            ClipboardError::Core(e)
                        })?
                    }
                    40 => {
                        // Standard DIB format
                        trace!(" Converting DIB to PNG for Portal");
                        lamco_clipboard_core::image::dib_to_png(&data).map_err(|e| {
                            error!("DIB to PNG conversion failed: {}", e);
                            ClipboardError::Core(e)
                        })?
                    }
                    _ => {
                        // Unknown header size, try DIBV5 parser which handles both
                        debug!(
                            "Unknown bitmap header size {}, trying auto-detect",
                            header_size
                        );
                        lamco_clipboard_core::image::dibv5_to_png(&data).map_err(|e| {
                            error!("Bitmap to PNG conversion failed: {}", e);
                            ClipboardError::Core(e)
                        })?
                    }
                }
            } else {
                error!(
                    "Image data too small for bitmap header: {} bytes",
                    data.len()
                );
                return Err(ClipboardError::Core(
                    lamco_clipboard_core::ClipboardError::ImageDecode(
                        "Data too small for bitmap".to_string(),
                    ),
                ));
            }
        } else if requested_mime.starts_with("image/jpeg") {
            // Portal wants JPEG, Windows sent DIB or DIBV5
            if data.len() >= 4 {
                let header_size = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
                if header_size == 124 {
                    trace!(" Converting DIBV5 to JPEG for Portal");
                    lamco_clipboard_core::image::dibv5_to_jpeg(&data).map_err(|e| {
                        error!("DIBV5 to JPEG conversion failed: {}", e);
                        ClipboardError::Core(e)
                    })?
                } else {
                    trace!(" Converting DIB to JPEG for Portal");
                    lamco_clipboard_core::image::dib_to_jpeg(&data).map_err(|e| {
                        error!("DIB to JPEG conversion failed: {}", e);
                        ClipboardError::Core(e)
                    })?
                }
            } else {
                error!(
                    "Image data too small for bitmap header: {} bytes",
                    data.len()
                );
                return Err(ClipboardError::Core(
                    lamco_clipboard_core::ClipboardError::ImageDecode(
                        "Data too small for bitmap".to_string(),
                    ),
                ));
            }
        } else if requested_mime.starts_with("image/bmp")
            || requested_mime.starts_with("image/x-bmp")
        {
            // Portal wants BMP, Windows sent DIB
            trace!(" Converting DIB to BMP for Portal");
            lamco_clipboard_core::image::dib_to_bmp(&data).map_err(|e| {
                error!("DIB to BMP conversion failed: {}", e);
                ClipboardError::Core(e)
            })?
        } else if requested_mime == "text/rtf" || requested_mime == "application/rtf" {
            // RTF is plain ASCII/Latin-1 text, NOT UTF-16
            // Windows CF_RTF sends raw RTF markup as bytes
            debug!(
                "RTF format detected ({} bytes) - passing through with line ending conversion",
                data.len()
            );

            // Convert to string (lossy for any invalid UTF-8, though RTF should be ASCII)
            let text = String::from_utf8_lossy(&data);

            // Sanitize for Linux: CRLF → LF, remove null bytes
            let sanitized = sanitize_text_for_linux(&text);
            let rtf_bytes = sanitized.as_bytes().to_vec();

            debug!(
                "RTF: {} raw bytes → {} bytes after line ending conversion",
                data.len(),
                rtf_bytes.len()
            );
            if !rtf_bytes.is_empty() {
                let preview_len = rtf_bytes.len().min(80);
                debug!(
                    "RTF preview: {:?}",
                    String::from_utf8_lossy(&rtf_bytes[..preview_len])
                );
            }
            rtf_bytes
        } else if (requested_mime.starts_with("text/plain")
            || requested_mime.starts_with("text/html"))
            && data.len() >= 2
        {
            // text/plain and text/html from Windows are UTF-16LE (CF_UNICODETEXT)
            // MIME may have charset suffix like "text/plain;charset=utf-8"
            // Convert UTF-16LE to UTF-8 with line ending conversion
            let utf16_data: Vec<u16> = data
                .chunks_exact(2)
                .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]))
                .take_while(|&c| c != 0) // Stop at null terminator
                .collect();

            // Use lossy conversion to handle malformed UTF-16
            // This handles invalid surrogates and replaces them with U+FFFD
            let text = String::from_utf16_lossy(&utf16_data);

            // Sanitize for Linux: CRLF → LF, remove null bytes
            let sanitized = sanitize_text_for_linux(&text);
            let utf8_bytes = sanitized.as_bytes().to_vec();

            debug!(
                "Converted UTF-16 to UTF-8: {} UTF-16 chars ({} bytes) → {} UTF-8 bytes with LF line endings",
                utf16_data.len(),
                data.len(),
                utf8_bytes.len()
            );
            if !sanitized.is_empty() {
                debug!("Text preview: {:?}", &sanitized[..sanitized.len().min(50)]);
            }
            utf8_bytes
        } else {
            // Unknown format or too small - pass through
            debug!(
                "Unknown format or small data, using raw {} bytes",
                data.len()
            );
            data
        };

        // Deliver converted data via clipboard provider
        let provider = provider_opt.as_ref().expect("provider checked above");

        match provider
            .complete_transfer(serial, &requested_mime, portal_data, true)
            .await
        {
            Ok(()) => {
                info!(
                    "Clipboard data delivered via {} provider (serial {})",
                    provider.name(),
                    serial
                );

                // Cancel unfulfilled requests (apps send multiple MIME requests per paste)
                let mut pending = pending_portal_requests.write().await;
                let unfulfilled: Vec<(u32, String)> = pending
                    .iter()
                    .filter(|(s, _, _)| *s != serial)
                    .map(|(s, m, _)| (*s, m.clone()))
                    .collect();
                pending.clear();
                drop(pending);

                for (unfulfilled_serial, mime) in &unfulfilled {
                    if let Err(e) = provider
                        .complete_transfer(*unfulfilled_serial, mime, vec![], false)
                        .await
                    {
                        warn!("Failed to cancel serial {}: {}", unfulfilled_serial, e);
                    }
                }
            }
            Err(e) => {
                error!("Failed to deliver clipboard data via provider: {:#}", e);
                pending_portal_requests
                    .write()
                    .await
                    .retain(|(s, _, _)| *s != serial);
            }
        }

        Ok(())
    }

    /// Provider-based data response handler
    ///
    /// Called when clipboard_provider is set but legacy Portal is unavailable.
    /// Mirrors the logic of handle_rdp_data_response but uses the provider's
    /// complete_transfer() API instead of Portal's write_selection_data/selection_write_done.
    /// Handle RDP data error (must notify clipboard provider to prevent retry crash)
    ///
    /// This is called when the RDP client responds with FormatDataResponse(error=true),
    /// which is normal protocol behavior when the client doesn't have the requested format.
    /// Per MS-RDPECLIP, this is expected and not an error condition.
    async fn handle_rdp_data_error(
        clipboard_provider: &SharedClipboardProvider,
        pending_portal_requests: &PendingPortalRequests,
    ) -> Result<()> {
        debug!("RDP FormatDataResponse: format not available, notifying clipboard backend");

        let pending = pending_portal_requests.read().await;
        let entries: Vec<(u32, String)> = pending.iter().map(|(s, m, _)| (*s, m.clone())).collect();
        drop(pending);

        if let Some(ref provider) = *clipboard_provider.read().await {
            for (serial, mime_type) in &entries {
                debug!(
                    "Notifying {} provider of transfer failure (serial {})",
                    provider.name(),
                    serial
                );
                if let Err(e) = provider
                    .complete_transfer(*serial, mime_type, vec![], false)
                    .await
                {
                    warn!("Failed to notify provider of transfer failure: {:#}", e);
                }
            }
        } else {
            warn!("No clipboard provider available to notify of transfer failure");
        }

        pending_portal_requests.write().await.clear();
        Ok(())
    }

    /// Handle Portal format announcement (Linux → Windows)
    ///
    /// `force=true` from D-Bus extension overrides RDP ownership; `force=false` may be blocked.
    #[expect(
        clippy::too_many_arguments,
        reason = "orchestrator handler with shared state refs"
    )]
    async fn handle_portal_formats(
        mime_types: Vec<String>,
        force: bool,
        converter: &FormatConverter,
        sync_manager: &Arc<RwLock<SyncManager>>,
        server_event_sender: &ServerEventSender,
        local_advertised_formats: &Arc<RwLock<Vec<ClipboardFormat>>>,
        current_rdp_formats: &Arc<RwLock<Vec<ClipboardFormat>>>,
        clipboard_provider: &SharedClipboardProvider,
        last_reannounce_time: &Arc<RwLock<Option<std::time::SystemTime>>>,
        reannounce_count: &Arc<RwLock<HashMap<Vec<u32>, u32>>>,
    ) -> Result<()> {
        use std::time::{Duration, SystemTime};

        info!(
            "handle_portal_formats called with {} MIME types (force={}): {:?}",
            mime_types.len(),
            force,
            mime_types
        );

        let sync_decision = {
            let mut mgr = sync_manager.write().await;
            mgr.handle_portal_formats(mime_types.clone(), force)
        };

        match sync_decision {
            crate::clipboard::sync::PortalSyncDecision::Allow => {
                // Normal Linux → Windows sync
                debug!("Sync decision: Allow - proceeding with normal sync");
            }

            crate::clipboard::sync::PortalSyncDecision::Block => {
                debug!("Sync decision: Block - skipping Portal formats");
                return Ok(());
            }

            crate::clipboard::sync::PortalSyncDecision::KlipperReannounce => {
                // Klipper took over clipboard - re-announce RDP formats to reclaim ownership
                info!("┌─ Klipper Takeover Mitigation ─────────────────────────────");
                info!("│ Klipper has taken clipboard ownership");

                // GUARD 1: Time-based (prevent rapid reannouncements)
                let time_ok = {
                    let last_time = last_reannounce_time.read().await;
                    match *last_time {
                        Some(t) => {
                            let elapsed = SystemTime::now()
                                .duration_since(t)
                                .unwrap_or(Duration::from_secs(999))
                                .as_millis();

                            if elapsed < 500 {
                                warn!("│ SKIP: Reannounced {}ms ago (< 500ms guard)", elapsed);
                                info!(
                                    "└────────────────────────────────────────────────────────────"
                                );
                                return Ok(());
                            } else {
                                debug!("│ Time guard OK: {}ms since last reannounce", elapsed);
                                true
                            }
                        }
                        None => {
                            debug!("│ First reannounce - no time guard");
                            true
                        }
                    }
                };

                if !time_ok {
                    info!("└────────────────────────────────────────────────────────────");
                    return Ok(());
                }

                // GUARD 2: Count-based (max 2 reannouncements per RDP format list)
                let count_ok = {
                    let stored_formats = current_rdp_formats.read().await;
                    let mut format_ids: Vec<u32> = stored_formats.iter().map(|f| f.id).collect();
                    format_ids.sort_unstable(); // Ensure consistent ordering for HashMap key

                    let mut counts = reannounce_count.write().await;
                    let count = counts.entry(format_ids).or_insert(0);

                    if *count >= 2 {
                        warn!(
                            "│ SKIP: Already reannounced {} times for this RDP copy",
                            count
                        );
                        warn!("│ Accepting Klipper ownership to prevent infinite loop");
                        info!("└────────────────────────────────────────────────────────────");
                        return Ok(());
                    } else {
                        *count += 1;
                        info!("│ Reannounce attempt #{} (max 2 allowed)", count);
                        true
                    }
                };

                if !count_ok {
                    info!("└────────────────────────────────────────────────────────────");
                    return Ok(());
                }

                // RE-ANNOUNCE: Call SetSelection again with original RDP formats
                let stored = current_rdp_formats.read().await;

                if stored.is_empty() {
                    warn!("│ No RDP formats stored to re-announce");
                    info!("└────────────────────────────────────────────────────────────");
                    return Ok(());
                }

                let reannounce_mimes = converter.rdp_to_mime_types(&stored)?;

                info!(
                    "│ Re-announcing {} RDP formats as {} MIME types",
                    stored.len(),
                    reannounce_mimes.len()
                );
                debug!("│ MIME types: {:?}", reannounce_mimes);

                if let Some(ref provider) = *clipboard_provider.read().await {
                    match provider.announce_formats(reannounce_mimes).await {
                        Ok(()) => {
                            info!(
                                "│ SetSelection succeeded via {} provider - ownership reclaimed",
                                provider.name()
                            );
                            *last_reannounce_time.write().await = Some(SystemTime::now());
                        }
                        Err(e) => {
                            error!("│ Provider announce_formats failed: {:#}", e);
                        }
                    }
                } else {
                    warn!("│ No clipboard provider available for reannounce");
                }

                info!("└────────────────────────────────────────────────────────────");
                return Ok(());
            }
        }

        let rdp_formats = converter.mime_to_rdp_formats(&mime_types)?;
        debug!(
            "Converted {} MIME types to {} RDP formats",
            mime_types.len(),
            rdp_formats.len()
        );

        let ironrdp_formats: Vec<ironrdp_cliprdr::pdu::ClipboardFormat> = rdp_formats
            .iter()
            .map(|f| {
                let name = if !f.format_name.is_empty() {
                    Some(ironrdp_cliprdr::pdu::ClipboardFormatName::new(
                        f.format_name.clone(),
                    ))
                } else {
                    None
                };
                ironrdp_cliprdr::pdu::ClipboardFormat {
                    id: ironrdp_cliprdr::pdu::ClipboardFormatId(f.format_id),
                    name,
                }
            })
            .collect();

        {
            let mut advertised = local_advertised_formats.write().await;
            advertised.clear();
            for fmt in &ironrdp_formats {
                advertised.push(ClipboardFormat {
                    id: fmt.id.0,
                    name: fmt.name.as_ref().map(|n| n.value().to_string()),
                });
            }
            debug!(
                "Stored {} advertised formats for data request lookup",
                advertised.len()
            );
        }

        debug!(" Sending FormatList to RDP client:");
        for (idx, fmt) in ironrdp_formats.iter().enumerate() {
            let name_str = fmt
                .name
                .as_ref()
                .map_or("", ironrdp_cliprdr::pdu::ClipboardFormatName::value);
            info!("   Format {}: ID={}, Name={:?}", idx, fmt.id.0, name_str);
        }

        let sender_opt = server_event_sender.read().await.clone();
        if let Some(sender) = sender_opt {
            use ironrdp_cliprdr::backend::ClipboardMessage;

            info!(
                "Sending ServerEvent::Clipboard(SendInitiateCopy) with {} formats to event loop",
                ironrdp_formats.len()
            );

            let send_result = sender.send(ironrdp_server::ServerEvent::Clipboard(
                ClipboardMessage::SendInitiateCopy(ironrdp_formats),
            ));

            match send_result {
                Ok(()) => {
                    debug!(" ServerEvent::Clipboard sent successfully to IronRDP event loop");
                    info!("   Event loop should now call cliprdr.initiate_copy() → encode FormatList PDU → send to client");
                }
                Err(e) => {
                    error!("Failed to send ServerEvent::Clipboard: {:?}", e);
                    error!("   This means the event loop channel is closed/dropped!");
                }
            }
        } else {
            warn!("ServerEvent sender not available - cannot announce formats to RDP");
        }

        Ok(())
    }

    /// Handle Portal data request (Windows → Linux paste initiation)
    ///
    /// When a Linux app pastes while RDP owns the clipboard, Portal sends
    /// SelectionTransfer which becomes PortalDataRequest. We ask the RDP
    /// client to supply the data via SendInitiatePaste.
    async fn handle_portal_data_request(
        mime_type: String,
        converter: &FormatConverter,
        _sync_manager: &Arc<RwLock<SyncManager>>,
        server_event_sender: &ServerEventSender,
        current_rdp_formats: &Arc<RwLock<Vec<ClipboardFormat>>>,
    ) -> Result<()> {
        debug!("Portal data request for MIME type: {}", mime_type);

        // Look up the format ID from the actual Windows format list first.
        // This handles runtime-registered formats (FileGroupDescriptorW etc.)
        // whose IDs vary per session.
        let formats = current_rdp_formats.read().await;
        let format_id = if let Some(id) = lookup_format_id_for_mime(&formats, &mime_type) {
            id
        } else {
            // Fall back to static mapping for standard formats
            drop(formats);
            converter.mime_to_format_id(&mime_type)?
        };

        info!(
            "Portal needs data — requesting format {} from RDP client (MIME: {})",
            format_id, mime_type
        );

        let sender_opt = server_event_sender.read().await.clone();
        if let Some(sender) = sender_opt {
            use ironrdp_cliprdr::{backend::ClipboardMessage, pdu::ClipboardFormatId};

            if let Err(e) = sender.send(ironrdp_server::ServerEvent::Clipboard(
                ClipboardMessage::SendInitiatePaste(ClipboardFormatId(format_id)),
            )) {
                error!(
                    "Failed to send SendInitiatePaste for format {}: {:?}",
                    format_id, e
                );
            }
        } else {
            warn!(
                "ServerEvent sender not available — cannot request format {} from RDP client",
                format_id
            );
        }

        Ok(())
    }

    // PortalDataResponse handler removed — nothing sends this event variant.
    // Data flows directly: handle_rdp_data_request → Portal read_data → SendFormatData.

    /// Handle RDP file contents request (Linux → Windows file transfer)
    async fn handle_rdp_file_contents_request(
        stream_id: u32,
        list_index: u32,
        position: u64,
        requested_size: u32,
        is_size_request: bool,
        server_event_sender: &ServerEventSender,
        file_transfer_state: &Arc<RwLock<FileTransferState>>,
    ) -> Result<()> {
        info!(
            "FileContentsRequest: stream={}, index={}, pos={}, size={}, size_req={}",
            stream_id, list_index, position, requested_size, is_size_request
        );

        let sender = match server_event_sender.read().await.as_ref() {
            Some(s) => s.clone(),
            None => {
                error!("ServerEvent sender not available for file transfer");
                return Err(ClipboardError::NotInitialized);
            }
        };

        let state = file_transfer_state.read().await;
        let file_info = state
            .outgoing_files
            .get(list_index as usize)
            .ok_or_else(|| {
                error!(
                    "Invalid file list index: {} (have {} files)",
                    list_index,
                    state.outgoing_files.len()
                );
                ClipboardError::InvalidState(format!("File index {list_index} not found"))
            })?;

        use ironrdp_cliprdr::{backend::ClipboardMessage, pdu::FileContentsResponse};

        if is_size_request {
            info!(
                "Returning file size: {} bytes for '{}'",
                file_info.size, file_info.filename
            );

            let response = FileContentsResponse::new_size_response(stream_id, file_info.size);
            info!(
                "Sending FileContentsResponse(stream={}, size={})",
                stream_id, file_info.size
            );

            if let Err(e) = sender.send(ironrdp_server::ServerEvent::Clipboard(
                ClipboardMessage::SendFileContentsResponse(response),
            )) {
                error!("Failed to send FileContentsResponse: {:?}", e);
            }
        } else {
            let path = file_info.path.clone();
            let file_size = file_info.size;
            drop(state); // Release lock before file I/O

            match Self::read_file_chunk(&path, position, requested_size) {
                Ok(data) => {
                    info!(
                        "Read {} bytes from '{}' at offset {} (file size: {})",
                        data.len(),
                        path.display(),
                        position,
                        file_size
                    );

                    let response = FileContentsResponse::new_data_response(stream_id, data.clone());
                    info!(
                        "Sending FileContentsResponse(stream={}, {} bytes)",
                        stream_id,
                        data.len()
                    );

                    if let Err(e) = sender.send(ironrdp_server::ServerEvent::Clipboard(
                        ClipboardMessage::SendFileContentsResponse(response),
                    )) {
                        error!("Failed to send FileContentsResponse: {:?}", e);
                    }
                }
                Err(e) => {
                    error!("Failed to read file '{}': {}", path.display(), e);

                    let response = FileContentsResponse::new_error(stream_id);
                    info!("Sending FileContentsResponse ERROR (stream={})", stream_id);

                    if let Err(e) = sender.send(ironrdp_server::ServerEvent::Clipboard(
                        ClipboardMessage::SendFileContentsResponse(response),
                    )) {
                        error!("Failed to send FileContentsResponse error: {:?}", e);
                    }
                }
            }
        }

        Ok(())
    }

    /// Read a chunk from a file
    fn read_file_chunk(path: &PathBuf, offset: u64, size: u32) -> Result<Vec<u8>> {
        let mut file = File::open(path)
            .map_err(|e| ClipboardError::FileIoError(format!("Failed to open file: {e}")))?;

        file.seek(SeekFrom::Start(offset)).map_err(|e| {
            ClipboardError::FileIoError(format!("Failed to seek to offset {offset}: {e}"))
        })?;

        let mut buffer = vec![0u8; size as usize];
        let bytes_read = file
            .read(&mut buffer)
            .map_err(|e| ClipboardError::FileIoError(format!("Failed to read file: {e}")))?;

        buffer.truncate(bytes_read);
        Ok(buffer)
    }

    /// Handle RDP file contents response - Linux receives file from Windows
    ///
    /// Called when Windows client provides file data chunks.
    /// For files >64MB, requests continuation chunks until complete.
    /// When all files are complete, delivers file:// URIs to Portal.
    async fn handle_rdp_file_contents_response(
        stream_id: u32,
        data: Vec<u8>,
        is_error: bool,
        file_transfer_state: &Arc<RwLock<FileTransferState>>,
        clipboard_provider: &SharedClipboardProvider,
        server_event_sender: &ServerEventSender,
    ) -> Result<()> {
        if is_error {
            warn!("FileContentsResponse ERROR: stream={}", stream_id);

            let mut state = file_transfer_state.write().await;
            if let Some(file) = state.incoming_files.remove(&stream_id) {
                info!("Cleaning up failed transfer: {}", file.filename);
                let _ = std::fs::remove_file(&file.temp_path);
            }

            if let Some(serial) = state.portal_serial.take() {
                drop(state);
                if let Some(ref provider) = *clipboard_provider.read().await {
                    let _ = provider.complete_transfer(serial, "", vec![], false).await;
                }
            }

            return Ok(());
        }

        info!(
            "FileContentsResponse [v2]: stream={}, {} bytes",
            stream_id,
            data.len()
        );

        let mut state = file_transfer_state.write().await;
        let download_dir = state.download_dir.clone();

        let file = match state.incoming_files.get_mut(&stream_id) {
            Some(f) => f,
            None => {
                warn!(
                    "Received FileContentsResponse for unknown stream {}",
                    stream_id
                );
                return Ok(());
            }
        };

        file.file_handle.write_all(&data).map_err(|e| {
            error!(
                "Failed to write {} bytes to '{}': {}",
                data.len(),
                file.temp_path.display(),
                e
            );
            ClipboardError::FileIoError(format!("File write failed: {e}"))
        })?;

        file.received_size += data.len() as u64;

        let percent = if file.total_size > 0 {
            (file.received_size as f64 / file.total_size as f64) * 100.0
        } else {
            100.0
        };
        info!(
            "Progress: '{}' - {}/{} bytes ({:.1}%)",
            file.filename,
            file.received_size,
            if file.total_size > 0 {
                file.total_size
            } else {
                file.received_size
            },
            percent
        );

        let file_complete = file.total_size > 0 && file.received_size >= file.total_size;

        if file_complete {
            debug!(" File transfer complete: '{}'", file.filename);

            file.file_handle
                .flush()
                .map_err(|e| ClipboardError::FileIoError(format!("Failed to flush file: {e}")))?;

            let temp_path = file.temp_path.clone();
            let filename = file.filename.clone();

            let final_path = download_dir.join(&filename);
            state.completed_files.push(final_path.clone());
            state.incoming_files.remove(&stream_id);

            let all_complete = state.incoming_files.is_empty();
            let portal_serial = state.portal_serial;
            let completed_files = state.completed_files.clone();
            drop(state); // Release lock before file operation

            std::fs::rename(&temp_path, &final_path).map_err(|e| {
                error!(
                    "Failed to move '{}' to '{}': {}",
                    temp_path.display(),
                    final_path.display(),
                    e
                );
                ClipboardError::FileIoError(format!("Failed to finalize file: {e}"))
            })?;

            info!("Saved file to: {}", final_path.display());

            if all_complete {
                debug!(
                    "All {} file(s) transferred successfully",
                    completed_files.len()
                );

                // Only encode characters problematic in URIs, NOT dots/dashes/underscores
                use percent_encoding::{utf8_percent_encode, AsciiSet, CONTROLS};
                const FILE_URI_ENCODE: &AsciiSet = &CONTROLS
                    .add(b' ')
                    .add(b'"')
                    .add(b'#')
                    .add(b'%')
                    .add(b'<')
                    .add(b'>')
                    .add(b'?')
                    .add(b'`')
                    .add(b'{')
                    .add(b'}');
                let uris: Vec<String> = completed_files
                    .iter()
                    .map(|path| {
                        let path_str = path.to_string_lossy();
                        let encoded: String = path_str
                            .split('/')
                            .map(|component| {
                                utf8_percent_encode(component, FILE_URI_ENCODE).to_string()
                            })
                            .collect::<Vec<_>>()
                            .join("/");
                        format!("file://{encoded}")
                    })
                    .collect();

                // x-special/gnome-copied-files format: "copy\nfile:///path1\nfile:///path2\0" (null-terminated)
                let uri_list = format!("copy\n{}\0", uris.join("\n"));

                debug!(
                    "Generated URI list (gnome-copied-files format): {:?}",
                    uri_list
                );

                if let Some(serial) = portal_serial {
                    let uri_bytes = uri_list.into_bytes();

                    if let Some(ref provider) = *clipboard_provider.read().await {
                        match provider
                            .complete_transfer(
                                serial,
                                "x-special/gnome-copied-files",
                                uri_bytes,
                                true,
                            )
                            .await
                        {
                            Ok(()) => {
                                info!(
                                    "Delivered {} file URI(s) via {} provider (serial={})",
                                    completed_files.len(),
                                    provider.name(),
                                    serial
                                );
                            }
                            Err(e) => {
                                error!("Failed to deliver URIs via provider: {:?}", e);
                            }
                        }
                    } else {
                        warn!("No clipboard provider available to deliver file URIs");
                    }
                }

                let mut state = file_transfer_state.write().await;
                state.completed_files.clear();
                state.portal_serial = None;
            }
        } else if file.total_size > 0 {
            // File is NOT complete - need to request the next chunk
            let remaining = file.total_size - file.received_size;
            let next_chunk_size = remaining.min(64 * 1024 * 1024) as u32; // Max 64MB per request
            let position = file.received_size;
            let file_index = file.file_index;
            let clip_data_id = file.clip_data_id;
            let filename = file.filename.clone();
            drop(state); // Release lock before sending

            if let Some(sender) = server_event_sender.read().await.as_ref() {
                use ironrdp_cliprdr::{
                    backend::ClipboardMessage,
                    pdu::{FileContentsFlags, FileContentsRequest},
                };

                info!(
                    "Requesting next chunk for '{}' (pos={}, size={}, remaining={})",
                    filename, position, next_chunk_size, remaining
                );

                if let Err(e) = sender.send(ironrdp_server::ServerEvent::Clipboard(
                    ClipboardMessage::SendFileContentsRequest(FileContentsRequest {
                        stream_id,
                        index: file_index,
                        flags: FileContentsFlags::DATA,
                        position,
                        requested_size: next_chunk_size,
                        data_id: Some(clip_data_id),
                    }),
                )) {
                    error!("Failed to send continuation FileContentsRequest: {:?}", e);
                }
            } else {
                error!("ServerEvent sender not available for chunk continuation");
            }
        }

        Ok(())
    }

    /// Shutdown the clipboard manager
    ///
    /// Sends a shutdown signal to the event loop if it's running.
    /// If the event loop hasn't been started, this is a no-op.
    /// Clear Portal clipboard selection
    ///
    /// Calls Portal SetSelection with empty MIME types to clear clipboard.
    /// This cancels pending clipboard operations and prevents callbacks
    /// from firing after disconnect.
    ///
    /// # Use Cases
    ///
    /// - On RDP disconnect: Prevents stale clipboard operations
    /// - Before shutdown: Cleans up Portal state
    /// - On reconnect: Resets clipboard for new session
    ///
    /// # Errors
    ///
    /// Returns error if Portal not available or SetSelection fails.
    /// Non-fatal - continue shutdown even if this fails.
    pub async fn shutdown(&mut self) -> Result<()> {
        info!("Clipboard orchestrator shutdown starting");

        // Shut down the clipboard provider
        if let Some(provider) = self.clipboard_provider.read().await.as_ref() {
            provider.shutdown().await;
            info!("Clipboard provider shut down");
        }

        if let Some(ref tx) = self.shutdown_tx {
            if let Err(e) = tx.send(()).await {
                warn!("Failed to send shutdown signal to event processor: {}", e);
            }
        }

        let _ = self.shutdown_broadcast.send(());

        let task_count = {
            let handles = self.task_handles.lock().await;
            handles.len()
        };

        if task_count > 0 {
            let timeout = tokio::time::Duration::from_secs(5);
            let mut handles = self.task_handles.lock().await;

            for (i, handle) in handles.drain(..).enumerate() {
                match tokio::time::timeout(timeout, handle).await {
                    Ok(Ok(())) => {
                        debug!("Task {} finished cleanly", i + 1);
                    }
                    Ok(Err(e)) => {
                        warn!("Task {} panicked: {:?}", i + 1, e);
                    }
                    Err(_) => {
                        warn!("Task {} timed out, aborting", i + 1);
                    }
                }
            }
        }

        if let Some(coord) = self.cooperation_coordinator.write().await.take() {
            drop(coord);
            info!("Cooperation coordinator stopped");
        }

        self.shutdown_tx = None;

        info!("Clipboard orchestrator shutdown complete");

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_clipboard_manager_creation() {
        let config = ClipboardOrchestratorConfig::default();
        let manager = ClipboardOrchestrator::new(config).await.unwrap();

        assert!(manager.event_tx.capacity() > 0);
    }

    #[tokio::test]
    async fn test_rdp_format_list_handling() {
        let config = ClipboardOrchestratorConfig::default();
        let manager = ClipboardOrchestrator::new(config).await.unwrap();

        let formats = vec![ClipboardFormat::with_name(13, "CF_UNICODETEXT")];
        let event = ClipboardEvent::RdpFormatList(formats);
        manager.event_tx.send(event).await.unwrap();
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
    }

    #[tokio::test]
    async fn test_shutdown() {
        let config = ClipboardOrchestratorConfig::default();
        let mut manager = ClipboardOrchestrator::new(config).await.unwrap();
        manager.shutdown().await.unwrap();
    }
}
