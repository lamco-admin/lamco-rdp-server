//! Chunked transfer engine for large clipboard data.
//!
//! Handles transferring large clipboard content (files, images) in chunks
//! with progress tracking and integrity verification.

use sha2::{Digest, Sha256};
use std::time::{Duration, Instant};

use crate::{ClipboardError, ClipboardResult};

/// Default chunk size: 64KB
pub const DEFAULT_CHUNK_SIZE: usize = 64 * 1024;

/// Default maximum data size: 16MB
pub const DEFAULT_MAX_SIZE: usize = 16 * 1024 * 1024;

/// Default timeout: 30 seconds
pub const DEFAULT_TIMEOUT_MS: u64 = 30_000;

/// State of a transfer operation
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferState {
    /// Transfer not started
    Pending,
    /// Transfer in progress
    InProgress,
    /// Transfer completed successfully
    Completed,
    /// Transfer was cancelled
    Cancelled,
    /// Transfer failed
    Failed,
}

impl TransferState {
    /// Returns true if the transfer is still active
    pub fn is_active(&self) -> bool {
        matches!(self, Self::Pending | Self::InProgress)
    }

    /// Returns true if the transfer has finished (success or failure)
    pub fn is_finished(&self) -> bool {
        matches!(self, Self::Completed | Self::Cancelled | Self::Failed)
    }
}

/// Progress information for a transfer
#[derive(Debug, Clone)]
pub struct TransferProgress {
    /// Total bytes to transfer
    pub total_bytes: u64,

    /// Bytes transferred so far
    pub transferred_bytes: u64,

    /// Current transfer state
    pub state: TransferState,

    /// Transfer start time
    pub started_at: Option<Instant>,

    /// Estimated time remaining in milliseconds
    pub eta_ms: Option<u64>,
}

impl TransferProgress {
    /// Create new progress tracker
    pub fn new(total_bytes: u64) -> Self {
        Self {
            total_bytes,
            transferred_bytes: 0,
            state: TransferState::Pending,
            started_at: None,
            eta_ms: None,
        }
    }

    /// Get completion percentage (0.0 - 100.0)
    pub fn percentage(&self) -> f64 {
        if self.total_bytes == 0 {
            return 100.0;
        }
        (self.transferred_bytes as f64 / self.total_bytes as f64) * 100.0
    }

    /// Get bytes per second transfer rate
    pub fn bytes_per_second(&self) -> Option<f64> {
        let started = self.started_at?;
        let elapsed = started.elapsed().as_secs_f64();
        if elapsed > 0.0 {
            Some(self.transferred_bytes as f64 / elapsed)
        } else {
            None
        }
    }
}

/// Configuration for the transfer engine
#[derive(Debug, Clone)]
pub struct TransferConfig {
    /// Chunk size in bytes
    pub chunk_size: usize,

    /// Maximum total size in bytes
    pub max_size: usize,

    /// Timeout in milliseconds
    pub timeout_ms: u64,

    /// Whether to verify integrity with hash
    pub verify_integrity: bool,
}

impl Default for TransferConfig {
    fn default() -> Self {
        Self {
            chunk_size: DEFAULT_CHUNK_SIZE,
            max_size: DEFAULT_MAX_SIZE,
            timeout_ms: DEFAULT_TIMEOUT_MS,
            verify_integrity: true,
        }
    }
}

/// Handles chunked transfers of clipboard data.
///
/// # Features
///
/// - Chunked transfer for large data
/// - Progress tracking with ETA
/// - Integrity verification via SHA256
/// - Timeout handling
/// - Cancellation support
///
/// # Example
///
/// ```rust
/// use lamco_clipboard_core::TransferEngine;
///
/// let mut engine = TransferEngine::new();
///
/// // Prepare data for sending
/// let data = vec![0u8; 1024 * 1024]; // 1MB
/// let chunks = engine.prepare_send(&data).unwrap();
///
/// // Send chunks (to RDP, network, etc.)
/// for (index, chunk) in chunks.iter().enumerate() {
///     println!("Sending chunk {} of {} ({} bytes)",
///              index + 1, chunks.len(), chunk.len());
/// }
///
/// // Get the hash for verification
/// let hash = engine.compute_hash(&data);
/// ```
#[derive(Debug)]
pub struct TransferEngine {
    /// Configuration
    config: TransferConfig,

    /// Current progress (for active transfer)
    progress: Option<TransferProgress>,

    /// Received chunks (for incoming transfer)
    received_chunks: Vec<Vec<u8>>,

    /// Expected hash (for verification)
    expected_hash: Option<String>,

    /// Transfer start time
    started_at: Option<Instant>,
}

impl Default for TransferEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl TransferEngine {
    /// Create a new transfer engine with default configuration
    pub fn new() -> Self {
        Self::with_config(TransferConfig::default())
    }

    /// Create a new transfer engine with custom configuration
    pub fn with_config(config: TransferConfig) -> Self {
        Self {
            config,
            progress: None,
            received_chunks: Vec::new(),
            expected_hash: None,
            started_at: None,
        }
    }

    /// Get current progress
    pub fn progress(&self) -> Option<&TransferProgress> {
        self.progress.as_ref()
    }

    /// Prepare data for chunked sending
    ///
    /// Returns a vector of chunks ready to be sent.
    pub fn prepare_send(&mut self, data: &[u8]) -> ClipboardResult<Vec<Vec<u8>>> {
        if data.len() > self.config.max_size {
            return Err(ClipboardError::DataSizeExceeded {
                actual: data.len(),
                max: self.config.max_size,
            });
        }

        self.started_at = Some(Instant::now());
        self.progress = Some(TransferProgress::new(data.len() as u64));

        if let Some(ref mut progress) = self.progress {
            progress.state = TransferState::InProgress;
            progress.started_at = Some(Instant::now());
        }

        let chunks: Vec<Vec<u8>> = data.chunks(self.config.chunk_size).map(|c| c.to_vec()).collect();

        Ok(chunks)
    }

    /// Start receiving a chunked transfer
    pub fn start_receive(&mut self, total_size: u64, expected_hash: Option<String>) -> ClipboardResult<()> {
        if total_size as usize > self.config.max_size {
            return Err(ClipboardError::DataSizeExceeded {
                actual: total_size as usize,
                max: self.config.max_size,
            });
        }

        self.received_chunks.clear();
        self.expected_hash = expected_hash;
        self.started_at = Some(Instant::now());
        self.progress = Some(TransferProgress::new(total_size));

        if let Some(ref mut progress) = self.progress {
            progress.state = TransferState::InProgress;
            progress.started_at = Some(Instant::now());
        }

        Ok(())
    }

    /// Receive a chunk of data
    pub fn receive_chunk(&mut self, chunk: Vec<u8>) -> ClipboardResult<()> {
        // Check timeout
        if let Some(started) = self.started_at {
            if started.elapsed() > Duration::from_millis(self.config.timeout_ms) {
                if let Some(ref mut progress) = self.progress {
                    progress.state = TransferState::Failed;
                }
                return Err(ClipboardError::TransferTimeout(self.config.timeout_ms));
            }
        }

        // Check if we have an active transfer
        let progress = self
            .progress
            .as_mut()
            .ok_or_else(|| ClipboardError::InvalidState("no active transfer".to_string()))?;

        // Check if transfer is still active
        if !progress.state.is_active() {
            return Err(ClipboardError::InvalidState("transfer not active".to_string()));
        }

        // Update progress
        progress.transferred_bytes += chunk.len() as u64;

        // Calculate ETA
        if let Some(started) = progress.started_at {
            let elapsed = started.elapsed().as_secs_f64();
            if elapsed > 0.0 && progress.transferred_bytes > 0 {
                let rate = progress.transferred_bytes as f64 / elapsed;
                let remaining = progress.total_bytes - progress.transferred_bytes;
                progress.eta_ms = Some((remaining as f64 / rate * 1000.0) as u64);
            }
        }

        // Store chunk
        self.received_chunks.push(chunk);

        // Check if complete
        if progress.transferred_bytes >= progress.total_bytes {
            progress.state = TransferState::Completed;
        }

        Ok(())
    }

    /// Finalize the receive and get the assembled data
    pub fn finalize_receive(&mut self) -> ClipboardResult<Vec<u8>> {
        let progress = self
            .progress
            .as_ref()
            .ok_or_else(|| ClipboardError::InvalidState("no active transfer".to_string()))?;

        if progress.state != TransferState::Completed {
            return Err(ClipboardError::InvalidState(format!(
                "transfer not completed: {:?}",
                progress.state
            )));
        }

        // Assemble data
        let mut data = Vec::with_capacity(progress.total_bytes as usize);
        for chunk in &self.received_chunks {
            data.extend_from_slice(chunk);
        }

        // Verify integrity if hash was provided
        if self.config.verify_integrity {
            if let Some(ref expected) = self.expected_hash {
                let actual = self.compute_hash(&data);
                if actual != *expected {
                    return Err(ClipboardError::FormatConversion(
                        "integrity check failed: hash mismatch".to_string(),
                    ));
                }
            }
        }

        // Clear state
        self.received_chunks.clear();
        self.progress = None;
        self.expected_hash = None;
        self.started_at = None;

        Ok(data)
    }

    /// Cancel the current transfer
    pub fn cancel(&mut self) {
        if let Some(ref mut progress) = self.progress {
            progress.state = TransferState::Cancelled;
        }
        self.received_chunks.clear();
    }

    /// Compute SHA256 hash of data
    pub fn compute_hash(&self, data: &[u8]) -> String {
        let mut hasher = Sha256::new();
        hasher.update(data);
        format!("{:x}", hasher.finalize())
    }

    /// Check if a transfer is in progress
    pub fn is_active(&self) -> bool {
        self.progress.as_ref().map(|p| p.state.is_active()).unwrap_or(false)
    }

    /// Get the configured chunk size
    pub fn chunk_size(&self) -> usize {
        self.config.chunk_size
    }

    /// Get the configured maximum size
    pub fn max_size(&self) -> usize {
        self.config.max_size
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_prepare_send() {
        let mut engine = TransferEngine::new();
        let data = vec![0u8; 100_000]; // 100KB

        let chunks = engine.prepare_send(&data).unwrap();

        // Should be ~2 chunks at 64KB each
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].len(), 64 * 1024);
        assert_eq!(chunks[1].len(), 100_000 - 64 * 1024);
    }

    #[test]
    fn test_receive_transfer() {
        let mut engine = TransferEngine::new();

        // Start receiving 1000 bytes
        engine.start_receive(1000, None).unwrap();

        // Receive in chunks
        engine.receive_chunk(vec![0u8; 500]).unwrap();
        engine.receive_chunk(vec![0u8; 500]).unwrap();

        // Check progress
        let progress = engine.progress().unwrap();
        assert_eq!(progress.state, TransferState::Completed);
        assert_eq!(progress.transferred_bytes, 1000);

        // Finalize
        let data = engine.finalize_receive().unwrap();
        assert_eq!(data.len(), 1000);
    }

    #[test]
    fn test_integrity_verification() {
        let mut engine = TransferEngine::new();
        let original_data = b"Hello, World!".to_vec();
        let hash = engine.compute_hash(&original_data);

        // Start receive with hash
        engine
            .start_receive(original_data.len() as u64, Some(hash.clone()))
            .unwrap();
        engine.receive_chunk(original_data.clone()).unwrap();

        // Should succeed with correct hash
        let data = engine.finalize_receive().unwrap();
        assert_eq!(data, original_data);
    }

    #[test]
    fn test_integrity_failure() {
        let mut engine = TransferEngine::new();
        let wrong_hash = "0000000000000000000000000000000000000000000000000000000000000000".to_string();

        engine.start_receive(13, Some(wrong_hash)).unwrap();
        engine.receive_chunk(b"Hello, World!".to_vec()).unwrap();

        // Should fail with wrong hash
        let result = engine.finalize_receive();
        assert!(result.is_err());
    }

    #[test]
    fn test_cancel() {
        let mut engine = TransferEngine::new();

        engine.start_receive(1000, None).unwrap();
        engine.receive_chunk(vec![0u8; 500]).unwrap();

        engine.cancel();

        let progress = engine.progress().unwrap();
        assert_eq!(progress.state, TransferState::Cancelled);
    }

    #[test]
    fn test_progress_percentage() {
        let mut progress = TransferProgress::new(100);
        progress.transferred_bytes = 50;
        assert!((progress.percentage() - 50.0).abs() < 0.01);
    }

    #[test]
    fn test_data_size_exceeded() {
        let config = TransferConfig {
            max_size: 100,
            ..Default::default()
        };
        let mut engine = TransferEngine::with_config(config);

        let result = engine.prepare_send(&vec![0u8; 200]);
        assert!(matches!(result, Err(ClipboardError::DataSizeExceeded { .. })));
    }
}
