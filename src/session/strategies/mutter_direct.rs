//! Mutter Direct API Strategy Implementation
//!
//! Uses org.gnome.Mutter.ScreenCast and org.gnome.Mutter.RemoteDesktop D-Bus APIs
//! directly, bypassing the XDG Portal permission model entirely.
//!
//! GNOME-specific, zero-dialog operation. Input is injected over EIS (libei)
//! via Mutter's ConnectToEIS. The EIS channel is activated lazily on the first
//! client connection and re-established if the compositor drops the socket —
//! mirroring the lifecycle the Portal `libei` strategy already proves on GNOME.
//! Activating eagerly at server startup let the socket rot before any client
//! arrived, which is why injection failed with `Broken pipe` despite a healthy
//! ScreenCast (video) half of the same session.
//!
//! Clipboard is handled natively via Mutter's RemoteDesktop clipboard methods,
//! eliminating the need for a hybrid Portal session.

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use anyhow::{Context, Result, anyhow};
use async_trait::async_trait;
use futures_util::StreamExt;
use tracing::{debug, error, info, warn};

use crate::{
    health::{HealthEvent, HealthReporter},
    mutter::{MutterSession, MutterSessionHandle},
    session::strategy::{
        PipeWireAccess, SessionHandle, SessionLifecyclePolicy, SessionStrategy, SessionType,
        StreamInfo,
    },
};

/// EIS input channel for a Mutter Direct session.
///
/// Owns the EIS context/connection and the discovered virtual input devices.
/// Activation (ConnectToEIS + handshake + device setup) is deferred until the
/// first client connects and is repeated if the socket dies, so the socket is
/// always fresh when input actually flows.
#[cfg(feature = "libei")]
struct MutterEisInput {
    /// Session-bus connection used to (re)issue ConnectToEIS on the RD session.
    dbus_connection: zbus::Connection,
    /// RemoteDesktop session object path (the EIS endpoint). Interior-mutable
    /// so the channel can be re-pointed at a freshly re-established session
    /// under the `PerConnection` lifecycle (see `rebind`).
    rd_session_path: Arc<tokio::sync::RwLock<zbus::zvariant::OwnedObjectPath>>,
    /// EIS protocol context — `None` until activated; replaced on reconnect.
    context: Arc<tokio::sync::RwLock<Option<reis::ei::Context>>>,
    /// EIS connection handle — held for the channel's lifetime. Dropping it
    /// closes the socket, so it must outlive the setup event burst.
    connection: Arc<tokio::sync::Mutex<Option<reis::ei::Connection>>>,
    /// Discovered virtual devices (keyboard/pointer/touch), cleared on reconnect.
    devices: Arc<super::eis_common::EisDevices>,
    /// Whether the channel is currently activated.
    activated: AtomicBool,
    /// Serializes concurrent (re)activation attempts.
    activating: AtomicBool,
    /// Shared health reporter (set on the handle after construction).
    health_reporter: Arc<std::sync::OnceLock<HealthReporter>>,
}

#[cfg(feature = "libei")]
impl MutterEisInput {
    fn new(
        dbus_connection: zbus::Connection,
        rd_session_path: zbus::zvariant::OwnedObjectPath,
        health_reporter: Arc<std::sync::OnceLock<HealthReporter>>,
    ) -> Self {
        Self {
            dbus_connection,
            rd_session_path: Arc::new(tokio::sync::RwLock::new(rd_session_path)),
            context: Arc::new(tokio::sync::RwLock::new(None)),
            connection: Arc::new(tokio::sync::Mutex::new(None)),
            devices: Arc::new(super::eis_common::EisDevices::new(0)),
            activated: AtomicBool::new(false),
            activating: AtomicBool::new(false),
            health_reporter,
        }
    }

    /// Ensure the EIS channel is live before an injection. Fast path when already
    /// activated; death is detected by the injection itself (see the `notify_*`
    /// wrappers), which triggers `reconnect`.
    async fn ensure_alive(&self) -> Result<()> {
        if self.activated.load(Ordering::Acquire) {
            return Ok(());
        }
        self.activate().await
    }

    /// Tear down the current EIS channel and activate a fresh one.
    async fn reconnect(&self) -> Result<()> {
        self.activated.store(false, Ordering::Release);
        self.devices.clear().await;
        *self.context.write().await = None;
        *self.connection.lock().await = None;
        self.activate().await
    }

    /// Re-point the EIS channel at a newly re-established RemoteDesktop session
    /// and drop the stale channel so the next injection re-activates against the
    /// new session. Used by the `PerConnection` lifecycle when the compositor's
    /// old session was closed on the previous client's disconnect.
    async fn rebind(&self, new_rd_session_path: zbus::zvariant::OwnedObjectPath) {
        info!(
            new_rd_session = %new_rd_session_path.as_str(),
            "[mutter-eis] Rebinding EIS to re-established RemoteDesktop session"
        );
        *self.rd_session_path.write().await = new_rd_session_path;
        self.activated.store(false, Ordering::Release);
        self.devices.clear().await;
        *self.context.write().await = None;
        *self.connection.lock().await = None;
    }

    /// Activate (or revive) the EIS channel: ConnectToEIS, handshake, and drain
    /// the compositor's device-setup burst. Idempotent and concurrency-safe.
    async fn activate(&self) -> Result<()> {
        use std::{collections::HashMap, os::unix::net::UnixStream};

        use reis::{PendingRequestResult, ei};

        use super::eis_stream::{EisEventStream, HANDSHAKE_TIMEOUT, ei_handshake};

        // Serialize activation: if another task is already activating, wait it out.
        if self.activating.swap(true, Ordering::AcqRel) {
            while self.activating.load(Ordering::Acquire) {
                tokio::time::sleep(std::time::Duration::from_millis(25)).await;
            }
            return Ok(());
        }
        struct ActivatingGuard<'a>(&'a AtomicBool);
        impl Drop for ActivatingGuard<'_> {
            fn drop(&mut self) {
                self.0.store(false, Ordering::Release);
            }
        }
        let _guard = ActivatingGuard(&self.activating);

        // Already up and the socket still accepts writes — nothing to do.
        if self.activated.load(Ordering::Acquire) {
            if let Some(ref ctx) = *self.context.read().await
                && ctx.flush().is_ok()
            {
                return Ok(());
            }
            warn!("[mutter-eis] socket dead on reactivation — reconnecting");
            self.devices.clear().await;
            self.activated.store(false, Ordering::Release);
        }

        let rd_session_path = self.rd_session_path.read().await.clone();
        info!(
            rd_session = %rd_session_path.as_str(),
            "[mutter-eis] Activating EIS (ConnectToEIS on RemoteDesktop session)"
        );
        let rd_session =
            crate::mutter::MutterRemoteDesktopSession::new(&self.dbus_connection, rd_session_path)
                .await
                .context("Failed to create RemoteDesktop proxy for EIS")?;
        // One retry with a fresh socket: the handshake either completes within
        // a millisecond or the socket is dead, and a dead socket must not take
        // the accept loop down with it (this runs on the connection task).
        let mut attempt = 0;
        let (context, mut events, handshake) = loop {
            attempt += 1;
            let fd = match rd_session.connect_to_eis(HashMap::new()).await {
                Ok(fd) => fd,
                Err(e) => {
                    // Surface input death to health directly: the caller only warns
                    // per failed injection, which leaves the health surface blind to
                    // an input channel that never came up at all.
                    if let Some(r) = self.health_reporter.get() {
                        r.report(HealthEvent::EisStreamEnded {
                            reason: format!("ConnectToEIS failed: {e}"),
                        });
                    }
                    return Err(e.context("ConnectToEIS failed"));
                }
            };

            let stream = UnixStream::from(fd);
            let context = ei::Context::new(stream).context("Failed to create EIS context")?;
            let mut events = EisEventStream::new(context.clone())
                .context("Failed to create EIS event stream")?;

            match ei_handshake(&mut events, "lamco-rdp-server-mutter", HANDSHAKE_TIMEOUT).await {
                Ok(handshake) => break (context, events, handshake),
                Err(e) if attempt < 2 => {
                    // Dropping `context` and `events` closes the socket.
                    warn!("[mutter-eis] {e:#}; retrying with a fresh ConnectToEIS");
                }
                Err(e) => {
                    if let Some(r) = self.health_reporter.get() {
                        r.report(HealthEvent::EisStreamEnded {
                            reason: format!("EIS handshake failed: {e:#}"),
                        });
                    }
                    return Err(e.context("EIS handshake failed"));
                }
            }
        };
        info!("[mutter-eis] handshake complete");
        let _ = context.flush();

        *self.devices.last_serial.lock().await = handshake.serial;
        self.devices.device_version.store(
            handshake
                .negotiated_interfaces
                .get("ei_device")
                .copied()
                .unwrap_or(0),
            std::sync::atomic::Ordering::Relaxed,
        );
        *self.context.write().await = Some(context);
        *self.connection.lock().await = Some(handshake.connection);

        // Drain the device-setup burst. The compositor sends seat + device
        // events back-to-back, then goes quiet; we stop reading once it does
        // (matching the libei strategy). The connection/context stay in the
        // struct, so the write side remains open for injection afterwards.
        let seat_caps: tokio::sync::Mutex<std::collections::HashMap<String, u64>> =
            tokio::sync::Mutex::new(std::collections::HashMap::new());
        loop {
            match tokio::time::timeout(std::time::Duration::from_secs(3), events.next()).await {
                Ok(Some(Ok(PendingRequestResult::Request(event)))) => {
                    // The post-bind connection.sync()'s callback.done arrives only
                    // after the full seat/device burst, so use it as a
                    // deterministic drain barrier rather than a 500ms quiet guess
                    // (which could cut off a slow burst).
                    let sync_barrier_reached = matches!(
                        &event,
                        ei::Event::Callback(_, ei::callback::Event::Done { .. })
                    );
                    let ctx_read = self.context.read().await;
                    if let Some(ref ctx) = *ctx_read {
                        handle_eis_event(event, ctx, &self.connection, &self.devices, &seat_caps)
                            .await?;
                    }
                    if sync_barrier_reached {
                        debug!("[mutter-eis] setup sync barrier reached — device burst drained");
                        break;
                    }
                }
                Ok(Some(Ok(PendingRequestResult::ParseError(msg)))) => {
                    warn!("[mutter-eis] parse error during setup: {msg}");
                }
                Ok(Some(Ok(PendingRequestResult::InvalidObject(id)))) => {
                    debug!("[mutter-eis] invalid object during setup: {id}");
                }
                Ok(Some(Err(e))) => {
                    error!("[mutter-eis] event stream error during setup: {e}");
                    return Err(e.into());
                }
                Ok(None) => {
                    debug!("[mutter-eis] stream EOF during setup");
                    break;
                }
                Err(_) => {
                    warn!(
                        "[mutter-eis] setup barrier failsafe timeout (3s) — sync never completed"
                    );
                    break;
                }
            }
        }

        self.activated.store(true, Ordering::Release);
        info!("[mutter-eis] EIS ready for input injection");
        if let Some(r) = self.health_reporter.get() {
            r.report(HealthEvent::EisStreamRecovered);
        }
        Ok(())
    }
}

/// Generate a `notify_*` body for `MutterEisInput`: ensure the channel is
/// alive, inject, and on failure recover and retry. `$call` is re-evaluated
/// so each attempt builds a fresh injection future against the current
/// context.
///
/// A [`super::eis_common::DeviceNotReady`] failure (this one device type
/// hasn't appeared in the compositor's device-setup burst yet) is handled by
/// waiting briefly and retrying against the *same* session, not by
/// reconnecting: a full `reconnect()` clears every device, not just the one
/// that wasn't ready, so if input keeps arriving faster than a fresh
/// device-setup burst can complete, each reconnect can itself provoke
/// another `DeviceNotReady` for whatever event arrives next -- a
/// self-amplifying storm that leaves input dead for as long as events keep
/// coming in. Any other error (a genuinely dead socket) still gets the full
/// `reconnect()`.
#[cfg(feature = "libei")]
macro_rules! eis_inject {
    ($self:expr, $what:literal, $call:expr) => {{
        $self.ensure_alive().await?;
        match $call.await {
            Ok(()) => Ok(()),
            Err(e) if e.downcast_ref::<super::eis_common::DeviceNotReady>().is_some() => {
                debug!(
                    "[mutter-eis] {} injection failed ({e}) — device not yet in the setup burst, waiting",
                    $what
                );
                for _ in 0..40 {
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                    match $call.await {
                        Ok(()) => return Ok(()),
                        Err(e2) if e2.downcast_ref::<super::eis_common::DeviceNotReady>().is_some() => {}
                        Err(e2) => {
                            warn!(
                                "[mutter-eis] {} injection failed ({e2}) — reconnecting",
                                $what
                            );
                            $self.reconnect().await?;
                            return $call.await;
                        }
                    }
                }
                warn!(
                    "[mutter-eis] {} device never became ready after 2s — reconnecting",
                    $what
                );
                $self.reconnect().await?;
                $call.await
            }
            Err(e) => {
                warn!(
                    "[mutter-eis] {} injection failed ({e}) — reconnecting",
                    $what
                );
                $self.reconnect().await?;
                $call.await
            }
        }
    }};
}

#[cfg(feature = "libei")]
impl MutterEisInput {
    async fn keyboard_keycode(&self, keycode: i32, pressed: bool) -> Result<()> {
        eis_inject!(
            self,
            "keyboard",
            super::eis_common::eis_keyboard_keycode(
                self.context.as_ref(),
                self.devices.as_ref(),
                keycode,
                pressed
            )
        )
    }

    async fn keysym(&self, keysym: u32, pressed: bool) -> Result<()> {
        eis_inject!(
            self,
            "text",
            super::eis_common::eis_text_keysym(
                self.context.as_ref(),
                self.devices.as_ref(),
                keysym,
                pressed
            )
        )
    }

    async fn pointer_motion_absolute(
        &self,
        x: f64,
        y: f64,
        offset: Option<(f64, f64)>,
    ) -> Result<()> {
        eis_inject!(
            self,
            "pointer-abs",
            super::eis_common::eis_pointer_motion_absolute(
                self.context.as_ref(),
                self.devices.as_ref(),
                x,
                y,
                offset
            )
        )
    }

    async fn pointer_motion_relative(&self, dx: f64, dy: f64) -> Result<()> {
        eis_inject!(
            self,
            "pointer-rel",
            super::eis_common::eis_pointer_motion_relative(
                self.context.as_ref(),
                self.devices.as_ref(),
                dx,
                dy
            )
        )
    }

    async fn pointer_button(&self, button: i32, pressed: bool) -> Result<()> {
        eis_inject!(
            self,
            "button",
            super::eis_common::eis_pointer_button(
                self.context.as_ref(),
                self.devices.as_ref(),
                button,
                pressed
            )
        )
    }

    async fn pointer_axis(&self, dx: f64, dy: f64) -> Result<()> {
        eis_inject!(
            self,
            "axis",
            super::eis_common::eis_pointer_axis(
                self.context.as_ref(),
                self.devices.as_ref(),
                dx,
                dy
            )
        )
    }

    async fn pointer_axis_discrete(&self, dx: i32, dy: i32) -> Result<()> {
        eis_inject!(
            self,
            "axis-discrete",
            super::eis_common::eis_pointer_axis_discrete(
                self.context.as_ref(),
                self.devices.as_ref(),
                dx,
                dy
            )
        )
    }

    // === Batched pointer-device injection ===
    // See SessionHandle::stage_pointer_* / commit_input_batch for why this
    // exists (batching a coalesced group of pointer-device events into one
    // EIS frame instead of one frame per event).

    async fn stage_pointer_motion_relative(&self, dx: f64, dy: f64) -> Result<()> {
        eis_inject!(
            self,
            "pointer-rel-stage",
            super::eis_common::stage_pointer_motion_relative(self.devices.as_ref(), dx, dy)
        )
    }

    async fn stage_pointer_button(&self, button: i32, pressed: bool) -> Result<()> {
        eis_inject!(
            self,
            "button-stage",
            super::eis_common::stage_pointer_button(self.devices.as_ref(), button, pressed)
        )
    }

    async fn stage_pointer_axis(&self, dx: f64, dy: f64) -> Result<()> {
        eis_inject!(
            self,
            "axis-stage",
            super::eis_common::stage_pointer_axis(self.devices.as_ref(), dx, dy)
        )
    }

    async fn stage_pointer_axis_discrete(&self, dx: i32, dy: i32) -> Result<()> {
        eis_inject!(
            self,
            "axis-discrete-stage",
            super::eis_common::stage_pointer_axis_discrete(self.devices.as_ref(), dx, dy)
        )
    }

    async fn commit_pointer_frame(&self) -> Result<()> {
        eis_inject!(
            self,
            "commit-frame",
            super::eis_common::commit_pointer_frame(self.context.as_ref(), self.devices.as_ref())
        )
    }

    async fn touch_down(&self, slot: u32, x: f64, y: f64) -> Result<()> {
        eis_inject!(
            self,
            "touch-down",
            super::eis_common::eis_touch_down(
                self.context.as_ref(),
                self.devices.as_ref(),
                slot,
                x,
                y
            )
        )
    }

    async fn touch_motion(&self, slot: u32, x: f64, y: f64) -> Result<()> {
        eis_inject!(
            self,
            "touch-motion",
            super::eis_common::eis_touch_motion(
                self.context.as_ref(),
                self.devices.as_ref(),
                slot,
                x,
                y
            )
        )
    }

    async fn touch_up(&self, slot: u32) -> Result<()> {
        eis_inject!(
            self,
            "touch-up",
            super::eis_common::eis_touch_up(self.context.as_ref(), self.devices.as_ref(), slot)
        )
    }
}

/// Mutter session handle wrapper
pub struct MutterSessionHandleImpl {
    /// Underlying Mutter session. Interior-mutable because the `PerConnection`
    /// lifecycle swaps in a freshly re-established session (new D-Bus paths and
    /// PipeWire node) when a client connects after the previous one was closed.
    mutter_handle: std::sync::RwLock<MutterSessionHandle>,
    /// Stable session-bus connection, held separately so RemoteDesktop/ScreenCast
    /// proxies can borrow it without holding the session lock across a D-Bus
    /// await. The same bus reaches re-established session objects.
    connection: zbus::Connection,
    /// EIS input channel (lazy-activated, self-healing, re-pointable)
    #[cfg(feature = "libei")]
    eis: Arc<MutterEisInput>,
    /// Session validity flag — false when the Mutter session is closed
    /// (compositor-reaped or deliberately released); reset true on re-establish.
    session_valid: Arc<AtomicBool>,
    /// Health reporter for session lifecycle events (set once after construction)
    health_reporter: Arc<std::sync::OnceLock<HealthReporter>>,
    /// Output connector used to re-create the Mutter session per connection.
    monitor_connector: Option<String>,
    /// Carried so a re-established session records the same way the first did.
    record_mode: crate::mutter::session_manager::RecordMode,
    /// Whether a headless virtual monitor is marked as part of the platform.
    virtual_is_platform: bool,
    /// Set while we deliberately close the session in `release_after_client`, so
    /// the Closed-signal listener logs an expected teardown rather than an
    /// unexpected compositor kill.
    deliberate_close: Arc<AtomicBool>,
    /// Handles of the current Closed-signal listener tasks. Aborted and replaced
    /// on each re-establishment so a late Closed from a stopped session can't
    /// invalidate its successor.
    listener_tasks: std::sync::Mutex<Vec<tokio::task::JoinHandle<()>>>,
    /// Serialises `establish_for_client` against `release_after_client`.
    ///
    /// A client disconnecting and the next one connecting are separate events on
    /// separate tasks, and a scripted client pair can put them 6ms apart. Without
    /// this, the release of the old session ran concurrently with the setup of the
    /// new one: the teardown tore down the PipeWire stream the new session had
    /// just built, and the connecting client saw a truncated negotiation.
    lifecycle: tokio::sync::Mutex<()>,
}

impl MutterSessionHandleImpl {
    /// Start listening for Mutter ScreenCast and RemoteDesktop Closed D-Bus signals.
    ///
    /// When the compositor destroys either session, this sets `session_valid` to false
    /// and reports to the health system.
    pub async fn start_closed_listeners(&self) {
        // Abort listeners armed against a previous session generation, so a late
        // Closed signal from a session we've already stopped can't invalidate
        // the one we just re-established.
        for handle in self
            .listener_tasks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .drain(..)
        {
            handle.abort();
        }

        let connection = &self.connection;
        let (sc_path, rd_path) = {
            let guard = self
                .mutter_handle
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            (
                guard.screencast_session.clone(),
                guard.remote_desktop_session.clone(),
            )
        };

        let mut handles = Vec::new();

        // ScreenCast session Closed listener
        match crate::mutter::MutterScreenCastSession::new(connection, sc_path).await {
            Ok(sc_session) => match sc_session.subscribe_closed().await {
                Ok(stream) => {
                    handles.push(self.spawn_closed_listener(
                        stream,
                        "ScreenCast",
                        "Mutter ScreenCast Closed signal",
                    ));
                    info!("Mutter ScreenCast Closed listener started");
                }
                Err(e) => debug!("Could not subscribe to ScreenCast Closed: {e}"),
            },
            Err(e) => debug!("Could not create ScreenCast session proxy: {e}"),
        }

        // RemoteDesktop session Closed listener
        match crate::mutter::MutterRemoteDesktopSession::new(connection, rd_path).await {
            Ok(rd_session) => match rd_session.subscribe_closed().await {
                Ok(stream) => {
                    handles.push(self.spawn_closed_listener(
                        stream,
                        "RemoteDesktop",
                        "Mutter RemoteDesktop Closed signal",
                    ));
                    info!("Mutter RemoteDesktop Closed listener started");
                }
                Err(e) => debug!("Could not subscribe to RemoteDesktop Closed: {e}"),
            },
            Err(e) => debug!("Could not create RemoteDesktop session proxy: {e}"),
        }

        *self
            .listener_tasks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = handles;
    }

    /// Spawn a task that waits for one `Closed` signal on `stream` and updates
    /// session validity + health. A close that follows a deliberate
    /// `release_after_client` is logged as an expected teardown and not surfaced
    /// as a health failure; an unexpected close (compositor idle-reap, crash) is.
    fn spawn_closed_listener<S>(
        &self,
        stream: S,
        which: &'static str,
        reason: &'static str,
    ) -> tokio::task::JoinHandle<()>
    where
        S: futures_util::Stream + Send + 'static,
    {
        let valid = Arc::clone(&self.session_valid);
        let health_reporter = Arc::clone(&self.health_reporter);
        let deliberate_close = Arc::clone(&self.deliberate_close);
        tokio::spawn(async move {
            futures_util::pin_mut!(stream);
            stream.next().await;
            valid.store(false, Ordering::Release);
            if deliberate_close.load(Ordering::Acquire) {
                info!(
                    "[mutter-lifecycle] {which} session Closed (expected — deliberate release on disconnect)"
                );
                return;
            }
            // Not deliberate, but not necessarily a failure: the compositor
            // idle-reaps a session with no client attached, which is expected
            // teardown. Report the raw signal and let the health monitor, which
            // holds the client-connected state, make the healthy/degraded call
            // rather than asserting failure severity from here.
            warn!("Mutter {which} session Closed signal received without a deliberate release");
            if let Some(r) = health_reporter.get() {
                r.report(HealthEvent::SessionClosed {
                    reason: reason.into(),
                });
            }
        })
    }

    /// Build a RemoteDesktop proxy for the current (possibly re-established)
    /// session. Clones the path out from under the lock and borrows the stable
    /// `connection` field, so no lock guard is held across the D-Bus await.
    #[cfg(not(feature = "libei"))]
    async fn current_rd_session(&self) -> Result<crate::mutter::MutterRemoteDesktopSession<'_>> {
        let path = self
            .mutter_handle
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remote_desktop_session
            .clone();
        crate::mutter::MutterRemoteDesktopSession::new(&self.connection, path).await
    }
}

#[async_trait]
impl SessionHandle for MutterSessionHandleImpl {
    fn set_health_reporter(&self, reporter: HealthReporter) {
        let _ = self.health_reporter.set(reporter);
    }

    fn pipewire_access(&self) -> PipeWireAccess {
        PipeWireAccess::NodeId(
            self.mutter_handle
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .pipewire_node_id(),
        )
    }

    fn streams(&self) -> Vec<StreamInfo> {
        self.mutter_handle
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .streams()
            .iter()
            .map(|s| StreamInfo {
                node_id: s.node_id,
                width: s.width,
                height: s.height,
                position_x: s.position_x,
                position_y: s.position_y,
            })
            .collect()
    }

    fn session_type(&self) -> SessionType {
        SessionType::MutterDirect
    }

    fn is_session_valid(&self) -> bool {
        self.session_valid.load(Ordering::Acquire)
    }

    async fn activate_input(&self) -> Result<()> {
        // Establish the EIS channel now that a client is connected. Doing this
        // here (rather than at session creation) keeps the socket fresh — the
        // compositor closes an idle EIS channel, so an eagerly-opened one would
        // already be dead by the time the first input event arrives.
        #[cfg(feature = "libei")]
        {
            self.eis.activate().await
        }
        #[cfg(not(feature = "libei"))]
        {
            Ok(())
        }
    }

    async fn notify_keyboard_keycode(&self, keycode: i32, pressed: bool) -> Result<()> {
        if !self.session_valid.load(Ordering::Acquire) {
            return Err(anyhow!(
                "Mutter session invalid — cannot send keyboard event"
            ));
        }
        #[cfg(feature = "libei")]
        {
            self.eis.keyboard_keycode(keycode, pressed).await
        }
        #[cfg(not(feature = "libei"))]
        {
            let rd_session = self.current_rd_session().await?;
            rd_session
                .notify_keyboard_keycode(keycode as u32, pressed)
                .await
                .context("Failed to inject keyboard keycode via Mutter D-Bus")
        }
    }

    async fn notify_keyboard_keysym(&self, keysym: u32, pressed: bool) -> Result<()> {
        if !self.session_valid.load(Ordering::Acquire) {
            return Err(anyhow!(
                "Mutter session invalid — cannot send keyboard keysym"
            ));
        }
        #[cfg(feature = "libei")]
        {
            self.eis.keysym(keysym, pressed).await
        }
        #[cfg(not(feature = "libei"))]
        {
            let rd_session = self.current_rd_session().await?;
            rd_session
                .notify_keyboard_keysym(keysym, pressed)
                .await
                .context("Failed to inject keyboard keysym via Mutter D-Bus")
        }
    }

    async fn notify_pointer_motion_absolute(&self, stream_id: u32, x: f64, y: f64) -> Result<()> {
        if !self.session_valid.load(Ordering::Acquire) {
            return Err(anyhow!(
                "Mutter session invalid — cannot send pointer motion"
            ));
        }
        #[cfg(feature = "libei")]
        {
            let stream_offset = self
                .streams()
                .into_iter()
                .find(|s| s.node_id == stream_id)
                .map(|s| (s.position_x as f64, s.position_y as f64));
            self.eis.pointer_motion_absolute(x, y, stream_offset).await
        }
        #[cfg(not(feature = "libei"))]
        {
            let _ = stream_id;
            let stream_path = self
                .mutter_handle
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .streams
                .first()
                .cloned()
                .ok_or_else(|| anyhow!("No streams available"))?;
            let rd_session = self.current_rd_session().await?;
            rd_session
                .notify_pointer_motion_absolute(&stream_path, x, y)
                .await
                .context("Failed to inject pointer motion via Mutter D-Bus")
        }
    }

    async fn notify_pointer_button(&self, button: i32, pressed: bool) -> Result<()> {
        if !self.session_valid.load(Ordering::Acquire) {
            return Err(anyhow!(
                "Mutter session invalid — cannot send pointer button"
            ));
        }
        #[cfg(feature = "libei")]
        {
            self.eis.pointer_button(button, pressed).await
        }
        #[cfg(not(feature = "libei"))]
        {
            let rd_session = self.current_rd_session().await?;
            rd_session
                .notify_pointer_button(button, pressed)
                .await
                .context("Failed to inject pointer button via Mutter D-Bus")
        }
    }

    async fn notify_pointer_axis(&self, dx: f64, dy: f64) -> Result<()> {
        if !self.session_valid.load(Ordering::Acquire) {
            return Err(anyhow!("Mutter session invalid — cannot send pointer axis"));
        }
        #[cfg(feature = "libei")]
        {
            self.eis.pointer_axis(dx, dy).await
        }
        #[cfg(not(feature = "libei"))]
        {
            let rd_session = self.current_rd_session().await?;
            rd_session
                .notify_pointer_axis(dx, dy)
                .await
                .context("Failed to inject pointer axis via Mutter D-Bus")
        }
    }

    async fn notify_pointer_axis_discrete(&self, dx: i32, dy: i32) -> Result<()> {
        if !self.session_valid.load(Ordering::Acquire) {
            return Err(anyhow!(
                "Mutter session invalid — cannot send discrete scroll"
            ));
        }
        #[cfg(feature = "libei")]
        {
            self.eis.pointer_axis_discrete(dx, dy).await
        }
        #[cfg(not(feature = "libei"))]
        {
            // Mutter's D-Bus RemoteDesktop has no discrete-scroll method; fall
            // back to the continuous conversion.
            let dx_c = (dx as f64 / 120.0) * 15.0;
            let dy_c = (dy as f64 / 120.0) * 15.0;
            let rd_session = self.current_rd_session().await?;
            rd_session
                .notify_pointer_axis(dx_c, dy_c)
                .await
                .context("Failed to inject discrete scroll via Mutter D-Bus")
        }
    }

    async fn notify_pointer_motion_relative(&self, dx: f64, dy: f64) -> Result<()> {
        if !self.session_valid.load(Ordering::Acquire) {
            return Err(anyhow!(
                "Mutter session invalid — cannot send relative motion"
            ));
        }
        #[cfg(feature = "libei")]
        {
            self.eis.pointer_motion_relative(dx, dy).await
        }
        #[cfg(not(feature = "libei"))]
        {
            let rd_session = self.current_rd_session().await?;
            rd_session
                .notify_pointer_motion_relative(dx, dy)
                .await
                .context("Failed to inject relative pointer motion via Mutter D-Bus")
        }
    }

    // === Batched pointer-device injection ===
    // Only meaningful with EIS (libei) active -- gated at the item level so
    // the non-libei D-Bus build simply falls back to SessionHandle's default
    // (stage immediately via the notify_pointer_* methods above, which
    // themselves already branch to the D-Bus path when libei is off).
    #[cfg(feature = "libei")]
    async fn stage_pointer_motion_relative(&self, dx: f64, dy: f64) -> Result<()> {
        if !self.session_valid.load(Ordering::Acquire) {
            return Err(anyhow!(
                "Mutter session invalid — cannot stage relative motion"
            ));
        }
        self.eis.stage_pointer_motion_relative(dx, dy).await
    }

    #[cfg(feature = "libei")]
    async fn stage_pointer_button(&self, button: i32, pressed: bool) -> Result<()> {
        if !self.session_valid.load(Ordering::Acquire) {
            return Err(anyhow!("Mutter session invalid — cannot stage button"));
        }
        self.eis.stage_pointer_button(button, pressed).await
    }

    #[cfg(feature = "libei")]
    async fn stage_pointer_axis(&self, dx: f64, dy: f64) -> Result<()> {
        if !self.session_valid.load(Ordering::Acquire) {
            return Err(anyhow!("Mutter session invalid — cannot stage axis"));
        }
        self.eis.stage_pointer_axis(dx, dy).await
    }

    #[cfg(feature = "libei")]
    async fn stage_pointer_axis_discrete(&self, dx_120: i32, dy_120: i32) -> Result<()> {
        if !self.session_valid.load(Ordering::Acquire) {
            return Err(anyhow!(
                "Mutter session invalid — cannot stage discrete scroll"
            ));
        }
        // dx_120/dy_120 are already 120-units-per-detent, passed straight
        // through (mirrors notify_pointer_axis_discrete above).
        self.eis.stage_pointer_axis_discrete(dx_120, dy_120).await
    }

    #[cfg(feature = "libei")]
    async fn commit_input_batch(&self) -> Result<()> {
        if !self.session_valid.load(Ordering::Acquire) {
            // Nothing was actually staged if the session was already
            // invalid (every stage_* call above would have errored first).
            return Ok(());
        }
        self.eis.commit_pointer_frame().await
    }

    async fn notify_touch_down(&self, _stream_id: u32, slot: u32, x: f64, y: f64) -> Result<()> {
        if !self.session_valid.load(Ordering::Acquire) {
            return Err(anyhow!("Mutter session invalid — cannot send touch down"));
        }
        #[cfg(feature = "libei")]
        {
            self.eis.touch_down(slot, x, y).await
        }
        #[cfg(not(feature = "libei"))]
        {
            let stream_path = self
                .mutter_handle
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .streams
                .first()
                .cloned()
                .ok_or_else(|| anyhow!("No streams available"))?;
            let rd_session = self.current_rd_session().await?;
            rd_session
                .notify_touch_down(&stream_path, slot, x, y)
                .await
                .context("Failed to inject touch down via Mutter D-Bus")
        }
    }

    async fn notify_touch_motion(&self, _stream_id: u32, slot: u32, x: f64, y: f64) -> Result<()> {
        if !self.session_valid.load(Ordering::Acquire) {
            return Err(anyhow!("Mutter session invalid — cannot send touch motion"));
        }
        #[cfg(feature = "libei")]
        {
            self.eis.touch_motion(slot, x, y).await
        }
        #[cfg(not(feature = "libei"))]
        {
            let stream_path = self
                .mutter_handle
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .streams
                .first()
                .cloned()
                .ok_or_else(|| anyhow!("No streams available"))?;
            let rd_session = self.current_rd_session().await?;
            rd_session
                .notify_touch_motion(&stream_path, slot, x, y)
                .await
                .context("Failed to inject touch motion via Mutter D-Bus")
        }
    }

    async fn notify_touch_up(&self, slot: u32) -> Result<()> {
        if !self.session_valid.load(Ordering::Acquire) {
            return Err(anyhow!("Mutter session invalid — cannot send touch up"));
        }
        #[cfg(feature = "libei")]
        {
            self.eis.touch_up(slot).await
        }
        #[cfg(not(feature = "libei"))]
        {
            let rd_session = self.current_rd_session().await?;
            rd_session
                .notify_touch_up(slot)
                .await
                .context("Failed to inject touch up via Mutter D-Bus")
        }
    }

    fn clipboard_source(&self) -> crate::session::strategy::ClipboardSource {
        match self
            .mutter_handle
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clipboard
            .as_ref()
        {
            Some(mgr) => {
                crate::session::strategy::ClipboardSource::Mutter(std::sync::Arc::clone(mgr))
            }
            None => crate::session::strategy::ClipboardSource::None,
        }
    }

    fn lifecycle_policy(&self) -> SessionLifecyclePolicy {
        // Mutter reaps an idle RemoteDesktop session shortly after the RDP
        // client leaves, and re-creating one is dialog-free on the direct D-Bus
        // path — so we establish per connection rather than persist one.
        SessionLifecyclePolicy::PerConnection
    }

    async fn establish_for_client(&self) -> Result<(Vec<StreamInfo>, bool)> {
        // Wait for any in-flight release to finish before touching the session.
        let _lifecycle = self.lifecycle.lock().await;

        // Still-valid session (the startup session on the very first client, or
        // one that hasn't been released yet): reuse it, but only after proving
        // the compositor still answers for it. The flag can be stale: no Closed
        // signal arrives when the compositor's bus name vanishes outright (a
        // shell still settling at server startup, or a shell restart), so a
        // session created moments before that death stays flagged valid while
        // every one of its D-Bus objects is gone. A cheap SessionId property
        // read is the arbiter; on failure we fall through to re-establishment
        // instead of handing the client a corpse with permanently dead input.
        if self.session_valid.load(Ordering::Acquire) {
            let rd_path = self
                .mutter_handle
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .remote_desktop_session
                .clone();
            let alive =
                match crate::mutter::MutterRemoteDesktopSession::new(&self.connection, rd_path)
                    .await
                {
                    Ok(rd) => rd.session_id().await.is_ok(),
                    Err(_) => false,
                };
            // A live session is not automatically a correct one. An area
            // stream fixes its rectangle at creation and Mutter never updates
            // it, so if the host's resolution, scale or layout moved while we
            // were idle, reusing the session would capture the wrong region
            // and report nothing wrong. Re-establishing rebuilds against the
            // current geometry, and the existing rebind path carries the
            // capture pipeline across.
            let area_stale = if alive {
                let source = self
                    .mutter_handle
                    .read()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .capture_source
                    .clone();
                match (source.connector(), source.area()) {
                    (Some(connector), Some(created_for)) => {
                        match crate::mutter::area_moved(&self.connection, connector, created_for)
                            .await
                        {
                            Ok(Some(now)) => {
                                info!(
                                    "[mutter-lifecycle] Capture area for {connector} moved from {}x{} at ({},{}) to {}x{} at ({},{}) — re-establishing",
                                    created_for.width,
                                    created_for.height,
                                    created_for.x,
                                    created_for.y,
                                    now.width,
                                    now.height,
                                    now.x,
                                    now.y
                                );
                                true
                            }
                            Ok(None) => false,
                            // Not evidence of a change, so do not tear down a
                            // working session over it.
                            Err(e) => {
                                debug!(
                                    "[mutter-lifecycle] Could not re-check the capture area: {e:#}"
                                );
                                false
                            }
                        }
                    }
                    _ => false,
                }
            } else {
                false
            };

            if alive && !area_stale {
                let streams = self.streams();
                let node = self
                    .mutter_handle
                    .read()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .pipewire_node_id();
                info!(
                    node,
                    stream_count = streams.len(),
                    "[mutter-lifecycle] Session still valid (liveness verified) — reusing for incoming client"
                );
                return Ok((streams, false));
            }
            if alive {
                self.session_valid.store(false, Ordering::Release);
            }
            warn!(
                "[mutter-lifecycle] Session flagged valid but the compositor no longer answers for it — re-establishing"
            );
            self.session_valid.store(false, Ordering::Release);
        }

        // The previous session was closed on the last disconnect (deliberate
        // release, or a compositor idle-reap that beat us to it). Create a fresh
        // RemoteDesktop + ScreenCast session and rebind input + capture to it.
        info!("[mutter-lifecycle] Re-establishing Mutter session for incoming client");
        let manager = MutterSession::new()
            .await
            .context("Failed to create Mutter session manager for re-establishment")?;
        let mut fresh = manager
            .create_session(
                self.monitor_connector.as_deref(),
                self.record_mode,
                self.virtual_is_platform,
            )
            .await
            .context("Failed to re-establish Mutter session")?;

        let new_rd_path = fresh.remote_desktop_session.clone();
        let node = fresh.pipewire_node_id();
        let streams: Vec<StreamInfo> = fresh
            .streams()
            .iter()
            .map(|s| StreamInfo {
                node_id: s.node_id,
                width: s.width,
                height: s.height,
                position_x: s.position_x,
                position_y: s.position_y,
            })
            .collect();

        // Preserve the clipboard Arc the server already holds: re-point it at the
        // new session (dialog-free) and keep that same Arc in the re-established
        // handle, so the wired clipboard provider stays valid across reconnects
        // instead of talking to the destroyed session.
        let existing_clipboard = self
            .mutter_handle
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clipboard
            .clone();
        if let Some(clipboard) = &existing_clipboard {
            if let Err(e) = clipboard.rebind(new_rd_path.clone()).await {
                error!("[mutter-lifecycle] Clipboard rebind failed: {e:#}");
            }
            fresh.clipboard = Some(Arc::clone(clipboard));
        }

        // Swap in the live session, mark valid, then re-point EIS and re-arm the
        // Closed listeners against the new session's D-Bus paths.
        *self
            .mutter_handle
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = fresh;
        self.session_valid.store(true, Ordering::Release);
        self.deliberate_close.store(false, Ordering::Release);

        #[cfg(feature = "libei")]
        self.eis.rebind(new_rd_path).await;
        #[cfg(not(feature = "libei"))]
        let _ = new_rd_path;

        self.start_closed_listeners().await;

        info!(
            node,
            stream_count = streams.len(),
            "[mutter-lifecycle] Mutter session re-established; caller must rebind capture pipeline (node id may be reused)"
        );
        Ok((streams, true))
    }

    async fn release_after_client(&self) {
        let _lifecycle = self.lifecycle.lock().await;

        // Close the session deterministically on disconnect. This closes the
        // idle-reap race window (the compositor can't quietly kill a session we
        // already stopped) and guarantees the next client gets a fresh one.
        info!("[mutter-lifecycle] Releasing Mutter session on client disconnect");
        self.deliberate_close.store(true, Ordering::Release);
        self.session_valid.store(false, Ordering::Release);

        let (sc_path, rd_path) = {
            let guard = self
                .mutter_handle
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            (
                guard.screencast_session.clone(),
                guard.remote_desktop_session.clone(),
            )
        };

        // Stop is best-effort: the RDP disconnect often tears the session down
        // before we get here (Mutter then rejects Stop with "Failed to call
        // Stop"), which is expected — the goal (session gone) is met, and the
        // next connect re-establishes regardless.
        match crate::mutter::MutterScreenCastSession::new(&self.connection, sc_path).await {
            Ok(sc) => {
                if let Err(e) = sc.stop().await {
                    debug!("[mutter-lifecycle] ScreenCast stop best-effort (already closing): {e}");
                }
            }
            Err(e) => debug!("[mutter-lifecycle] ScreenCast proxy for release failed: {e}"),
        }
        match crate::mutter::MutterRemoteDesktopSession::new(&self.connection, rd_path).await {
            Ok(rd) => {
                if let Err(e) = rd.stop().await {
                    debug!(
                        "[mutter-lifecycle] RemoteDesktop stop best-effort (already closing): {e}"
                    );
                }
            }
            Err(e) => debug!("[mutter-lifecycle] RemoteDesktop proxy for release failed: {e}"),
        }
        info!("[mutter-lifecycle] Mutter session released");
    }
}

/// Mutter Direct API strategy
///
/// Bypasses portal entirely by using GNOME Mutter's native D-Bus interfaces.
/// Requires GNOME compositor and non-sandboxed application.
pub struct MutterDirectStrategy {
    /// Monitor connector (e.g., "HDMI-1"), or None for virtual monitor
    monitor_connector: Option<String>,
    /// Whether to record an area or the monitor itself (GNOME/mutter#3903).
    record_mode: crate::mutter::session_manager::RecordMode,
    /// Whether a headless virtual monitor is marked as part of the platform.
    virtual_is_platform: bool,
}

impl MutterDirectStrategy {
    pub fn new(monitor_connector: Option<String>) -> Self {
        Self {
            monitor_connector,
            record_mode: crate::mutter::session_manager::RecordMode::default(),
            virtual_is_platform: false,
        }
    }

    /// Set how a monitor is recorded (see `RecordMode`).
    #[must_use]
    pub fn with_record_mode(
        mut self,
        record_mode: crate::mutter::session_manager::RecordMode,
    ) -> Self {
        self.record_mode = record_mode;
        self
    }

    /// Mark a headless virtual monitor as part of the platform.
    ///
    /// Only reaches the compositor on the headless path, since `is-platform` is
    /// a `RecordVirtual` property; monitor and area sessions never send it.
    #[must_use]
    pub fn with_virtual_is_platform(mut self, is_platform: bool) -> Self {
        self.virtual_is_platform = is_platform;
        self
    }

    pub async fn is_available() -> bool {
        crate::mutter::is_mutter_api_available().await
    }
}

#[async_trait]
impl SessionStrategy for MutterDirectStrategy {
    fn name(&self) -> &'static str {
        "Mutter Direct D-Bus API"
    }

    fn requires_initial_setup(&self) -> bool {
        false
    }

    fn supports_unattended_restore(&self) -> bool {
        true
    }

    async fn create_session(&self) -> Result<Arc<dyn SessionHandle>> {
        info!("Creating session using Mutter Direct API");

        let compositor = crate::compositor::identify_compositor();
        if !matches!(compositor, crate::compositor::CompositorType::Gnome { .. }) {
            return Err(anyhow!("Mutter Direct API only available on GNOME"));
        }

        if std::path::Path::new("/.flatpak-info").exists() {
            return Err(anyhow!(
                "Mutter Direct API not available in Flatpak (sandbox blocks D-Bus access)"
            ));
        }

        let manager = MutterSession::new()
            .await
            .context("Failed to create Mutter session manager")?;

        let mutter_handle = manager
            .create_session(
                self.monitor_connector.as_deref(),
                self.record_mode,
                self.virtual_is_platform,
            )
            .await
            .context("Failed to create Mutter session")?;

        info!("Mutter session created (zero dialogs)");

        for (idx, stream) in mutter_handle.streams().iter().enumerate() {
            info!(
                "  Stream {}: {}x{} at ({}, {}), PipeWire node: {}",
                idx,
                stream.width,
                stream.height,
                stream.position_x,
                stream.position_y,
                stream.node_id
            );
        }

        let session_valid = Arc::new(AtomicBool::new(true));
        let health_reporter = Arc::new(std::sync::OnceLock::new());

        // EIS input is set up lazily (on first client connect via activate_input)
        // and reconnected on socket death — see MutterEisInput. We only need the
        // RemoteDesktop session path + bus connection to (re)issue ConnectToEIS.
        #[cfg(feature = "libei")]
        let eis = Arc::new(MutterEisInput::new(
            mutter_handle.connection.clone(),
            mutter_handle.remote_desktop_session.clone(),
            Arc::clone(&health_reporter),
        ));

        if mutter_handle.clipboard.is_some() {
            info!("Mutter clipboard available (native, no Portal needed)");
        }

        let connection = mutter_handle.connection.clone();
        let handle = MutterSessionHandleImpl {
            mutter_handle: std::sync::RwLock::new(mutter_handle),
            connection,
            #[cfg(feature = "libei")]
            eis,
            session_valid,
            health_reporter,
            monitor_connector: self.monitor_connector.clone(),
            record_mode: self.record_mode,
            virtual_is_platform: self.virtual_is_platform,
            deliberate_close: Arc::new(AtomicBool::new(false)),
            listener_tasks: std::sync::Mutex::new(Vec::new()),
            lifecycle: tokio::sync::Mutex::new(()),
        };

        // Listen for Mutter Closed signals (proactive session death detection)
        handle.start_closed_listeners().await;

        Ok(Arc::new(handle))
    }

    async fn cleanup(&self, _session: &dyn SessionHandle) -> Result<()> {
        info!("Cleaning up Mutter session");
        debug!("Mutter session cleanup (automatic via D-Bus object lifecycle)");
        Ok(())
    }
}

// === EIS device-setup event handling (behind libei feature) ===

#[cfg(feature = "libei")]
async fn handle_eis_event(
    event: reis::ei::Event,
    context: &reis::ei::Context,
    connection: &tokio::sync::Mutex<Option<reis::ei::Connection>>,
    devices: &super::eis_common::EisDevices,
    seat_caps: &tokio::sync::Mutex<std::collections::HashMap<String, u64>>,
) -> Result<()> {
    use reis::ei;

    use super::eis_common::DeviceData;

    match event {
        ei::Event::Connection(_connection, request) => match request {
            ei::connection::Event::Seat { seat: _ } => {
                debug!("[mutter-eis] Seat added");
            }
            ei::connection::Event::Ping { ping } => {
                ping.done(0);
                let _ = context.flush();
            }
            _ => {}
        },

        ei::Event::Seat(seat, request) => match request {
            ei::seat::Event::Capability { mask, interface } => {
                seat_caps.lock().await.insert(interface.clone(), mask);
                debug!(
                    "[mutter-eis] Seat capability: {} (mask: {})",
                    interface, mask
                );
            }
            ei::seat::Event::Done => {
                let caps = seat_caps.lock().await;
                let bind_mask = caps.values().fold(0u64, |a, b| a | b);
                seat.bind(bind_mask);
                if let Some(ref conn) = *connection.lock().await {
                    conn.sync(1);
                }
                let _ = context.flush();
                info!(
                    "[mutter-eis] Seat bound with capabilities 0x{:x}: {:?}",
                    bind_mask,
                    caps.keys().collect::<Vec<_>>()
                );
            }
            ei::seat::Event::Device { device } => {
                let mut devs = devices.all.lock().await;
                devs.insert(device, DeviceData::default());
            }
            _ => {}
        },

        ei::Event::Device(device, request) => {
            let mut devs = devices.all.lock().await;
            let data = devs.entry(device.clone()).or_default();

            match request {
                ei::device::Event::Name { name } => {
                    data.name = Some(name);
                }
                ei::device::Event::DeviceType { device_type } => {
                    data.device_type = Some(device_type);
                }
                ei::device::Event::Interface { object } => {
                    let iface_name = object.interface().to_owned();
                    data.interfaces.insert(iface_name, object);
                }
                ei::device::Event::Done => {
                    // Assign device roles only. start_emulating is issued on
                    // Resumed (below) — issuing it on Done *and* Resumed is
                    // rejected by some compositors.
                    super::eis_common::assign_device_roles(&device, data, devices).await;
                    // v3 devices require ready() after done before resumed.
                    if super::eis_common::eis_device_ready(devices, &device) {
                        let _ = context.flush();
                    }
                }
                ei::device::Event::Resumed { serial } => {
                    *devices.last_serial.lock().await = serial;
                    device.start_emulating(serial.wrapping_add(1), serial);
                    let _ = context.flush();
                    info!("[mutter-eis] Device resumed with serial: {}", serial);
                }
                _ => {}
            }
        }

        // Response to `connection.sync()`; consumed explicitly for handshake
        // observability. Gating readiness on it is future work (reis/EIS roadmap).
        ei::Event::Callback(_callback, ei::callback::Event::Done { callback_data }) => {
            tracing::debug!("[mutter-eis] sync callback done (data={callback_data})");
        }

        _ => {}
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[ignore = "Requires GNOME with Mutter running"]
    async fn test_mutter_direct_strategy() {
        if !MutterDirectStrategy::is_available().await {
            println!("Mutter API not available, skipping test");
            return;
        }

        let strategy = MutterDirectStrategy::new(None);

        match strategy.create_session().await {
            Ok(handle) => {
                println!("Mutter session created successfully");
                println!("Session type: {:?}", handle.session_type());
                println!("Streams: {}", handle.streams().len());

                strategy.cleanup(handle.as_ref()).await.ok();
            }
            Err(e) => {
                println!("Failed to create Mutter session: {e}");
            }
        }
    }

    #[tokio::test]
    async fn test_mutter_availability_check() {
        let available = MutterDirectStrategy::is_available().await;
        println!("Mutter Direct API available: {available}");
    }
}
