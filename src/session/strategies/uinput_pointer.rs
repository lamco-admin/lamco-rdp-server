//! Last-resort kernel pointer injection via `/dev/uinput`.
//!
//! COSMIC (and other Smithay compositors) expose `zwp_virtual_keyboard_v1` but
//! not `zwlr_virtual_pointer_v1`, and their RemoteDesktop/EIS portal has not yet
//! shipped (cosmic-comp #2442 + xdg-desktop-portal-cosmic #317). Until that
//! lands — the proper, portal-mediated path — the server injects the pointer at
//! the kernel evdev layer as a trusted native process.
//!
//! This is a deliberate, application-owned escape hatch: it bypasses the Wayland
//! permission model and needs write access to `/dev/uinput` (an `input`-group /
//! udev grant). It lives in the server, not in the portal crate, precisely
//! because that trust decision belongs to the application the user installed —
//! a portal exists to *mediate* input, not to ship a bypass.

use anyhow::{Context, Result};
use evdev::{
    AbsInfo, AbsoluteAxisCode, AttributeSet, BusType, InputEvent, InputId, RelativeAxisCode,
    UinputAbsSetup, uinput::VirtualDevice,
};

/// Absolute-axis range of the virtual device. Not tied to any real display
/// resolution -- it's the standard evdev tablet-axis range (`0..=32767`, the
/// same span `ABS_X`/`ABS_Y` use on real absolute pointing hardware such as
/// graphics tablets and touchscreens). `motion_absolute` maps the caller's
/// normalized `[0,1]` position onto this range; the compositor rescales it
/// to actual output pixels.
const ABS_MAX: i32 = 32767;
const EV_SYN: u16 = 0x00;
const EV_KEY: u16 = 0x01;
const EV_REL: u16 = 0x02;
const EV_ABS: u16 = 0x03;

/// Why `/dev/uinput` isn't usable, distinguishing the two failure classes a
/// caller can actually act on. `None` means it's fine to try opening it.
fn unavailable_reason() -> Option<&'static str> {
    let path = std::path::Path::new("/dev/uinput");
    if !path.exists() {
        return Some("does not exist (uinput kernel module not loaded)");
    }
    if std::fs::OpenOptions::new().write(true).open(path).is_err() {
        return Some("exists but is not writable (add this user to the input group)");
    }
    None
}

/// A virtual absolute pointing device backed by `/dev/uinput`.
pub struct UinputPointer {
    device: VirtualDevice,
}

impl UinputPointer {
    /// Create the virtual pointer. Errors if `/dev/uinput` can't be opened so the
    /// caller can fall back to a pointer-less (keyboard-only) session.
    pub fn new() -> Result<Self> {
        if let Some(reason) = unavailable_reason() {
            anyhow::bail!("open /dev/uinput for lamco-rdp-pointer: {reason}");
        }

        let abs_x = UinputAbsSetup::new(
            AbsoluteAxisCode::ABS_X,
            AbsInfo::new(0, 0, ABS_MAX, 0, 0, 1),
        );
        let abs_y = UinputAbsSetup::new(
            AbsoluteAxisCode::ABS_Y,
            AbsInfo::new(0, 0, ABS_MAX, 0, 0, 1),
        );

        let mut keys = AttributeSet::new();
        keys.insert(evdev::KeyCode::BTN_LEFT);
        keys.insert(evdev::KeyCode::BTN_RIGHT);
        keys.insert(evdev::KeyCode::BTN_MIDDLE);

        let mut rel = AttributeSet::new();
        rel.insert(RelativeAxisCode::REL_WHEEL);
        rel.insert(RelativeAxisCode::REL_HWHEEL);

        let device = VirtualDevice::builder()
            .context("open /dev/uinput for lamco-rdp-pointer")?
            .name("lamco-rdp-pointer")
            .input_id(InputId::new(BusType::BUS_USB, 0x4C41, 0x4D43, 1))
            .with_absolute_axis(&abs_x)
            .context("set ABS_X")?
            .with_absolute_axis(&abs_y)
            .context("set ABS_Y")?
            .with_keys(&keys)
            .context("set buttons")?
            .with_relative_axes(&rel)
            .context("set scroll axes")?
            .build()
            .context("build uinput device")?;

        tracing::info!("uinput pointer device created (/dev/uinput, ABS 0-{ABS_MAX})");
        Ok(Self { device })
    }

    /// Move to a position normalized to `[0,1]` within the source frame. Callers
    /// must divide pixel coordinates by the stream extent before calling this
    /// (the device maps `[0,1]` onto its full ABS range).
    pub fn motion_absolute(&mut self, x_norm: f64, y_norm: f64) -> Result<()> {
        let ax = (x_norm.clamp(0.0, 1.0) * f64::from(ABS_MAX)) as i32;
        let ay = (y_norm.clamp(0.0, 1.0) * f64::from(ABS_MAX)) as i32;
        self.device
            .emit(&[
                InputEvent::new_now(EV_ABS, AbsoluteAxisCode::ABS_X.0, ax),
                InputEvent::new_now(EV_ABS, AbsoluteAxisCode::ABS_Y.0, ay),
                InputEvent::new_now(EV_SYN, 0, 0),
            ])
            .context("uinput motion")
    }

    /// Press or release a button identified by its Linux `BTN_*` code.
    pub fn button(&mut self, code: u32, pressed: bool) -> Result<()> {
        self.device
            .emit(&[
                InputEvent::new_now(EV_KEY, code as u16, i32::from(pressed)),
                InputEvent::new_now(EV_SYN, 0, 0),
            ])
            .context("uinput button")
    }

    /// Scroll by one wheel detent per non-zero axis (sign = direction).
    pub fn scroll(&mut self, dx: f64, dy: f64) -> Result<()> {
        let mut events = Vec::new();
        if dy.abs() > f64::EPSILON {
            let v = if dy > 0.0 { 1 } else { -1 };
            events.push(InputEvent::new_now(
                EV_REL,
                RelativeAxisCode::REL_WHEEL.0,
                v,
            ));
        }
        if dx.abs() > f64::EPSILON {
            let v = if dx > 0.0 { 1 } else { -1 };
            events.push(InputEvent::new_now(
                EV_REL,
                RelativeAxisCode::REL_HWHEEL.0,
                v,
            ));
        }
        if !events.is_empty() {
            events.push(InputEvent::new_now(EV_SYN, 0, 0));
            self.device.emit(&events).context("uinput scroll")?;
        }
        Ok(())
    }
}
