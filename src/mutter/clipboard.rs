//! Mutter Clipboard via RemoteDesktop D-Bus API
//!
//! Implements clipboard sharing using Mutter's RemoteDesktop session clipboard methods
//! (EnableClipboard, DisableClipboard, SetSelection, SelectionRead, SelectionWrite).
//!
//! This replaces the need for a Portal clipboard session when using the Mutter strategy.

use std::{
    collections::HashMap,
    io::{Read, Write},
};

use anyhow::{Context, Result};
use tokio::sync::Mutex;
use tracing::{debug, info, warn};
use zbus::zvariant::{OwnedObjectPath, Value};

use super::remote_desktop::MutterRemoteDesktopSession;

/// Supported MIME types for clipboard sharing
const CLIPBOARD_MIME_TYPES: &[&str] = &[
    "text/plain;charset=utf-8",
    "text/plain",
    "UTF8_STRING",
    "STRING",
    "TEXT",
    "text/html",
    "image/png",
    "image/bmp",
];

/// Mutter clipboard manager
///
/// Wraps the RemoteDesktop session's clipboard methods to provide clipboard
/// sharing without needing a separate Portal session.
pub struct MutterClipboardManager {
    /// D-Bus connection for creating session proxies
    connection: zbus::Connection,
    /// RemoteDesktop session object path
    rd_session_path: OwnedObjectPath,
    /// Whether clipboard is currently enabled
    enabled: Mutex<bool>,
}

impl MutterClipboardManager {
    /// Create a new clipboard manager for the given RemoteDesktop session
    pub(crate) fn new(connection: zbus::Connection, rd_session_path: OwnedObjectPath) -> Self {
        Self {
            connection,
            rd_session_path,
            enabled: Mutex::new(false),
        }
    }

    /// Get a session proxy for D-Bus calls
    async fn session_proxy(&self) -> Result<MutterRemoteDesktopSession<'_>> {
        MutterRemoteDesktopSession::new(&self.connection, self.rd_session_path.clone())
            .await
            .context("Failed to create session proxy for clipboard")
    }

    /// Enable clipboard sharing with Mutter
    pub(crate) async fn enable(&self) -> Result<()> {
        let proxy = self.session_proxy().await?;

        let mut options: HashMap<String, Value<'_>> = HashMap::new();
        // Advertise the MIME types we can handle
        let mime_types: Vec<Value<'_>> = CLIPBOARD_MIME_TYPES
            .iter()
            .map(|&s| Value::new(s.to_string()))
            .collect();
        options.insert("mime-types".to_string(), Value::new(mime_types));

        proxy.enable_clipboard(options).await?;

        *self.enabled.lock().await = true;
        info!("Mutter clipboard enabled");
        Ok(())
    }

    /// Disable clipboard sharing
    pub(crate) async fn disable(&self) -> Result<()> {
        let proxy = self.session_proxy().await?;
        proxy.disable_clipboard().await?;

        *self.enabled.lock().await = false;
        info!("Mutter clipboard disabled");
        Ok(())
    }

    /// Check if clipboard is enabled
    pub(crate) async fn is_enabled(&self) -> bool {
        *self.enabled.lock().await
    }

    /// Advertise clipboard content ownership (Linux -> RDP direction)
    ///
    /// Call this when the local clipboard content changes and we want to
    /// advertise it to Mutter.
    pub(crate) async fn set_selection(&self, mime_types: &[String]) -> Result<()> {
        let proxy = self.session_proxy().await?;

        let mut options: HashMap<String, Value<'_>> = HashMap::new();
        let types: Vec<Value<'_>> = mime_types.iter().map(|s| Value::new(s.clone())).collect();
        options.insert("mime-types".to_string(), Value::new(types));

        proxy.set_selection(options).await?;
        debug!(
            "Set clipboard selection with {} MIME types",
            mime_types.len()
        );
        Ok(())
    }

    /// Read clipboard content from Mutter for a given MIME type
    ///
    /// Returns the raw clipboard data bytes.
    pub(crate) async fn read_selection(&self, mime_type: &str) -> Result<Vec<u8>> {
        let proxy = self.session_proxy().await?;

        let fd = proxy.selection_read(mime_type).await?;

        // Read data from the FD in a blocking task (FD I/O can block)
        let data = tokio::task::spawn_blocking(move || -> Result<Vec<u8>> {
            let mut file = std::fs::File::from(fd);
            let mut buf = Vec::new();
            file.read_to_end(&mut buf)
                .context("Failed to read clipboard data from FD")?;
            Ok(buf)
        })
        .await
        .context("Clipboard read task panicked")??;

        debug!(
            "Read {} bytes of clipboard data (MIME: {})",
            data.len(),
            mime_type
        );
        Ok(data)
    }

    /// Write clipboard content to Mutter (RDP -> Linux direction)
    ///
    /// Responds to a SelectionTransfer by writing data to the FD Mutter provides.
    pub(crate) async fn write_selection(&self, serial: u32, data: &[u8]) -> Result<()> {
        let proxy = self.session_proxy().await?;

        let fd = proxy.selection_write(serial).await?;

        let data_owned = data.to_vec();
        let write_result = tokio::task::spawn_blocking(move || -> Result<()> {
            let mut file = std::fs::File::from(fd);
            file.write_all(&data_owned)
                .context("Failed to write clipboard data to FD")?;
            Ok(())
        })
        .await
        .context("Clipboard write task panicked")?;

        match &write_result {
            Ok(()) => {
                proxy.selection_write_done(serial, true).await?;
                debug!(
                    "Wrote {} bytes to clipboard (serial: {})",
                    data.len(),
                    serial
                );
            }
            Err(e) => {
                warn!("Clipboard write failed: {}", e);
                proxy.selection_write_done(serial, false).await.ok();
            }
        }

        write_result
    }

    /// Subscribe to SelectionOwnerChanged signal
    ///
    /// This signal fires when another application claims clipboard ownership,
    /// meaning new content is available for us to read and forward to the RDP client.
    pub(crate) async fn subscribe_selection_owner_changed(
        &self,
    ) -> Result<impl futures_util::Stream<Item = zbus::Message>> {
        let proxy = self.session_proxy().await?;
        proxy.subscribe_selection_owner_changed().await
    }

    /// Subscribe to SelectionTransfer signal
    ///
    /// This signal fires when Mutter requests clipboard data from us
    /// (i.e., a local application wants to paste RDP client content).
    pub(crate) async fn subscribe_selection_transfer(
        &self,
    ) -> Result<impl futures_util::Stream<Item = zbus::Message>> {
        let proxy = self.session_proxy().await?;
        proxy.subscribe_selection_transfer().await
    }
}

impl std::fmt::Debug for MutterClipboardManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MutterClipboardManager")
            .field("rd_session_path", &self.rd_session_path)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_clipboard_mime_types() {
        // Verify we have the essential MIME types
        assert!(CLIPBOARD_MIME_TYPES.contains(&"text/plain;charset=utf-8"));
        assert!(CLIPBOARD_MIME_TYPES.contains(&"text/plain"));
        assert!(CLIPBOARD_MIME_TYPES.contains(&"image/png"));
    }
}
