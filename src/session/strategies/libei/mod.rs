//! libei/EIS Strategy: Flatpak-Compatible wlroots Input
//!
//! This module implements input injection using the libei (Emulated Input) protocol via
//! Portal RemoteDesktop.ConnectToEIS(), providing Flatpak-compatible wlroots support.
//!
//! # Overview
//!
//! The libei strategy uses the Portal RemoteDesktop interface to obtain an EIS (Emulated
//! Input Server) socket, then communicates via the EI protocol using the `reis` crate.
//!
//! # Architecture
//!
//! ```text
//! lamco-rdp-server (Flatpak or native)
//!   | D-Bus
//! Portal RemoteDesktop
//!   +- CreateSession()
//!   +- SelectDevices(keyboard, pointer, touchscreen)
//!   +- Start() -> user approves if needed
//!   +- ConnectToEIS() -> Unix socket FD
//!       |
//! EIS Protocol (via reis crate)
//!   +- Handshake (version, capabilities)
//!   +- Seat discovery
//!   +- Device creation (keyboard, pointer, touchscreen)
//!   +- Event streaming (key, motion, button, scroll, touch)
//!       |
//! Portal backend (xdg-desktop-portal-wlr, hyprland, etc.)
//!   +- Compositor protocols (zwp_virtual_keyboard, zwlr_virtual_pointer)
//! ```
//!
//! # Compatibility
//!
//! **Works with:**
//! - xdg-desktop-portal-wlr with PR #359 (InputCapture + RemoteDesktop/ConnectToEIS)
//! - xdg-desktop-portal-hyprland with ConnectToEIS support
//! - Any portal backend implementing RemoteDesktop v2+ with ConnectToEIS
//!
//! **Flatpak compatible:** Yes (Portal provides socket FD across sandbox boundary)

use std::{collections::HashMap, os::unix::net::UnixStream, sync::Arc};

use anyhow::{Context as AnyhowContext, Result};
use ashpd::desktop::{
    PersistMode,
    remote_desktop::{DeviceType, RemoteDesktop},
    screencast::{CursorMode, Screencast, SourceType as ScSourceType},
};
use async_trait::async_trait;
use futures::stream::StreamExt;
use reis::{PendingRequestResult, ei};
use tokio::sync::{Mutex, RwLock};
use tracing::{debug, error, info, warn};

use super::{
    eis_common::{self, DeviceData, EisDevices},
    eis_stream::{EisEventStream, HANDSHAKE_TIMEOUT, ei_handshake},
};
use crate::{
    health::{HealthEvent, HealthReporter},
    session::{
        Tokens,
        strategy::{PipeWireAccess, SessionHandle, SessionStrategy, SessionType, StreamInfo},
    },
};

/// libei/EIS strategy implementation
///
/// Provides input injection via Portal RemoteDesktop + EIS protocol.
pub struct LibeiStrategy {
    #[expect(dead_code, reason = "retained for future EIS session recovery")]
    portal_manager: Option<Arc<lamco_portal::PortalManager>>,
    token_manager: Option<Arc<Tokens>>,
}

impl LibeiStrategy {
    pub fn new(
        portal_manager: Option<Arc<lamco_portal::PortalManager>>,
        token_manager: Option<Arc<Tokens>>,
    ) -> Self {
        Self {
            portal_manager,
            token_manager,
        }
    }

    pub async fn is_available() -> bool {
        match RemoteDesktop::new().await {
            Ok(_rd) => {
                debug!("[libei] Portal RemoteDesktop proxy created successfully");
                true
            }
            Err(e) => {
                debug!("[libei] Portal RemoteDesktop not available: {}", e);
                false
            }
        }
    }
}

impl Default for LibeiStrategy {
    fn default() -> Self {
        Self::new(None, None)
    }
}

#[async_trait]
impl SessionStrategy for LibeiStrategy {
    fn name(&self) -> &'static str {
        "libei"
    }

    fn requires_initial_setup(&self) -> bool {
        true
    }

    fn supports_unattended_restore(&self) -> bool {
        true
    }

    async fn create_session(&self) -> Result<Arc<dyn SessionHandle>> {
        info!("libei: Creating session with Portal RemoteDesktop + EIS");

        let remote_desktop = RemoteDesktop::new()
            .await
            .context("Failed to create RemoteDesktop proxy")?;

        let session = remote_desktop
            .create_session(ashpd::desktop::CreateSessionOptions::default())
            .await
            .context("Failed to create RemoteDesktop session")?;

        // Tag this Portal session for log correlation across multi-session setups
        // (KDE+libei creates 3 sessions: this libei one, ScreenCast for video,
        // and a duplicate combined session for clipboard via server/mod.rs).
        let session_tag = format!("libei-{}", &uuid::Uuid::new_v4().to_string()[..8]);
        info!(
            session_tag = %session_tag,
            "[libei] Portal RemoteDesktop session created"
        );

        // Load restore token from previous session (avoids permission dialog)
        let restore_token = if let Some(ref tm) = self.token_manager {
            match tm.load_token("libei-default").await {
                Ok(Some(token)) => {
                    info!("libei: Loaded restore token ({} chars)", token.len());
                    Some(token)
                }
                Ok(None) => {
                    info!("libei: No restore token found, permission dialog will appear");
                    None
                }
                Err(e) => {
                    warn!("libei: Failed to load restore token: {}", e);
                    None
                }
            }
        } else {
            None
        };

        use ashpd::desktop::remote_desktop::SelectDevicesOptions;
        remote_desktop
            .select_devices(
                &session,
                SelectDevicesOptions::default()
                    .set_devices(
                        DeviceType::Keyboard | DeviceType::Pointer | DeviceType::Touchscreen,
                    )
                    .set_restore_token(restore_token.as_deref())
                    .set_persist_mode(PersistMode::ExplicitlyRevoked),
            )
            .await
            .context("Failed to select input devices")?;

        info!("libei: Selected keyboard, pointer, and touchscreen devices");

        // Attach ScreenCast video to the SAME RemoteDesktop session (issue #51):
        // one portal session, one permission dialog, and the video grant persists
        // under the same restore token as input. Replaces the former standalone
        // ScreenCast session (server/mod.rs) that hardcoded PersistMode::DoNot and
        // re-prompted for screen sharing on every start.
        let screencast = Screencast::new()
            .await
            .context("Failed to create ScreenCast proxy for libei video")?;
        let cursor_mode = {
            let available = screencast
                .available_cursor_modes()
                .await
                .context("Failed to query ScreenCast cursor modes")?;
            if available.contains(CursorMode::Metadata) {
                CursorMode::Metadata
            } else if available.contains(CursorMode::Embedded) {
                CursorMode::Embedded
            } else {
                CursorMode::Hidden
            }
        };
        // Persistence for the combined session is owned once, at the
        // RemoteDesktop::select_devices() call above — this attached
        // ScreenCast inherits it and must not set its own persist_mode.
        // Newer xdg-desktop-portal-kde builds (6.6.4, Fedora 44 beta) reject
        // a redundant persist_mode here with "Remote desktop sessions cannot
        // persist"; 6.6.3 (the version #51 was fixed against) tolerated it.
        use ashpd::desktop::screencast::SelectSourcesOptions;
        screencast
            .select_sources(
                &session,
                SelectSourcesOptions::default()
                    .set_cursor_mode(cursor_mode)
                    .set_sources(enumflags2::BitFlags::from(ScSourceType::Monitor))
                    .set_multiple(false),
            )
            .await
            .context("Failed to select ScreenCast sources on libei session")?;

        // Without a restore token the portal shows a permission dialog here and
        // Start() does not return until someone answers it. The RDP listener is
        // not bound yet, so from outside the process this looks like a hang with
        // no output at all: say what is being waited on before blocking.
        info!(
            "libei: Requesting portal session start{}",
            if restore_token.is_some() {
                " (restore token present, no dialog expected)"
            } else {
                " (no restore token: the portal will prompt on the session's display and this call blocks until it is answered)"
            }
        );

        let response = remote_desktop
            .start(
                &session,
                None,
                ashpd::desktop::remote_desktop::StartOptions::default(),
            )
            .await
            .context("Failed to start RemoteDesktop session")?;

        // Extract and save restore token for future sessions
        let selected = response.response()?;
        let new_token = selected.restore_token().map(ToString::to_string);

        if let Some(ref token) = new_token {
            info!("libei: Received restore token ({} chars)", token.len());
            if let Some(ref tm) = self.token_manager
                && let Err(e) = tm.save_token("libei-default", token).await
            {
                warn!("libei: Failed to save restore token: {}", e);
            }
        } else {
            debug!("[libei] No restore token in response (portal may not support persistence)");
        }

        // Phase 1 complete: Portal session has permissions.
        // EIS activation (ConnectToEIS) is deferred until the first
        // RDP client connects, preventing compositor idle timeout.
        info!("libei: Portal session ready (EIS deferred until client connects)");

        // Video streams from the same session (ScreenCast sources selected above).
        let video_streams: Vec<StreamInfo> = selected
            .streams()
            .iter()
            .map(|s| {
                let (w, h) = s.size().unwrap_or((0, 0));
                let (x, y) = s.position().unwrap_or((0, 0));
                StreamInfo {
                    node_id: s.pipe_wire_node_id(),
                    width: w as u32,
                    height: h as u32,
                    position_x: x,
                    position_y: y,
                }
            })
            .collect();
        if video_streams.is_empty() {
            return Err(anyhow::anyhow!(
                "libei: RemoteDesktop session returned no ScreenCast streams"
            ));
        }
        let video_fd = {
            use std::os::fd::AsRawFd;
            let fd = screencast
                .open_pipe_wire_remote(
                    &session,
                    ashpd::desktop::screencast::OpenPipeWireRemoteOptions::default(),
                )
                .await
                .context("Failed to open PipeWire remote for libei video")?;
            let raw = fd.as_raw_fd();
            // Ownership transfers to the display pipeline downstream (from_raw_fd);
            // forget here so this scope does not close it.
            std::mem::forget(fd);
            raw
        };
        info!(
            fd = video_fd,
            streams = video_streams.len(),
            "libei: acquired video via ScreenCast on the RemoteDesktop session"
        );

        let portal_session = Arc::new(RwLock::new(session));

        // Subscribe to Session.Closed signal. Without this, when the portal
        // backend (kwin/mutter/wlr) decides to kill the session, we keep using
        // a dead handle and the cascade surfaces ~7s later as mstsc TCP RST or
        // PipeWire stream death with no visible cause in our logs.
        //
        // The spawned task holds an OwnedRwLockReadGuard for the session
        // lifetime; this is sound because portal_session is read-only after
        // creation (verified: no .write()/.write_owned() calls on it anywhere).
        {
            let session_for_closed = portal_session.clone();
            let task_tag = session_tag.clone();
            tokio::spawn(async move {
                let session_guard = session_for_closed.read_owned().await;
                let mut closed_stream = match session_guard.receive_closed().await {
                    Ok(s) => s,
                    Err(e) => {
                        warn!(
                            session_tag = %task_tag,
                            error = %e,
                            "[libei] Failed to subscribe to Session.Closed signal"
                        );
                        return;
                    }
                };
                info!(
                    session_tag = %task_tag,
                    "[libei] Subscribed to Portal Session.Closed signal"
                );
                match closed_stream.next().await {
                    Some(payload) => {
                        error!(
                            session_tag = %task_tag,
                            payload = ?payload,
                            "[libei] PORTAL SESSION CLOSED by backend — input/EIS path is now dead"
                        );
                    }
                    None => {
                        warn!(
                            session_tag = %task_tag,
                            "[libei] Portal Session.Closed stream ended without a Closed event (D-Bus connection lost?)"
                        );
                    }
                }
                // session_guard held until task ends — keeps the session alive
                // and the signal subscription valid.
            });
        }

        let handle = Arc::new(LibeiSessionHandleImpl {
            portal_session,
            remote_desktop: Arc::new(remote_desktop),
            context: Arc::new(RwLock::new(None)),
            connection: Arc::new(Mutex::new(None)),
            seats: Arc::new(Mutex::new(HashMap::new())),
            devices: Arc::new(EisDevices::new(0)),
            streams: Arc::new(Mutex::new(video_streams)),
            video_fd,
            health_reporter: std::sync::OnceLock::new(),
            eis_activated: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            activating: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        });

        Ok(handle as Arc<dyn SessionHandle>)
    }

    async fn cleanup(&self, _session: &dyn SessionHandle) -> Result<()> {
        info!("libei: Session cleanup complete");
        Ok(())
    }
}

/// Seat data for EIS seats
#[derive(Default)]
struct SeatData {
    name: Option<String>,
    capabilities: HashMap<String, u64>,
}

/// libei session handle implementation
///
/// Implements SessionHandle trait using event-driven EIS protocol.
/// The context and devices are behind RwLock to allow replacement
/// during EIS session recovery (when the EIS socket dies due to
/// compositor idle timeout).
pub struct LibeiSessionHandleImpl {
    /// Portal session (alive for server lifetime, holds permissions)
    portal_session: Arc<RwLock<ashpd::desktop::Session<RemoteDesktop>>>,
    /// RemoteDesktop proxy for calling ConnectToEIS
    remote_desktop: Arc<RemoteDesktop>,
    /// EIS context — None until activate_input() is called
    context: Arc<RwLock<Option<ei::Context>>>,
    /// EIS connection — None until activated
    connection: Arc<Mutex<Option<ei::Connection>>>,
    /// Seat tracking (populated by event loop after activation)
    seats: Arc<Mutex<HashMap<ei::Seat, SeatData>>>,
    /// Input devices (populated by event loop after activation)
    devices: Arc<EisDevices>,
    /// Video streams from ScreenCast on this same RemoteDesktop session (#51)
    streams: Arc<Mutex<Vec<StreamInfo>>>,
    /// PipeWire FD for the video streams, opened on this session (#51)
    video_fd: std::os::fd::RawFd,
    health_reporter: std::sync::OnceLock<HealthReporter>,
    /// Whether EIS has been activated (ConnectToEIS called)
    eis_activated: Arc<std::sync::atomic::AtomicBool>,
    /// Prevent concurrent activation
    activating: Arc<std::sync::atomic::AtomicBool>,
}

impl LibeiSessionHandleImpl {
    /// Activate EIS: call ConnectToEIS, handshake, and set up devices.
    ///
    /// Called on-demand when the first RDP client connects (via activate_input).
    /// This prevents the EIS socket from dying due to compositor idle timeout.
    async fn activate_eis(&self) -> Result<()> {
        // Prevent concurrent activation
        if self
            .activating
            .swap(true, std::sync::atomic::Ordering::AcqRel)
        {
            // Another task is already activating — wait for it
            while self.activating.load(std::sync::atomic::Ordering::Acquire) {
                tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
            }
            return Ok(());
        }

        struct ActivationGuard(Arc<std::sync::atomic::AtomicBool>);
        impl Drop for ActivationGuard {
            fn drop(&mut self) {
                self.0.store(false, std::sync::atomic::Ordering::Release);
            }
        }
        let _guard = ActivationGuard(Arc::clone(&self.activating));

        if self
            .eis_activated
            .load(std::sync::atomic::Ordering::Acquire)
        {
            // Check if socket is still alive
            let ctx = self.context.read().await;
            if let Some(ref c) = *ctx {
                if c.flush().is_ok() {
                    return Ok(());
                }
                warn!("[libei] EIS socket dead on reactivation, creating new session");
            }
            drop(ctx);
            // Socket is dead — reset state for fresh activation
            self.devices.clear().await;
            self.seats.lock().await.clear();
            self.eis_activated
                .store(false, std::sync::atomic::Ordering::Release);
        }

        info!("[libei] Activating EIS — calling ConnectToEIS");

        let session = self.portal_session.read().await;
        let fd = self
            .remote_desktop
            .connect_to_eis(
                &session,
                ashpd::desktop::remote_desktop::ConnectToEISOptions::default(),
            )
            .await
            .context("ConnectToEIS failed")?;
        drop(session);

        let stream = UnixStream::from(fd);
        let context = ei::Context::new(stream).context("Failed to create EIS context")?;

        let mut events =
            EisEventStream::new(context.clone()).context("Failed to create EIS event stream")?;

        let handshake_resp = ei_handshake(&mut events, "lamco-rdp-server", HANDSHAKE_TIMEOUT)
            .await
            .context("EIS handshake failed")?;

        info!("[libei] EIS handshake complete");

        if let Err(e) = context.flush() {
            warn!("[libei] Context flush after handshake failed: {}", e);
        }

        // Store context
        {
            let mut ctx = self.context.write().await;
            *ctx = Some(context);
        }
        *self.connection.lock().await = Some(handshake_resp.connection);
        *self.devices.last_serial.lock().await = handshake_resp.serial;
        self.devices.device_version.store(
            handshake_resp
                .negotiated_interfaces
                .get("ei_device")
                .copied()
                .unwrap_or(0),
            std::sync::atomic::Ordering::Relaxed,
        );

        // Process the seat+device setup burst, ending on a deterministic
        // barrier rather than a quiet-time guess. `seat.done` issues bind +
        // connection.sync(); per the EI protocol the server emits its
        // ei_callback.done only after every event resulting from those prior
        // requests (the whole seat/device/resumed burst) has been sent, so the
        // callback.done is our reliable "setup drained" signal regardless of how
        // long the burst takes. The per-event timeout is a 3s failsafe only (the
        // old 500ms quiet-timeout could cut off a slow burst — the suspected
        // cause of the GNOME 46 zero-devices failure).
        loop {
            match tokio::time::timeout(tokio::time::Duration::from_secs(3), events.next()).await {
                Ok(Some(Ok(PendingRequestResult::Request(event)))) => {
                    let sync_barrier_reached = matches!(
                        &event,
                        ei::Event::Callback(_, ei::callback::Event::Done { .. })
                    );
                    self.handle_event(event).await?;
                    if sync_barrier_reached {
                        debug!("[libei] setup sync barrier reached — device burst drained");
                        break;
                    }
                }
                Ok(Some(Ok(PendingRequestResult::ParseError(msg)))) => {
                    warn!("[libei] EIS parse error during setup: {}", msg);
                }
                Ok(Some(Ok(PendingRequestResult::InvalidObject(id)))) => {
                    debug!("[libei] Invalid object during setup: {}", id);
                }
                Ok(Some(Err(e))) => {
                    error!("[libei] EIS event stream error during setup: {}", e);
                    return Err(e.into());
                }
                Ok(None) => {
                    // Stream EOF — setup complete
                    debug!("[libei] Event stream EOF during setup");
                    break;
                }
                Err(_) => {
                    warn!("[libei] setup barrier failsafe timeout (3s) — sync never completed");
                    break;
                }
            }
        }

        info!("[libei] EIS setup complete — devices ready for input injection");
        self.eis_activated
            .store(true, std::sync::atomic::Ordering::Release);

        if let Some(r) = self.health_reporter.get() {
            r.report(HealthEvent::EisStreamRecovered);
        }

        Ok(())
    }

    async fn handle_event(&self, event: ei::Event) -> Result<()> {
        match event {
            ei::Event::Connection(_connection, request) => match request {
                ei::connection::Event::Seat { seat } => {
                    debug!("[libei] Seat added");
                    let mut seats = self.seats.lock().await;
                    seats.insert(seat, SeatData::default());
                }
                ei::connection::Event::Ping { ping } => {
                    ping.done(0);
                    if let Some(ref ctx) = *self.context.read().await {
                        let _ = ctx.flush();
                    }
                }
                _ => {}
            },

            ei::Event::Seat(seat, request) => {
                let mut seats = self.seats.lock().await;
                let Some(data) = seats.get_mut(&seat) else {
                    warn!("[libei] Received event for unknown seat, ignoring");
                    return Ok(());
                };

                match request {
                    ei::seat::Event::Name { name } => {
                        data.name = Some(name.clone());
                        debug!("[libei] Seat name: {}", name);
                    }
                    ei::seat::Event::Capability { mask, interface } => {
                        data.capabilities.insert(interface.clone(), mask);
                        debug!("[libei] Seat capability: {} (mask: {})", interface, mask);
                    }
                    ei::seat::Event::Done => {
                        let caps = data.capabilities.values().fold(0, |a, b| a | b);
                        seat.bind(caps);
                        if let Some(ref conn) = *self.connection.lock().await {
                            conn.sync(1);
                        }
                        if let Some(ref ctx) = *self.context.read().await {
                            let _ = ctx.flush();
                        }

                        info!(
                            "libei: Seat '{}' ready with capabilities: {:?}",
                            data.name.as_deref().unwrap_or("unknown"),
                            data.capabilities.keys().collect::<Vec<_>>()
                        );
                    }
                    ei::seat::Event::Device { device } => {
                        debug!("[libei] Device added to seat");
                        let mut devs = self.devices.all.lock().await;
                        devs.insert(
                            device,
                            DeviceData {
                                seat: Some(seat.clone()),
                                ..Default::default()
                            },
                        );
                    }
                    _ => {}
                }
            }

            ei::Event::Device(device, request) => {
                let mut devs = self.devices.all.lock().await;
                let Some(data) = devs.get_mut(&device) else {
                    warn!("[libei] Received event for unknown device, ignoring");
                    return Ok(());
                };

                match request {
                    ei::device::Event::Name { name } => {
                        data.name = Some(name.clone());
                        debug!("[libei] Device name: {}", name);
                    }
                    ei::device::Event::DeviceType { device_type } => {
                        data.device_type = Some(device_type);
                        debug!("[libei] Device type: {:?}", device_type);
                    }
                    ei::device::Event::Interface { object } => {
                        let interface_name = object.interface().to_owned();
                        data.interfaces.insert(interface_name.clone(), object);
                        info!("[libei] Device interface: {}", interface_name);
                    }
                    ei::device::Event::Region {
                        offset_x,
                        offset_y,
                        width,
                        hight,
                        scale,
                    } => {
                        info!(
                            "[libei] Device region: {}x{} at ({},{}) scale={}",
                            width, hight, offset_x, offset_y, scale
                        );
                        data.regions.push(eis_common::DeviceRegion {
                            x: offset_x,
                            y: offset_y,
                            width,
                            height: hight,
                        });
                    }
                    ei::device::Event::Done => {
                        // Assign device roles (keyboard, pointer, touch).
                        // start_emulating is called in Resumed, not here --
                        // calling it in Done + Resumed causes KWin to reject
                        // the second call with "Invalid device state 3"
                        // and disconnect the client.
                        eis_common::assign_device_roles(&device, data, &self.devices).await;
                        // v3 devices require ready() after done before the
                        // server will emit resumed (and thus start_emulating).
                        if eis_common::eis_device_ready(&self.devices, &device)
                            && let Some(ref ctx) = *self.context.read().await
                        {
                            let _ = ctx.flush();
                        }
                        info!(
                            "[libei] Device '{}' setup complete",
                            data.name.as_deref().unwrap_or("unknown"),
                        );
                    }
                    ei::device::Event::Resumed { serial } => {
                        *self.devices.last_serial.lock().await = serial;
                        device.start_emulating(serial.wrapping_add(1), serial);
                        if let Some(ref ctx) = *self.context.read().await {
                            let _ = ctx.flush();
                        }
                        info!("[libei] Device resumed and emulating (serial={})", serial);
                    }
                    _ => {}
                }
            }

            // Response to `connection.sync()`. We do not yet gate readiness on
            // it (see the reis/EIS roadmap), but consuming it explicitly makes
            // the handshake sequence observable rather than swallowed.
            ei::Event::Callback(_callback, ei::callback::Event::Done { callback_data }) => {
                tracing::debug!("[libei] sync callback done (data={callback_data})");
            }

            _ => {}
        }

        Ok(())
    }
}

#[async_trait]
impl SessionHandle for LibeiSessionHandleImpl {
    fn set_health_reporter(&self, reporter: HealthReporter) {
        let _ = self.health_reporter.set(reporter);
    }

    fn pipewire_access(&self) -> PipeWireAccess {
        // Video is acquired via ScreenCast on this same RemoteDesktop session (#51).
        PipeWireAccess::FileDescriptor(self.video_fd)
    }

    fn streams(&self) -> Vec<StreamInfo> {
        futures::executor::block_on(async { self.streams.lock().await.clone() })
    }

    fn session_type(&self) -> SessionType {
        SessionType::Libei
    }

    async fn activate_input(&self) -> Result<()> {
        self.activate_eis().await
    }

    async fn notify_keyboard_keycode(&self, keycode: i32, pressed: bool) -> Result<()> {
        eis_common::eis_keyboard_keycode(&self.context, &self.devices, keycode, pressed).await
    }

    async fn notify_keyboard_keysym(&self, keysym: u32, pressed: bool) -> Result<()> {
        eis_common::eis_text_keysym(&self.context, &self.devices, keysym, pressed).await
    }

    async fn notify_pointer_motion_absolute(&self, stream_id: u32, x: f64, y: f64) -> Result<()> {
        let stream_offset = self
            .streams
            .lock()
            .await
            .iter()
            .find(|s| s.node_id == stream_id)
            .map(|s| (s.position_x as f64, s.position_y as f64));
        eis_common::eis_pointer_motion_absolute(&self.context, &self.devices, x, y, stream_offset)
            .await
    }

    async fn notify_pointer_button(&self, button: i32, pressed: bool) -> Result<()> {
        eis_common::eis_pointer_button(&self.context, &self.devices, button, pressed).await
    }

    async fn notify_pointer_axis(&self, dx: f64, dy: f64) -> Result<()> {
        eis_common::eis_pointer_axis(&self.context, &self.devices, dx, dy).await
    }

    async fn notify_pointer_axis_discrete(&self, dx: i32, dy: i32) -> Result<()> {
        eis_common::eis_pointer_axis_discrete(&self.context, &self.devices, dx, dy).await
    }

    async fn notify_pointer_motion_relative(&self, dx: f64, dy: f64) -> Result<()> {
        eis_common::eis_pointer_motion_relative(&self.context, &self.devices, dx, dy).await
    }

    async fn stage_pointer_motion_relative(&self, dx: f64, dy: f64) -> Result<()> {
        eis_common::stage_pointer_motion_relative(&self.devices, dx, dy).await
    }

    async fn stage_pointer_button(&self, button: i32, pressed: bool) -> Result<()> {
        eis_common::stage_pointer_button(&self.devices, button, pressed).await
    }

    async fn stage_pointer_axis(&self, dx: f64, dy: f64) -> Result<()> {
        eis_common::stage_pointer_axis(&self.devices, dx, dy).await
    }

    async fn stage_pointer_axis_discrete(&self, dx_120: i32, dy_120: i32) -> Result<()> {
        // dx_120/dy_120 are already in 120-units-per-detent -- the same
        // convention ei_scroll.scroll_discrete uses, passed straight
        // through (mirrors notify_pointer_axis_discrete above).
        eis_common::stage_pointer_axis_discrete(&self.devices, dx_120, dy_120).await
    }

    async fn commit_input_batch(&self) -> Result<()> {
        eis_common::commit_pointer_frame(&self.context, &self.devices).await
    }

    async fn notify_touch_down(&self, _stream_id: u32, slot: u32, x: f64, y: f64) -> Result<()> {
        eis_common::eis_touch_down(&self.context, &self.devices, slot, x, y).await
    }

    async fn notify_touch_motion(&self, _stream_id: u32, slot: u32, x: f64, y: f64) -> Result<()> {
        eis_common::eis_touch_motion(&self.context, &self.devices, slot, x, y).await
    }

    async fn notify_touch_up(&self, slot: u32) -> Result<()> {
        eis_common::eis_touch_up(&self.context, &self.devices, slot).await
    }

    fn clipboard_source(&self) -> crate::session::strategy::ClipboardSource {
        crate::session::strategy::ClipboardSource::None
    }

    #[cfg(feature = "wl-clipboard")]
    async fn build_clipboard(
        &self,
        _portal_fallback: Option<crate::session::strategy::ClipboardComponents>,
        _rate_limit_ms: u64,
    ) -> Option<std::sync::Arc<dyn crate::clipboard::provider::ClipboardProvider>> {
        // Wayland data-control clipboard (wl-clipboard-rs) — no second Portal
        // session, so libei keeps EIS input without the duplicate-session
        // permission prompt / input-routing coupling.
        Some(std::sync::Arc::new(
            crate::clipboard::providers::WlClipboardProvider::new(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[ignore = "Requires Portal RemoteDesktop"]
    async fn test_libei_availability() {
        let available = LibeiStrategy::is_available().await;
        println!("libei available: {available}");
    }

    #[tokio::test]
    #[ignore = "Requires active Portal session and user approval"]
    async fn test_create_session() {
        if !LibeiStrategy::is_available().await {
            println!("Skipping: libei not available");
            return;
        }

        let strategy = LibeiStrategy::new(None, None);
        match strategy.create_session().await {
            Ok(session) => {
                assert_eq!(session.session_type(), SessionType::Libei);
                println!("libei session created successfully");

                // Give event loop time to discover devices
                tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
            }
            Err(e) => {
                println!("libei session creation failed: {e}");
            }
        }
    }
}
