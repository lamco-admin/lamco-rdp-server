//! Clipboard Provider Abstraction
//!
//! Defines a unified interface for clipboard backends (Portal D-Bus,
//! Wayland data-control, Mutter D-Bus). The `ClipboardOrchestrator`
//! calls provider methods without knowing which backend is active.

use async_trait::async_trait;
use tokio::sync::mpsc;

use crate::clipboard::error::Result;

/// Events from a clipboard provider backend.
///
/// All providers translate their native signals into this common event type
/// before sending to the orchestrator's event channel.
#[derive(Debug, Clone)]
pub enum ClipboardProviderEvent {
    /// System clipboard ownership changed.
    ///
    /// `mime_types`: MIME types available from the new owner.
    /// `force`: true = authoritative source (D-Bus extension, data-control signal),
    ///          false = potentially echoed (Portal SelectionOwnerChanged)
    SelectionChanged {
        mime_types: Vec<String>,
        force: bool,
    },

    /// System requests data for a MIME type (delayed rendering / SelectionTransfer).
    ///
    /// The provider identified by `serial` that we need to fulfill via `complete_transfer()`.
    SelectionTransfer { serial: u32, mime_type: String },
}

/// Clipboard provider backend interface.
///
/// Abstracts over Portal D-Bus, Wayland data-control, and Mutter D-Bus
/// clipboard implementations. The orchestrator calls these methods
/// without knowing which backend is active.
#[async_trait]
pub trait ClipboardProvider: Send + Sync {
    /// Backend name for logging and diagnostics.
    fn name(&self) -> &'static str;

    /// Whether this provider supports file transfer (URI list).
    ///
    /// Portal and data-control support it. Mutter D-Bus is text-focused.
    fn supports_file_transfer(&self) -> bool;

    /// Announce clipboard ownership with available MIME types.
    ///
    /// Called when RDP client copies. Takes ownership of the system clipboard
    /// and announces formats (delayed rendering: data transferred on demand).
    async fn announce_formats(&self, mime_types: Vec<String>) -> Result<()>;

    /// Read clipboard data for a specific MIME type.
    ///
    /// Called when the Linux clipboard is owned by another app and we need
    /// to read data (Linux owns clipboard, Windows wants it).
    async fn read_data(&self, mime_type: &str) -> Result<Vec<u8>>;

    /// Complete a SelectionTransfer request (write data to system clipboard).
    ///
    /// Called when a Linux app pastes after we announced ownership. The `serial`
    /// matches the `SelectionTransfer` event from `subscribe()`.
    async fn complete_transfer(
        &self,
        serial: u32,
        mime_type: &str,
        data: Vec<u8>,
        success: bool,
    ) -> Result<()>;

    /// Subscribe to provider events.
    ///
    /// Returns a receiver for selection-changed and transfer-request events.
    /// The orchestrator spawns a listener that forwards these to its event channel.
    fn subscribe(&self) -> mpsc::UnboundedReceiver<ClipboardProviderEvent>;

    /// Check if the provider is alive and functional.
    async fn health_check(&self) -> Result<()>;

    /// Shut down the provider (stop listeners, release resources).
    async fn shutdown(&self);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_provider_event_debug() {
        let event = ClipboardProviderEvent::SelectionChanged {
            mime_types: vec!["text/plain".to_string()],
            force: true,
        };
        assert!(format!("{event:?}").contains("SelectionChanged"));

        let event = ClipboardProviderEvent::SelectionTransfer {
            serial: 42,
            mime_type: "text/plain".to_string(),
        };
        assert!(format!("{event:?}").contains("SelectionTransfer"));
    }
}
