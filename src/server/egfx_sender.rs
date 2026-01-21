//! EGFX Frame Sender
//!
//! Handles sending H.264 encoded frames through the EGFX channel.
//!
//! # Architecture
//!
//! This module bridges the H.264 encoder output to the IronRDP EGFX pipeline:
//!
//! ```text
//! H.264 NAL data (from Avc420Encoder)
//!        │
//!        ├─► EgfxFrameSender
//!        │     ├─► send_avc420_frame() on GraphicsPipelineServer
//!        │     ├─► drain_output() → Vec<DvcMessage>
//!        │     ├─► encode_dvc_messages() → Vec<SvcMessage>
//!        │     │
//!        │     ▼
//!        │   ServerEvent::Egfx(SendMessages)
//!        │     │
//!        ▼     ▼
//! IronRDP Server event loop → Wire → RDP Client
//! ```
//!
//! # API Boundaries
//!
//! This module uses IronRDP types internally but exposes a clean API.
//! The display handler doesn't need to know about EGFX protocol details.

use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{debug, trace, warn};

// IronRDP types - used internally only
use ironrdp_dvc::encode_dvc_messages;
use ironrdp_egfx::pdu::Avc420Region;
use ironrdp_server::{EgfxServerMessage, GfxServerHandle, ServerEvent};
use ironrdp_svc::ChannelFlags;

use crate::damage::DamageRegion;
use crate::server::gfx_factory::HandlerState;

/// Result type for frame sending operations
pub type SendResult<T> = Result<T, SendError>;

/// Errors that can occur when sending frames
#[derive(Debug)]
pub enum SendError {
    /// EGFX channel not ready (capability negotiation incomplete)
    NotReady,
    /// AVC420 codec not supported by client
    Avc420NotSupported,
    /// No primary surface available
    NoSurface,
    /// Frame dropped due to backpressure
    Backpressure,
    /// Server event channel closed
    ChannelClosed,
    /// DVC message encoding failed
    EncodingFailed(String),
    /// Lock acquisition failed
    LockFailed,
}

impl std::fmt::Display for SendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SendError::NotReady => write!(f, "EGFX channel not ready"),
            SendError::Avc420NotSupported => write!(f, "AVC420 not supported by client"),
            SendError::NoSurface => write!(f, "No primary surface available"),
            SendError::Backpressure => write!(f, "Frame dropped due to backpressure"),
            SendError::ChannelClosed => write!(f, "Server event channel closed"),
            SendError::EncodingFailed(e) => write!(f, "DVC encoding failed: {}", e),
            SendError::LockFailed => write!(f, "Failed to acquire lock"),
        }
    }
}

impl std::error::Error for SendError {}

/// EGFX Frame Sender
///
/// Sends H.264 encoded frames through the EGFX channel to RDP clients.
/// Supports both AVC420 and AVC444 codecs.
///
/// # Channel ID
///
/// The DVC channel_id is now stored in `GraphicsPipelineServer` and queried
/// at frame send time via `GfxServerHandle`. This eliminates the need for
/// external channel_id propagation.
///
/// # Codec Support
///
/// - **AVC420**: Single H.264 stream with 4:2:0 chroma (standard)
/// - **AVC444**: Dual H.264 streams with 4:4:4 chroma (premium)
///
/// # Usage
///
/// ```ignore
/// let sender = EgfxFrameSender::new(gfx_handle, handler_state, event_tx);
///
/// // Check if ready before sending
/// if sender.is_ready().await {
///     // For AVC420
///     sender.send_frame(&h264_data, width, height, timestamp_ms).await?;
///
///     // For AVC444
///     sender.send_avc444_frame(&stream1, &stream2, width, height, timestamp_ms).await?;
/// }
/// ```
pub struct EgfxFrameSender {
    /// Handle to the GraphicsPipelineServer for sending frames
    /// Also used to query channel_id via server.channel_id()
    gfx_server: GfxServerHandle,

    /// Handler state for checking readiness (codec support, surface availability)
    handler_state: Arc<tokio::sync::RwLock<Option<HandlerState>>>,

    /// Channel for sending server events (unbounded for backpressure-free EGFX)
    event_tx: mpsc::UnboundedSender<ServerEvent>,

    /// Frame counter for debugging
    frame_count: std::sync::atomic::AtomicU64,
}

impl EgfxFrameSender {
    /// Create a new EGFX frame sender
    ///
    /// # Arguments
    ///
    /// * `gfx_server` - Handle to the shared GraphicsPipelineServer
    /// * `handler_state` - Shared handler state for readiness checks
    /// * `event_tx` - Channel for sending ServerEvent::Egfx messages
    pub fn new(
        gfx_server: GfxServerHandle,
        handler_state: Arc<tokio::sync::RwLock<Option<HandlerState>>>,
        event_tx: mpsc::UnboundedSender<ServerEvent>,
    ) -> Self {
        Self {
            gfx_server,
            handler_state,
            event_tx,
            frame_count: std::sync::atomic::AtomicU64::new(0),
        }
    }

    /// Check if EGFX is ready and AVC420 is supported
    pub async fn is_ready(&self) -> bool {
        if let Some(state) = self.handler_state.read().await.as_ref() {
            state.is_ready && state.is_avc420_enabled
        } else {
            false
        }
    }

    /// Check if only EGFX is ready (regardless of codec)
    pub async fn is_egfx_ready(&self) -> bool {
        if let Some(state) = self.handler_state.read().await.as_ref() {
            state.is_ready
        } else {
            false
        }
    }

    /// Get the primary surface ID
    pub async fn primary_surface_id(&self) -> Option<u16> {
        self.handler_state
            .read()
            .await
            .as_ref()
            .and_then(|state| state.primary_surface_id)
    }

    /// Send an H.264 encoded frame through EGFX
    ///
    /// # Arguments
    ///
    /// * `h264_data` - H.264 NAL units (Annex B format with start codes)
    /// * `encoded_width` - Width used for H.264 encoding (MUST be aligned to 16)
    /// * `encoded_height` - Height used for H.264 encoding (MUST be aligned to 16)
    /// * `display_width` - Actual width to display (may be < encoded_width)
    /// * `display_height` - Actual height to display (may be < encoded_height)
    /// * `timestamp_ms` - Frame timestamp in milliseconds
    ///
    /// # Returns
    ///
    /// `Ok(frame_id)` if the frame was sent successfully, or an error.
    ///
    /// # Note
    ///
    /// The encoded dimensions must be 16-pixel aligned per MS-RDPEGFX spec.
    /// The display dimensions specify the visible region (DestRect) for cropping.
    pub async fn send_frame(
        &self,
        h264_data: &[u8],
        encoded_width: u16,
        encoded_height: u16,
        display_width: u16,
        display_height: u16,
        timestamp_ms: u32,
    ) -> SendResult<u32> {
        // Check readiness
        let state = self
            .handler_state
            .read()
            .await
            .as_ref()
            .cloned()
            .ok_or(SendError::NotReady)?;

        if !state.is_ready {
            return Err(SendError::NotReady);
        }

        if !state.is_avc420_enabled {
            return Err(SendError::Avc420NotSupported);
        }

        let surface_id = state.primary_surface_id.ok_or(SendError::NoSurface)?;

        // Debug: Parse and log ALL NAL units in the frame (Annex B format)
        {
            let mut offset = 0usize;
            let mut nal_count = 0;
            let mut nal_types = Vec::new();

            while offset < h264_data.len() {
                // Find start code (00 00 00 01 or 00 00 01)
                let start_code_len = if offset + 4 <= h264_data.len()
                    && h264_data[offset..offset + 4] == [0x00, 0x00, 0x00, 0x01]
                {
                    4
                } else if offset + 3 <= h264_data.len()
                    && h264_data[offset..offset + 3] == [0x00, 0x00, 0x01]
                {
                    3
                } else {
                    offset += 1;
                    continue;
                };

                let nal_start = offset + start_code_len;

                // Find next start code to determine NAL length
                let mut nal_end = h264_data.len();
                for j in (nal_start + 1)..h264_data.len().saturating_sub(2) {
                    if h264_data[j..].starts_with(&[0x00, 0x00, 0x01]) {
                        // Check if it's a 4-byte start code
                        if j > 0 && h264_data[j - 1] == 0x00 {
                            nal_end = j - 1;
                        } else {
                            nal_end = j;
                        }
                        break;
                    }
                }

                if nal_start < h264_data.len() {
                    let nal_header = h264_data[nal_start];
                    let nal_type = nal_header & 0x1f;
                    let nal_ref_idc = (nal_header >> 5) & 0x03;
                    let nal_len = nal_end - nal_start;

                    let type_name = match nal_type {
                        1 => "P-slice",
                        5 => "IDR",
                        6 => "SEI",
                        7 => "SPS",
                        8 => "PPS",
                        9 => "AUD",
                        _ => "Other",
                    };

                    // For SPS/PPS, log first few bytes for debugging
                    if nal_type == 7 || nal_type == 8 {
                        let preview_len = std::cmp::min(16, nal_len);
                        let preview: Vec<String> = h264_data[nal_start..nal_start + preview_len]
                            .iter()
                            .map(|b| format!("{:02x}", b))
                            .collect();
                        nal_types.push(format!(
                            "{}({}b,ref={})[{}]",
                            type_name,
                            nal_len,
                            nal_ref_idc,
                            preview.join(" ")
                        ));
                    } else {
                        nal_types.push(format!("{}({}b,ref={})", type_name, nal_len, nal_ref_idc));
                    }

                    nal_count += 1;

                    if nal_count >= 10 {
                        nal_types.push("...".to_string());
                        break;
                    }
                }

                offset = nal_end;
            }

            trace!(
                "EGFX: Frame NAL units ({}): [{}]",
                nal_count,
                nal_types.join(", ")
            );
            trace!(
                "EGFX: Total H.264 data size: {} bytes (Annex B format)",
                h264_data.len()
            );
        }

        // DEBUG: Dump first 3 frames to files for validation
        // Use a static counter since timestamp_ms might be large
        use std::sync::atomic::{AtomicU32, Ordering};
        static FRAME_DUMP_COUNT: AtomicU32 = AtomicU32::new(0);

        let dump_count = FRAME_DUMP_COUNT.fetch_add(1, Ordering::SeqCst);
        if dump_count < 3 {
            use std::io::Write;
            let filename = format!("/tmp/rdp-frame-{}.h264", dump_count);
            if let Ok(mut file) = std::fs::File::create(&filename) {
                if file.write_all(h264_data).is_ok() {
                    trace!(
                        "🎬 Dumped frame {} to {} ({} bytes, timestamp={}ms)",
                        dump_count,
                        filename,
                        h264_data.len(),
                        timestamp_ms
                    );
                }
            }
        }

        // Create region covering the DISPLAY area (not the padded encoded area)
        // This ensures only the actual frame is visible, cropping any padding
        // QP 22 is a good balance of quality vs bitrate for RDP
        let regions = vec![Avc420Region::full_frame(display_width, display_height, 22)];

        trace!(
            "Region: Display {}×{} from encoded {}×{} (cropping: {}px right, {}px bottom)",
            display_width,
            display_height,
            encoded_width,
            encoded_height,
            encoded_width.saturating_sub(display_width),
            encoded_height.saturating_sub(display_height)
        );

        // Lock the server and send frame
        // Also query channel_id while holding the lock
        // Note: Using std::sync::Mutex (not tokio) because GfxServerHandle
        // is shared with DvcProcessor which requires sync methods
        let (frame_id, dvc_messages, channel_id) = {
            let mut server = self.gfx_server.lock().map_err(|_| SendError::LockFailed)?;

            // Get channel_id from the server (set by DVC infrastructure in start())
            let channel_id = server.channel_id().ok_or(SendError::NotReady)?;

            // Send the frame
            let frame_id = server
                .send_avc420_frame(surface_id, h264_data, &regions, timestamp_ms)
                .ok_or(SendError::Backpressure)?;

            // Drain output to get DVC messages
            let messages = server.drain_output();

            (frame_id, messages, channel_id)
        };

        // Convert DVC messages to SVC messages
        if !dvc_messages.is_empty() {
            trace!(
                "EGFX: drain_output returned {} DVC messages for frame {}",
                dvc_messages.len(),
                frame_id
            );

            let svc_messages =
                encode_dvc_messages(channel_id, dvc_messages, ChannelFlags::SHOW_PROTOCOL)
                    .map_err(|e| SendError::EncodingFailed(e.to_string()))?;

            trace!(
                "EGFX: Encoded {} SVC messages for channel {}",
                svc_messages.len(),
                channel_id
            );

            // Send via ServerEvent (unbounded channel - never blocks)
            let event = ServerEvent::Egfx(EgfxServerMessage::SendMessages {
                channel_id,
                messages: svc_messages,
            });

            self.event_tx
                .send(event)
                .map_err(|_| SendError::ChannelClosed)?;

            trace!("EGFX: ServerEvent::Egfx sent for frame {}", frame_id);
        } else {
            warn!(
                "EGFX: drain_output returned EMPTY for frame {} - no data sent!",
                frame_id
            );
        }

        // Update stats
        let count = self
            .frame_count
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if count % 30 == 0 {
            trace!(
                "EGFX: Sent frame {} (id={}, display={}×{}, encoded={}×{}, {} bytes)",
                count,
                frame_id,
                display_width,
                display_height,
                encoded_width,
                encoded_height,
                h264_data.len()
            );
        }

        Ok(frame_id)
    }

    /// Send an AVC444 encoded frame (dual H.264 streams) through EGFX
    ///
    /// # Arguments
    ///
    /// * `stream1_data` - Main view H.264 NAL units (luma + subsampled chroma)
    /// * `stream2_data` - Auxiliary view H.264 NAL units (additional chroma)
    /// * `encoded_width` - Width used for H.264 encoding (MUST be aligned to 16)
    /// * `encoded_height` - Height used for H.264 encoding (MUST be aligned to 16)
    /// * `display_width` - Actual width to display (may be < encoded_width)
    /// * `display_height` - Actual height to display (may be < encoded_height)
    /// * `timestamp_ms` - Frame timestamp in milliseconds
    ///
    /// # Returns
    ///
    /// `Ok(frame_id)` if the frame was sent successfully, or an error.
    ///
    /// # Note
    ///
    /// AVC444 provides full 4:4:4 chroma resolution for graphics/CAD applications.
    /// Both streams must use the same encoded dimensions.
    pub async fn send_avc444_frame(
        &self,
        stream1_data: &[u8],
        stream2_data: &[u8],
        encoded_width: u16,
        encoded_height: u16,
        display_width: u16,
        display_height: u16,
        timestamp_ms: u32,
    ) -> SendResult<u32> {
        // Check readiness (same as AVC420)
        let state = self
            .handler_state
            .read()
            .await
            .as_ref()
            .cloned()
            .ok_or(SendError::NotReady)?;

        if !state.is_ready {
            return Err(SendError::NotReady);
        }

        // Note: We check AVC420 capability as a proxy for AVC444
        // TODO: Add explicit is_avc444_enabled flag when capability negotiation is enhanced
        if !state.is_avc420_enabled {
            return Err(SendError::Avc420NotSupported);
        }

        let surface_id = state.primary_surface_id.ok_or(SendError::NoSurface)?;

        trace!(
            "EGFX AVC444: Sending frame - stream1: {} bytes, stream2: {} bytes, {}x{}",
            stream1_data.len(),
            stream2_data.len(),
            display_width,
            display_height
        );

        // Create regions for both streams (same dimensions)
        let luma_regions = vec![Avc420Region::full_frame(display_width, display_height, 22)];
        let chroma_regions = vec![Avc420Region::full_frame(display_width, display_height, 22)];

        // Lock the server and send AVC444 frame
        let (frame_id, dvc_messages, channel_id) = {
            let mut server = self.gfx_server.lock().map_err(|_| SendError::LockFailed)?;

            let channel_id = server.channel_id().ok_or(SendError::NotReady)?;

            // Send the AVC444 frame using ironrdp-egfx's send_avc444_frame method
            let frame_id = server
                .send_avc444_frame(
                    surface_id,
                    stream1_data,
                    &luma_regions,
                    Some(stream2_data),
                    Some(&chroma_regions),
                    timestamp_ms,
                )
                .ok_or(SendError::Backpressure)?;

            let messages = server.drain_output();

            (frame_id, messages, channel_id)
        };

        // Convert DVC messages to SVC messages (same as AVC420)
        if !dvc_messages.is_empty() {
            trace!(
                "EGFX AVC444: drain_output returned {} DVC messages for frame {}",
                dvc_messages.len(),
                frame_id
            );

            let svc_messages =
                encode_dvc_messages(channel_id, dvc_messages, ChannelFlags::SHOW_PROTOCOL)
                    .map_err(|e| SendError::EncodingFailed(e.to_string()))?;

            let event = ServerEvent::Egfx(EgfxServerMessage::SendMessages {
                channel_id,
                messages: svc_messages,
            });

            self.event_tx
                .send(event)
                .map_err(|_| SendError::ChannelClosed)?;

            trace!("EGFX AVC444: ServerEvent::Egfx sent for frame {}", frame_id);
        } else {
            warn!(
                "EGFX AVC444: drain_output returned EMPTY for frame {} - no data sent!",
                frame_id
            );
        }

        // Update stats
        let count = self
            .frame_count
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if count % 30 == 0 {
            trace!(
                "EGFX AVC444: Sent frame {} (id={}, {}×{}, stream1={}b, stream2={}b)",
                count,
                frame_id,
                display_width,
                display_height,
                stream1_data.len(),
                stream2_data.len()
            );
        }

        Ok(frame_id)
    }

    /// Check if AVC444 is supported by the client
    ///
    /// Currently returns the same as AVC420 support until explicit AVC444
    /// capability negotiation is implemented.
    pub async fn is_avc444_supported(&self) -> bool {
        // TODO: Check explicit AVC444 capability when available
        self.is_ready().await
    }

    /// Get number of frames sent
    pub fn frames_sent(&self) -> u64 {
        self.frame_count.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Send an H.264 frame with specific damage regions
    ///
    /// This method allows specifying which regions of the frame have changed,
    /// enabling the client to optimize rendering by only updating those areas.
    ///
    /// # Arguments
    ///
    /// * `h264_data` - H.264 NAL units (Annex B format)
    /// * `encoded_width` - Width used for encoding (16-pixel aligned)
    /// * `encoded_height` - Height used for encoding (16-pixel aligned)
    /// * `display_width` - Actual display width
    /// * `display_height` - Actual display height
    /// * `damage_regions` - List of changed regions (empty = full frame)
    /// * `timestamp_ms` - Frame timestamp
    ///
    /// # Returns
    ///
    /// `Ok(frame_id)` on success, or an error.
    pub async fn send_frame_with_regions(
        &self,
        h264_data: &[u8],
        encoded_width: u16,
        encoded_height: u16,
        display_width: u16,
        display_height: u16,
        damage_regions: &[DamageRegion],
        timestamp_ms: u32,
    ) -> SendResult<u32> {
        // Check readiness
        let state = self
            .handler_state
            .read()
            .await
            .as_ref()
            .cloned()
            .ok_or(SendError::NotReady)?;

        if !state.is_ready {
            return Err(SendError::NotReady);
        }

        if !state.is_avc420_enabled {
            return Err(SendError::Avc420NotSupported);
        }

        let surface_id = state.primary_surface_id.ok_or(SendError::NoSurface)?;

        // Convert damage regions to EGFX regions
        // If no regions provided, use full frame
        let regions = if damage_regions.is_empty() {
            vec![Avc420Region::full_frame(display_width, display_height, 22)]
        } else {
            damage_regions_to_avc420(damage_regions, display_width, display_height)
        };

        // Log region info
        if regions.len() > 1 {
            let total_area: u64 = damage_regions.iter().map(|r| r.area()).sum();
            let frame_area = display_width as u64 * display_height as u64;
            let ratio = (total_area as f32 / frame_area as f32 * 100.0) as u32;
            debug!(
                "EGFX: Sending {} regions ({}% of frame) for {}×{} frame",
                regions.len(),
                ratio,
                display_width,
                display_height
            );
        }

        // Lock the server and send frame
        let (frame_id, dvc_messages, channel_id) = {
            let mut server = self.gfx_server.lock().map_err(|_| SendError::LockFailed)?;
            let channel_id = server.channel_id().ok_or(SendError::NotReady)?;

            let frame_id = server
                .send_avc420_frame(surface_id, h264_data, &regions, timestamp_ms)
                .ok_or(SendError::Backpressure)?;

            let messages = server.drain_output();
            (frame_id, messages, channel_id)
        };

        // Send DVC messages
        if !dvc_messages.is_empty() {
            let svc_messages =
                encode_dvc_messages(channel_id, dvc_messages, ChannelFlags::SHOW_PROTOCOL)
                    .map_err(|e| SendError::EncodingFailed(e.to_string()))?;

            let event = ServerEvent::Egfx(EgfxServerMessage::SendMessages {
                channel_id,
                messages: svc_messages,
            });

            self.event_tx
                .send(event)
                .map_err(|_| SendError::ChannelClosed)?;
        }

        // Update stats
        self.frame_count
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);

        Ok(frame_id)
    }

    /// Send an AVC444 frame with specific damage regions
    ///
    /// Similar to `send_frame_with_regions` but for AVC444 dual-stream encoding.
    ///
    /// # Phase 1: Auxiliary Stream Omission
    ///
    /// The `stream2_data` parameter is now Optional. When `None`, IronRDP's
    /// `send_avc444_frame` will set LC=1 (luma only), instructing the client
    /// to reuse its cached auxiliary stream for bandwidth optimization.
    pub async fn send_avc444_frame_with_regions(
        &self,
        stream1_data: &[u8],
        stream2_data: Option<&[u8]>, // Now optional!
        encoded_width: u16,
        encoded_height: u16,
        display_width: u16,
        display_height: u16,
        damage_regions: &[DamageRegion],
        timestamp_ms: u32,
    ) -> SendResult<u32> {
        // Check readiness
        let state = self
            .handler_state
            .read()
            .await
            .as_ref()
            .cloned()
            .ok_or(SendError::NotReady)?;

        if !state.is_ready {
            return Err(SendError::NotReady);
        }

        if !state.is_avc420_enabled {
            return Err(SendError::Avc420NotSupported);
        }

        let surface_id = state.primary_surface_id.ok_or(SendError::NoSurface)?;

        // Convert damage regions to EGFX regions
        let regions = if damage_regions.is_empty() {
            vec![Avc420Region::full_frame(display_width, display_height, 22)]
        } else {
            damage_regions_to_avc420(damage_regions, display_width, display_height)
        };

        // Log region info for AVC444
        if regions.len() > 1 {
            debug!(
                "EGFX AVC444: Sending {} regions for {}×{} frame",
                regions.len(),
                display_width,
                display_height
            );
        }

        // Lock the server and send AVC444 frame
        let (frame_id, dvc_messages, channel_id) = {
            let mut server = self.gfx_server.lock().map_err(|_| SendError::LockFailed)?;
            let channel_id = server.channel_id().ok_or(SendError::NotReady)?;

            // === PHASE 1: PASS OPTIONAL AUX TO IRONRDP ===
            // IronRDP's send_avc444_frame accepts Option<&[u8]> for aux stream
            // - Some(data) → LC=0 (both streams present)
            // - None → LC=1 (luma only, client reuses previous aux)
            // This was verified in Step 0 of Phase 1 research
            let frame_id = server
                .send_avc444_frame(
                    surface_id,
                    stream1_data,
                    &regions,
                    stream2_data, // Pass Option<&[u8]> - IronRDP handles it!
                    stream2_data.map(|_| regions.as_slice()), // Option<&[Avc420Region]>
                    timestamp_ms,
                )
                .ok_or(SendError::Backpressure)?;

            let messages = server.drain_output();
            (frame_id, messages, channel_id)
        };

        // Send DVC messages
        if !dvc_messages.is_empty() {
            let svc_messages =
                encode_dvc_messages(channel_id, dvc_messages, ChannelFlags::SHOW_PROTOCOL)
                    .map_err(|e| SendError::EncodingFailed(e.to_string()))?;

            let event = ServerEvent::Egfx(EgfxServerMessage::SendMessages {
                channel_id,
                messages: svc_messages,
            });

            self.event_tx
                .send(event)
                .map_err(|_| SendError::ChannelClosed)?;
        }

        // Update stats
        self.frame_count
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);

        Ok(frame_id)
    }
}

/// Convert DamageRegion list to Avc420Region list
///
/// Clamps regions to display bounds and assigns QP values.
/// Avc420Region uses left/top/right/bottom (inclusive LTRB) format.
fn damage_regions_to_avc420(
    regions: &[DamageRegion],
    display_width: u16,
    display_height: u16,
) -> Vec<Avc420Region> {
    regions
        .iter()
        .filter_map(|r| {
            // Clamp to display bounds (LTRB format, inclusive)
            let left = r.x.min(display_width as u32) as u16;
            let top = r.y.min(display_height as u32) as u16;
            // Right and bottom are inclusive, so subtract 1 from the exclusive bounds
            let right = (r.x + r.width).min(display_width as u32).saturating_sub(1) as u16;
            let bottom = (r.y + r.height)
                .min(display_height as u32)
                .saturating_sub(1) as u16;

            // Skip invalid regions (where right < left or bottom < top)
            if right < left || bottom < top {
                return None;
            }

            // Avc420Region fields:
            // - quantization_parameter: H.264 QP (0-51, lower = better quality)
            // - quality: 0-100 (higher = better)
            Some(Avc420Region {
                left,
                top,
                right,
                bottom,
                quantization_parameter: 22, // Good quality/bitrate balance
                quality: 100,               // Maximum quality for damage regions
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_send_error_display() {
        assert_eq!(SendError::NotReady.to_string(), "EGFX channel not ready");
        assert_eq!(
            SendError::Avc420NotSupported.to_string(),
            "AVC420 not supported by client"
        );
        assert_eq!(
            SendError::Backpressure.to_string(),
            "Frame dropped due to backpressure"
        );
    }
}
