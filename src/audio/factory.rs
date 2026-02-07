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
use crate::config::AudioConfig;

pub struct LamcoSoundFactory {
    audio_config: AudioConfig,
    event_sender: Option<mpsc::UnboundedSender<ServerEvent>>,
    node_id: Option<u32>,
    enabled: bool,
}

impl std::fmt::Debug for LamcoSoundFactory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LamcoSoundFactory")
            .field("node_id", &self.node_id)
            .field("enabled", &self.enabled)
            .field("codec", &self.audio_config.codec)
            .finish()
    }
}

impl LamcoSoundFactory {
    pub fn new(audio_config: AudioConfig, node_id: Option<u32>) -> Self {
        info!(
            "Sound factory created: codec={}, sample_rate={}, channels={}, node_id={:?}",
            audio_config.codec, audio_config.sample_rate, audio_config.channels, node_id
        );

        let enabled = audio_config.enabled;
        Self {
            audio_config,
            event_sender: None,
            node_id,
            enabled,
        }
    }

    pub fn disabled() -> Self {
        Self {
            audio_config: AudioConfig::default(),
            event_sender: None,
            node_id: None,
            enabled: false,
        }
    }

    pub fn set_node_id(&mut self, node_id: u32) {
        self.node_id = Some(node_id);
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    pub fn audio_config(&self) -> &AudioConfig {
        &self.audio_config
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

        let event_sender = match &self.event_sender {
            Some(sender) => sender.clone(),
            None => {
                warn!("No event sender available - audio events won't reach client");
                // Create handler anyway, it just won't be able to send audio
                // This allows format negotiation to complete
                let handler =
                    PipeWireAudioHandler::new(self.audio_config.clone(), None, self.node_id);
                return Box::new(handler);
            }
        };

        let handler =
            PipeWireAudioHandler::new(self.audio_config.clone(), Some(event_sender), self.node_id);
        info!("Created PipeWire audio handler for RDPSND");

        Box::new(handler)
    }
}

#[derive(Debug)]
struct NoOpAudioHandler;

impl RdpsndServerHandler for NoOpAudioHandler {
    fn get_formats(&self) -> &[ironrdp_rdpsnd::pdu::AudioFormat] {
        &[]
    }

    fn start(&mut self, _client_format: &ironrdp_rdpsnd::pdu::ClientAudioFormatPdu) -> Option<u16> {
        None
    }

    fn stop(&mut self) {}
}

pub fn create_sound_factory(audio_config: &AudioConfig, node_id: Option<u32>) -> LamcoSoundFactory {
    if audio_config.enabled {
        LamcoSoundFactory::new(audio_config.clone(), node_id)
    } else {
        LamcoSoundFactory::disabled()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_factory_creation() {
        let config = AudioConfig::default();
        let factory = LamcoSoundFactory::new(config, Some(42));
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
        let config = AudioConfig::default();
        let factory = LamcoSoundFactory::new(config, None);
        let handler = factory.build_backend();

        // Should have formats (OPUS, PCM, etc.)
        assert!(!handler.get_formats().is_empty());
    }

    #[test]
    fn test_create_sound_factory() {
        let enabled_config = AudioConfig::default();
        let enabled = create_sound_factory(&enabled_config, Some(123));
        assert!(enabled.is_enabled());

        let mut disabled_config = AudioConfig::default();
        disabled_config.enabled = false;
        let disabled = create_sound_factory(&disabled_config, Some(123));
        assert!(!disabled.is_enabled());
    }
}
