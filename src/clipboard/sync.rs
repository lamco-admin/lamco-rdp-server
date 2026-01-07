//! Clipboard Synchronization and Loop Prevention
//!
//! Manages bidirectional clipboard synchronization between RDP and Wayland,
//! with state tracking and echo protection.
//!
//! # Architecture
//!
//! The sync module provides server-specific orchestration on top of library primitives:
//!
//! - [`SyncManager`] - State machine tracking clipboard ownership and echo protection
//! - [`ClipboardState`] - Current clipboard ownership (Idle, RdpOwned, PortalOwned, Syncing)
//! - [`LoopDetector`] - From lamco-clipboard-core, provides hash-based loop detection
//!
//! The `SyncManager` adds:
//! - **Echo protection**: Time-based filtering to prevent D-Bus echoes
//! - **Ownership tracking**: State machine to know who "owns" the clipboard
//! - **Policy decisions**: When to allow/block sync based on state

use crate::clipboard::error::Result;
use std::time::{Duration, SystemTime};
use tracing::{debug, warn};

// Import loop detection from library
pub use lamco_clipboard_core::loop_detector::ClipboardSource;
pub use lamco_clipboard_core::{ClipboardFormat, LoopDetectionConfig, LoopDetector};

/// Clipboard ownership state
#[derive(Debug, Clone)]
pub enum ClipboardState {
    /// No active clipboard data
    Idle,
    /// RDP client owns the clipboard (with timestamp when ownership started)
    RdpOwned(Vec<ClipboardFormat>, SystemTime),
    /// Wayland/Portal owns the clipboard
    PortalOwned(Vec<String>),
    /// Currently syncing data
    Syncing(SyncDirection),
}

impl PartialEq for ClipboardState {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (ClipboardState::Idle, ClipboardState::Idle) => true,
            (ClipboardState::RdpOwned(f1, _), ClipboardState::RdpOwned(f2, _)) => {
                // Compare format IDs only (ignore timestamps)
                f1.len() == f2.len() && f1.iter().zip(f2.iter()).all(|(a, b)| a.id == b.id)
            }
            (ClipboardState::PortalOwned(m1), ClipboardState::PortalOwned(m2)) => m1 == m2,
            (ClipboardState::Syncing(d1), ClipboardState::Syncing(d2)) => d1 == d2,
            _ => false,
        }
    }
}

impl Eq for ClipboardState {}

/// Synchronization direction
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncDirection {
    /// RDP → Portal
    RdpToPortal,
    /// Portal → RDP
    PortalToRdp,
}

/// Echo protection window in milliseconds
///
/// D-Bus signals within this window after RDP ownership are likely echoes
/// from our Portal writes, not real user copies.
const ECHO_PROTECTION_WINDOW_MS: u128 = 2000;

/// Synchronization manager coordinates clipboard sync
///
/// Provides server-specific orchestration by combining:
/// - State machine tracking (who owns the clipboard)
/// - Echo protection (time-based D-Bus signal filtering)
/// - Library-based loop detection (hash comparison)
#[derive(Debug)]
pub struct SyncManager {
    /// Current clipboard state
    state: ClipboardState,
    /// Loop detector (from lamco-clipboard-core)
    loop_detector: LoopDetector,
}

impl SyncManager {
    /// Create a new synchronization manager with default loop detector config
    pub fn new() -> Self {
        Self {
            state: ClipboardState::Idle,
            loop_detector: LoopDetector::new(),
        }
    }

    /// Create a new synchronization manager with custom loop detector config
    pub fn with_config(config: LoopDetectionConfig) -> Self {
        Self {
            state: ClipboardState::Idle,
            loop_detector: LoopDetector::with_config(config),
        }
    }

    /// Get current state
    pub fn state(&self) -> &ClipboardState {
        &self.state
    }

    /// Handle RDP format list announcement
    ///
    /// Called when the RDP client announces available clipboard formats.
    /// Checks for loops and updates state if allowed.
    ///
    /// # Returns
    ///
    /// - `Ok(true)` - Proceed with sync
    /// - `Ok(false)` - Skip sync (loop detected)
    pub fn handle_rdp_formats(&mut self, formats: Vec<ClipboardFormat>) -> Result<bool> {
        // Check for loop
        if self.loop_detector.would_cause_loop(&formats) {
            warn!("Ignoring RDP format list due to loop detection");
            return Ok(false); // Don't sync
        }

        // Update state with current timestamp
        self.state = ClipboardState::RdpOwned(formats.clone(), SystemTime::now());

        // Record operation in loop detector
        self.loop_detector
            .record_formats(&formats, ClipboardSource::Rdp);
        self.loop_detector.record_sync(ClipboardSource::Rdp);

        Ok(true) // Proceed with sync
    }

    /// Handle Portal MIME types announcement
    ///
    /// The `force` parameter indicates if this is from an authoritative source (D-Bus extension)
    /// that should override RDP ownership. When force=false, we block if RDP owns the clipboard
    /// to prevent echo loops. When force=true (D-Bus), we check if enough time has passed
    /// since RDP took ownership to distinguish between echoes and real user copies.
    ///
    /// # Arguments
    ///
    /// * `mime_types` - MIME types available in Portal clipboard
    /// * `force` - If true, this is from D-Bus extension (authoritative source)
    ///
    /// # Returns
    ///
    /// - `Ok(true)` - Proceed with sync
    /// - `Ok(false)` - Skip sync (echo or loop detected)
    pub fn handle_portal_formats(&mut self, mime_types: Vec<String>, force: bool) -> Result<bool> {
        // If RDP currently owns the clipboard, apply echo protection
        if let ClipboardState::RdpOwned(_, ownership_time) = &self.state {
            let elapsed = SystemTime::now()
                .duration_since(*ownership_time)
                .unwrap_or(Duration::from_secs(0));

            if !force {
                // Portal SelectionOwnerChanged (force=false) - always block when RDP owns
                debug!("Ignoring Portal format list - RDP currently owns clipboard (preventing echo loop)");
                return Ok(false);
            } else if elapsed.as_millis() < ECHO_PROTECTION_WINDOW_MS {
                // D-Bus signal (force=true) but too soon after RDP ownership - this is an echo!
                debug!(
                    "Ignoring D-Bus signal - received {}ms after RDP ownership (echo protection)",
                    elapsed.as_millis()
                );
                return Ok(false);
            } else {
                // D-Bus signal after protection window - likely a real user copy
                debug!(
                    "D-Bus signal {}ms after RDP ownership - allowing override (user likely copied)",
                    elapsed.as_millis()
                );
            }
        }

        // Check for loop using hash comparison - but SKIP for authoritative D-Bus signals
        // that passed the timing check above
        if !force && self.loop_detector.would_cause_loop_mime(&mime_types) {
            warn!("Ignoring Portal format list due to loop detection");
            return Ok(false); // Don't sync
        }

        // Update state - Linux now owns clipboard
        self.state = ClipboardState::PortalOwned(mime_types.clone());
        if force {
            debug!("D-Bus extension signal - taking clipboard ownership from RDP");
        }

        // Record operation in loop detector
        self.loop_detector
            .record_mime_types(&mime_types, ClipboardSource::Local);
        self.loop_detector.record_sync(ClipboardSource::Local);

        Ok(true) // Proceed with sync
    }

    /// Check if content would cause loop
    ///
    /// # Arguments
    ///
    /// * `content` - Clipboard content data
    /// * `from_rdp` - True if content is from RDP, false if from Portal
    ///
    /// # Returns
    ///
    /// - `Ok(true)` - Proceed with transfer
    /// - `Ok(false)` - Skip transfer (content loop detected)
    pub fn check_content(&mut self, content: &[u8], from_rdp: bool) -> Result<bool> {
        let source = if from_rdp {
            ClipboardSource::Rdp
        } else {
            ClipboardSource::Local
        };

        if self.loop_detector.would_cause_content_loop(content, source) {
            warn!("Content loop detected, skipping transfer");
            return Ok(false); // Don't transfer
        }

        // Record content for future loop detection
        self.loop_detector.record_content(content, source);

        Ok(true) // Proceed with transfer
    }

    /// Set syncing state
    pub fn set_syncing(&mut self, direction: SyncDirection) {
        self.state = ClipboardState::Syncing(direction);
    }

    /// Reset to idle state
    pub fn reset(&mut self) {
        self.state = ClipboardState::Idle;
    }

    /// Reset loop detector history
    pub fn reset_loop_detector(&mut self) {
        self.loop_detector.clear();
    }

    /// Check if RDP format list would cause a loop (without state change)
    ///
    /// Use this for read-only loop checking before committing to state changes.
    pub fn would_cause_loop_rdp(&self, formats: &[ClipboardFormat]) -> bool {
        self.loop_detector.would_cause_loop(formats)
    }

    /// Check if Portal MIME types would cause a loop (without state change)
    ///
    /// Use this for read-only loop checking before committing to state changes.
    pub fn would_cause_loop_portal(&self, mime_types: &[String]) -> bool {
        self.loop_detector.would_cause_loop_mime(mime_types)
    }

    /// Set RDP formats as current clipboard owner
    ///
    /// # Arguments
    ///
    /// * `formats` - RDP clipboard formats
    pub fn set_rdp_formats(&mut self, formats: Vec<ClipboardFormat>) {
        self.state = ClipboardState::RdpOwned(formats.clone(), SystemTime::now());
        self.loop_detector
            .record_formats(&formats, ClipboardSource::Rdp);
    }

    /// Set Portal MIME types as current clipboard owner
    ///
    /// # Arguments
    ///
    /// * `mime_types` - Portal clipboard MIME types
    pub fn set_portal_formats(&mut self, mime_types: Vec<String>) {
        self.state = ClipboardState::PortalOwned(mime_types.clone());
        self.loop_detector
            .record_mime_types(&mime_types, ClipboardSource::Local);
    }

    /// Check if currently rate limited for the given source
    ///
    /// Only returns true if rate limiting is configured.
    pub fn is_rate_limited(&self, from_rdp: bool) -> bool {
        let source = if from_rdp {
            ClipboardSource::Rdp
        } else {
            ClipboardSource::Local
        };
        self.loop_detector.is_rate_limited(source)
    }
}

impl Default for SyncManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_text_formats() -> Vec<ClipboardFormat> {
        vec![ClipboardFormat::unicode_text()]
    }

    fn make_image_formats() -> Vec<ClipboardFormat> {
        vec![ClipboardFormat {
            id: 2,
            name: Some("CF_BITMAP".to_string()),
        }]
    }

    #[test]
    fn test_sync_manager_rdp_formats() {
        let mut manager = SyncManager::new();
        let formats = make_text_formats();

        // Should allow first format announcement
        assert!(manager.handle_rdp_formats(formats.clone()).unwrap());

        // Verify state
        match manager.state() {
            ClipboardState::RdpOwned(f, _timestamp) => {
                assert_eq!(f.len(), formats.len());
                assert_eq!(f[0].id, formats[0].id);
            }
            _ => panic!("Expected RdpOwned state"),
        }
    }

    #[test]
    fn test_sync_manager_portal_formats() {
        let mut manager = SyncManager::new();
        let mime_types = vec!["text/plain".to_string(), "text/html".to_string()];

        // Should allow first format announcement (force=true simulates D-Bus)
        assert!(manager
            .handle_portal_formats(mime_types.clone(), true)
            .unwrap());

        // Verify state
        match manager.state() {
            ClipboardState::PortalOwned(m) => assert_eq!(m, &mime_types),
            _ => panic!("Expected PortalOwned state"),
        }
    }

    #[test]
    fn test_sync_manager_echo_protection() {
        let mut manager = SyncManager::new();

        // RDP takes ownership
        let formats = make_text_formats();
        assert!(manager.handle_rdp_formats(formats).unwrap());

        // Immediate D-Bus signal should be blocked (echo protection)
        let mime_types = vec!["text/plain".to_string()];
        assert!(!manager
            .handle_portal_formats(mime_types.clone(), true)
            .unwrap());

        // Non-force Portal signal should always be blocked when RDP owns
        assert!(!manager.handle_portal_formats(mime_types, false).unwrap());
    }

    #[test]
    fn test_sync_manager_loop_prevention() {
        let config = LoopDetectionConfig {
            window_ms: 1000,
            ..Default::default()
        };
        let mut manager = SyncManager::with_config(config);

        // RDP announces text formats
        let text_formats = make_text_formats();
        assert!(manager.handle_rdp_formats(text_formats.clone()).unwrap());

        // Simulate Portal echo - record it as coming from Local
        let text_mime = vec!["text/plain".to_string()];
        manager
            .loop_detector
            .record_mime_types(&text_mime, ClipboardSource::Local);

        // Now RDP trying to announce same formats again should be blocked (loop)
        assert!(!manager.handle_rdp_formats(text_formats).unwrap());
    }

    #[test]
    fn test_sync_manager_content_check() {
        let mut manager = SyncManager::new();
        let content = b"Test content";

        // First check from RDP should pass
        assert!(manager.check_content(content, true).unwrap());

        // Same content from Portal should fail (loop)
        assert!(!manager.check_content(content, false).unwrap());
    }

    #[test]
    fn test_sync_manager_reset() {
        let mut manager = SyncManager::new();
        let formats = make_text_formats();

        manager.handle_rdp_formats(formats).unwrap();

        // Reset state
        manager.reset();

        // Verify state is idle
        assert_eq!(manager.state(), &ClipboardState::Idle);
    }

    #[test]
    fn test_sync_manager_different_formats_allowed() {
        let mut manager = SyncManager::new();

        // RDP announces text
        let text_formats = make_text_formats();
        assert!(manager.handle_rdp_formats(text_formats).unwrap());

        // RDP announces different format (image) - should succeed
        let image_formats = make_image_formats();
        assert!(manager.handle_rdp_formats(image_formats).unwrap());
    }

    #[test]
    fn test_clipboard_state_equality() {
        let formats = make_text_formats();

        let state1 = ClipboardState::RdpOwned(formats.clone(), SystemTime::now());
        std::thread::sleep(std::time::Duration::from_millis(1));
        let state2 = ClipboardState::RdpOwned(formats, SystemTime::now());

        // Should be equal (timestamps ignored)
        assert_eq!(state1, state2);

        assert_eq!(ClipboardState::Idle, ClipboardState::Idle);
        assert_ne!(ClipboardState::Idle, state1);
    }
}
