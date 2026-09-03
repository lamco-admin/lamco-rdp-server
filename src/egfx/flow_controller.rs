//! EGFX flow controller — closed-loop encoder rate control.
//!
//! Mirrors GNOME Remote Desktop's `GrdRdpGfxFrameController` architecture.
//! See `docs/analysis/MSTSC-MIDSESSION-CAPSADVERTISE-COMPREHENSIVE-2026-05-23.md`
//! Appendix § 10 for the algorithm reference and rationale.
//!
//! # Problem this solves
//!
//! Without flow control we ship encoded frames as fast as PipeWire produces
//! them. If the client (mstsc, xfreerdp, etc) decoder can't keep up — under
//! load, slow CPU, network congestion — its queue grows unbounded. Eventually
//! the client either: (a) emits `RDPGFX_CAPSADVERTISE` as a decoder-recovery
//! sequence (we handle this via L1), or (b) gives up and closes the Graphics
//! DVC channel (round-6 observed `queue_depth=73757` immediately preceding a
//! `DRDYNVC Close → TCP RST`).
//!
//! # Algorithm (3-state machine)
//!
//! ```text
//!                          n_unacked >= activate_th
//! Inactive (slots=∞)  ────────────────────────────►  Active (slots=0)
//!       ▲                                                  │
//!       │  n_unacked < deactivate_th                       │ RTT degraded
//!       │                                                  │ (new_th < old_th)
//!       │                                                  ▼
//!       │                                ActiveLoweringLatency (slots=0)
//!       │                                                  │
//!       └─────────────── n_unacked < deactivate_th ────────┘
//! ```
//!
//! Threshold is RTT-adaptive: `MAX(2, MIN(delayed_frames + 2, refresh_rate))`
//! where `delayed_frames = rtt * refresh_rate / 1s`.
//!
//! # RTT source
//!
//! Currently derived from FrameAcknowledge round-trip: time from frame-send
//! to frame-ack-received. This is approximately the network RTT plus client
//! decode time and any client-side queue delay.
//!
//! NetworkAutoDetect (`[MS-RDPBCGR]` § 2.2.14) provides a complementary,
//! pure-network RTT signal via `ironrdp-server`'s `AutoDetectManager`. The qemu
//! console drives `ServerEvent::AutoDetectRttRequest` on a cadence and the
//! server publishes the latest measurement through an `Arc<AtomicU32>` handle
//! (`RdpServer::autodetect_rtt_handle`, upstream PR #1346). `effective_rtt`
//! fuses the two (R2b, freshness-floor policy): the FrameAck-derived `avg_rtt`
//! while a sample is fresh, otherwise the autodetect RTT so an idle gap doesn't
//! leave the threshold on a stale average. See
//! `docs/design/PROXMOX-CONSOLE-REVIVAL-PLAN-2026-05-31.md` (R2b) and
//! `docs/analysis/spec/lamco-rdp-server/AUTODETECT-CONSUMER-AUDIT-2026-05-23.md`.

use std::{
    collections::VecDeque,
    sync::{
        Arc,
        atomic::{AtomicU32, Ordering},
    },
    time::{Duration, Instant},
};

use tracing::{debug, info, warn};

/// Default activation threshold — at minimum require 2 unacked frames before
/// considering throttling. Matches GNOME RD `ACTIVATE_THROTTLING_TH_DEFAULT`.
const ACTIVATE_THROTTLING_TH_DEFAULT: u32 = 2;

/// Deactivation hysteresis — leave throttling when unacked drops below this.
/// Matches GNOME RD `DEACTIVATE_THROTTLING_TH_DEFAULT`.
const DEACTIVATE_THROTTLING_TH_DEFAULT: u32 = 1;

/// Sentinel for "no encoder pacing limit" — encoder may produce at full rate.
pub const UNLIMITED_FRAME_SLOTS: u32 = u32::MAX;

/// RTT samples are averaged over a sliding window of this duration.
/// 500ms matches GNOME RD `RTT_AVG_PERIOD_US`.
const RTT_AVG_WINDOW: Duration = Duration::from_millis(500);

/// Encode/ack rate is computed over this window.
#[expect(
    clippy::duration_suboptimal_units,
    reason = "ms is the natural unit for every other timing constant in this module \
              (RTT_AVG_WINDOW=500ms, avg_rtt initial=50ms, RTT samples in ms); keeping \
              this in ms preserves a single mental model for flow-control timing."
)]
const RATE_WINDOW: Duration = Duration::from_millis(1000);

/// Bound the frame history. At 60fps with 1s RTT we'd track ~60 frames; double
/// it for safety. Old entries get garbage-collected on each unack/ack call.
const MAX_FRAME_HISTORY: usize = 240;

/// Maximum RTT we'll honor before clamping the threshold formula. Without this,
/// a transient 10s spike (e.g. NIC stall) would set `activate_th = refresh_rate`
/// which prevents throttling for a long time. 2 seconds covers all realistic
/// LAN/WAN cases while bounding the worst-case threshold growth.
const RTT_CLAMP: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThrottlingState {
    /// Encoder runs at full rate; pacing slots = ∞.
    Inactive,
    /// Encoder suspended (slots=0) because unacked frame count exceeded
    /// threshold. Will lift when ack rate catches up.
    Active,
    /// Like Active, but the throttling threshold itself was just reduced
    /// because RTT improved. We hold encoder off until unacked drops below
    /// the new (lower) threshold to actually realize the latency benefit.
    ActiveLoweringLatency,
}

#[derive(Debug, Clone, Copy)]
struct FrameRecord {
    frame_id: u32,
    encoded_at: Instant,
    acked_at: Option<Instant>,
    /// 1 for P-slice (Main only), 2 for AVC444 IDR (Main + Aux).
    /// Used by callers for diagnostic logging; the controller treats all
    /// frames as a single slot for now.
    #[expect(dead_code, reason = "reserved for future dual-frame accounting")]
    n_subframes: u32,
}

/// Configuration injected at construction. Defaults follow GNOME RD.
#[derive(Debug, Clone, Copy)]
pub struct FlowControllerConfig {
    /// Target client refresh rate, used in the adaptive threshold formula.
    /// 30fps default matches our common target.
    pub refresh_rate: u32,
    /// Override for the activation threshold default. Most users should keep
    /// `ACTIVATE_THROTTLING_TH_DEFAULT`; configurable for testing.
    pub activate_th_floor: u32,
    pub deactivate_th: u32,
}

impl Default for FlowControllerConfig {
    fn default() -> Self {
        Self {
            refresh_rate: 30,
            activate_th_floor: ACTIVATE_THROTTLING_TH_DEFAULT,
            deactivate_th: DEACTIVATE_THROTTLING_TH_DEFAULT,
        }
    }
}

/// Closed-loop EGFX flow controller. Owned by SharedHandlerState behind a
/// Mutex; both `on_frame_ack_full` (FrameAck handler) and the display loop
/// (frame send + L1 reinit) modify it.
pub struct FlowController {
    config: FlowControllerConfig,

    /// Bounded ring of frame records. Append on encode (`unack_frame`),
    /// mutate-in-place to acked on ack (`ack_frame`), drop oldest on overflow.
    frames: VecDeque<FrameRecord>,

    /// RTT sliding window. Push on every ack with the measured frame RTT.
    /// Old entries trimmed on each access.
    rtt_samples: VecDeque<(Instant, Duration)>,

    /// Computed average over `RTT_AVG_WINDOW`. Initialized conservatively.
    avg_rtt: Duration,

    /// Current state machine position.
    state: ThrottlingState,

    /// Current activation threshold (computed from RTT). The deactivation
    /// threshold is constant; only activation moves with RTT.
    activate_th: u32,

    /// Pacing budget — 0 means "stop encoding", N means "encode at most N
    /// more frames before next ack", UNLIMITED_FRAME_SLOTS means "go".
    total_frame_slots: u32,

    /// Stats — incremented monotonically, useful for diagnostic logs.
    stats_throttle_entries: u64,
    stats_throttle_exits: u64,

    /// Optional NetworkAutoDetect RTT handle (milliseconds, `u32::MAX` until
    /// measured), shared with the RDP server that writes the latest probe RTT.
    /// Freshness floor: when no FrameAck-derived sample is recent, the threshold
    /// uses this pure-network RTT instead of a stale FrameAck average. See
    /// `effective_rtt`.
    autodetect_rtt: Option<Arc<AtomicU32>>,
}

impl FlowController {
    pub fn new(config: FlowControllerConfig) -> Self {
        Self {
            config,
            frames: VecDeque::with_capacity(MAX_FRAME_HISTORY),
            rtt_samples: VecDeque::with_capacity(64),
            // Conservative default — 50ms is typical LAN RTT. Updated on first
            // ack; until then we use this for threshold calculation.
            avg_rtt: Duration::from_millis(50),
            state: ThrottlingState::Inactive,
            activate_th: config.activate_th_floor,
            total_frame_slots: UNLIMITED_FRAME_SLOTS,
            stats_throttle_entries: 0,
            stats_throttle_exits: 0,
            autodetect_rtt: None,
        }
    }

    /// Wire a shared NetworkAutoDetect RTT handle. The server writes the latest
    /// measured RTT into it; the controller reads it as a freshness floor (see
    /// `effective_rtt`).
    pub fn set_autodetect_rtt_handle(&mut self, handle: Arc<AtomicU32>) {
        self.autodetect_rtt = Some(handle);
    }

    /// Record that a frame has been encoded and submitted. Updates state
    /// machine; may transition Inactive → Active if unacked exceeds threshold.
    pub fn unack_frame(&mut self, frame_id: u32, n_subframes: u32) {
        let now = Instant::now();
        self.frames.push_back(FrameRecord {
            frame_id,
            encoded_at: now,
            acked_at: None,
            n_subframes,
        });
        while self.frames.len() > MAX_FRAME_HISTORY {
            self.frames.pop_front();
        }

        let n_unacked = self.unacked_count();
        let new_activate_th = self.adaptive_threshold();

        match self.state {
            ThrottlingState::Inactive => {
                self.activate_th = new_activate_th;
                if n_unacked >= self.activate_th {
                    self.enter_active("unack_frame threshold crossed", n_unacked);
                }
            }
            ThrottlingState::Active => {
                if new_activate_th < self.activate_th {
                    // RTT improved — but we have more unacked frames than the
                    // new threshold. Stay throttled, switch to LoweringLatency.
                    debug!(
                        old_th = self.activate_th,
                        new_th = new_activate_th,
                        n_unacked,
                        "EGFX flow controller: RTT improved, entering LoweringLatency"
                    );
                    self.state = ThrottlingState::ActiveLoweringLatency;
                    self.total_frame_slots = 0;
                } else {
                    self.activate_th = new_activate_th;
                    self.recompute_slots_from_rates(true);
                }
            }
            ThrottlingState::ActiveLoweringLatency => {
                // Encoder is suspended; nothing to do on additional unacks.
            }
        }
    }

    /// Record that the client acked a frame. Computes RTT, updates state
    /// machine; may transition Active → Inactive if unacked falls below.
    pub fn ack_frame(&mut self, frame_id: u32) {
        let now = Instant::now();
        let mut found_rtt: Option<Duration> = None;
        for f in &mut self.frames {
            if f.frame_id == frame_id && f.acked_at.is_none() {
                f.acked_at = Some(now);
                found_rtt = Some(now.saturating_duration_since(f.encoded_at));
                break;
            }
        }
        if let Some(rtt) = found_rtt {
            self.rtt_samples.push_back((now, rtt));
            self.update_avg_rtt(now);
        }

        let n_unacked = self.unacked_count();

        match self.state {
            ThrottlingState::Inactive => {
                // Still no pressure; nothing to do.
            }
            ThrottlingState::Active => {
                if n_unacked < self.config.deactivate_th {
                    self.exit_to_inactive("ack drained below deactivate threshold");
                } else {
                    let new_th = self.adaptive_threshold();
                    if new_th < self.activate_th {
                        debug!(
                            old_th = self.activate_th,
                            new_th,
                            n_unacked,
                            "EGFX flow controller: ack-side RTT improved, entering LoweringLatency"
                        );
                        self.state = ThrottlingState::ActiveLoweringLatency;
                        self.total_frame_slots = 0;
                    } else {
                        self.activate_th = new_th;
                        self.recompute_slots_from_rates(false);
                    }
                }
            }
            ThrottlingState::ActiveLoweringLatency => {
                let new_th = self.adaptive_threshold();
                if n_unacked < self.config.deactivate_th {
                    self.exit_to_inactive("LoweringLatency drained below deactivate");
                } else if n_unacked <= new_th {
                    // We've drained below the new threshold; resume normal Active mode.
                    self.state = ThrottlingState::Active;
                    self.activate_th = new_th;
                    self.recompute_slots_from_rates(false);
                }
                // else stay in LoweringLatency
            }
        }
    }

    /// Drop all unacked-frame history and reset to Inactive. Call this on
    /// L1 re-init (mid-session CapsAdvertise) — the client has discarded its
    /// view of pending frames, so our view must agree.
    pub fn clear_all_unacked(&mut self) {
        self.frames.clear();
        self.rtt_samples.clear();
        if self.state != ThrottlingState::Inactive {
            info!(
                from_state = ?self.state,
                "EGFX flow controller: clear_all_unacked (L1 reinit) — resetting to Inactive"
            );
            self.stats_throttle_exits += 1;
        }
        self.state = ThrottlingState::Inactive;
        self.activate_th = self.config.activate_th_floor;
        self.total_frame_slots = UNLIMITED_FRAME_SLOTS;
    }

    /// Break a stalled throttle. When the client stops acking — a decoder
    /// stall, a dropped ack, or a connection that has not yet surfaced as
    /// closed — the unacked count never drains and `should_throttle()` would
    /// stay true forever, freezing video. If the oldest unacked frame is older
    /// than `timeout`, treat all outstanding frames as lost, reset to Inactive
    /// so the encoder resumes, and return `Some(stalled_ms)` so the caller can
    /// force a fresh IDR to resynchronize the decoder and surface the stall to
    /// health. Returns `None` when there is nothing outstanding or the wait is
    /// still within the timeout.
    pub fn check_ack_timeout(&mut self, timeout: Duration) -> Option<u64> {
        let now = Instant::now();
        let oldest_unacked = self
            .frames
            .iter()
            .filter(|f| f.acked_at.is_none())
            .map(|f| f.encoded_at)
            .min()?;
        let stalled = now.saturating_duration_since(oldest_unacked);
        if stalled < timeout {
            return None;
        }
        let stalled_ms = stalled.as_millis() as u64;

        warn!(
            stalled_ms,
            unacked = self.unacked_count(),
            state = ?self.state,
            "EGFX flow controller: frame-ack stall exceeded timeout — dropping unacked \
             frames, resuming encoder, forcing IDR"
        );
        self.frames.clear();
        self.rtt_samples.clear();
        self.state = ThrottlingState::Inactive;
        self.activate_th = self.config.activate_th_floor;
        self.total_frame_slots = UNLIMITED_FRAME_SLOTS;
        self.stats_throttle_exits += 1;
        Some(stalled_ms)
    }

    /// Should the encoder pause? Returns true when `total_frame_slots == 0`.
    /// Display loop checks this before encoding each frame.
    pub fn should_throttle(&self) -> bool {
        self.total_frame_slots == 0
    }

    /// Diagnostic accessors.
    pub fn state(&self) -> ThrottlingState {
        self.state
    }
    pub fn avg_rtt(&self) -> Duration {
        self.avg_rtt
    }
    pub fn activate_th(&self) -> u32 {
        self.activate_th
    }
    pub fn unacked_count(&self) -> u32 {
        self.frames
            .iter()
            .filter(|f| f.acked_at.is_none())
            .count()
            .try_into()
            .unwrap_or(u32::MAX)
    }
    pub fn total_frame_slots(&self) -> u32 {
        self.total_frame_slots
    }
    pub fn stats(&self) -> (u64, u64) {
        (self.stats_throttle_entries, self.stats_throttle_exits)
    }

    // -- internal --

    /// RTT used for the adaptive threshold. Policy: the FrameAck-derived
    /// `avg_rtt` while a sample is fresh (within `RTT_AVG_WINDOW`); otherwise
    /// the NetworkAutoDetect RTT (refreshed independently of frame traffic), so
    /// an idle gap doesn't leave the threshold on a stale average; otherwise the
    /// last known `avg_rtt`.
    fn effective_rtt(&self) -> Duration {
        let frameack_fresh = self
            .rtt_samples
            .back()
            .is_some_and(|(t, _)| t.elapsed() < RTT_AVG_WINDOW);
        if frameack_fresh {
            return self.avg_rtt;
        }
        match self.autodetect_rtt_ms() {
            Some(rtt_ms) => Duration::from_millis(u64::from(rtt_ms)),
            None => self.avg_rtt,
        }
    }

    /// Latest NetworkAutoDetect RTT in ms, or `None` if no handle is wired or no
    /// measurement has arrived yet (sentinel `u32::MAX`).
    fn autodetect_rtt_ms(&self) -> Option<u32> {
        let rtt = self.autodetect_rtt.as_ref()?.load(Ordering::Relaxed);
        (rtt != u32::MAX).then_some(rtt)
    }

    fn adaptive_threshold(&self) -> u32 {
        let rtt = self.effective_rtt().min(RTT_CLAMP);
        let rtt_us = rtt.as_micros() as u64;
        let delayed_frames = rtt_us.saturating_mul(u64::from(self.config.refresh_rate)) / 1_000_000;
        // MAX(activate_th_floor, MIN(delayed_frames + 2, refresh_rate))
        let raw = delayed_frames
            .saturating_add(2)
            .min(u64::from(self.config.refresh_rate)) as u32;
        raw.max(self.config.activate_th_floor)
    }

    /// Recompute total_frame_slots from current enc_rate vs ack_rate.
    /// `is_from_unack`: true if called from unack_frame (use +1 margin),
    /// false if from ack_frame (use base margin). Matches grd's two
    /// slightly-different formulas in those two paths.
    fn recompute_slots_from_rates(&mut self, is_from_unack: bool) {
        let (enc_rate, ack_rate) = self.rates();
        let margin = if is_from_unack { 2 } else { 1 };
        self.total_frame_slots = if enc_rate > ack_rate + (margin - 1) {
            0
        } else {
            ack_rate.saturating_add(margin).saturating_sub(enc_rate)
        };
    }

    fn rates(&self) -> (u32, u32) {
        let now = Instant::now();
        let cutoff = now.checked_sub(RATE_WINDOW).unwrap_or(now);
        let mut enc = 0u32;
        let mut ack = 0u32;
        for f in &self.frames {
            if f.encoded_at >= cutoff {
                enc = enc.saturating_add(1);
            }
            if let Some(ack_t) = f.acked_at
                && ack_t >= cutoff
            {
                ack = ack.saturating_add(1);
            }
        }
        (enc, ack)
    }

    fn update_avg_rtt(&mut self, now: Instant) {
        let cutoff = now.checked_sub(RTT_AVG_WINDOW).unwrap_or(now);
        while let Some(&(t, _)) = self.rtt_samples.front() {
            if t < cutoff {
                self.rtt_samples.pop_front();
            } else {
                break;
            }
        }
        if self.rtt_samples.is_empty() {
            return;
        }
        let sum: Duration = self.rtt_samples.iter().map(|(_, r)| *r).sum();
        let denom = u32::try_from(self.rtt_samples.len()).unwrap_or(u32::MAX);
        self.avg_rtt = sum / denom.max(1);
    }

    fn enter_active(&mut self, reason: &str, n_unacked: u32) {
        info!(
            reason,
            n_unacked,
            activate_th = self.activate_th,
            avg_rtt_us = self.avg_rtt.as_micros() as u64,
            "EGFX flow controller: THROTTLING ENGAGED (Inactive → Active)"
        );
        self.state = ThrottlingState::Active;
        self.total_frame_slots = 0;
        self.stats_throttle_entries = self.stats_throttle_entries.saturating_add(1);
    }

    fn exit_to_inactive(&mut self, reason: &str) {
        let (enc_rate, ack_rate) = self.rates();
        info!(
            reason,
            avg_rtt_us = self.avg_rtt.as_micros() as u64,
            enc_rate,
            ack_rate,
            "EGFX flow controller: THROTTLING RELEASED (→ Inactive)"
        );
        self.state = ThrottlingState::Inactive;
        self.total_frame_slots = UNLIMITED_FRAME_SLOTS;
        self.stats_throttle_exits = self.stats_throttle_exits.saturating_add(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> FlowControllerConfig {
        FlowControllerConfig {
            refresh_rate: 30,
            activate_th_floor: 2,
            deactivate_th: 1,
        }
    }

    #[test]
    fn new_starts_inactive_unlimited() {
        let fc = FlowController::new(cfg());
        assert_eq!(fc.state(), ThrottlingState::Inactive);
        assert_eq!(fc.total_frame_slots(), UNLIMITED_FRAME_SLOTS);
        assert_eq!(fc.unacked_count(), 0);
        assert!(!fc.should_throttle());
    }

    #[test]
    fn unack_below_threshold_stays_inactive() {
        let mut fc = FlowController::new(cfg());
        fc.unack_frame(1, 1);
        assert_eq!(fc.state(), ThrottlingState::Inactive);
        assert!(!fc.should_throttle());
    }

    #[test]
    fn unack_reaching_threshold_engages_throttle() {
        let mut fc = FlowController::new(cfg());
        // avg_rtt=50ms default → delayed_frames = 50000us * 30 / 1000000 = 1
        // activate_th = max(2, min(1+2, 30)) = 3
        fc.unack_frame(1, 1);
        fc.unack_frame(2, 1);
        fc.unack_frame(3, 1);
        // 3 unacked, threshold=3 → engages
        assert_eq!(fc.state(), ThrottlingState::Active);
        assert!(fc.should_throttle());
    }

    #[test]
    fn ack_draining_releases_throttle() {
        let mut fc = FlowController::new(cfg());
        fc.unack_frame(1, 1);
        fc.unack_frame(2, 1);
        fc.unack_frame(3, 1);
        assert!(fc.should_throttle());
        fc.ack_frame(1);
        fc.ack_frame(2);
        fc.ack_frame(3);
        // n_unacked=0, deactivate_th=1 → released
        assert_eq!(fc.state(), ThrottlingState::Inactive);
        assert!(!fc.should_throttle());
    }

    #[test]
    fn clear_all_unacked_resets_state() {
        let mut fc = FlowController::new(cfg());
        fc.unack_frame(1, 1);
        fc.unack_frame(2, 1);
        fc.unack_frame(3, 1);
        assert!(fc.should_throttle());
        fc.clear_all_unacked();
        assert_eq!(fc.state(), ThrottlingState::Inactive);
        assert_eq!(fc.unacked_count(), 0);
        assert!(!fc.should_throttle());
    }

    #[test]
    fn adaptive_threshold_scales_with_rtt() {
        let mut fc = FlowController::new(cfg());
        // Force avg_rtt to 100ms by injecting samples
        let now = Instant::now();
        fc.rtt_samples.push_back((now, Duration::from_millis(100)));
        fc.update_avg_rtt(now);
        // delayed_frames = 100000us * 30 / 1000000 = 3
        // activate_th = max(2, min(3+2, 30)) = 5
        assert_eq!(fc.adaptive_threshold(), 5);
    }

    #[test]
    fn effective_rtt_prefers_fresh_frameack_over_autodetect() {
        let mut fc = FlowController::new(cfg());
        fc.set_autodetect_rtt_handle(Arc::new(AtomicU32::new(200)));
        let now = Instant::now();
        fc.rtt_samples.push_back((now, Duration::from_millis(20)));
        fc.update_avg_rtt(now);
        assert_eq!(fc.effective_rtt(), Duration::from_millis(20));
    }

    #[test]
    fn effective_rtt_falls_back_to_autodetect_when_frameack_stale() {
        let mut fc = FlowController::new(cfg());
        let handle = Arc::new(AtomicU32::new(80));
        fc.set_autodetect_rtt_handle(Arc::clone(&handle));
        // No FrameAck samples: stale window -> use the autodetect RTT.
        assert_eq!(fc.effective_rtt(), Duration::from_millis(80));
        // Sentinel (no measurement yet) -> fall back to the last avg_rtt.
        handle.store(u32::MAX, Ordering::Relaxed);
        assert_eq!(fc.effective_rtt(), fc.avg_rtt);
    }

    #[test]
    fn effective_rtt_is_avg_rtt_without_a_handle() {
        let fc = FlowController::new(cfg());
        assert_eq!(fc.effective_rtt(), fc.avg_rtt);
    }
}
