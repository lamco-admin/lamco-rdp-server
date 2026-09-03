//! File Transfer Backend Abstraction
//!
//! Defines a unified interface for clipboard file materialization backends.
//! The ClipboardOrchestrator calls backend methods without knowing whether
//! files are served via FUSE (on-demand) or staging (eager download).
//!
//! # Backends
//!
//! - [`FuseFileTransfer`] — Virtual filesystem, on-demand fetch via RDP (native only)
//! - [`StagingFileTransfer`] — Download all files upfront to temp dir (universal)
//! - Portal FileTransfer (future, not yet implemented)
//!
//! # Announcement
//!
//! All backends produce `Vec<PathBuf>` which is announced to the system clipboard
//! via `text/uri-list` and `x-special/gnome-copied-files` MIME types. This covers
//! Nautilus, Dolphin, Thunar, Nemo, PCManFM, and COSMIC Files.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use ironrdp_server::ServerEvent;
use tokio::sync::mpsc;

use crate::clipboard::error::Result;

pub mod fuse_backend;
pub mod staging_backend;
pub mod strategy;

/// True when `path` lives under a `lamco-clipboard-fuse-<pid>` mount owned by a
/// *different* server process — a stale mount left in the desktop clipboard by a
/// prior instance (issue #58). Such a URI resolves to a dead path, so callers
/// drop it rather than offer it to the RDP client.
pub fn is_stale_foreign_fuse_path(path: &Path) -> bool {
    let ours = std::process::id();
    path.components().any(|c| {
        c.as_os_str()
            .to_str()
            .and_then(|s| s.strip_prefix("lamco-clipboard-fuse-"))
            .and_then(|pid| pid.parse::<u32>().ok())
            .is_some_and(|pid| pid != ours)
    })
}

/// Descriptor of a file available for transfer from Windows.
///
/// Bridges between the CLIPRDR FileGroupDescriptorW (parsed from wire format)
/// and the file transfer backend's needs. The filename is already sanitized
/// for Linux filesystem conventions.
#[derive(Debug, Clone)]
pub struct TransferFileDescriptor {
    /// Filename sanitized for Linux (no backslashes, no reserved chars)
    pub filename: String,
    /// File size in bytes (0 if unknown)
    pub size: u64,
    /// Index in the FileGroupDescriptorW list (for FileContentsRequest)
    pub file_index: u32,
    /// Clipboard data ID for RDP locking
    pub clip_data_id: u32,
}

/// Result of preparing files for clipboard announcement.
#[derive(Debug)]
pub enum PrepareResult {
    /// Files ready immediately: paths to announce via clipboard provider.
    /// FUSE returns this — virtual files are available without downloading.
    Ready(Vec<PathBuf>),

    /// Files being downloaded asynchronously (staging).
    /// The backend will send `FileTransferEvent::FilesReady` when done.
    Pending,

    /// Backend failed; caller should try fallback or report error.
    Failed(String),
}

/// Events sent from the file transfer backend to the orchestrator.
#[derive(Debug)]
pub enum FileTransferEvent {
    /// All files have been materialized and are ready for clipboard announcement.
    FilesReady {
        /// File paths to announce via text/uri-list
        paths: Vec<PathBuf>,
        /// Portal serial for completing the clipboard transfer
        portal_serial: u32,
    },

    /// File transfer failed.
    TransferFailed {
        /// Reason for failure
        reason: String,
        /// Portal serial for error response
        portal_serial: u32,
    },
}

/// Info about a file available for outgoing transfer (Linux → Windows).
#[derive(Debug, Clone)]
pub struct OutgoingFileInfo {
    /// Index in the advertised file list
    pub list_index: u32,
    /// Path to the file on the Linux filesystem
    pub path: PathBuf,
    /// File size in bytes
    pub size: u64,
    /// Original filename
    pub filename: String,
}

/// File transfer backend interface.
///
/// Abstracts over FUSE (on-demand), staging (eager download), and
/// future Portal FileTransfer. The orchestrator calls these methods
/// without knowing which materialization strategy is active.
///
/// # Lifecycle
///
/// 1. Backend is constructed based on `FileTransferMode::select()`
/// 2. `initialize()` mounts FUSE / creates staging dir
/// 3. `prepare_files()` called per clipboard format announce from Windows
/// 4. `deliver_file_data()` routes RDP FileContentsResponse chunks
/// 5. `shutdown()` unmounts / cleans temp files
#[async_trait]
pub trait FileTransferBackend: Send + Sync {
    /// Backend name for logging and diagnostics.
    fn name(&self) -> &'static str;

    /// Whether this backend needs the full file data upfront.
    ///
    /// - FUSE: `false` (fetches data on read, no eager download)
    /// - Staging: `true` (downloads everything before announcing)
    fn requires_eager_download(&self) -> bool;

    /// Initialize the backend (mount FUSE, create staging dir, etc.)
    async fn initialize(&mut self) -> Result<()>;

    /// Prepare files for clipboard announcement.
    ///
    /// Called when Windows sends a FileGroupDescriptorW via CLIPRDR.
    ///
    /// - FUSE: creates virtual files, returns `PrepareResult::Ready(paths)` immediately
    /// - Staging: initiates downloads, returns `PrepareResult::Pending`
    async fn prepare_files(
        &self,
        descriptors: &[TransferFileDescriptor],
        portal_serial: u32,
        server_event_sender: &mpsc::UnboundedSender<ServerEvent>,
    ) -> Result<PrepareResult>;

    /// Deliver an RDP FileContentsResponse chunk to this backend.
    ///
    /// - FUSE: routes to the pending oneshot channel (unblocks FUSE read())
    /// - Staging: writes chunk to temp file, tracks progress, sends continuation
    async fn deliver_file_data(&self, stream_id: u32, data: Vec<u8>, is_error: bool) -> Result<()>;

    /// Handle outgoing file contents requests (Linux → Windows).
    ///
    /// When Windows pastes from our clipboard, it sends FileContentsRequest
    /// for each file. The backend reads the local file and sends the response.
    async fn handle_outgoing_request(
        &self,
        stream_id: u32,
        list_index: u32,
        position: u64,
        requested_size: u32,
        is_size_request: bool,
        server_event_sender: &mpsc::UnboundedSender<ServerEvent>,
    ) -> Result<()>;

    /// Set outgoing files (Linux → Windows file transfer).
    fn set_outgoing_files(&self, files: Vec<OutgoingFileInfo>);

    /// Subscribe to backend events (staging completion, errors).
    ///
    /// The orchestrator listens on this channel and delivers the resulting
    /// URIs via the clipboard provider when `FileTransferEvent::FilesReady` arrives.
    fn subscribe(&self) -> mpsc::UnboundedReceiver<FileTransferEvent>;

    /// Allocate a new stream ID for RDP FileContentsRequest/Response.
    fn allocate_stream_id(&self) -> u32;

    /// Check if the backend is healthy and operational.
    async fn health_check(&self) -> Result<()>;

    /// Shut down the backend (unmount FUSE, clean temp files, etc.)
    async fn shutdown(&mut self) -> Result<()>;
}

// --- Shared URI generation utilities ---

/// Percent-encode a filesystem path into a `file://` URI.
///
/// Each path component is encoded independently so `/` separators survive.
/// The encode set matches the OWASP recommendation for `file://` URIs: spaces,
/// `"`, `#`, `%`, `<`, `>`, `?`, backtick, `{`, `}`, plus all C0 controls.
/// Dots, dashes, underscores, and non-ASCII UTF-8 pass through unchanged.
fn encode_file_uri(path: &Path) -> String {
    use percent_encoding::{AsciiSet, CONTROLS, utf8_percent_encode};

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

    let encoded: String = path
        .to_string_lossy()
        .split('/')
        .map(|c| utf8_percent_encode(c, FILE_URI_ENCODE).to_string())
        .collect::<Vec<_>>()
        .join("/");
    format!("file://{encoded}")
}

/// Generate `x-special/gnome-copied-files` content from file paths.
///
/// Format: `copy\nfile:///path/to/file1\nfile:///path/to/file2\0`
///
/// Consumed by: Nautilus, Nemo, Thunar, PCManFM, COSMIC Files.
pub fn generate_gnome_copied_files_content(paths: &[PathBuf]) -> String {
    let uris: Vec<String> = paths.iter().map(|p| encode_file_uri(p)).collect();
    format!("copy\n{}\0", uris.join("\n"))
}

/// Generate `text/uri-list` content from file paths.
///
/// Format: `file:///path/to/file1\r\nfile:///path/to/file2\r\n`
///
/// Universal format consumed by all file managers.
pub fn generate_uri_list_content(paths: &[PathBuf]) -> String {
    let mut content = String::new();
    for path in paths {
        content.push_str(&encode_file_uri(path));
        content.push_str("\r\n");
    }
    content
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_transfer_descriptor() {
        let desc = TransferFileDescriptor {
            filename: "test.txt".to_string(),
            size: 1024,
            file_index: 0,
            clip_data_id: 1,
        };
        assert_eq!(desc.filename, "test.txt");
        assert_eq!(desc.size, 1024);
    }

    #[test]
    fn test_generate_gnome_copied_files() {
        let paths = vec![
            PathBuf::from("/tmp/test/file1.txt"),
            PathBuf::from("/tmp/test/file2.txt"),
        ];
        let content = generate_gnome_copied_files_content(&paths);
        assert!(content.starts_with("copy\n"));
        assert!(content.contains("file:///tmp/test/file1.txt"));
        assert!(content.contains("file:///tmp/test/file2.txt"));
        assert!(content.ends_with('\0'));
    }

    #[test]
    fn test_generate_gnome_copied_files_encodes_spaces() {
        let paths = vec![PathBuf::from("/home/user/My Documents/file name.txt")];
        let content = generate_gnome_copied_files_content(&paths);
        assert!(content.contains("/home/user/My%20Documents/file%20name.txt"));
        assert!(!content.contains("My Documents"));
    }

    #[test]
    fn test_generate_gnome_copied_files_preserves_separators() {
        let paths = vec![PathBuf::from("/tmp/a/b c/d e.txt")];
        let content = generate_gnome_copied_files_content(&paths);
        // Slashes between components must NOT be encoded as %2F.
        assert!(content.contains("file:///tmp/a/b%20c/d%20e.txt"));
    }

    #[test]
    fn test_generate_uri_list() {
        let paths = vec![PathBuf::from("/tmp/test/file1.txt")];
        let content = generate_uri_list_content(&paths);
        assert_eq!(content, "file:///tmp/test/file1.txt\r\n");
    }

    #[test]
    fn test_generate_uri_list_encodes_spaces() {
        let paths = vec![PathBuf::from("/path with spaces/file.txt")];
        let content = generate_uri_list_content(&paths);
        assert_eq!(content, "file:///path%20with%20spaces/file.txt\r\n");
    }

    #[test]
    fn test_encode_file_uri_special_chars() {
        // Each problematic char becomes its %XX escape. Dots, dashes, and
        // underscores pass through unchanged.
        let path = PathBuf::from("/x/a b#c?d<e>f\"g`h{i}j.ext-_test.txt");
        let uri = encode_file_uri(&path);
        assert!(uri.starts_with("file:///x/"));
        for (raw, escaped) in [
            (' ', "%20"),
            ('"', "%22"),
            ('#', "%23"),
            ('<', "%3C"),
            ('>', "%3E"),
            ('?', "%3F"),
            ('`', "%60"),
            ('{', "%7B"),
            ('}', "%7D"),
        ] {
            assert!(
                uri.contains(escaped),
                "missing {escaped} for {raw:?}: {uri}"
            );
        }
        assert!(uri.contains("ext-_test.txt"));
    }

    #[test]
    fn test_encode_file_uri_escapes_literal_percent() {
        // A literal '%' in the path must be re-encoded as %25, otherwise the
        // decoder would treat e.g. "%2F" in the filename as a slash.
        let path = PathBuf::from("/x/already%2Fencoded.txt");
        let uri = encode_file_uri(&path);
        assert_eq!(uri, "file:///x/already%252Fencoded.txt");
    }
}
