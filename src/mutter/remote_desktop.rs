//! Mutter RemoteDesktop D-Bus Interface
//!
//! Defines proxies for org.gnome.Mutter.RemoteDesktop D-Bus interfaces.
//! Used for input injection (keyboard, mouse) without portal permissions.

use anyhow::{Context, Result};
use zbus::{
    zvariant::{ObjectPath, OwnedObjectPath},
    Connection,
};

/// Main RemoteDesktop interface proxy
///
/// Service: org.gnome.Mutter.RemoteDesktop
/// Path: /org/gnome/Mutter/RemoteDesktop
#[derive(Debug)]
pub struct MutterRemoteDesktop<'a> {
    connection: Connection,
    proxy: zbus::Proxy<'a>,
}

impl MutterRemoteDesktop<'_> {
    /// Create a new RemoteDesktop proxy
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
    ///
    /// # Returns
    ///
    /// Object path to the created session
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
    /// Create a session proxy for an existing session
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

    /// Connect to EIS (Emulated Input Service) if needed
    ///
    /// GNOME 46+ requires ConnectToEIS before input injection works
    pub async fn connect_to_eis(&self) -> Result<()> {
        use std::collections::HashMap;

        use zbus::zvariant::Value;

        let options: HashMap<String, Value> = HashMap::new();

        // ConnectToEIS returns a file descriptor, but we don't need it for basic input
        match self.proxy.call_method("ConnectToEIS", &(options,)).await {
            Ok(_) => {
                tracing::info!("Connected to EIS (Emulated Input Service)");
                Ok(())
            }
            Err(e) => {
                // ConnectToEIS might not be available on older GNOME versions
                tracing::debug!("ConnectToEIS not available: {}", e);
                Ok(())
            }
        }
    }

    /// Start the remote desktop session
    pub async fn start(&self) -> Result<()> {
        // Connect to EIS first (required on GNOME 46+)
        self.connect_to_eis().await?;

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

    /// Inject keyboard keycode event
    ///
    /// # Arguments
    ///
    /// * `keycode` - Linux keycode (evdev)
    /// * `pressed` - true for press, false for release
    pub async fn notify_keyboard_keycode(&self, keycode: i32, pressed: bool) -> Result<()> {
        let state = if pressed { 1u32 } else { 0u32 };

        self.proxy
            .call_method("NotifyKeyboardKeycode", &(keycode, state))
            .await
            .context("Failed to inject keyboard keycode")?;

        Ok(())
    }

    /// Inject keyboard keysym event
    ///
    /// # Arguments
    ///
    /// * `keysym` - X11 keysym
    /// * `pressed` - true for press, false for release
    pub async fn notify_keyboard_keysym(&self, keysym: u32, pressed: bool) -> Result<()> {
        let state = if pressed { 1u32 } else { 0u32 };

        self.proxy
            .call_method("NotifyKeyboardKeysym", &(keysym, state))
            .await
            .context("Failed to inject keyboard keysym")?;

        Ok(())
    }

    /// Inject absolute pointer motion
    ///
    /// # Arguments
    ///
    /// * `stream` - Stream path (from ScreenCast)
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
    /// # Arguments
    ///
    /// * `dx` - Relative X movement
    /// * `dy` - Relative Y movement
    pub async fn notify_pointer_motion(&self, dx: f64, dy: f64) -> Result<()> {
        self.proxy
            .call_method("NotifyPointerMotion", &(dx, dy))
            .await
            .context("Failed to inject pointer motion")?;

        Ok(())
    }

    /// Inject pointer button event
    ///
    /// # Arguments
    ///
    /// * `button` - Button number (1=left, 2=middle, 3=right)
    /// * `pressed` - true for press, false for release
    pub async fn notify_pointer_button(&self, button: i32, pressed: bool) -> Result<()> {
        let state = if pressed { 1u32 } else { 0u32 };

        self.proxy
            .call_method("NotifyPointerButton", &(button, state))
            .await
            .context("Failed to inject pointer button")?;

        Ok(())
    }

    /// Inject pointer axis (scroll) event
    ///
    /// # Arguments
    ///
    /// * `dx` - Horizontal scroll delta
    /// * `dy` - Vertical scroll delta
    pub async fn notify_pointer_axis(&self, dx: f64, dy: f64) -> Result<()> {
        // Flags: 0 = no special flags
        let flags = 0u32;

        self.proxy
            .call_method("NotifyPointerAxis", &(dx, dy, flags))
            .await
            .context("Failed to inject pointer axis")?;

        Ok(())
    }

    /// Inject discrete pointer axis (scroll) event
    ///
    /// # Arguments
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[ignore] // Requires GNOME with Mutter
    async fn test_mutter_remote_desktop_availability() {
        match zbus::Connection::session().await {
            Ok(conn) => match MutterRemoteDesktop::new(&conn).await {
                Ok(_proxy) => println!("Mutter RemoteDesktop API available"),
                Err(e) => println!("Mutter RemoteDesktop not available: {}", e),
            },
            Err(e) => println!("D-Bus session not available: {}", e),
        }
    }
}
