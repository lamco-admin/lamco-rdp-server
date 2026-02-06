//! RDPSND Server Factory Implementation
//!
//! This module implements the `SoundServerFactory` trait from IronRDP,
//! providing the integration point between lamco-rdp-server and the
//! IronRDP RdpServer builder.
//!
//! The factory creates `PipeWireAudioHandler` instances for each
//! RDP session that supports audio.
//!
//! # Event Flow
//!
//! ```text
//! LamcoSoundFactory
//!       │
//!       ├── set_sender(ServerEvent channel) ← called by RdpServer
//!       │
//!       └── build_backend() → PipeWireAudioHandler
//!                                    │
//!                                    └── sends ServerEvent::Rdpsnd(Wave)
//!                                              │
//!                                              ▼
//!                                    RdpServer event loop
//!                                              │
//!                                              └── rdpsnd.wave()
//! ```

use ironrdp_rdpsnd::server::RdpsndServerHandler;
use ironrdp_server::{ServerEvent, ServerEventSender, SoundServerFactory};
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use crate::audio::handler::PipeWireAudioHandler;

/// Factory for creating RDPSND audio handlers
///
/// Implements `SoundServerFactory` to integrate with IronRDP's `RdpServer`.
/// The factory receives a server event channel via `set_sender()` and passes
/// it to the audio handler so captured audio can be sent to RDP clients.
pub struct LamcoSoundFactory {
    /// Channel sender for server events (received from RdpServer)
    event_sender: Option<mpsc::UnboundedSender<ServerEvent>>,
    /// PipeWire node ID for audio capture (optional)
    node_id: Option<u32>,
    /// Whether audio is enabled
    enabled: bool,
}

impl std::fmt::Debug for LamcoSoundFactory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LamcoSoundFactory")
            .field("node_id", &self.node_id)
            .field("enabled", &self.enabled)
            .finish()
    }
}

impl LamcoSoundFactory {
    /// Create a new sound factory
    ///
    /// # Arguments
    ///
    /// * `node_id` - Optional PipeWire node ID for audio capture
    pub fn new(node_id: Option<u32>) -> Self {
        info!("Sound factory created with node_id={:?}", node_id);

        Self {
            event_sender: None,
            node_id,
            enabled: true,
        }
    }

    /// Create a disabled sound factory (no audio support)
    pub fn disabled() -> Self {
        Self {
            event_sender: None,
            node_id: None,
            enabled: false,
        }
    }

    /// Set the PipeWire node ID
    pub fn set_node_id(&mut self, node_id: u32) {
        self.node_id = Some(node_id);
    }

    /// Check if audio is enabled
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }
}

impl ServerEventSender for LamcoSoundFactory {
    fn set_sender(&mut self, sender: mpsc::UnboundedSender<ServerEvent>) {
        self.event_sender = Some(sender);
    }
}

impl SoundServerFactory for LamcoSoundFactory {
    fn build_backend(&self) -> Box<dyn RdpsndServerHandler> {
        if !self.enabled {
            debug!("Audio disabled, creating no-op handler");
            return Box::new(NoOpAudioHandler);
        }

        // Clone the event sender for the handler
        let event_sender = match &self.event_sender {
            Some(sender) => sender.clone(),
            None => {
                warn!("No event sender available - audio events won't reach client");
                // Create handler anyway, it just won't be able to send audio
                // This allows format negotiation to complete
                let handler = PipeWireAudioHandler::new(None, self.node_id);
                return Box::new(handler);
            }
        };

        let handler = PipeWireAudioHandler::new(Some(event_sender), self.node_id);
        info!("Created PipeWire audio handler for RDPSND");

        Box::new(handler)
    }
}

/// No-op audio handler for when audio is disabled
#[derive(Debug)]
struct NoOpAudioHandler;

impl RdpsndServerHandler for NoOpAudioHandler {
    fn get_formats(&self) -> &[ironrdp_rdpsnd::pdu::AudioFormat] {
        // Return empty formats - client will see no audio support
        &[]
    }

    fn start(&mut self, _client_format: &ironrdp_rdpsnd::pdu::ClientAudioFormatPdu) -> Option<u16> {
        // Never start - no formats supported
        None
    }

    fn stop(&mut self) {
        // Nothing to stop
    }
}

/// Create audio factory based on configuration and capabilities
///
/// # Arguments
///
/// * `enabled` - Whether audio is enabled in configuration
/// * `node_id` - Optional PipeWire node ID from portal session
///
/// # Returns
///
/// Sound factory configured appropriately
pub fn create_sound_factory(enabled: bool, node_id: Option<u32>) -> LamcoSoundFactory {
    if enabled {
        LamcoSoundFactory::new(node_id)
    } else {
        LamcoSoundFactory::disabled()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_factory_creation() {
        let factory = LamcoSoundFactory::new(Some(42));
        assert!(factory.is_enabled());
        assert_eq!(factory.node_id, Some(42));
    }

    #[test]
    fn test_factory_disabled() {
        let factory = LamcoSoundFactory::disabled();
        assert!(!factory.is_enabled());

        let handler = factory.build_backend();
        assert!(handler.get_formats().is_empty());
    }

    #[test]
    fn test_factory_build_backend() {
        let factory = LamcoSoundFactory::new(None);
        let handler = factory.build_backend();

        // Should have formats (OPUS, PCM, etc.)
        assert!(!handler.get_formats().is_empty());
    }

    #[test]
    fn test_create_sound_factory() {
        let enabled = create_sound_factory(true, Some(123));
        assert!(enabled.is_enabled());

        let disabled = create_sound_factory(false, Some(123));
        assert!(!disabled.is_enabled());
    }
}
