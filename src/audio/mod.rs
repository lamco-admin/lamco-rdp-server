//! Audio Support for RDPSND (Remote Desktop Protocol Sound)
//!
//! This module implements audio streaming from the server to RDP clients
//! using the MS-RDPEA (Audio Output Virtual Channel Extension) protocol.
//!
//! # Architecture
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────────┐
//! │                      lamco-rdp-server                           │
//! │                                                                 │
//! │  ┌─────────────────┐    ┌─────────────────┐    ┌─────────────┐ │
//! │  │  PipeWire       │    │  OPUS Encoder   │    │  RDPSND     │ │
//! │  │  Audio Capture  │───▶│  (codecs mod)   │───▶│  Server     │ │
//! │  │  (capture.rs)   │    │                 │    │ (IronRDP)   │ │
//! │  └─────────────────┘    └─────────────────┘    └─────────────┘ │
//! │          │                      │                      │        │
//! │          ▼                      ▼                      ▼        │
//! │  ┌─────────────────────────────────────────────────────────────┐│
//! │  │              Audio Pipeline Manager (pipeline.rs)          ││
//! │  │  - Format negotiation                                       ││
//! │  │  - Latency management                                       ││
//! │  │  - Volume sync                                              ││
//! │  └─────────────────────────────────────────────────────────────┘│
//! └─────────────────────────────────────────────────────────────────┘
//!          │
//!          ▼ (Wave2 PDUs via SVC channel)
//!     ┌─────────────────┐
//!     │   RDP Client    │
//!     │  Audio Playback │
//!     └─────────────────┘
//! ```
//!
//! # Codec Support
//!
//! | Codec | Tag | Quality | Bandwidth | Client Support |
//! |-------|-----|---------|-----------|----------------|
//! | OPUS | 0x704F | Excellent | ~96 kbps | Modern clients |
//! | PCM | 0x0001 | Lossless | ~1.5 Mbps | Universal |
//! | ADPCM | 0x0002 | Good | ~352 kbps | Legacy Windows |
//! | G.711 μ-law | 0x0007 | Telephony | 64 kbps | All |
//! | G.711 A-law | 0x0006 | Telephony | 64 kbps | All |
//!
//! # Usage
//!
//! Audio support is integrated automatically when building the RDP server:
//!
//! ```ignore
//! use lamco_rdp_server::audio::LamcoSoundFactory;
//!
//! // Create sound factory (optionally with PipeWire node ID)
//! let sound_factory = LamcoSoundFactory::new(audio_node_id);
//!
//! // Add to RDP server builder
//! let rdp_server = RdpServer::builder()
//!     // ... other configuration ...
//!     .with_sound_factory(Some(Box::new(sound_factory)))
//!     .build();
//! ```
//!
//! # Portal Integration
//!
//! Desktop audio capture requires permission via the XDG Portal ScreenCast
//! interface. When the ScreenCast session includes audio, a PipeWire
//! `node_id` is provided that can be used to capture the desktop audio.
//!
//! # Performance
//!
//! Target metrics:
//! - End-to-end latency: <100ms (competitive with FreeRDP)
//! - CPU usage: 2-5% for OPUS encoding
//! - Memory: ~8MB additional (PipeWire + encoder + buffers)

pub mod capture;
pub mod codecs;
pub mod factory;
pub mod handler;
pub mod pipeline;

pub use capture::{AudioCapture, AudioCaptureHandle, AudioFormat, AudioSamples, CaptureConfig};
pub use codecs::{
    AdpcmDecoder, AdpcmEncoder, AlawEncoder, AudioEncoder, G711Encoder, G711Variant, MulawEncoder,
    OpusApplication, OpusEncoder, OpusEncoderConfig, PcmEncoder,
};
pub use factory::{create_sound_factory, LamcoSoundFactory};
pub use handler::PipeWireAudioHandler;
pub use pipeline::{AudioPipeline, FrameBuffer, PipelineConfig, PipelineState, PipelineStats};

pub fn audio_available() -> bool {
    if let Ok(runtime_dir) = std::env::var("XDG_RUNTIME_DIR") {
        let socket_path = format!("{runtime_dir}/pipewire-0");
        if std::path::Path::new(&socket_path).exists() {
            return true;
        }
    }
    false
}

pub fn default_audio_config() -> CaptureConfig {
    CaptureConfig::default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_audio_available() {
        // This test just ensures the function doesn't panic
        let _ = audio_available();
    }

    #[test]
    fn test_default_config() {
        let config = default_audio_config();
        assert_eq!(config.sample_rate, 48000);
        assert_eq!(config.channels, 2);
    }
}
