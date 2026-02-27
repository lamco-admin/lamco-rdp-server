//! Session Health Subsystem
//!
//! **Execution Path:** Cross-cutting (all session strategies)
//! **Status:** Active (v1.5.0+)
//! **Platform:** Universal
//!
//! Provides runtime liveness monitoring for all session subsystems (video, input,
//! clipboard, session lifecycle). Complements the `ServiceRegistry`, which tracks
//! static capabilities detected at startup — this module tracks whether those
//! capabilities are currently functioning.
//!
//! # Architecture
//!
//! ```text
//! D-Bus Signals / Stream Events
//!   Portal Closed ──────┐
//!   Mutter Closed ──────┤
//!   NameOwnerChanged ───┤     ┌──────────────────┐
//!   EIS stream EOF ─────┤     │                  │
//!                       ├────▶│  Health Monitor   │──▶ watch<SessionHealthState>
//!   PipeWire state ─────┤     │  Task             │
//!   Input errors ───────┤     │                  │
//!   Clipboard errors ───┘     └──────────────────┘
//! ```
//!
//! Three primitives:
//! - `tokio::sync::watch<SessionHealthState>` — broadcasts health to subscribers
//! - `tokio::sync::mpsc<HealthEvent>` — subsystems report events to monitor
//! - `Arc<AtomicBool>` — backwards-compatible `session_valid` mirror

pub mod compositor_watcher;
mod monitor;

use std::{
    fmt,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};

pub use monitor::SessionHealthMonitor;

/// Health of a single subsystem
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubsystemHealth {
    /// Operating normally
    Healthy,
    /// Impaired but functional (e.g., PipeWire paused, clipboard degraded)
    Degraded(String),
    /// Not working — recovery may be possible
    Failed(String),
}

impl SubsystemHealth {
    pub fn is_healthy(&self) -> bool {
        matches!(self, Self::Healthy)
    }

    pub fn is_failed(&self) -> bool {
        matches!(self, Self::Failed(_))
    }
}

impl fmt::Display for SubsystemHealth {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Healthy => write!(f, "healthy"),
            Self::Degraded(reason) => write!(f, "degraded: {reason}"),
            Self::Failed(reason) => write!(f, "failed: {reason}"),
        }
    }
}

/// Aggregated session health
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionHealthState {
    /// PipeWire stream liveness
    pub video: SubsystemHealth,
    /// Input injection liveness (Portal D-Bus / EIS / Wayland)
    pub input: SubsystemHealth,
    /// Clipboard provider liveness
    pub clipboard: SubsystemHealth,
    /// Portal/Mutter session object validity
    pub session: SubsystemHealth,
    /// Computed from individual subsystems
    pub overall: OverallHealth,
}

impl Default for SessionHealthState {
    fn default() -> Self {
        Self {
            video: SubsystemHealth::Healthy,
            input: SubsystemHealth::Healthy,
            clipboard: SubsystemHealth::Healthy,
            session: SubsystemHealth::Healthy,
            overall: OverallHealth::Healthy,
        }
    }
}

impl SessionHealthState {
    /// Recompute `overall` from individual subsystem states
    fn recompute_overall(&mut self) {
        if self.session.is_failed() {
            // Session destruction is fatal — everything depends on it
            self.overall = OverallHealth::Invalid;
        } else if self.video.is_failed() || self.input.is_failed() {
            // Core subsystem failure
            self.overall = OverallHealth::Invalid;
        } else if !self.video.is_healthy()
            || !self.input.is_healthy()
            || !self.clipboard.is_healthy()
            || !self.session.is_healthy()
        {
            self.overall = OverallHealth::Degraded;
        } else {
            self.overall = OverallHealth::Healthy;
        }
    }
}

/// Overall health computed from subsystem states
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverallHealth {
    /// All subsystems healthy
    Healthy,
    /// Some subsystems degraded but session usable
    Degraded,
    /// Session invalid — recovery or teardown needed
    Invalid,
    /// Recovery in progress
    Recovering,
}

impl fmt::Display for OverallHealth {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Healthy => write!(f, "healthy"),
            Self::Degraded => write!(f, "degraded"),
            Self::Invalid => write!(f, "invalid"),
            Self::Recovering => write!(f, "recovering"),
        }
    }
}

/// Events reported by subsystems to the health monitor
#[derive(Debug, Clone)]
pub enum HealthEvent {
    /// Portal or Mutter session was closed by the compositor
    SessionClosed { reason: String },

    /// Portal session reported invalid via D-Bus error
    SessionInvalidated { reason: String },

    /// PipeWire stream state changed
    VideoStreamStateChanged { state: VideoStreamState },

    /// Input injection failed with a transient or permanent error
    InputFailed { reason: String, permanent: bool },

    /// Input recovered after prior failure
    InputRecovered,

    /// Clipboard provider health check failed
    ClipboardFailed { reason: String },

    /// Clipboard provider recovered
    ClipboardRecovered,

    /// Compositor D-Bus name disappeared (restart or crash)
    CompositorLost { bus_name: String },

    /// EIS event stream ended (device loss)
    EisStreamEnded { reason: String },
}

/// PipeWire stream states relevant to health monitoring
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoStreamState {
    Streaming,
    Paused,
    Error,
}

impl fmt::Display for VideoStreamState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Streaming => write!(f, "streaming"),
            Self::Paused => write!(f, "paused"),
            Self::Error => write!(f, "error"),
        }
    }
}

/// Handle for subscribing to health state changes.
///
/// Subsystems hold a `HealthSubscriber` to check current health or await changes.
#[derive(Clone)]
pub struct HealthSubscriber {
    rx: tokio::sync::watch::Receiver<SessionHealthState>,
    /// Backwards-compatible mirror of session validity
    session_valid: Arc<AtomicBool>,
}

impl HealthSubscriber {
    /// Current health state (non-blocking)
    pub fn current(&self) -> SessionHealthState {
        self.rx.borrow().clone()
    }

    /// Current overall health (non-blocking)
    pub fn overall(&self) -> OverallHealth {
        self.rx.borrow().overall
    }

    /// Whether the session is still valid (backwards-compatible with AtomicBool checks)
    pub fn is_session_valid(&self) -> bool {
        self.session_valid.load(Ordering::Acquire)
    }

    /// Wait for the next health state change
    pub async fn changed(&mut self) -> Result<(), tokio::sync::watch::error::RecvError> {
        self.rx.changed().await
    }

    /// The underlying `session_valid` flag for code paths that still use AtomicBool
    pub fn session_valid_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.session_valid)
    }
}

/// Handle for reporting health events from subsystems.
#[derive(Clone)]
pub struct HealthReporter {
    tx: tokio::sync::mpsc::UnboundedSender<HealthEvent>,
}

impl HealthReporter {
    /// Report a health event to the monitor
    pub fn report(&self, event: HealthEvent) {
        // Best-effort: if the monitor is gone, the event is dropped
        let _ = self.tx.send(event);
    }
}

/// Spawn a task that watches health state changes and forwards them
/// as `ServerEvent::SessionHealthChanged` to the D-Bus signal relay.
///
/// Without this bridge, the D-Bus relay has a `SessionHealthChanged` handler
/// but nothing ever produces the event.
pub fn start_health_dbus_bridge(
    mut subscriber: HealthSubscriber,
    event_tx: tokio::sync::mpsc::UnboundedSender<crate::dbus::events::ServerEvent>,
    mut shutdown: tokio::sync::broadcast::Receiver<()>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut previous_overall = subscriber.overall();

        loop {
            tokio::select! {
                result = subscriber.changed() => {
                    if result.is_err() {
                        // Watch sender dropped — monitor is gone
                        break;
                    }
                    let state = subscriber.current();
                    let new_overall = state.overall;

                    if new_overall != previous_overall {
                        // Build a detail string from the subsystem that changed most
                        let detail = health_change_detail(&state);

                        let _ = event_tx.send(crate::dbus::events::ServerEvent::SessionHealthChanged {
                            old_health: previous_overall.to_string(),
                            new_health: new_overall.to_string(),
                            detail,
                        });

                        previous_overall = new_overall;
                    }
                }
                _ = shutdown.recv() => break,
            }
        }
    })
}

/// Build a human-readable detail string from the health state,
/// focusing on unhealthy subsystems.
fn health_change_detail(state: &SessionHealthState) -> String {
    let mut parts = Vec::new();

    if !state.session.is_healthy() {
        parts.push(format!("session {}", state.session));
    }
    if !state.video.is_healthy() {
        parts.push(format!("video {}", state.video));
    }
    if !state.input.is_healthy() {
        parts.push(format!("input {}", state.input));
    }
    if !state.clipboard.is_healthy() {
        parts.push(format!("clipboard {}", state.clipboard));
    }

    if parts.is_empty() {
        "all subsystems healthy".into()
    } else {
        parts.join("; ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_health_state() {
        let state = SessionHealthState::default();
        assert_eq!(state.overall, OverallHealth::Healthy);
        assert!(state.video.is_healthy());
        assert!(state.input.is_healthy());
        assert!(state.clipboard.is_healthy());
        assert!(state.session.is_healthy());
    }

    #[test]
    fn test_recompute_session_failed_makes_invalid() {
        let mut state = SessionHealthState::default();
        state.session = SubsystemHealth::Failed("closed".into());
        state.recompute_overall();
        assert_eq!(state.overall, OverallHealth::Invalid);
    }

    #[test]
    fn test_recompute_degraded() {
        let mut state = SessionHealthState::default();
        state.clipboard = SubsystemHealth::Degraded("slow".into());
        state.recompute_overall();
        assert_eq!(state.overall, OverallHealth::Degraded);
    }

    #[test]
    fn test_recompute_video_failed_makes_invalid() {
        let mut state = SessionHealthState::default();
        state.video = SubsystemHealth::Failed("stream error".into());
        state.recompute_overall();
        assert_eq!(state.overall, OverallHealth::Invalid);
    }

    #[test]
    fn test_subsystem_health_display() {
        assert_eq!(SubsystemHealth::Healthy.to_string(), "healthy");
        assert_eq!(
            SubsystemHealth::Failed("gone".into()).to_string(),
            "failed: gone"
        );
    }

    #[test]
    fn test_health_change_detail_all_healthy() {
        let state = SessionHealthState::default();
        assert_eq!(health_change_detail(&state), "all subsystems healthy");
    }

    #[test]
    fn test_health_change_detail_mixed() {
        let mut state = SessionHealthState::default();
        state.video = SubsystemHealth::Degraded("PipeWire stream paused".into());
        state.clipboard = SubsystemHealth::Failed("provider gone".into());
        let detail = health_change_detail(&state);
        assert!(detail.contains("video degraded"));
        assert!(detail.contains("clipboard failed"));
        // session and input are healthy, so they shouldn't appear
        assert!(!detail.contains("session "));
        assert!(!detail.contains("input "));
    }

    #[tokio::test]
    async fn test_health_dbus_bridge_emits_on_change() {
        let (shutdown_tx, _) = tokio::sync::broadcast::channel(1);
        let (monitor, reporter, subscriber) = SessionHealthMonitor::new(shutdown_tx.subscribe());

        let monitor_handle = tokio::spawn(monitor.run());

        let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel();

        let _bridge = start_health_dbus_bridge(subscriber, event_tx, shutdown_tx.subscribe());

        // Let the bridge and monitor tasks start
        tokio::task::yield_now().await;

        // Trigger a health change
        reporter.report(HealthEvent::SessionClosed {
            reason: "test".into(),
        });

        // The bridge should emit a SessionHealthChanged event.
        // Use a generous timeout — CI runners can be slow.
        let event = tokio::time::timeout(std::time::Duration::from_secs(2), event_rx.recv())
            .await
            .expect("timed out waiting for event")
            .expect("channel closed");

        match event {
            crate::dbus::events::ServerEvent::SessionHealthChanged {
                old_health,
                new_health,
                detail,
            } => {
                assert_eq!(old_health, "healthy");
                assert_eq!(new_health, "invalid");
                assert!(detail.contains("session"));
            }
            other => panic!("unexpected event: {other:?}"),
        }

        let _ = shutdown_tx.send(());
        let _ = monitor_handle.await;
    }
}
