//! Clipboard Provider Abstraction
//!
//! **Execution Path:** N/A (trait definition + placeholders)
//! **Status:** 🚧 PLANNED (v1.4.0+) - Trait defined but not actively used
//! **Platform:** Universal (when implemented)
//! **Purpose:** Backend abstraction for Portal vs Wayland data-control
//!
//! Defines a unified interface for clipboard backends (Portal, Direct ext-data-control-v1).
//! Allows ClipboardOrchestrator to work with different backends without knowing implementation details.
//!
//! **Current State:** Trait and placeholders exist but unused. ClipboardOrchestrator uses Portal directly.
//! **Future (v1.4.0+):** Full provider abstraction when implementing WaylandDataControlMode.

use async_trait::async_trait;
use tokio::sync::mpsc;

use crate::clipboard::error::Result;

/// Events from clipboard provider
#[derive(Debug, Clone)]
pub enum ClipboardProviderEvent {
    /// Clipboard ownership changed (other app or manager took over)
    ///
    /// Contains MIME types announced by new owner.
    /// For cooperation mode, this triggers reading and syncing.
    SelectionChanged(Vec<String>),

    /// Data request from system
    ///
    /// System is requesting data for a specific MIME type.
    /// Provider should respond via write_data().
    DataRequest {
        /// MIME type being requested
        mime_type: String,
        /// Request ID for matching response
        request_id: u64,
    },

    /// Data response from system
    ///
    /// System provided data in response to our read request.
    DataResponse {
        /// MIME type of the data
        mime_type: String,
        /// The clipboard data
        data: Vec<u8>,
        /// Whether this was an error response
        is_error: bool,
    },
}

/// Clipboard provider backend interface
///
/// Abstracts over Portal D-Bus and future direct ext-data-control-v1 implementations.
/// Provides unified interface for clipboard operations regardless of backend.
#[async_trait]
pub trait ClipboardProvider: Send + Sync {
    /// Backend name for logging
    fn name(&self) -> &'static str;

    /// Announce clipboard ownership with MIME types
    ///
    /// Takes clipboard ownership and announces available formats.
    /// Other apps can then request data from us.
    async fn announce_formats(&self, mime_types: Vec<String>) -> Result<()>;

    /// Read clipboard data for specific MIME type
    ///
    /// Requests data from current clipboard owner.
    /// Response will arrive via ClipboardProviderEvent::DataResponse.
    async fn read_data(&self, mime_type: String) -> Result<u64>;

    /// Write data in response to system request
    ///
    /// Called when system requests data from us (after we announced).
    async fn write_data(&self, request_id: u64, data: Vec<u8>) -> Result<()>;

    /// Subscribe to provider events
    ///
    /// Returns a receiver for clipboard events (selection changes, data requests, etc.)
    fn subscribe(&self) -> mpsc::UnboundedReceiver<ClipboardProviderEvent>;

    /// Check if provider is available and functional
    ///
    /// Performs basic health check (D-Bus connection, Portal session, etc.)
    async fn health_check(&self) -> Result<()>;
}

/// Portal-based clipboard provider (Placeholder)
///
/// NOTE: Not currently used. ClipboardManager uses Portal directly.
/// Defined for trait completeness and future provider abstraction work.
pub struct PortalClipboardProvider {
    _phantom: std::marker::PhantomData<()>,
}

/// Direct ext-data-control-v1 provider (Future: v1.4.0+)
///
/// Uses ext-data-control-v1 Wayland protocol directly.
/// Lower latency, more control, but only works in native (non-Flatpak) mode.
///
/// Status: Not yet implemented. Placeholder for Sprint 3-4 work.
pub struct DirectDataControlProvider {
    _phantom: std::marker::PhantomData<()>,
}

impl DirectDataControlProvider {
    /// Create new Direct provider (not yet implemented)
    pub fn new() -> Result<Self> {
        Err(crate::clipboard::error::ClipboardError::PortalError(
            "Direct data-control provider not yet implemented (Sprint 3-4)".to_string(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_provider_event_debug() {
        let event = ClipboardProviderEvent::SelectionChanged(vec!["text/plain".to_string()]);
        assert!(format!("{:?}", event).contains("SelectionChanged"));

        let event = ClipboardProviderEvent::DataRequest {
            mime_type: "text/plain".to_string(),
            request_id: 123,
        };
        assert!(format!("{:?}", event).contains("DataRequest"));
    }
}
