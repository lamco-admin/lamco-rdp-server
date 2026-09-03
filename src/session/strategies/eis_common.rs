//! Shared EIS (Emulated Input Server) utilities
//!
//! Common types and functions used by both `mutter_direct` and `libei` strategies
//! for device tracking, input event injection, and EIS protocol helpers.

use std::{collections::HashMap, fmt};

use anyhow::{Result, anyhow};
use reis::ei;
use tokio::sync::{Mutex, RwLock};

/// A specific EIS device (keyboard, pointer, pointer_absolute, ...) has not yet
/// been registered by the compositor's device-setup burst.
///
/// Distinct from a genuinely dead EIS connection: the other device types on
/// this same session may already be usable, and the missing one typically
/// arrives within milliseconds to a couple of seconds as the burst continues.
/// Callers should wait briefly for it rather than tearing down the whole
/// session (which would also discard every already-ready device and restart
/// the readiness race for all of them).
#[derive(Debug)]
pub struct DeviceNotReady(pub String);

impl fmt::Display for DeviceNotReady {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "EIS {} not ready", self.0)
    }
}

impl std::error::Error for DeviceNotReady {}

/// Region defining the coordinate space for an absolute pointer device.
/// Provided by the compositor via EIS Device::Region events.
#[derive(Debug, Clone)]
pub struct DeviceRegion {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

/// Tracked data for an EIS device (keyboard, pointer, touchscreen, etc.)
#[derive(Default)]
pub struct DeviceData {
    pub name: Option<String>,
    pub device_type: Option<ei::device::DeviceType>,
    pub interfaces: HashMap<String, reis::Object>,
    pub seat: Option<ei::Seat>,
    /// Regions for absolute pointer devices (defines coordinate space)
    pub regions: Vec<DeviceRegion>,
}

impl DeviceData {
    /// Downcast a stored interface object to its typed form.
    pub fn interface<T: reis::Interface>(&self) -> Option<T> {
        self.interfaces.get(T::NAME)?.clone().downcast()
    }
}

/// Tracked devices for an EIS session, separated by role.
pub struct EisDevices {
    pub all: Mutex<HashMap<ei::Device, DeviceData>>,
    pub keyboard: Mutex<Option<ei::Device>>,
    /// Device with ei_pointer (relative motion)
    pub pointer: Mutex<Option<ei::Device>>,
    /// Device with ei_pointer_absolute (absolute coordinates).
    /// KDE creates separate devices for relative and absolute pointers.
    pub pointer_absolute: Mutex<Option<ei::Device>>,
    pub touch: Mutex<Option<ei::Device>>,
    /// Device with ei_text (Unicode / keysym injection, libei 1.6+ / reis 0.7).
    /// Absent when the compositor does not advertise the text capability.
    pub text: Mutex<Option<ei::Device>>,
    /// Negotiated `ei_device` interface version, captured at handshake. v3+
    /// requires the client to call `device.ready()` after `device.done` before
    /// the server emits `resumed`; v2 (libei < 1.6) has no `ready()` and must
    /// not receive one.
    pub device_version: std::sync::atomic::AtomicU32,
    pub last_serial: Mutex<u32>,
}

impl EisDevices {
    pub fn new(initial_serial: u32) -> Self {
        Self {
            all: Mutex::new(HashMap::new()),
            keyboard: Mutex::new(None),
            pointer: Mutex::new(None),
            pointer_absolute: Mutex::new(None),
            touch: Mutex::new(None),
            text: Mutex::new(None),
            device_version: std::sync::atomic::AtomicU32::new(0),
            last_serial: Mutex::new(initial_serial),
        }
    }

    /// Clear all device state for EIS session recovery.
    pub async fn clear(&self) {
        self.all.lock().await.clear();
        *self.keyboard.lock().await = None;
        *self.pointer.lock().await = None;
        *self.pointer_absolute.lock().await = None;
        *self.touch.lock().await = None;
        *self.text.lock().await = None;
    }
}

/// Current CLOCK_MONOTONIC time in microseconds.
///
/// libei `frame()` timestamps are specified as CLOCK_MONOTONIC microseconds;
/// CLOCK_REALTIME (`SystemTime`/`UNIX_EPOCH`) is the wrong clock and can jump
/// backwards on wall-clock adjustment, corrupting event ordering.
pub fn current_time_us() -> u64 {
    use nix::time::{ClockId, clock_gettime};
    clock_gettime(ClockId::CLOCK_MONOTONIC).map_or(0, |ts| {
        ts.tv_sec() as u64 * 1_000_000 + ts.tv_nsec() as u64 / 1_000
    })
}

/// Locate a device's interface and stage one event on it via the closure,
/// without committing a `frame()` or flushing.
///
/// Split out so [`commit_device_frame`] can be called once after several
/// `stage_device_event` calls targeting the same device, batching a
/// coordinated group of events (e.g. relative motion immediately followed
/// by a button press) into one atomic EIS frame instead of one frame per
/// event. Committing each individually lets the compositor process them as
/// separate hardware events, opening a race where the button can be
/// observed at the pre-motion pointer position.
async fn stage_device_event<T: reis::Interface>(
    device_lock: &Mutex<Option<ei::Device>>,
    devices: &EisDevices,
    device_name: &str,
    f: impl FnOnce(&T),
) -> Result<()> {
    let device = device_lock
        .lock()
        .await
        .clone()
        .ok_or_else(|| anyhow::Error::new(DeviceNotReady(device_name.to_string())))?;

    let devs = devices.all.lock().await;
    let data = devs
        .get(&device)
        .ok_or_else(|| anyhow!("Device data missing for {device_name}"))?;
    let iface = data
        .interface::<T>()
        .ok_or_else(|| anyhow!("{} interface not found", std::any::type_name::<T>()))?;
    drop(devs);

    f(&iface);
    Ok(())
}

/// Commit a device's staged events with one `frame()` and flush the context.
///
/// Call once after one or more [`stage_device_event`] calls on the same
/// device. `device_lock`/`device_name` identify the device the same way as
/// `stage_device_event` so callers pass the same pair to both.
async fn commit_device_frame(
    context: &RwLock<Option<ei::Context>>,
    device_lock: &Mutex<Option<ei::Device>>,
    devices: &EisDevices,
    device_name: &str,
) -> Result<()> {
    let device = device_lock
        .lock()
        .await
        .clone()
        .ok_or_else(|| anyhow::Error::new(DeviceNotReady(device_name.to_string())))?;

    let serial = *devices.last_serial.lock().await;
    device.frame(serial, current_time_us());
    let ctx = context.read().await;
    let ctx_ref = ctx
        .as_ref()
        .ok_or_else(|| anyhow!("EIS context not initialized"))?;
    ctx_ref.flush()?;
    Ok(())
}

/// Stage one event and immediately commit its own frame -- the original
/// one-event-one-frame behavior, for callers that don't need to batch
/// multiple events together.
///
/// Accepts `RwLock<Option<ei::Context>>` for the libei strategy (deferred EIS
/// activation) and `RwLock<ei::Context>` for mutter_direct (always active).
/// Both work because the mutter_direct context is wrapped in Option at call
/// sites via `.as_ref()`.
async fn with_device_interface<T: reis::Interface>(
    context: &RwLock<Option<ei::Context>>,
    device_lock: &Mutex<Option<ei::Device>>,
    devices: &EisDevices,
    device_name: &str,
    f: impl FnOnce(&T),
) -> Result<()> {
    stage_device_event(device_lock, devices, device_name, f).await?;
    commit_device_frame(context, device_lock, devices, device_name).await
}

// === Keyboard ===

pub async fn eis_keyboard_keycode(
    context: &RwLock<Option<ei::Context>>,
    devices: &EisDevices,
    keycode: i32,
    pressed: bool,
) -> Result<()> {
    // EIS Keyboard::key() takes Linux evdev keycodes directly.
    // The input handler already converts RDP scancodes to evdev via lamco-rdp-input.
    let eis_keycode = keycode as u32;
    let state = if pressed {
        ei::keyboard::KeyState::Press
    } else {
        ei::keyboard::KeyState::Released
    };

    with_device_interface::<ei::Keyboard>(context, &devices.keyboard, devices, "keyboard", |kbd| {
        kbd.key(eis_keycode, state);
    })
    .await
}

/// Inject a keyboard event by XKB keysym via the ei_text interface.
///
/// This is the path for Unicode input (CJK, emoji, accented / off-layout
/// characters) that has no direct evdev keycode. It requires the compositor to
/// advertise the `ei_text` capability (libei 1.6+). When no text device is
/// present the call is a no-op, so Unicode input degrades cleanly on older
/// compositors rather than erroring — the caller keeps the keycode path for
/// everything that maps to evdev.
pub async fn eis_text_keysym(
    context: &RwLock<Option<ei::Context>>,
    devices: &EisDevices,
    keysym: u32,
    pressed: bool,
) -> Result<()> {
    if devices.text.lock().await.is_none() {
        return Ok(());
    }

    let state = if pressed {
        ei::keyboard::KeyState::Press
    } else {
        ei::keyboard::KeyState::Released
    };

    with_device_interface::<ei::Text>(context, &devices.text, devices, "text", |text| {
        text.keysym(keysym, state);
    })
    .await
}

// === Pointer (absolute) ===

pub async fn eis_pointer_motion_absolute(
    context: &RwLock<Option<ei::Context>>,
    devices: &EisDevices,
    x: f64,
    y: f64,
    stream_offset: Option<(f64, f64)>,
) -> Result<()> {
    // Incoming (x, y) are relative to the captured stream. Map them into the
    // EIS absolute (compositor-layout) space by adding the captured stream's
    // layout position — the authoritative source the caller resolves from the
    // ScreenCast stream. Fall back to the EIS device-region heuristic (the prior
    // KDE-tested path: pick the region with a non-zero offset) only when the
    // stream position is unknown, so no previously-working case regresses.
    let (offset_x, offset_y) = match stream_offset {
        Some(offset) => offset,
        None => {
            let abs_device = devices.pointer_absolute.lock().await;
            if let Some(ref dev) = *abs_device {
                let devs = devices.all.lock().await;
                if let Some(data) = devs.get(dev) {
                    data.regions
                        .iter()
                        .find(|r| r.x > 0 || r.y > 0)
                        .map_or((0.0, 0.0), |r| (r.x as f64, r.y as f64))
                } else {
                    (0.0, 0.0)
                }
            } else {
                (0.0, 0.0)
            }
        }
    };

    let abs_x = x + offset_x;
    let abs_y = y + offset_y;

    with_device_interface::<ei::PointerAbsolute>(
        context,
        &devices.pointer_absolute,
        devices,
        "pointer_absolute",
        |ptr| {
            ptr.motion_absolute(abs_x as f32, abs_y as f32);
        },
    )
    .await
}

// === Pointer (relative) ===

pub async fn eis_pointer_motion_relative(
    context: &RwLock<Option<ei::Context>>,
    devices: &EisDevices,
    dx: f64,
    dy: f64,
) -> Result<()> {
    with_device_interface::<ei::Pointer>(context, &devices.pointer, devices, "pointer", |ptr| {
        ptr.motion_relative(dx as f32, dy as f32);
    })
    .await
}

/// Stage a relative-motion sample on the pointer device without committing a
/// frame. Pair with other `stage_pointer_*` calls and a single
/// [`commit_pointer_frame`] to batch a coordinated group of pointer events
/// (e.g. a drag: motion immediately followed by a button press) into one
/// atomic EIS frame.
pub async fn stage_pointer_motion_relative(devices: &EisDevices, dx: f64, dy: f64) -> Result<()> {
    stage_device_event::<ei::Pointer>(&devices.pointer, devices, "pointer", |ptr| {
        ptr.motion_relative(dx as f32, dy as f32);
    })
    .await
}

// === Button ===

pub async fn eis_pointer_button(
    context: &RwLock<Option<ei::Context>>,
    devices: &EisDevices,
    button: i32,
    pressed: bool,
) -> Result<()> {
    with_device_interface::<ei::Button>(context, &devices.pointer, devices, "pointer", |btn| {
        btn.button(
            button as u32,
            if pressed {
                ei::button::ButtonState::Press
            } else {
                ei::button::ButtonState::Released
            },
        );
    })
    .await
}

/// Stage a button press/release on the pointer device without committing a
/// frame. See [`stage_pointer_motion_relative`] for why this exists.
pub async fn stage_pointer_button(devices: &EisDevices, button: i32, pressed: bool) -> Result<()> {
    stage_device_event::<ei::Button>(&devices.pointer, devices, "pointer", |btn| {
        btn.button(
            button as u32,
            if pressed {
                ei::button::ButtonState::Press
            } else {
                ei::button::ButtonState::Released
            },
        );
    })
    .await
}

// === Scroll ===

pub async fn eis_pointer_axis(
    context: &RwLock<Option<ei::Context>>,
    devices: &EisDevices,
    dx: f64,
    dy: f64,
) -> Result<()> {
    with_device_interface::<ei::Scroll>(context, &devices.pointer, devices, "pointer", |scroll| {
        if dx.abs() > 0.01 {
            scroll.scroll(dx as f32, 0.0);
        }
        if dy.abs() > 0.01 {
            scroll.scroll(0.0, dy as f32);
        }
    })
    .await
}

/// Stage a continuous-scroll sample on the pointer device without
/// committing a frame. See [`stage_pointer_motion_relative`] for why this
/// exists.
pub async fn stage_pointer_axis(devices: &EisDevices, dx: f64, dy: f64) -> Result<()> {
    stage_device_event::<ei::Scroll>(&devices.pointer, devices, "pointer", |scroll| {
        if dx.abs() > 0.01 {
            scroll.scroll(dx as f32, 0.0);
        }
        if dy.abs() > 0.01 {
            scroll.scroll(0.0, dy as f32);
        }
    })
    .await
}

/// Discrete (notch-based) scroll. `dx`/`dy` are in 120-units per detent — the
/// same convention as the RDP wheel and libei's `ei_scroll.scroll_discrete`, so
/// one RDP notch becomes exactly one detent instead of a smoothed pixel delta.
pub async fn eis_pointer_axis_discrete(
    context: &RwLock<Option<ei::Context>>,
    devices: &EisDevices,
    dx: i32,
    dy: i32,
) -> Result<()> {
    with_device_interface::<ei::Scroll>(context, &devices.pointer, devices, "pointer", |scroll| {
        // No paired scroll_stop here: per ei_scroll's protocol doc, sending
        // scroll_stop for an axis that had a nonzero value in scroll_discrete
        // within the same frame is a client bug the EIS implementation may
        // silently drop or disconnect over. A discrete notch is already a
        // complete, self-terminating event, it doesn't need one.
        scroll.scroll_discrete(dx, dy);
    })
    .await
}

/// Stage a discrete-scroll notch on the pointer device without committing a
/// frame. See [`stage_pointer_motion_relative`] for why this exists.
pub async fn stage_pointer_axis_discrete(devices: &EisDevices, dx: i32, dy: i32) -> Result<()> {
    stage_device_event::<ei::Scroll>(&devices.pointer, devices, "pointer", |scroll| {
        scroll.scroll_discrete(dx, dy);
    })
    .await
}

/// Commit every pointer-device event staged via `stage_pointer_*` since the
/// last commit, as one atomic EIS frame. Call once per coalesced batch of
/// pointer-device events (relative motion, button, scroll); absolute motion
/// is a separate EIS device (see `eis_pointer_motion_absolute`) and is not
/// covered by this commit.
pub async fn commit_pointer_frame(
    context: &RwLock<Option<ei::Context>>,
    devices: &EisDevices,
) -> Result<()> {
    commit_device_frame(context, &devices.pointer, devices, "pointer").await
}

// === Touch ===

pub async fn eis_touch_down(
    context: &RwLock<Option<ei::Context>>,
    devices: &EisDevices,
    slot: u32,
    x: f64,
    y: f64,
) -> Result<()> {
    with_device_interface::<ei::Touchscreen>(
        context,
        &devices.touch,
        devices,
        "touchscreen",
        |ts| {
            ts.down(slot, x as f32, y as f32);
        },
    )
    .await
}

pub async fn eis_touch_motion(
    context: &RwLock<Option<ei::Context>>,
    devices: &EisDevices,
    slot: u32,
    x: f64,
    y: f64,
) -> Result<()> {
    with_device_interface::<ei::Touchscreen>(
        context,
        &devices.touch,
        devices,
        "touchscreen",
        |ts| {
            ts.motion(slot, x as f32, y as f32);
        },
    )
    .await
}

pub async fn eis_touch_up(
    context: &RwLock<Option<ei::Context>>,
    devices: &EisDevices,
    slot: u32,
) -> Result<()> {
    with_device_interface::<ei::Touchscreen>(
        context,
        &devices.touch,
        devices,
        "touchscreen",
        |ts| {
            ts.up(slot);
        },
    )
    .await
}

/// Call `device.ready()` when the negotiated `ei_device` is v3+.
///
/// Per the EI protocol, a v3 client MUST send `ei_device.ready()` after
/// `device.done`; the server withholds `resumed` (hence `start_emulating`,
/// hence all input) until it does. v2 devices have no `ready()` request and
/// must not be sent one. Returns `true` if `ready()` was issued (the caller
/// must then flush the EIS context). Call this from the `device.done` handler.
pub fn eis_device_ready(devices: &EisDevices, device: &ei::Device) -> bool {
    use std::sync::atomic::Ordering;
    if devices.device_version.load(Ordering::Relaxed) < 3 {
        return false;
    }
    device.ready();
    true
}

/// Process a device Done event and assign tracked device roles.
///
/// Call this during device discovery when `ei::device::Event::Done` fires.
/// Returns `true` if the device was a virtual device (sender) and was assigned.
pub async fn assign_device_roles(
    device: &ei::Device,
    data: &DeviceData,
    devices: &EisDevices,
) -> bool {
    if !matches!(data.device_type, Some(ei::device::DeviceType::Virtual)) {
        return false;
    }

    if data.interface::<ei::Keyboard>().is_some() {
        *devices.keyboard.lock().await = Some(device.clone());
        tracing::info!("[eis] Keyboard device ready");
    }

    if data.interface::<ei::PointerAbsolute>().is_some() {
        *devices.pointer_absolute.lock().await = Some(device.clone());
        tracing::info!("[eis] Pointer absolute device ready");
    }
    if data.interface::<ei::Pointer>().is_some() {
        *devices.pointer.lock().await = Some(device.clone());
        tracing::info!("[eis] Pointer relative device ready");
    }

    if data.interface::<ei::Touchscreen>().is_some() {
        *devices.touch.lock().await = Some(device.clone());
        tracing::info!("[eis] Touchscreen device ready");
    }

    if data.interface::<ei::Text>().is_some() {
        *devices.text.lock().await = Some(device.clone());
        tracing::info!("[eis] Text device ready (Unicode keysym injection)");
    }

    true
}
