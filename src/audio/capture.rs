//! PipeWire Audio Capture
//!
//! This module provides desktop audio capture via PipeWire, enabling
//! RDPSND audio streaming. It captures audio from the desktop session
//! and delivers PCM samples for encoding.
//!
//! # Architecture
//!
//! ```text
//! PipeWire Daemon
//!        │
//!        ▼
//! ┌─────────────────┐
//! │  AudioCapture   │ ──► PCM samples (f32 or i16)
//! │  (pipewire-rs)  │
//! └─────────────────┘
//!        │
//!        ▼
//! Encoder (OPUS/PCM/ADPCM/G.711)
//! ```
//!
//! # Portal Integration
//!
//! Desktop audio capture typically requires Portal permission via the
//! ScreenCast portal. The `node_id` parameter connects to the audio
//! stream associated with the screen capture session.

use anyhow::{Context, Result};
use std::convert::TryInto;
use std::mem;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{debug, error, info, trace, warn};

use pipewire as pw;
use pw::spa;
use pw::spa::param::format::{MediaSubtype, MediaType};
use pw::spa::param::format_utils;
use pw::spa::pod::Pod;

/// Audio sample format
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioFormat {
    /// 32-bit floating point (-1.0 to 1.0)
    F32,
    /// 16-bit signed integer
    I16,
}

impl AudioFormat {
    /// Convert to SPA audio format
    fn to_spa_format(self) -> spa::param::audio::AudioFormat {
        match self {
            Self::F32 => spa::param::audio::AudioFormat::F32LE,
            Self::I16 => spa::param::audio::AudioFormat::S16LE,
        }
    }

    /// Get bytes per sample
    fn bytes_per_sample(self) -> usize {
        match self {
            Self::F32 => mem::size_of::<f32>(),
            Self::I16 => mem::size_of::<i16>(),
        }
    }
}

/// Audio capture configuration
#[derive(Debug, Clone)]
pub struct CaptureConfig {
    /// Sample rate in Hz (typically 48000)
    pub sample_rate: u32,
    /// Number of channels (1 = mono, 2 = stereo)
    pub channels: u32,
    /// Sample format
    pub format: AudioFormat,
    /// Buffer size in frames (affects latency)
    pub buffer_frames: u32,
}

impl Default for CaptureConfig {
    fn default() -> Self {
        Self {
            sample_rate: 48000,
            channels: 2,
            format: AudioFormat::F32,
            buffer_frames: 1024, // ~21ms at 48kHz
        }
    }
}

/// Audio samples from capture
#[derive(Debug, Clone)]
pub enum AudioSamples {
    /// 32-bit float samples (interleaved if stereo)
    F32(Vec<f32>),
    /// 16-bit integer samples (interleaved if stereo)
    I16(Vec<i16>),
}

impl AudioSamples {
    /// Get the number of samples (total, including all channels)
    pub fn len(&self) -> usize {
        match self {
            Self::F32(s) => s.len(),
            Self::I16(s) => s.len(),
        }
    }

    /// Check if empty
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Convert to i16 samples
    pub fn to_i16(&self) -> Vec<i16> {
        match self {
            Self::F32(samples) => samples
                .iter()
                .map(|&s| (s.clamp(-1.0, 1.0) * 32767.0) as i16)
                .collect(),
            Self::I16(samples) => samples.clone(),
        }
    }

    /// Convert to f32 samples
    pub fn to_f32(&self) -> Vec<f32> {
        match self {
            Self::F32(samples) => samples.clone(),
            Self::I16(samples) => samples.iter().map(|&s| s as f32 / 32768.0).collect(),
        }
    }
}

/// Audio capture handle for receiving samples
pub struct AudioCaptureHandle {
    /// Channel receiver for audio samples
    pub receiver: mpsc::Receiver<AudioSamples>,
    /// Signal to stop capture
    stop_signal: Arc<AtomicBool>,
}

impl AudioCaptureHandle {
    /// Stop the audio capture
    pub fn stop(&self) {
        self.stop_signal.store(true, Ordering::SeqCst);
    }

    /// Check if capture is stopped
    pub fn is_stopped(&self) -> bool {
        self.stop_signal.load(Ordering::SeqCst)
    }
}

/// User data passed to PipeWire stream callbacks
struct CaptureUserData {
    /// Negotiated audio format from PipeWire
    format: spa::param::audio::AudioInfoRaw,
    /// Desired output format
    output_format: AudioFormat,
    /// Channel sender for audio samples
    sender: mpsc::Sender<AudioSamples>,
    /// Stop signal
    stop_signal: Arc<AtomicBool>,
    /// Statistics: total samples captured
    samples_captured: u64,
    /// Statistics: samples dropped due to backpressure
    samples_dropped: u64,
}

/// PipeWire audio capture
///
/// Captures desktop audio via PipeWire and sends PCM samples through a channel.
pub struct AudioCapture {
    config: CaptureConfig,
    sender: mpsc::Sender<AudioSamples>,
    stop_signal: Arc<AtomicBool>,
}

impl AudioCapture {
    /// Create a new audio capture instance
    ///
    /// # Arguments
    ///
    /// * `config` - Capture configuration
    /// * `channel_size` - Size of the sample channel buffer
    ///
    /// # Returns
    ///
    /// Tuple of (AudioCapture, AudioCaptureHandle)
    pub fn new(config: CaptureConfig, channel_size: usize) -> (Self, AudioCaptureHandle) {
        let (sender, receiver) = mpsc::channel(channel_size);
        let stop_signal = Arc::new(AtomicBool::new(false));

        let capture = Self {
            config,
            sender,
            stop_signal: Arc::clone(&stop_signal),
        };

        let handle = AudioCaptureHandle {
            receiver,
            stop_signal,
        };

        (capture, handle)
    }

    /// Start capturing audio from a PipeWire node
    ///
    /// # Arguments
    ///
    /// * `node_id` - Optional PipeWire node ID to capture from (from Portal session).
    ///               If None, connects to any suitable audio source.
    ///
    /// This method blocks and runs the PipeWire main loop. It should be
    /// called from a dedicated thread.
    pub fn start_capture(&self, node_id: Option<u32>) -> Result<()> {
        info!(
            "Starting audio capture: {}Hz, {} channels, format={:?}, node_id={:?}",
            self.config.sample_rate, self.config.channels, self.config.format, node_id
        );

        // Create PipeWire main loop and context
        let mainloop =
            pw::main_loop::MainLoop::new(None).context("Failed to create PipeWire MainLoop")?;
        let context =
            pw::context::Context::new(&mainloop).context("Failed to create PipeWire Context")?;
        let core = context
            .connect(None)
            .context("Failed to connect to PipeWire daemon")?;

        // Create stream properties
        let mut props = pw::properties::properties! {
            *pw::keys::MEDIA_TYPE => "Audio",
            *pw::keys::MEDIA_CATEGORY => "Capture",
            *pw::keys::MEDIA_ROLE => "Screen",
            *pw::keys::NODE_NAME => "lamco-rdp-audio",
            *pw::keys::APP_NAME => "lamco-rdp-server",
        };

        // If we have a specific node ID from Portal, target it
        // Note: Using raw key string as TARGET_OBJECT requires v0_3_44 feature
        if let Some(id) = node_id {
            props.insert("target.object", id.to_string());
        }

        // Capture from sink monitor (desktop audio output)
        props.insert("stream.capture.sink", "true");

        let stream = pw::stream::Stream::new(&core, "rdp-audio-capture", props)
            .context("Failed to create PipeWire stream")?;

        // Prepare user data for callbacks
        let user_data = CaptureUserData {
            format: spa::param::audio::AudioInfoRaw::default(),
            output_format: self.config.format,
            sender: self.sender.clone(),
            stop_signal: Arc::clone(&self.stop_signal),
            samples_captured: 0,
            samples_dropped: 0,
        };

        // Clone stop signal for state_changed callback
        let stop_signal_for_callback = Arc::clone(&self.stop_signal);

        // Set up stream callbacks
        let _listener = stream
            .add_local_listener_with_user_data(user_data)
            .state_changed(move |_stream, _user_data, old, new| {
                debug!("Audio stream state: {:?} -> {:?}", old, new);

                match new {
                    pw::stream::StreamState::Error(err) => {
                        error!("Audio stream error: {}", err);
                        // Signal stop on error - the main loop will exit on next iteration
                        stop_signal_for_callback.store(true, Ordering::SeqCst);
                    }
                    pw::stream::StreamState::Streaming => {
                        info!("Audio capture streaming started");
                    }
                    pw::stream::StreamState::Paused => {
                        debug!("Audio stream paused");
                    }
                    _ => {}
                }
            })
            .param_changed(|_stream, user_data, id, param| {
                // NULL means to clear the format
                let Some(param) = param else {
                    return;
                };

                // Only process format changes
                if id != spa::param::ParamType::Format.as_raw() {
                    return;
                }

                // Parse media type and subtype
                let (media_type, media_subtype) = match format_utils::parse_format(param) {
                    Ok(v) => v,
                    Err(e) => {
                        warn!("Failed to parse audio format: {:?}", e);
                        return;
                    }
                };

                // Only accept raw audio
                if media_type != MediaType::Audio || media_subtype != MediaSubtype::Raw {
                    debug!(
                        "Ignoring non-raw audio format: {:?}/{:?}",
                        media_type, media_subtype
                    );
                    return;
                }

                // Parse the audio format details
                if let Err(e) = user_data.format.parse(param) {
                    warn!("Failed to parse audio info: {:?}", e);
                    return;
                }

                info!(
                    "Audio format negotiated: rate={}, channels={}, format={:?}",
                    user_data.format.rate(),
                    user_data.format.channels(),
                    user_data.format.format()
                );
            })
            .process(|stream, user_data| {
                // Check stop signal
                if user_data.stop_signal.load(Ordering::Relaxed) {
                    return;
                }

                // Dequeue buffer from PipeWire
                let Some(mut buffer) = stream.dequeue_buffer() else {
                    trace!("No buffer available");
                    return;
                };

                let datas = buffer.datas_mut();
                if datas.is_empty() {
                    return;
                }

                let data = &mut datas[0];
                let chunk = data.chunk();
                let size = chunk.size() as usize;

                if size == 0 {
                    return;
                }

                let Some(slice) = data.data() else {
                    return;
                };

                // Get actual format from negotiated params
                let n_channels = user_data.format.channels() as usize;
                if n_channels == 0 {
                    return;
                }

                // Convert raw bytes to samples based on negotiated format
                let samples = match user_data.format.format() {
                    spa::param::audio::AudioFormat::F32LE
                    | spa::param::audio::AudioFormat::F32BE => {
                        let byte_count = size.min(slice.len());
                        let sample_count = byte_count / mem::size_of::<f32>();
                        let mut f32_samples = Vec::with_capacity(sample_count);

                        for i in 0..sample_count {
                            let start = i * mem::size_of::<f32>();
                            let end = start + mem::size_of::<f32>();
                            if end <= slice.len() {
                                let bytes: [u8; 4] = slice[start..end].try_into().unwrap_or([0; 4]);
                                let sample = if user_data.format.format()
                                    == spa::param::audio::AudioFormat::F32LE
                                {
                                    f32::from_le_bytes(bytes)
                                } else {
                                    f32::from_be_bytes(bytes)
                                };
                                f32_samples.push(sample);
                            }
                        }

                        // Convert to output format if needed
                        match user_data.output_format {
                            AudioFormat::F32 => AudioSamples::F32(f32_samples),
                            AudioFormat::I16 => {
                                let i16_samples: Vec<i16> = f32_samples
                                    .iter()
                                    .map(|&s| (s.clamp(-1.0, 1.0) * 32767.0) as i16)
                                    .collect();
                                AudioSamples::I16(i16_samples)
                            }
                        }
                    }
                    spa::param::audio::AudioFormat::S16LE
                    | spa::param::audio::AudioFormat::S16BE => {
                        let byte_count = size.min(slice.len());
                        let sample_count = byte_count / mem::size_of::<i16>();
                        let mut i16_samples = Vec::with_capacity(sample_count);

                        for i in 0..sample_count {
                            let start = i * mem::size_of::<i16>();
                            let end = start + mem::size_of::<i16>();
                            if end <= slice.len() {
                                let bytes: [u8; 2] = slice[start..end].try_into().unwrap_or([0; 2]);
                                let sample = if user_data.format.format()
                                    == spa::param::audio::AudioFormat::S16LE
                                {
                                    i16::from_le_bytes(bytes)
                                } else {
                                    i16::from_be_bytes(bytes)
                                };
                                i16_samples.push(sample);
                            }
                        }

                        // Convert to output format if needed
                        match user_data.output_format {
                            AudioFormat::I16 => AudioSamples::I16(i16_samples),
                            AudioFormat::F32 => {
                                let f32_samples: Vec<f32> =
                                    i16_samples.iter().map(|&s| s as f32 / 32768.0).collect();
                                AudioSamples::F32(f32_samples)
                            }
                        }
                    }
                    other => {
                        trace!("Unsupported audio format: {:?}", other);
                        return;
                    }
                };

                let sample_count = samples.len();

                // Try to send samples (non-blocking to maintain realtime)
                match user_data.sender.try_send(samples) {
                    Ok(()) => {
                        user_data.samples_captured += sample_count as u64;
                        trace!("Captured {} samples", sample_count);
                    }
                    Err(mpsc::error::TrySendError::Full(_)) => {
                        // Channel full - drop samples (prefer low latency over completeness)
                        user_data.samples_dropped += sample_count as u64;
                        trace!("Dropped {} samples (channel full)", sample_count);
                    }
                    Err(mpsc::error::TrySendError::Closed(_)) => {
                        // Channel closed, signal stop
                        user_data.stop_signal.store(true, Ordering::SeqCst);
                        debug!("Audio sample channel closed");
                    }
                }
            })
            .register()
            .context("Failed to register stream listener")?;

        // Build audio format parameters pod
        // Request our preferred format, PipeWire may negotiate something different
        // We leave channels and rate empty to accept the native graph rate/channels
        // as shown in the official PipeWire audio-capture example
        let mut audio_info = spa::param::audio::AudioInfoRaw::new();
        audio_info.set_format(self.config.format.to_spa_format());
        // Optionally set rate/channels if we want to request specific values:
        // audio_info.set_rate(self.config.sample_rate);
        // audio_info.set_channels(self.config.channels);
        // Channel positions are left unset (UNPOSITIONED flag is default)

        // Serialize format to pod
        let obj = spa::pod::Object {
            type_: spa::utils::SpaTypes::ObjectParamFormat.as_raw(),
            id: spa::param::ParamType::EnumFormat.as_raw(),
            properties: audio_info.into(),
        };

        let pod_bytes: Vec<u8> = spa::pod::serialize::PodSerializer::serialize(
            std::io::Cursor::new(Vec::new()),
            &spa::pod::Value::Object(obj),
        )
        .context("Failed to serialize audio format pod")?
        .0
        .into_inner();

        let pod = Pod::from_bytes(&pod_bytes).context("Failed to create pod from bytes")?;

        let mut params = [pod];

        // Connect the stream
        let flags = pw::stream::StreamFlags::AUTOCONNECT
            | pw::stream::StreamFlags::MAP_BUFFERS
            | pw::stream::StreamFlags::RT_PROCESS;

        stream
            .connect(spa::utils::Direction::Input, node_id, flags, &mut params)
            .context("Failed to connect PipeWire stream")?;

        info!("Audio capture stream connected, starting main loop");

        // Run main loop until stopped using iterate() method
        // This allows us to check the stop signal between iterations
        let loop_ref = mainloop.loop_();
        while !self.stop_signal.load(Ordering::Relaxed) {
            // Iterate with 100ms timeout
            loop_ref.iterate(std::time::Duration::from_millis(100));
        }

        info!("Audio capture stopped");
        Ok(())
    }

    /// Stop the capture
    pub fn stop(&self) {
        self.stop_signal.store(true, Ordering::SeqCst);
    }
}

/// Spawn audio capture in a dedicated thread
///
/// # Arguments
///
/// * `config` - Capture configuration
/// * `node_id` - Optional PipeWire node ID
/// * `channel_size` - Sample channel buffer size
///
/// # Returns
///
/// Handle to receive audio samples and control capture
pub fn spawn_audio_capture(
    config: CaptureConfig,
    node_id: Option<u32>,
    channel_size: usize,
) -> Result<AudioCaptureHandle> {
    let (capture, handle) = AudioCapture::new(config, channel_size);

    // Spawn capture thread
    std::thread::Builder::new()
        .name("pipewire-audio".into())
        .spawn(move || {
            // Initialize PipeWire in this thread
            pw::init();

            if let Err(e) = capture.start_capture(node_id) {
                error!("Audio capture error: {:#}", e);
            }
        })
        .context("Failed to spawn audio capture thread")?;

    Ok(handle)
}

/// Create an audio capture handle connected to test samples
///
/// This is useful for testing the audio pipeline without actual PipeWire.
/// Generates a simple sine wave tone.
#[cfg(test)]
pub fn spawn_test_capture(
    config: CaptureConfig,
    channel_size: usize,
) -> Result<AudioCaptureHandle> {
    let (sender, receiver) = mpsc::channel(channel_size);
    let stop_signal = Arc::new(AtomicBool::new(false));

    let handle = AudioCaptureHandle {
        receiver,
        stop_signal: Arc::clone(&stop_signal),
    };

    let sample_rate = config.sample_rate;
    let channels = config.channels;

    // Spawn a thread that generates test samples
    std::thread::Builder::new()
        .name("test-audio".into())
        .spawn(move || {
            let mut phase: f32 = 0.0;
            let frequency = 440.0; // A4
            let phase_increment = 2.0 * std::f32::consts::PI * frequency / sample_rate as f32;
            let samples_per_frame = 960; // 20ms at 48kHz

            while !stop_signal.load(Ordering::Relaxed) {
                let mut samples = Vec::with_capacity(samples_per_frame * channels as usize);

                for _ in 0..samples_per_frame {
                    let sample = (phase.sin() * 0.3) as f32; // -0.3 to 0.3 amplitude
                    for _ in 0..channels {
                        samples.push(sample);
                    }
                    phase += phase_increment;
                    if phase > 2.0 * std::f32::consts::PI {
                        phase -= 2.0 * std::f32::consts::PI;
                    }
                }

                if sender.blocking_send(AudioSamples::F32(samples)).is_err() {
                    break;
                }

                // Sleep for approximately frame duration
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
        })
        .context("Failed to spawn test audio thread")?;

    Ok(handle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_capture_config_default() {
        let config = CaptureConfig::default();
        assert_eq!(config.sample_rate, 48000);
        assert_eq!(config.channels, 2);
        assert_eq!(config.format, AudioFormat::F32);
    }

    #[test]
    fn test_audio_samples_conversion() {
        let f32_samples = AudioSamples::F32(vec![0.0, 0.5, -0.5, 1.0, -1.0]);
        let i16_converted = f32_samples.to_i16();
        assert_eq!(i16_converted.len(), 5);
        assert_eq!(i16_converted[0], 0);
        // 0.5 * 32767 = 16383.5 -> 16383
        assert!((i16_converted[1] - 16383).abs() <= 1);

        let i16_samples = AudioSamples::I16(vec![0, 16384, -16384, 32767, -32768]);
        let f32_converted = i16_samples.to_f32();
        assert_eq!(f32_converted.len(), 5);
        assert!((f32_converted[0] - 0.0).abs() < 0.001);
    }

    #[test]
    fn test_audio_format_bytes_per_sample() {
        assert_eq!(AudioFormat::F32.bytes_per_sample(), 4);
        assert_eq!(AudioFormat::I16.bytes_per_sample(), 2);
    }

    #[test]
    fn test_audio_format_to_spa() {
        assert_eq!(
            AudioFormat::F32.to_spa_format(),
            spa::param::audio::AudioFormat::F32LE
        );
        assert_eq!(
            AudioFormat::I16.to_spa_format(),
            spa::param::audio::AudioFormat::S16LE
        );
    }
}
