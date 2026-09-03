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
    server::{NegotiatedFormat, RdpsndError, RdpsndServerHandler, RdpsndServerMessage},
};
use ironrdp_server::ServerEvent;
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, error, info, warn};

use crate::{
    audio::{
        capture::{
            AudioCaptureHandle, AudioFormat as CaptureFormat, CaptureConfig, spawn_audio_capture,
        },
        codecs::{AudioEncoder, OpusEncoderConfig},
    },
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
    selected_format: Option<AudioFormat>,
    encoder: Option<AudioEncoder>,
    event_sender: Option<mpsc::UnboundedSender<ServerEvent>>,
    node_id: Option<u32>,
    active: bool,
    capture_stop: Option<oneshot::Sender<()>>,
    pump: Option<tokio::task::JoinHandle<()>>,
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
            // Advertise 44.1 kHz PCM first. It is the native rate of Windows
            // audio endpoints, so mstsc plays it directly. Offering the 48 kHz
            // capture rate first made mstsc select 48 kHz and then feed it to a
            // 44.1 kHz endpoint without resampling: pitch dropped ~1.5 semitones
            // (voices sounded deeper) and audio fell behind video ~5 s per minute
            // (48000/44100 = 1.088). start() drives PipeWire capture at whatever
            // rate is negotiated, so selecting 44.1 kHz resamples end to end.
            let pcm_44100_bytes_per_sec = 44100 * channels as u32 * 2;
            format_specs.push(FormatSpec {
                format_tag: WaveFormat::PCM,
                channels,
                sample_rate: 44100,
                avg_bytes_per_sec: pcm_44100_bytes_per_sec,
                block_align,
                bits_per_sample: 16,
                extra_data: None,
            });

            // Also offer the configured capture rate (e.g. 48 kHz) as a secondary
            // option for clients that resample correctly, but only when it differs.
            if sample_rate != 44100 {
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
            audio_config.codec,
            sample_rate,
            channels,
            formats.len(),
            node_id
        );

        Self {
            audio_config,
            formats,
            selected_format: None,
            encoder: None,
            event_sender,
            node_id,
            active: false,
            capture_stop: None,
            pump: None,
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
                    config.sample_rate,
                    config.channels,
                    config.bitrate,
                    config.frame_size,
                    self.audio_config.frame_ms
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

    fn choose_format<'a>(
        &mut self,
        common: &'a [NegotiatedFormat],
    ) -> Option<&'a NegotiatedFormat> {
        // `common` is already the intersection of get_formats() and what the
        // client accepted, in our get_formats() preference order (the crate
        // computes this via exact field equality -- see ironrdp-rdpsnd's
        // audio_format_eq). This replaces the old client-side manual iteration
        // + formats_compatible fuzzy-match (which specially tolerated any
        // client-advertised rate/channels for OPUS): the new crate-owned
        // negotiation has no such leniency, since the encoder is always built
        // from our own advertised parameters regardless of what the client
        // echoes back, and a compliant client echoes an offered entry
        // verbatim rather than inventing new rate/channel combinations.
        if common.is_empty() {
            warn!("No compatible audio format found with client");
        }
        common.first()
    }

    fn start(&mut self, format: &NegotiatedFormat) -> Result<(), Box<dyn RdpsndError>> {
        let audio_format = format.format();
        info!(
            "Selected audio format: {:?} ({}Hz, {} channels)",
            audio_format.format, audio_format.n_samples_per_sec, audio_format.n_channels
        );

        let encoder = match self.create_encoder(audio_format) {
            Some(encoder) => encoder,
            // create_encoder already logs the specific failure (unsupported
            // format tag, or the underlying codec constructor's error).
            None => {
                return Err(Box::new(std::io::Error::other(
                    "audio encoder creation failed, see prior log for detail",
                )));
            }
        };
        debug!("Audio encoder created: {}", encoder.name());
        self.selected_format = Some(audio_format.clone());
        self.active = true;

        let Some(sender) = self.event_sender.clone() else {
            // Format is selected so negotiation completes, but with no channel
            // back to the server there is nowhere to deliver captured audio, so
            // capture does not start.
            warn!("No event sender available; audio capture not started");
            return Ok(());
        };

        let sample_rate = audio_format.n_samples_per_sec;
        let channels = u32::from(audio_format.n_channels.max(1));
        let frame_ms = self.audio_config.frame_ms.max(1);
        // Interleaved sample count for one RDP wave frame. Opus requires exactly
        // this many per encode; PCM/G711/ADPCM tolerate it. The pump timestamps
        // each wave from wall-clock elapsed rather than counting frames.
        let frame_len = ((sample_rate * frame_ms / 1000) * channels).max(channels) as usize;

        let capture_config = CaptureConfig {
            sample_rate,
            channels,
            format: CaptureFormat::I16,
            buffer_frames: 1024,
        };

        // node_id is left as configured (currently None): lamco-pipewire sets
        // stream.capture.sink=true, so a null target binds the default sink's
        // monitor — the desktop audio output the client wants to hear.
        let handle = match spawn_audio_capture(capture_config, self.node_id, 16) {
            Ok(handle) => handle,
            Err(e) => {
                error!("Failed to start audio capture: {e:#}");
                return Err(Box::new(std::io::Error::other(
                    "audio capture start failed",
                )));
            }
        };

        let (stop_tx, stop_rx) = oneshot::channel();
        let pump = tokio::spawn(audio_pump(handle, stop_rx, encoder, sender, frame_len));
        self.capture_stop = Some(stop_tx);
        self.pump = Some(pump);

        info!("Audio capture started: {sample_rate}Hz, {channels}ch, {frame_ms}ms frames");
        Ok(())
    }

    fn stop(&mut self) {
        if !self.active {
            // Never started (e.g. connection dropped before TrainingConfirm),
            // nothing to tear down
            return;
        }
        info!("Audio handler stopping");
        // Signal the pump; it flips the capture stop flag and exits on its own.
        // Detach rather than abort so the PipeWire thread is always told to stop
        // even if the pump is mid-encode when the client goes away.
        if let Some(stop_tx) = self.capture_stop.take() {
            let _ = stop_tx.send(());
        }
        self.pump.take();
        self.active = false;
        self.selected_format = None;
        self.encoder = None;
    }
}

/// Drains captured PCM into `frame_len`-sample chunks, encodes each, and hands
/// the result to the server event loop as an RDPSND wave. Runs until the client
/// stops audio (`stop_rx`), the capture stream ends, or the event channel closes.
async fn audio_pump(
    mut handle: AudioCaptureHandle,
    mut stop_rx: oneshot::Receiver<()>,
    mut encoder: AudioEncoder,
    sender: mpsc::UnboundedSender<ServerEvent>,
    frame_len: usize,
) {
    let start = std::time::Instant::now();
    let mut pending: Vec<i16> = Vec::with_capacity(frame_len * 4);

    loop {
        tokio::select! {
            biased;
            _ = &mut stop_rx => break,
            samples = handle.receiver.recv() => {
                let Some(samples) = samples else { break };
                pending.extend_from_slice(&samples.to_i16());

                while pending.len() >= frame_len {
                    let frame: Vec<i16> = pending.drain(..frame_len).collect();
                    match encoder.encode_i16(&frame) {
                        Ok(data) if !data.is_empty() => {
                            // Timestamp each wave from wall-clock elapsed, not a
                            // per-frame accumulator. The accumulator drifts behind
                            // real time whenever capture or encode can't keep pace,
                            // and the RDPSND server then drops the lagging waves as
                            // stale (audible dropouts).
                            let timestamp = start.elapsed().as_millis() as u32;
                            let wave = RdpsndServerMessage::Wave(data, timestamp);
                            if sender.send(ServerEvent::Rdpsnd(wave)).is_err() {
                                handle.stop();
                                return;
                            }
                        }
                        Ok(_) => {}
                        Err(e) => warn!("Audio encode error: {e}"),
                    }
                }
            }
        }
    }

    handle.stop();
}

impl Drop for PipeWireAudioHandler {
    fn drop(&mut self) {
        if let Some(stop_tx) = self.capture_stop.take() {
            let _ = stop_tx.send(());
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
    fn test_adpcm_extra_data() {
        let data = adpcm_extra_data();
        assert!(!data.is_empty());
        assert!(data.len() >= 4 + 7 * 4);
    }
}
