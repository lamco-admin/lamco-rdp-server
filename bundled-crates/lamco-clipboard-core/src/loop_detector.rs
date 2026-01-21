//! Loop detection for clipboard synchronization.
//!
//! Prevents clipboard sync loops by tracking format and content hashes.

use sha2::{Digest, Sha256};
use std::collections::VecDeque;
use std::time::{Duration, Instant};

use crate::ClipboardFormat;

/// Configuration for loop detection
#[derive(Debug, Clone)]
pub struct LoopDetectionConfig {
    /// Time window for detecting loops (default: 500ms)
    pub window_ms: u64,

    /// Maximum number of operations to track
    pub max_history: usize,

    /// Enable content hashing for deduplication
    pub enable_content_hashing: bool,

    /// Optional rate limit in milliseconds (default: None)
    ///
    /// When set, sync operations are throttled to at most one per `rate_limit_ms`.
    /// This provides belt-and-suspenders protection against rapid clipboard updates
    /// even when loop detection passes.
    pub rate_limit_ms: Option<u64>,
}

impl Default for LoopDetectionConfig {
    fn default() -> Self {
        Self {
            window_ms: 500,
            max_history: 10,
            enable_content_hashing: true,
            rate_limit_ms: None,
        }
    }
}

impl LoopDetectionConfig {
    /// Create config with rate limiting enabled
    ///
    /// # Example
    ///
    /// ```rust
    /// use lamco_clipboard_core::LoopDetectionConfig;
    ///
    /// let config = LoopDetectionConfig::with_rate_limit(200);
    /// assert_eq!(config.rate_limit_ms, Some(200));
    /// ```
    pub fn with_rate_limit(rate_limit_ms: u64) -> Self {
        Self {
            rate_limit_ms: Some(rate_limit_ms),
            ..Default::default()
        }
    }
}

/// Source of a clipboard operation
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClipboardSource {
    /// Operation from RDP client
    Rdp,
    /// Operation from local clipboard (Portal, X11, etc.)
    Local,
}

impl ClipboardSource {
    /// Get the opposite source
    pub fn opposite(self) -> Self {
        match self {
            Self::Rdp => Self::Local,
            Self::Local => Self::Rdp,
        }
    }
}

/// A recorded clipboard operation for loop detection
#[derive(Debug, Clone)]
struct ClipboardOperation {
    /// Hash of the operation (formats or content)
    hash: String,
    /// Source of the operation
    source: ClipboardSource,
    /// When the operation occurred
    timestamp: Instant,
}

/// Detects and prevents clipboard synchronization loops.
///
/// # How It Works
///
/// When clipboard content is copied, the same content often triggers events
/// on both ends (RDP and local). Without loop detection, this causes:
///
/// 1. User copies on Windows → RDP sends to Linux
/// 2. Linux clipboard updates → Signal sent to sync back
/// 3. Windows clipboard updates → RDP sends to Linux again
/// 4. ... infinite loop
///
/// The `LoopDetector` prevents this by:
///
/// 1. **Format hashing**: Hashes the list of formats/MIME types
/// 2. **Content hashing**: Hashes actual clipboard content (optional)
/// 3. **Time windowing**: Only detects loops within a configurable time window
/// 4. **Source tracking**: Distinguishes RDP vs local operations
/// 5. **Rate limiting**: Optional throttle to prevent rapid sync storms
///
/// # Example
///
/// ```rust
/// use lamco_clipboard_core::{LoopDetector, ClipboardFormat};
/// use lamco_clipboard_core::loop_detector::ClipboardSource;
///
/// let mut detector = LoopDetector::new();
///
/// // Record an RDP operation
/// let formats = vec![ClipboardFormat::unicode_text()];
/// detector.record_formats(&formats, ClipboardSource::Rdp);
///
/// // Check if a local operation would cause a loop
/// if detector.would_cause_loop(&formats) {
///     println!("Loop detected, skipping sync");
/// }
/// ```
#[derive(Debug)]
pub struct LoopDetector {
    /// Configuration
    config: LoopDetectionConfig,

    /// Recent format operations
    format_history: VecDeque<ClipboardOperation>,

    /// Recent content hashes
    content_history: VecDeque<ClipboardOperation>,

    /// Last sync time for rate limiting (per source)
    last_sync_rdp: Option<Instant>,
    last_sync_local: Option<Instant>,
}

impl Default for LoopDetector {
    fn default() -> Self {
        Self::new()
    }
}

impl LoopDetector {
    /// Create a new loop detector with default configuration
    pub fn new() -> Self {
        Self::with_config(LoopDetectionConfig::default())
    }

    /// Create a new loop detector with custom configuration
    pub fn with_config(config: LoopDetectionConfig) -> Self {
        Self {
            config,
            format_history: VecDeque::new(),
            content_history: VecDeque::new(),
            last_sync_rdp: None,
            last_sync_local: None,
        }
    }

    /// Record a format list operation
    pub fn record_formats(&mut self, formats: &[ClipboardFormat], source: ClipboardSource) {
        let hash = Self::hash_formats(formats);
        self.record_operation(&mut self.format_history.clone(), hash, source);
        // Need to work around borrow checker
        let hash = Self::hash_formats(formats);
        self.format_history.push_back(ClipboardOperation {
            hash,
            source,
            timestamp: Instant::now(),
        });
        self.cleanup_history();
    }

    /// Record a MIME type list operation
    pub fn record_mime_types(&mut self, mime_types: &[String], source: ClipboardSource) {
        let hash = Self::hash_mime_types(mime_types);
        self.format_history.push_back(ClipboardOperation {
            hash,
            source,
            timestamp: Instant::now(),
        });
        self.cleanup_history();
    }

    /// Record content data for deduplication
    pub fn record_content(&mut self, data: &[u8], source: ClipboardSource) {
        if !self.config.enable_content_hashing {
            return;
        }

        let hash = Self::hash_content(data);
        self.content_history.push_back(ClipboardOperation {
            hash,
            source,
            timestamp: Instant::now(),
        });
        self.cleanup_history();
    }

    /// Check if syncing these formats would cause a loop
    ///
    /// Returns true if a recent operation from the opposite source
    /// had the same format hash.
    pub fn would_cause_loop(&self, formats: &[ClipboardFormat]) -> bool {
        let hash = Self::hash_formats(formats);
        self.check_hash_collision(&self.format_history, &hash, ClipboardSource::Local)
    }

    /// Check if syncing these MIME types would cause a loop
    pub fn would_cause_loop_mime(&self, mime_types: &[String]) -> bool {
        let hash = Self::hash_mime_types(mime_types);
        self.check_hash_collision(&self.format_history, &hash, ClipboardSource::Rdp)
    }

    /// Check if this content would cause a loop
    pub fn would_cause_content_loop(&self, data: &[u8], source: ClipboardSource) -> bool {
        if !self.config.enable_content_hashing {
            return false;
        }

        let hash = Self::hash_content(data);
        self.check_hash_collision(&self.content_history, &hash, source)
    }

    /// Compute hash for deduplication of arbitrary data
    pub fn compute_hash(data: &[u8]) -> String {
        Self::hash_content(data)
    }

    /// Clear all history
    pub fn clear(&mut self) {
        self.format_history.clear();
        self.content_history.clear();
        self.last_sync_rdp = None;
        self.last_sync_local = None;
    }

    /// Check if sync is rate limited for the given source
    ///
    /// Returns true if a sync was performed too recently and should be skipped.
    /// This is only active when `rate_limit_ms` is configured.
    ///
    /// # Example
    ///
    /// ```rust
    /// use lamco_clipboard_core::{LoopDetector, LoopDetectionConfig};
    /// use lamco_clipboard_core::loop_detector::ClipboardSource;
    ///
    /// let config = LoopDetectionConfig::with_rate_limit(200);
    /// let mut detector = LoopDetector::with_config(config);
    ///
    /// // First sync is not rate limited
    /// assert!(!detector.is_rate_limited(ClipboardSource::Rdp));
    ///
    /// // Record that we synced
    /// detector.record_sync(ClipboardSource::Rdp);
    ///
    /// // Immediate second sync would be rate limited
    /// assert!(detector.is_rate_limited(ClipboardSource::Rdp));
    /// ```
    pub fn is_rate_limited(&self, source: ClipboardSource) -> bool {
        let Some(rate_limit_ms) = self.config.rate_limit_ms else {
            return false;
        };

        let last_sync = match source {
            ClipboardSource::Rdp => self.last_sync_rdp,
            ClipboardSource::Local => self.last_sync_local,
        };

        let Some(last) = last_sync else {
            return false;
        };

        let elapsed = last.elapsed();
        elapsed < Duration::from_millis(rate_limit_ms)
    }

    /// Record that a sync operation was performed
    ///
    /// Call this after successfully syncing clipboard data to update
    /// the rate limiting timestamp.
    pub fn record_sync(&mut self, source: ClipboardSource) {
        let now = Instant::now();
        match source {
            ClipboardSource::Rdp => self.last_sync_rdp = Some(now),
            ClipboardSource::Local => self.last_sync_local = Some(now),
        }
    }

    /// Combined check: would cause loop OR is rate limited
    ///
    /// Convenience method that checks both conditions. Returns true if
    /// the sync should be skipped for any reason.
    pub fn should_skip_sync(&self, formats: &[ClipboardFormat], source: ClipboardSource) -> bool {
        if self.is_rate_limited(source) {
            tracing::debug!("Sync skipped: rate limited for {:?}", source);
            return true;
        }

        let would_loop = match source {
            ClipboardSource::Rdp => self.would_cause_loop(formats),
            ClipboardSource::Local => self.would_cause_loop(formats),
        };

        if would_loop {
            tracing::debug!("Sync skipped: would cause loop");
        }

        would_loop
    }

    /// Combined check for MIME types: would cause loop OR is rate limited
    pub fn should_skip_sync_mime(&self, mime_types: &[String], source: ClipboardSource) -> bool {
        if self.is_rate_limited(source) {
            tracing::debug!("Sync skipped: rate limited for {:?}", source);
            return true;
        }

        let would_loop = self.would_cause_loop_mime(mime_types);

        if would_loop {
            tracing::debug!("Sync skipped: would cause loop");
        }

        would_loop
    }

    // =========================================================================
    // Private Methods
    // =========================================================================

    fn check_hash_collision(
        &self,
        history: &VecDeque<ClipboardOperation>,
        hash: &str,
        current_source: ClipboardSource,
    ) -> bool {
        let window = Duration::from_millis(self.config.window_ms);
        let now = Instant::now();

        for op in history.iter().rev() {
            // Only check recent operations
            if now.duration_since(op.timestamp) > window {
                break;
            }

            // Only detect loops from the opposite source
            if op.source == current_source.opposite() && op.hash == hash {
                return true;
            }
        }

        false
    }

    fn record_operation(&mut self, history: &mut VecDeque<ClipboardOperation>, hash: String, source: ClipboardSource) {
        history.push_back(ClipboardOperation {
            hash,
            source,
            timestamp: Instant::now(),
        });
    }

    fn cleanup_history(&mut self) {
        let window = Duration::from_millis(self.config.window_ms * 2);
        let now = Instant::now();

        // Remove old entries
        while let Some(front) = self.format_history.front() {
            if now.duration_since(front.timestamp) > window {
                self.format_history.pop_front();
            } else {
                break;
            }
        }

        while let Some(front) = self.content_history.front() {
            if now.duration_since(front.timestamp) > window {
                self.content_history.pop_front();
            } else {
                break;
            }
        }

        // Enforce max history size
        while self.format_history.len() > self.config.max_history {
            self.format_history.pop_front();
        }

        while self.content_history.len() > self.config.max_history {
            self.content_history.pop_front();
        }
    }

    fn hash_formats(formats: &[ClipboardFormat]) -> String {
        let mut hasher = Sha256::new();
        for format in formats {
            hasher.update(format.id.to_le_bytes());
            if let Some(name) = &format.name {
                hasher.update(name.as_bytes());
            }
        }
        format!("{:x}", hasher.finalize())
    }

    fn hash_mime_types(mime_types: &[String]) -> String {
        let mut hasher = Sha256::new();
        for mime in mime_types {
            hasher.update(mime.as_bytes());
            hasher.update(b"\0");
        }
        format!("{:x}", hasher.finalize())
    }

    fn hash_content(data: &[u8]) -> String {
        let mut hasher = Sha256::new();
        hasher.update(data);
        format!("{:x}", hasher.finalize())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_no_loop_different_formats() {
        let mut detector = LoopDetector::new();

        let formats1 = vec![ClipboardFormat::unicode_text()];
        let formats2 = vec![ClipboardFormat::html()];

        detector.record_formats(&formats1, ClipboardSource::Rdp);
        assert!(!detector.would_cause_loop(&formats2));
    }

    #[test]
    fn test_loop_same_formats() {
        let mut detector = LoopDetector::new();

        let formats = vec![ClipboardFormat::unicode_text()];

        detector.record_formats(&formats, ClipboardSource::Rdp);
        assert!(detector.would_cause_loop(&formats));
    }

    #[test]
    fn test_no_loop_same_source() {
        let mut detector = LoopDetector::new();

        let formats = vec![ClipboardFormat::unicode_text()];

        // Record from Local
        detector.record_formats(&formats, ClipboardSource::Local);

        // Check would_cause_loop checks against RDP source, so same formats from Local
        // shouldn't trigger (opposite source check)
        // Actually would_cause_loop always checks against Local source
        // So this should NOT trigger because we recorded from Local, checking Local
        // Hmm, the check is: op.source == current_source.opposite()
        // would_cause_loop uses ClipboardSource::Local as current_source
        // So it checks if op.source == Local.opposite() == Rdp
        // We recorded from Local, so op.source == Local != Rdp
        // So this should NOT detect a loop - correct!
        assert!(!detector.would_cause_loop(&formats));
    }

    #[test]
    fn test_content_hash() {
        let mut detector = LoopDetector::new();

        let data = b"Hello, World!";
        detector.record_content(data, ClipboardSource::Rdp);

        assert!(detector.would_cause_content_loop(data, ClipboardSource::Local));
        assert!(!detector.would_cause_content_loop(b"Different", ClipboardSource::Local));
    }

    #[test]
    fn test_clear_history() {
        let mut detector = LoopDetector::new();

        let formats = vec![ClipboardFormat::unicode_text()];
        detector.record_formats(&formats, ClipboardSource::Rdp);

        detector.clear();

        assert!(!detector.would_cause_loop(&formats));
    }

    #[test]
    fn test_compute_hash() {
        let hash1 = LoopDetector::compute_hash(b"test");
        let hash2 = LoopDetector::compute_hash(b"test");
        let hash3 = LoopDetector::compute_hash(b"different");

        assert_eq!(hash1, hash2);
        assert_ne!(hash1, hash3);
    }

    #[test]
    fn test_rate_limit_disabled_by_default() {
        let detector = LoopDetector::new();

        // Without rate limiting, should never be rate limited
        assert!(!detector.is_rate_limited(ClipboardSource::Rdp));
        assert!(!detector.is_rate_limited(ClipboardSource::Local));
    }

    #[test]
    fn test_rate_limit_config() {
        let config = LoopDetectionConfig::with_rate_limit(200);
        assert_eq!(config.rate_limit_ms, Some(200));

        let mut detector = LoopDetector::with_config(config);

        // First check - not rate limited
        assert!(!detector.is_rate_limited(ClipboardSource::Rdp));

        // Record sync
        detector.record_sync(ClipboardSource::Rdp);

        // Immediately after - should be rate limited
        assert!(detector.is_rate_limited(ClipboardSource::Rdp));

        // But Local should not be affected
        assert!(!detector.is_rate_limited(ClipboardSource::Local));
    }

    #[test]
    fn test_rate_limit_clear() {
        let config = LoopDetectionConfig::with_rate_limit(200);
        let mut detector = LoopDetector::with_config(config);

        detector.record_sync(ClipboardSource::Rdp);
        assert!(detector.is_rate_limited(ClipboardSource::Rdp));

        detector.clear();
        assert!(!detector.is_rate_limited(ClipboardSource::Rdp));
    }

    #[test]
    fn test_should_skip_sync_combined() {
        let config = LoopDetectionConfig::with_rate_limit(200);
        let mut detector = LoopDetector::with_config(config);

        let formats = vec![ClipboardFormat::unicode_text()];

        // Initially: not rate limited, no loop
        assert!(!detector.should_skip_sync(&formats, ClipboardSource::Rdp));

        // Record from RDP
        detector.record_formats(&formats, ClipboardSource::Rdp);
        detector.record_sync(ClipboardSource::Rdp);

        // Now should skip for Local (loop detection)
        assert!(detector.should_skip_sync(&formats, ClipboardSource::Local));

        // And skip for RDP (rate limiting)
        assert!(detector.should_skip_sync(&formats, ClipboardSource::Rdp));
    }
}
