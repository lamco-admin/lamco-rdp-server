//! Mutter RemoteDesktop D-Bus Interface
//!
//! Proxies for org.gnome.Mutter.RemoteDesktop D-Bus interfaces.
//! Used for input injection, clipboard, and EIS access without portal permissions.

use std::collections::HashMap;

use anyhow::{Context, Result};
use zbus::{
    zvariant::{self, ObjectPath, OwnedObjectPath, Value},
    Connection,
};

/// Main RemoteDesktop interface proxy
///
/// Service: org.gnome.Mutter.RemoteDesktop
/// Path: /org/gnome/Mutter/RemoteDesktop
#[derive(Debug)]
pub struct MutterRemoteDesktop<'a> {
    #[expect(dead_code, reason = "kept alive for D-Bus proxy lifetime")]
    connection: Connection,
    proxy: zbus::Proxy<'a>,
}

impl MutterRemoteDesktop<'_> {
    pub async fn new(connection: &Connection) -> Result<Self> {
        let proxy = zbus::ProxyBuilder::new(connection)
            .interface("org.gnome.Mutter.RemoteDesktop")?
            .path("/org/gnome/Mutter/RemoteDesktop")?
            .destination("org.gnome.Mutter.RemoteDesktop")?
            .build()
            .await
            .context("Failed to create Mutter RemoteDesktop proxy")?;

        Ok(Self {
            connection: connection.clone(),
            proxy,
        })
    }

    /// Create a new remote desktop session
    pub async fn create_session(&self) -> Result<OwnedObjectPath> {
        let response = self
            .proxy
            .call_method("CreateSession", &())
            .await
            .context("Failed to call CreateSession")?;

        let body = response.body();
        let path: OwnedObjectPath = body
            .deserialize()
            .context("Failed to deserialize CreateSession response")?;

        Ok(path)
    }

    /// API version number
    pub async fn version(&self) -> Result<i32> {
        self.proxy
            .get_property("Version")
            .await
            .context("Failed to read RemoteDesktop Version property")
    }

    /// Bitmask of supported input device types
    pub async fn supported_device_types(&self) -> Result<u32> {
        self.proxy
            .get_property("SupportedDeviceTypes")
            .await
            .context("Failed to read SupportedDeviceTypes property")
    }
}

/// RemoteDesktop Session interface proxy
///
/// Interface: org.gnome.Mutter.RemoteDesktop.Session
/// Path: /org/gnome/Mutter/RemoteDesktop/Session/*
#[derive(Debug)]
pub struct MutterRemoteDesktopSession<'a> {
    proxy: zbus::Proxy<'a>,
}

impl MutterRemoteDesktopSession<'_> {
    pub async fn new(connection: &Connection, session_path: OwnedObjectPath) -> Result<Self> {
        let proxy = zbus::ProxyBuilder::new(connection)
            .interface("org.gnome.Mutter.RemoteDesktop.Session")?
            .path(session_path)?
            .destination("org.gnome.Mutter.RemoteDesktop")?
            .build()
            .await
            .context("Failed to create RemoteDesktop Session proxy")?;

        Ok(Self { proxy })
    }

    // === Properties ===

    /// Read the session ID (used to link ScreenCast sessions)
    pub async fn session_id(&self) -> Result<String> {
        self.proxy
            .get_property("SessionId")
            .await
            .context("Failed to read SessionId property")
    }

    /// Current CapsLock state
    pub async fn caps_lock_state(&self) -> Result<bool> {
        self.proxy
            .get_property("CapsLockState")
            .await
            .context("Failed to read CapsLockState property")
    }

    /// Current NumLock state
    pub async fn num_lock_state(&self) -> Result<bool> {
        self.proxy
            .get_property("NumLockState")
            .await
            .context("Failed to read NumLockState property")
    }

    // === Session Lifecycle ===

    /// Connect to EIS (Emulated Input Service) and return the socket FD
    ///
    /// On GNOME 46+, EIS provides lower-latency input injection than D-Bus methods.
    /// The returned FD is a Unix socket for the EI protocol.
    pub async fn connect_to_eis(
        &self,
        options: HashMap<String, Value<'_>>,
    ) -> Result<std::os::fd::OwnedFd> {
        let reply = self
            .proxy
            .call_method("ConnectToEIS", &(options,))
            .await
            .context("ConnectToEIS call failed")?;

        let body = reply.body();
        let fd: zvariant::OwnedFd = body
            .deserialize()
            .context("Failed to deserialize ConnectToEIS FD")?;

        tracing::info!("Connected to EIS (Emulated Input Service)");
        Ok(fd.into())
    }

    /// Start the remote desktop session
    pub async fn start(&self) -> Result<()> {
        self.proxy
            .call_method("Start", &())
            .await
            .context("Failed to call Start")?;

        Ok(())
    }

    /// Stop the remote desktop session
    pub async fn stop(&self) -> Result<()> {
        self.proxy
            .call_method("Stop", &())
            .await
            .context("Failed to call Stop")?;

        Ok(())
    }

    /// Subscribe to the Closed signal for unexpected session termination
    pub async fn subscribe_closed(
        &self,
    ) -> Result<impl futures_util::Stream<Item = zbus::Message>> {
        self.proxy
            .receive_signal("Closed")
            .await
            .context("Failed to subscribe to Closed signal")
    }

    // === Input Methods ===
    // Signatures match the Mutter D-Bus spec:
    //   NotifyKeyboardKeycode(u keycode, b state)
    //   NotifyKeyboardKeysym(u keysym, b state)
    //   NotifyPointerButton(i button, b state)
    //   NotifyPointerMotionRelative(dx, dy)
    //   NotifyPointerMotionAbsolute(stream, x, y)
    //   NotifyPointerAxis(dx, dy, flags)
    //   NotifyPointerAxisDiscrete(axis, steps)

    /// Inject keyboard keycode event
    ///
    /// * `keycode` - Linux evdev keycode
    /// * `pressed` - true for press, false for release
    pub async fn notify_keyboard_keycode(&self, keycode: u32, pressed: bool) -> Result<()> {
        self.proxy
            .call_method("NotifyKeyboardKeycode", &(keycode, pressed))
            .await
            .context("Failed to inject keyboard keycode")?;

        Ok(())
    }

    /// Inject keyboard keysym event
    ///
    /// * `keysym` - X11 keysym
    /// * `pressed` - true for press, false for release
    pub async fn notify_keyboard_keysym(&self, keysym: u32, pressed: bool) -> Result<()> {
        self.proxy
            .call_method("NotifyKeyboardKeysym", &(keysym, pressed))
            .await
            .context("Failed to inject keyboard keysym")?;

        Ok(())
    }

    /// Inject absolute pointer motion
    ///
    /// * `stream` - ScreenCast stream object path
    /// * `x` - Absolute X coordinate
    /// * `y` - Absolute Y coordinate
    pub async fn notify_pointer_motion_absolute(
        &self,
        stream: &ObjectPath<'_>,
        x: f64,
        y: f64,
    ) -> Result<()> {
        self.proxy
            .call_method("NotifyPointerMotionAbsolute", &(stream, x, y))
            .await
            .context("Failed to inject pointer motion")?;

        Ok(())
    }

    /// Inject relative pointer motion
    ///
    /// Note: Mutter D-Bus spec names this `NotifyPointerMotionRelative`
    pub async fn notify_pointer_motion_relative(&self, dx: f64, dy: f64) -> Result<()> {
        self.proxy
            .call_method("NotifyPointerMotionRelative", &(dx, dy))
            .await
            .context("Failed to inject relative pointer motion")?;

        Ok(())
    }

    /// Inject pointer button event
    ///
    /// * `button` - Linux button code (BTN_LEFT=0x110, etc.)
    /// * `pressed` - true for press, false for release
    pub async fn notify_pointer_button(&self, button: i32, pressed: bool) -> Result<()> {
        self.proxy
            .call_method("NotifyPointerButton", &(button, pressed))
            .await
            .context("Failed to inject pointer button")?;

        Ok(())
    }

    /// Inject pointer axis (scroll) event
    ///
    /// * `dx` - Horizontal scroll delta
    /// * `dy` - Vertical scroll delta
    pub async fn notify_pointer_axis(&self, dx: f64, dy: f64) -> Result<()> {
        let flags = 0u32;

        self.proxy
            .call_method("NotifyPointerAxis", &(dx, dy, flags))
            .await
            .context("Failed to inject pointer axis")?;

        Ok(())
    }

    /// Inject discrete pointer axis (scroll) event
    ///
    /// * `axis` - Axis (0=horizontal, 1=vertical)
    /// * `steps` - Number of discrete steps
    pub async fn notify_pointer_axis_discrete(&self, axis: u32, steps: i32) -> Result<()> {
        self.proxy
            .call_method("NotifyPointerAxisDiscrete", &(axis, steps))
            .await
            .context("Failed to inject discrete pointer axis")?;

        Ok(())
    }

    // === Touch Input (stubs for future use) ===

    pub async fn notify_touch_down(
        &self,
        stream: &ObjectPath<'_>,
        slot: u32,
        x: f64,
        y: f64,
    ) -> Result<()> {
        self.proxy
            .call_method("NotifyTouchDown", &(stream, slot, x, y))
            .await
            .context("Failed to inject touch down")?;

        Ok(())
    }

    pub async fn notify_touch_motion(
        &self,
        stream: &ObjectPath<'_>,
        slot: u32,
        x: f64,
        y: f64,
    ) -> Result<()> {
        self.proxy
            .call_method("NotifyTouchMotion", &(stream, slot, x, y))
            .await
            .context("Failed to inject touch motion")?;

        Ok(())
    }

    pub async fn notify_touch_up(&self, slot: u32) -> Result<()> {
        self.proxy
            .call_method("NotifyTouchUp", &(slot,))
            .await
            .context("Failed to inject touch up")?;

        Ok(())
    }

    // === Clipboard ===

    /// Enable clipboard sharing
    ///
    /// Options can include supported MIME types
    pub async fn enable_clipboard(&self, options: HashMap<String, Value<'_>>) -> Result<()> {
        self.proxy
            .call_method("EnableClipboard", &(options,))
            .await
            .context("Failed to enable clipboard")?;

        Ok(())
    }

    /// Disable clipboard sharing
    pub async fn disable_clipboard(&self) -> Result<()> {
        self.proxy
            .call_method("DisableClipboard", &())
            .await
            .context("Failed to disable clipboard")?;

        Ok(())
    }

    /// Advertise clipboard content (set selection ownership)
    pub async fn set_selection(&self, options: HashMap<String, Value<'_>>) -> Result<()> {
        self.proxy
            .call_method("SetSelection", &(options,))
            .await
            .context("Failed to set selection")?;

        Ok(())
    }

    /// Get FD for writing clipboard data (responding to a SelectionTransfer)
    pub async fn selection_write(&self, serial: u32) -> Result<std::os::fd::OwnedFd> {
        let reply = self
            .proxy
            .call_method("SelectionWrite", &(serial,))
            .await
            .context("Failed to call SelectionWrite")?;

        let body = reply.body();
        let fd: zvariant::OwnedFd = body
            .deserialize()
            .context("Failed to deserialize SelectionWrite FD")?;

        Ok(fd.into())
    }

    /// Signal that clipboard write is complete
    pub async fn selection_write_done(&self, serial: u32, success: bool) -> Result<()> {
        self.proxy
            .call_method("SelectionWriteDone", &(serial, success))
            .await
            .context("Failed to call SelectionWriteDone")?;

        Ok(())
    }

    /// Read clipboard content for a given MIME type
    pub async fn selection_read(&self, mime_type: &str) -> Result<std::os::fd::OwnedFd> {
        let reply = self
            .proxy
            .call_method("SelectionRead", &(mime_type,))
            .await
            .context("Failed to call SelectionRead")?;

        let body = reply.body();
        let fd: zvariant::OwnedFd = body
            .deserialize()
            .context("Failed to deserialize SelectionRead FD")?;

        Ok(fd.into())
    }

    /// Subscribe to SelectionOwnerChanged signal
    pub async fn subscribe_selection_owner_changed(
        &self,
    ) -> Result<impl futures_util::Stream<Item = zbus::Message>> {
        self.proxy
            .receive_signal("SelectionOwnerChanged")
            .await
            .context("Failed to subscribe to SelectionOwnerChanged signal")
    }

    /// Subscribe to SelectionTransfer signal
    pub async fn subscribe_selection_transfer(
        &self,
    ) -> Result<impl futures_util::Stream<Item = zbus::Message>> {
        self.proxy
            .receive_signal("SelectionTransfer")
            .await
            .context("Failed to subscribe to SelectionTransfer signal")
    }

    // === Keymap ===

    /// Set the keyboard keymap
    pub async fn set_keymap(&self, options: HashMap<String, Value<'_>>) -> Result<()> {
        self.proxy
            .call_method("SetKeymap", &(options,))
            .await
            .context("Failed to set keymap")?;

        Ok(())
    }

    /// Set the active keymap layout index
    pub async fn set_keymap_layout_index(&self, index: u32) -> Result<()> {
        self.proxy
            .call_method("SetKeymapLayoutIndex", &(index,))
            .await
            .context("Failed to set keymap layout index")?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[ignore = "Requires GNOME with Mutter"]
    async fn test_mutter_remote_desktop_availability() {
        match zbus::Connection::session().await {
            Ok(conn) => match MutterRemoteDesktop::new(&conn).await {
                Ok(proxy) => {
                    println!("Mutter RemoteDesktop API available");
                    match proxy.version().await {
                        Ok(v) => println!("  Version: {v}"),
                        Err(e) => println!("  Version not available: {e}"),
                    }
                }
                Err(e) => println!("Mutter RemoteDesktop not available: {e}"),
            },
            Err(e) => println!("D-Bus session not available: {e}"),
        }
    }
}
