//! Clipboard Synchronization Module
//!
//! Provides complete bidirectional clipboard synchronization between RDP
//! clients and Wayland compositors with production-grade loop prevention,
//! format conversion, and efficient data transfer.
//!
//! # Architecture
//!
//! This module uses the lamco crate ecosystem for clipboard primitives:
//!
//! - [`lamco_clipboard_core`] - Format conversion, loop detection, transfer engine
//! - [`lamco_portal::dbus_clipboard`] - D-Bus GNOME clipboard bridge
//! - [`lamco_rdp_clipboard`] - IronRDP clipboard backend
//!
//! The server adds:
//!
//! - [`SyncManager`] - State machine orchestration (server-specific policy)
//! - [`ClipboardManager`] - Event routing between Portal and RDP
//! - [`LamcoCliprdrFactory`] - Server-specific backend factory wrapper
//!
//! # Data Flow
//!
//! ```text
//! RDP Client                IronRDP               Server             Portal            Wayland
//! ━━━━━━━━━━                ━━━━━━━               ━━━               ━━━━━━            ━━━━━━━
//!
//! Copy (Ctrl+C)
//!   └─> Format List ────> RdpCliprdrBackend ─> ClipboardManager
//!                                                    │
//!                                                    ├─> SyncManager (state)
//!                                                    ├─> LoopDetector (library)
//!                                                    ├─> FormatConverter (library)
//!                                                    │
//!                                                    └─────────────> Clipboard API
//!
//! Paste (Ctrl+V) <── Data Response <── Convert <── Get Data <──── DbusClipboardBridge
//! ```
//!
//! # Features
//!
//! - **Bidirectional Sync**: RDP ↔ Wayland clipboard sharing
//! - **Format Conversion**: Text (UTF-8/UTF-16/HTML/RTF), Images (PNG/JPEG/BMP/DIB), Files
//! - **Loop Prevention**: Content hashing + state machine + echo protection
//! - **Chunked Transfer**: Large data (>1MB) with progress tracking
//! - **Error Recovery**: Policy-based retry and fallback strategies

// Server-specific modules (policy and orchestration)
pub mod error;
pub mod fuse;
pub mod ironrdp_backend;
pub mod manager;
pub mod sync;

// =============================================================================
// Re-export from lamco crates (library primitives)
// =============================================================================

// Format conversion and clipboard types from lamco-clipboard-core
pub use lamco_clipboard_core::{
    // Error types (base)
    ClipboardError as CoreClipboardError,
    ClipboardFormat,
    FormatConverter,
    LoopDetectionConfig,
    // Loop detection
    LoopDetector,
    // Transfer engine
    TransferConfig,
    TransferEngine,
    TransferProgress,
    TransferState,
};

// Standalone format mapping functions from library
pub use lamco_clipboard_core::formats::{
    mime_to_rdp_formats as lib_mime_to_rdp_formats, rdp_format_to_mime as lib_rdp_format_to_mime,
};

// D-Bus bridge from lamco-portal (GNOME fallback)
pub use lamco_portal::dbus_clipboard::{DbusClipboardBridge, DbusClipboardEvent};

// RDP backend from lamco-rdp-clipboard
pub use lamco_rdp_clipboard::{
    ClipboardEvent as RdpClipboardEvent, ClipboardEventReceiver, ClipboardEventSender,
    ClipboardGeneralCapabilityFlags, RdpCliprdrBackend, RdpCliprdrFactory as LibRdpCliprdrFactory,
};

// =============================================================================
// Re-export server-specific types
// =============================================================================

// Server error types (extends library errors with recovery policy)
pub use error::{ClipboardError, ErrorContext, ErrorType, RecoveryAction, Result, RetryConfig};

// Server IronRDP factory (wraps library factory)
pub use ironrdp_backend::LamcoCliprdrFactory;

// Server clipboard manager
pub use manager::{ClipboardConfig, ClipboardEvent, ClipboardManager};

// Server sync manager (state machine + echo protection)
pub use sync::{ClipboardState, SyncDirection, SyncManager};

// FUSE-based clipboard file transfer
pub use fuse::{
    generate_gnome_copied_files_content, generate_uri_list_content, get_mount_point,
    FileContentsRequest, FileContentsResponse, FileDescriptor, FuseManager,
};

// =============================================================================
// Extension trait for FormatConverter (server-specific convenience methods)
// =============================================================================

/// Extension trait adding convenience methods to FormatConverter
///
/// The library provides standalone functions for format mapping.
/// This trait wraps them as methods for ergonomic use in the server.
pub trait FormatConverterExt {
    /// Convert RDP formats to MIME types
    fn rdp_to_mime_types(&self, formats: &[ClipboardFormat]) -> error::Result<Vec<String>>;

    /// Convert MIME types to RDP formats
    fn mime_to_rdp_formats(&self, mime_types: &[String]) -> error::Result<Vec<RdpFormat>>;

    /// Convert a single format ID to MIME type
    fn format_id_to_mime(&self, format_id: u32) -> error::Result<String>;

    /// Convert a single MIME type to format ID
    fn mime_to_format_id(&self, mime_type: &str) -> error::Result<u32>;
}

/// RDP format with ID and name (for server compatibility)
#[derive(Debug, Clone)]
pub struct RdpFormat {
    /// Format ID
    pub format_id: u32,
    /// Format name (empty string if no name)
    pub format_name: String,
}

/// Convert registered format name to MIME type.
///
/// Windows clipboard formats like FileGroupDescriptorW and FileContents have
/// **registered format IDs** assigned at runtime by `RegisterClipboardFormat()`.
/// The IDs vary between sessions/machines, but the names are constant.
///
/// This function maps well-known format names to MIME types.
pub fn format_name_to_mime(name: &str) -> Option<&'static str> {
    match name {
        // File transfer formats → text/uri-list
        "FileGroupDescriptorW" | "FileGroupDescriptor" => Some("text/uri-list"),
        // FileContents is a data retrieval mechanism, not a standalone format
        // But if it appears in a format list, it indicates file transfer capability
        "FileContents" => None, // Not mapped directly; FileGroupDescriptorW takes precedence
        // Drop effect is metadata, not actual content
        "Preferred DropEffect" => None,
        // HTML formats
        "HTML Format" => Some("text/html"),
        // RTF formats
        "Rich Text Format" => Some("text/rtf"),
        // Other common registered formats can be added here
        _ => None,
    }
}

impl FormatConverterExt for FormatConverter {
    fn rdp_to_mime_types(&self, formats: &[ClipboardFormat]) -> error::Result<Vec<String>> {
        let mut mime_types = Vec::new();
        for format in formats {
            // First try ID-based lookup
            if let Some(mime) = lib_rdp_format_to_mime(format.id) {
                if !mime_types.contains(&mime.to_string()) {
                    mime_types.push(mime.to_string());
                }
            } else if let Some(ref name) = format.name {
                // For registered formats, the ID varies per session but the name is constant.
                // Check the format name for known registered formats.
                let mime = format_name_to_mime(name);
                if let Some(m) = mime {
                    if !mime_types.contains(&m.to_string()) {
                        mime_types.push(m.to_string());
                    }
                    // For file formats, also announce x-special/gnome-copied-files for GNOME compatibility
                    // KDE Dolphin accepts text/uri-list, but GNOME Nautilus requires gnome-copied-files
                    if m == "text/uri-list" {
                        let gnome_mime = "x-special/gnome-copied-files";
                        if !mime_types.contains(&gnome_mime.to_string()) {
                            mime_types.push(gnome_mime.to_string());
                        }
                    }
                }
            }
        }
        if mime_types.is_empty() {
            // Fall back to text/plain if no mapping found
            mime_types.push("text/plain".to_string());
        }
        Ok(mime_types)
    }

    fn mime_to_rdp_formats(&self, mime_types: &[String]) -> error::Result<Vec<RdpFormat>> {
        let strs: Vec<&str> = mime_types.iter().map(|s| s.as_str()).collect();
        let formats = lib_mime_to_rdp_formats(&strs);
        Ok(formats
            .into_iter()
            .map(|f| RdpFormat {
                format_id: f.id,
                format_name: f.name.unwrap_or_default(),
            })
            .collect())
    }

    fn format_id_to_mime(&self, format_id: u32) -> error::Result<String> {
        lib_rdp_format_to_mime(format_id)
            .map(|s| s.to_string())
            .ok_or_else(|| {
                error::ClipboardError::Core(CoreClipboardError::UnsupportedFormat(format!(
                    "No MIME type for format ID {}",
                    format_id
                )))
            })
    }

    fn mime_to_format_id(&self, mime_type: &str) -> error::Result<u32> {
        let formats = lib_mime_to_rdp_formats(&[mime_type]);
        formats.first().map(|f| f.id).ok_or_else(|| {
            error::ClipboardError::Core(CoreClipboardError::UnsupportedFormat(
                mime_type.to_string(),
            ))
        })
    }
}
