//! Clipboard Error Types
//!
//! **Execution Path:** N/A (error type definitions)
//! **Status:** Active (v1.0.0+)
//! **Platform:** Universal
//!
//! Server-specific error handling for the clipboard synchronization module.
//!
//! This module extends the base errors from [`lamco_clipboard_core::ClipboardError`]
//! with server-specific error recovery policy.

use thiserror::Error;

// Re-export base error from library
pub use lamco_clipboard_core::ClipboardError as CoreClipboardError;

/// Result type for clipboard operations
pub type Result<T> = std::result::Result<T, ClipboardError>;

/// Server clipboard error types
///
/// Wraps [`CoreClipboardError`] and adds server-specific error variants.
#[derive(Error, Debug)]
pub enum ClipboardError {
    /// Core clipboard error (from lamco-clipboard-core)
    #[error(transparent)]
    Core(#[from] CoreClipboardError),

    /// Portal clipboard error (server-specific)
    #[error("Portal clipboard error: {0}")]
    PortalError(String),

    /// Invalid state for operation (server state machine)
    #[error("Invalid state: {0}")]
    InvalidState(String),

    /// RDP connection error
    #[error("RDP connection error: {0}")]
    RdpConnectionError(String),

    /// D-Bus error
    #[error("D-Bus error: {0}")]
    DBus(String),

    /// Channel send error (server internal)
    #[error("Channel send error")]
    ChannelSend,

    /// Channel receive error (server internal)
    #[error("Channel receive error")]
    ChannelReceive,

    /// Loop detected (from SyncManager policy)
    #[error("Clipboard loop detected")]
    LoopDetected,

    /// File I/O error during file transfer
    #[error("File I/O error: {0}")]
    FileIoError(String),

    /// Component not initialized
    #[error("Component not initialized")]
    NotInitialized,

    /// Unknown error
    #[error("Unknown error: {0}")]
    Unknown(String),
}

impl ClipboardError {
    pub fn io(e: std::io::Error) -> Self {
        Self::Core(CoreClipboardError::Io(e))
    }
}

/// Error classification for recovery strategies
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ErrorType {
    /// Portal-related errors
    Portal,
    /// Format conversion errors
    FormatConversion,
    /// Data validation errors
    DataValidation,
    /// Transfer errors
    Transfer,
    /// State errors
    State,
    /// Communication errors
    Communication,
    /// Loop detection errors
    Loop,
    /// Unknown error type
    Unknown,
}

/// Classify error for recovery strategy selection
pub fn classify_error(error: &ClipboardError) -> ErrorType {
    match error {
        ClipboardError::PortalError(_) | ClipboardError::DBus(_) => ErrorType::Portal,
        ClipboardError::InvalidState(_) => ErrorType::State,
        ClipboardError::ChannelSend | ClipboardError::ChannelReceive => ErrorType::Communication,
        ClipboardError::LoopDetected => ErrorType::Loop,
        ClipboardError::RdpConnectionError(_) => ErrorType::Transfer,
        ClipboardError::FileIoError(_) => ErrorType::Transfer,
        ClipboardError::NotInitialized => ErrorType::State,
        ClipboardError::Unknown(_) => ErrorType::Unknown,

        ClipboardError::Core(core_err) => classify_core_error(core_err),
    }
}

fn classify_core_error(error: &CoreClipboardError) -> ErrorType {
    match error {
        CoreClipboardError::UnsupportedFormat(_) | CoreClipboardError::FormatConversion(_) => {
            ErrorType::FormatConversion
        }

        CoreClipboardError::ImageDecode(_) | CoreClipboardError::ImageEncode(_) => {
            ErrorType::FormatConversion
        }

        CoreClipboardError::InvalidUtf8
        | CoreClipboardError::InvalidUtf16
        | CoreClipboardError::DataSizeExceeded { .. } => ErrorType::DataValidation,

        CoreClipboardError::TransferTimeout(_) | CoreClipboardError::TransferCancelled => {
            ErrorType::Transfer
        }

        CoreClipboardError::Backend(_) => ErrorType::Communication,

        CoreClipboardError::LoopDetected => ErrorType::Loop,

        // Catch-all for any other variants
        #[allow(unreachable_patterns)]
        _ => ErrorType::Unknown,
    }
}

/// Error context for recovery decisions
#[derive(Debug, Clone)]
pub struct ErrorContext {
    /// Format ID if applicable
    pub format_id: Option<u32>,

    /// MIME type if applicable
    pub mime_type: Option<String>,

    /// Data size if applicable
    pub data_size: Option<usize>,

    /// Retry attempt number
    pub attempt: u32,

    /// Additional context information
    pub details: String,
}

impl ErrorContext {
    pub fn new() -> Self {
        Self {
            format_id: None,
            mime_type: None,
            data_size: None,
            attempt: 0,
            details: String::new(),
        }
    }

    pub fn with_format_id(mut self, id: u32) -> Self {
        self.format_id = Some(id);
        self
    }

    pub fn with_mime_type(mut self, mime_type: impl Into<String>) -> Self {
        self.mime_type = Some(mime_type.into());
        self
    }

    pub fn with_data_size(mut self, size: usize) -> Self {
        self.data_size = Some(size);
        self
    }

    pub fn with_attempt(mut self, attempt: u32) -> Self {
        self.attempt = attempt;
        self
    }

    pub fn with_details(mut self, details: impl Into<String>) -> Self {
        self.details = details.into();
        self
    }
}

impl Default for ErrorContext {
    fn default() -> Self {
        Self::new()
    }
}

/// Recovery action to take after error
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecoveryAction {
    /// Retry the operation
    Retry(RetryConfig),

    /// Try with fallback format
    RetryWithFallbackFormat,

    /// Reduce data size and retry
    ReduceDataSize(usize),

    /// Skip this transfer
    Skip,

    /// Reset clipboard state
    ResetState,

    /// Request new portal session
    RequestNewSession,

    /// Fail and propagate error
    Fail,
}

/// Retry configuration
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetryConfig {
    /// Maximum number of retries
    pub max_retries: u32,

    /// Initial delay in milliseconds
    pub initial_delay_ms: u64,

    /// Backoff multiplier
    pub backoff_multiplier: u32,

    /// Maximum delay in milliseconds
    pub max_delay_ms: u64,
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            max_retries: 3,
            initial_delay_ms: 100,
            backoff_multiplier: 2,
            max_delay_ms: 5000,
        }
    }
}

impl RetryConfig {
    pub fn delay_for_attempt(&self, attempt: u32) -> std::time::Duration {
        let delay = self.initial_delay_ms * (self.backoff_multiplier as u64).pow(attempt);
        let delay = delay.min(self.max_delay_ms);
        std::time::Duration::from_millis(delay)
    }
}

/// Determine recovery action for error
pub fn recovery_action(error: &ClipboardError, context: &ErrorContext) -> RecoveryAction {
    match classify_error(error) {
        ErrorType::Portal => {
            if context.attempt < 2 {
                RecoveryAction::Retry(RetryConfig::default())
            } else {
                RecoveryAction::RequestNewSession
            }
        }

        ErrorType::FormatConversion => {
            if context.attempt == 0 {
                RecoveryAction::RetryWithFallbackFormat
            } else {
                RecoveryAction::Skip
            }
        }

        ErrorType::DataValidation => {
            if let ClipboardError::Core(CoreClipboardError::DataSizeExceeded { actual, max }) =
                error
            {
                let reduced_size = max / 2;
                if *actual > reduced_size {
                    return RecoveryAction::ReduceDataSize(reduced_size);
                }
            }
            RecoveryAction::Skip
        }

        ErrorType::Transfer => {
            if context.attempt < 3 {
                RecoveryAction::Retry(RetryConfig::default())
            } else {
                RecoveryAction::Fail
            }
        }

        ErrorType::State => RecoveryAction::ResetState,

        ErrorType::Communication => {
            if context.attempt < 2 {
                RecoveryAction::Retry(RetryConfig {
                    max_retries: 2,
                    initial_delay_ms: 50,
                    backoff_multiplier: 2,
                    max_delay_ms: 1000,
                })
            } else {
                RecoveryAction::Fail
            }
        }

        ErrorType::Loop => RecoveryAction::Skip,

        ErrorType::Unknown => RecoveryAction::Fail,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_classification_server_errors() {
        // Server-specific errors
        let error = ClipboardError::PortalError("test".to_string());
        assert_eq!(classify_error(&error), ErrorType::Portal);

        let error = ClipboardError::DBus("test".to_string());
        assert_eq!(classify_error(&error), ErrorType::Portal);

        let error = ClipboardError::InvalidState("test".to_string());
        assert_eq!(classify_error(&error), ErrorType::State);

        let error = ClipboardError::ChannelSend;
        assert_eq!(classify_error(&error), ErrorType::Communication);

        let error = ClipboardError::LoopDetected;
        assert_eq!(classify_error(&error), ErrorType::Loop);

        let error = ClipboardError::RdpConnectionError("test".to_string());
        assert_eq!(classify_error(&error), ErrorType::Transfer);
    }

    #[test]
    fn test_error_classification_core_errors() {
        // Core library errors (wrapped)
        let error = ClipboardError::Core(CoreClipboardError::UnsupportedFormat("test".to_string()));
        assert_eq!(classify_error(&error), ErrorType::FormatConversion);

        let error = ClipboardError::Core(CoreClipboardError::InvalidUtf8);
        assert_eq!(classify_error(&error), ErrorType::DataValidation);

        let error = ClipboardError::Core(CoreClipboardError::TransferCancelled);
        assert_eq!(classify_error(&error), ErrorType::Transfer);
    }

    #[test]
    fn test_error_context() {
        let ctx = ErrorContext::new()
            .with_format_id(13)
            .with_mime_type("text/plain")
            .with_data_size(1024)
            .with_attempt(2)
            .with_details("test error");

        assert_eq!(ctx.format_id, Some(13));
        assert_eq!(ctx.mime_type, Some("text/plain".to_string()));
        assert_eq!(ctx.data_size, Some(1024));
        assert_eq!(ctx.attempt, 2);
        assert_eq!(ctx.details, "test error");
    }

    #[test]
    fn test_retry_config() {
        let config = RetryConfig::default();

        assert_eq!(config.delay_for_attempt(0).as_millis(), 100);
        assert_eq!(config.delay_for_attempt(1).as_millis(), 200);
        assert_eq!(config.delay_for_attempt(2).as_millis(), 400);
        assert_eq!(config.delay_for_attempt(3).as_millis(), 800);

        // Should cap at max_delay_ms
        assert_eq!(config.delay_for_attempt(10).as_millis(), 5000);
    }

    #[test]
    fn test_recovery_action_portal_error() {
        let error = ClipboardError::PortalError("test".to_string());
        let ctx = ErrorContext::new().with_attempt(0);

        match recovery_action(&error, &ctx) {
            RecoveryAction::Retry(_) => {}
            _ => panic!("Expected Retry action"),
        }

        let ctx = ErrorContext::new().with_attempt(3);
        match recovery_action(&error, &ctx) {
            RecoveryAction::RequestNewSession => {}
            _ => panic!("Expected RequestNewSession action"),
        }
    }

    #[test]
    fn test_recovery_action_format_error() {
        let error = ClipboardError::Core(CoreClipboardError::UnsupportedFormat("test".to_string()));
        let ctx = ErrorContext::new().with_attempt(0);

        match recovery_action(&error, &ctx) {
            RecoveryAction::RetryWithFallbackFormat => {}
            _ => panic!("Expected RetryWithFallbackFormat action"),
        }
    }

    #[test]
    fn test_recovery_action_size_exceeded() {
        let error = ClipboardError::Core(CoreClipboardError::DataSizeExceeded {
            actual: 20_000_000,
            max: 16_777_216,
        });
        let ctx = ErrorContext::new();

        match recovery_action(&error, &ctx) {
            RecoveryAction::ReduceDataSize(size) => {
                assert_eq!(size, 16_777_216 / 2);
            }
            _ => panic!("Expected ReduceDataSize action"),
        }
    }

    #[test]
    fn test_recovery_action_loop_detected() {
        let error = ClipboardError::LoopDetected;
        let ctx = ErrorContext::new();

        match recovery_action(&error, &ctx) {
            RecoveryAction::Skip => {}
            _ => panic!("Expected Skip action"),
        }
    }
}
