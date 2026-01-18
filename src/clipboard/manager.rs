//! Clipboard Manager
//!
//! Main clipboard synchronization coordinator that manages bidirectional
//! clipboard sharing between RDP client and Wayland compositor.
//!
//! # Architecture
//!
//! The manager uses library types from the lamco crate ecosystem:
//! - `lamco-clipboard-core` - Format conversion, transfer engine
//! - `lamco-portal` - D-Bus clipboard bridge
//!
//! Server-specific types from this crate:
//! - `SyncManager` - State machine with echo protection
//! - `ClipboardEvent` - Server event routing

use crate::clipboard::error::{ClipboardError, Result};
use crate::clipboard::sync::{ClipboardState, SyncManager};
use crate::clipboard::FormatConverterExt; // Extension trait for converter methods
use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex, RwLock};
use tracing::{debug, error, info, trace, warn};

// Import from lamco crates
use lamco_clipboard_core::{
    sanitize::{
        parse_file_uris, sanitize_filename_for_linux, sanitize_text_for_linux,
        sanitize_text_for_windows,
    },
    ClipboardFormat, FormatConverter, LoopDetectionConfig, TransferConfig, TransferEngine,
};
use lamco_portal::dbus_clipboard::DbusClipboardBridge;

/// Clipboard configuration
#[derive(Debug, Clone)]
pub struct ClipboardConfig {
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
}

impl Default for ClipboardConfig {
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
            rate_limit_ms: 200, // Max 5 events/second
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
            Self::RdpDataRequest(id, _) => write!(f, "RdpDataRequest({})", id),
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
                    "RdpFileContentsRequest(stream={}, index={}, size={}, size_req={})",
                    stream_id, list_index, size, is_size_request
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
                write!(f, "PortalFormatsAvailable({:?}, force={})", mimes, force)
            }
            Self::PortalDataRequest(mime) => write!(f, "PortalDataRequest({})", mime),
            Self::PortalDataResponse(data) => write!(f, "PortalDataResponse({} bytes)", data.len()),
        }
    }
}

/// Clipboard manager coordinates all clipboard operations
pub struct ClipboardManager {
    /// Configuration
    config: ClipboardConfig,

    /// Format converter
    converter: Arc<FormatConverter>,

    /// Transfer engine
    transfer_engine: Arc<TransferEngine>,

    /// Synchronization manager
    sync_manager: Arc<RwLock<SyncManager>>,

    /// Event sender
    event_tx: mpsc::Sender<ClipboardEvent>,

    /// Shutdown signal
    shutdown_tx: Option<mpsc::Sender<()>>,

    /// Portal clipboard manager for read/write operations (wrapped for dynamic update)
    portal_clipboard: Arc<RwLock<Option<Arc<crate::portal::PortalClipboardManager>>>>,

    /// Portal session (shared with input handler, wrapped for concurrent access and dynamic update)
    portal_session: Arc<
        RwLock<
            Option<
                Arc<
                    RwLock<
                        ashpd::desktop::Session<
                            'static,
                            ashpd::desktop::remote_desktop::RemoteDesktop<'static>,
                        >,
                    >,
                >,
            >,
        >,
    >,

    /// Pending Portal SelectionTransfer requests (FIFO queue)
    /// Each entry: (serial, mime_type, request_time)
    /// Used to correlate SelectionTransfer signals with RDP FormatDataResponse in order
    pending_portal_requests:
        Arc<RwLock<std::collections::VecDeque<(u32, String, std::time::Instant)>>>,

    /// Server event sender for sending clipboard requests to IronRDP
    /// Set by LamcoCliprdrFactory after ServerEvent sender is available
    server_event_sender: Arc<RwLock<Option<mpsc::UnboundedSender<ironrdp_server::ServerEvent>>>>,

    /// D-Bus bridge for GNOME clipboard extension (Portal signals unreliable on GNOME)
    dbus_bridge: Arc<RwLock<Option<DbusClipboardBridge>>>,

    /// Recently written content hashes (for loop suppression)
    /// When we write data to Portal, D-Bus bridge will see it as a clipboard change.
    /// We track hashes of data WE wrote to suppress forwarding it back to RDP.
    /// Maps hash → timestamp of write
    recently_written_hashes: Arc<RwLock<std::collections::HashMap<String, std::time::Instant>>>,

    /// File transfer state (for handling file clipboard operations)
    file_transfer_state: Arc<RwLock<FileTransferState>>,

    /// FUSE filesystem manager for on-demand file transfer
    fuse_manager: Arc<RwLock<Option<crate::clipboard::fuse::FuseManager>>>,

    /// Channel sender for FUSE file content requests
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
    #[allow(dead_code)] // Used for debugging
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
    #[allow(dead_code)] // Used for multi-file tracking
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

    #[allow(dead_code)] // WIP: File transfer cleanup
    fn clear_pending_descriptors(&mut self) {
        self.pending_descriptors.clear();
    }

    /// Get the next stream ID and increment the counter
    fn allocate_stream_id(&mut self) -> u32 {
        let id = self.next_stream_id;
        self.next_stream_id = self.next_stream_id.wrapping_add(1);
        id
    }

    /// Check if all incoming files are complete
    #[allow(dead_code)] // WIP: File transfer completion check
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
        // Check if CF_UNICODETEXT is available
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

impl std::fmt::Debug for ClipboardManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClipboardManager")
            .field("config", &self.config)
            .field(
                "has_portal_clipboard",
                &self
                    .portal_clipboard
                    .try_read()
                    .map(|g| g.is_some())
                    .unwrap_or(false),
            )
            .field(
                "has_dbus_bridge",
                &self
                    .dbus_bridge
                    .try_read()
                    .map(|g| g.is_some())
                    .unwrap_or(false),
            )
            .finish_non_exhaustive()
    }
}

impl ClipboardManager {
    /// Create a new clipboard manager
    pub async fn new(config: ClipboardConfig) -> Result<Self> {
        let converter = Arc::new(FormatConverter::new());

        // Configure transfer engine with library types
        let transfer_config = TransferConfig {
            chunk_size: config.chunk_size,
            max_size: config.max_data_size,
            timeout_ms: config.timeout_ms,
            verify_integrity: true,
        };
        let transfer_engine = Arc::new(TransferEngine::with_config(transfer_config));

        // Configure loop detection with rate limiting if enabled
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
        // SyncManager now creates its own LoopDetector from config
        let sync_manager = Arc::new(RwLock::new(SyncManager::with_config(loop_config)));

        let (event_tx, event_rx) = mpsc::channel(100);

        // Create file transfer state with downloads directory
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

        // Create FUSE request channel (will be used to handle on-demand file reads)
        let (fuse_request_tx, fuse_request_rx) =
            mpsc::channel::<crate::clipboard::fuse::FileContentsRequest>(32);

        // Create FUSE manager (mount will happen when needed)
        let fuse_manager = match crate::clipboard::fuse::FuseManager::new(fuse_request_tx.clone()) {
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

        let mut manager = Self {
            config,
            converter,
            transfer_engine,
            sync_manager,
            event_tx,
            shutdown_tx: None,
            portal_clipboard: Arc::new(RwLock::new(None)), // Will be set after Portal initialization
            portal_session: Arc::new(RwLock::new(None)),   // Will be set with portal_clipboard
            pending_portal_requests: Arc::new(RwLock::new(std::collections::VecDeque::new())),
            server_event_sender: Arc::new(RwLock::new(None)), // Set by WrdCliprdrFactory
            dbus_bridge: Arc::new(RwLock::new(None)), // Will be set by start_dbus_clipboard_listener
            recently_written_hashes: Arc::new(RwLock::new(std::collections::HashMap::new())),
            file_transfer_state,
            fuse_manager: Arc::clone(&fuse_manager),
            fuse_request_tx: Some(fuse_request_tx),
            pending_fuse_responses: Arc::clone(&pending_fuse_responses),
            current_rdp_formats: Arc::new(RwLock::new(Vec::new())),
            local_advertised_formats: Arc::new(RwLock::new(Vec::new())),
        };

        // Start FUSE request handler (bridges FUSE reads to RDP requests)
        manager.start_fuse_request_handler(fuse_request_rx, Arc::clone(&pending_fuse_responses));

        // Start event processor
        manager.start_event_processor(event_rx);

        debug!("Clipboard manager initialized");

        Ok(manager)
    }

    /// Get event sender for external components
    pub fn event_sender(&self) -> mpsc::Sender<ClipboardEvent> {
        self.event_tx.clone()
    }

    /// Set server event sender (called by LamcoCliprdrFactory after initialization)
    pub async fn set_server_event_sender(
        &self,
        sender: mpsc::UnboundedSender<ironrdp_server::ServerEvent>,
    ) {
        *self.server_event_sender.write().await = Some(sender);
        debug!(" ServerEvent sender registered with clipboard manager");
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

    /// Create virtual files in FUSE from file descriptors
    ///
    /// Returns paths to the virtual files for URI generation.
    /// Used when Windows copies files and we need to present them to Linux.
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
        fuse.as_ref().map(|m| m.is_mounted()).unwrap_or(false)
    }

    /// Set Portal clipboard manager and session (async to acquire write lock)
    pub async fn set_portal_clipboard(
        &mut self,
        portal: Arc<crate::portal::PortalClipboardManager>,
        session: Arc<
            RwLock<
                ashpd::desktop::Session<
                    'static,
                    ashpd::desktop::remote_desktop::RemoteDesktop<'static>,
                >,
            >,
        >,
    ) {
        *self.portal_clipboard.write().await = Some(Arc::clone(&portal));
        *self.portal_session.write().await = Some(Arc::clone(&session));
        debug!(" Portal clipboard and session dynamically set in clipboard manager");

        // Start SelectionTransfer listener for delayed rendering (Windows → Linux paste)
        self.start_selection_transfer_listener(Arc::clone(&portal), Arc::clone(&session))
            .await;

        // Start SelectionOwnerChanged listener for local clipboard monitoring (Linux → Windows copy)
        self.start_owner_changed_listener(Arc::clone(&portal), Arc::clone(&session))
            .await;

        // D-Bus bridge fallback - SelectionOwnerChanged unreliable on GNOME
        self.start_dbus_clipboard_listener().await;
    }

    /// Start SelectionTransfer listener for delayed rendering (Windows → Linux paste)
    async fn start_selection_transfer_listener(
        &self,
        portal: Arc<crate::portal::PortalClipboardManager>,
        _session: Arc<
            RwLock<
                ashpd::desktop::Session<
                    'static,
                    ashpd::desktop::remote_desktop::RemoteDesktop<'static>,
                >,
            >,
        >,
    ) {
        // Create channel for SelectionTransfer events
        let (transfer_tx, mut transfer_rx) = mpsc::unbounded_channel();

        // Start the Portal listener (spawns background task)
        match portal.start_selection_transfer_listener(transfer_tx).await {
            Ok(()) => {
                debug!("Starting SelectionTransfer handler task");

                // Clone refs for the spawned task
                let pending_requests = Arc::clone(&self.pending_portal_requests);
                let server_event_sender = Arc::clone(&self.server_event_sender);
                let converter = Arc::clone(&self.converter);
                let sync_manager = Arc::clone(&self.sync_manager);
                let portal_clipboard = Arc::clone(&self.portal_clipboard);
                let portal_session = Arc::clone(&self.portal_session);
                let current_rdp_formats = Arc::clone(&self.current_rdp_formats);

                // Spawn task to handle SelectionTransfer events
                tokio::spawn(async move {
                    while let Some(transfer_event) = transfer_rx.recv().await {
                        info!(
                            "SelectionTransfer signal: {} (serial {})",
                            transfer_event.mime_type, transfer_event.serial
                        );

                        // CRITICAL FIX: Portal sends 45+ SelectionTransfer signals for ONE paste operation
                        // (LibreOffice/apps request clipboard in many MIME types: text/plain, UTF8_STRING, etc.)
                        // We must process ONLY the first request and CANCEL all others.
                        //
                        // ADDITIONAL: Time-based deduplication to prevent multiple pastes within 3 seconds
                        // (handles case where user/app triggers paste twice rapidly)
                        //
                        // Per XDG Portal spec: Each SelectionTransfer must be answered with either:
                        // - SelectionWrite() + data + SelectionWriteDone(true)  [fulfill]
                        // - SelectionWriteDone(false)                           [cancel]

                        // Time-based deduplication (100ms window for compositor rapid-fire bugs ONLY)
                        // CRITICAL: Paste is user-driven (Ctrl+V), not polling
                        // Each SelectionTransfer = user pressed Ctrl+V = distinct user intent
                        // We must honor EVERY user action, even if pasting same content repeatedly
                        // Only block technical glitches: compositor sending duplicate signals < 100ms apart
                        use std::sync::atomic::{AtomicU64, Ordering};
                        static LAST_PASTE_TIME_MS: AtomicU64 = AtomicU64::new(0);

                        let now_ms = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap()
                            .as_millis() as u64;
                        let last_paste = LAST_PASTE_TIME_MS.load(Ordering::Relaxed);

                        if last_paste > 0 && now_ms - last_paste < 100 {
                            debug!("Rapid duplicate signal ({}ms apart) - likely compositor bug - canceling serial {}",
                                  now_ms - last_paste, transfer_event.serial);

                            if let (Some(portal), Some(session)) = (
                                portal_clipboard.read().await.clone(),
                                portal_session.read().await.clone(),
                            ) {
                                let session_guard = session.read().await;
                                if let Err(e) = portal
                                    .portal_clipboard()
                                    .selection_write_done(
                                        &session_guard,
                                        transfer_event.serial,
                                        false,
                                    )
                                    .await
                                {
                                    error!(
                                        "Failed to cancel duplicate signal serial {}: {}",
                                        transfer_event.serial, e
                                    );
                                }
                            }
                            continue;
                        }

                        // Check if we're already processing another paste request
                        // REMOVED: Don't block based on pending requests
                        // Each Ctrl+V is distinct user intent, queue them in order

                        // Update last paste time
                        LAST_PASTE_TIME_MS.store(now_ms, Ordering::Relaxed);

                        // This is the FIRST request for this paste operation - handle it
                        debug!(
                            " First SelectionTransfer for paste operation - will fulfill serial {}",
                            transfer_event.serial
                        );

                        // Log timing to track delay between signal and write
                        let _transfer_time = std::time::Instant::now();

                        // CRITICAL: Check clipboard state before asking RDP for data
                        // Only ask RDP if RDP owns the clipboard (has the data we need)
                        // If Portal owns (Linux copied something), RDP doesn't have the data
                        {
                            let sync = sync_manager.read().await;
                            let state = sync.state();
                            match state {
                                ClipboardState::RdpOwned(_, _) => {
                                    debug!(
                                        "Clipboard state is RdpOwned - will request data from RDP"
                                    );
                                }
                                ClipboardState::PortalOwned(_) => {
                                    warn!("Ignoring SelectionTransfer - Portal owns clipboard, RDP doesn't have data");
                                    continue;
                                }
                                ClipboardState::Idle => {
                                    // Idle might be OK if RDP sent formats but we haven't tracked state yet
                                    debug!("Clipboard state is Idle - will attempt RDP request");
                                }
                                ClipboardState::Syncing(_) => {
                                    debug!("Clipboard state is Syncing - will attempt RDP request");
                                }
                            }
                        }

                        // Already added to pending queue above (before sending request)
                        // This ensures FIFO ordering: first request gets first response

                        // Convert MIME type → RDP format ID
                        // First try stored format list (for registered formats with dynamic IDs)
                        // Then fall back to hardcoded mapping
                        let stored_formats = current_rdp_formats.read().await;
                        let format_id = if let Some(id) =
                            lookup_format_id_for_mime(&stored_formats, &transfer_event.mime_type)
                        {
                            debug!(
                                "Using stored format ID {} for MIME {} (registered format)",
                                id, transfer_event.mime_type
                            );
                            id
                        } else {
                            match converter.mime_to_format_id(&transfer_event.mime_type) {
                                Ok(id) => id,
                                Err(e) => {
                                    error!(
                                        "Failed to convert MIME {} to format ID: {}",
                                        transfer_event.mime_type, e
                                    );
                                    // Don't add to queue since we can't fulfill this
                                    drop(stored_formats); // Release lock before await
                                    if let (Some(portal), Some(session)) = (
                                        portal_clipboard.read().await.clone(),
                                        portal_session.read().await.clone(),
                                    ) {
                                        let session_guard = session.read().await;
                                        let _ = portal
                                            .portal_clipboard()
                                            .selection_write_done(
                                                &session_guard,
                                                transfer_event.serial,
                                                false,
                                            )
                                            .await;
                                    }
                                    continue;
                                }
                            }
                        };
                        drop(stored_formats); // Release lock before await

                        // Send ServerEvent to request data from RDP client (TRUE delayed rendering!)
                        let sender_opt = server_event_sender.read().await.clone();
                        if let Some(sender) = sender_opt {
                            use ironrdp_cliprdr::backend::ClipboardMessage;
                            use ironrdp_cliprdr::pdu::ClipboardFormatId;

                            // Add to pending queue BEFORE sending request
                            pending_requests.write().await.push_back((
                                transfer_event.serial,
                                transfer_event.mime_type.clone(),
                                std::time::Instant::now(),
                            ));

                            if let Err(e) = sender.send(ironrdp_server::ServerEvent::Clipboard(
                                ClipboardMessage::SendInitiatePaste(ClipboardFormatId(format_id)),
                            )) {
                                error!("Failed to send FormatDataRequest via ServerEvent: {:?}", e);
                                pending_requests
                                    .write()
                                    .await
                                    .retain(|(s, _, _)| *s != transfer_event.serial);
                            } else {
                                info!(
                                    "Sent FormatDataRequest for format {} (Portal serial {})",
                                    format_id, transfer_event.serial
                                );

                                // Start timeout task - cancel transfer if RDP doesn't respond in 5 seconds
                                let serial = transfer_event.serial;
                                let pending_clone = Arc::clone(&pending_requests);
                                let portal_clone = Arc::clone(&portal_clipboard);
                                let session_clone = Arc::clone(&portal_session);

                                tokio::spawn(async move {
                                    tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;

                                    // Check if request still pending in FIFO queue
                                    if pending_clone
                                        .read()
                                        .await
                                        .iter()
                                        .any(|(s, _, _)| *s == serial)
                                    {
                                        warn!("Clipboard request timeout for serial {} - RDP client didn't respond in 5 seconds", serial);

                                        // Notify Portal of transfer failure
                                        if let (Some(portal), Some(session)) = (
                                            portal_clone.read().await.clone(),
                                            session_clone.read().await.clone(),
                                        ) {
                                            let session_guard = session.read().await;
                                            if let Err(e) = portal
                                                .portal_clipboard()
                                                .selection_write_done(&session_guard, serial, false)
                                                .await
                                            {
                                                error!("Failed to notify Portal of timeout: {}", e);
                                            } else {
                                                debug!(" Notified Portal of transfer timeout (serial {})", serial);
                                            }
                                        }

                                        // Remove from pending requests
                                        pending_clone
                                            .write()
                                            .await
                                            .retain(|(s, _, _)| *s != serial);
                                    }
                                });
                            }
                        } else {
                            warn!("ServerEvent sender not available yet - cannot request from RDP");
                            pending_requests
                                .write()
                                .await
                                .retain(|(s, _, _)| *s != transfer_event.serial);
                        }
                    }

                    warn!("SelectionTransfer handler task ended");
                });

                info!("SelectionTransfer listener and handler started - delayed rendering enabled");
            }
            Err(e) => {
                error!("Failed to start SelectionTransfer listener: {:#}", e);
                warn!("Delayed rendering (Windows → Linux paste) will not work");
            }
        }
    }

    /// Monitor local clipboard changes (Linux → Windows copy flow)
    async fn start_owner_changed_listener(
        &self,
        portal: Arc<crate::portal::PortalClipboardManager>,
        _session: Arc<
            RwLock<
                ashpd::desktop::Session<
                    'static,
                    ashpd::desktop::remote_desktop::RemoteDesktop<'static>,
                >,
            >,
        >,
    ) {
        // Create channel for SelectionOwnerChanged events
        let (owner_tx, mut owner_rx) = mpsc::unbounded_channel();

        // Start the Portal listener
        match portal.start_owner_changed_listener(owner_tx).await {
            Ok(()) => {
                debug!("Starting SelectionOwnerChanged handler task");

                let event_tx = self.event_tx.clone();

                // Spawn task to handle clipboard ownership changes
                tokio::spawn(async move {
                    info!(
                        "SelectionOwnerChanged handler task ready - waiting for clipboard changes"
                    );
                    let mut change_count = 0;

                    while let Some(mime_types) = owner_rx.recv().await {
                        change_count += 1;
                        info!(
                            "Local clipboard change #{}: {} formats: {:?}",
                            change_count,
                            mime_types.len(),
                            mime_types
                        );

                        // Send event to announce these formats to RDP clients
                        // Portal already filtered echoes (session_is_owner=true), so force=true
                        if let Err(e) = event_tx
                            .send(ClipboardEvent::PortalFormatsAvailable(
                                mime_types.clone(),
                                true,
                            ))
                            .await
                        {
                            error!("Failed to send PortalFormatsAvailable event: {}", e);
                            break;
                        } else {
                            debug!(" Sent PortalFormatsAvailable event to clipboard manager");
                        }
                    }

                    warn!(
                        "SelectionOwnerChanged handler task ended after {} changes",
                        change_count
                    );
                });

                debug!(" SelectionOwnerChanged listener started - monitoring Linux clipboard");
                debug!("Using Portal path (KDE/Sway/wlroots mode) - NOT D-Bus extension");
            }
            Err(e) => {
                error!("Failed to start SelectionOwnerChanged listener: {:#}", e);
                warn!("Linux → Windows clipboard flow will not work via Portal signals");
                warn!("Will attempt D-Bus bridge for GNOME extension fallback");
            }
        }
    }

    /// D-Bus clipboard listener for GNOME (Portal signals unreliable on Mutter)
    pub async fn start_dbus_clipboard_listener(&self) {
        debug!("Checking for GNOME clipboard extension on D-Bus...");

        // Check if extension is available (static method)
        if !DbusClipboardBridge::is_available().await {
            debug!("GNOME clipboard extension not detected - D-Bus bridge inactive");
            debug!(
                "Install wayland-rdp-clipboard extension for Linux → Windows clipboard on GNOME"
            );
            return;
        }

        // Connect to D-Bus bridge (this spawns the signal listener internally)
        let bridge = match DbusClipboardBridge::connect().await {
            Ok(b) => b,
            Err(e) => {
                error!("Failed to connect to D-Bus clipboard bridge: {}", e);
                return;
            }
        };

        // Subscribe to clipboard events (broadcast::Receiver)
        let mut dbus_rx = bridge.subscribe();

        // Store bridge reference
        *self.dbus_bridge.write().await = Some(bridge);

        // Clone event sender and hash tracker for the spawned task
        let event_tx = self.event_tx.clone();
        let recently_written_hashes = Arc::clone(&self.recently_written_hashes);
        let rate_limit_ms = self.config.rate_limit_ms;

        // Start background hash cleanup task
        // This removes the expensive cleanup from the clipboard event hot path
        let hashes_for_cleanup = Arc::clone(&self.recently_written_hashes);
        tokio::spawn(async move {
            const LOOP_SUPPRESSION_WINDOW_MS: u128 = 2000;
            const MAX_HASH_CACHE_SIZE: usize = 50;

            loop {
                tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;

                let mut hashes = hashes_for_cleanup.write().await;
                let before_size = hashes.len();

                // Clean up expired entries
                let now = std::time::Instant::now();
                hashes.retain(|_, written_at| {
                    now.duration_since(*written_at).as_millis() < LOOP_SUPPRESSION_WINDOW_MS
                });

                // Enforce size limit
                while hashes.len() > MAX_HASH_CACHE_SIZE {
                    if let Some(oldest_key) = hashes
                        .iter()
                        .min_by_key(|(_, time)| *time)
                        .map(|(k, _)| k.clone())
                    {
                        hashes.remove(&oldest_key);
                    } else {
                        break;
                    }
                }

                let after_size = hashes.len();
                if before_size != after_size {
                    debug!("Hash cleanup: {} → {} entries", before_size, after_size);
                }
            }
        });

        // Spawn task to forward D-Bus events to ClipboardManager
        tokio::spawn(async move {
            info!(
                "D-Bus clipboard event forwarder started (rate limit: {}ms)",
                rate_limit_ms
            );
            let mut event_count = 0;
            let mut suppressed_count = 0;
            let mut rate_limited_count = 0;

            // Loop suppression: ignore events within this window after we wrote data
            #[allow(dead_code)] // WIP: Loop suppression refinement
            const LOOP_SUPPRESSION_WINDOW_MS: u128 = 2000;
            // Maximum pending hash entries (prevent unbounded memory)
            #[allow(dead_code)] // WIP: Hash cache bounds
            const MAX_HASH_CACHE_SIZE: usize = 50;

            let mut last_forward_time: Option<std::time::Instant> = None;

            // Note: broadcast::Receiver uses Ok(event) pattern, not Some(event)
            // It also returns RecvError::Lagged if we fell behind - we ignore those
            while let Ok(dbus_event) = dbus_rx.recv().await {
                event_count += 1;

                // Library's DbusClipboardEvent only monitors CLIPBOARD selection
                // (PRIMARY selection not supported - matches RDP capability)

                let hash_short = &dbus_event.content_hash[..8.min(dbus_event.content_hash.len())];

                // RATE LIMITING: Enforce minimum interval between forwarded events
                // This prevents rapid-fire D-Bus signals from overwhelming the Portal
                if rate_limit_ms > 0 {
                    if let Some(last_time) = last_forward_time {
                        let elapsed = last_time.elapsed().as_millis() as u64;
                        if elapsed < rate_limit_ms {
                            rate_limited_count += 1;
                            debug!(
                                "Rate limited: {}ms since last event (min: {}ms) - skipping event #{}",
                                elapsed, rate_limit_ms, event_count
                            );
                            continue;
                        }
                    }
                }

                // LOOP SUPPRESSION: Check if this hash matches data we recently wrote to Portal
                // If so, this is feedback from our own write - don't forward back to RDP!
                // NOTE: Hash cleanup moved to background task for performance
                {
                    let hashes = recently_written_hashes.read().await;

                    // Check if this event's hash matches one we recently wrote
                    if hashes.contains_key(&dbus_event.content_hash) {
                        suppressed_count += 1;
                        info!(
                            "LOOP SUPPRESSED #{}: D-Bus event hash {} matches our recent write - skipping",
                            suppressed_count, hash_short
                        );
                        continue;
                    }
                }

                // Update rate limit timestamp
                last_forward_time = Some(std::time::Instant::now());

                info!(
                    "D-Bus clipboard change #{}: {} MIME types (hash: {})",
                    event_count,
                    dbus_event.mime_types.len(),
                    hash_short
                );
                debug!("   MIME types: {:?}", dbus_event.mime_types);

                // Forward to ClipboardManager as PortalFormatsAvailable event
                // This triggers the same flow as if Portal had sent SelectionOwnerChanged
                // D-Bus extension signals are authoritative (force=true) - always override RDP ownership
                if let Err(e) = event_tx
                    .send(ClipboardEvent::PortalFormatsAvailable(
                        dbus_event.mime_types,
                        true,
                    ))
                    .await
                {
                    error!("Failed to forward D-Bus event to ClipboardManager: {}", e);
                    break;
                }

                debug!(" Forwarded clipboard change to RDP client announcement flow");
            }

            warn!(
                "D-Bus clipboard event forwarder ended after {} events ({} loop-suppressed, {} rate-limited)",
                event_count, suppressed_count, rate_limited_count
            );
        });

        debug!(" D-Bus clipboard bridge started - GNOME extension integration active");
        debug!("Using D-Bus path (GNOME mode) - NOT Portal SelectionOwnerChanged");
        debug!("Linux -> Windows clipboard now enabled via extension");
    }

    /// Start FUSE request handler
    ///
    /// This bridges synchronous FUSE read() calls to async RDP FileContentsRequests.
    /// When the Linux file manager reads a virtual file, FUSE blocks on a channel
    /// while we fetch the data from Windows via RDP.
    fn start_fuse_request_handler(
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

        tokio::spawn(async move {
            debug!("FUSE request handler started");

            while let Some(request) = request_rx.recv().await {
                // Allocate a stream ID for this request
                let stream_id = {
                    let mut state = file_transfer_state.write().await;
                    state.allocate_stream_id()
                };

                debug!(
                    "FUSE request: file_index={} offset={} size={} -> stream_id={}",
                    request.file_index, request.offset, request.size, stream_id
                );

                // Store response channel for later
                {
                    let mut pending = pending_responses.write().await;
                    pending.insert(stream_id, request.response_tx);
                }

                // Send FileContentsRequest to RDP
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
                        // Send error response back to FUSE
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
                    // Send error response back to FUSE
                    if let Some(response_tx) = pending_responses.write().await.remove(&stream_id) {
                        let _ = response_tx
                            .send(FileContentsResponse::Error("RDP not connected".to_string()));
                    }
                }
            }

            debug!("FUSE request handler ended");
        });
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
        // Clone the Arc<RwLock<>> wrappers - they can be read dynamically
        let portal_clipboard = Arc::clone(&self.portal_clipboard);
        let portal_session = Arc::clone(&self.portal_session);
        let pending_portal_requests = Arc::clone(&self.pending_portal_requests);
        let server_event_sender = Arc::clone(&self.server_event_sender);
        let recently_written_hashes = Arc::clone(&self.recently_written_hashes);
        let file_transfer_state = Arc::clone(&self.file_transfer_state);
        let fuse_manager = Arc::clone(&self.fuse_manager);
        let current_rdp_formats = Arc::clone(&self.current_rdp_formats);
        let local_advertised_formats = Arc::clone(&self.local_advertised_formats);

        let (shutdown_tx, mut shutdown_rx) = mpsc::channel::<()>(1);
        self.shutdown_tx = Some(shutdown_tx);

        tokio::spawn(async move {
            loop {
                tokio::select! {
                    Some(event) = event_rx.recv() => {
                        if let Err(e) = Self::handle_event(
                            event,
                            &converter,
                            &sync_manager,
                            &transfer_engine,
                            &config,
                            &portal_clipboard,
                            &portal_session,
                            &pending_portal_requests,
                            &server_event_sender,
                            &recently_written_hashes,
                            &file_transfer_state,
                            &fuse_manager,
                            &current_rdp_formats,
                            &local_advertised_formats,
                        ).await {
                            error!("Error handling clipboard event: {:?}", e);
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
    async fn handle_event(
        event: ClipboardEvent,
        converter: &FormatConverter,
        sync_manager: &Arc<RwLock<SyncManager>>,
        transfer_engine: &TransferEngine,
        _config: &ClipboardConfig,
        portal_clipboard: &Arc<RwLock<Option<Arc<crate::portal::PortalClipboardManager>>>>,
        portal_session: &Arc<
            RwLock<
                Option<
                    Arc<
                        RwLock<
                            ashpd::desktop::Session<
                                'static,
                                ashpd::desktop::remote_desktop::RemoteDesktop<'static>,
                            >,
                        >,
                    >,
                >,
            >,
        >,
        pending_portal_requests: &Arc<
            RwLock<std::collections::VecDeque<(u32, String, std::time::Instant)>>,
        >,
        server_event_sender: &Arc<
            RwLock<Option<mpsc::UnboundedSender<ironrdp_server::ServerEvent>>>,
        >,
        recently_written_hashes: &Arc<
            RwLock<std::collections::HashMap<String, std::time::Instant>>,
        >,
        file_transfer_state: &Arc<RwLock<FileTransferState>>,
        fuse_manager: &Arc<RwLock<Option<crate::clipboard::fuse::FuseManager>>>,
        current_rdp_formats: &Arc<RwLock<Vec<ClipboardFormat>>>,
        local_advertised_formats: &Arc<RwLock<Vec<ClipboardFormat>>>,
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

                    // Send the cached formats to RDP
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
                    portal_clipboard,
                    portal_session,
                    current_rdp_formats,
                )
                .await
            }

            ClipboardEvent::RdpDataRequest(format_id, _response_callback) => {
                Self::handle_rdp_data_request(
                    format_id,
                    converter,
                    sync_manager,
                    portal_clipboard,
                    portal_session,
                    server_event_sender,
                    local_advertised_formats,
                    file_transfer_state,
                )
                .await
            }

            ClipboardEvent::RdpDataResponse(data) => {
                Self::handle_rdp_data_response(
                    data,
                    sync_manager,
                    transfer_engine,
                    portal_clipboard,
                    portal_session,
                    pending_portal_requests,
                    recently_written_hashes,
                    file_transfer_state,
                    fuse_manager,
                    server_event_sender,
                )
                .await
            }

            ClipboardEvent::RdpDataError => {
                Self::handle_rdp_data_error(
                    portal_clipboard,
                    portal_session,
                    pending_portal_requests,
                )
                .await
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
                    portal_clipboard,
                    portal_session,
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
                )
                .await
            }

            ClipboardEvent::PortalDataRequest(mime_type) => {
                Self::handle_portal_data_request(
                    mime_type,
                    converter,
                    sync_manager,
                    portal_clipboard,
                    portal_session,
                )
                .await
            }

            ClipboardEvent::PortalDataResponse(data) => {
                Self::handle_portal_data_response(data, sync_manager, transfer_engine).await
            }
        }
    }

    /// Handle RDP format list announcement
    async fn handle_rdp_format_list(
        formats: Vec<ClipboardFormat>,
        converter: &FormatConverter,
        sync_manager: &Arc<RwLock<SyncManager>>,
        portal_clipboard: &Arc<RwLock<Option<Arc<crate::portal::PortalClipboardManager>>>>,
        portal_session: &Arc<
            RwLock<
                Option<
                    Arc<
                        RwLock<
                            ashpd::desktop::Session<
                                'static,
                                ashpd::desktop::remote_desktop::RemoteDesktop<'static>,
                            >,
                        >,
                    >,
                >,
            >,
        >,
        current_rdp_formats: &Arc<RwLock<Vec<ClipboardFormat>>>,
    ) -> Result<()> {
        debug!("RDP format list received: {:?}", formats);

        // Store the format list for later lookup (registered format IDs vary per session)
        {
            let mut stored_formats = current_rdp_formats.write().await;
            *stored_formats = formats.clone();
            debug!(
                "Stored {} RDP formats for format ID lookup",
                stored_formats.len()
            );
        }

        // Check with sync manager (loop detection)
        let should_sync = {
            let mut mgr = sync_manager.write().await;
            mgr.handle_rdp_formats(formats.clone())?
        };

        if !should_sync {
            debug!("Skipping RDP format list due to loop detection");
            return Ok(());
        }

        // Convert RDP formats to MIME types
        let mime_types = converter.rdp_to_mime_types(&formats)?;

        debug!("Converted to MIME types: {:?}", mime_types);

        // Get Portal clipboard and session (dynamically read from Arc<RwLock<>>)
        let portal_opt = portal_clipboard.read().await.clone();
        let session_opt = portal_session.read().await.clone();

        debug!(
            "Checking Portal availability: clipboard={}, session={}",
            portal_opt.is_some(),
            session_opt.is_some()
        );

        let (portal, session) = match (portal_opt, session_opt) {
            (Some(p), Some(s)) => (p, s),
            (None, Some(_)) => {
                warn!("Portal clipboard not available (but session is)");
                return Ok(());
            }
            (Some(_), None) => {
                warn!("Portal session not available (but clipboard is) - THIS SHOULD NOT HAPPEN");
                return Ok(());
            }
            (None, None) => {
                debug!("Portal clipboard and session not yet initialized (normal during startup)");
                return Ok(());
            }
        };

        // Announce formats to Portal using delayed rendering (SetSelection)
        // This tells Wayland "these formats are available" WITHOUT transferring data
        let session_guard = session.read().await;
        portal
            .announce_rdp_formats(&session_guard, mime_types)
            .await
            .map_err(|e| {
                ClipboardError::PortalError(format!("Failed to announce formats: {}", e))
            })?;

        debug!(" RDP clipboard formats announced to Portal via SetSelection");

        Ok(())
    }

    /// Handle RDP data request (Linux → Windows paste)
    async fn handle_rdp_data_request(
        format_id: u32,
        converter: &FormatConverter,
        _sync_manager: &Arc<RwLock<SyncManager>>,
        portal_clipboard: &Arc<RwLock<Option<Arc<crate::portal::PortalClipboardManager>>>>,
        portal_session: &Arc<
            RwLock<
                Option<
                    Arc<
                        RwLock<
                            ashpd::desktop::Session<
                                'static,
                                ashpd::desktop::remote_desktop::RemoteDesktop<'static>,
                            >,
                        >,
                    >,
                >,
            >,
        >,
        server_event_sender: &Arc<
            RwLock<Option<mpsc::UnboundedSender<ironrdp_server::ServerEvent>>>,
        >,
        local_advertised_formats: &Arc<RwLock<Vec<ClipboardFormat>>>,
        file_transfer_state: &Arc<RwLock<FileTransferState>>,
    ) -> Result<()> {
        info!(
            "RDP data request for format ID: {} (Linux → Windows paste)",
            format_id
        );

        // Check if this is a registered format from our advertised list
        let advertised = local_advertised_formats.read().await;
        let format_name = advertised
            .iter()
            .find(|f| f.id == format_id || (format_id == 0 && f.name.is_some()))
            .and_then(|f| f.name.clone());
        drop(advertised);

        // Check if this is FileGroupDescriptorW (file transfer)
        if let Some(ref name) = format_name {
            if name == "FileGroupDescriptorW" {
                debug!("Windows requests FileGroupDescriptorW - sending file list from Linux clipboard");
                return Self::handle_file_descriptor_request(
                    portal_clipboard,
                    portal_session,
                    server_event_sender,
                    file_transfer_state,
                )
                .await;
            }
        }

        // Get Portal clipboard and session
        let portal_opt = portal_clipboard.read().await.clone();
        let session_opt = portal_session.read().await.clone();

        let (portal, session) = match (portal_opt, session_opt) {
            (Some(p), Some(s)) => (p, s),
            _ => {
                warn!("Portal not available for RDP data request");
                // Send error response to RDP client
                Self::send_format_data_error(server_event_sender).await;
                return Ok(());
            }
        };

        // Convert format ID to MIME type
        let mime_type = match converter.format_id_to_mime(format_id) {
            Ok(m) => m,
            Err(e) => {
                warn!("Unknown format ID {}: {:?}", format_id, e);
                Self::send_format_data_error(server_event_sender).await;
                return Ok(());
            }
        };
        debug!("Format {} maps to MIME: {}", format_id, mime_type);

        // Read from Portal clipboard via SelectionRead
        let session_guard = session.read().await;
        let portal_data = match portal
            .read_local_clipboard(&session_guard, &mime_type)
            .await
        {
            Ok(data) => {
                info!(
                    "Read {} bytes from Portal clipboard ({})",
                    data.len(),
                    mime_type
                );
                data
            }
            Err(e) => {
                error!("Failed to read from Portal clipboard: {:#}", e);
                // Send error response to RDP client
                drop(session_guard);
                Self::send_format_data_error(server_event_sender).await;
                return Ok(());
            }
        };
        drop(session_guard);

        // Convert Portal data to RDP format based on format ID and MIME type
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
                lamco_clipboard_core::image::png_to_dib(&portal_data).map_err(|e| {
                    error!("PNG to DIB conversion failed: {}", e);
                    ClipboardError::Core(e)
                })?
            } else if mime_type.starts_with("image/jpeg") {
                trace!(" Converting JPEG to DIB for Windows");
                lamco_clipboard_core::image::jpeg_to_dib(&portal_data).map_err(|e| {
                    error!("JPEG to DIB conversion failed: {}", e);
                    ClipboardError::Core(e)
                })?
            } else if mime_type.starts_with("image/bmp") || mime_type.starts_with("image/x-bmp") {
                trace!(" Converting BMP to DIB for Windows");
                lamco_clipboard_core::image::bmp_to_dib(&portal_data).map_err(|e| {
                    error!("BMP to DIB conversion failed: {}", e);
                    ClipboardError::Core(e)
                })?
            } else {
                debug!("Unknown image MIME for DIB: {}, passing through", mime_type);
                portal_data
            }
        } else if format_id == 17 {
            // CF_DIBV5 - Windows wants DIBV5 with alpha channel support
            if mime_type.starts_with("image/png") {
                trace!(" Converting PNG to DIBV5 for Windows (with alpha)");
                lamco_clipboard_core::image::png_to_dibv5(&portal_data).map_err(|e| {
                    error!("PNG to DIBV5 conversion failed: {}", e);
                    ClipboardError::Core(e)
                })?
            } else if mime_type.starts_with("image/jpeg") {
                trace!(" Converting JPEG to DIBV5 for Windows");
                lamco_clipboard_core::image::jpeg_to_dibv5(&portal_data).map_err(|e| {
                    error!("JPEG to DIBV5 conversion failed: {}", e);
                    ClipboardError::Core(e)
                })?
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

        // Send response back to RDP client via ServerEvent
        let sender_opt = server_event_sender.read().await.clone();
        if let Some(sender) = sender_opt {
            use ironrdp_cliprdr::backend::ClipboardMessage;
            use ironrdp_cliprdr::pdu::FormatDataResponse;
            use ironrdp_pdu::IntoOwned;

            // Create FormatDataResponse and convert to owned
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
        portal_clipboard: &Arc<RwLock<Option<Arc<crate::portal::PortalClipboardManager>>>>,
        portal_session: &Arc<
            RwLock<
                Option<
                    Arc<
                        RwLock<
                            ashpd::desktop::Session<
                                'static,
                                ashpd::desktop::remote_desktop::RemoteDesktop<'static>,
                            >,
                        >,
                    >,
                >,
            >,
        >,
        server_event_sender: &Arc<
            RwLock<Option<mpsc::UnboundedSender<ironrdp_server::ServerEvent>>>,
        >,
        file_transfer_state: &Arc<RwLock<FileTransferState>>,
    ) -> Result<()> {
        // Get Portal clipboard and session
        let portal_opt = portal_clipboard.read().await.clone();
        let session_opt = portal_session.read().await.clone();

        let (portal, session) = match (portal_opt, session_opt) {
            (Some(p), Some(s)) => (p, s),
            _ => {
                warn!("Portal not available for file descriptor request");
                Self::send_format_data_error(server_event_sender).await;
                return Ok(());
            }
        };

        // Try to read file URIs from Portal - prefer x-special/gnome-copied-files, fall back to text/uri-list
        let session_guard = session.read().await;
        let uri_data = match portal
            .read_local_clipboard(&session_guard, "x-special/gnome-copied-files")
            .await
        {
            Ok(data) if !data.is_empty() => {
                info!(
                    "Read {} bytes from Portal clipboard (x-special/gnome-copied-files)",
                    data.len()
                );
                data
            }
            _ => {
                // Fall back to text/uri-list
                match portal
                    .read_local_clipboard(&session_guard, "text/uri-list")
                    .await
                {
                    Ok(data) => {
                        info!(
                            "Read {} bytes from Portal clipboard (text/uri-list)",
                            data.len()
                        );
                        data
                    }
                    Err(e) => {
                        error!("Failed to read file URIs from Portal: {:#}", e);
                        drop(session_guard);
                        Self::send_format_data_error(server_event_sender).await;
                        return Ok(());
                    }
                }
            }
        };
        drop(session_guard);

        // Parse URIs from the clipboard data using the library function
        // This handles both text/uri-list and x-special/gnome-copied-files formats
        let file_paths = parse_file_uris(&uri_data);

        for path in &file_paths {
            trace!("Found file: {:?}", path);
        }

        if file_paths.is_empty() {
            warn!("No valid file paths found in clipboard");
            Self::send_format_data_error(server_event_sender).await;
            return Ok(());
        }

        // Store outgoing files for FileContents requests
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

        // Build FILEDESCRIPTORW data
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

        // Send response to Windows
        let sender_opt = server_event_sender.read().await.clone();
        if let Some(sender) = sender_opt {
            use ironrdp_cliprdr::backend::ClipboardMessage;
            use ironrdp_cliprdr::pdu::FormatDataResponse;
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
    async fn send_format_data_error(
        server_event_sender: &Arc<
            RwLock<Option<mpsc::UnboundedSender<ironrdp_server::ServerEvent>>>,
        >,
    ) {
        let sender_opt = server_event_sender.read().await.clone();
        if let Some(sender) = sender_opt {
            use ironrdp_cliprdr::backend::ClipboardMessage;
            use ironrdp_cliprdr::pdu::FormatDataResponse;
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
    async fn handle_rdp_data_response(
        data: Vec<u8>,
        sync_manager: &Arc<RwLock<SyncManager>>,
        _transfer_engine: &TransferEngine,
        portal_clipboard: &Arc<RwLock<Option<Arc<crate::portal::PortalClipboardManager>>>>,
        portal_session: &Arc<
            RwLock<
                Option<
                    Arc<
                        RwLock<
                            ashpd::desktop::Session<
                                'static,
                                ashpd::desktop::remote_desktop::RemoteDesktop<'static>,
                            >,
                        >,
                    >,
                >,
            >,
        >,
        pending_portal_requests: &Arc<
            RwLock<std::collections::VecDeque<(u32, String, std::time::Instant)>>,
        >,
        _recently_written_hashes: &Arc<
            RwLock<std::collections::HashMap<String, std::time::Instant>>,
        >,
        file_transfer_state: &Arc<RwLock<FileTransferState>>,
        fuse_manager: &Arc<RwLock<Option<crate::clipboard::fuse::FuseManager>>>,
        server_event_sender: &Arc<
            RwLock<Option<mpsc::UnboundedSender<ironrdp_server::ServerEvent>>>,
        >,
    ) -> Result<()> {
        debug!("RDP data response received: {} bytes", data.len());

        // Check for content loop
        let should_transfer = sync_manager.write().await.check_content(&data, true)?;
        if !should_transfer {
            debug!("Skipping RDP data due to content loop detection");
            return Ok(());
        }

        // Get Portal clipboard and session
        let portal_opt = portal_clipboard.read().await.clone();
        let session_opt = portal_session.read().await.clone();

        let (portal, session) = match (portal_opt, session_opt) {
            (Some(p), Some(s)) => (p, s),
            _ => {
                warn!("Portal not available - cannot deliver clipboard data");
                return Ok(());
            }
        };

        // CRITICAL: Get FIRST pending request (FIFO order)
        // IronRDP doesn't correlate requests/responses, so we use FIFO queue
        // First FormatDataRequest gets first FormatDataResponse (proper server implementation)
        let mut pending = pending_portal_requests.write().await;
        let request_opt = pending.pop_front(); // Take oldest request
        drop(pending);

        let (serial, requested_mime, _request_time) = match request_opt {
            Some(req) => req,
            None => {
                warn!("No pending Portal request - FormatDataResponse arrived with no matching request");
                warn!("This can happen if requests timed out - data will be discarded");
                return Ok(());
            }
        };

        info!(
            "Matched FormatDataResponse to Portal serial {} (FIFO queue)",
            serial
        );
        debug!(
            "Portal requested MIME: {}, received {} bytes from Windows",
            requested_mime,
            data.len()
        );

        // Special handling for file transfer formats
        // Both text/uri-list and x-special/gnome-copied-files represent file URIs
        if requested_mime == "text/uri-list" || requested_mime == "x-special/gnome-copied-files" {
            // This is likely FileGroupDescriptorW data - parse file descriptors
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

                    // CRITICAL: Check if we already have an active file transfer in progress
                    // Portal sends BOTH text/uri-list AND x-special/gnome-copied-files requests
                    // for the same paste operation - we must only process the FIRST one
                    {
                        let state = file_transfer_state.read().await;
                        if !state.incoming_files.is_empty() {
                            info!(
                                "Skipping duplicate file transfer request ({}) - transfer already in progress with {} file(s)",
                                requested_mime, state.incoming_files.len()
                            );
                            drop(state);

                            // Cancel this Portal request - we're already handling the transfer
                            let session_guard = session.read().await;
                            let _ = portal
                                .portal_clipboard()
                                .selection_write_done(&session_guard, serial, false)
                                .await;
                            return Ok(());
                        }
                    }

                    // Check if FUSE is available and mounted - use on-demand transfer
                    let fuse_available = {
                        let fuse = fuse_manager.read().await;
                        fuse.as_ref().map(|m| m.is_mounted()).unwrap_or(false)
                    };

                    if fuse_available {
                        // FUSE path: create virtual files and deliver URIs immediately
                        // Data will be fetched on-demand when file manager reads
                        info!("Using FUSE on-demand file transfer (no upfront download)");

                        // Lock clipboard data for the duration of the transfer
                        let clip_data_id = 1u32;
                        if let Some(sender) = server_event_sender.read().await.as_ref() {
                            use ironrdp_cliprdr::backend::ClipboardMessage;
                            if let Err(e) = sender.send(ironrdp_server::ServerEvent::Clipboard(
                                ClipboardMessage::SendLockClipboard { clip_data_id },
                            )) {
                                warn!("Failed to send Lock PDU for FUSE transfer: {:?}", e);
                            }
                        }

                        // Convert library FileDescriptor to FUSE FileDescriptor
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

                        // Create virtual files in FUSE
                        let paths = {
                            let fuse = fuse_manager.read().await;
                            if let Some(ref manager) = *fuse {
                                manager.set_files(fuse_descriptors, Some(clip_data_id))
                            } else {
                                Vec::new()
                            }
                        };

                        if paths.is_empty() {
                            error!("FUSE failed to create virtual files - falling back to staging");
                            // Fall through to staging approach
                        } else {
                            // Generate URI list for Portal
                            let uri_content =
                                crate::clipboard::fuse::generate_gnome_copied_files_content(&paths);
                            let uri_bytes = uri_content.into_bytes();

                            info!(
                                "Delivering {} FUSE virtual file URI(s) to Portal (serial={})",
                                paths.len(),
                                serial
                            );

                            // Write URI list to Portal
                            let session_guard = session.read().await;
                            match portal
                                .write_selection_data(&session_guard, serial, uri_bytes)
                                .await
                            {
                                Ok(_) => {
                                    info!(
                                        "FUSE file URIs delivered - files available for on-demand read"
                                    );
                                }
                                Err(e) => {
                                    error!("Failed to deliver FUSE URIs to Portal: {:?}", e);
                                    let _ = portal
                                        .portal_clipboard()
                                        .selection_write_done(&session_guard, serial, false)
                                        .await;
                                }
                            }

                            return Ok(());
                        }
                    }

                    // Staging fallback path: download files upfront (when FUSE not available)
                    info!("Using staging file transfer (FUSE not available)");

                    // Initialize file transfer state and request file contents
                    let sender_opt = server_event_sender.read().await.clone();
                    let sender = match sender_opt {
                        Some(s) => s,
                        None => {
                            error!(
                                "ServerEvent sender not available - cannot request file contents"
                            );
                            // Cancel Portal request since we can't proceed
                            let session_guard = session.read().await;
                            let _ = portal
                                .portal_clipboard()
                                .selection_write_done(&session_guard, serial, false)
                                .await;
                            return Ok(());
                        }
                    };

                    {
                        let mut state = file_transfer_state.write().await;

                        // Clear any previous transfer state
                        state.clear_incoming();
                        state.set_pending_descriptors(descriptors.clone());
                        state.portal_serial = Some(serial);

                        use ironrdp_cliprdr::backend::ClipboardMessage;
                        use ironrdp_cliprdr::pdu::{FileContentsFlags, FileContentsRequest};

                        // Lock clipboard data before requesting file contents
                        // Required when CAN_LOCK_CLIPDATA is negotiated
                        let clip_data_id = 1u32; // Use a consistent ID for this transfer
                        info!("Sending Lock PDU (clip_data_id={})", clip_data_id);
                        if let Err(e) = sender.send(ironrdp_server::ServerEvent::Clipboard(
                            ClipboardMessage::SendLockClipboard { clip_data_id },
                        )) {
                            error!("Failed to send Lock PDU: {:?}", e);
                        }

                        // Create IncomingFile entry for each file and request its contents
                        for (idx, desc) in descriptors.iter().enumerate() {
                            let stream_id = state.allocate_stream_id();
                            // Sanitize Windows filename for Linux filesystem compatibility
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

                            // Create temp file for receiving data
                            let temp_path = state
                                .download_dir
                                .join(format!(".{}.{}.tmp", filename, stream_id));

                            // Ensure download directory exists
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

                            // Register this incoming file
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

                            // Send FileContentsRequest for this file
                            // Request all data at once (position 0, size = total_size or reasonable max)
                            let request_size = if total_size > 0 {
                                total_size.min(64 * 1024 * 1024) as u32 // Max 64MB per request
                            } else {
                                64 * 1024 * 1024 // Request 64MB if size unknown
                            };

                            if let Err(e) = sender.send(ironrdp_server::ServerEvent::Clipboard(
                                ClipboardMessage::SendFileContentsRequest(FileContentsRequest {
                                    stream_id,
                                    index: idx as u32,
                                    flags: FileContentsFlags::DATA, // Request actual file data
                                    position: 0,
                                    requested_size: request_size,
                                    data_id: Some(clip_data_id), // Must match the Lock PDU's clip_data_id
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

                    // Don't cancel Portal request - we'll deliver files when transfer completes
                    // The FileContentsResponse handler will finalize and deliver URIs
                    return Ok(());
                }
                Err(e) => {
                    error!("Failed to parse FileGroupDescriptorW: {:?}", e);
                    // Fall through to generic handling
                }
            }
        }

        // Convert RDP data to Portal format based on requested MIME type
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
        } else if (requested_mime == "text/plain" || requested_mime == "text/html")
            && data.len() >= 2
        {
            // text/plain and text/html from Windows are UTF-16LE (CF_UNICODETEXT)
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

        // REMOVED: Hash-based deduplication
        // Paste is user-driven (Ctrl+V) - each action is distinct user intent
        // User may legitimately want to paste same content multiple times
        // Hash dedup was blocking valid user actions and breaking clipboard UX

        // Write data to Portal via SelectionWrite workflow
        // IMPORTANT: Use timeout to prevent event loop from getting stuck on lock contention
        debug!(
            "Acquiring session read lock for Portal write (serial {})",
            serial
        );
        let lock_start = std::time::Instant::now();
        let session_guard = match tokio::time::timeout(
            tokio::time::Duration::from_secs(10),
            session.read(),
        )
        .await
        {
            Ok(guard) => {
                let lock_time = lock_start.elapsed();
                if lock_time.as_millis() > 100 {
                    warn!(
                        "Session lock took {}ms to acquire (serial {})",
                        lock_time.as_millis(),
                        serial
                    );
                } else {
                    debug!(
                        "Session lock acquired in {}ms (serial {})",
                        lock_time.as_millis(),
                        serial
                    );
                }
                guard
            }
            Err(_) => {
                error!(
                    "TIMEOUT: Failed to acquire session read lock after 10s (serial {}) - possible deadlock!",
                    serial
                );
                error!("This prevents event loop from getting stuck. Canceling this clipboard transfer.");
                // Cancel the Portal transfer to prevent Portal from waiting forever
                if let (Some(p), Some(s)) = (
                    portal_clipboard.read().await.clone(),
                    portal_session.read().await.clone(),
                ) {
                    // Use a short timeout for the cancel operation too
                    if let Ok(sg) =
                        tokio::time::timeout(tokio::time::Duration::from_secs(2), s.read()).await
                    {
                        let _ = p
                            .portal_clipboard()
                            .selection_write_done(&sg, serial, false)
                            .await;
                    }
                }
                return Err(ClipboardError::Unknown(
                    "Session lock timeout - possible deadlock".to_string(),
                ));
            }
        };

        let _write_attempt_time = std::time::Instant::now();
        info!(
            "📝 About to call Portal selection_write: serial={}, data_len={} bytes",
            serial,
            portal_data.len()
        );

        // Use timeout on the Portal write operation as well
        let write_result = tokio::time::timeout(
            tokio::time::Duration::from_secs(30),
            portal.write_selection_data(&session_guard, serial, portal_data.clone()),
        )
        .await;

        match write_result {
            Err(_) => {
                error!(
                    "TIMEOUT: Portal selection_write took >30s (serial {}) - canceling",
                    serial
                );
                // Notify Portal of failure
                let _ = portal
                    .portal_clipboard()
                    .selection_write_done(&session_guard, serial, false)
                    .await;
                return Err(ClipboardError::Unknown("Portal write timeout".to_string()));
            }
            Ok(Err(e)) => {
                error!("Failed to write clipboard data to Portal: {:#}", e);

                // Remove THIS failed request from pending queue
                let mut pending = pending_portal_requests.write().await;
                pending.retain(|(s, _, _)| *s != serial);
                drop(pending);

                return Err(ClipboardError::PortalError(format!(
                    "SelectionWrite failed: {}",
                    e
                )));
            }
            Ok(Ok(())) => {
                info!(
                    "Clipboard data delivered to Portal via SelectionWrite (serial {})",
                    serial
                );

                // CRITICAL: Cancel ALL other pending requests
                // LibreOffice/apps send 16-45 SelectionTransfer signals for ONE Ctrl+V (multiple MIME types)
                // We fulfilled the first one, must cancel all others or get 16+ pastes
                let mut pending = pending_portal_requests.write().await;
                let unfulfilled: Vec<u32> = pending
                    .iter()
                    .filter(|(s, _, _)| *s != serial)
                    .map(|(s, _, _)| *s)
                    .collect();
                pending.clear(); // Clear ALL (including the one we just fulfilled)
                drop(pending);

                // Cancel unfulfilled Portal requests
                if !unfulfilled.is_empty() {
                    debug!(" Canceling {} unfulfilled SelectionTransfer requests (LibreOffice multi-MIME)", unfulfilled.len());

                    if let (Some(portal), Some(session)) = (
                        portal_clipboard.read().await.clone(),
                        portal_session.read().await.clone(),
                    ) {
                        let session_guard = session.read().await;
                        for unfulfilled_serial in unfulfilled {
                            if let Err(e) = portal
                                .portal_clipboard()
                                .selection_write_done(&session_guard, unfulfilled_serial, false)
                                .await
                            {
                                error!("Failed to cancel serial {}: {}", unfulfilled_serial, e);
                            } else {
                                debug!("Canceled unfulfilled serial {}", unfulfilled_serial);
                            }
                        }
                    }
                }
            }
        }

        Ok(())
    }

    /// Handle RDP data error (must notify Portal to prevent retry crash)
    ///
    /// This is called when the RDP client responds with FormatDataResponse(error=true),
    /// which is normal protocol behavior when the client doesn't have the requested format.
    /// Per MS-RDPECLIP, this is expected and not an error condition.
    async fn handle_rdp_data_error(
        portal_clipboard: &Arc<RwLock<Option<Arc<crate::portal::PortalClipboardManager>>>>,
        portal_session: &Arc<
            RwLock<
                Option<
                    Arc<
                        RwLock<
                            ashpd::desktop::Session<
                                'static,
                                ashpd::desktop::remote_desktop::RemoteDesktop<'static>,
                            >,
                        >,
                    >,
                >,
            >,
        >,
        pending_portal_requests: &Arc<
            RwLock<std::collections::VecDeque<(u32, String, std::time::Instant)>>,
        >,
    ) -> Result<()> {
        // RDP client returned error - format not available (expected protocol behavior)
        debug!("RDP FormatDataResponse: format not available, notifying Portal");

        // Get Portal clipboard and session
        let portal_opt = portal_clipboard.read().await.clone();
        let session_opt = portal_session.read().await.clone();

        let (portal, session) = match (portal_opt, session_opt) {
            (Some(p), Some(s)) => (p, s),
            _ => {
                warn!("Portal not available - clearing pending requests only");
                pending_portal_requests.write().await.clear();
                return Ok(());
            }
        };

        // Get all pending requests and notify Portal of failure for each
        let pending = pending_portal_requests.read().await;
        let serials: Vec<u32> = pending.iter().map(|(s, _, _)| *s).collect();
        drop(pending);

        for serial in serials {
            debug!("Notifying Portal of transfer failure (serial {})", serial);

            // Notify Portal that the transfer failed
            let session_guard = session.read().await;
            match portal
                .portal_clipboard()
                .selection_write_done(&session_guard, serial, false)
                .await
            {
                Ok(()) => {
                    debug!(" Portal notified of transfer failure (serial {})", serial);
                }
                Err(e) => {
                    // Log but don't fail - the transfer is already failed
                    warn!("Failed to notify Portal of transfer failure: {:#}", e);
                }
            }

            // Remove from pending FIFO queue
            pending_portal_requests
                .write()
                .await
                .retain(|(s, _, _)| *s != serial);
        }

        Ok(())
    }

    /// Handle Portal format announcement (Linux → Windows)
    ///
    /// `force=true` from D-Bus extension overrides RDP ownership; `force=false` may be blocked.
    async fn handle_portal_formats(
        mime_types: Vec<String>,
        force: bool,
        converter: &FormatConverter,
        sync_manager: &Arc<RwLock<SyncManager>>,
        server_event_sender: &Arc<
            RwLock<Option<mpsc::UnboundedSender<ironrdp_server::ServerEvent>>>,
        >,
        local_advertised_formats: &Arc<RwLock<Vec<ClipboardFormat>>>,
    ) -> Result<()> {
        info!(
            "handle_portal_formats called with {} MIME types (force={}): {:?}",
            mime_types.len(),
            force,
            mime_types
        );

        // Check with sync manager (loop detection)
        let should_sync = {
            let mut mgr = sync_manager.write().await;
            mgr.handle_portal_formats(mime_types.clone(), force)?
        };

        if !should_sync {
            debug!("Skipping Portal formats due to loop detection");
            return Ok(());
        }

        // Convert MIME types to RDP formats
        let rdp_formats = converter.mime_to_rdp_formats(&mime_types)?;
        debug!(
            "Converted {} MIME types to {} RDP formats",
            mime_types.len(),
            rdp_formats.len()
        );

        // Convert to IronRDP ClipboardFormat type
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

        // Store the formats we're advertising (for data request lookup)
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

        // Log format details for debugging
        debug!(" Sending FormatList to RDP client:");
        for (idx, fmt) in ironrdp_formats.iter().enumerate() {
            let name_str = fmt.name.as_ref().map(|n| n.value()).unwrap_or("");
            info!("   Format {}: ID={}, Name={:?}", idx, fmt.id.0, name_str);
        }

        // Send ServerEvent to announce formats to RDP clients
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
    async fn handle_portal_data_request(
        mime_type: String,
        converter: &FormatConverter,
        _sync_manager: &Arc<RwLock<SyncManager>>,
        _portal_clipboard: &Arc<RwLock<Option<Arc<crate::portal::PortalClipboardManager>>>>,
        _portal_session: &Arc<
            RwLock<
                Option<
                    Arc<
                        RwLock<
                            ashpd::desktop::Session<
                                'static,
                                ashpd::desktop::remote_desktop::RemoteDesktop<'static>,
                            >,
                        >,
                    >,
                >,
            >,
        >,
    ) -> Result<()> {
        debug!("Portal data request for MIME type: {}", mime_type);

        // Convert MIME type to RDP format ID
        let format_id = converter.mime_to_format_id(&mime_type)?;

        info!(
            "Portal needs data - will request format {} from RDP client",
            format_id
        );

        // NOTE: We can't send ServerEvent from here because we don't have the sender in event handlers.
        // The sender is available in the SelectionTransfer listener task.
        // We need to refactor to send the ServerEvent from there instead.
        //
        // For now, this handler just logs. The actual request will be sent from the
        // SelectionTransfer handler task which has access to both pending_requests and event_tx.

        warn!("PortalDataRequest event received but ServerEvent sending happens in SelectionTransfer handler");

        Ok(())
    }

    /// Handle Portal data response
    async fn handle_portal_data_response(
        data: Vec<u8>,
        sync_manager: &Arc<RwLock<SyncManager>>,
        _transfer_engine: &TransferEngine,
    ) -> Result<()> {
        debug!("Portal data response received: {} bytes", data.len());

        // Check for content loop
        let should_transfer = sync_manager.write().await.check_content(&data, false)?;

        if !should_transfer {
            debug!("Skipping Portal data due to content loop detection");
            return Ok(());
        }

        // Note: Data forwarding to RDP happens via ironrdp_backend.rs
        // message proxy. This helper provides conversion logic when needed.

        Ok(())
    }

    /// Handle RDP file contents request (Linux → Windows file transfer)
    async fn handle_rdp_file_contents_request(
        stream_id: u32,
        list_index: u32,
        position: u64,
        requested_size: u32,
        is_size_request: bool,
        server_event_sender: &Arc<
            RwLock<Option<mpsc::UnboundedSender<ironrdp_server::ServerEvent>>>,
        >,
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

        // Get file from outgoing files list
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
                ClipboardError::InvalidState(format!("File index {} not found", list_index))
            })?;

        // Import types for sending FileContentsResponse
        use ironrdp_cliprdr::backend::ClipboardMessage;
        use ironrdp_cliprdr::pdu::FileContentsResponse;

        if is_size_request {
            // Return file size as 8-byte little-endian
            info!(
                "Returning file size: {} bytes for '{}'",
                file_info.size, file_info.filename
            );

            // Create and send FileContentsResponse with size
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
            // Read data from file
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

                    // Create and send FileContentsResponse with data
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

                    // Send error response
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
            .map_err(|e| ClipboardError::FileIoError(format!("Failed to open file: {}", e)))?;

        file.seek(SeekFrom::Start(offset)).map_err(|e| {
            ClipboardError::FileIoError(format!("Failed to seek to offset {}: {}", offset, e))
        })?;

        let mut buffer = vec![0u8; size as usize];
        let bytes_read = file
            .read(&mut buffer)
            .map_err(|e| ClipboardError::FileIoError(format!("Failed to read file: {}", e)))?;

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
        portal_clipboard: &Arc<RwLock<Option<Arc<crate::portal::PortalClipboardManager>>>>,
        portal_session: &Arc<
            RwLock<
                Option<
                    Arc<
                        RwLock<
                            ashpd::desktop::Session<
                                'static,
                                ashpd::desktop::remote_desktop::RemoteDesktop<'static>,
                            >,
                        >,
                    >,
                >,
            >,
        >,
        server_event_sender: &Arc<
            RwLock<Option<mpsc::UnboundedSender<ironrdp_server::ServerEvent>>>,
        >,
    ) -> Result<()> {
        if is_error {
            warn!("FileContentsResponse ERROR: stream={}", stream_id);

            // Clean up failed transfer
            let mut state = file_transfer_state.write().await;
            if let Some(file) = state.incoming_files.remove(&stream_id) {
                info!("Cleaning up failed transfer: {}", file.filename);
                let _ = std::fs::remove_file(&file.temp_path);
            }

            // Cancel Portal request if this was part of a transfer
            if let Some(serial) = state.portal_serial.take() {
                drop(state);
                if let (Some(portal), Some(session)) = (
                    portal_clipboard.read().await.as_ref().cloned(),
                    portal_session.read().await.as_ref().cloned(),
                ) {
                    let session_guard = session.read().await;
                    let _ = portal
                        .portal_clipboard()
                        .selection_write_done(&session_guard, serial, false)
                        .await;
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

        // Get incoming file entry (should exist from transfer initiation)
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

        // Write data chunk to file
        file.file_handle.write_all(&data).map_err(|e| {
            error!(
                "Failed to write {} bytes to '{}': {}",
                data.len(),
                file.temp_path.display(),
                e
            );
            ClipboardError::FileIoError(format!("File write failed: {}", e))
        })?;

        file.received_size += data.len() as u64;

        // Log progress (less frequently for large files)
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

        // Check if this file transfer is complete
        let file_complete = file.total_size > 0 && file.received_size >= file.total_size;

        if file_complete {
            debug!(" File transfer complete: '{}'", file.filename);

            // Flush and close temp file
            file.file_handle
                .flush()
                .map_err(|e| ClipboardError::FileIoError(format!("Failed to flush file: {}", e)))?;

            let temp_path = file.temp_path.clone();
            let filename = file.filename.clone();

            // Move temp file to final location
            let final_path = download_dir.join(&filename);

            // Store the completed file path before any more operations
            state.completed_files.push(final_path.clone());

            // Remove from incoming files
            state.incoming_files.remove(&stream_id);

            // Check if ALL files are now complete
            let all_complete = state.incoming_files.is_empty();
            let portal_serial = state.portal_serial;
            let completed_files = state.completed_files.clone();
            drop(state); // Release lock before file operation

            // Perform the file rename (outside of lock)
            std::fs::rename(&temp_path, &final_path).map_err(|e| {
                error!(
                    "Failed to move '{}' to '{}': {}",
                    temp_path.display(),
                    final_path.display(),
                    e
                );
                ClipboardError::FileIoError(format!("Failed to finalize file: {}", e))
            })?;

            info!("Saved file to: {}", final_path.display());

            // If all files complete, deliver URIs to Portal
            if all_complete {
                debug!(
                    "All {} file(s) transferred successfully",
                    completed_files.len()
                );

                // Build file:// URI list with proper URL encoding
                // Only encode characters that are problematic in URIs, NOT dots/dashes/underscores
                use percent_encoding::{utf8_percent_encode, AsciiSet, CONTROLS};
                // Encode: control chars, space, and URI-special characters
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
                        // URL-encode each path component except the slashes
                        let path_str = path.to_string_lossy();
                        let encoded: String = path_str
                            .split('/')
                            .map(|component| {
                                utf8_percent_encode(component, FILE_URI_ENCODE).to_string()
                            })
                            .collect::<Vec<_>>()
                            .join("/");
                        format!("file://{}", encoded)
                    })
                    .collect();

                // Format as x-special/gnome-copied-files
                // Format: "copy\nfile:///path1\nfile:///path2\0" (null-terminated)
                let uri_list = format!("copy\n{}\0", uris.join("\n"));

                debug!(
                    "Generated URI list (gnome-copied-files format): {:?}",
                    uri_list
                );

                // Deliver to Portal
                if let Some(serial) = portal_serial {
                    if let (Some(portal), Some(session)) = (
                        portal_clipboard.read().await.as_ref().cloned(),
                        portal_session.read().await.as_ref().cloned(),
                    ) {
                        let session_guard = session.read().await;

                        // Write URI list data
                        let uri_bytes = uri_list.into_bytes();
                        match portal
                            .write_selection_data(&session_guard, serial, uri_bytes.clone())
                            .await
                        {
                            Ok(_) => {
                                info!(
                                    "Delivered {} file URI(s) to Portal (serial={})",
                                    completed_files.len(),
                                    serial
                                );
                            }
                            Err(e) => {
                                error!("Failed to deliver URIs to Portal: {:?}", e);
                                // Try to cancel gracefully
                                let _ = portal
                                    .portal_clipboard()
                                    .selection_write_done(&session_guard, serial, false)
                                    .await;
                            }
                        }
                    } else {
                        warn!("Portal not available to deliver file URIs");
                    }
                }

                // Clear completed files list
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

            // Request next chunk
            if let Some(sender) = server_event_sender.read().await.as_ref() {
                use ironrdp_cliprdr::backend::ClipboardMessage;
                use ironrdp_cliprdr::pdu::{FileContentsFlags, FileContentsRequest};

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
    pub async fn shutdown(&mut self) -> Result<()> {
        if let Some(ref tx) = self.shutdown_tx {
            tx.send(()).await.map_err(|e| {
                ClipboardError::InvalidState(format!("Failed to send shutdown signal: {}", e))
            })?;
        }
        // Clear the sender after shutdown
        self.shutdown_tx = None;
        Ok(())
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_clipboard_manager_creation() {
        let config = ClipboardConfig::default();
        let mut manager = ClipboardManager::new(config).await.unwrap();

        assert!(manager.event_tx.capacity() > 0);
    }

    #[tokio::test]
    async fn test_rdp_format_list_handling() {
        let config = ClipboardConfig::default();
        let mut manager = ClipboardManager::new(config).await.unwrap();

        let formats = vec![ClipboardFormat::with_name(13, "CF_UNICODETEXT")];
        let event = ClipboardEvent::RdpFormatList(formats);
        manager.event_tx.send(event).await.unwrap();
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
    }

    #[tokio::test]
    async fn test_shutdown() {
        let config = ClipboardConfig::default();
        let mut manager = ClipboardManager::new(config).await.unwrap();
        manager.shutdown().await.unwrap();
    }
}
