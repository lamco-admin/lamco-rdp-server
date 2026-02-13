//! Audio Pipeline Manager
//!
//! Orchestrates the complete audio streaming pipeline:
//!
//! ```text
//! PipeWire Capture → Encoder → RDPSND Server → RDP Client
//! ```
//!
//! The pipeline runs as an async task, receiving audio samples from
//! PipeWire and sending encoded audio through the RDPSND channel.

use std::{sync::Arc, time::Instant};

use anyhow::{Context, Result};
use ironrdp_rdpsnd::server::RdpsndServer;
use ironrdp_svc::SvcMessage;
use tokio::sync::{mpsc, Mutex, RwLock};
use tracing::{debug, error, info, warn};

use crate::audio::{
    capture::{AudioSamples, CaptureConfig},
    codecs::AudioEncoder,
};

#[derive(Debug, Clone)]
pub struct PipelineConfig {
    pub capture: CaptureConfig,
    pub target_latency_ms: u32,
    pub opus_frame_size: usize,
    pub adaptive_bitrate: bool,
    pub initial_bitrate: u32,
}

impl Default for PipelineConfig {
    fn default() -> Self {
        Self {
            capture: CaptureConfig::default(),
            target_latency_ms: 100,
            opus_frame_size: 960, // 20ms at 48kHz
            adaptive_bitrate: true,
            initial_bitrate: 96000,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PipelineState {
    Idle,
    Starting,
    Running,
    Paused,
    Stopping,
    Stopped,
}

pub struct AudioPipeline {
    config: PipelineConfig,
    state: Arc<RwLock<PipelineState>>,
    encoder: Option<AudioEncoder>,
    audio_timestamp: u32,
    frame_duration_ms: u32,
    sample_rx: Option<mpsc::Receiver<AudioSamples>>,
    svc_tx: Option<mpsc::Sender<Vec<SvcMessage>>>,
    sample_buffer: Vec<i16>,
    samples_per_frame: usize,
    stats: PipelineStats,
}

#[derive(Debug, Clone, Default)]
pub struct PipelineStats {
    pub frames_encoded: u64,
    pub bytes_sent: u64,
    pub frames_dropped: u64,
    pub current_bitrate: u32,
    pub avg_encode_time_us: u64,
}

impl AudioPipeline {
    pub fn new(config: PipelineConfig) -> Self {
        let frame_duration_ms = (config.opus_frame_size as u32 * 1000) / config.capture.sample_rate;
        let samples_per_frame = config.opus_frame_size * config.capture.channels as usize;

        Self {
            config,
            state: Arc::new(RwLock::new(PipelineState::Idle)),
            encoder: None,
            audio_timestamp: 0,
            frame_duration_ms,
            sample_rx: None,
            svc_tx: None,
            sample_buffer: Vec::with_capacity(samples_per_frame * 4),
            samples_per_frame,
            stats: PipelineStats::default(),
        }
    }

    pub fn set_sample_receiver(&mut self, rx: mpsc::Receiver<AudioSamples>) {
        self.sample_rx = Some(rx);
    }

    pub fn set_svc_sender(&mut self, tx: mpsc::Sender<Vec<SvcMessage>>) {
        self.svc_tx = Some(tx);
    }

    pub fn set_encoder(&mut self, encoder: AudioEncoder) {
        self.encoder = Some(encoder);
    }

    pub async fn state(&self) -> PipelineState {
        *self.state.read().await
    }

    pub fn stats(&self) -> &PipelineStats {
        &self.stats
    }

    pub async fn run(&mut self, rdpsnd: Arc<Mutex<RdpsndServer>>) -> Result<()> {
        let sample_rx = self
            .sample_rx
            .take()
            .context("Sample receiver not configured")?;

        let svc_tx = self.svc_tx.as_ref().context("SVC sender not configured")?;

        let encoder = self.encoder.as_mut().context("Encoder not configured")?;

        *self.state.write().await = PipelineState::Running;
        info!(
            "Audio pipeline starting: {}ms frames, {} samples/frame",
            self.frame_duration_ms, self.samples_per_frame
        );

        let mut sample_rx = sample_rx;

        loop {
            let state = *self.state.read().await;
            if state == PipelineState::Stopping || state == PipelineState::Stopped {
                break;
            }

            match sample_rx.recv().await {
                Some(samples) => {
                    let i16_samples = samples.to_i16();
                    self.sample_buffer.extend_from_slice(&i16_samples);

                    while self.sample_buffer.len() >= self.samples_per_frame {
                        let frame: Vec<i16> =
                            self.sample_buffer.drain(..self.samples_per_frame).collect();

                        let start = Instant::now();

                        match encoder.encode_i16(&frame) {
                            Ok(encoded) => {
                                let encode_time = start.elapsed().as_micros() as u64;
                                self.stats.avg_encode_time_us =
                                    (self.stats.avg_encode_time_us * 7 + encode_time) / 8;

                                let timestamp = self.audio_timestamp;
                                self.audio_timestamp =
                                    self.audio_timestamp.wrapping_add(self.frame_duration_ms);

                                let mut rdpsnd_guard = rdpsnd.lock().await;
                                match rdpsnd_guard.wave(encoded.clone(), timestamp) {
                                    Ok(messages) => {
                                        let svc_messages: Vec<SvcMessage> = messages.into();
                                        if let Err(e) = svc_tx.try_send(svc_messages) {
                                            if matches!(e, mpsc::error::TrySendError::Full(_)) {
                                                self.stats.frames_dropped += 1;
                                                debug!("Audio SVC channel full, dropping frame");
                                            } else {
                                                error!("Audio SVC channel closed");
                                                break;
                                            }
                                        } else {
                                            self.stats.frames_encoded += 1;
                                            self.stats.bytes_sent += encoded.len() as u64;
                                        }
                                    }
                                    Err(e) => {
                                        warn!("RDPSND wave error: {}", e);
                                    }
                                }
                            }
                            Err(e) => {
                                warn!("Audio encoding error: {}", e);
                            }
                        }
                    }
                }
                None => {
                    info!("Audio sample channel closed");
                    break;
                }
            }
        }

        *self.state.write().await = PipelineState::Stopped;
        info!(
            "Audio pipeline stopped: {} frames encoded, {} bytes sent, {} dropped",
            self.stats.frames_encoded, self.stats.bytes_sent, self.stats.frames_dropped
        );

        Ok(())
    }

    pub async fn stop(&self) {
        *self.state.write().await = PipelineState::Stopping;
    }
}

pub struct FrameBuffer {
    samples: Vec<i16>,
    samples_per_frame: usize,
    channels: usize,
}

impl FrameBuffer {
    pub fn new(frame_size: usize, channels: usize) -> Self {
        let samples_per_frame = frame_size * channels;
        Self {
            samples: Vec::with_capacity(samples_per_frame * 4),
            samples_per_frame,
            channels,
        }
    }

    pub fn push(&mut self, samples: &[i16]) {
        self.samples.extend_from_slice(samples);
    }

    pub fn has_frame(&self) -> bool {
        self.samples.len() >= self.samples_per_frame
    }

    pub fn pop_frame(&mut self) -> Option<Vec<i16>> {
        if self.has_frame() {
            let frame: Vec<i16> = self.samples.drain(..self.samples_per_frame).collect();
            Some(frame)
        } else {
            None
        }
    }

    pub fn clear(&mut self) {
        self.samples.clear();
    }

    pub fn len(&self) -> usize {
        self.samples.len()
    }

    pub fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pipeline_config_default() {
        let config = PipelineConfig::default();
        assert_eq!(config.capture.sample_rate, 48000);
        assert_eq!(config.opus_frame_size, 960);
        assert_eq!(config.initial_bitrate, 96000);
    }

    #[test]
    fn test_frame_buffer() {
        let mut buffer = FrameBuffer::new(960, 2);
        assert!(!buffer.has_frame());

        // Push partial frame
        let samples: Vec<i16> = (0..1000).collect();
        buffer.push(&samples);
        assert!(!buffer.has_frame());

        // Push more to complete frame
        let more: Vec<i16> = (0..920).collect();
        buffer.push(&more);
        assert!(buffer.has_frame());

        let frame = buffer.pop_frame().unwrap();
        assert_eq!(frame.len(), 1920); // 960 * 2 channels
    }

    #[test]
    fn test_frame_duration() {
        let config = PipelineConfig::default();
        let pipeline = AudioPipeline::new(config);
        assert_eq!(pipeline.frame_duration_ms, 20); // 960 samples at 48kHz = 20ms
    }
}
