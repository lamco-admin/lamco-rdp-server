//! ClipboardSink trait - abstract clipboard backend interface.
//!
//! This trait defines the interface that clipboard backends must implement.
//! It is protocol-agnostic and uses MIME types for format identification.

use crate::ClipboardResult;
use std::future::Future;

/// Information about a file in the clipboard
#[derive(Debug, Clone)]
pub struct FileInfo {
    /// File name (without path)
    pub name: String,

    /// File size in bytes
    pub size: u64,

    /// MIME type of the file (if known)
    pub mime_type: Option<String>,

    /// Whether this is a directory
    pub is_directory: bool,

    /// Last modified timestamp (Unix epoch seconds)
    pub modified: Option<u64>,
}

impl FileInfo {
    /// Create a new FileInfo for a regular file
    pub fn file(name: impl Into<String>, size: u64) -> Self {
        Self {
            name: name.into(),
            size,
            mime_type: None,
            is_directory: false,
            modified: None,
        }
    }

    /// Create a new FileInfo for a directory
    pub fn directory(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            size: 0,
            mime_type: None,
            is_directory: true,
            modified: None,
        }
    }

    /// Set the MIME type
    pub fn with_mime_type(mut self, mime_type: impl Into<String>) -> Self {
        self.mime_type = Some(mime_type.into());
        self
    }

    /// Set the modified timestamp
    pub fn with_modified(mut self, timestamp: u64) -> Self {
        self.modified = Some(timestamp);
        self
    }
}

/// A clipboard change notification
#[derive(Debug, Clone)]
pub struct ClipboardChange {
    /// MIME types available in the clipboard
    pub mime_types: Vec<String>,

    /// Whether this is the primary selection (X11) or clipboard
    pub is_primary: bool,

    /// Content hash for deduplication (optional)
    pub content_hash: Option<String>,
}

impl ClipboardChange {
    /// Create a new clipboard change notification
    pub fn new(mime_types: Vec<String>) -> Self {
        Self {
            mime_types,
            is_primary: false,
            content_hash: None,
        }
    }

    /// Set whether this is the primary selection
    pub fn with_primary(mut self, is_primary: bool) -> Self {
        self.is_primary = is_primary;
        self
    }

    /// Set the content hash
    pub fn with_hash(mut self, hash: impl Into<String>) -> Self {
        self.content_hash = Some(hash.into());
        self
    }
}

/// Receiver for clipboard change notifications.
///
/// This is a channel receiver that yields clipboard changes.
/// Implementations should use tokio or async-std channels internally.
pub struct ClipboardChangeReceiver {
    inner: Box<dyn ClipboardChangeReceiverInner>,
}

impl ClipboardChangeReceiver {
    /// Create a new receiver from a boxed inner implementation
    pub fn new(inner: Box<dyn ClipboardChangeReceiverInner>) -> Self {
        Self { inner }
    }

    /// Wait for the next clipboard change (blocking)
    pub fn recv_blocking(&mut self) -> Option<ClipboardChange> {
        self.inner.recv_blocking()
    }

    /// Try to receive without blocking
    pub fn try_recv(&mut self) -> Option<ClipboardChange> {
        self.inner.try_recv()
    }
}

/// Inner trait for clipboard change receivers (object-safe)
pub trait ClipboardChangeReceiverInner: Send {
    /// Receive the next change (blocking)
    fn recv_blocking(&mut self) -> Option<ClipboardChange>;

    /// Try to receive without blocking
    fn try_recv(&mut self) -> Option<ClipboardChange>;
}

/// Abstract clipboard backend interface.
///
/// This trait defines the operations that clipboard backends must implement.
/// It uses MIME types for format identification - the [`FormatConverter`](crate::FormatConverter)
/// handles conversion to/from Windows clipboard format IDs.
///
/// # Design Principles
///
/// - **Async by default**: All operations return futures (Portal uses D-Bus, file I/O shouldn't block)
/// - **MIME-centric**: Uses MIME types, not Windows format IDs
/// - **File transfer support**: Includes methods for MS-RDPECLIP FileContents protocol
/// - **Minimal surface**: 7 core methods
///
/// # Example
///
/// ```rust,ignore
/// use lamco_clipboard_core::{ClipboardSink, ClipboardResult, FileInfo};
///
/// struct MyClipboard { /* ... */ }
///
/// impl ClipboardSink for MyClipboard {
///     async fn announce_formats(&self, mime_types: Vec<String>) -> ClipboardResult<()> {
///         // Announce that these formats are available
///         Ok(())
///     }
///
///     async fn read_clipboard(&self, mime_type: &str) -> ClipboardResult<Vec<u8>> {
///         // Read clipboard data for the given MIME type
///         Ok(vec![])
///     }
///
///     // ... implement other methods
/// }
/// ```
pub trait ClipboardSink: Send + Sync {
    /// Announce that new clipboard content is available.
    ///
    /// This is called when the clipboard owner changes. The backend should
    /// store the format list and be ready to provide data when requested.
    ///
    /// # Arguments
    ///
    /// * `mime_types` - MIME types available in the clipboard
    fn announce_formats(&self, mime_types: Vec<String>) -> impl Future<Output = ClipboardResult<()>> + Send;

    /// Read clipboard data for a MIME type.
    ///
    /// This may trigger delayed rendering if the data is not immediately available.
    ///
    /// # Arguments
    ///
    /// * `mime_type` - The MIME type to read
    fn read_clipboard(&self, mime_type: &str) -> impl Future<Output = ClipboardResult<Vec<u8>>> + Send;

    /// Write data to the clipboard.
    ///
    /// # Arguments
    ///
    /// * `mime_type` - The MIME type of the data
    /// * `data` - The clipboard data
    fn write_clipboard(&self, mime_type: &str, data: Vec<u8>) -> impl Future<Output = ClipboardResult<()>> + Send;

    /// Subscribe to clipboard change notifications.
    ///
    /// Returns a receiver that yields clipboard changes as they occur.
    fn subscribe_changes(&self) -> impl Future<Output = ClipboardResult<ClipboardChangeReceiver>> + Send;

    /// Get the list of files in the clipboard.
    ///
    /// This is used for file transfer support (CF_HDROP / FileContents).
    fn get_file_list(&self) -> impl Future<Output = ClipboardResult<Vec<FileInfo>>> + Send;

    /// Read a chunk of a file from the clipboard.
    ///
    /// This is used for chunked file transfer (MS-RDPECLIP FileContents).
    ///
    /// # Arguments
    ///
    /// * `index` - Index of the file in the file list
    /// * `offset` - Byte offset to start reading from
    /// * `size` - Number of bytes to read
    fn read_file_chunk(
        &self,
        index: u32,
        offset: u64,
        size: u32,
    ) -> impl Future<Output = ClipboardResult<Vec<u8>>> + Send;

    /// Write a file to the clipboard destination.
    ///
    /// This is used when receiving files from the remote clipboard.
    ///
    /// # Arguments
    ///
    /// * `path` - Destination path for the file
    /// * `data` - File contents
    fn write_file(&self, path: &str, data: Vec<u8>) -> impl Future<Output = ClipboardResult<()>> + Send;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_file_info_builder() {
        let file = FileInfo::file("test.txt", 1024)
            .with_mime_type("text/plain")
            .with_modified(1234567890);

        assert_eq!(file.name, "test.txt");
        assert_eq!(file.size, 1024);
        assert_eq!(file.mime_type, Some("text/plain".to_string()));
        assert!(!file.is_directory);
        assert_eq!(file.modified, Some(1234567890));
    }

    #[test]
    fn test_clipboard_change() {
        let change = ClipboardChange::new(vec!["text/plain".to_string()])
            .with_primary(true)
            .with_hash("abc123");

        assert_eq!(change.mime_types, vec!["text/plain"]);
        assert!(change.is_primary);
        assert_eq!(change.content_hash, Some("abc123".to_string()));
    }
}
