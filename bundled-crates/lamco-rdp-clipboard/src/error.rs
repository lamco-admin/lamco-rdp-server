//! Error types for RDP clipboard operations.

use lamco_clipboard_core::ClipboardError;
use thiserror::Error;

/// Errors that can occur during RDP clipboard operations.
#[derive(Debug, Error)]
pub enum ClipboardRdpError {
    /// Error from the clipboard core library
    #[error("clipboard error: {0}")]
    Clipboard(#[from] ClipboardError),

    /// Error sending clipboard event
    #[error("failed to send clipboard event: {0}")]
    SendError(String),

    /// Error receiving clipboard event
    #[error("failed to receive clipboard event: {0}")]
    RecvError(String),

    /// Backend not initialized
    #[error("clipboard backend not initialized")]
    NotInitialized,

    /// Invalid state
    #[error("invalid state: {0}")]
    InvalidState(String),

    /// Format not available
    #[error("format not available: {0}")]
    FormatNotAvailable(u32),

    /// File transfer error
    #[error("file transfer error: {0}")]
    FileTransfer(String),

    /// Timeout waiting for response
    #[error("timeout waiting for clipboard response")]
    Timeout,
}

/// Result type for RDP clipboard operations.
pub type ClipboardRdpResult<T> = Result<T, ClipboardRdpError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_display() {
        let err = ClipboardRdpError::NotInitialized;
        assert_eq!(err.to_string(), "clipboard backend not initialized");

        let err = ClipboardRdpError::FormatNotAvailable(13);
        assert_eq!(err.to_string(), "format not available: 13");
    }

    #[test]
    fn test_from_clipboard_error() {
        let core_err = ClipboardError::UnsupportedFormat("test".to_string());
        let rdp_err: ClipboardRdpError = core_err.into();
        assert!(matches!(rdp_err, ClipboardRdpError::Clipboard(_)));
    }
}
