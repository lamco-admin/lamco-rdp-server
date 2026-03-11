//! PipeWire Audio Capture
//!
//! Re-exports audio capture from lamco-pipewire. All PipeWire audio
//! capture functionality lives in the lamco-pipewire crate; this module
//! provides the same public API for server consumers.

pub use lamco_pipewire::audio::{
    AudioCapture, AudioCaptureHandle, AudioFormat, AudioSamples, CaptureConfig, spawn_audio_capture,
};

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
}
