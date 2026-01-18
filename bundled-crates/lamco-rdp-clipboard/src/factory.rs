//! Factory for creating RDP clipboard backends.

use ironrdp_cliprdr::backend::{CliprdrBackend, CliprdrBackendFactory};

use crate::backend::RdpCliprdrBackend;
use crate::event::{ClipboardEventReceiver, ClipboardEventSender};

/// Factory for creating [`RdpCliprdrBackend`] instances.
///
/// This factory creates backends that share a common event channel,
/// allowing centralized event processing across multiple RDP connections.
///
/// # Example
///
/// ```rust,ignore
/// use lamco_rdp_clipboard::RdpCliprdrFactory;
/// use ironrdp_cliprdr::backend::CliprdrBackendFactory;
///
/// let factory = RdpCliprdrFactory::new("/tmp/clipboard");
/// let receiver = factory.subscribe();
///
/// // Each call creates a new backend instance
/// let backend1 = factory.build_cliprdr_backend();
/// let backend2 = factory.build_cliprdr_backend();
///
/// // Process events from all backends
/// for event in receiver.drain() {
///     // Handle event...
/// }
/// ```
#[derive(Debug, Clone)]
pub struct RdpCliprdrFactory {
    /// Temporary directory for file transfers
    temp_dir: String,

    /// Shared event sender
    event_sender: ClipboardEventSender,
}

impl RdpCliprdrFactory {
    /// Create a new factory with the given temporary directory.
    pub fn new(temp_dir: impl Into<String>) -> Self {
        Self {
            temp_dir: temp_dir.into(),
            event_sender: ClipboardEventSender::new(),
        }
    }

    /// Create a factory with a custom event sender.
    pub fn with_event_sender(temp_dir: impl Into<String>, event_sender: ClipboardEventSender) -> Self {
        Self {
            temp_dir: temp_dir.into(),
            event_sender,
        }
    }

    /// Get a receiver for clipboard events.
    ///
    /// All backends created by this factory will send events to this receiver.
    pub fn subscribe(&self) -> ClipboardEventReceiver {
        self.event_sender.subscribe()
    }

    /// Get the temporary directory path.
    pub fn temp_dir(&self) -> &str {
        &self.temp_dir
    }

    /// Get the event sender (for sharing with other components).
    pub fn event_sender(&self) -> &ClipboardEventSender {
        &self.event_sender
    }
}

impl CliprdrBackendFactory for RdpCliprdrFactory {
    fn build_cliprdr_backend(&self) -> Box<dyn CliprdrBackend> {
        let backend = RdpCliprdrBackend::new(self.temp_dir.clone(), self.event_sender.clone());
        Box::new(backend)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_factory_creation() {
        let factory = RdpCliprdrFactory::new("/tmp/test");
        assert_eq!(factory.temp_dir(), "/tmp/test");
    }

    #[test]
    fn test_build_backend() {
        let factory = RdpCliprdrFactory::new("/tmp/test");
        let backend = factory.build_cliprdr_backend();

        assert_eq!(backend.temporary_directory(), "/tmp/test");
    }

    #[test]
    fn test_shared_event_channel() {
        let factory = RdpCliprdrFactory::new("/tmp/test");
        let receiver = factory.subscribe();

        // Create two backends
        let mut backend1 = factory.build_cliprdr_backend();
        let mut backend2 = factory.build_cliprdr_backend();

        // Both should send to the same receiver
        backend1.on_ready();
        backend2.on_ready();

        let events = receiver.drain();
        assert_eq!(events.len(), 2);
    }
}
