//! Error types for clipboard operations.

use thiserror::Error;

/// Result type for clipboard operations
pub type ClipboardResult<T> = std::result::Result<T, ClipboardError>;

/// Errors that can occur during clipboard operations
#[derive(Error, Debug)]
pub enum ClipboardError {
    /// Backend error (Portal, X11, etc.)
    #[error("backend error: {0}")]
    Backend(String),

    /// Format conversion failed
    #[error("format conversion failed: {0}")]
    FormatConversion(String),

    /// Unsupported clipboard format
    #[error("unsupported format: {0}")]
    UnsupportedFormat(String),

    /// Invalid UTF-8 data
    #[error("invalid UTF-8 data")]
    InvalidUtf8,

    /// Invalid UTF-16 data
    #[error("invalid UTF-16 data")]
    InvalidUtf16,

    /// Image decode error
    #[error("image decode error: {0}")]
    ImageDecode(String),

    /// Image encode error
    #[error("image encode error: {0}")]
    ImageEncode(String),

    /// Data size exceeded maximum
    #[error("data size {actual} exceeds maximum {max}")]
    DataSizeExceeded {
        /// Actual size in bytes
        actual: usize,
        /// Maximum allowed size in bytes
        max: usize,
    },

    /// Transfer timeout
    #[error("transfer timeout after {0}ms")]
    TransferTimeout(u64),

    /// Transfer was cancelled
    #[error("transfer cancelled")]
    TransferCancelled,

    /// Loop detected - would cause clipboard sync loop
    #[error("clipboard loop detected")]
    LoopDetected,

    /// Invalid state for operation
    #[error("invalid state: {0}")]
    InvalidState(String),

    /// File not found
    #[error("file not found: {0}")]
    FileNotFound(String),

    /// Permission denied
    #[error("permission denied: {0}")]
    PermissionDenied(String),

    /// I/O error
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

impl ClipboardError {
    /// Returns true if this error is recoverable
    pub fn is_recoverable(&self) -> bool {
        matches!(
            self,
            Self::TransferTimeout(_) | Self::LoopDetected | Self::InvalidState(_)
        )
    }

    /// Returns true if this error indicates a format issue
    pub fn is_format_error(&self) -> bool {
        matches!(
            self,
            Self::FormatConversion(_)
                | Self::UnsupportedFormat(_)
                | Self::InvalidUtf8
                | Self::InvalidUtf16
                | Self::ImageDecode(_)
                | Self::ImageEncode(_)
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_display() {
        let err = ClipboardError::FormatConversion("test".to_string());
        assert_eq!(err.to_string(), "format conversion failed: test");
    }

    #[test]
    fn test_is_recoverable() {
        assert!(ClipboardError::LoopDetected.is_recoverable());
        assert!(ClipboardError::TransferTimeout(1000).is_recoverable());
        assert!(!ClipboardError::InvalidUtf8.is_recoverable());
    }

    #[test]
    fn test_is_format_error() {
        assert!(ClipboardError::InvalidUtf8.is_format_error());
        assert!(ClipboardError::UnsupportedFormat("test".to_string()).is_format_error());
        assert!(!ClipboardError::LoopDetected.is_format_error());
    }
}
