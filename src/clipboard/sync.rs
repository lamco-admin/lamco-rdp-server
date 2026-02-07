//! Clipboard Synchronization and Loop Prevention
//!
//! **Execution Path:** N/A (state machine logic)
//! **Status:** Active (v1.0.0+)
//! **Platform:** Universal
//! **Role:** State tracking and echo protection for ClipboardOrchestrator
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

/// Result from handle_portal_formats indicating what action to take
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortalSyncDecision {
    /// Allow sync to RDP (normal Linux → Windows clipboard)
    Allow,
    /// Block sync (echo or loop detected)
    Block,
    /// Klipper took over clipboard - re-announce RDP formats to reclaim
    KlipperReannounce,
}

/// Echo protection window in milliseconds
///
/// D-Bus signals within this window after RDP ownership are likely echoes
/// from our Portal writes, not real user copies.
const ECHO_PROTECTION_WINDOW_MS: u128 = 2000;

/// KDE klipper signature MIME types
/// These indicate klipper is involved in the clipboard operation
const KLIPPER_SIGNATURES: &[&str] = &[
    "application/x-kde-onlyReplaceEmpty",
    "application/x-kde-cutselection",
];

/// Check if MIME types contain klipper signature
fn has_klipper_signature(mime_types: &[String]) -> bool {
    mime_types
        .iter()
        .any(|m| KLIPPER_SIGNATURES.iter().any(|sig| m.contains(sig)))
}

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

    pub fn state(&self) -> &ClipboardState {
        &self.state
    }

    /// Returns true to proceed with sync, false if loop detected.
    pub fn handle_rdp_formats(&mut self, formats: Vec<ClipboardFormat>) -> bool {
        let state_name = match &self.state {
            ClipboardState::Idle => "Idle",
            ClipboardState::RdpOwned(_, _) => "RdpOwned",
            ClipboardState::PortalOwned(_) => "PortalOwned",
            ClipboardState::Syncing(_) => "Syncing",
        };

        let format_names: Vec<_> = formats
            .iter()
            .map(|f| f.name.as_deref().unwrap_or("?"))
            .collect();

        debug!("┌─ handle_rdp_formats ─────────────────────────────────────────");
        debug!(
            "│ Current state: {}, formats: {} ({:?})",
            state_name,
            formats.len(),
            format_names
        );

        if self.loop_detector.would_cause_loop(&formats) {
            debug!("│ DECISION: Block (loop detection - format hash match)");
            debug!("└────────────────────────────────────────────────────────────────");
            warn!("Ignoring RDP format list due to loop detection");
            return false;
        }

        let now = SystemTime::now();
        self.state = ClipboardState::RdpOwned(formats.clone(), now);
        debug!(
            "│ State transition: {} → RdpOwned (echo protection starts now)",
            state_name
        );
        debug!("│ DECISION: Allow sync to Portal/Linux");
        debug!("└────────────────────────────────────────────────────────────────");

        self.loop_detector
            .record_formats(&formats, ClipboardSource::Rdp);
        self.loop_detector.record_sync(ClipboardSource::Rdp);

        true
    }

    /// Handle Portal MIME types announcement
    ///
    /// `force=true` from D-Bus extension overrides RDP ownership.
    /// `force=false` is blocked when RDP owns clipboard (echo prevention).
    /// Within the echo protection window, Klipper takeover triggers KlipperReannounce.
    pub fn handle_portal_formats(
        &mut self,
        mime_types: Vec<String>,
        force: bool,
    ) -> PortalSyncDecision {
        let is_klipper = has_klipper_signature(&mime_types);
        let state_name = match &self.state {
            ClipboardState::Idle => "Idle",
            ClipboardState::RdpOwned(_, _) => "RdpOwned",
            ClipboardState::PortalOwned(_) => "PortalOwned",
            ClipboardState::Syncing(_) => "Syncing",
        };

        debug!("┌─ handle_portal_formats ───────────────────────────────────────");
        debug!(
            "│ Current state: {}, force={}, klipper_signature={}",
            state_name, force, is_klipper
        );
        debug!("│ MIME types ({}): {:?}", mime_types.len(), mime_types);

        if let ClipboardState::RdpOwned(formats, ownership_time) = &self.state {
            let elapsed = SystemTime::now()
                .duration_since(*ownership_time)
                .unwrap_or(Duration::from_secs(0));

            debug!(
                "│ RDP ownership: {} formats, {}ms ago",
                formats.len(),
                elapsed.as_millis()
            );

            if !force {
                // Portal SelectionOwnerChanged (force=false) - always block when RDP owns
                debug!("│ DECISION: Block (force=false while RDP owns)");
                debug!("└────────────────────────────────────────────────────────────────");
                return PortalSyncDecision::Block;
            } else if elapsed.as_millis() < ECHO_PROTECTION_WINDOW_MS {
                // Within echo protection window
                if is_klipper {
                    // Klipper takeover detected - signal caller to re-announce RDP formats
                    warn!(
                        "│ KLIPPER TAKEOVER DETECTED: {}ms after RDP ownership (within {}ms window)",
                        elapsed.as_millis(),
                        ECHO_PROTECTION_WINDOW_MS
                    );
                    warn!("│ Klipper has taken clipboard ownership with reduced MIME types");
                    warn!("│ DECISION: KlipperReannounce - caller should re-call SetSelection");
                    debug!("└────────────────────────────────────────────────────────────────");
                    // Don't transition state - we want to stay RdpOwned and reclaim
                    return PortalSyncDecision::KlipperReannounce;
                } else if mime_types.is_empty() {
                    // Klipper clearing clipboard (0 MIME types) after we reclaimed ownership
                    // This is a secondary Klipper behavior - it clears after takeover
                    warn!(
                        "│ KLIPPER CLIPBOARD CLEAR: 0 MIME types {}ms after RDP ownership",
                        elapsed.as_millis()
                    );
                    warn!("│ Klipper is clearing clipboard after we reclaimed it");
                    warn!("│ DECISION: KlipperReannounce - re-announce to counter clearing");
                    debug!("└────────────────────────────────────────────────────────────────");
                    // Don't transition state - we want to stay RdpOwned and reclaim
                    return PortalSyncDecision::KlipperReannounce;
                } else {
                    debug!(
                        "│ DECISION: Block (echo protection, {}ms < {}ms window)",
                        elapsed.as_millis(),
                        ECHO_PROTECTION_WINDOW_MS
                    );
                }
                debug!("└────────────────────────────────────────────────────────────────");
                return PortalSyncDecision::Block;
            } else {
                // D-Bus signal after protection window - likely a real user copy
                debug!(
                    "│ DECISION: Allow ({}ms >= {}ms, likely user copy)",
                    elapsed.as_millis(),
                    ECHO_PROTECTION_WINDOW_MS
                );
            }
        } else {
            debug!("│ Not in RdpOwned state, proceeding with sync check");
        }

        // Check for loop using hash comparison - but SKIP for authoritative D-Bus signals
        // that passed the timing check above
        if !force && self.loop_detector.would_cause_loop_mime(&mime_types) {
            debug!("│ DECISION: Block (loop detection - hash match)");
            debug!("└────────────────────────────────────────────────────────────────");
            warn!("Ignoring Portal format list due to loop detection");
            return PortalSyncDecision::Block;
        }

        self.state = ClipboardState::PortalOwned(mime_types.clone());
        debug!("│ State transition: {} → PortalOwned", state_name);
        debug!("│ DECISION: Allow sync to RDP client");
        debug!("└────────────────────────────────────────────────────────────────");

        self.loop_detector
            .record_mime_types(&mime_types, ClipboardSource::Local);
        self.loop_detector.record_sync(ClipboardSource::Local);

        PortalSyncDecision::Allow
    }

    pub fn check_content(&mut self, content: &[u8], from_rdp: bool) -> bool {
        let source = if from_rdp {
            ClipboardSource::Rdp
        } else {
            ClipboardSource::Local
        };

        if self.loop_detector.would_cause_content_loop(content, source) {
            warn!("Content loop detected, skipping transfer");
            return false;
        }

        self.loop_detector.record_content(content, source);

        true
    }

    pub fn set_syncing(&mut self, direction: SyncDirection) {
        self.state = ClipboardState::Syncing(direction);
    }

    pub fn reset(&mut self) {
        self.state = ClipboardState::Idle;
    }

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

    pub fn set_rdp_formats(&mut self, formats: Vec<ClipboardFormat>) {
        self.state = ClipboardState::RdpOwned(formats.clone(), SystemTime::now());
        self.loop_detector
            .record_formats(&formats, ClipboardSource::Rdp);
    }

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
        assert!(manager.handle_rdp_formats(formats.clone()));

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
        assert_eq!(
            manager.handle_portal_formats(mime_types.clone(), true),
            PortalSyncDecision::Allow
        );

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
        assert!(manager.handle_rdp_formats(formats));

        // Immediate D-Bus signal should be blocked (echo protection)
        let mime_types = vec!["text/plain".to_string()];
        assert_eq!(
            manager.handle_portal_formats(mime_types.clone(), true),
            PortalSyncDecision::Block
        );

        // Non-force Portal signal should always be blocked when RDP owns
        assert_eq!(
            manager.handle_portal_formats(mime_types, false),
            PortalSyncDecision::Block
        );
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
        assert!(manager.handle_rdp_formats(text_formats.clone()));

        // Simulate Portal echo - record it as coming from Local
        let text_mime = vec!["text/plain".to_string()];
        manager
            .loop_detector
            .record_mime_types(&text_mime, ClipboardSource::Local);

        // Now RDP trying to announce same formats again should be blocked (loop)
        assert!(!manager.handle_rdp_formats(text_formats));
    }

    #[test]
    fn test_sync_manager_content_check() {
        let mut manager = SyncManager::new();
        let content = b"Test content";

        // First check from RDP should pass
        assert!(manager.check_content(content, true));

        // Same content from Portal should fail (loop)
        assert!(!manager.check_content(content, false));
    }

    #[test]
    fn test_sync_manager_reset() {
        let mut manager = SyncManager::new();
        let formats = make_text_formats();

        manager.handle_rdp_formats(formats);

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
        assert!(manager.handle_rdp_formats(text_formats));

        // RDP announces different format (image) - should succeed
        let image_formats = make_image_formats();
        assert!(manager.handle_rdp_formats(image_formats));
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
