//! Session Health Monitor Task
//!
//! Aggregates health events from all subsystems into a unified
//! `SessionHealthState`, broadcast via `tokio::sync::watch`.

use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
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

        self.state_tx.send_modify(|state| {
            let old_overall = state.overall;

            match event {
                HealthEvent::SessionClosed { ref reason } => {
                    error!("Session closed by compositor: {reason}");
                    state.session = SubsystemHealth::Failed(reason.clone());
                    // Session closure cascades — input and clipboard also fail
                    state.input = SubsystemHealth::Failed("session closed".into());
                    state.clipboard = SubsystemHealth::Failed("session closed".into());
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
                    }
                    VideoStreamState::Paused => {
                        warn!("Video stream paused");
                        state.video = SubsystemHealth::Degraded("PipeWire stream paused".into());
                    }
                    VideoStreamState::Error => {
                        error!("Video stream error");
                        state.video = SubsystemHealth::Failed("PipeWire stream error".into());
                    }
                },

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

                HealthEvent::SubsystemNotAvailable { ref subsystem } => {
                    debug!("{subsystem} not available in this session");
                    match subsystem.as_str() {
                        "clipboard" => state.clipboard = SubsystemHealth::NotApplicable,
                        "input" => state.input = SubsystemHealth::NotApplicable,
                        _ => {}
                    }
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
        let (monitor, reporter, subscriber) = SessionHealthMonitor::new(shutdown_rx);

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
        let (monitor, reporter, subscriber) = SessionHealthMonitor::new(shutdown_rx);

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
        let (monitor, reporter, subscriber) = SessionHealthMonitor::new(shutdown_rx);

        let monitor_handle = tokio::spawn(monitor.run());

        reporter.report(HealthEvent::VideoStreamStateChanged {
            state: VideoStreamState::Paused,
        });

        tokio::task::yield_now().await;
        tokio::task::yield_now().await;

        let state = subscriber.current();
        assert_eq!(state.overall, OverallHealth::Degraded);
        assert!(!state.video.is_healthy());
        // Session is still valid even when degraded
        assert!(subscriber.is_session_valid());

        let _ = shutdown_tx.send(());
        let _ = monitor_handle.await;
    }

    #[tokio::test]
    async fn test_monitor_recovery() {
        let (shutdown_tx, _) = tokio::sync::broadcast::channel(1);
        let shutdown_rx = shutdown_tx.subscribe();
        let (monitor, reporter, subscriber) = SessionHealthMonitor::new(shutdown_rx);

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
}
