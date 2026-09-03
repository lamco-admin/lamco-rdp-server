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
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use lamco_clipboard_core::{
    ClipboardFormat, FormatConverter, LoopDetectionConfig, TransferConfig, TransferEngine,
};
use tokio::sync::{RwLock, mpsc};
use tracing::{debug, error, info, warn};

use crate::clipboard::{
    error::{ClipboardError, Result},
    sync::{PortalSyncDecision, SyncManager},
};

/// Shared clipboard provider reference (used by multiple handlers)
type SharedClipboardProvider =
    Arc<RwLock<Option<Arc<dyn crate::clipboard::provider::ClipboardProvider>>>>;

/// Pending portal request queue (format_id, mime_type, timestamp)
type PendingPortalRequests =
    Arc<RwLock<std::collections::VecDeque<(u32, String, std::time::Instant)>>>;

/// Which data-control eager-fetch this response corresponds to.
///
/// Data-control's `send()` is synchronous at the Wayland protocol level, so formats it
/// serves must be fetched from the RDP client and cached before the compositor ever asks.
/// IronRDP only supports one outstanding data request at a time, so these are fetched one
/// at a time via `pending_eager_fetches`, chained from `handle_rdp_data_response`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum EagerFetchKind {
    Text,
    Html { format_id: u32 },
    Image { format_id: u32 },
}

/// Queue of eager fetches still needed for the current data-control announcement.
type PendingEagerFetches = Arc<RwLock<std::collections::VecDeque<EagerFetchKind>>>;

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

/// Sentinel serial for eager-fetch requests (data-control upfront provision).
/// Real compositor serials are sequential small numbers, so u32::MAX won't collide.
const EAGER_FETCH_SERIAL: u32 = u32::MAX;

/// Response callback for sending data back to RDP
pub type RdpResponseCallback = Arc<dyn Fn(Vec<u8>) + Send + Sync>;

/// Clipboard events from RDP or Portal
#[derive(Clone)]
pub enum ClipboardEvent {
    /// RDP clipboard channel is ready - should re-announce Linux clipboard
    RdpReady,

    /// RDP client disconnected — release local ownership and drop session state
    RdpDisconnect,

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
        /// File index (lindex). Per [MS-RDPECLIP] 2.2.5.3, lindex is a signed
        /// 32-bit integer; negative values are rejected at decode time, so
        /// downstream code can treat this as a valid non-negative index.
        list_index: i32,
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

    /// RDP client sent the remote file list (FileGroupDescriptorW metadata) in
    /// response to a paste request. Used to pre-populate eager clipboard sources
    /// (e.g. Wayland ext-data-control) with file URIs up front.
    RdpRemoteFileList {
        files: Vec<lamco_rdp_clipboard::RemoteFileMetadata>,
        clip_data_id: Option<u32>,
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
            Self::RdpDisconnect => write!(f, "RdpDisconnect"),
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
            Self::RdpRemoteFileList { files, .. } => {
                write!(f, "RdpRemoteFileList({} files)", files.len())
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

    /// Queue of data-control eager fetches (HTML/image) still needed for the current
    /// announcement, chained one at a time behind `pending_portal_requests`. See
    /// `EagerFetchKind`.
    pending_eager_fetches: PendingEagerFetches,

    /// Server event sender for sending clipboard requests to IronRDP
    /// Set by LamcoCliprdrFactory after ServerEvent sender is available
    server_event_sender: Arc<RwLock<Option<mpsc::UnboundedSender<ironrdp_server::ServerEvent>>>>,

    /// Clipboard provider (trait-abstracted backend).
    clipboard_provider: Arc<RwLock<Option<Arc<dyn crate::clipboard::provider::ClipboardProvider>>>>,

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

    /// Cache of last successful clipboard data per MIME type (RDP -> Linux direction).
    /// When Mutter sends multiple SelectionTransfer requests for the same paste,
    /// subsequent requests can be served from cache without re-fetching from the RDP client.
    transfer_data_cache: Arc<RwLock<HashMap<String, Vec<u8>>>>,

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

    /// File transfer backend (FUSE, Staging, or Portal).
    ///
    /// Abstracts file materialization for clipboard file transfer.
    /// Selected at initialization based on deployment mode and config.
    file_transfer_backend:
        Arc<tokio::sync::RwLock<Box<dyn crate::clipboard::file_transfer::FileTransferBackend>>>,

    /// Latch: the RDP CLIPRDR channel has reached the Ready state for the live
    /// connection (the client sent its first Format List). Server-initiated
    /// pulls (`SendInitiatePaste`, file-contents requests) are illegal before
    /// Ready — IronRDP rejects them with "clipboard channel is not in Ready
    /// state", and that error propagates out of the client run-loop and drops
    /// the connection. Set on `RdpReady` / first Format List, cleared on
    /// connect-start and `RdpDisconnect`. Announces (`SendInitiateCopy`) are
    /// always legal and are never gated on this.
    rdp_ready: Arc<AtomicBool>,

    /// True while the Linux selection belongs to us because the remote copied.
    ///
    /// Announcing the remote's formats makes this process the selection owner,
    /// and a compositor will not let an owner read its own selection back:
    /// Mutter answers `SelectionRead` with "Tried to read own selection". So a
    /// Linux→Windows data request while this is set must be served from what
    /// the remote gave us, never by asking the compositor. Ownership persists
    /// until a local application copies something, which arrives as
    /// `PortalFormatsAvailable` and clears this.
    remote_owns_selection: Arc<AtomicBool>,
}

// File transfer state (FileTransferState, IncomingFile, OutgoingFile) has been
// extracted to src/clipboard/file_transfer/staging_backend.rs as part of the
// file transfer backend refactoring. The staging backend owns all download
// tracking state; the FUSE backend manages its own virtual file state.

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
        if let Some(mapped_mime) = super::lib_rdp_format_to_mime(format.id)
            && mapped_mime == mime_type
        {
            return Some(format.id);
        }

        // For registered formats, check by name
        if let Some(ref name) = format.name
            && let Some(mapped_mime) = format_name_to_mime(name)
        {
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
                    .is_ok_and(|g| g.is_some()),
            )
            .finish_non_exhaustive()
    }
}

mod portal_ingress;
mod rdp_ingress;

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

        // File transfer backend: select based on deployment mode and config
        let download_dir = std::env::var("HOME").ok().map_or_else(
            || PathBuf::from("/tmp"),
            |h| PathBuf::from(h).join("Downloads"),
        );

        let file_transfer_backend = {
            use crate::clipboard::file_transfer::{
                FileTransferBackend, fuse_backend::FuseFileTransfer,
                staging_backend::StagingFileTransfer, strategy::FileTransferMode,
            };

            let in_flatpak = crate::config::is_flatpak();
            let mode = FileTransferMode::select(in_flatpak, None);

            let backend: Box<dyn FileTransferBackend> = match mode {
                FileTransferMode::Fuse => {
                    let mut fuse = FuseFileTransfer::new();
                    match fuse.initialize().await {
                        Ok(()) => {
                            info!("File transfer backend: FUSE on-demand");
                            Box::new(fuse)
                        }
                        Err(e) => {
                            warn!("FUSE init failed ({}), falling back to staging", e);
                            let mut staging = StagingFileTransfer::new(download_dir.clone());
                            let _ = staging.initialize().await;
                            Box::new(staging)
                        }
                    }
                }
                FileTransferMode::Staging | FileTransferMode::Portal => {
                    let mut staging = StagingFileTransfer::new(download_dir.clone());
                    if let Err(e) = staging.initialize().await {
                        warn!("Staging init failed: {:?}", e);
                    }
                    info!("File transfer backend: Staging download");
                    Box::new(staging)
                }
            };

            Arc::new(tokio::sync::RwLock::new(backend))
        };

        let klipper_info = crate::clipboard::klipper::KlipperMonitor::detect().await;
        // Klipper's clear-after-takeover behavior is KDE-only; tell the sync state
        // machine whether Klipper actually runs so it doesn't treat a normal empty
        // selection (e.g. on COSMIC data-control) as a Klipper clear.
        sync_manager
            .write()
            .await
            .set_klipper_present(klipper_info.detected);
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
            pending_eager_fetches: Arc::new(RwLock::new(std::collections::VecDeque::new())),
            server_event_sender: Arc::new(RwLock::new(None)), // Set by WrdCliprdrFactory
            clipboard_provider: Arc::new(RwLock::new(None)),
            current_rdp_formats: Arc::new(RwLock::new(Vec::new())),
            local_advertised_formats: Arc::new(RwLock::new(Vec::new())),
            klipper_info,
            last_reannounce_time: Arc::new(RwLock::new(None)),
            reannounce_count: Arc::new(RwLock::new(HashMap::new())),
            transfer_data_cache: Arc::new(RwLock::new(HashMap::new())),
            strategy: crate::clipboard::ClipboardIntegrationMode::PortalDirect, // Default, will be set by initialize_strategy
            health_reporter: None,
            cooperation_coordinator: Arc::new(RwLock::new(None)),
            cooperation_content_cache: Arc::new(RwLock::new(None)),
            shutdown_broadcast: Arc::clone(&shutdown_broadcast),
            task_handles: Arc::clone(&task_handles),
            file_transfer_backend,
            rdp_ready: Arc::new(AtomicBool::new(false)),
            remote_owns_selection: Arc::new(AtomicBool::new(false)),
        };

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

                        let decision = {
                            let mut mgr = sync_manager.write().await;
                            mgr.handle_portal_formats(
                                vec!["text/plain".to_string()],
                                true, // force=true, this is authoritative from Klipper
                            )
                        };

                        // While RDP owns the clipboard — e.g. a Windows→Linux file
                        // transfer just staged its FileGroupDescriptorW — Klipper
                        // re-represents that content as text and fires this update.
                        // Re-announcing it to the client signals that Linux took
                        // ownership, so the client releases its FileContents and the
                        // pending paste fails with CB_RESPONSE_FAIL. Honor the
                        // echo-protection decision and skip the re-announce.
                        if decision == PortalSyncDecision::Block {
                            debug!(
                                "Cooperation: suppressing Klipper text re-announce (sync decision=Block; RDP owns clipboard)"
                            );
                            continue;
                        }

                        match *server_event_sender.read().await { Some(ref sender) => {
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
                        } _ => {
                            debug!("Cooperation: No server event sender (not ready yet)");
                        }}
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
        let health_reporter = self.health_reporter.clone();

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
                            crate::clipboard::provider::ClipboardProviderEvent::ListenerHealth {
                                healthy,
                                reason,
                            } => {
                                // Surface signal-listener liveness to the health
                                // monitor so a listener stranded on a dead session
                                // shows up instead of falsely reading healthy.
                                if let Some(ref reporter) = health_reporter {
                                    if healthy {
                                        reporter.report(crate::health::HealthEvent::ClipboardRecovered);
                                    } else {
                                        reporter.report(crate::health::HealthEvent::ClipboardFailed {
                                            reason,
                                        });
                                    }
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

    /// Start event processing loop
    fn start_event_processor(&mut self, mut event_rx: mpsc::Receiver<ClipboardEvent>) {
        let converter = self.converter.clone();
        let sync_manager = self.sync_manager.clone();
        let transfer_engine = self.transfer_engine.clone();
        let config = self.config.clone();
        let clipboard_provider = Arc::clone(&self.clipboard_provider);
        let pending_portal_requests = Arc::clone(&self.pending_portal_requests);
        let pending_eager_fetches = Arc::clone(&self.pending_eager_fetches);
        let server_event_sender = Arc::clone(&self.server_event_sender);
        let current_rdp_formats = Arc::clone(&self.current_rdp_formats);
        let local_advertised_formats = Arc::clone(&self.local_advertised_formats);
        let last_reannounce_time = Arc::clone(&self.last_reannounce_time);
        let reannounce_count = Arc::clone(&self.reannounce_count);
        let klipper_info = Arc::clone(&self.klipper_info);
        let cooperation_coordinator = Arc::clone(&self.cooperation_coordinator);
        let cooperation_content_cache = Arc::clone(&self.cooperation_content_cache);
        let file_transfer_backend = Arc::clone(&self.file_transfer_backend);
        let transfer_data_cache = Arc::clone(&self.transfer_data_cache);
        let rdp_ready = Arc::clone(&self.rdp_ready);
        let remote_owns_selection = Arc::clone(&self.remote_owns_selection);
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
                            &pending_eager_fetches,
                            &server_event_sender,
                            &current_rdp_formats,
                            &local_advertised_formats,
                            &last_reannounce_time,
                            &reannounce_count,
                            &klipper_info,
                            &cooperation_coordinator,
                            &cooperation_content_cache,
                            &file_transfer_backend,
                            &transfer_data_cache,
                            &rdp_ready,
                            &remote_owns_selection,
                        ).await {
                            let err_msg = format!("{e}");
                            error!("Error handling clipboard event: {err_msg}");
                            consecutive_errors += 1;

                            // Session-invalid errors are fatal — report immediately
                            let is_session_invalid = err_msg.contains("session invalid")
                                || err_msg.contains("Session invalid");
                            if (is_session_invalid || consecutive_errors >= 3)
                                && let Some(ref reporter) = health_reporter {
                                    reporter.report(crate::health::HealthEvent::ClipboardFailed {
                                        reason: format!("{consecutive_errors} consecutive errors: {e}"),
                                    });
                                }
                        } else if consecutive_errors > 0 {
                            // Recovered after errors
                            if consecutive_errors >= 3
                                && let Some(ref reporter) = health_reporter {
                                    reporter.report(crate::health::HealthEvent::ClipboardRecovered);
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
        pending_eager_fetches: &PendingEagerFetches,
        server_event_sender: &ServerEventSender,
        current_rdp_formats: &Arc<RwLock<Vec<ClipboardFormat>>>,
        local_advertised_formats: &Arc<RwLock<Vec<ClipboardFormat>>>,
        last_reannounce_time: &Arc<RwLock<Option<std::time::SystemTime>>>,
        reannounce_count: &Arc<RwLock<HashMap<Vec<u32>, u32>>>,
        klipper_info: &Arc<RwLock<crate::clipboard::klipper::KlipperInfo>>,
        cooperation_coordinator: &Arc<
            RwLock<Option<crate::clipboard::KlipperCooperationCoordinator>>,
        >,
        cooperation_content_cache: &Arc<RwLock<Option<Vec<u8>>>>,
        file_transfer_backend: &Arc<
            tokio::sync::RwLock<Box<dyn crate::clipboard::file_transfer::FileTransferBackend>>,
        >,
        transfer_data_cache: &Arc<RwLock<HashMap<String, Vec<u8>>>>,
        rdp_ready: &Arc<AtomicBool>,
        remote_owns_selection: &Arc<AtomicBool>,
    ) -> Result<()> {
        match event {
            ClipboardEvent::RdpReady => {
                // The CLIPRDR channel is now Ready for the live connection.
                // Server-initiated pulls become legal from here.
                rdp_ready.store(true, Ordering::SeqCst);

                // Let the active backend re-arm for the freshly-connected remote.
                if let Some(provider) = clipboard_provider.read().await.as_ref()
                    && let Err(e) = provider.on_remote_ready().await
                {
                    warn!("Provider on_remote_ready failed: {e}");
                }

                debug!(
                    "RDP clipboard channel ready - checking for pending Linux clipboard to announce"
                );
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

                        // File-aware re-announce: if the cached offer includes a file
                        // descriptor, re-register the files and send
                        // SendInitiateFileCopy (now legal — channel is Ready) so a file
                        // copy made before Ready still pastes as files instead of
                        // degrading to a plain copy ("no file list available"). Else a
                        // plain SendInitiateCopy.
                        let has_files = rdp_formats.iter().any(|f| {
                            f.name
                                .as_ref()
                                .is_some_and(|n| n.value() == "FileGroupDescriptorW")
                        });
                        let message = if has_files {
                            match Self::prepare_outgoing_file_copy(
                                clipboard_provider,
                                file_transfer_backend,
                            )
                            .await
                            {
                                Some(files) => ClipboardMessage::SendInitiateFileCopy(files),
                                None => ClipboardMessage::SendInitiateCopy(rdp_formats),
                            }
                        } else {
                            ClipboardMessage::SendInitiateCopy(rdp_formats)
                        };

                        info!("Re-announcing cached Linux clipboard formats to RDP client");
                        if let Err(e) = sender.send(ironrdp_server::ServerEvent::Clipboard(message))
                        {
                            error!("Failed to re-send FormatList: {:?}", e);
                        }
                    }
                } else {
                    debug!("No cached Linux clipboard formats to announce");
                }
                Ok(())
            }

            ClipboardEvent::RdpDisconnect => {
                // Idempotent teardown. A single disconnect drives
                // perform_disconnect_cleanup from two paths (on_disconnect + the
                // connection-handler closure), and the connect-start reset
                // re-emits defensively — swap(false) collapses all of them to a
                // single teardown, since only the true→false transition has work
                // to do. (A disconnect before the channel ever reached Ready has
                // nothing to release: no FormatList, no ownership, empty caches.)
                if !rdp_ready.swap(false, Ordering::SeqCst) {
                    return Ok(());
                }
                info!(
                    "RDP clipboard disconnect — releasing local ownership and clearing session state"
                );

                // Release ownership on the active backend so local apps stop
                // trying to paste from a remote that is gone. Local change
                // listeners stay alive for the next client.
                if let Some(provider) = clipboard_provider.read().await.as_ref()
                    && let Err(e) = provider.on_remote_gone().await
                {
                    warn!("Provider on_remote_gone failed: {e}");
                }

                // Drop per-connection state. `local_advertised_formats` is left
                // intact — it mirrors the live local clipboard, which the next
                // client's RdpReady re-announces.
                current_rdp_formats.write().await.clear();
                pending_portal_requests.write().await.clear();
                transfer_data_cache.write().await.clear();
                reannounce_count.write().await.clear();
                *cooperation_content_cache.write().await = None;
                Ok(())
            }

            ClipboardEvent::RdpFormatList(formats) => {
                // A Format List means the channel is Ready by definition; latch
                // it so this path never races ahead of the bridge's Ready event.
                rdp_ready.store(true, Ordering::SeqCst);
                // Clear transfer data cache: new copy operation from RDP client
                transfer_data_cache.write().await.clear();
                Self::handle_rdp_format_list(
                    formats,
                    converter,
                    sync_manager,
                    clipboard_provider,
                    current_rdp_formats,
                    _config,
                    klipper_info,
                    cooperation_coordinator,
                    server_event_sender,
                    pending_portal_requests,
                    pending_eager_fetches,
                    local_advertised_formats,
                    remote_owns_selection,
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
                    file_transfer_backend,
                    cooperation_content_cache,
                    transfer_data_cache,
                    remote_owns_selection,
                )
                .await
            }

            ClipboardEvent::RdpDataResponse(data) => {
                Self::handle_rdp_data_response(
                    data,
                    converter,
                    sync_manager,
                    transfer_engine,
                    clipboard_provider,
                    pending_portal_requests,
                    pending_eager_fetches,
                    _config,
                    file_transfer_backend,
                    server_event_sender,
                    transfer_data_cache,
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
                // Route through file transfer backend
                let sender = match server_event_sender.read().await.as_ref() {
                    Some(s) => s.clone(),
                    None => {
                        return Err(ClipboardError::NotInitialized);
                    }
                };
                let backend = file_transfer_backend.read().await;
                backend
                    .handle_outgoing_request(
                        stream_id,
                        // list_index is i32 (signed lindex per MS-RDPECLIP 2.2.5.3);
                        // negative values are rejected by IronRDP at decode time, so
                        // the as-cast to u32 is safe at this boundary. Backend signatures
                        // kept on u32 because the value is used as a Vec index.
                        list_index as u32,
                        position,
                        size,
                        is_size_request,
                        &sender,
                    )
                    .await
            }

            ClipboardEvent::RdpFileContentsResponse {
                stream_id,
                data,
                is_error,
            } => {
                let backend = file_transfer_backend.read().await;
                backend.deliver_file_data(stream_id, data, is_error).await
            }

            ClipboardEvent::RdpRemoteFileList {
                files,
                clip_data_id,
            } => {
                Self::handle_rdp_remote_file_list(
                    files,
                    clip_data_id,
                    clipboard_provider,
                    file_transfer_backend,
                    server_event_sender,
                    transfer_data_cache,
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
                    file_transfer_backend,
                    rdp_ready,
                    remote_owns_selection,
                )
                .await
            }

            ClipboardEvent::PortalDataRequest(mime_type) => {
                // Check cache first: if we already have data for this MIME type
                // from a recent successful transfer, serve it directly.
                // This handles Mutter's double-request pattern where it sends
                // multiple SelectionTransfer signals for one paste operation.
                let cached = transfer_data_cache.read().await.get(&mime_type).cloned();
                if let Some(cached_data) = cached {
                    debug!(
                        "Serving {} bytes from transfer cache for MIME: {}",
                        cached_data.len(),
                        mime_type
                    );
                    if let Some(ref provider) = *clipboard_provider.read().await {
                        let serial = {
                            let mut pending = pending_portal_requests.write().await;
                            pending.pop_front().map(|(s, _, _)| s)
                        };
                        if let Some(serial) = serial {
                            let _ = provider
                                .complete_transfer(serial, &mime_type, cached_data, true)
                                .await;
                        }
                    }
                    Ok(())
                } else if !rdp_ready.load(Ordering::SeqCst) {
                    // RDP clipboard isn't Ready: dispatching SendInitiatePaste now
                    // would crash the connection. The forwarder already pushed this
                    // request's serial onto the FIFO (the oldest unfulfilled entry),
                    // so pop it and cancel the compositor transfer with empty data —
                    // keeping the 1-push/1-pop correlation balanced and giving the
                    // local paste a definitive empty result instead of a hang that
                    // also desyncs the next paste.
                    let entry = pending_portal_requests.write().await.pop_front();
                    if let (Some((serial, m, _)), Some(provider)) =
                        (entry, clipboard_provider.read().await.as_ref())
                    {
                        let _ = provider
                            .complete_transfer(serial, &m, Vec::new(), false)
                            .await;
                    }
                    warn!(
                        "Portal data request for {mime_type} but RDP clipboard not Ready — \
                         suppressing pull (local paste gets no data)"
                    );
                    Ok(())
                } else {
                    Self::handle_portal_data_request(
                        mime_type,
                        converter,
                        sync_manager,
                        server_event_sender,
                        current_rdp_formats,
                    )
                    .await
                }
            }

            ClipboardEvent::PortalDataResponse(_) => {
                // PortalDataResponse is unused — data flows through
                // handle_rdp_data_request → Portal read_data → SendFormatData
                Ok(())
            }
        }
    }

    /// Send error response for FormatDataRequest
    async fn send_format_data_error(server_event_sender: &ServerEventSender) {
        let sender_opt = server_event_sender.read().await.clone();
        if let Some(sender) = sender_opt {
            use ironrdp_cliprdr::{backend::ClipboardMessage, pdu::FormatDataResponse};
            use ironrdp_pdu::IntoOwned;

            let response = FormatDataResponse::new_error();
            let owned_response = response.into_owned();

            match sender.send(ironrdp_server::ServerEvent::Clipboard(
                ClipboardMessage::SendFormatData(owned_response),
            )) {
                Err(e) => {
                    error!("Failed to send error FormatDataResponse: {:?}", e);
                }
                _ => {
                    debug!("Sent error FormatDataResponse to RDP client");
                }
            }
        }
    }

    // PortalDataResponse handler removed — nothing sends this event variant.
    // Data flows directly: handle_rdp_data_request → Portal read_data → SendFormatData.

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

        if let Some(ref tx) = self.shutdown_tx
            && let Err(e) = tx.send(()).await
        {
            warn!("Failed to send shutdown signal to event processor: {}", e);
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
    async fn test_clipboard_orchestrator_creation() {
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
