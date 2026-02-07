//! Unified error types for hardware encoding backends
//!
//! This module provides a common error type that wraps backend-specific
//! errors from VA-API and NVENC, enabling unified error handling in
//! the encoder factory and display handler.

use std::path::PathBuf;
use thiserror::Error;

/// Unified error type for hardware encoding operations
///
/// This enum covers initialization, runtime encoding, and configuration
/// errors from all hardware backends. Backend-specific errors are wrapped
/// to provide detailed diagnostics while maintaining a common interface.
#[derive(Debug, Error)]
pub enum HardwareEncoderError {
    // =========================================================================
    // Initialization Errors
    // =========================================================================
    /// No hardware encoding backend is available on this system
    #[error("No hardware encoder available: {reason}")]
    NoBackendAvailable { reason: String },

    /// The specified GPU device could not be opened
    #[error("Device not found: {path}")]
    DeviceNotFound { path: PathBuf },

    /// The GPU does not support H.264 encoding
    #[error("H.264 encoding not supported by hardware")]
    H264NotSupported,

    /// Failed to initialize the encoder context
    #[error("Encoder initialization failed: {0}")]
    InitFailed(String),

    /// The requested encoder configuration is not supported
    #[error("Unsupported configuration: {0}")]
    UnsupportedConfig(String),

    // =========================================================================
    // Runtime Encoding Errors
    // =========================================================================
    /// Frame encoding failed
    #[error("Encode failed: {0}")]
    EncodeFailed(String),

    /// No more surfaces/buffers available in the pool
    #[error("Buffer pool exhausted (need {needed}, have {available})")]
    BufferPoolExhausted { needed: usize, available: usize },

    /// Invalid frame dimensions provided
    #[error("Invalid dimensions: {width}x{height} - {reason}")]
    InvalidDimensions {
        width: u32,
        height: u32,
        reason: String,
    },

    /// Color conversion failed (BGRA to NV12)
    #[error("Color conversion failed: {0}")]
    ColorConversionFailed(String),

    /// Timeout waiting for encoder to complete
    #[error("Encoder timeout after {timeout_ms}ms")]
    Timeout { timeout_ms: u64 },

    // =========================================================================
    // Configuration Errors
    // =========================================================================
    /// Dynamic resolution change is not supported by this backend
    #[error("Resolution reconfiguration not supported by {backend}")]
    ReconfigureNotSupported { backend: &'static str },

    /// Invalid quality preset specified
    #[error("Invalid quality preset: {preset} (valid: speed, balanced, quality)")]
    InvalidPreset { preset: String },

    // =========================================================================
    // Backend-Specific Errors (wrapped)
    // =========================================================================
    /// VA-API specific error
    #[cfg(feature = "vaapi")]
    #[error("VA-API error: {0}")]
    Vaapi(#[from] VaapiError),

    /// NVENC specific error
    #[cfg(feature = "nvenc")]
    #[error("NVENC error: {0}")]
    Nvenc(#[from] NvencError),
}

impl HardwareEncoderError {
    pub fn is_backend_unavailable(&self) -> bool {
        matches!(
            self,
            HardwareEncoderError::NoBackendAvailable { .. }
                | HardwareEncoderError::DeviceNotFound { .. }
                | HardwareEncoderError::H264NotSupported
                | HardwareEncoderError::InitFailed(_)
        )
    }

    pub fn is_recoverable(&self) -> bool {
        matches!(
            self,
            HardwareEncoderError::BufferPoolExhausted { .. } | HardwareEncoderError::Timeout { .. }
        )
    }
}

// =============================================================================
// VA-API Specific Errors
// =============================================================================

/// VA-API backend specific errors
#[cfg(feature = "vaapi")]
#[derive(Debug, Error)]
pub enum VaapiError {
    /// Failed to open DRM render device
    #[error("Failed to open device {path}: {source}")]
    DeviceOpenFailed {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// Failed to initialize VA display
    #[error("VA display initialization failed: {0}")]
    DisplayInitFailed(String),

    /// VA-API version is too old
    #[error("VA-API version {major}.{minor} too old (need {min_major}.{min_minor}+)")]
    VersionTooOld {
        major: i32,
        minor: i32,
        min_major: i32,
        min_minor: i32,
    },

    /// Profile query failed
    #[error("Failed to query VA profiles: {0}")]
    ProfileQueryFailed(String),

    /// Entrypoint query failed
    #[error("Failed to query entrypoints: {0}")]
    EntrypointQueryFailed(String),

    /// H.264 encoding not supported
    #[error("H.264 encoding not supported by this GPU")]
    H264NotSupported,

    /// Encode entrypoint not available
    #[error("Encode entrypoint not available")]
    EncodeNotSupported,

    /// Config creation failed
    #[error("Config creation failed: {0}")]
    ConfigCreateFailed(String),

    /// Surface creation failed
    #[error("Surface creation failed: {0}")]
    SurfaceCreateFailed(String),

    /// Context creation failed
    #[error("Context creation failed: {0}")]
    ContextCreateFailed(String),

    /// VPP (Video Post-Processing) not available
    #[error("VPP not available for color conversion")]
    VppNotAvailable,

    /// Buffer operation failed
    #[error("Buffer operation failed: {0}")]
    BufferError(String),

    /// VA operation returned error status
    #[error("VA call failed: {function}() returned {status}")]
    VaCallFailed { function: &'static str, status: i32 },

    /// Sync operation failed or timed out
    #[error("Surface sync failed: {0}")]
    SyncFailed(String),
}

// =============================================================================
// NVENC Specific Errors
// =============================================================================

/// NVENC backend specific errors
#[cfg(feature = "nvenc")]
#[derive(Debug, Error)]
pub enum NvencError {
    /// CUDA device not found or not accessible
    #[error("CUDA device not found: {0}")]
    CudaDeviceNotFound(String),

    /// NVENC API initialization failed
    #[error("NVENC API initialization failed: {0}")]
    ApiInitFailed(String),

    /// Failed to create encoding session
    #[error("Session creation failed: {0}")]
    SessionCreationFailed(String),

    /// Requested codec not supported
    #[error("Codec {codec} not supported (available: {available:?})")]
    CodecNotSupported {
        codec: String,
        available: Vec<String>,
    },

    /// Preset not supported for this GPU
    #[error("Preset {preset} not supported")]
    PresetNotSupported { preset: String },

    /// Input buffer operation failed
    #[error("Input buffer error: {0}")]
    InputBufferError(String),

    /// Output bitstream operation failed
    #[error("Bitstream error: {0}")]
    BitstreamError(String),

    /// NVENC returned error code
    #[error("NVENC error: {function}() returned {code:?}")]
    NvencCallFailed {
        function: &'static str,
        code: String,
    },

    /// Encoder resources exhausted
    #[error("Encoder resources exhausted")]
    ResourcesExhausted,
}

/// Result type for hardware encoder operations
pub type HardwareEncoderResult<T> = Result<T, HardwareEncoderError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_is_backend_unavailable() {
        let err = HardwareEncoderError::H264NotSupported;
        assert!(err.is_backend_unavailable());

        let err = HardwareEncoderError::EncodeFailed("test".to_string());
        assert!(!err.is_backend_unavailable());
    }

    #[test]
    fn test_error_is_recoverable() {
        let err = HardwareEncoderError::BufferPoolExhausted {
            needed: 1,
            available: 0,
        };
        assert!(err.is_recoverable());

        let err = HardwareEncoderError::H264NotSupported;
        assert!(!err.is_recoverable());
    }

    #[test]
    fn test_error_display() {
        let err = HardwareEncoderError::InvalidDimensions {
            width: 1920,
            height: 1081,
            reason: "height must be even".to_string(),
        };
        assert!(err.to_string().contains("1920x1081"));
        assert!(err.to_string().contains("height must be even"));
    }
}
