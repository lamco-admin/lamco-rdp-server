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
//!   ↓ D-Bus
//! Portal RemoteDesktop
//!   ├─ CreateSession()
//!   ├─ SelectDevices(keyboard, pointer)
//!   ├─ Start() → user approves if needed
//!   └─ ConnectToEIS() → Unix socket FD
//!       ↓
//! EIS Protocol (via reis crate)
//!   ├─ Handshake (version, capabilities)
//!   ├─ Seat discovery
//!   ├─ Device creation (keyboard, pointer)
//!   └─ Event streaming (key, motion, button, scroll)
//!       ↓
//! Portal backend (xdg-desktop-portal-wlr, hyprland, etc.)
//!   └─ Compositor protocols (zwp_virtual_keyboard, zwlr_virtual_pointer)
//! ```
//!
//! # Event-Driven Model
//!
//! The EIS protocol is event-driven:
//! 1. Create context from socket FD
//! 2. Perform handshake → receive connection and event stream
//! 3. Listen for SeatAdded events → bind capabilities
//! 4. Listen for DeviceAdded events → get keyboard/pointer devices
//! 5. Send input events via devices
//! 6. Frame events to group related inputs
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

use anyhow::{anyhow, Context as AnyhowContext, Result};
use ashpd::desktop::{
    remote_desktop::{DeviceType, RemoteDesktop},
    PersistMode,
};
use async_trait::async_trait;
use futures::stream::StreamExt;
use reis::{ei, tokio::EiEventStream, PendingRequestResult};
use tokio::sync::{Mutex, RwLock};
use tracing::{debug, error, info, warn};

use crate::session::strategy::{
    ClipboardComponents, PipeWireAccess, SessionHandle, SessionStrategy, SessionType, StreamInfo,
};

/// libei/EIS strategy implementation
///
/// Provides input injection via Portal RemoteDesktop + EIS protocol.
pub struct LibeiStrategy {
    /// Optional Portal manager for session management
    portal_manager: Option<Arc<lamco_portal::PortalManager>>,
}

impl LibeiStrategy {
    pub fn new(portal_manager: Option<Arc<lamco_portal::PortalManager>>) -> Self {
        Self { portal_manager }
    }

    pub async fn is_available() -> bool {
        match RemoteDesktop::new().await {
            Ok(rd) => {
                // Try to check if ConnectToEIS is available
                // This is a v2+ method, older portals won't have it
                debug!("[libei] Portal RemoteDesktop proxy created successfully");
                // We can't easily check for ConnectToEIS without creating a session
                // Assume available if RemoteDesktop portal exists
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
        Self::new(None)
    }
}

#[async_trait]
impl SessionStrategy for LibeiStrategy {
    fn name(&self) -> &'static str {
        "libei"
    }

    fn requires_initial_setup(&self) -> bool {
        // Portal RemoteDesktop requires user approval
        true
    }

    fn supports_unattended_restore(&self) -> bool {
        // Supports restore tokens if Portal v4+
        true
    }

    async fn create_session(&self) -> Result<Arc<dyn SessionHandle>> {
        info!("🚀 libei: Creating session with Portal RemoteDesktop + EIS");

        let remote_desktop = RemoteDesktop::new()
            .await
            .context("Failed to create RemoteDesktop proxy")?;

        info!("🔌 libei: Creating Portal RemoteDesktop session");

        let session = remote_desktop
            .create_session()
            .await
            .context("Failed to create RemoteDesktop session")?;

        remote_desktop
            .select_devices(
                &session,
                DeviceType::Keyboard | DeviceType::Pointer,
                None,
                PersistMode::DoNot, // TODO: Support token persistence
            )
            .await
            .context("Failed to select input devices")?;

        info!("✅ libei: Selected keyboard and pointer devices");

        remote_desktop
            .start(&session, None)
            .await
            .context("Failed to start RemoteDesktop session")?;

        info!("✅ libei: RemoteDesktop session started");

        info!("🔌 libei: Calling ConnectToEIS to get socket FD");

        let fd = remote_desktop
            .connect_to_eis(&session)
            .await
            .context("ConnectToEIS failed - portal may not support this method (requires v2+)")?;

        info!("✅ libei: Received EIS socket FD");

        let stream = UnixStream::from(fd);

        let context =
            ei::Context::new(stream).context("Failed to create EIS context from socket")?;

        info!("🔑 libei: EIS context created, performing handshake");

        let mut events =
            EiEventStream::new(context.clone()).context("Failed to create EIS event stream")?;

        let handshake_resp = reis::tokio::ei_handshake(
            &mut events,
            "lamco-rdp-server",
            ei::handshake::ContextType::Sender,
        )
        .await
        .context("EIS handshake failed")?;

        info!("✅ libei: EIS handshake complete, connection established");

        let handle = Arc::new(LibeiSessionHandleImpl {
            portal_session: Arc::new(RwLock::new(session)),
            context: Arc::new(context),
            connection: Arc::new(Mutex::new(handshake_resp.connection)),
            event_stream: Arc::new(Mutex::new(events)),
            seats: Arc::new(Mutex::new(HashMap::new())),
            devices: Arc::new(Mutex::new(HashMap::new())),
            keyboard_device: Arc::new(Mutex::new(None)),
            pointer_device: Arc::new(Mutex::new(None)),
            streams: Arc::new(Mutex::new(vec![])),
            last_serial: Arc::new(Mutex::new(handshake_resp.serial)),
        });

        let handle_clone = handle.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_clone.event_loop().await {
                error!("❌ libei: Event loop error: {:#}", e);
            }
        });

        info!("✅ libei: Session created with background event loop");

        Ok(handle as Arc<dyn SessionHandle>)
    }

    async fn cleanup(&self, _session: &dyn SessionHandle) -> Result<()> {
        info!("🔒 libei: Session cleanup complete");
        Ok(())
    }
}

/// Device data for EIS devices
#[derive(Default)]
struct DeviceData {
    name: Option<String>,
    device_type: Option<ei::device::DeviceType>,
    interfaces: HashMap<String, reis::Object>,
    seat: Option<ei::Seat>,
}

impl DeviceData {
    fn interface<T: reis::Interface>(&self) -> Option<T> {
        self.interfaces.get(T::NAME)?.clone().downcast()
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
    portal_session: Arc<RwLock<ashpd::desktop::Session<'static, RemoteDesktop<'static>>>>,
    context: Arc<ei::Context>,
    connection: Arc<Mutex<ei::Connection>>,
    event_stream: Arc<Mutex<EiEventStream>>,
    seats: Arc<Mutex<HashMap<ei::Seat, SeatData>>>,
    devices: Arc<Mutex<HashMap<ei::Device, DeviceData>>>,
    keyboard_device: Arc<Mutex<Option<ei::Device>>>,
    pointer_device: Arc<Mutex<Option<ei::Device>>>,
    streams: Arc<Mutex<Vec<StreamInfo>>>,
    last_serial: Arc<Mutex<u32>>,
}

impl LibeiSessionHandleImpl {
    /// Background event loop for EIS protocol
    ///
    /// Handles seat/device discovery and maintains EIS connection state.
    async fn event_loop(&self) -> Result<()> {
        let mut events = self.event_stream.lock().await;

        while let Some(result) = events.next().await {
            let event = match result {
                Ok(PendingRequestResult::Request(event)) => event,
                Ok(PendingRequestResult::ParseError(msg)) => {
                    warn!("⚠️  libei: EIS parse error: {}", msg);
                    continue;
                }
                Ok(PendingRequestResult::InvalidObject(obj_id)) => {
                    debug!("[libei] Invalid object ID: {}", obj_id);
                    continue;
                }
                Err(e) => {
                    error!("❌ libei: Event stream error: {}", e);
                    return Err(e.into());
                }
            };

            self.handle_event(event).await?;
        }

        info!("🔌 libei: Event loop terminated");
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
                let data = seats.get_mut(&seat).unwrap();

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
                            "✅ libei: Seat '{}' ready with capabilities: {:?}",
                            data.name.as_deref().unwrap_or("unknown"),
                            data.capabilities.keys().collect::<Vec<_>>()
                        );
                    }
                    ei::seat::Event::Device { device } => {
                        debug!("[libei] Device added to seat");
                        let mut devices = self.devices.lock().await;
                        devices.insert(
                            device.clone(),
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
                let mut devices = self.devices.lock().await;
                let data = devices.get_mut(&device).unwrap();

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
                        if let Some(device_type) = data.device_type {
                            match device_type {
                                ei::device::DeviceType::Physical => {
                                    // Physical device - for InputCapture (receiver)
                                    // We're a sender, so ignore
                                }
                                ei::device::DeviceType::Virtual => {
                                    // Virtual device - for RemoteDesktop (sender)
                                    if data.interface::<ei::Keyboard>().is_some() {
                                        debug!("[libei] Found keyboard device");
                                        let mut kbd = self.keyboard_device.lock().await;
                                        *kbd = Some(device.clone());
                                        info!("✅ libei: Keyboard device ready");
                                    }
                                    if data.interface::<ei::Pointer>().is_some()
                                        || data.interface::<ei::PointerAbsolute>().is_some()
                                    {
                                        debug!("[libei] Found pointer device");
                                        let mut ptr = self.pointer_device.lock().await;
                                        *ptr = Some(device.clone());
                                        info!("✅ libei: Pointer device ready");
                                    }
                                }
                            }
                        }

                        debug!(
                            "[libei] Device '{}' ready with interfaces: {:?}",
                            data.name.as_deref().unwrap_or("unknown"),
                            data.interfaces.keys().collect::<Vec<_>>()
                        );
                    }
                    ei::device::Event::Resumed { serial } => {
                        *self.last_serial.lock().await = serial;
                        debug!("[libei] Device resumed with serial: {}", serial);
                    }
                    _ => {}
                }
            }

            _ => {
                // Ignore other events (keyboard keymap, etc.)
            }
        }

        Ok(())
    }

    async fn current_serial(&self) -> u32 {
        *self.last_serial.lock().await
    }

    fn current_time_us() -> u64 {
        use std::time::{SystemTime, UNIX_EPOCH};
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_micros() as u64
    }
}

#[async_trait]
impl SessionHandle for LibeiSessionHandleImpl {
    fn pipewire_access(&self) -> PipeWireAccess {
        // libei provides input only (no video capture)
        // Video comes from Portal ScreenCast (separate session)
        warn!(
            "⚠️  libei: pipewire_access() called but this strategy provides input only. \
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
        let kbd_device_opt = {
            let kbd = self.keyboard_device.lock().await;
            kbd.clone()
        };

        let device = kbd_device_opt.ok_or_else(|| anyhow!("Keyboard device not yet available"))?;

        let devices = self.devices.lock().await;
        let device_data = devices
            .get(&device)
            .ok_or_else(|| anyhow!("Keyboard device data not found"))?;

        let keyboard = device_data
            .interface::<ei::Keyboard>()
            .ok_or_else(|| anyhow!("Keyboard interface not found on device"))?;

        drop(devices);

        // EIS keycodes are offset by 8 from evdev (Linux kernel offset)
        let eis_keycode = (keycode - 8) as u32;
        let state = if pressed {
            ei::keyboard::KeyState::Press
        } else {
            ei::keyboard::KeyState::Released
        };

        keyboard.key(eis_keycode, state);

        let serial = self.current_serial().await;
        let time = Self::current_time_us();
        device.frame(serial, time);

        self.context.flush()?;

        debug!(
            "[libei] Keyboard event: keycode={} (eis={}), pressed={}",
            keycode, eis_keycode, pressed
        );

        Ok(())
    }

    async fn notify_pointer_motion_absolute(&self, stream_id: u32, x: f64, y: f64) -> Result<()> {
        let ptr_device_opt = {
            let ptr = self.pointer_device.lock().await;
            ptr.clone()
        };

        let device = ptr_device_opt.ok_or_else(|| anyhow!("Pointer device not yet available"))?;

        let devices = self.devices.lock().await;
        let device_data = devices
            .get(&device)
            .ok_or_else(|| anyhow!("Pointer device data not found"))?;

        let pointer_abs = device_data
            .interface::<ei::PointerAbsolute>()
            .ok_or_else(|| anyhow!("PointerAbsolute interface not found on device"))?;

        drop(devices);

        pointer_abs.motion_absolute(x as f32, y as f32);

        let serial = self.current_serial().await;
        let time = Self::current_time_us();
        device.frame(serial, time);

        self.context.flush()?;

        debug!(
            "[libei] Pointer motion: stream={}, x={}, y={}",
            stream_id, x, y
        );

        Ok(())
    }

    async fn notify_pointer_button(&self, button: i32, pressed: bool) -> Result<()> {
        let ptr_device_opt = {
            let ptr = self.pointer_device.lock().await;
            ptr.clone()
        };

        let device = ptr_device_opt.ok_or_else(|| anyhow!("Pointer device not yet available"))?;

        let devices = self.devices.lock().await;
        let device_data = devices
            .get(&device)
            .ok_or_else(|| anyhow!("Pointer device data not found"))?;

        let button_interface = device_data
            .interface::<ei::Button>()
            .ok_or_else(|| anyhow!("Button interface not found on device"))?;

        drop(devices);

        button_interface.button(
            button as u32,
            if pressed {
                ei::button::ButtonState::Press
            } else {
                ei::button::ButtonState::Released
            },
        );

        let serial = self.current_serial().await;
        let time = Self::current_time_us();
        device.frame(serial, time);

        self.context.flush()?;

        debug!(
            "[libei] Pointer button: button={}, pressed={}",
            button, pressed
        );

        Ok(())
    }

    async fn notify_pointer_axis(&self, dx: f64, dy: f64) -> Result<()> {
        let ptr_device_opt = {
            let ptr = self.pointer_device.lock().await;
            ptr.clone()
        };

        let device = ptr_device_opt.ok_or_else(|| anyhow!("Pointer device not yet available"))?;

        let devices = self.devices.lock().await;
        let device_data = devices
            .get(&device)
            .ok_or_else(|| anyhow!("Pointer device data not found"))?;

        let scroll = device_data
            .interface::<ei::Scroll>()
            .ok_or_else(|| anyhow!("Scroll interface not found on device"))?;

        drop(devices);
        if dx.abs() > 0.01 {
            scroll.scroll(dx as f32, 0.0);
        }
        if dy.abs() > 0.01 {
            scroll.scroll(0.0, dy as f32);
        }

        let serial = self.current_serial().await;
        let time = Self::current_time_us();
        device.frame(serial, time);

        self.context.flush()?;

        debug!("[libei] Pointer axis: dx={}, dy={}", dx, dy);

        Ok(())
    }

    fn portal_clipboard(&self) -> Option<ClipboardComponents> {
        // libei can share the Portal session for clipboard
        // The session is managed separately from input devices
        // For now, return None - clipboard would be via separate Portal session
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[ignore] // Requires Portal RemoteDesktop
    async fn test_libei_availability() {
        let available = LibeiStrategy::is_available().await;
        println!("libei available: {}", available);
    }

    #[tokio::test]
    #[ignore] // Requires active Portal session and user approval
    async fn test_create_session() {
        if !LibeiStrategy::is_available().await {
            println!("Skipping: libei not available");
            return;
        }

        let strategy = LibeiStrategy::new(None);
        match strategy.create_session().await {
            Ok(session) => {
                assert_eq!(session.session_type(), SessionType::Libei);
                println!("✅ libei session created successfully");

                // Give event loop time to discover devices
                tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
            }
            Err(e) => {
                println!("❌ libei session creation failed: {}", e);
            }
        }
    }
}
