//! RDPSND Server Handler Implementation
//!
//! This module implements the `RdpsndServerHandler` trait from IronRDP,
//! providing the bridge between PipeWire audio capture and RDP audio streaming.
//!
//! # Handler Lifecycle
//!
//! 1. `get_formats()` - Server advertises supported audio formats to client
//! 2. Client responds with its supported formats
//! 3. `start()` - Handler selects best matching format and starts capture
//! 4. Audio frames sent via `ServerEvent::Rdpsnd(Wave)` → server calls `rdpsnd.wave()`
//! 5. `stop()` - Handler stops capture on session end
//!
//! # Event Flow
//!
//! Audio data flows through the server event channel:
//! ```text
//! PipeWire capture → encode → ServerEvent::Rdpsnd(Wave) → RdpServer → client
//! ```

use ironrdp_rdpsnd::{
    pdu::{AudioFormat, WaveFormat},
    server::{RdpsndServerHandler, RdpsndServerMessage},
};
use ironrdp_server::ServerEvent;
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

use crate::{
    audio::codecs::{AudioEncoder, OpusEncoderConfig},
    config::AudioConfig,
};

#[derive(Debug, Clone)]
struct FormatSpec {
    format_tag: WaveFormat,
    channels: u16,
    sample_rate: u32,
    avg_bytes_per_sec: u32,
    block_align: u16,
    bits_per_sample: u16,
    extra_data: Option<Vec<u8>>,
}

impl FormatSpec {
    fn to_audio_format(&self) -> AudioFormat {
        AudioFormat {
            format: self.format_tag,
            n_channels: self.channels,
            n_samples_per_sec: self.sample_rate,
            n_avg_bytes_per_sec: self.avg_bytes_per_sec,
            n_block_align: self.block_align,
            bits_per_sample: self.bits_per_sample,
            data: self.extra_data.clone(),
        }
    }
}

pub struct PipeWireAudioHandler {
    audio_config: AudioConfig,
    formats: Vec<AudioFormat>,
    selected_format: Option<u16>,
    encoder: Option<AudioEncoder>,
    event_sender: Option<mpsc::UnboundedSender<ServerEvent>>,
    node_id: Option<u32>,
    active: bool,
}

impl std::fmt::Debug for PipeWireAudioHandler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PipeWireAudioHandler")
            .field("formats", &self.formats.len())
            .field("selected_format", &self.selected_format)
            .field("has_event_sender", &self.event_sender.is_some())
            .field("node_id", &self.node_id)
            .field("active", &self.active)
            .finish()
    }
}

impl PipeWireAudioHandler {
    pub fn new(
        audio_config: AudioConfig,
        event_sender: Option<mpsc::UnboundedSender<ServerEvent>>,
        node_id: Option<u32>,
    ) -> Self {
        let sample_rate = audio_config.sample_rate;
        let channels = audio_config.channels as u16;
        let block_align = channels * 2; // 16-bit samples
        let pcm_bytes_per_sec = sample_rate * channels as u32 * 2;

        let opus_bytes_per_sec = audio_config.opus_bitrate / 8;

        // Ordered by preference: preferred codec first, then fallbacks
        let mut format_specs = vec![];

        if audio_config.codec == "opus" || audio_config.codec == "auto" {
            format_specs.push(FormatSpec {
                format_tag: WaveFormat::OPUS,
                channels,
                sample_rate,
                avg_bytes_per_sec: opus_bytes_per_sec,
                block_align,
                bits_per_sample: 16,
                extra_data: None,
            });
        }

        if audio_config.codec == "pcm" || audio_config.codec == "auto" {
            format_specs.push(FormatSpec {
                format_tag: WaveFormat::PCM,
                channels,
                sample_rate,
                avg_bytes_per_sec: pcm_bytes_per_sec,
                block_align,
                bits_per_sample: 16,
                extra_data: None,
            });
        }

        if audio_config.codec == "adpcm" || audio_config.codec == "auto" {
            format_specs.push(FormatSpec {
                format_tag: WaveFormat::ADPCM,
                channels,
                sample_rate: 44100, // ADPCM standard rate
                avg_bytes_per_sec: 44100,
                block_align: 2048,
                bits_per_sample: 4,
                extra_data: Some(adpcm_extra_data()),
            });
        }

        if audio_config.codec == "auto" {
            format_specs.push(FormatSpec {
                format_tag: WaveFormat::MULAW,
                channels: 1,
                sample_rate: 8000,
                avg_bytes_per_sec: 8000,
                block_align: 1,
                bits_per_sample: 8,
                extra_data: None,
            });
            format_specs.push(FormatSpec {
                format_tag: WaveFormat::ALAW,
                channels: 1,
                sample_rate: 8000,
                avg_bytes_per_sec: 8000,
                block_align: 1,
                bits_per_sample: 8,
                extra_data: None,
            });
        }

        let formats: Vec<AudioFormat> = format_specs
            .iter()
            .map(FormatSpec::to_audio_format)
            .collect();

        info!(
            "PipeWire audio handler: codec={}, sample_rate={}, channels={}, formats={}, node_id={:?}",
            audio_config.codec, sample_rate, channels, formats.len(), node_id
        );

        Self {
            audio_config,
            formats,
            selected_format: None,
            encoder: None,
            event_sender,
            node_id,
            active: false,
        }
    }

    /// Returns `true` if sent, `false` if no event channel.
    pub fn send_audio(&self, data: Vec<u8>, timestamp: u32) -> bool {
        if let Some(sender) = &self.event_sender {
            let msg = ServerEvent::Rdpsnd(RdpsndServerMessage::Wave(data, timestamp));
            if let Err(e) = sender.send(msg) {
                error!("Failed to send audio event: {}", e);
                return false;
            }
            true
        } else {
            warn!("No event sender available, audio not sent");
            false
        }
    }

    pub fn can_send_audio(&self) -> bool {
        self.event_sender.is_some() && self.active
    }

    pub fn encoder(&mut self) -> Option<&mut AudioEncoder> {
        self.encoder.as_mut()
    }

    pub fn is_active(&self) -> bool {
        self.active
    }

    pub fn node_id(&self) -> Option<u32> {
        self.node_id
    }

    fn create_encoder(&self, format: &AudioFormat) -> Option<AudioEncoder> {
        match format.format {
            WaveFormat::OPUS => {
                let frame_size =
                    (format.n_samples_per_sec * self.audio_config.frame_ms / 1000) as usize;
                let config = OpusEncoderConfig {
                    sample_rate: format.n_samples_per_sec,
                    channels: format.n_channels as usize,
                    bitrate: self.audio_config.opus_bitrate,
                    frame_size,
                    ..Default::default()
                };
                debug!(
                    "Creating OPUS encoder: sample_rate={}, channels={}, bitrate={}, frame_size={} ({}ms)",
                    config.sample_rate, config.channels, config.bitrate, config.frame_size, self.audio_config.frame_ms
                );
                match AudioEncoder::opus_with_config(config) {
                    Ok(enc) => Some(enc),
                    Err(e) => {
                        error!("Failed to create OPUS encoder: {}", e);
                        None
                    }
                }
            }
            WaveFormat::PCM => Some(AudioEncoder::pcm(
                format.n_channels as usize,
                format.n_samples_per_sec,
                format.bits_per_sample,
            )),
            WaveFormat::ADPCM => Some(AudioEncoder::adpcm(
                format.n_channels as usize,
                1017, // Standard samples per block
            )),
            WaveFormat::MULAW => Some(AudioEncoder::g711_mulaw()),
            WaveFormat::ALAW => Some(AudioEncoder::g711_alaw()),
            _ => {
                warn!("Unsupported format tag: {:?}", format.format);
                None
            }
        }
    }
}

impl RdpsndServerHandler for PipeWireAudioHandler {
    fn get_formats(&self) -> &[AudioFormat] {
        &self.formats
    }

    fn start(&mut self, client_format: &ironrdp_rdpsnd::pdu::ClientAudioFormatPdu) -> Option<u16> {
        info!(
            "Client audio format negotiation: {} formats, flags={:?}",
            client_format.formats.len(),
            client_format.flags
        );

        // Iterate our formats in priority order to find first mutual match
        for (our_idx, our_fmt) in self.formats.iter().enumerate() {
            for client_fmt in &client_format.formats {
                if formats_compatible(our_fmt, client_fmt) {
                    info!(
                        "Selected audio format: {:?} ({}Hz, {} channels)",
                        our_fmt.format, our_fmt.n_samples_per_sec, our_fmt.n_channels
                    );

                    if let Some(encoder) = self.create_encoder(our_fmt) {
                        self.selected_format = Some(our_idx as u16);
                        debug!("Audio encoder created: {}", encoder.name());
                        self.encoder = Some(encoder);
                        self.active = true;

                        return Some(our_idx as u16);
                    }
                }
            }
        }

        warn!("No compatible audio format found with client");
        None
    }

    fn stop(&mut self) {
        info!("Audio handler stopping");
        self.active = false;
        self.selected_format = None;
        self.encoder = None;
    }
}

fn formats_compatible(server: &AudioFormat, client: &AudioFormat) -> bool {
    if server.format != client.format {
        return false;
    }

    match server.format {
        // OPUS is flexible on rate/channels during negotiation
        WaveFormat::OPUS => true,
        _ => {
            server.n_channels == client.n_channels
                && server.n_samples_per_sec == client.n_samples_per_sec
                && server.bits_per_sample == client.bits_per_sample
        }
    }
}

/// IMA ADPCM requires coefficient data in the format header.
fn adpcm_extra_data() -> Vec<u8> {
    // wSamplesPerBlock (2 bytes) + wNumCoef (2 bytes) + coefficients
    let samples_per_block: u16 = 1017;
    let num_coef: u16 = 7;

    let coefficients: [(i16, i16); 7] = [
        (256, 0),
        (512, -256),
        (0, 0),
        (192, 64),
        (240, 0),
        (460, -208),
        (392, -232),
    ];

    let mut data = Vec::with_capacity(4 + num_coef as usize * 4);
    data.extend_from_slice(&samples_per_block.to_le_bytes());
    data.extend_from_slice(&num_coef.to_le_bytes());

    for (coef1, coef2) in &coefficients {
        data.extend_from_slice(&coef1.to_le_bytes());
        data.extend_from_slice(&coef2.to_le_bytes());
    }

    data
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_handler_creation() {
        let handler = PipeWireAudioHandler::new(AudioConfig::default(), None, None);

        assert!(!handler.formats.is_empty());
        assert!(!handler.is_active());
        assert!(handler.selected_format.is_none());
        assert!(!handler.can_send_audio()); // No event sender
    }

    #[test]
    fn test_handler_with_event_sender() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let handler = PipeWireAudioHandler::new(AudioConfig::default(), Some(tx), Some(42));

        assert!(!handler.formats.is_empty());
        assert!(!handler.is_active());
        assert!(!handler.can_send_audio());
        assert_eq!(handler.node_id(), Some(42));
    }

    #[test]
    fn test_format_compatibility() {
        let opus1 = AudioFormat {
            format: WaveFormat::OPUS,
            n_channels: 2,
            n_samples_per_sec: 48000,
            n_avg_bytes_per_sec: 12000,
            n_block_align: 4,
            bits_per_sample: 16,
            data: None,
        };

        let opus2 = AudioFormat {
            format: WaveFormat::OPUS,
            n_channels: 1,
            n_samples_per_sec: 24000,
            n_avg_bytes_per_sec: 6000,
            n_block_align: 2,
            bits_per_sample: 16,
            data: None,
        };

        assert!(formats_compatible(&opus1, &opus2));

        let pcm1 = AudioFormat {
            format: WaveFormat::PCM,
            n_channels: 2,
            n_samples_per_sec: 48000,
            n_avg_bytes_per_sec: 192000,
            n_block_align: 4,
            bits_per_sample: 16,
            data: None,
        };

        let pcm2 = AudioFormat {
            format: WaveFormat::PCM,
            n_channels: 2,
            n_samples_per_sec: 44100,
            n_avg_bytes_per_sec: 176400,
            n_block_align: 4,
            bits_per_sample: 16,
            data: None,
        };

        assert!(formats_compatible(&pcm1, &pcm1.clone()));
        assert!(!formats_compatible(&pcm1, &pcm2));
    }

    #[test]
    fn test_adpcm_extra_data() {
        let data = adpcm_extra_data();
        assert!(!data.is_empty());
        assert!(data.len() >= 4 + 7 * 4);
    }
}
