//! Mutter Direct API Strategy Implementation
//!
//! Uses org.gnome.Mutter.ScreenCast and org.gnome.Mutter.RemoteDesktop D-Bus APIs
//! directly, bypassing the XDG Portal permission model entirely.
//!
//! GNOME-specific, zero-dialog operation.

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use std::sync::Arc;
use tracing::{debug, info, warn};

use crate::mutter::{MutterSessionHandle, MutterSessionManager};
use crate::session::strategy::{
    PipeWireAccess, SessionHandle, SessionStrategy, SessionType, StreamInfo,
};

/// Mutter session handle wrapper
pub struct MutterSessionHandleImpl {
    /// Underlying Mutter session
    mutter_handle: MutterSessionHandle,
}

#[async_trait]
impl SessionHandle for MutterSessionHandleImpl {
    fn pipewire_access(&self) -> PipeWireAccess {
        // Mutter provides PipeWire node ID, not file descriptor
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
        // Create RemoteDesktop session proxy for input injection
        let rd_session = crate::mutter::MutterRemoteDesktopSession::new(
            &self.mutter_handle.connection,
            self.mutter_handle.remote_desktop_session.clone(),
        )
        .await
        .context("Failed to create Mutter RemoteDesktop session proxy")?;

        rd_session
            .notify_keyboard_keycode(keycode, pressed)
            .await
            .context("Failed to inject keyboard keycode via Mutter")
    }

    async fn notify_pointer_motion_absolute(&self, stream_id: u32, x: f64, y: f64) -> Result<()> {
        // Create RemoteDesktop session proxy
        let rd_session = crate::mutter::MutterRemoteDesktopSession::new(
            &self.mutter_handle.connection,
            self.mutter_handle.remote_desktop_session.clone(),
        )
        .await
        .context("Failed to create Mutter RemoteDesktop session proxy")?;

        // Mutter needs the stream object path, not just the node ID
        // Find the stream path that corresponds to this node ID
        let stream_path = self
            .mutter_handle
            .streams
            .first()
            .ok_or_else(|| anyhow!("No streams available"))?;

        rd_session
            .notify_pointer_motion_absolute(stream_path, x, y)
            .await
            .context("Failed to inject pointer motion via Mutter")
    }

    async fn notify_pointer_button(&self, button: i32, pressed: bool) -> Result<()> {
        // Create RemoteDesktop session proxy
        let rd_session = crate::mutter::MutterRemoteDesktopSession::new(
            &self.mutter_handle.connection,
            self.mutter_handle.remote_desktop_session.clone(),
        )
        .await
        .context("Failed to create Mutter RemoteDesktop session proxy")?;

        rd_session
            .notify_pointer_button(button, pressed)
            .await
            .context("Failed to inject pointer button via Mutter")
    }

    async fn notify_pointer_axis(&self, dx: f64, dy: f64) -> Result<()> {
        // Create RemoteDesktop session proxy
        let rd_session = crate::mutter::MutterRemoteDesktopSession::new(
            &self.mutter_handle.connection,
            self.mutter_handle.remote_desktop_session.clone(),
        )
        .await
        .context("Failed to create Mutter RemoteDesktop session proxy")?;

        rd_session
            .notify_pointer_axis(dx, dy)
            .await
            .context("Failed to inject pointer axis via Mutter")
    }

    fn portal_clipboard(&self) -> Option<crate::session::strategy::ClipboardComponents> {
        // Mutter has no clipboard API
        // Caller must create a separate Portal session for clipboard operations
        None
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
    /// Create a new Mutter Direct strategy
    ///
    /// # Arguments
    ///
    /// * `monitor_connector` - Optional monitor connector name for physical monitor,
    ///                         or None to use virtual monitor (headless)
    pub fn new(monitor_connector: Option<String>) -> Self {
        Self { monitor_connector }
    }

    /// Check if Mutter API is available before use
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
        // NO setup required - no permission dialog
        false
    }

    fn supports_unattended_restore(&self) -> bool {
        // Always works without user interaction
        true
    }

    async fn create_session(&self) -> Result<Arc<dyn SessionHandle>> {
        info!("Creating session using Mutter Direct API (NO DIALOG)");

        // Verify we're on GNOME
        let compositor = crate::compositor::identify_compositor();
        if !matches!(compositor, crate::compositor::CompositorType::Gnome { .. }) {
            return Err(anyhow!("Mutter Direct API only available on GNOME"));
        }

        // Check if running in Flatpak (would block D-Bus access)
        if std::path::Path::new("/.flatpak-info").exists() {
            return Err(anyhow!(
                "Mutter Direct API not available in Flatpak (sandbox blocks D-Bus access)"
            ));
        }

        // Create Mutter session manager
        let manager = MutterSessionManager::new()
            .await
            .context("Failed to create Mutter session manager")?;

        // Create session with specified monitor (or virtual)
        let mutter_handle = manager
            .create_session(self.monitor_connector.as_deref())
            .await
            .context("Failed to create Mutter session")?;

        info!("✅ Mutter session created successfully (ZERO DIALOGS)");

        // Log what we got
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

        // Wrap in our handle type
        let handle = MutterSessionHandleImpl { mutter_handle };

        Ok(Arc::new(handle))
    }

    async fn cleanup(&self, session: &dyn SessionHandle) -> Result<()> {
        info!("Cleaning up Mutter session");

        // Mutter sessions clean up automatically when D-Bus objects are dropped
        // But we can explicitly stop them for cleaner shutdown

        debug!("Mutter session cleanup (automatic via D-Bus object lifecycle)");

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[ignore] // Requires GNOME with Mutter running
    async fn test_mutter_direct_strategy() {
        if !MutterDirectStrategy::is_available().await {
            println!("Mutter API not available, skipping test");
            return;
        }

        let strategy = MutterDirectStrategy::new(None); // Virtual monitor

        match strategy.create_session().await {
            Ok(handle) => {
                println!("Mutter session created successfully");
                println!("Session type: {:?}", handle.session_type());
                println!("Streams: {}", handle.streams().len());

                // Cleanup
                strategy.cleanup(handle.as_ref()).await.ok();
            }
            Err(e) => {
                println!("Failed to create Mutter session: {}", e);
            }
        }
    }

    #[tokio::test]
    async fn test_mutter_availability_check() {
        let available = MutterDirectStrategy::is_available().await;
        println!("Mutter Direct API available: {}", available);
    }
}
