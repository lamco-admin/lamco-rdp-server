//! RDP clipboard backend implementation.
//!
//! Implements the IronRDP [`CliprdrBackend`](ironrdp_cliprdr::backend::CliprdrBackend) trait.

use ironrdp_cliprdr::backend::CliprdrBackend;
use ironrdp_cliprdr::pdu::{
    ClipboardFormat as RdpClipboardFormat, ClipboardGeneralCapabilityFlags, FileContentsRequest, FileContentsResponse,
    FormatDataRequest, FormatDataResponse, LockDataId,
};
use ironrdp_core::AsAny;

use crate::event::{ClipboardEvent, ClipboardEventSender};

/// RDP clipboard backend that bridges IronRDP and [`ClipboardSink`].
///
/// This implementation queues events for asynchronous processing rather than
/// blocking the RDP message loop. Use [`ClipboardEventReceiver`]
/// to process events in an async context.
///
/// [`ClipboardSink`]: lamco_clipboard_core::ClipboardSink
/// [`ClipboardEventReceiver`]: crate::ClipboardEventReceiver
///
/// # Example
///
/// ```rust,ignore
/// use lamco_rdp_clipboard::{RdpCliprdrBackend, ClipboardEventSender};
///
/// let event_sender = ClipboardEventSender::new();
/// let receiver = event_sender.subscribe();
///
/// let backend = RdpCliprdrBackend::new(
///     "/tmp/clipboard".to_string(),
///     event_sender,
/// );
///
/// // Process events in an async task
/// loop {
///     for event in receiver.drain() {
///         match event {
///             ClipboardEvent::RemoteCopy { formats } => {
///                 // Handle remote copy...
///             }
///             _ => {}
///         }
///     }
/// }
/// ```
#[derive(Debug)]
pub struct RdpCliprdrBackend {
    /// Temporary directory for file transfers
    temp_dir: String,

    /// Event sender for async processing
    event_sender: ClipboardEventSender,

    /// Negotiated capabilities
    capabilities: ClipboardGeneralCapabilityFlags,

    /// Remote formats currently available
    remote_formats: Vec<RdpClipboardFormat>,

    /// Whether backend is ready
    is_ready: bool,
}

impl RdpCliprdrBackend {
    /// Create a new RDP clipboard backend.
    ///
    /// # Arguments
    ///
    /// * `temp_dir` - Directory for temporary file storage during transfers
    /// * `event_sender` - Sender for queueing events for async processing
    pub fn new(temp_dir: String, event_sender: ClipboardEventSender) -> Self {
        Self {
            temp_dir,
            event_sender,
            capabilities: ClipboardGeneralCapabilityFlags::empty(),
            remote_formats: Vec::new(),
            is_ready: false,
        }
    }

    /// Get the current remote formats
    pub fn remote_formats(&self) -> &[RdpClipboardFormat] {
        &self.remote_formats
    }

    /// Check if backend is ready
    pub fn is_ready(&self) -> bool {
        self.is_ready
    }

    /// Get the negotiated capabilities
    pub fn capabilities(&self) -> ClipboardGeneralCapabilityFlags {
        self.capabilities
    }

    /// Create an event sender/receiver pair and backend
    pub fn create_with_channel(temp_dir: String) -> (Self, crate::ClipboardEventReceiver) {
        let sender = ClipboardEventSender::new();
        let receiver = sender.subscribe();
        let backend = Self::new(temp_dir, sender);
        (backend, receiver)
    }
}

impl AsAny for RdpCliprdrBackend {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

impl CliprdrBackend for RdpCliprdrBackend {
    fn temporary_directory(&self) -> &str {
        &self.temp_dir
    }

    fn client_capabilities(&self) -> ClipboardGeneralCapabilityFlags {
        // Request support for long format names, file streaming, locking, and privacy
        ClipboardGeneralCapabilityFlags::USE_LONG_FORMAT_NAMES
            | ClipboardGeneralCapabilityFlags::STREAM_FILECLIP_ENABLED
            | ClipboardGeneralCapabilityFlags::CAN_LOCK_CLIPDATA
            // Privacy: don't include source file paths in clipboard data
            // This prevents leaking the original file location from the remote system
            | ClipboardGeneralCapabilityFlags::FILECLIP_NO_FILE_PATHS
    }

    fn on_ready(&mut self) {
        tracing::debug!("Clipboard backend ready");
        self.is_ready = true;
        self.event_sender.send(ClipboardEvent::Ready);
    }

    fn on_request_format_list(&mut self) {
        tracing::debug!("Format list requested");
        self.event_sender.send(ClipboardEvent::RequestFormatList);
    }

    fn on_process_negotiated_capabilities(&mut self, capabilities: ClipboardGeneralCapabilityFlags) {
        tracing::debug!("Negotiated capabilities: {:?}", capabilities);
        self.capabilities = capabilities;
        self.event_sender
            .send(ClipboardEvent::NegotiatedCapabilities(capabilities));
    }

    fn on_remote_copy(&mut self, available_formats: &[RdpClipboardFormat]) {
        tracing::debug!("Remote copy: {} formats available", available_formats.len());

        // Store formats for later reference
        self.remote_formats = available_formats.to_vec();

        // Queue for async processing
        self.event_sender.send(ClipboardEvent::remote_copy(available_formats));
    }

    fn on_format_data_request(&mut self, request: FormatDataRequest) {
        tracing::debug!("Format data request: format={:?}", request.format);
        self.event_sender.send(ClipboardEvent::format_data_request(&request));
    }

    fn on_format_data_response(&mut self, response: FormatDataResponse<'_>) {
        tracing::debug!(
            "Format data response: {} bytes, error={}",
            response.data().len(),
            response.is_error()
        );
        self.event_sender.send(ClipboardEvent::format_data_response(&response));
    }

    fn on_file_contents_request(&mut self, request: FileContentsRequest) {
        tracing::debug!(
            "File contents request: stream={}, index={}, pos={}, size={}",
            request.stream_id,
            request.index,
            request.position,
            request.requested_size
        );
        self.event_sender.send(ClipboardEvent::file_contents_request(&request));
    }

    fn on_file_contents_response(&mut self, response: FileContentsResponse<'_>) {
        tracing::debug!(
            "File contents response: stream={}, {} bytes",
            response.stream_id(),
            response.data().len()
        );
        self.event_sender
            .send(ClipboardEvent::file_contents_response(&response));
    }

    fn on_lock(&mut self, data_id: LockDataId) {
        tracing::debug!("Lock: data_id={}", data_id.0);
        self.event_sender.send(ClipboardEvent::lock(data_id));
    }

    fn on_unlock(&mut self, data_id: LockDataId) {
        tracing::debug!("Unlock: data_id={}", data_id.0);
        self.event_sender.send(ClipboardEvent::unlock(data_id));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_backend_creation() {
        let (backend, _receiver) = RdpCliprdrBackend::create_with_channel("/tmp/test".to_string());
        assert!(!backend.is_ready());
        assert_eq!(backend.temporary_directory(), "/tmp/test");
    }

    #[test]
    fn test_on_ready() {
        let sender = ClipboardEventSender::new();
        let receiver = sender.subscribe();
        let mut backend = RdpCliprdrBackend::new("/tmp".to_string(), sender);

        assert!(!backend.is_ready());
        backend.on_ready();
        assert!(backend.is_ready());

        let events = receiver.drain();
        assert_eq!(events.len(), 1);
        assert!(matches!(events[0], ClipboardEvent::Ready));
    }

    #[test]
    fn test_client_capabilities() {
        let (backend, _) = RdpCliprdrBackend::create_with_channel("/tmp".to_string());
        let caps = backend.client_capabilities();

        assert!(caps.contains(ClipboardGeneralCapabilityFlags::USE_LONG_FORMAT_NAMES));
        assert!(caps.contains(ClipboardGeneralCapabilityFlags::STREAM_FILECLIP_ENABLED));
        assert!(caps.contains(ClipboardGeneralCapabilityFlags::CAN_LOCK_CLIPDATA));
        assert!(caps.contains(ClipboardGeneralCapabilityFlags::FILECLIP_NO_FILE_PATHS));
    }
}
