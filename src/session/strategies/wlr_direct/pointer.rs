//! Virtual Pointer Implementation for wlr-virtual-pointer Protocol
//!
//! This module provides a clean wrapper around `zwlr_virtual_pointer_v1` protocol,
//! handling mouse input injection for wlroots-based compositors.
//!
//! # Protocol Details
//!
//! The wlr-virtual-pointer protocol allows creating virtual pointer devices that can:
//! - Move the cursor with absolute or relative coordinates
//! - Inject button press/release events (mouse clicks)
//! - Inject scroll events (wheel, touchpad gestures)
//!
//! Events are grouped with `frame()` calls to indicate logical groupings.
//!
//! # Coordinate System
//!
//! `motion_absolute()` uses a coordinate space defined by `x_extent` and `y_extent`:
//! - Coordinates are in the range [0, extent]
//! - Typically extent = screen/output dimensions
//! - For multi-monitor: use per-stream extents from StreamInfo
//!
//! # Button Codes
//!
//! Button codes follow Linux evdev standards:
//! - 272 (BTN_LEFT) - Left click
//! - 273 (BTN_RIGHT) - Right click
//! - 274 (BTN_MIDDLE) - Middle click
//! - 275 (BTN_SIDE) - Side button (back)
//! - 276 (BTN_EXTRA) - Extra button (forward)

use anyhow::Result;
use tracing::debug;
use wayland_client::{protocol::wl_seat::WlSeat, QueueHandle};
use wayland_protocols_wlr::virtual_pointer::v1::client::{
    zwlr_virtual_pointer_manager_v1::ZwlrVirtualPointerManagerV1,
    zwlr_virtual_pointer_v1::ZwlrVirtualPointerV1,
};

/// Virtual pointer wrapper for wlr-virtual-pointer-v1 protocol
///
/// Wraps the Wayland protocol object and provides a clean Rust API for
/// pointer input injection.
///
/// # Lifecycle
///
/// The virtual pointer is created once during session initialization and
/// reused for all pointer events. It is automatically destroyed when dropped.
pub struct VirtualPointer {
    pointer: ZwlrVirtualPointerV1,
}

impl VirtualPointer {
    pub fn new<State>(
        manager: &ZwlrVirtualPointerManagerV1,
        seat: &WlSeat,
        qh: &QueueHandle<State>,
    ) -> Result<Self>
    where
        State: wayland_client::Dispatch<ZwlrVirtualPointerV1, ()> + 'static,
    {
        let pointer = manager.create_virtual_pointer(Some(seat), qh, ());

        debug!("✅ wlr_direct: Virtual pointer created");

        Ok(Self { pointer })
    }

    /// Send absolute pointer motion event
    ///
    /// The compositor maps [0, extent] to actual output dimensions.
    pub fn motion_absolute(&self, time: u32, x: u32, y: u32, x_extent: u32, y_extent: u32) {
        debug!(
            "[wlr_direct] Pointer motion: x={}, y={}, extent={}x{}",
            x, y, x_extent, y_extent
        );

        self.pointer.motion_absolute(time, x, y, x_extent, y_extent);
    }

    pub fn button(&self, time: u32, button: u32, state: ButtonState) {
        debug!(
            "[wlr_direct] Pointer button: button={}, state={:?}",
            button, state
        );

        let wl_state = match state {
            ButtonState::Released => wayland_client::protocol::wl_pointer::ButtonState::Released,
            ButtonState::Pressed => wayland_client::protocol::wl_pointer::ButtonState::Pressed,
        };

        self.pointer.button(time, button, wl_state);
    }

    pub fn axis(&self, time: u32, axis: Axis, value: f64) {
        debug!(
            "[wlr_direct] Pointer axis: axis={:?}, value={}",
            axis, value
        );

        let wl_axis = match axis {
            Axis::VerticalScroll => wayland_client::protocol::wl_pointer::Axis::VerticalScroll,
            Axis::HorizontalScroll => wayland_client::protocol::wl_pointer::Axis::HorizontalScroll,
        };

        // Wayland axis values use wl_fixed_t (24.8 fixed-point)
        // The wayland-client crate handles the conversion
        self.pointer.axis(time, wl_axis, value);
    }

    /// Should be called before axis() events to provide context to the compositor.
    pub fn axis_source(&self, source: AxisSource) {
        let wl_source = match source {
            AxisSource::Wheel => wayland_client::protocol::wl_pointer::AxisSource::Wheel,
            AxisSource::Finger => wayland_client::protocol::wl_pointer::AxisSource::Finger,
            AxisSource::Continuous => wayland_client::protocol::wl_pointer::AxisSource::Continuous,
            AxisSource::WheelTilt => wayland_client::protocol::wl_pointer::AxisSource::WheelTilt,
        };

        self.pointer.axis_source(wl_source);
    }

    /// End of pointer event group
    ///
    /// Indicates that a logical group of pointer events is complete.
    ///
    /// # Protocol Details
    ///
    /// The frame() call tells the compositor to apply all pending events atomically.
    /// This should be called after every logical input action:
    /// - After motion_absolute() for a move
    /// - After button() for a click
    /// - After axis() for a scroll
    ///
    /// # Example
    ///
    /// ```ignore
    /// pointer.motion_absolute(time, x, y, width, height);
    /// pointer.frame();  // Apply motion
    ///
    /// pointer.button(time, 272, ButtonState::Pressed);
    /// pointer.frame();  // Apply button press
    /// ```
    pub fn frame(&self) {
        self.pointer.frame();
    }

    pub fn inner(&self) -> &ZwlrVirtualPointerV1 {
        &self.pointer
    }
}

impl Drop for VirtualPointer {
    fn drop(&mut self) {
        debug!("🔌 wlr_direct: Virtual pointer destroyed");
        self.pointer.destroy();
    }
}

/// Button state for pointer events
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ButtonState {
    Released,
    Pressed,
}

impl From<bool> for ButtonState {
    fn from(pressed: bool) -> Self {
        if pressed {
            ButtonState::Pressed
        } else {
            ButtonState::Released
        }
    }
}

/// Pointer axis type
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Axis {
    VerticalScroll,
    HorizontalScroll,
}

/// Axis source type
///
/// Indicates how the scroll event was generated. This helps the compositor
/// apply appropriate acceleration curves and gesture detection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AxisSource {
    Wheel,
    Finger,
    /// Wheel without detents
    Continuous,
    /// Horizontal scroll from tilting the wheel
    WheelTilt,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_button_state_from_bool() {
        assert_eq!(ButtonState::from(true), ButtonState::Pressed);
        assert_eq!(ButtonState::from(false), ButtonState::Released);
    }

    #[test]
    fn test_axis_types() {
        // Ensure axis types are distinct
        assert_ne!(Axis::VerticalScroll, Axis::HorizontalScroll);
    }

    #[test]
    fn test_axis_source_types() {
        // Verify all axis source variants exist
        let _wheel = AxisSource::Wheel;
        let _finger = AxisSource::Finger;
        let _continuous = AxisSource::Continuous;
        let _tilt = AxisSource::WheelTilt;
    }
}
