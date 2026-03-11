//! Shared EIS (Emulated Input Server) utilities
//!
//! Common types and functions used by both `mutter_direct` and `libei` strategies
//! for device tracking, input event injection, and EIS protocol helpers.

use std::collections::HashMap;

use anyhow::{Result, anyhow};
use reis::ei;
use tokio::sync::Mutex;

/// Tracked data for an EIS device (keyboard, pointer, touchscreen, etc.)
#[derive(Default)]
pub struct DeviceData {
    pub name: Option<String>,
    pub device_type: Option<ei::device::DeviceType>,
    pub interfaces: HashMap<String, reis::Object>,
    pub seat: Option<ei::Seat>,
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
    pub pointer: Mutex<Option<ei::Device>>,
    pub touch: Mutex<Option<ei::Device>>,
    pub last_serial: Mutex<u32>,
}

impl EisDevices {
    pub fn new(initial_serial: u32) -> Self {
        Self {
            all: Mutex::new(HashMap::new()),
            keyboard: Mutex::new(None),
            pointer: Mutex::new(None),
            touch: Mutex::new(None),
            last_serial: Mutex::new(initial_serial),
        }
    }
}

/// Current time in microseconds (for EIS frame timestamps).
pub fn current_time_us() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros() as u64
}

/// Helper: get device + interface + serial, flush frame after closure.
async fn with_device_interface<T: reis::Interface>(
    context: &ei::Context,
    device_lock: &Mutex<Option<ei::Device>>,
    devices: &EisDevices,
    device_name: &str,
    f: impl FnOnce(&T),
) -> Result<()> {
    let device = device_lock
        .lock()
        .await
        .clone()
        .ok_or_else(|| anyhow!("EIS {device_name} not ready"))?;

    let devs = devices.all.lock().await;
    let data = devs
        .get(&device)
        .ok_or_else(|| anyhow!("Device data missing for {device_name}"))?;
    let iface = data
        .interface::<T>()
        .ok_or_else(|| anyhow!("{} interface not found", std::any::type_name::<T>()))?;
    drop(devs);

    f(&iface);

    let serial = *devices.last_serial.lock().await;
    device.frame(serial, current_time_us());
    context.flush()?;
    Ok(())
}

// === Keyboard ===

pub async fn eis_keyboard_keycode(
    context: &ei::Context,
    devices: &EisDevices,
    keycode: i32,
    pressed: bool,
) -> Result<()> {
    // EIS keycodes offset by 8 from evdev
    let eis_keycode = (keycode - 8) as u32;
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

// === Pointer (absolute) ===

pub async fn eis_pointer_motion_absolute(
    context: &ei::Context,
    devices: &EisDevices,
    x: f64,
    y: f64,
) -> Result<()> {
    with_device_interface::<ei::PointerAbsolute>(
        context,
        &devices.pointer,
        devices,
        "pointer",
        |ptr| {
            ptr.motion_absolute(x as f32, y as f32);
        },
    )
    .await
}

// === Pointer (relative) ===

pub async fn eis_pointer_motion_relative(
    context: &ei::Context,
    devices: &EisDevices,
    dx: f64,
    dy: f64,
) -> Result<()> {
    with_device_interface::<ei::Pointer>(context, &devices.pointer, devices, "pointer", |ptr| {
        ptr.motion_relative(dx as f32, dy as f32);
    })
    .await
}

// === Button ===

pub async fn eis_pointer_button(
    context: &ei::Context,
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

// === Scroll ===

pub async fn eis_pointer_axis(
    context: &ei::Context,
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

// === Touch ===

pub async fn eis_touch_down(
    context: &ei::Context,
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
    context: &ei::Context,
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

pub async fn eis_touch_up(context: &ei::Context, devices: &EisDevices, slot: u32) -> Result<()> {
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

    if data.interface::<ei::Pointer>().is_some()
        || data.interface::<ei::PointerAbsolute>().is_some()
    {
        *devices.pointer.lock().await = Some(device.clone());
        tracing::info!("[eis] Pointer device ready");
    }

    if data.interface::<ei::Touchscreen>().is_some() {
        *devices.touch.lock().await = Some(device.clone());
        tracing::info!("[eis] Touchscreen device ready");
    }

    true
}
