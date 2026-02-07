//! G.711 Audio Codec Implementation
//!
//! G.711 is an ITU-T standard for audio companding used in telephony.
//! This module provides both μ-law (North America, Japan) and A-law
//! (Europe, rest of world) encoding.
//!
//! G.711 provides 8:1 compression ratio (16-bit PCM → 8-bit).
//! Typical usage: 8 kHz mono = 64 kbps.
//!
//! References:
//! - ITU-T G.711 specification
//! - RFC 3551 (RTP Audio/Video Profile)

const MULAW_BIAS: i16 = 0x84;
const MULAW_CLIP: i16 = 32635;

/// G.711 μ-law Encoder
///
/// μ-law (mu-law) is the companding standard used in North America and Japan.
/// It provides better dynamic range for low-amplitude signals.
#[derive(Debug, Clone, Default)]
pub struct MulawEncoder;

impl MulawEncoder {
    pub fn new() -> Self {
        Self
    }

    /// Encode a single 16-bit PCM sample to 8-bit μ-law
    ///
    /// The algorithm:
    /// 1. Get sign and magnitude
    /// 2. Add bias and clip
    /// 3. Find segment (leading zeros count)
    /// 4. Combine sign, segment, and quantization bits
    /// 5. Invert all bits
    #[inline]
    pub fn encode_sample(&self, sample: i16) -> u8 {
        // Get sign bit and absolute value
        let sign = if sample < 0 { 0x80u8 } else { 0x00u8 };
        let mut sample = if sample < 0 {
            (-sample).min(MULAW_CLIP)
        } else {
            sample.min(MULAW_CLIP)
        };

        // Add bias
        sample += MULAW_BIAS;

        // Find segment (exponent)
        let exponent = Self::get_exponent(sample);

        // Get mantissa (4 bits)
        let mantissa = ((sample >> (exponent + 3)) & 0x0F) as u8;

        // Combine: sign(1) + exponent(3) + mantissa(4), then invert
        let encoded = sign | ((exponent as u8) << 4) | mantissa;
        !encoded
    }

    pub fn encode(&self, pcm: &[i16]) -> Vec<u8> {
        pcm.iter().map(|&s| self.encode_sample(s)).collect()
    }

    #[inline]
    fn get_exponent(sample: i16) -> u8 {
        // Count leading zeros to find segment
        // Segment 0: 0x00..0xFF
        // Segment 1: 0x100..0x1FF
        // etc.
        let sample = sample as u16;
        match sample {
            0..=0x00FF => 0,
            0x0100..=0x01FF => 1,
            0x0200..=0x03FF => 2,
            0x0400..=0x07FF => 3,
            0x0800..=0x0FFF => 4,
            0x1000..=0x1FFF => 5,
            0x2000..=0x3FFF => 6,
            _ => 7,
        }
    }

    #[inline]
    pub fn decode_sample(&self, ulaw: u8) -> i16 {
        let ulaw = !ulaw;

        let sign = ulaw & 0x80;
        let exponent = ((ulaw >> 4) & 0x07) as i16;
        let mantissa = (ulaw & 0x0F) as i16;

        let mut sample = ((mantissa << 3) + MULAW_BIAS) << exponent;
        sample -= MULAW_BIAS;

        if sign != 0 {
            -sample
        } else {
            sample
        }
    }
}

/// G.711 A-law Encoder
///
/// A-law is the companding standard used in Europe and most of the world
/// outside North America. It has a slightly better signal-to-noise ratio
/// for small signals compared to μ-law.
///
/// This implementation uses lookup tables for ITU-T G.711 compliance,
/// which is the industry-standard approach for accuracy and performance.
#[derive(Debug, Clone, Default)]
pub struct AlawEncoder;

/// A-law encoding lookup table: 16-bit linear PCM -> 8-bit A-law
///
/// This table is generated at compile time following ITU-T G.711.
/// Only the upper 8 bits of the absolute value are used as index,
/// with interpolation for the lower bits via segment/mantissa calculation.
const ALAW_ENCODE_TABLE: [u8; 128] = {
    let mut table = [0u8; 128];
    let mut i = 0i32;
    while i < 128 {
        // Map linear index to A-law value
        // Index represents upper bits of 13-bit magnitude (after >> 6)
        let mut sample = i << 6; // Reconstruct approximate 13-bit value

        let seg: u8;
        let mant: u8;

        if sample < 32 {
            seg = 0;
            mant = (sample >> 1) as u8;
        } else if sample < 64 {
            seg = 1;
            mant = ((sample - 32) >> 1) as u8;
        } else if sample < 128 {
            seg = 2;
            mant = ((sample - 64) >> 2) as u8;
        } else if sample < 256 {
            seg = 3;
            mant = ((sample - 128) >> 3) as u8;
        } else if sample < 512 {
            seg = 4;
            mant = ((sample - 256) >> 4) as u8;
        } else if sample < 1024 {
            seg = 5;
            mant = ((sample - 512) >> 5) as u8;
        } else if sample < 2048 {
            seg = 6;
            mant = ((sample - 1024) >> 6) as u8;
        } else {
            seg = 7;
            mant = ((sample - 2048) >> 7) as u8;
        }

        // Combine segment and mantissa, XOR with 0x55
        table[i as usize] = ((seg << 4) | (mant & 0x0F)) ^ 0x55;
        i += 1;
    }
    table
};

/// A-law decoding lookup table: 8-bit A-law -> 16-bit linear PCM
///
/// Pre-computed decode values for all 256 possible A-law inputs.
const ALAW_DECODE_TABLE: [i16; 256] = {
    let mut table = [0i16; 256];
    let mut i = 0u32;
    while i < 256 {
        // Remove XOR encoding
        let alaw = (i as u8) ^ 0x55;

        // Extract components
        let sign = (alaw & 0x80) != 0;
        let seg = (alaw >> 4) & 0x07;
        let mant = alaw & 0x0F;

        // Reconstruct linear value based on segment
        // A-law segments have specific step sizes
        let mut linear: i32 = match seg {
            0 => ((mant as i32) << 1) + 1,
            1 => ((mant as i32) << 1) + 1 + 32,
            _ => {
                // For segments 2-7: base + mantissa contribution
                let base = 1i32 << (seg + 4); // 64, 128, 256, 512, 1024, 2048
                let step = 1i32 << (seg - 1); // 2, 4, 8, 16, 32, 64
                base + ((mant as i32) << 1) * step + step
            }
        };

        // Convert to 16-bit scale (multiply by 8 for 13->16 bit)
        linear <<= 3;

        // Apply sign
        if sign {
            linear = -linear;
        }

        // Clamp to i16 range
        table[i as usize] = if linear > 32767 {
            32767
        } else if linear < -32768 {
            -32768
        } else {
            linear as i16
        };

        i += 1;
    }
    table
};

impl AlawEncoder {
    pub fn new() -> Self {
        Self
    }

    /// Encode a single 16-bit PCM sample to 8-bit A-law
    ///
    /// Uses the standard ITU-T G.711 A-law algorithm:
    /// 1. Get sign bit and 13-bit magnitude
    /// 2. Find segment based on magnitude range
    /// 3. Extract 4-bit mantissa within segment
    /// 4. Combine: sign(1) | segment(3) | mantissa(4)
    /// 5. XOR with 0x55 for transmission
    #[inline]
    pub fn encode_sample(&self, sample: i16) -> u8 {
        // Handle sign and get magnitude
        let (sign, mag) = if sample >= 0 {
            (0x00u8, sample as i32)
        } else {
            // Handle i16::MIN specially to avoid overflow
            (
                0x80u8,
                if sample == -32768 {
                    32767
                } else {
                    -sample as i32
                },
            )
        };

        // Convert 16-bit to 13-bit (shift right 3)
        let mag = (mag >> 3).min(4095) as i32;

        // Find segment and mantissa based on magnitude
        let (seg, mant): (u8, u8) = if mag < 32 {
            // Segment 0: linear region, step = 2
            (0, (mag >> 1) as u8)
        } else if mag < 64 {
            // Segment 1: linear region, step = 2
            (1, ((mag - 32) >> 1) as u8)
        } else if mag < 128 {
            // Segment 2: step = 4
            (2, ((mag - 64) >> 2) as u8)
        } else if mag < 256 {
            // Segment 3: step = 8
            (3, ((mag - 128) >> 3) as u8)
        } else if mag < 512 {
            // Segment 4: step = 16
            (4, ((mag - 256) >> 4) as u8)
        } else if mag < 1024 {
            // Segment 5: step = 32
            (5, ((mag - 512) >> 5) as u8)
        } else if mag < 2048 {
            // Segment 6: step = 64
            (6, ((mag - 1024) >> 6) as u8)
        } else {
            // Segment 7: step = 128, clamped
            (7, (((mag - 2048) >> 7) as u8).min(15))
        };

        // Combine: sign | segment | mantissa, then XOR with 0x55
        (sign | (seg << 4) | (mant & 0x0F)) ^ 0x55
    }

    pub fn encode(&self, pcm: &[i16]) -> Vec<u8> {
        pcm.iter().map(|&s| self.encode_sample(s)).collect()
    }

    #[inline]
    pub fn decode_sample(&self, alaw: u8) -> i16 {
        ALAW_DECODE_TABLE[alaw as usize]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum G711Variant {
    Mulaw,
    Alaw,
}

#[derive(Debug, Clone)]
pub enum G711Encoder {
    Mulaw(MulawEncoder),
    Alaw(AlawEncoder),
}

impl G711Encoder {
    pub fn new(variant: G711Variant) -> Self {
        match variant {
            G711Variant::Mulaw => Self::Mulaw(MulawEncoder::new()),
            G711Variant::Alaw => Self::Alaw(AlawEncoder::new()),
        }
    }

    pub fn encode(&self, pcm: &[i16]) -> Vec<u8> {
        match self {
            Self::Mulaw(enc) => enc.encode(pcm),
            Self::Alaw(enc) => enc.encode(pcm),
        }
    }

    pub fn variant(&self) -> G711Variant {
        match self {
            Self::Mulaw(_) => G711Variant::Mulaw,
            Self::Alaw(_) => G711Variant::Alaw,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mulaw_encode_decode_roundtrip() {
        let encoder = MulawEncoder::new();

        // Test various sample values
        let test_samples: [i16; 7] = [0, 100, 1000, 10000, -100, -1000, -10000];

        for &original in &test_samples {
            let encoded = encoder.encode_sample(original);
            let decoded = encoder.decode_sample(encoded);

            // G.711 is lossy, so we check that the decoded value is close
            // The error should be within the quantization range
            let error = (original - decoded).abs();
            assert!(
                error < 1000,
                "μ-law roundtrip error too large: {} -> {} -> {} (error: {})",
                original,
                encoded,
                decoded,
                error
            );
        }
    }

    #[test]
    fn test_alaw_encode_decode_roundtrip() {
        let encoder = AlawEncoder::new();

        let test_samples: [i16; 7] = [0, 100, 1000, 10000, -100, -1000, -10000];

        for &original in &test_samples {
            let encoded = encoder.encode_sample(original);
            let decoded = encoder.decode_sample(encoded);

            let error = (original - decoded).abs();
            assert!(
                error < 1000,
                "A-law roundtrip error too large: {} -> {} -> {} (error: {})",
                original,
                encoded,
                decoded,
                error
            );
        }
    }

    #[test]
    fn test_mulaw_silence() {
        let encoder = MulawEncoder::new();
        // Silence (0) should encode to a known value
        let encoded = encoder.encode_sample(0);
        // μ-law silence is typically 0xFF
        assert_eq!(encoded, 0xFF);
    }

    #[test]
    fn test_encode_buffer() {
        let encoder = G711Encoder::new(G711Variant::Mulaw);
        let samples: Vec<i16> = vec![0, 1000, -1000, 16000, -16000];
        let encoded = encoder.encode(&samples);
        assert_eq!(encoded.len(), samples.len());
    }
}
