//! # lamco-clipboard-core
//!
//! Protocol-agnostic clipboard utilities for Rust.
//!
//! This crate provides core clipboard functionality that can be used with any
//! clipboard backend (Portal, X11, headless, etc.):
//!
//! - **[`ClipboardSink`] trait** - Abstract clipboard backend interface
//! - **[`FormatConverter`]** - MIME ↔ Windows clipboard format conversion
//! - **[`LoopDetector`]** - Prevent clipboard sync loops with content hashing
//! - **[`TransferEngine`]** - Chunked transfer for large clipboard data
//!
//! ## Quick Start
//!
//! ```rust
//! use lamco_clipboard_core::{ClipboardSink, FormatConverter, LoopDetector};
//! use lamco_clipboard_core::formats::{ClipboardFormat, mime_to_rdp_formats};
//!
//! // Convert MIME types to RDP formats
//! let formats = mime_to_rdp_formats(&["text/plain", "text/html"]);
//!
//! // Check for clipboard loops
//! let mut detector = LoopDetector::new();
//! if !detector.would_cause_loop(&formats) {
//!     // Safe to sync
//! }
//! ```
//!
//! ## Feature Flags
//!
//! - `image` - Enable image format conversion (PNG, JPEG, BMP ↔ DIB)
//!
//! ## Architecture
//!
//! The [`ClipboardSink`] trait provides an async interface for clipboard operations.
//! Implementations handle the actual clipboard access (Portal D-Bus, X11, etc.)
//! while this crate handles format conversion and loop detection.

#![cfg_attr(docsrs, feature(doc_cfg))]
#![deny(missing_docs)]

mod error;
mod sink;
mod transfer;

pub mod formats;
pub mod loop_detector;
pub mod sanitize;

#[cfg(feature = "image")]
pub mod image;

pub use error::{ClipboardError, ClipboardResult};
pub use formats::{
    build_file_group_descriptor_w, ClipboardFormat, FileDescriptor, FileDescriptorFlags, FormatConverter,
};
pub use loop_detector::{ClipboardSource, LoopDetectionConfig, LoopDetector};
pub use sink::{ClipboardChange, ClipboardChangeReceiver, ClipboardChangeReceiverInner, ClipboardSink, FileInfo};
pub use transfer::{
    TransferConfig, TransferEngine, TransferProgress, TransferState, DEFAULT_CHUNK_SIZE, DEFAULT_MAX_SIZE,
    DEFAULT_TIMEOUT_MS,
};

/// Prelude module for convenient imports
pub mod prelude {
    pub use crate::formats::{mime_to_rdp_formats, rdp_format_to_mime};
    pub use crate::{ClipboardChange, ClipboardError, ClipboardResult, ClipboardSink, FormatConverter, LoopDetector};
}
