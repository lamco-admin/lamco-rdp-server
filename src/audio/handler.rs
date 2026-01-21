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

use ironrdp_rdpsnd::pdu::{AudioFormat, WaveFormat};
use ironrdp_rdpsnd::server::{RdpsndServerHandler, RdpsndServerMessage};
use ironrdp_server::ServerEvent;
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

use crate::audio::codecs::{AudioEncoder, OpusEncoderConfig};

/// Supported audio format specification for advertisement
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

/// PipeWire Audio Handler for RDPSND
///
/// Implements `RdpsndServerHandler` to bridge PipeWire audio capture
/// with RDP audio streaming. Audio data is sent through the server's
/// event channel as `ServerEvent::Rdpsnd(RdpsndServerMessage::Wave)`.
pub struct PipeWireAudioHandler {
    /// Supported audio formats (advertised to client)
    formats: Vec<AudioFormat>,
    /// Selected format index (after negotiation)
    selected_format: Option<u16>,
    /// Audio encoder for selected format
    encoder: Option<AudioEncoder>,
    /// Server event channel for sending audio to RDP clients
    event_sender: Option<mpsc::UnboundedSender<ServerEvent>>,
    /// PipeWire node ID for audio capture (optional)
    node_id: Option<u32>,
    /// Whether capture is active
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
    /// Create a new PipeWire audio handler
    ///
    /// # Arguments
    ///
    /// * `event_sender` - Server event channel for sending audio to RDP clients
    /// * `node_id` - Optional PipeWire node ID for audio capture
    pub fn new(
        event_sender: Option<mpsc::UnboundedSender<ServerEvent>>,
        node_id: Option<u32>,
    ) -> Self {
        // Build list of supported formats in priority order
        let format_specs = vec![
            // OPUS - modern, efficient, best quality/bandwidth
            FormatSpec {
                format_tag: WaveFormat::OPUS,
                channels: 2,
                sample_rate: 48000,
                avg_bytes_per_sec: 12000, // ~96kbps
                block_align: 4,
                bits_per_sample: 16,
                extra_data: None,
            },
            // PCM - universal fallback, high bandwidth
            FormatSpec {
                format_tag: WaveFormat::PCM,
                channels: 2,
                sample_rate: 48000,
                avg_bytes_per_sec: 192000, // 48kHz * 2ch * 16bit
                block_align: 4,
                bits_per_sample: 16,
                extra_data: None,
            },
            // ADPCM - good compression for legacy clients
            FormatSpec {
                format_tag: WaveFormat::ADPCM,
                channels: 2,
                sample_rate: 44100,
                avg_bytes_per_sec: 44100, // ~352kbps stereo
                block_align: 2048,
                bits_per_sample: 4,
                extra_data: Some(adpcm_extra_data()),
            },
            // G.711 μ-law - telephony fallback (mono 8kHz)
            FormatSpec {
                format_tag: WaveFormat::MULAW,
                channels: 1,
                sample_rate: 8000,
                avg_bytes_per_sec: 8000, // 64kbps
                block_align: 1,
                bits_per_sample: 8,
                extra_data: None,
            },
            // G.711 A-law - telephony fallback (mono 8kHz)
            FormatSpec {
                format_tag: WaveFormat::ALAW,
                channels: 1,
                sample_rate: 8000,
                avg_bytes_per_sec: 8000, // 64kbps
                block_align: 1,
                bits_per_sample: 8,
                extra_data: None,
            },
        ];

        let formats: Vec<AudioFormat> = format_specs.iter().map(|f| f.to_audio_format()).collect();

        info!(
            "PipeWire audio handler created with {} formats, node_id={:?}, has_event_sender={}",
            formats.len(),
            node_id,
            event_sender.is_some()
        );

        Self {
            formats,
            selected_format: None,
            encoder: None,
            event_sender,
            node_id,
            active: false,
        }
    }

    /// Send encoded audio data to the RDP client
    ///
    /// This sends the audio through the server event channel as
    /// `ServerEvent::Rdpsnd(RdpsndServerMessage::Wave)`.
    ///
    /// # Arguments
    ///
    /// * `data` - Encoded audio data
    /// * `timestamp` - Audio timestamp in milliseconds
    ///
    /// # Returns
    ///
    /// `true` if the audio was sent successfully, `false` if the channel is unavailable
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

    /// Check if the handler can send audio
    pub fn can_send_audio(&self) -> bool {
        self.event_sender.is_some() && self.active
    }

    /// Get the selected encoder (if any)
    pub fn encoder(&mut self) -> Option<&mut AudioEncoder> {
        self.encoder.as_mut()
    }

    /// Check if audio capture is active
    pub fn is_active(&self) -> bool {
        self.active
    }

    /// Get the PipeWire node ID
    pub fn node_id(&self) -> Option<u32> {
        self.node_id
    }

    /// Create an encoder for the selected format
    fn create_encoder(&self, format: &AudioFormat) -> Option<AudioEncoder> {
        match format.format {
            WaveFormat::OPUS => {
                let config = OpusEncoderConfig {
                    sample_rate: format.n_samples_per_sec,
                    channels: format.n_channels as usize,
                    bitrate: 96000, // 96kbps
                    ..Default::default()
                };
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
    /// Return supported audio formats
    ///
    /// These formats are sent to the client during negotiation.
    /// The client will respond with formats it supports, and we'll
    /// pick the best mutual match.
    fn get_formats(&self) -> &[AudioFormat] {
        &self.formats
    }

    /// Called when client selects a format
    ///
    /// # Arguments
    ///
    /// * `client_format` - The client's audio format response
    ///
    /// # Returns
    ///
    /// Index of the selected format from our list, or None if no match
    fn start(&mut self, client_format: &ironrdp_rdpsnd::pdu::ClientAudioFormatPdu) -> Option<u16> {
        info!(
            "Client audio format negotiation: {} formats, flags={:?}",
            client_format.formats.len(),
            client_format.flags
        );

        // Find the best matching format
        // We iterate our formats in priority order and find the first
        // one that the client also supports
        for (our_idx, our_fmt) in self.formats.iter().enumerate() {
            for client_fmt in &client_format.formats {
                if formats_compatible(our_fmt, client_fmt) {
                    info!(
                        "Selected audio format: {:?} ({}Hz, {} channels)",
                        our_fmt.format, our_fmt.n_samples_per_sec, our_fmt.n_channels
                    );

                    // Create encoder for selected format
                    if let Some(encoder) = self.create_encoder(our_fmt) {
                        self.selected_format = Some(our_idx as u16);
                        self.encoder = Some(encoder);
                        self.active = true;

                        debug!(
                            "Audio encoder created: {}",
                            self.encoder.as_ref().unwrap().name()
                        );

                        return Some(our_idx as u16);
                    }
                }
            }
        }

        warn!("No compatible audio format found with client");
        None
    }

    /// Called when audio session ends
    fn stop(&mut self) {
        info!("Audio handler stopping");
        self.active = false;
        self.selected_format = None;
        self.encoder = None;
    }
}

/// Check if two audio formats are compatible
fn formats_compatible(server: &AudioFormat, client: &AudioFormat) -> bool {
    // Format tag must match
    if server.format != client.format {
        return false;
    }

    // For most formats, we need matching sample rate and channels
    // Some formats (like OPUS) are more flexible
    match server.format {
        WaveFormat::OPUS => {
            // OPUS is flexible - just need same tag
            true
        }
        _ => {
            // For other formats, need exact match
            server.n_channels == client.n_channels
                && server.n_samples_per_sec == client.n_samples_per_sec
                && server.bits_per_sample == client.bits_per_sample
        }
    }
}

/// Generate ADPCM extra data (coefficients)
///
/// IMA ADPCM requires coefficient data in the format header.
fn adpcm_extra_data() -> Vec<u8> {
    // Standard IMA ADPCM coefficients
    // wSamplesPerBlock (2 bytes) + wNumCoef (2 bytes) + coefficients
    let samples_per_block: u16 = 1017;
    let num_coef: u16 = 7;

    // Standard ADPCM coefficients
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
        // Create handler without event sender (for unit testing)
        let handler = PipeWireAudioHandler::new(None, None);

        assert!(!handler.formats.is_empty());
        assert!(!handler.is_active());
        assert!(handler.selected_format.is_none());
        assert!(!handler.can_send_audio()); // No event sender
    }

    #[test]
    fn test_handler_with_event_sender() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let handler = PipeWireAudioHandler::new(Some(tx), Some(42));

        assert!(!handler.formats.is_empty());
        assert!(!handler.is_active()); // Not active until start() called
        assert!(!handler.can_send_audio()); // Active but not started
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

        // OPUS is flexible - should match
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

        // PCM requires exact match
        assert!(formats_compatible(&pcm1, &pcm1.clone()));
        assert!(!formats_compatible(&pcm1, &pcm2)); // Different sample rate
    }

    #[test]
    fn test_adpcm_extra_data() {
        let data = adpcm_extra_data();
        assert!(!data.is_empty());
        // Should have samples_per_block + num_coef + 7 coefficient pairs
        assert!(data.len() >= 4 + 7 * 4);
    }
}
