//! EGFX Video Handler
//!
//! Bridges PipeWire video frames to the EGFX H.264 encoding pipeline.
//!
//! # Architecture
//!
//! ```text
//! PipeWire Frames
//!        │
//!        ├─> VideoFrame (BGRA data)
//!        │
//!        ▼
//! EgfxVideoHandler
//!        │
//!        ├─> H264Encoder (BGRA → H.264)
//!        │
//!        ▼
//! EGFX PDUs (WireToSurface)
//!        │
//!        ├─> DVC Channel Queue
//!        │
//!        ▼
//! RDP Client (H.264 decode)
//! ```
//!
//! # Integration
//!
//! This handler receives frames from PipeWire and encodes them for EGFX delivery.
//! It can run in parallel with the RemoteFX path, with the server choosing
//! which encoding to use based on client capabilities and frame characteristics.

use std::time::Instant;

#[cfg(feature = "h264")]
use tokio::sync::Mutex;
use tokio::sync::{mpsc, RwLock};
use tracing::{debug, error, info, trace, warn};

#[cfg(feature = "h264")]
use crate::egfx::encoder::{Avc420Encoder, EncoderConfig};
use crate::{
    egfx::encoder::{EncoderError, EncoderResult},
    pipewire::VideoFrame,
};

/// Configuration for EGFX video handling
#[derive(Debug, Clone)]
pub struct EgfxVideoConfig {
    /// Target bitrate in kbps
    pub bitrate_kbps: u32,

    /// Maximum frames per second
    pub max_fps: u32,

    /// Enable frame skipping under load
    pub enable_frame_skip: bool,

    /// Quality vs speed tradeoff (0=fastest, 100=best quality)
    pub quality_preset: u32,

    /// Enable AVC 444 mode (full chroma)
    pub avc_444_mode: bool,
}

impl Default for EgfxVideoConfig {
    fn default() -> Self {
        Self {
            bitrate_kbps: 5000,
            max_fps: 30,
            enable_frame_skip: true,
            quality_preset: 50,
            avc_444_mode: false,
        }
    }
}

/// Encoded frame ready for EGFX transmission
#[derive(Debug)]
pub struct EncodedFrame {
    /// H.264 NAL units
    pub h264_data: Vec<u8>,

    /// Frame timestamp (milliseconds since session start)
    pub timestamp_ms: u64,

    /// Is this a keyframe (IDR)
    pub is_keyframe: bool,

    /// Frame dimensions
    pub width: u32,
    pub height: u32,

    /// Encoding time in microseconds
    pub encode_time_us: u64,
}

/// Statistics for video encoding
#[derive(Debug, Default, Clone)]
pub struct EncodingStats {
    /// Frames encoded
    pub frames_encoded: u64,

    /// Frames dropped due to backpressure
    pub frames_dropped: u64,

    /// Keyframes generated
    pub keyframes: u64,

    /// Total bytes encoded
    pub total_bytes: u64,

    /// Average encode time in microseconds
    pub avg_encode_time_us: u64,

    /// Peak encode time in microseconds
    pub peak_encode_time_us: u64,
}

/// EGFX Video Handler
///
/// Receives video frames and produces H.264 encoded data for EGFX transmission.
pub struct EgfxVideoHandler {
    /// Encoder configuration
    config: EgfxVideoConfig,

    /// H.264 encoder instance (behind feature flag)
    #[cfg(feature = "h264")]
    encoder: Mutex<Avc420Encoder>,

    /// Encoding statistics
    stats: RwLock<EncodingStats>,

    /// Session start time for timestamps
    session_start: Instant,

    /// Output channel for encoded frames
    output_tx: mpsc::Sender<EncodedFrame>,

    /// Current surface dimensions
    surface_size: RwLock<(u32, u32)>,

    /// Frame counter for sequence tracking
    frame_counter: std::sync::atomic::AtomicU64,
}

impl EgfxVideoHandler {
    #[cfg(feature = "h264")]
    pub fn new(
        config: EgfxVideoConfig,
        initial_width: u32,
        initial_height: u32,
        output_tx: mpsc::Sender<EncodedFrame>,
    ) -> EncoderResult<Self> {
        // Configure encoder from video config
        let encoder_config = EncoderConfig {
            bitrate_kbps: config.bitrate_kbps,
            max_fps: config.max_fps as f32,
            enable_skip_frame: config.enable_frame_skip,
            width: Some(initial_width as u16),
            height: Some(initial_height as u16),
            color_space: None,    // Auto-select based on resolution
            ..Default::default()  // QP defaults
        };

        let encoder = Avc420Encoder::new(encoder_config)?;

        Ok(Self {
            config,
            encoder: Mutex::new(encoder),
            stats: RwLock::new(EncodingStats::default()),
            session_start: Instant::now(),
            output_tx,
            surface_size: RwLock::new((initial_width, initial_height)),
            frame_counter: std::sync::atomic::AtomicU64::new(0),
        })
    }

    /// Stub implementation when H.264 feature is disabled
    #[cfg(not(feature = "h264"))]
    pub fn new(
        _config: EgfxVideoConfig,
        _initial_width: u32,
        _initial_height: u32,
        _output_tx: mpsc::Sender<EncodedFrame>,
    ) -> EncoderResult<Self> {
        Err(EncoderError::InitFailed(
            "H.264 support not compiled in (enable 'h264' feature)".to_string(),
        ))
    }

    #[cfg(feature = "h264")]
    pub async fn process_frame(&self, frame: VideoFrame) -> EncoderResult<bool> {
        let frame_num = self
            .frame_counter
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let timestamp_ms = self.session_start.elapsed().as_millis() as u64;

        let (current_w, current_h) = *self.surface_size.read().await;
        if frame.width != current_w || frame.height != current_h {
            info!(
                "Surface resize: {}x{} -> {}x{}",
                current_w, current_h, frame.width, frame.height
            );
            *self.surface_size.write().await = (frame.width, frame.height);
            // Encoder will handle resize internally
        }

        let encode_start = Instant::now();

        let encoded = {
            let mut encoder = self.encoder.lock().await;
            encoder.encode_bgra(&frame.data, frame.width, frame.height, timestamp_ms)?
        };

        let encode_time_us = encode_start.elapsed().as_micros() as u64;

        {
            let mut stats = self.stats.write().await;
            stats.frames_encoded += 1;
            stats.peak_encode_time_us = stats.peak_encode_time_us.max(encode_time_us);

            // Running average
            let n = stats.frames_encoded as f64;
            stats.avg_encode_time_us =
                ((stats.avg_encode_time_us as f64 * (n - 1.0) + encode_time_us as f64) / n) as u64;
        }

        let h264_frame = match encoded {
            Some(f) => f,
            None => {
                trace!("Frame {} skipped by encoder", frame_num);
                return Ok(false);
            }
        };

        // Update stats for keyframes and bytes
        {
            let mut stats = self.stats.write().await;
            if h264_frame.is_keyframe {
                stats.keyframes += 1;
            }
            stats.total_bytes += h264_frame.data.len() as u64;
        }

        let encoded_frame = EncodedFrame {
            h264_data: h264_frame.data,
            timestamp_ms,
            is_keyframe: h264_frame.is_keyframe,
            width: frame.width,
            height: frame.height,
            encode_time_us,
        };

        // Send to output channel (non-blocking)
        match self.output_tx.try_send(encoded_frame) {
            Ok(()) => {
                if frame_num % 30 == 0 {
                    debug!(
                        "📹 Encoded frame {} in {}us, keyframe={}",
                        frame_num, encode_time_us, h264_frame.is_keyframe
                    );
                }
                Ok(true)
            }
            Err(mpsc::error::TrySendError::Full(_)) => {
                let mut stats = self.stats.write().await;
                stats.frames_dropped += 1;
                warn!("EGFX output queue full - dropping frame {}", frame_num);
                Ok(false)
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                error!("EGFX output channel closed");
                Err(EncoderError::EncodeFailed(
                    "Output channel closed".to_string(),
                ))
            }
        }
    }

    /// Stub when H.264 feature disabled
    #[cfg(not(feature = "h264"))]
    pub async fn process_frame(&self, _frame: VideoFrame) -> EncoderResult<bool> {
        Err(EncoderError::InitFailed(
            "H.264 support not compiled in".to_string(),
        ))
    }

    /// Request a keyframe (IDR frame) on next encode
    #[cfg(feature = "h264")]
    pub async fn force_keyframe(&self) {
        let mut encoder = self.encoder.lock().await;
        encoder.force_keyframe();
        debug!("Keyframe requested");
    }

    /// Stub when H.264 feature disabled
    #[cfg(not(feature = "h264"))]
    pub async fn force_keyframe(&self) {}

    pub async fn get_stats(&self) -> EncodingStats {
        self.stats.read().await.clone()
    }

    pub async fn reset_stats(&self) {
        *self.stats.write().await = EncodingStats::default();
    }

    pub async fn surface_size(&self) -> (u32, u32) {
        *self.surface_size.read().await
    }
}

pub(super) struct EgfxVideoHandlerFactory {
    config: EgfxVideoConfig,
}

impl EgfxVideoHandlerFactory {
    pub(super) fn new(config: EgfxVideoConfig) -> Self {
        Self { config }
    }

    pub(super) fn build_handler(
        &self,
        #[cfg_attr(not(feature = "h264"), allow(unused_variables))] width: u32,
        #[cfg_attr(not(feature = "h264"), allow(unused_variables))] height: u32,
    ) -> Option<mpsc::Receiver<EncodedFrame>> {
        #[cfg(feature = "h264")]
        {
            let (tx, rx) = mpsc::channel(16);

            match EgfxVideoHandler::new(self.config.clone(), width, height, tx) {
                Ok(_handler) => {
                    info!(
                        "EGFX video handler created: {}x{}, {}kbps",
                        width, height, self.config.bitrate_kbps
                    );
                    Some(rx)
                }
                Err(e) => {
                    error!("Failed to create EGFX video handler: {:?}", e);
                    None
                }
            }
        }

        #[cfg(not(feature = "h264"))]
        {
            warn!("EGFX video handler unavailable: H.264 feature not enabled");
            None
        }
    }

    pub(super) fn is_available(&self) -> bool {
        cfg!(feature = "h264")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_egfx_video_config_default() {
        let config = EgfxVideoConfig::default();
        assert_eq!(config.bitrate_kbps, 5000);
        assert_eq!(config.max_fps, 30);
        assert!(config.enable_frame_skip);
    }

    #[test]
    fn test_factory_availability() {
        let factory = EgfxVideoHandlerFactory::new(EgfxVideoConfig::default());
        // Availability depends on feature flag
        #[cfg(feature = "h264")]
        assert!(factory.is_available());
        #[cfg(not(feature = "h264"))]
        assert!(!factory.is_available());
    }

    #[cfg(feature = "h264")]
    #[tokio::test]
    async fn test_handler_creation() {
        let (tx, _rx) = mpsc::channel(16);
        let config = EgfxVideoConfig::default();

        let handler = EgfxVideoHandler::new(config, 1920, 1080, tx);

        // May fail if OpenH264 not available
        if let Ok(handler) = handler {
            let size = handler.surface_size().await;
            assert_eq!(size, (1920, 1080));
        }
    }

    #[tokio::test]
    async fn test_stats_tracking() {
        let stats = EncodingStats::default();
        assert_eq!(stats.frames_encoded, 0);
        assert_eq!(stats.frames_dropped, 0);
    }
}
