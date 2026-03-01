//! Mutter Direct API Strategy Implementation
//!
//! Uses org.gnome.Mutter.ScreenCast and org.gnome.Mutter.RemoteDesktop D-Bus APIs
//! directly, bypassing the XDG Portal permission model entirely.
//!
//! GNOME-specific, zero-dialog operation. Supports three input paths:
//! - EIS (GNOME 46+): Low-latency input via libei/reis
//! - D-Bus (GNOME 45+): Input injection via RemoteDesktop session methods
//!
//! Clipboard is handled natively via Mutter's RemoteDesktop clipboard methods,
//! eliminating the need for a hybrid Portal session.

use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use futures_util::StreamExt;
#[cfg(feature = "libei")]
use tracing::warn;
use tracing::{debug, error, info};

use crate::{
    health::{HealthEvent, HealthReporter},
    mutter::{MutterSessionHandle, MutterSessionManager},
    session::strategy::{PipeWireAccess, SessionHandle, SessionStrategy, SessionType, StreamInfo},
};

/// Input method selection for Mutter strategy
#[cfg(feature = "libei")]
enum MutterInputMethod {
    /// EIS protocol via ConnectToEIS (GNOME 46+, lower latency)
    Eis {
        context: Arc<reis::ei::Context>,
        keyboard_device: Arc<tokio::sync::Mutex<Option<reis::ei::Device>>>,
        pointer_device: Arc<tokio::sync::Mutex<Option<reis::ei::Device>>>,
        devices: Arc<tokio::sync::Mutex<std::collections::HashMap<reis::ei::Device, DeviceData>>>,
        last_serial: Arc<tokio::sync::Mutex<u32>>,
    },
    /// D-Bus methods on RemoteDesktop.Session (universal fallback)
    Dbus,
}

/// Device data for EIS devices (shared with libei strategy)
#[cfg(feature = "libei")]
#[derive(Default)]
struct DeviceData {
    name: Option<String>,
    device_type: Option<reis::ei::device::DeviceType>,
    interfaces: std::collections::HashMap<String, reis::Object>,
}

#[cfg(feature = "libei")]
impl DeviceData {
    fn interface<T: reis::Interface>(&self) -> Option<T> {
        self.interfaces.get(T::NAME)?.clone().downcast()
    }
}

/// Mutter session handle wrapper
pub struct MutterSessionHandleImpl {
    /// Underlying Mutter session
    mutter_handle: MutterSessionHandle,
    /// Input method (EIS or D-Bus)
    #[cfg(feature = "libei")]
    input_method: MutterInputMethod,
    /// Session validity flag — set to false when Mutter session is destroyed
    session_valid: Arc<AtomicBool>,
    /// Health reporter for session lifecycle events (set once after construction)
    health_reporter: Arc<std::sync::OnceLock<HealthReporter>>,
}

impl MutterSessionHandleImpl {
    /// Start listening for Mutter ScreenCast and RemoteDesktop Closed D-Bus signals.
    ///
    /// When the compositor destroys either session, this sets `session_valid` to false
    /// and reports to the health system.
    pub async fn start_closed_listeners(&self) {
        let connection = &self.mutter_handle.connection;

        // ScreenCast session Closed listener
        let sc_path = self.mutter_handle.screencast_session.clone();
        match crate::mutter::MutterScreenCastSession::new(connection, sc_path).await {
            Ok(sc_session) => match sc_session.subscribe_closed().await {
                Ok(stream) => {
                    let valid = Arc::clone(&self.session_valid);
                    let health_reporter = Arc::clone(&self.health_reporter);
                    tokio::spawn(async move {
                        futures_util::pin_mut!(stream);
                        stream.next().await;
                        error!("Mutter ScreenCast session Closed signal received");
                        valid.store(false, Ordering::Release);
                        if let Some(r) = health_reporter.get() {
                            r.report(HealthEvent::SessionClosed {
                                reason: "Mutter ScreenCast Closed signal".into(),
                            });
                        }
                    });
                    info!("Mutter ScreenCast Closed listener started");
                }
                Err(e) => debug!("Could not subscribe to ScreenCast Closed: {e}"),
            },
            Err(e) => debug!("Could not create ScreenCast session proxy: {e}"),
        }

        // RemoteDesktop session Closed listener
        let rd_path = self.mutter_handle.remote_desktop_session.clone();
        match crate::mutter::MutterRemoteDesktopSession::new(connection, rd_path).await {
            Ok(rd_session) => match rd_session.subscribe_closed().await {
                Ok(stream) => {
                    let valid = Arc::clone(&self.session_valid);
                    let health_reporter = Arc::clone(&self.health_reporter);
                    tokio::spawn(async move {
                        futures_util::pin_mut!(stream);
                        stream.next().await;
                        error!("Mutter RemoteDesktop session Closed signal received");
                        valid.store(false, Ordering::Release);
                        if let Some(r) = health_reporter.get() {
                            r.report(HealthEvent::SessionClosed {
                                reason: "Mutter RemoteDesktop Closed signal".into(),
                            });
                        }
                    });
                    info!("Mutter RemoteDesktop Closed listener started");
                }
                Err(e) => debug!("Could not subscribe to RemoteDesktop Closed: {e}"),
            },
            Err(e) => debug!("Could not create RemoteDesktop session proxy: {e}"),
        }
    }
}

#[async_trait]
impl SessionHandle for MutterSessionHandleImpl {
    fn set_health_reporter(&self, reporter: HealthReporter) {
        let _ = self.health_reporter.set(reporter);
    }

    fn pipewire_access(&self) -> PipeWireAccess {
        PipeWireAccess::NodeId(self.mutter_handle.pipewire_node_id())
    }

    fn streams(&self) -> Vec<StreamInfo> {
        self.mutter_handle
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

    async fn notify_keyboard_keycode(&self, keycode: i32, pressed: bool) -> Result<()> {
        if !self.session_valid.load(Ordering::Acquire) {
            return Err(anyhow!(
                "Mutter session invalid — cannot send keyboard event"
            ));
        }

        #[cfg(feature = "libei")]
        if let MutterInputMethod::Eis {
            ref context,
            ref keyboard_device,
            ref devices,
            ref last_serial,
            ..
        } = self.input_method
        {
            return eis_keyboard_keycode(
                context,
                keyboard_device,
                devices,
                last_serial,
                keycode,
                pressed,
            )
            .await;
        }

        // D-Bus fallback: Mutter expects u32 keycode
        let rd_session = self.mutter_handle.remote_desktop_session().await?;
        rd_session
            .notify_keyboard_keycode(keycode as u32, pressed)
            .await
            .context("Failed to inject keyboard keycode via Mutter D-Bus")
    }

    async fn notify_pointer_motion_absolute(&self, _stream_id: u32, x: f64, y: f64) -> Result<()> {
        if !self.session_valid.load(Ordering::Acquire) {
            return Err(anyhow!(
                "Mutter session invalid — cannot send pointer motion"
            ));
        }

        #[cfg(feature = "libei")]
        if let MutterInputMethod::Eis {
            ref context,
            ref pointer_device,
            ref devices,
            ref last_serial,
            ..
        } = self.input_method
        {
            return eis_pointer_motion_absolute(
                context,
                pointer_device,
                devices,
                last_serial,
                x,
                y,
            )
            .await;
        }

        // D-Bus fallback: use stream object path
        let stream_path = self
            .mutter_handle
            .streams
            .first()
            .ok_or_else(|| anyhow!("No streams available"))?;

        let rd_session = self.mutter_handle.remote_desktop_session().await?;
        rd_session
            .notify_pointer_motion_absolute(stream_path, x, y)
            .await
            .context("Failed to inject pointer motion via Mutter D-Bus")
    }

    async fn notify_pointer_button(&self, button: i32, pressed: bool) -> Result<()> {
        if !self.session_valid.load(Ordering::Acquire) {
            return Err(anyhow!(
                "Mutter session invalid — cannot send pointer button"
            ));
        }

        #[cfg(feature = "libei")]
        if let MutterInputMethod::Eis {
            ref context,
            ref pointer_device,
            ref devices,
            ref last_serial,
            ..
        } = self.input_method
        {
            return eis_pointer_button(
                context,
                pointer_device,
                devices,
                last_serial,
                button,
                pressed,
            )
            .await;
        }

        let rd_session = self.mutter_handle.remote_desktop_session().await?;
        rd_session
            .notify_pointer_button(button, pressed)
            .await
            .context("Failed to inject pointer button via Mutter D-Bus")
    }

    async fn notify_pointer_axis(&self, dx: f64, dy: f64) -> Result<()> {
        if !self.session_valid.load(Ordering::Acquire) {
            return Err(anyhow!("Mutter session invalid — cannot send pointer axis"));
        }

        #[cfg(feature = "libei")]
        if let MutterInputMethod::Eis {
            ref context,
            ref pointer_device,
            ref devices,
            ref last_serial,
            ..
        } = self.input_method
        {
            return eis_pointer_axis(context, pointer_device, devices, last_serial, dx, dy).await;
        }

        let rd_session = self.mutter_handle.remote_desktop_session().await?;
        rd_session
            .notify_pointer_axis(dx, dy)
            .await
            .context("Failed to inject pointer axis via Mutter D-Bus")
    }

    fn portal_clipboard(&self) -> Option<crate::session::strategy::ClipboardComponents> {
        // Mutter clipboard is handled natively, not via Portal.
        None
    }

    fn mutter_clipboard(&self) -> Option<std::sync::Arc<crate::mutter::MutterClipboardManager>> {
        self.mutter_handle
            .clipboard
            .as_ref()
            .map(std::sync::Arc::clone)
    }
}

/// Mutter Direct API strategy
///
/// Bypasses portal entirely by using GNOME Mutter's native D-Bus interfaces.
/// Requires GNOME compositor and non-sandboxed application.
pub struct MutterDirectStrategy {
    /// Monitor connector (e.g., "HDMI-1"), or None for virtual monitor
    monitor_connector: Option<String>,
}

impl MutterDirectStrategy {
    pub fn new(monitor_connector: Option<String>) -> Self {
        Self { monitor_connector }
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

        let manager = MutterSessionManager::new()
            .await
            .context("Failed to create Mutter session manager")?;

        #[cfg_attr(
            not(feature = "libei"),
            expect(unused_mut, reason = "mut needed when libei takes eis_fd")
        )]
        let mut mutter_handle = manager
            .create_session(self.monitor_connector.as_deref())
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

        // Set up input method (take EIS FD before moving mutter_handle)
        #[cfg(feature = "libei")]
        let input_method = if let Some(eis_fd) = mutter_handle.eis_fd.take() {
            match setup_eis_input(
                eis_fd,
                Arc::clone(&session_valid),
                Arc::clone(&health_reporter),
            )
            .await
            {
                Ok(method) => {
                    info!("Using EIS for input (low-latency path)");
                    method
                }
                Err(e) => {
                    warn!("EIS setup failed, falling back to D-Bus input: {}", e);
                    MutterInputMethod::Dbus
                }
            }
        } else {
            info!("No EIS FD, using D-Bus input");
            MutterInputMethod::Dbus
        };

        if mutter_handle.clipboard.is_some() {
            info!("Mutter clipboard available (native, no Portal needed)");
        }

        let handle = MutterSessionHandleImpl {
            mutter_handle,
            #[cfg(feature = "libei")]
            input_method,
            session_valid,
            health_reporter,
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

// === EIS Input Implementation (behind libei feature) ===

#[cfg(feature = "libei")]
async fn setup_eis_input(
    fd: std::os::fd::OwnedFd,
    session_valid: Arc<AtomicBool>,
    health_reporter: Arc<std::sync::OnceLock<HealthReporter>>,
) -> Result<MutterInputMethod> {
    use std::os::unix::net::UnixStream;

    use futures::stream::StreamExt;
    use reis::{ei, tokio::EiEventStream, PendingRequestResult};

    let stream = UnixStream::from(fd);
    let context = ei::Context::new(stream).context("Failed to create EIS context")?;
    let mut events =
        EiEventStream::new(context.clone()).context("Failed to create EIS event stream")?;

    let handshake_resp = reis::tokio::ei_handshake(
        &mut events,
        "lamco-rdp-server-mutter",
        ei::handshake::ContextType::Sender,
    )
    .await
    .context("EIS handshake failed")?;

    info!("Mutter EIS handshake complete");

    let context = Arc::new(context);
    let keyboard_device = Arc::new(tokio::sync::Mutex::new(None));
    let pointer_device = Arc::new(tokio::sync::Mutex::new(None));
    let devices: Arc<tokio::sync::Mutex<std::collections::HashMap<reis::ei::Device, DeviceData>>> =
        Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new()));
    let last_serial = Arc::new(tokio::sync::Mutex::new(handshake_resp.serial));

    // Spawn event loop for device discovery
    let ctx_clone = context.clone();
    let kbd_clone = keyboard_device.clone();
    let ptr_clone = pointer_device.clone();
    let dev_clone = devices.clone();
    let serial_clone = last_serial.clone();
    let connection = Arc::new(tokio::sync::Mutex::new(handshake_resp.connection));

    tokio::spawn(async move {
        while let Some(result) = events.next().await {
            let event = match result {
                Ok(PendingRequestResult::Request(event)) => event,
                Ok(PendingRequestResult::ParseError(msg)) => {
                    warn!("Mutter EIS parse error: {}", msg);
                    continue;
                }
                Ok(PendingRequestResult::InvalidObject(_)) => continue,
                Err(e) => {
                    tracing::error!("Mutter EIS event stream error: {}", e);
                    session_valid.store(false, Ordering::Release);
                    if let Some(r) = health_reporter.get() {
                        r.report(HealthEvent::EisStreamEnded {
                            reason: format!("stream error: {e}"),
                        });
                    }
                    return;
                }
            };

            if let Err(e) = handle_eis_event(
                event,
                &ctx_clone,
                &connection,
                &kbd_clone,
                &ptr_clone,
                &dev_clone,
                &serial_clone,
            )
            .await
            {
                tracing::error!("Mutter EIS event handler error: {}", e);
                session_valid.store(false, Ordering::Release);
                if let Some(r) = health_reporter.get() {
                    r.report(HealthEvent::EisStreamEnded {
                        reason: format!("handler error: {e}"),
                    });
                }
                return;
            }
        }
        // EIS stream ended naturally — compositor likely closed the session
        tracing::error!("Mutter EIS event stream ended");
        session_valid.store(false, Ordering::Release);
        if let Some(r) = health_reporter.get() {
            r.report(HealthEvent::EisStreamEnded {
                reason: "stream EOF".into(),
            });
        }
    });

    Ok(MutterInputMethod::Eis {
        context,
        keyboard_device,
        pointer_device,
        devices,
        last_serial,
    })
}

#[cfg(feature = "libei")]
async fn handle_eis_event(
    event: reis::ei::Event,
    context: &reis::ei::Context,
    connection: &tokio::sync::Mutex<reis::ei::Connection>,
    keyboard_device: &tokio::sync::Mutex<Option<reis::ei::Device>>,
    pointer_device: &tokio::sync::Mutex<Option<reis::ei::Device>>,
    devices: &tokio::sync::Mutex<std::collections::HashMap<reis::ei::Device, DeviceData>>,
    last_serial: &tokio::sync::Mutex<u32>,
) -> Result<()> {
    use reis::ei;

    match event {
        ei::Event::Connection(_connection, request) => match request {
            ei::connection::Event::Seat { seat: _ } => {
                debug!("[mutter-eis] Seat added");
                // We'll handle seat binding in the Done event
            }
            ei::connection::Event::Ping { ping } => {
                ping.done(0);
                let _ = context.flush();
            }
            _ => {}
        },

        ei::Event::Seat(seat, request) => match request {
            ei::seat::Event::Capability { mask, interface } => {
                debug!(
                    "[mutter-eis] Seat capability: {} (mask: {})",
                    interface, mask
                );
            }
            ei::seat::Event::Done => {
                // Bind all capabilities
                seat.bind(u64::MAX);
                let conn = connection.lock().await;
                conn.sync(1);
                drop(conn);
                let _ = context.flush();
                info!("[mutter-eis] Seat bound");
            }
            ei::seat::Event::Device { device } => {
                let mut devs = devices.lock().await;
                devs.insert(device, DeviceData::default());
            }
            _ => {}
        },

        ei::Event::Device(device, request) => {
            let mut devs = devices.lock().await;
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
                    if matches!(data.device_type, Some(ei::device::DeviceType::Virtual)) {
                        if data.interface::<ei::Keyboard>().is_some() {
                            *keyboard_device.lock().await = Some(device.clone());
                            info!("[mutter-eis] Keyboard device ready");
                        }
                        if data.interface::<ei::Pointer>().is_some()
                            || data.interface::<ei::PointerAbsolute>().is_some()
                        {
                            *pointer_device.lock().await = Some(device.clone());
                            info!("[mutter-eis] Pointer device ready");
                        }
                    }
                }
                ei::device::Event::Resumed { serial } => {
                    *last_serial.lock().await = serial;
                }
                _ => {}
            }
        }

        _ => {}
    }

    Ok(())
}

#[cfg(feature = "libei")]
fn current_time_us() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_micros() as u64
}

#[cfg(feature = "libei")]
async fn eis_keyboard_keycode(
    context: &reis::ei::Context,
    keyboard_device: &tokio::sync::Mutex<Option<reis::ei::Device>>,
    devices: &tokio::sync::Mutex<std::collections::HashMap<reis::ei::Device, DeviceData>>,
    last_serial: &tokio::sync::Mutex<u32>,
    keycode: i32,
    pressed: bool,
) -> Result<()> {
    use reis::ei;

    let device = keyboard_device
        .lock()
        .await
        .clone()
        .ok_or_else(|| anyhow!("EIS keyboard not ready"))?;

    let devs = devices.lock().await;
    let data = devs
        .get(&device)
        .ok_or_else(|| anyhow!("Device data missing"))?;
    let keyboard = data
        .interface::<ei::Keyboard>()
        .ok_or_else(|| anyhow!("Keyboard interface not found"))?;
    drop(devs);

    // EIS keycodes offset by 8 from evdev
    let eis_keycode = (keycode - 8) as u32;
    let state = if pressed {
        ei::keyboard::KeyState::Press
    } else {
        ei::keyboard::KeyState::Released
    };

    keyboard.key(eis_keycode, state);

    let serial = *last_serial.lock().await;
    device.frame(serial, current_time_us());
    context.flush()?;

    Ok(())
}

#[cfg(feature = "libei")]
async fn eis_pointer_motion_absolute(
    context: &reis::ei::Context,
    pointer_device: &tokio::sync::Mutex<Option<reis::ei::Device>>,
    devices: &tokio::sync::Mutex<std::collections::HashMap<reis::ei::Device, DeviceData>>,
    last_serial: &tokio::sync::Mutex<u32>,
    x: f64,
    y: f64,
) -> Result<()> {
    use reis::ei;

    let device = pointer_device
        .lock()
        .await
        .clone()
        .ok_or_else(|| anyhow!("EIS pointer not ready"))?;

    let devs = devices.lock().await;
    let data = devs
        .get(&device)
        .ok_or_else(|| anyhow!("Device data missing"))?;
    let pointer_abs = data
        .interface::<ei::PointerAbsolute>()
        .ok_or_else(|| anyhow!("PointerAbsolute interface not found"))?;
    drop(devs);

    pointer_abs.motion_absolute(x as f32, y as f32);

    let serial = *last_serial.lock().await;
    device.frame(serial, current_time_us());
    context.flush()?;

    Ok(())
}

#[cfg(feature = "libei")]
async fn eis_pointer_button(
    context: &reis::ei::Context,
    pointer_device: &tokio::sync::Mutex<Option<reis::ei::Device>>,
    devices: &tokio::sync::Mutex<std::collections::HashMap<reis::ei::Device, DeviceData>>,
    last_serial: &tokio::sync::Mutex<u32>,
    button: i32,
    pressed: bool,
) -> Result<()> {
    use reis::ei;

    let device = pointer_device
        .lock()
        .await
        .clone()
        .ok_or_else(|| anyhow!("EIS pointer not ready"))?;

    let devs = devices.lock().await;
    let data = devs
        .get(&device)
        .ok_or_else(|| anyhow!("Device data missing"))?;
    let btn = data
        .interface::<ei::Button>()
        .ok_or_else(|| anyhow!("Button interface not found"))?;
    drop(devs);

    btn.button(
        button as u32,
        if pressed {
            ei::button::ButtonState::Press
        } else {
            ei::button::ButtonState::Released
        },
    );

    let serial = *last_serial.lock().await;
    device.frame(serial, current_time_us());
    context.flush()?;

    Ok(())
}

#[cfg(feature = "libei")]
async fn eis_pointer_axis(
    context: &reis::ei::Context,
    pointer_device: &tokio::sync::Mutex<Option<reis::ei::Device>>,
    devices: &tokio::sync::Mutex<std::collections::HashMap<reis::ei::Device, DeviceData>>,
    last_serial: &tokio::sync::Mutex<u32>,
    dx: f64,
    dy: f64,
) -> Result<()> {
    use reis::ei;

    let device = pointer_device
        .lock()
        .await
        .clone()
        .ok_or_else(|| anyhow!("EIS pointer not ready"))?;

    let devs = devices.lock().await;
    let data = devs
        .get(&device)
        .ok_or_else(|| anyhow!("Device data missing"))?;
    let scroll = data
        .interface::<ei::Scroll>()
        .ok_or_else(|| anyhow!("Scroll interface not found"))?;
    drop(devs);

    if dx.abs() > 0.01 {
        scroll.scroll(dx as f32, 0.0);
    }
    if dy.abs() > 0.01 {
        scroll.scroll(0.0, dy as f32);
    }

    let serial = *last_serial.lock().await;
    device.frame(serial, current_time_us());
    context.flush()?;

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
