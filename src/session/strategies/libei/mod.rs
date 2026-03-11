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
};
use async_trait::async_trait;
use futures::stream::StreamExt;
use reis::{PendingRequestResult, ei, tokio::EiEventStream};
use tokio::sync::{Mutex, RwLock};
use tracing::{debug, error, info, warn};

use super::eis_common::{self, DeviceData, EisDevices};
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
            .create_session()
            .await
            .context("Failed to create RemoteDesktop session")?;

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

        remote_desktop
            .select_devices(
                &session,
                DeviceType::Keyboard | DeviceType::Pointer | DeviceType::Touchscreen,
                restore_token.as_deref(),
                PersistMode::ExplicitlyRevoked,
            )
            .await
            .context("Failed to select input devices")?;

        info!("libei: Selected keyboard, pointer, and touchscreen devices");

        let response = remote_desktop
            .start(&session, None)
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

        info!("libei: RemoteDesktop session started, calling ConnectToEIS");

        let fd = remote_desktop
            .connect_to_eis(&session)
            .await
            .context("ConnectToEIS failed - portal may not support this method (requires v2+)")?;

        let stream = UnixStream::from(fd);
        let context =
            ei::Context::new(stream).context("Failed to create EIS context from socket")?;

        let mut events =
            EiEventStream::new(context.clone()).context("Failed to create EIS event stream")?;

        let handshake_resp = reis::tokio::ei_handshake(
            &mut events,
            "lamco-rdp-server",
            ei::handshake::ContextType::Sender,
        )
        .await
        .context("EIS handshake failed")?;

        info!("libei: EIS handshake complete");

        let handle = Arc::new(LibeiSessionHandleImpl {
            portal_session: Arc::new(RwLock::new(session)),
            context: Arc::new(context),
            connection: Arc::new(Mutex::new(handshake_resp.connection)),
            event_stream: Arc::new(Mutex::new(events)),
            seats: Arc::new(Mutex::new(HashMap::new())),
            devices: Arc::new(EisDevices::new(handshake_resp.serial)),
            streams: Arc::new(Mutex::new(vec![])),
            health_reporter: std::sync::OnceLock::new(),
        });

        let handle_clone = handle.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_clone.event_loop().await {
                error!("libei: Event loop error: {:#}", e);
            }
        });

        info!("libei: Session created with background event loop");

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
pub struct LibeiSessionHandleImpl {
    #[expect(dead_code, reason = "keeps Portal session alive for EIS lifetime")]
    portal_session: Arc<RwLock<ashpd::desktop::Session<'static, RemoteDesktop<'static>>>>,
    context: Arc<ei::Context>,
    connection: Arc<Mutex<ei::Connection>>,
    event_stream: Arc<Mutex<EiEventStream>>,
    seats: Arc<Mutex<HashMap<ei::Seat, SeatData>>>,
    devices: Arc<EisDevices>,
    streams: Arc<Mutex<Vec<StreamInfo>>>,
    health_reporter: std::sync::OnceLock<HealthReporter>,
}

impl LibeiSessionHandleImpl {
    /// Background event loop for EIS protocol
    async fn event_loop(&self) -> Result<()> {
        let mut events = self.event_stream.lock().await;

        while let Some(result) = events.next().await {
            let event = match result {
                Ok(PendingRequestResult::Request(event)) => event,
                Ok(PendingRequestResult::ParseError(msg)) => {
                    warn!("libei: EIS parse error: {}", msg);
                    continue;
                }
                Ok(PendingRequestResult::InvalidObject(obj_id)) => {
                    debug!("[libei] Invalid object ID: {}", obj_id);
                    continue;
                }
                Err(e) => {
                    error!("libei: Event stream error: {}", e);
                    if let Some(r) = self.health_reporter.get() {
                        r.report(HealthEvent::EisStreamEnded {
                            reason: format!("stream error: {e}"),
                        });
                    }
                    return Err(e.into());
                }
            };

            self.handle_event(event).await?;
        }

        info!("libei: Event loop terminated");
        if let Some(r) = self.health_reporter.get() {
            r.report(HealthEvent::EisStreamEnded {
                reason: "stream EOF".into(),
            });
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
                    let _ = self.context.flush();
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
                        let connection = self.connection.lock().await;
                        connection.sync(1);
                        drop(connection);
                        let _ = self.context.flush();

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
                        debug!("[libei] Device interface: {}", interface_name);
                    }
                    ei::device::Event::Done => {
                        eis_common::assign_device_roles(&device, data, &self.devices).await;
                        debug!(
                            "[libei] Device '{}' ready with interfaces: {:?}",
                            data.name.as_deref().unwrap_or("unknown"),
                            data.interfaces.keys().collect::<Vec<_>>()
                        );
                    }
                    ei::device::Event::Resumed { serial } => {
                        *self.devices.last_serial.lock().await = serial;
                        debug!("[libei] Device resumed with serial: {}", serial);
                    }
                    _ => {}
                }
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
        // libei provides input only; video comes from Portal ScreenCast
        warn!(
            "libei: pipewire_access() called but this strategy provides input only. \
             Video capture requires Portal ScreenCast."
        );
        PipeWireAccess::NodeId(0)
    }

    fn streams(&self) -> Vec<StreamInfo> {
        futures::executor::block_on(async { self.streams.lock().await.clone() })
    }

    fn session_type(&self) -> SessionType {
        SessionType::Libei
    }

    async fn notify_keyboard_keycode(&self, keycode: i32, pressed: bool) -> Result<()> {
        eis_common::eis_keyboard_keycode(&self.context, &self.devices, keycode, pressed).await
    }

    async fn notify_pointer_motion_absolute(&self, _stream_id: u32, x: f64, y: f64) -> Result<()> {
        eis_common::eis_pointer_motion_absolute(&self.context, &self.devices, x, y).await
    }

    async fn notify_pointer_button(&self, button: i32, pressed: bool) -> Result<()> {
        eis_common::eis_pointer_button(&self.context, &self.devices, button, pressed).await
    }

    async fn notify_pointer_axis(&self, dx: f64, dy: f64) -> Result<()> {
        eis_common::eis_pointer_axis(&self.context, &self.devices, dx, dy).await
    }

    async fn notify_pointer_motion_relative(&self, dx: f64, dy: f64) -> Result<()> {
        eis_common::eis_pointer_motion_relative(&self.context, &self.devices, dx, dy).await
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[ignore = "Requires Portal RemoteDesktop"]
    async fn test_libei_availability() {
        let available = LibeiStrategy::is_available().await;
        println!("libei available: {}", available);
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
                println!("libei session creation failed: {}", e);
            }
        }
    }
}
