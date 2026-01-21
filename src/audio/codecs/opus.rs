//! OPUS Audio Codec Wrapper
//!
//! OPUS is a royalty-free, highly versatile audio codec designed for
//! interactive real-time applications. It provides excellent quality
//! at low latencies (typically 20ms frames).
//!
//! This module wraps the `opus2` crate to provide a simple interface
//! for encoding PCM audio to OPUS for RDP streaming.
//!
//! Key features:
//! - Variable bitrate: 32-128 kbps typical
//! - Low latency: 20ms frame size
//! - Excellent quality for voice and music
//!
//! References:
//! - RFC 6716 (OPUS Codec)
//! - https://opus-codec.org

use anyhow::{Context, Result};
use tracing::debug;

/// OPUS audio application mode
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpusApplication {
    /// Optimized for voice (VoIP, speech)
    Voip,
    /// Optimized for non-voice audio (music, general audio)
    Audio,
    /// Optimized for lowest latency
    LowDelay,
}

impl From<OpusApplication> for opus2::Application {
    fn from(app: OpusApplication) -> Self {
        match app {
            OpusApplication::Voip => opus2::Application::Voip,
            OpusApplication::Audio => opus2::Application::Audio,
            OpusApplication::LowDelay => opus2::Application::LowDelay,
        }
    }
}

/// OPUS encoder configuration
#[derive(Debug, Clone)]
pub struct OpusEncoderConfig {
    /// Sample rate (48000 Hz recommended)
    pub sample_rate: u32,
    /// Number of channels (1 = mono, 2 = stereo)
    pub channels: usize,
    /// Target bitrate in bits per second
    pub bitrate: u32,
    /// Application mode
    pub application: OpusApplication,
    /// Frame size in samples (at 48kHz: 120, 240, 480, 960, 1920, 2880)
    /// 960 samples = 20ms at 48kHz (recommended)
    pub frame_size: usize,
}

impl Default for OpusEncoderConfig {
    fn default() -> Self {
        Self {
            sample_rate: 48000,
            channels: 2,
            bitrate: 96000, // 96 kbps - good quality for desktop audio
            application: OpusApplication::Audio,
            frame_size: 960, // 20ms at 48kHz
        }
    }
}

impl OpusEncoderConfig {
    /// Create a new config with the specified sample rate and channels
    pub fn new(sample_rate: u32, channels: usize) -> Self {
        Self {
            sample_rate,
            channels,
            ..Default::default()
        }
    }

    /// Set the bitrate
    pub fn with_bitrate(mut self, bitrate: u32) -> Self {
        self.bitrate = bitrate;
        self
    }

    /// Set the application mode
    pub fn with_application(mut self, application: OpusApplication) -> Self {
        self.application = application;
        self
    }

    /// Set the frame size
    pub fn with_frame_size(mut self, frame_size: usize) -> Self {
        self.frame_size = frame_size;
        self
    }

    /// Get frame duration in milliseconds
    pub fn frame_duration_ms(&self) -> u32 {
        (self.frame_size as u32 * 1000) / self.sample_rate
    }
}

/// OPUS Encoder
///
/// Encodes PCM audio to OPUS format. Supports both mono and stereo at
/// various sample rates (8000, 12000, 16000, 24000, 48000 Hz).
pub struct OpusEncoder {
    encoder: opus2::Encoder,
    config: OpusEncoderConfig,
    /// Temporary buffer for encoding output
    output_buffer: Vec<u8>,
}

impl std::fmt::Debug for OpusEncoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpusEncoder")
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

impl OpusEncoder {
    /// Create a new OPUS encoder with the given configuration
    pub fn new(config: OpusEncoderConfig) -> Result<Self> {
        let channels = match config.channels {
            1 => opus2::Channels::Mono,
            2 => opus2::Channels::Stereo,
            n => anyhow::bail!("Unsupported channel count: {} (must be 1 or 2)", n),
        };

        let mut encoder =
            opus2::Encoder::new(config.sample_rate, channels, config.application.into())
                .context("Failed to create OPUS encoder")?;

        // Set bitrate
        encoder
            .set_bitrate(opus2::Bitrate::Bits(config.bitrate as i32))
            .context("Failed to set OPUS bitrate")?;

        // Allocate output buffer (OPUS frame is at most ~1275 bytes)
        let output_buffer = vec![0u8; 4096];

        debug!(
            "OPUS encoder created: {}Hz, {} channels, {}bps, {}ms frames",
            config.sample_rate,
            config.channels,
            config.bitrate,
            config.frame_duration_ms()
        );

        Ok(Self {
            encoder,
            config,
            output_buffer,
        })
    }

    /// Create a default stereo encoder at 48kHz
    pub fn new_default() -> Result<Self> {
        Self::new(OpusEncoderConfig::default())
    }

    /// Get the encoder configuration
    pub fn config(&self) -> &OpusEncoderConfig {
        &self.config
    }

    /// Get the expected input frame size in samples (per channel)
    pub fn frame_size(&self) -> usize {
        self.config.frame_size
    }

    /// Get the expected input buffer size (samples * channels)
    pub fn input_frame_size(&self) -> usize {
        self.config.frame_size * self.config.channels
    }

    /// Set the encoder bitrate dynamically
    pub fn set_bitrate(&mut self, bitrate: u32) -> Result<()> {
        self.encoder
            .set_bitrate(opus2::Bitrate::Bits(bitrate as i32))
            .context("Failed to set OPUS bitrate")?;
        self.config.bitrate = bitrate;
        Ok(())
    }

    /// Encode a frame of PCM samples to OPUS
    ///
    /// # Arguments
    ///
    /// * `pcm` - Input PCM samples as i16 (interleaved if stereo)
    ///           Must be exactly `frame_size * channels` samples
    ///
    /// # Returns
    ///
    /// Encoded OPUS packet
    pub fn encode(&mut self, pcm: &[i16]) -> Result<Vec<u8>> {
        let expected_samples = self.input_frame_size();
        if pcm.len() != expected_samples {
            anyhow::bail!(
                "Invalid input size: expected {} samples, got {}",
                expected_samples,
                pcm.len()
            );
        }

        let encoded_len = self
            .encoder
            .encode(pcm, &mut self.output_buffer)
            .context("OPUS encoding failed")?;

        Ok(self.output_buffer[..encoded_len].to_vec())
    }

    /// Encode a frame of PCM samples as f32 to OPUS
    ///
    /// # Arguments
    ///
    /// * `pcm` - Input PCM samples as f32 [-1.0, 1.0] (interleaved if stereo)
    ///           Must be exactly `frame_size * channels` samples
    ///
    /// # Returns
    ///
    /// Encoded OPUS packet
    pub fn encode_float(&mut self, pcm: &[f32]) -> Result<Vec<u8>> {
        let expected_samples = self.input_frame_size();
        if pcm.len() != expected_samples {
            anyhow::bail!(
                "Invalid input size: expected {} samples, got {}",
                expected_samples,
                pcm.len()
            );
        }

        let encoded_len = self
            .encoder
            .encode_float(pcm, &mut self.output_buffer)
            .context("OPUS encoding failed")?;

        Ok(self.output_buffer[..encoded_len].to_vec())
    }
}

/// Convert f32 samples to i16
///
/// Clamps values to [-1.0, 1.0] range before converting.
#[inline]
pub fn f32_to_i16(samples: &[f32]) -> Vec<i16> {
    samples
        .iter()
        .map(|&s| {
            let clamped = s.clamp(-1.0, 1.0);
            (clamped * 32767.0) as i16
        })
        .collect()
}

/// Convert i16 samples to f32
#[inline]
pub fn i16_to_f32(samples: &[i16]) -> Vec<f32> {
    samples.iter().map(|&s| s as f32 / 32768.0).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_opus_encoder_creation() {
        let encoder = OpusEncoder::new_default();
        assert!(
            encoder.is_ok(),
            "Failed to create OPUS encoder: {:?}",
            encoder.err()
        );
    }

    #[test]
    fn test_opus_encode_stereo() {
        let mut encoder = OpusEncoder::new_default().unwrap();
        let frame_size = encoder.input_frame_size();

        // Create a simple sine wave test signal
        let samples: Vec<i16> = (0..frame_size)
            .map(|i| {
                let t = i as f32 / 48000.0;
                ((t * 440.0 * 2.0 * std::f32::consts::PI).sin() * 16000.0) as i16
            })
            .collect();

        let encoded = encoder.encode(&samples);
        assert!(encoded.is_ok(), "Encoding failed: {:?}", encoded.err());

        let packet = encoded.unwrap();
        assert!(!packet.is_empty(), "Encoded packet is empty");
        assert!(packet.len() < 1500, "Packet too large for MTU");
    }

    #[test]
    fn test_opus_encode_mono() {
        let config = OpusEncoderConfig {
            sample_rate: 48000,
            channels: 1,
            bitrate: 64000,
            application: OpusApplication::Voip,
            frame_size: 960,
        };
        let mut encoder = OpusEncoder::new(config).unwrap();
        let frame_size = encoder.input_frame_size();

        let samples: Vec<i16> = (0..frame_size).map(|i| (i % 1000) as i16).collect();

        let encoded = encoder.encode(&samples);
        assert!(encoded.is_ok());
    }

    #[test]
    fn test_opus_encode_float() {
        let mut encoder = OpusEncoder::new_default().unwrap();
        let frame_size = encoder.input_frame_size();

        let samples: Vec<f32> = (0..frame_size)
            .map(|i| {
                let t = i as f32 / 48000.0;
                (t * 440.0 * 2.0 * std::f32::consts::PI).sin() * 0.5
            })
            .collect();

        let encoded = encoder.encode_float(&samples);
        assert!(encoded.is_ok());
    }

    #[test]
    fn test_f32_i16_conversion() {
        let float_samples = vec![0.0, 0.5, -0.5, 1.0, -1.0];
        let int_samples = f32_to_i16(&float_samples);

        assert_eq!(int_samples[0], 0);
        assert_eq!(int_samples[1], 16383);
        assert_eq!(int_samples[2], -16383);
        assert_eq!(int_samples[3], 32767);
        assert_eq!(int_samples[4], -32767);
    }

    #[test]
    fn test_config_frame_duration() {
        let config = OpusEncoderConfig::default();
        assert_eq!(config.frame_duration_ms(), 20); // 960 samples at 48kHz = 20ms
    }
}
