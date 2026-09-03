//! Session Health Monitor Task
//!
//! Aggregates health events from all subsystems into a unified
//! `SessionHealthState`, broadcast via `tokio::sync::watch`.

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use tokio::sync::{mpsc, watch};
use tracing::{debug, error, info, warn};

use super::{
    HealthEvent, HealthReporter, HealthSubscriber, OverallHealth, SessionHealthState,
    SubsystemHealth, VideoStreamState,
};

/// Central health monitor that aggregates subsystem events.
///
/// Create via `SessionHealthMonitor::new()`, which returns the monitor
/// plus a `HealthReporter` (for subsystems to send events) and a
/// `HealthSubscriber` (for subsystems to read current health).
pub struct SessionHealthMonitor {
    /// Receives health events from subsystems
    event_rx: mpsc::UnboundedReceiver<HealthEvent>,
    /// Broadcasts aggregated health state
    state_tx: watch::Sender<SessionHealthState>,
    /// Backwards-compatible session validity flag
    session_valid: Arc<AtomicBool>,
    /// Shutdown signal
    shutdown: tokio::sync::broadcast::Receiver<()>,
    /// Whether an RDP client is currently connected (shared with the display
    /// handler). Between clients a paused capture stream is the idle state, not
    /// a degradation, so the Paused handler consults this before degrading.
    client_active: Arc<AtomicBool>,
}

impl SessionHealthMonitor {
    /// Create a new health monitor with reporter and subscriber handles.
    ///
    /// The returned `HealthReporter` should be cloned and distributed to
    /// subsystems that need to report health events.
    ///
    /// The returned `HealthSubscriber` should be cloned and distributed to
    /// subsystems that need to read health state.
    pub fn new(
        shutdown: tokio::sync::broadcast::Receiver<()>,
        client_active: Arc<AtomicBool>,
    ) -> (Self, HealthReporter, HealthSubscriber) {
        let (event_tx, event_rx) = mpsc::unbounded_channel();
        let (state_tx, state_rx) = watch::channel(SessionHealthState::default());
        let session_valid = Arc::new(AtomicBool::new(true));

        let reporter = HealthReporter { tx: event_tx };
        let subscriber = HealthSubscriber {
            rx: state_rx,
            session_valid: Arc::clone(&session_valid),
        };

        let monitor = Self {
            event_rx,
            state_tx,
            session_valid,
            shutdown,
            client_active,
        };

        (monitor, reporter, subscriber)
    }

    /// Run the health monitor event loop.
    ///
    /// Consumes self. Runs until shutdown signal or all reporters are dropped.
    pub async fn run(mut self) {
        info!("Session health monitor started");

        loop {
            let event = tokio::select! {
                Some(event) = self.event_rx.recv() => event,
                _ = self.shutdown.recv() => {
                    info!("Health monitor received shutdown");
                    break;
                }
            };

            self.handle_event(event);
        }

        info!("Session health monitor stopped");
    }

    fn handle_event(&self, event: HealthEvent) {
        debug!("Health event: {event:?}");

        let client_active = self.client_active.load(Ordering::Acquire);
        self.state_tx.send_modify(|state| {
            let old_overall = state.overall;

            match event {
                HealthEvent::SessionClosed { ref reason } => {
                    if client_active {
                        error!("Session closed by compositor: {reason}");
                        state.session = SubsystemHealth::Failed(reason.clone());
                        // Session closure cascades — input and clipboard also fail
                        state.input = SubsystemHealth::Failed("session closed".into());
                        state.clipboard = SubsystemHealth::Failed("session closed".into());
                    } else {
                        // No client connected: the compositor closing an idle
                        // session is the expected PerConnection teardown, not a
                        // failure — it re-establishes on the next connect. Reset
                        // to healthy so health doesn't stick at "failed" while idle.
                        debug!("Session closed while idle (no client) — expected teardown, not a failure");
                        state.session = SubsystemHealth::Healthy;
                        state.input = SubsystemHealth::Healthy;
                        state.clipboard = SubsystemHealth::Healthy;
                    }
                }

                HealthEvent::SessionInvalidated { ref reason } => {
                    warn!("Session invalidated: {reason}");
                    state.session = SubsystemHealth::Failed(reason.clone());
                    // Session invalidation cascades — D-Bus calls to input and
                    // clipboard will also fail since they share the Portal session
                    state.input = SubsystemHealth::Failed("session invalidated".into());
                    state.clipboard = SubsystemHealth::Failed("session invalidated".into());
                }

                HealthEvent::VideoStreamStateChanged {
                    state: stream_state,
                } => match stream_state {
                    VideoStreamState::Streaming => {
                        if !state.video.is_healthy() {
                            info!("Video stream recovered: streaming");
                        }
                        state.video = SubsystemHealth::Healthy;
                        // If input was degraded due to stream pause (Portal coupling),
                        // recover it now that the stream is active again
                        if let SubsystemHealth::Degraded(ref reason) = state.input
                            && reason.contains("stream paused")
                        {
                            info!("Input recovered: stream resumed");
                            state.input = SubsystemHealth::Healthy;
                        }
                    }
                    VideoStreamState::Paused if !client_active => {
                        // No client connected: a paused capture stream is the
                        // idle state between clients (PerConnection releases the
                        // session on disconnect), not a degradation.
                        debug!("Video stream paused while idle (no client) — treating as healthy");
                        state.video = SubsystemHealth::Healthy;
                        if let SubsystemHealth::Degraded(ref reason) = state.input
                            && reason.contains("stream paused")
                        {
                            state.input = SubsystemHealth::Healthy;
                        }
                    }
                    VideoStreamState::Paused => {
                        warn!("Video stream paused");
                        state.video = SubsystemHealth::Degraded("PipeWire stream paused".into());
                        // On Portal sessions, input injection is coupled to stream state.
                        // GNOME rejects input D-Bus calls when the ScreenCast stream is
                        // not actively streaming. Mark input as degraded proactively
                        // (don't override a permanent failure).
                        if !state.input.is_failed() {
                            state.input =
                                SubsystemHealth::Degraded("stream paused — input suspended".into());
                        }
                    }
                    VideoStreamState::Error => {
                        error!("Video stream error");
                        state.video = SubsystemHealth::Failed("PipeWire stream error".into());
                    }
                },

                HealthEvent::VideoFrameStalled { stall_duration_ms } => {
                    // Damage-driven capture legitimately delivers no frames while
                    // the desktop is static, and on some compositors (e.g. KWin)
                    // the stream stays in the streaming state rather than pausing.
                    // A frame-timeout is therefore not by itself a health problem —
                    // it previously flapped the session degraded↔healthy on every
                    // idle. Log it for visibility and let the authoritative PipeWire
                    // stream-state events (Paused/Error/Unconnected) drive health.
                    debug!("Video frames stalled for {stall_duration_ms}ms (informational)");
                }

                HealthEvent::VideoFramesCorrupted { count, window_ms } => {
                    // Dropping these frames keeps the client's picture correct,
                    // so video is not failed. It does mean the compositor has
                    // stopped delivering usable content, which the user sees as
                    // a freeze, so it is worth a warning rather than a debug.
                    warn!(
                        "Compositor delivered {count} corrupted buffers in {window_ms}ms; frames dropped rather than encoded (compositor-side capture fault)"
                    );
                }

                HealthEvent::VideoFrameNeverStarted { elapsed_ms } => {
                    error!(
                        "No video frames received since session start ({}ms elapsed)",
                        elapsed_ms
                    );
                    state.video = SubsystemHealth::Failed(format!(
                        "capture never delivered frames ({elapsed_ms}ms)"
                    ));
                }

                HealthEvent::VideoFrameResumed => {
                    // Frame timing is idle-ambiguous, so it does not drive video
                    // health in general. But if video was degraded by a frame-ack
                    // stall, resumed frames mean the recovery took — clear it.
                    if let SubsystemHealth::Degraded(ref reason) = state.video
                        && reason.contains("frame-ack stall")
                    {
                        info!("Video frames resumed after frame-ack stall — recovered to healthy");
                        state.video = SubsystemHealth::Healthy;
                    } else {
                        debug!("Video frames resumed");
                    }
                }

                HealthEvent::VideoAckStalled { stalled_ms } => {
                    // A genuinely stuck stream: frames were outstanding and the
                    // client stopped acking (not an idle desktop, which has
                    // nothing outstanding). The flow controller already recovered
                    // via resume + IDR; mark video degraded for visibility and let
                    // VideoFrameResumed clear it.
                    warn!(
                        "Video frame-ack stall: client left a frame unacked for \
                         {stalled_ms}ms — recovered via IDR"
                    );
                    state.video = SubsystemHealth::Degraded(
                        "client frame-ack stall — recovered via IDR".into(),
                    );
                }

                HealthEvent::InputFailed {
                    ref reason,
                    permanent,
                } => {
                    if permanent {
                        error!("Input permanently failed: {reason}");
                        state.input = SubsystemHealth::Failed(reason.clone());
                    } else {
                        warn!("Input transiently failed: {reason}");
                        state.input = SubsystemHealth::Degraded(reason.clone());
                    }
                }

                HealthEvent::InputRecovered => {
                    if !state.input.is_healthy() {
                        info!("Input recovered");
                        state.input = SubsystemHealth::Healthy;
                    }
                }

                HealthEvent::ClipboardFailed { ref reason } => {
                    warn!("Clipboard failed: {reason}");
                    state.clipboard = SubsystemHealth::Failed(reason.clone());
                }

                HealthEvent::ClipboardRecovered => {
                    if !state.clipboard.is_healthy() {
                        info!("Clipboard recovered");
                        state.clipboard = SubsystemHealth::Healthy;
                    }
                }

                HealthEvent::CompositorLost { ref bus_name } => {
                    error!("Compositor D-Bus name lost: {bus_name}");
                    state.session = SubsystemHealth::Failed(format!("compositor lost: {bus_name}"));
                    state.input = SubsystemHealth::Failed("compositor lost".into());
                    state.video = SubsystemHealth::Degraded("compositor may have restarted".into());
                }

                HealthEvent::EisStreamEnded { ref reason } => {
                    warn!("EIS stream ended: {reason}");
                    state.input = SubsystemHealth::Failed(reason.clone());
                }

                HealthEvent::EisStreamRecovered => {
                    info!("EIS stream recovered -- input restored");
                    state.input = SubsystemHealth::Healthy;
                }

                HealthEvent::SubsystemNotAvailable { ref subsystem } => {
                    debug!("{subsystem} not available in this session");
                    match subsystem.as_str() {
                        "clipboard" => state.clipboard = SubsystemHealth::NotApplicable,
                        "input" => state.input = SubsystemHealth::NotApplicable,
                        _ => {}
                    }
                }

                HealthEvent::EgfxChannelClosed { ref reason } => {
                    if client_active {
                        warn!("EGFX channel closed: {reason}");
                        // EGFX closure degrades video but doesn't kill it —
                        // V8 bitmap fallback may still work for the client
                        if matches!(state.video, SubsystemHealth::Healthy) {
                            state.video = SubsystemHealth::Degraded(format!(
                                "EGFX channel closed: {reason}"
                            ));
                        }
                    } else {
                        // No client connected: the DVC channel closing is part of
                        // the client's own normal disconnect teardown, not a
                        // degradation — every disconnect closes its EGFX channel.
                        debug!(
                            "EGFX channel closed while idle (no client) — expected teardown: {reason}"
                        );
                    }
                }

                HealthEvent::EgfxChannelReady { ref version } => {
                    info!("EGFX channel ready: {version}");
                    // Recover video if it was degraded due to EGFX closure
                    if let SubsystemHealth::Degraded(ref msg) = state.video
                        && msg.contains("EGFX")
                    {
                        state.video = SubsystemHealth::Healthy;
                    }
                }

                HealthEvent::CompositorDamageHintsDistrusted { divergence_pp } => {
                    // Not a degradation: the pixel-diff fallback keeps video
                    // fully correct, just via a costlier detection path.
                    // Informational only, matching VideoFrameStalled's
                    // treatment of a self-corrected condition.
                    info!(
                        "Compositor damage hints distrusted for this connection ({divergence_pp:.1}pp divergence) — using pixel-diff fallback"
                    );
                }

                HealthEvent::InputBackendSelected { ref backend } => {
                    // Informational only -- which pointer backend a session
                    // is using doesn't itself indicate degraded health (the
                    // uinput fallback is a supported, working path), but it's
                    // worth surfacing for fleet-scale deployment audits.
                    info!("Session pointer input backend: {backend}");
                }
            }

            state.recompute_overall();

            // Mirror to AtomicBool for backwards compatibility
            let valid = !matches!(state.overall, OverallHealth::Invalid);
            self.session_valid.store(valid, Ordering::Release);

            if state.overall != old_overall {
                info!(
                    "Session health changed: {} → {}",
                    old_overall, state.overall
                );
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_monitor_session_closed() {
        let (shutdown_tx, _) = tokio::sync::broadcast::channel(1);
        let shutdown_rx = shutdown_tx.subscribe();
        let (monitor, reporter, subscriber) =
            SessionHealthMonitor::new(shutdown_rx, Arc::new(AtomicBool::new(true)));

        let monitor_handle = tokio::spawn(monitor.run());

        reporter.report(HealthEvent::SessionClosed {
            reason: "compositor destroyed session".into(),
        });

        // Small yield to let the monitor process
        tokio::task::yield_now().await;
        tokio::task::yield_now().await;

        let state = subscriber.current();
        assert_eq!(state.overall, OverallHealth::Invalid);
        assert!(state.session.is_failed());
        assert!(!subscriber.is_session_valid());

        let _ = shutdown_tx.send(());
        let _ = monitor_handle.await;
    }

    #[tokio::test]
    async fn test_monitor_session_invalidated_cascades() {
        let (shutdown_tx, _) = tokio::sync::broadcast::channel(1);
        let shutdown_rx = shutdown_tx.subscribe();
        let (monitor, reporter, subscriber) =
            SessionHealthMonitor::new(shutdown_rx, Arc::new(AtomicBool::new(true)));

        let monitor_handle = tokio::spawn(monitor.run());

        reporter.report(HealthEvent::SessionInvalidated {
            reason: "D-Bus: non-existing session".into(),
        });

        tokio::task::yield_now().await;
        tokio::task::yield_now().await;

        let state = subscriber.current();
        assert_eq!(state.overall, OverallHealth::Invalid);
        assert!(state.session.is_failed());
        assert!(state.input.is_failed());
        assert!(state.clipboard.is_failed());
        assert!(!subscriber.is_session_valid());

        let _ = shutdown_tx.send(());
        let _ = monitor_handle.await;
    }

    #[tokio::test]
    async fn test_monitor_video_paused_degrades() {
        let (shutdown_tx, _) = tokio::sync::broadcast::channel(1);
        let shutdown_rx = shutdown_tx.subscribe();
        let (monitor, reporter, subscriber) =
            SessionHealthMonitor::new(shutdown_rx, Arc::new(AtomicBool::new(true)));

        let monitor_handle = tokio::spawn(monitor.run());

        reporter.report(HealthEvent::VideoStreamStateChanged {
            state: VideoStreamState::Paused,
        });

        tokio::task::yield_now().await;
        tokio::task::yield_now().await;

        let state = subscriber.current();
        assert_eq!(state.overall, OverallHealth::Degraded);
        assert!(!state.video.is_healthy());
        // Stream pause cascades to input (Portal coupling)
        assert!(!state.input.is_healthy());
        // Session is still valid even when degraded
        assert!(subscriber.is_session_valid());

        let _ = shutdown_tx.send(());
        let _ = monitor_handle.await;
    }

    #[tokio::test]
    async fn test_monitor_video_paused_idle_no_client_stays_healthy() {
        // With no client connected, a paused capture stream is the idle state
        // between clients (PerConnection releases the session on disconnect),
        // not a degradation.
        let (shutdown_tx, _) = tokio::sync::broadcast::channel(1);
        let shutdown_rx = shutdown_tx.subscribe();
        let (monitor, reporter, subscriber) =
            SessionHealthMonitor::new(shutdown_rx, Arc::new(AtomicBool::new(false)));

        let monitor_handle = tokio::spawn(monitor.run());

        reporter.report(HealthEvent::VideoStreamStateChanged {
            state: VideoStreamState::Paused,
        });

        tokio::task::yield_now().await;
        tokio::task::yield_now().await;

        let state = subscriber.current();
        assert!(
            state.video.is_healthy(),
            "idle pause with no client should read healthy, got {:?}",
            state.video
        );
        assert!(
            state.input.is_healthy(),
            "idle pause with no client should not suspend input"
        );

        let _ = shutdown_tx.send(());
        let _ = monitor_handle.await;
    }

    #[tokio::test]
    async fn test_stream_resume_recovers_input() {
        let (shutdown_tx, _) = tokio::sync::broadcast::channel(1);
        let shutdown_rx = shutdown_tx.subscribe();
        let (monitor, reporter, subscriber) =
            SessionHealthMonitor::new(shutdown_rx, Arc::new(AtomicBool::new(true)));

        let monitor_handle = tokio::spawn(monitor.run());

        // Pause (degrades both video and input)
        reporter.report(HealthEvent::VideoStreamStateChanged {
            state: VideoStreamState::Paused,
        });
        tokio::task::yield_now().await;
        tokio::task::yield_now().await;

        assert!(!subscriber.current().input.is_healthy());

        // Resume (recovers both)
        reporter.report(HealthEvent::VideoStreamStateChanged {
            state: VideoStreamState::Streaming,
        });
        tokio::task::yield_now().await;
        tokio::task::yield_now().await;

        let state = subscriber.current();
        assert!(state.video.is_healthy());
        assert!(state.input.is_healthy());
        assert_eq!(state.overall, OverallHealth::Healthy);

        let _ = shutdown_tx.send(());
        let _ = monitor_handle.await;
    }

    #[tokio::test]
    async fn test_stream_pause_doesnt_override_failed_input() {
        let (shutdown_tx, _) = tokio::sync::broadcast::channel(1);
        let shutdown_rx = shutdown_tx.subscribe();
        let (monitor, reporter, subscriber) =
            SessionHealthMonitor::new(shutdown_rx, Arc::new(AtomicBool::new(true)));

        let monitor_handle = tokio::spawn(monitor.run());

        // Permanently fail input first
        reporter.report(HealthEvent::InputFailed {
            reason: "permanent failure".into(),
            permanent: true,
        });
        tokio::task::yield_now().await;
        tokio::task::yield_now().await;
        assert!(subscriber.current().input.is_failed());

        // Stream pause should NOT downgrade Failed to Degraded
        reporter.report(HealthEvent::VideoStreamStateChanged {
            state: VideoStreamState::Paused,
        });
        tokio::task::yield_now().await;
        tokio::task::yield_now().await;

        // Input should still be Failed, not Degraded
        assert!(subscriber.current().input.is_failed());

        let _ = shutdown_tx.send(());
        let _ = monitor_handle.await;
    }

    #[tokio::test]
    async fn test_monitor_recovery() {
        let (shutdown_tx, _) = tokio::sync::broadcast::channel(1);
        let shutdown_rx = shutdown_tx.subscribe();
        let (monitor, reporter, subscriber) =
            SessionHealthMonitor::new(shutdown_rx, Arc::new(AtomicBool::new(true)));

        let monitor_handle = tokio::spawn(monitor.run());

        // Degrade then recover
        reporter.report(HealthEvent::InputFailed {
            reason: "transient".into(),
            permanent: false,
        });
        tokio::task::yield_now().await;
        tokio::task::yield_now().await;

        assert_eq!(subscriber.current().overall, OverallHealth::Degraded);

        reporter.report(HealthEvent::InputRecovered);
        tokio::task::yield_now().await;
        tokio::task::yield_now().await;

        assert_eq!(subscriber.current().overall, OverallHealth::Healthy);

        let _ = shutdown_tx.send(());
        let _ = monitor_handle.await;
    }

    #[tokio::test]
    async fn test_monitor_video_stall_is_informational() {
        let (shutdown_tx, _) = tokio::sync::broadcast::channel(1);
        let shutdown_rx = shutdown_tx.subscribe();
        let (monitor, reporter, subscriber) =
            SessionHealthMonitor::new(shutdown_rx, Arc::new(AtomicBool::new(true)));

        let monitor_handle = tokio::spawn(monitor.run());

        // A frame-timeout is informational only: a static desktop legitimately
        // produces no frames (damage-driven capture), so a stall must NOT
        // degrade health. Video health is driven by PipeWire stream state.
        reporter.report(HealthEvent::VideoFrameStalled {
            stall_duration_ms: 5000,
        });
        tokio::task::yield_now().await;
        tokio::task::yield_now().await;

        let state = subscriber.current();
        assert_eq!(state.overall, OverallHealth::Healthy);
        assert!(state.video.is_healthy());
        assert!(subscriber.is_session_valid());

        // Resume is likewise informational and leaves health unchanged.
        reporter.report(HealthEvent::VideoFrameResumed);
        tokio::task::yield_now().await;
        tokio::task::yield_now().await;

        assert_eq!(subscriber.current().overall, OverallHealth::Healthy);
        assert!(subscriber.current().video.is_healthy());

        let _ = shutdown_tx.send(());
        let _ = monitor_handle.await;
    }
}
