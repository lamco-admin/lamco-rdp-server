//! LamcoGraphicsHandler - GraphicsPipelineHandler implementation
//!
//! This module provides the handler that bridges our OpenH264 encoder
//! with ironrdp-egfx's GraphicsPipelineServer.
//!
//! # State Synchronization
//!
//! The handler maintains local atomic state AND synchronizes with a shared
//! `HandlerState` (from `gfx_factory`) that the `EgfxFrameSender` reads.
//! This dual-state approach allows both:
//! - Fast local access for internal handler operations
//! - Cross-task visibility for the frame sender to check EGFX readiness

use std::sync::{
    atomic::{AtomicBool, AtomicU16, Ordering},
    Arc,
};

use ironrdp_egfx::{
    pdu::{CapabilitiesAdvertisePdu, CapabilitiesV81Flags, CapabilitySet},
    server::{GraphicsPipelineHandler, QoeMetrics, Surface},
};
use tracing::{debug, info, trace, warn};

use crate::server::{HandlerState, SharedHandlerState};

/// Handler for EGFX graphics pipeline events
///
/// This implements `GraphicsPipelineHandler` to receive callbacks from
/// ironrdp-egfx's `GraphicsPipelineServer` and manage our OpenH264 encoder.
///
/// # State Synchronization
///
/// The handler maintains both local state (for fast access) and syncs to
/// a `SharedHandlerState` that `EgfxFrameSender` reads. This allows the
/// display handler to check EGFX readiness without holding locks on
/// the GraphicsPipelineServer.
///
/// # Codec Support
///
/// - **AVC420**: H.264 with 4:2:0 chroma, supported in V8.1+ with AVC420_ENABLED flag
/// - **AVC444**: H.264 with 4:4:4 chroma via dual-stream encoding, supported in V10+
///   when AVC420_ENABLED is set (MS-RDPEGFX Section 2.2.3.10: V10 with AVC420_ENABLED
///   implies AVC444v2 support)
///
/// # Platform Quirks
///
/// AVC444 works correctly on most platforms with the single-encoder architecture
/// (validated in v1.3.0/v1.3.1). Some platforms (e.g., RHEL 9 / GNOME 40) exhibit
/// AVC444-specific issues that remain under investigation. When `force_avc420_only`
/// is set via platform quirk detection, the handler disables AVC444 for that platform.
pub struct LamcoGraphicsHandler {
    /// Surface dimensions
    width: u16,
    height: u16,

    /// Whether AVC420 was negotiated (local fast access)
    avc420_enabled: AtomicBool,

    /// Whether AVC444 was negotiated (V10+ with AVC420)
    avc444_enabled: AtomicBool,

    /// Whether the channel is ready for frames (local fast access)
    ready: AtomicBool,

    /// Whether a primary surface exists (local fast access)
    has_surface: AtomicBool,

    /// Current primary surface ID (local fast access)
    /// Only valid when has_surface is true
    primary_surface_id: AtomicU16,

    /// Negotiated capability set (stored for reference)
    negotiated_caps: std::sync::RwLock<Option<CapabilitySet>>,

    /// Shared state for cross-task synchronization with EgfxFrameSender
    ///
    /// When set, callbacks update this state so the display handler can
    /// check EGFX readiness without locking the GraphicsPipelineServer.
    shared_state: Option<SharedHandlerState>,

    /// Force AVC420-only mode due to platform quirks
    ///
    /// When true, AVC444 will be disabled even if the client supports it.
    /// Set based on compositor profile detection (e.g., RHEL 9 ForceAvc420 quirk).
    force_avc420_only: bool,

    /// Maximum frames in flight before backpressure
    ///
    /// Controls how many frames can be sent before waiting for acknowledgment.
    /// Higher values improve throughput but increase latency under congestion.
    /// Default: 3 frames
    max_frames_in_flight: u32,
}

impl LamcoGraphicsHandler {
    pub fn new(width: u16, height: u16) -> Self {
        Self {
            width,
            height,
            avc420_enabled: AtomicBool::new(false),
            avc444_enabled: AtomicBool::new(false),
            ready: AtomicBool::new(false),
            has_surface: AtomicBool::new(false),
            primary_surface_id: AtomicU16::new(0),
            negotiated_caps: std::sync::RwLock::new(None),
            shared_state: None,
            force_avc420_only: false,
            max_frames_in_flight: 3, // Default
        }
    }

    pub fn with_quirks(width: u16, height: u16, force_avc420_only: bool) -> Self {
        Self {
            width,
            height,
            avc420_enabled: AtomicBool::new(false),
            avc444_enabled: AtomicBool::new(false),
            ready: AtomicBool::new(false),
            has_surface: AtomicBool::new(false),
            primary_surface_id: AtomicU16::new(0),
            negotiated_caps: std::sync::RwLock::new(None),
            shared_state: None,
            force_avc420_only,
            max_frames_in_flight: 3, // Default
        }
    }

    pub fn with_shared_state(width: u16, height: u16, shared_state: SharedHandlerState) -> Self {
        Self {
            width,
            height,
            avc420_enabled: AtomicBool::new(false),
            avc444_enabled: AtomicBool::new(false),
            ready: AtomicBool::new(false),
            has_surface: AtomicBool::new(false),
            primary_surface_id: AtomicU16::new(0),
            force_avc420_only: false,
            negotiated_caps: std::sync::RwLock::new(None),
            shared_state: Some(shared_state),
            max_frames_in_flight: 3, // Default
        }
    }

    pub fn with_shared_state_and_quirks(
        width: u16,
        height: u16,
        shared_state: SharedHandlerState,
        force_avc420_only: bool,
    ) -> Self {
        Self::with_config(width, height, shared_state, force_avc420_only, 3)
    }

    pub fn with_config(
        width: u16,
        height: u16,
        shared_state: SharedHandlerState,
        force_avc420_only: bool,
        max_frames_in_flight: u32,
    ) -> Self {
        if force_avc420_only {
            info!("EGFX handler: AVC444 disabled by platform quirk (force_avc420_only)");
        }
        info!(
            "EGFX handler: max_frames_in_flight={}",
            max_frames_in_flight
        );
        Self {
            width,
            height,
            avc420_enabled: AtomicBool::new(false),
            avc444_enabled: AtomicBool::new(false),
            ready: AtomicBool::new(false),
            has_surface: AtomicBool::new(false),
            primary_surface_id: AtomicU16::new(0),
            force_avc420_only,
            negotiated_caps: std::sync::RwLock::new(None),
            shared_state: Some(shared_state),
            max_frames_in_flight,
        }
    }

    /// Synchronize current state to the shared HandlerState
    ///
    /// Called internally after state changes. Uses try_write to avoid
    /// blocking in callback contexts (sync callback with async state).
    fn sync_shared_state(&self) {
        if let Some(ref shared) = self.shared_state {
            // Note: We use try_write because callbacks are synchronous but
            // SharedHandlerState uses tokio::sync::RwLock. This is safe because
            // we initialize the state in the same thread before callbacks start.
            if let Ok(mut guard) = shared.try_write() {
                // Preserve existing channel_id if we had one.
                // NOTE: channel_id is stored in GraphicsPipelineServer (set by DvcProcessor::start),
                // and EgfxFrameSender queries it directly via server.channel_id() when sending frames.
                // We preserve it here for diagnostic purposes only - it's not used for frame sending.
                let existing_channel_id: u32 = guard
                    .as_ref()
                    .map_or(0, |s: &HandlerState| s.dvc_channel_id);

                let state = HandlerState {
                    is_ready: self.ready.load(Ordering::Acquire),
                    is_avc420_enabled: self.avc420_enabled.load(Ordering::Acquire),
                    is_avc444_enabled: self.avc444_enabled.load(Ordering::Acquire),
                    // Convert has_surface + surface_id to Option<u16>
                    // Surface ID 0 is valid in EGFX, so we use Option instead of sentinel
                    primary_surface_id: if self.has_surface.load(Ordering::Acquire) {
                        Some(self.primary_surface_id.load(Ordering::Acquire))
                    } else {
                        None
                    },
                    dvc_channel_id: existing_channel_id,
                };
                *guard = Some(state);
            } else {
                warn!("Failed to sync EGFX handler state (lock contention)");
            }
        }
    }

    pub fn is_ready(&self) -> bool {
        self.ready.load(Ordering::Acquire)
    }

    pub fn is_avc420_enabled(&self) -> bool {
        self.avc420_enabled.load(Ordering::Acquire)
    }

    pub fn primary_surface_id(&self) -> u16 {
        self.primary_surface_id.load(Ordering::Acquire)
    }

    pub fn set_dimensions(&mut self, width: u16, height: u16) {
        self.width = width;
        self.height = height;
    }

    pub fn dimensions(&self) -> (u16, u16) {
        (self.width, self.height)
    }
}

impl GraphicsPipelineHandler for LamcoGraphicsHandler {
    fn capabilities_advertise(&mut self, pdu: &CapabilitiesAdvertisePdu) {
        info!("EGFX: Client advertised {} capability sets", pdu.0.len());
        for (i, cap) in pdu.0.iter().enumerate() {
            debug!("EGFX CAP-ADV[{}]: {:?}", i, cap);
        }
    }

    fn on_ready(&mut self, negotiated: &CapabilitySet) {
        info!("EGFX: Channel ready with {:?}", negotiated);

        // Store negotiated caps
        if let Ok(mut guard) = self.negotiated_caps.write() {
            *guard = Some(negotiated.clone());
        }

        // Check for AVC420 and AVC444 support based on capability version
        //
        // Per MS-RDPEGFX Section 2.2.3.10 (RDPGFX_CAPSET_VERSION10):
        // - V8.1 with AVC420_ENABLED → AVC420 only (4:2:0 chroma)
        // - V10+ with AVC420_ENABLED → AVC420 AND AVC444v2 (4:4:4 chroma via dual-stream)
        //
        // AVC444v2 provides superior text/UI rendering through full chroma resolution.
        let (avc420, avc444) = match negotiated {
            CapabilitySet::V8_1 { flags, .. } => {
                // V8.1: AVC420 only, no AVC444 support
                let has_avc420 = flags.contains(CapabilitiesV81Flags::AVC420_ENABLED);
                (has_avc420, false)
            }
            // V10+ with AVC420 implies AVC444v2 support
            CapabilitySet::V10 { .. }
            | CapabilitySet::V10_1
            | CapabilitySet::V10_2 { .. }
            | CapabilitySet::V10_3 { .. }
            | CapabilitySet::V10_4 { .. }
            | CapabilitySet::V10_5 { .. }
            | CapabilitySet::V10_6 { .. }
            | CapabilitySet::V10_7 { .. } => {
                // V10+: Both AVC420 and AVC444v2 are implied by client capability
                (true, true)
            }
            // V8 and earlier don't support AVC
            _ => (false, false),
        };

        // Platform quirk: force AVC420 when ForceAvc420 is active.
        // AVC444 works on most platforms but has known issues on some
        // (e.g., RHEL 9 / GNOME 40 produces blurry text or protocol errors).
        let effective_avc444 = if self.force_avc420_only && avc444 {
            warn!(
                "EGFX: ForceAvc420 quirk active: suppressing AVC444. \
                 Client supports AVC444 but platform quirk forces AVC420 only."
            );
            false
        } else {
            avc444
        };

        self.avc420_enabled.store(avc420, Ordering::Release);
        self.avc444_enabled
            .store(effective_avc444, Ordering::Release);
        self.ready.store(true, Ordering::Release);

        // Sync to shared state for EgfxFrameSender visibility
        self.sync_shared_state();

        // Log codec capabilities
        match (avc420, effective_avc444) {
            (true, true) => {
                info!("EGFX: AVC420 + AVC444v2 encoding enabled (V10+ capabilities)");
            }
            (true, false) if self.force_avc420_only => {
                info!("EGFX: AVC420 encoding enabled (AVC444 suppressed by platform quirk)");
            }
            (true, false) => {
                info!("EGFX: AVC420 (H.264 4:2:0) encoding enabled");
            }
            (false, _) => {
                info!("EGFX: AVC not supported by client, will use RemoteFX fallback");
            }
        }
    }

    fn on_frame_ack(&mut self, frame_id: u32, queue_depth: u32) {
        trace!(
            "EGFX: frame_ack id={}, queue_depth={}",
            frame_id,
            queue_depth
        );
    }

    fn on_qoe_metrics(&mut self, metrics: QoeMetrics) {
        debug!(
            "EGFX: QoE metrics - frame {}, decode+render: {}μs",
            metrics.frame_id, metrics.time_diff_dr
        );
        // Future: Use metrics to adjust encoding quality dynamically
    }

    fn on_surface_created(&mut self, surface: &Surface) {
        info!(
            "EGFX: Surface {} created: {}x{}",
            surface.id, surface.width, surface.height
        );

        // Track first surface as primary
        if !self.has_surface.load(Ordering::Acquire) {
            self.primary_surface_id.store(surface.id, Ordering::Release);
            self.has_surface.store(true, Ordering::Release);
            // Sync to shared state - surface is now available
            self.sync_shared_state();
            info!("EGFX: Surface {} set as primary", surface.id);
        }
    }

    fn on_surface_deleted(&mut self, surface_id: u16) {
        debug!("EGFX: Surface {} deleted", surface_id);

        // Clear primary if it was deleted
        if self.has_surface.load(Ordering::Acquire)
            && self.primary_surface_id.load(Ordering::Acquire) == surface_id
        {
            self.has_surface.store(false, Ordering::Release);
            // Sync to shared state - surface no longer available
            self.sync_shared_state();
            info!("EGFX: Primary surface {} deleted", surface_id);
        }
    }

    fn on_close(&mut self) {
        debug!(
            "EGFX: channel closed (was_ready={}, avc420={}, avc444={}, has_surface={})",
            self.ready.load(Ordering::Acquire),
            self.avc420_enabled.load(Ordering::Acquire),
            self.avc444_enabled.load(Ordering::Acquire),
            self.has_surface.load(Ordering::Acquire),
        );
        self.ready.store(false, Ordering::Release);
        self.avc420_enabled.store(false, Ordering::Release);
        self.has_surface.store(false, Ordering::Release);
        // Sync to shared state - channel closed
        self.sync_shared_state();
    }

    fn max_frames_in_flight(&self) -> u32 {
        // Use configured value for backpressure control
        self.max_frames_in_flight
    }

    fn preferred_capabilities(&self) -> Vec<CapabilitySet> {
        use ironrdp_egfx::pdu::{
            CapabilitiesV103Flags, CapabilitiesV104Flags, CapabilitiesV107Flags,
            CapabilitiesV10Flags,
        };

        // Prefer highest V10.x version for best features (all V10+ support AVC420)
        // Fall back to V8.1 for older clients that explicitly enable AVC420
        vec![
            CapabilitySet::V10_7 {
                flags: CapabilitiesV107Flags::SMALL_CACHE,
            },
            CapabilitySet::V10_6 {
                flags: CapabilitiesV104Flags::SMALL_CACHE,
            },
            CapabilitySet::V10_5 {
                flags: CapabilitiesV104Flags::SMALL_CACHE,
            },
            CapabilitySet::V10_4 {
                flags: CapabilitiesV104Flags::SMALL_CACHE,
            },
            CapabilitySet::V10_3 {
                flags: CapabilitiesV103Flags::AVC_THIN_CLIENT,
            },
            CapabilitySet::V10_2 {
                flags: CapabilitiesV10Flags::SMALL_CACHE,
            },
            CapabilitySet::V10 {
                flags: CapabilitiesV10Flags::SMALL_CACHE,
            },
            CapabilitySet::V8_1 {
                flags: CapabilitiesV81Flags::AVC420_ENABLED | CapabilitiesV81Flags::SMALL_CACHE,
            },
        ]
    }
}

/// Thread-safe wrapper for LamcoGraphicsHandler
///
/// Since GraphicsPipelineHandler requires `Send`, but we also need
/// to query state from other tasks, this wrapper provides Arc-based sharing.
pub struct SharedGraphicsHandler {
    inner: Arc<std::sync::RwLock<LamcoGraphicsHandler>>,
}

impl SharedGraphicsHandler {
    pub fn new(width: u16, height: u16) -> Self {
        Self {
            inner: Arc::new(std::sync::RwLock::new(LamcoGraphicsHandler::new(
                width, height,
            ))),
        }
    }

    pub fn clone_inner(&self) -> Arc<std::sync::RwLock<LamcoGraphicsHandler>> {
        Arc::clone(&self.inner)
    }

    pub fn is_ready(&self) -> bool {
        self.inner.read().map(|h| h.is_ready()).unwrap_or(false)
    }

    pub fn is_avc420_enabled(&self) -> bool {
        self.inner
            .read()
            .map(|h| h.is_avc420_enabled())
            .unwrap_or(false)
    }
}
