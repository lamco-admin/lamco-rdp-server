//! Audio Codec Implementations for RDPSND
//!
//! This module provides encoders for the audio codecs supported by RDPSND:
//!
//! - **OPUS** (0x704F): Modern, efficient codec with excellent quality and low latency.
//!   Recommended for modern clients (Windows 10+, FreeRDP, Remmina).
//!
//! - **PCM** (0x0001): Uncompressed audio, universal compatibility but high bandwidth.
//!   Used as fallback when no other codec is supported.
//!
//! - **ADPCM** (0x0002): 4:1 compression, good legacy compatibility.
//!   Supported by older Windows clients.
//!
//! - **G.711 μ-law** (0x0007): Telephony codec, North America/Japan standard.
//! - **G.711 A-law** (0x0006): Telephony codec, European standard.
//!
//! # Codec Priority
//!
//! When negotiating with clients, codecs are offered in this order:
//! 1. OPUS - best quality/bandwidth ratio
//! 2. ADPCM - good compression for legacy
//! 3. PCM - universal fallback
//! 4. G.711 μ-law - telephony fallback
//! 5. G.711 A-law - telephony fallback

pub mod adpcm;
pub mod g711;
pub mod opus;

pub use adpcm::{AdpcmDecoder, AdpcmEncoder};
pub use g711::{AlawEncoder, G711Encoder, G711Variant, MulawEncoder};
pub use opus::{OpusApplication, OpusEncoder, OpusEncoderConfig};

use anyhow::Result;

#[derive(Debug)]
pub enum AudioEncoder {
    Opus(OpusEncoder),
    Pcm(PcmEncoder),
    Adpcm(AdpcmEncoder),
    G711Mulaw(MulawEncoder),
    G711Alaw(AlawEncoder),
}

impl AudioEncoder {
    pub fn opus() -> Result<Self> {
        Ok(Self::Opus(OpusEncoder::new_default()?))
    }

    pub fn opus_with_config(config: OpusEncoderConfig) -> Result<Self> {
        Ok(Self::Opus(OpusEncoder::new(config)?))
    }

    pub fn pcm(channels: usize, sample_rate: u32, bits_per_sample: u16) -> Self {
        Self::Pcm(PcmEncoder::new(channels, sample_rate, bits_per_sample))
    }

    pub fn adpcm(channels: usize, samples_per_block: usize) -> Self {
        Self::Adpcm(AdpcmEncoder::new(channels, samples_per_block))
    }

    pub fn g711_mulaw() -> Self {
        Self::G711Mulaw(MulawEncoder::new())
    }

    pub fn g711_alaw() -> Self {
        Self::G711Alaw(AlawEncoder::new())
    }

    /// Encode PCM samples (i16)
    ///
    /// Note: For OPUS, input must match the frame size. For other codecs,
    /// any size is accepted.
    pub fn encode_i16(&mut self, samples: &[i16]) -> Result<Vec<u8>> {
        match self {
            Self::Opus(enc) => enc.encode(samples),
            Self::Pcm(enc) => Ok(enc.encode(samples)),
            Self::Adpcm(enc) => Ok(enc.encode(samples)),
            Self::G711Mulaw(enc) => Ok(enc.encode(samples)),
            Self::G711Alaw(enc) => Ok(enc.encode(samples)),
        }
    }

    /// Encode PCM samples (f32)
    ///
    /// Converts to i16 for codecs that don't support float input.
    pub fn encode_f32(&mut self, samples: &[f32]) -> Result<Vec<u8>> {
        match self {
            Self::Opus(enc) => enc.encode_float(samples),
            _ => {
                let i16_samples = opus::f32_to_i16(samples);
                self.encode_i16(&i16_samples)
            }
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            Self::Opus(_) => "OPUS",
            Self::Pcm(_) => "PCM",
            Self::Adpcm(_) => "ADPCM",
            Self::G711Mulaw(_) => "G.711 μ-law",
            Self::G711Alaw(_) => "G.711 A-law",
        }
    }

    pub fn format_tag(&self) -> u16 {
        match self {
            Self::Opus(_) => 0x704F,
            Self::Pcm(_) => 0x0001,
            Self::Adpcm(_) => 0x0002,
            Self::G711Mulaw(_) => 0x0007,
            Self::G711Alaw(_) => 0x0006,
        }
    }
}

#[derive(Debug, Clone)]
pub struct PcmEncoder {
    channels: usize,
    sample_rate: u32,
    bits_per_sample: u16,
}

impl PcmEncoder {
    pub fn new(channels: usize, sample_rate: u32, bits_per_sample: u16) -> Self {
        Self {
            channels,
            sample_rate,
            bits_per_sample,
        }
    }

    pub fn new_default() -> Self {
        Self::new(2, 48000, 16)
    }

    pub fn encode(&self, samples: &[i16]) -> Vec<u8> {
        let mut output = Vec::with_capacity(samples.len() * 2);
        for &sample in samples {
            output.extend_from_slice(&sample.to_le_bytes());
        }
        output
    }

    pub fn config(&self) -> (usize, u32, u16) {
        (self.channels, self.sample_rate, self.bits_per_sample)
    }
}

impl Default for PcmEncoder {
    fn default() -> Self {
        Self::new_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_audio_encoder_opus() {
        let encoder = AudioEncoder::opus();
        assert!(encoder.is_ok());
        assert_eq!(encoder.unwrap().name(), "OPUS");
    }

    #[test]
    fn test_audio_encoder_pcm() {
        let mut encoder = AudioEncoder::pcm(2, 48000, 16);
        assert_eq!(encoder.name(), "PCM");

        let samples = vec![0i16, 1000, -1000, 32767, -32768];
        let encoded = encoder.encode_i16(&samples).unwrap();
        assert_eq!(encoded.len(), samples.len() * 2);
    }

    #[test]
    fn test_audio_encoder_g711() {
        let mut encoder = AudioEncoder::g711_mulaw();
        assert_eq!(encoder.format_tag(), 0x0007);

        let samples = vec![0i16, 1000, -1000];
        let encoded = encoder.encode_i16(&samples).unwrap();
        assert_eq!(encoded.len(), samples.len());
    }

    #[test]
    fn test_pcm_encoder_default() {
        let encoder = PcmEncoder::new_default();
        let (channels, rate, bits) = encoder.config();
        assert_eq!(channels, 2);
        assert_eq!(rate, 48000);
        assert_eq!(bits, 16);
    }
}
