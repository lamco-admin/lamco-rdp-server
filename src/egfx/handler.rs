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
    Arc,
    atomic::{AtomicBool, AtomicU16, Ordering},
};

use ironrdp_egfx::{
    pdu::{
        CapabilitiesAdvertisePdu, CapabilitiesV8Flags, CapabilitiesV10Flags, CapabilitiesV81Flags,
        CapabilitiesV103Flags, CapabilitiesV104Flags, CapabilitiesV107Flags, CapabilitySet,
    },
    server::{GraphicsPipelineHandler, QoeMetrics, Surface},
};
use parking_lot::RwLock;
use tracing::{debug, info, trace, warn};

use crate::{
    health::performance::EgfxSnapshot,
    runtime::metrics::{MetricsCollector, metric_names},
    server::SharedHandlerState,
};

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

    /// Whether the client negotiated EGFX with `AVC_DISABLED` set (identifies
    /// the Android Microsoft RD Client's pointer-rendering quirks; see
    /// `SharedHandlerState::needs_android_pointer_updates`).
    needs_android_pointer_updates: AtomicBool,

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
    shared_state: Option<Arc<SharedHandlerState>>,

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

    /// Metrics collector for recording EGFX pipeline performance.
    /// When set, on_frame_ack and on_qoe_metrics record data
    /// instead of discarding it.
    metrics: Option<Arc<MetricsCollector>>,

    /// Shared EGFX snapshot state for the SnapshotCollector.
    /// Updated on state changes so performance snapshots reflect
    /// current EGFX pipeline state.
    egfx_snapshot: Option<Arc<RwLock<EgfxSnapshot>>>,

    /// Health reporter for EGFX channel state events.
    /// Fires EgfxChannelReady on negotiation + surface, EgfxChannelClosed on close.
    health_reporter: Option<crate::health::HealthReporter>,
}

impl LamcoGraphicsHandler {
    pub fn new(width: u16, height: u16) -> Self {
        Self {
            width,
            height,
            avc420_enabled: AtomicBool::new(false),
            avc444_enabled: AtomicBool::new(false),
            needs_android_pointer_updates: AtomicBool::new(false),
            ready: AtomicBool::new(false),
            has_surface: AtomicBool::new(false),
            primary_surface_id: AtomicU16::new(0),
            negotiated_caps: std::sync::RwLock::new(None),
            shared_state: None,
            force_avc420_only: false,
            max_frames_in_flight: 3,
            metrics: None,
            egfx_snapshot: None,
            health_reporter: None,
        }
    }

    pub fn with_quirks(width: u16, height: u16, force_avc420_only: bool) -> Self {
        Self {
            width,
            height,
            avc420_enabled: AtomicBool::new(false),
            avc444_enabled: AtomicBool::new(false),
            needs_android_pointer_updates: AtomicBool::new(false),
            ready: AtomicBool::new(false),
            has_surface: AtomicBool::new(false),
            primary_surface_id: AtomicU16::new(0),
            negotiated_caps: std::sync::RwLock::new(None),
            shared_state: None,
            force_avc420_only,
            max_frames_in_flight: 3,
            metrics: None,
            egfx_snapshot: None,
            health_reporter: None,
        }
    }

    pub fn with_shared_state(width: u16, height: u16, shared_state: SharedHandlerState) -> Self {
        Self {
            width,
            height,
            avc420_enabled: AtomicBool::new(false),
            avc444_enabled: AtomicBool::new(false),
            needs_android_pointer_updates: AtomicBool::new(false),
            ready: AtomicBool::new(false),
            has_surface: AtomicBool::new(false),
            primary_surface_id: AtomicU16::new(0),
            force_avc420_only: false,
            negotiated_caps: std::sync::RwLock::new(None),
            shared_state: Some(Arc::new(shared_state)),
            max_frames_in_flight: 3,
            metrics: None,
            egfx_snapshot: None,
            health_reporter: None,
        }
    }

    pub fn with_shared_state_and_quirks(
        width: u16,
        height: u16,
        shared_state: Arc<SharedHandlerState>,
        force_avc420_only: bool,
    ) -> Self {
        Self::with_config(width, height, shared_state, force_avc420_only, 3)
    }

    pub fn with_config(
        width: u16,
        height: u16,
        shared_state: Arc<SharedHandlerState>,
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
            needs_android_pointer_updates: AtomicBool::new(false),
            ready: AtomicBool::new(false),
            has_surface: AtomicBool::new(false),
            primary_surface_id: AtomicU16::new(0),
            force_avc420_only,
            negotiated_caps: std::sync::RwLock::new(None),
            shared_state: Some(shared_state),
            max_frames_in_flight,
            metrics: None,
            egfx_snapshot: None,
            health_reporter: None,
        }
    }

    /// Attach a MetricsCollector for recording EGFX pipeline data.
    pub fn set_metrics(&mut self, metrics: Arc<MetricsCollector>) {
        self.metrics = Some(metrics);
    }

    /// Attach an EGFX snapshot handle for the SnapshotCollector.
    pub fn set_egfx_snapshot(&mut self, snapshot: Arc<RwLock<EgfxSnapshot>>) {
        self.egfx_snapshot = Some(snapshot);
    }

    /// Attach a health reporter for EGFX channel state events.
    pub fn set_health_reporter(&mut self, reporter: crate::health::HealthReporter) {
        self.health_reporter = Some(reporter);
    }

    /// Synchronize current state to the shared HandlerState
    ///
    /// Propagate local atomic state to the shared state.
    ///
    /// Uses atomic stores (no locks) so it never fails, even when called
    /// from synchronous DVC callbacks while the async pipeline loop is
    /// reading the same fields.
    fn sync_shared_state(&self) {
        if let Some(ref shared) = self.shared_state {
            shared
                .is_ready
                .store(self.ready.load(Ordering::Acquire), Ordering::Release);
            shared.client_supports_avc420.store(
                self.avc420_enabled.load(Ordering::Acquire),
                Ordering::Release,
            );
            shared.is_avc444_enabled.store(
                self.avc444_enabled.load(Ordering::Acquire),
                Ordering::Release,
            );
            shared
                .has_surface
                .store(self.has_surface.load(Ordering::Acquire), Ordering::Release);
            shared.primary_surface_id.store(
                self.primary_surface_id.load(Ordering::Acquire),
                Ordering::Release,
            );
            shared.needs_android_pointer_updates.store(
                self.needs_android_pointer_updates.load(Ordering::Acquire),
                Ordering::Release,
            );
        }
    }

    pub fn is_ready(&self) -> bool {
        self.ready.load(Ordering::Acquire)
    }

    pub fn client_supports_avc420(&self) -> bool {
        self.avc420_enabled.load(Ordering::Acquire)
    }

    pub fn needs_android_pointer_updates(&self) -> bool {
        self.needs_android_pointer_updates.load(Ordering::Acquire)
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
        let already_ready = self.ready.load(Ordering::Acquire);
        if already_ready {
            // mstsc has re-emitted RDPGFX_CAPSADVERTISE mid-session. This is
            // its decoder-recovery sequence: it expects the server to do a
            // full re-init (ResetGraphics + CreateSurface + MapSurfaceToOutput
            // + IDR). If we only send CapsConfirm and keep emitting P-slices
            // referencing the old decoder state, mstsc closes the Graphics
            // DVC channel and RSTs the TCP connection within milliseconds.
            //
            // Set the shared-state flag so the display loop runs the re-init
            // sequence before encoding the next frame.
            warn!(
                "EGFX: Client RE-ADVERTISED capabilities mid-session — triggering full re-init \
                 (decoder recovery sequence; common under load when long P-slice chain desyncs)"
            );

            // Reset LOCAL surface tracking. Without this, on_surface_created()
            // for the freshly-created post-reinit surface will see the OLD
            // has_surface=true, skip the "first surface" branch, and never
            // sync the new state to SharedHandlerState — causing send_frame()
            // to fail forever with NoSurface ("black screen post-reinit" bug
            // observed in round 4 testing).
            self.has_surface.store(false, Ordering::Release);
            self.primary_surface_id.store(0, Ordering::Release);

            if let Some(ref shared) = self.shared_state {
                shared.needs_full_reinit.store(true, Ordering::Release);
                // Also propagate the local reset to SharedHandlerState now,
                // before on_ready's sync_shared_state runs — otherwise that
                // sync would copy STALE local has_surface=true downward.
                shared.has_surface.store(false, Ordering::Release);
                shared.primary_surface_id.store(0, Ordering::Release);
            } else {
                warn!(
                    "EGFX: re-advertise detected but no shared_state — re-init NOT triggered. \
                     Client will likely disconnect."
                );
            }
        } else {
            info!("EGFX: Client advertised {} capability sets", pdu.0.len());
        }
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

        // Client-requested AVC disable, independent of the avc420/avc444
        // codec selection above: V10.1 carries no flags at all (unit
        // variant) and V8/V8.1 have no AVC_DISABLED concept, so both report
        // false here. All other V10.x variants use one of four flag types
        // that each define the same bit (0x20).
        let needs_android_pointer_updates = match negotiated {
            CapabilitySet::V10 { flags } | CapabilitySet::V10_2 { flags } => {
                flags.contains(CapabilitiesV10Flags::AVC_DISABLED)
            }
            CapabilitySet::V10_3 { flags } => flags.contains(CapabilitiesV103Flags::AVC_DISABLED),
            CapabilitySet::V10_4 { flags }
            | CapabilitySet::V10_5 { flags }
            | CapabilitySet::V10_6 { flags }
            | CapabilitySet::V10_6Err { flags } => {
                flags.contains(CapabilitiesV104Flags::AVC_DISABLED)
            }
            CapabilitySet::V10_7 { flags } => flags.contains(CapabilitiesV107Flags::AVC_DISABLED),
            _ => false,
        };
        if needs_android_pointer_updates {
            info!(
                "EGFX: client negotiated AVC_DISABLED — applying Android RD Client pointer quirks"
            );
        }

        self.avc420_enabled.store(avc420, Ordering::Release);
        self.avc444_enabled
            .store(effective_avc444, Ordering::Release);
        self.needs_android_pointer_updates
            .store(needs_android_pointer_updates, Ordering::Release);
        self.ready.store(true, Ordering::Release);

        // Sync to shared state for EgfxFrameSender visibility
        self.sync_shared_state();

        // Sync EGFX snapshot for SnapshotCollector
        if let Some(ref snap) = self.egfx_snapshot {
            let version_str = match negotiated {
                CapabilitySet::V8 { .. } => "V8.0",
                CapabilitySet::V8_1 { .. } => "V8.1",
                CapabilitySet::V10 { .. } => "V10.0",
                CapabilitySet::V10_1 => "V10.1",
                CapabilitySet::V10_2 { .. } => "V10.2",
                CapabilitySet::V10_3 { .. } => "V10.3",
                CapabilitySet::V10_4 { .. } => "V10.4",
                CapabilitySet::V10_5 { .. } => "V10.5",
                CapabilitySet::V10_6 { .. } => "V10.6",
                CapabilitySet::V10_7 { .. } => "V10.7",
                _ => "Unknown",
            };
            let mut s = snap.write();
            s.channel_ready = true;
            s.avc420_enabled = avc420;
            s.avc444_enabled = effective_avc444;
            s.negotiated_version = Some(version_str.into());
        }

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

    fn on_frame_ack(&mut self, frame_id: u32, queue_depth: u32, total_frames_decoded: u32) {
        // Client-reported; clamp to a plausible range so a garbage value can't
        // skew backpressure/flow-control or the telemetry gauge. Real depths are
        // single digits (backpressure thresholds are 3/6).
        const MAX_PLAUSIBLE_QUEUE_DEPTH: u32 = 256;
        let queue_depth = if queue_depth > MAX_PLAUSIBLE_QUEUE_DEPTH {
            debug!(
                "EGFX: clamping implausible client queue_depth {queue_depth} to {MAX_PLAUSIBLE_QUEUE_DEPTH}"
            );
            MAX_PLAUSIBLE_QUEUE_DEPTH
        } else {
            queue_depth
        };

        trace!(
            "EGFX: frame_ack id={}, queue_depth={}",
            frame_id, queue_depth
        );

        // Persist the client's decoded-frame counter + queue depth to shared
        // state so display_handler / flow-control logic can compute frame_delay
        // = encoded_frames_total - total_frames_decoded. See gfx_factory.rs.
        if let Some(ref shared) = self.shared_state {
            shared
                .last_total_frames_decoded
                .store(total_frames_decoded, Ordering::Release);
            shared
                .last_client_queue_depth
                .store(queue_depth, Ordering::Release);
            // Feed the closed-loop flow controller. ack_frame computes RTT
            // (encode→ack latency) and may transition the controller out of
            // Active/LoweringLatency once unacked frames drain.
            if let Ok(mut fc) = shared.flow_controller.lock() {
                fc.ack_frame(frame_id);
            }
        }

        if let Some(ref m) = self.metrics {
            m.set_gauge(metric_names::EGFX_QUEUE_DEPTH, f64::from(queue_depth));
            m.increment_counter(metric_names::EGFX_FRAME_ACKS, 1);
        }
        if let Some(ref snap) = self.egfx_snapshot {
            let mut s = snap.write();
            s.queue_depth = queue_depth;
            s.frame_acks += 1;
        }
    }

    fn on_qoe_metrics(&mut self, qoe: QoeMetrics) {
        debug!(
            "EGFX: QoE metrics - frame {}, decode+render: {}μs",
            qoe.frame_id, qoe.time_diff_dr
        );

        if let Some(ref m) = self.metrics {
            m.record_histogram(
                metric_names::CLIENT_DECODE_RENDER_US,
                f64::from(qoe.time_diff_dr),
            );
        }
        if let Some(ref snap) = self.egfx_snapshot {
            let mut s = snap.write();
            s.client_decode_render_us = Some(u64::from(qoe.time_diff_dr));
        }
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
            self.sync_shared_state();
            info!("EGFX: Surface {} set as primary", surface.id);

            // EGFX is fully ready when capabilities are negotiated AND a surface exists
            if self.ready.load(Ordering::Acquire)
                && let Some(ref reporter) = self.health_reporter
            {
                let version = self
                    .egfx_snapshot
                    .as_ref()
                    .and_then(|s| s.read().negotiated_version.clone())
                    .unwrap_or_else(|| "unknown".into());
                reporter.report(crate::health::HealthEvent::EgfxChannelReady { version });
            }
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

        if let Some(ref snap) = self.egfx_snapshot {
            let mut s = snap.write();
            s.channel_ready = false;
        }

        if let Some(ref reporter) = self.health_reporter {
            reporter.report(crate::health::HealthEvent::EgfxChannelClosed {
                reason: "DVC channel closed".into(),
            });
        }
    }

    fn max_frames_in_flight(&self) -> u32 {
        // Use configured value for backpressure control
        self.max_frames_in_flight
    }

    fn preferred_capabilities(&self) -> Vec<CapabilitySet> {
        use ironrdp_egfx::pdu::{
            CapabilitiesV10Flags, CapabilitiesV103Flags, CapabilitiesV104Flags,
            CapabilitiesV107Flags,
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
            // V8: clients without AVC support (rdpdo, ironrdp-web, minimal clients).
            // EGFX frames are sent via WireToSurface1 with Codec1Type::Uncompressed.
            CapabilitySet::V8 {
                flags: CapabilitiesV8Flags::SMALL_CACHE,
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
        self.inner.read().is_ok_and(|h| h.is_ready())
    }

    pub fn client_supports_avc420(&self) -> bool {
        self.inner.read().is_ok_and(|h| h.client_supports_avc420())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn avc_disabled_flag_is_detected_across_all_v10_variants() {
        let cases = [
            CapabilitySet::V10 {
                flags: CapabilitiesV10Flags::AVC_DISABLED,
            },
            CapabilitySet::V10_2 {
                flags: CapabilitiesV10Flags::AVC_DISABLED,
            },
            CapabilitySet::V10_3 {
                flags: CapabilitiesV103Flags::AVC_DISABLED,
            },
            CapabilitySet::V10_4 {
                flags: CapabilitiesV104Flags::AVC_DISABLED,
            },
            CapabilitySet::V10_5 {
                flags: CapabilitiesV104Flags::AVC_DISABLED,
            },
            CapabilitySet::V10_6 {
                flags: CapabilitiesV104Flags::AVC_DISABLED,
            },
            CapabilitySet::V10_6Err {
                flags: CapabilitiesV104Flags::AVC_DISABLED,
            },
            CapabilitySet::V10_7 {
                flags: CapabilitiesV107Flags::AVC_DISABLED,
            },
        ];
        for negotiated in cases {
            let mut handler = LamcoGraphicsHandler::new(1920, 1080);
            handler.on_ready(&negotiated);
            assert!(
                handler.needs_android_pointer_updates(),
                "{negotiated:?} should have set needs_android_pointer_updates"
            );
        }
    }

    #[test]
    fn without_avc_disabled_flag_no_android_quirk_is_applied() {
        let cases = [
            CapabilitySet::V10 {
                flags: CapabilitiesV10Flags::empty(),
            },
            CapabilitySet::V10_1,
            CapabilitySet::V10_7 {
                flags: CapabilitiesV107Flags::empty(),
            },
            CapabilitySet::V8_1 {
                flags: CapabilitiesV81Flags::AVC420_ENABLED,
            },
        ];
        for negotiated in cases {
            let mut handler = LamcoGraphicsHandler::new(1920, 1080);
            handler.on_ready(&negotiated);
            assert!(
                !handler.needs_android_pointer_updates(),
                "{negotiated:?} should NOT have set needs_android_pointer_updates"
            );
        }
    }

    #[test]
    fn needs_android_pointer_updates_syncs_to_shared_state() {
        let shared = Arc::new(SharedHandlerState::new());
        let mut handler = LamcoGraphicsHandler::with_shared_state_and_quirks(
            1920,
            1080,
            Arc::clone(&shared),
            false,
        );
        handler.on_ready(&CapabilitySet::V10 {
            flags: CapabilitiesV10Flags::AVC_DISABLED,
        });
        assert!(shared.needs_android_pointer_updates.load(Ordering::Acquire));
    }
}
