//! ScreenCast-Only Strategy (View-Only Mode)
//!
//! Uses Portal ScreenCast without RemoteDesktop to provide view-only
//! RDP sessions. Applicable when:
//! - User explicitly requests view-only mode via config (`server.view_only = true`)
//! - Running in Flatpak where sandbox blocks direct protocols and RemoteDesktop is unavailable
//! - Compositor has no Portal RemoteDesktop (wlroots without portal-wlr)
//! - All other input strategies have been exhausted (last-resort fallback)
//!
//! The resulting session has video and audio but no input injection or clipboard.

use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use ashpd::desktop::{
    screencast::{CursorMode, Screencast, SourceType},
    PersistMode,
};
use async_trait::async_trait;
use tracing::{info, warn};

use crate::session::strategy::{
    ClipboardComponents, PipeWireAccess, SessionHandle, SessionStrategy, SessionType, StreamInfo,
};

/// Session handle for ScreenCast-only (view-only) mode
pub struct ScreenCastOnlySessionHandle {
    pipewire_fd: i32,
    streams: Vec<StreamInfo>,
}

#[async_trait]
impl SessionHandle for ScreenCastOnlySessionHandle {
    fn pipewire_access(&self) -> PipeWireAccess {
        PipeWireAccess::FileDescriptor(self.pipewire_fd)
    }

    fn streams(&self) -> Vec<StreamInfo> {
        self.streams.clone()
    }

    fn session_type(&self) -> SessionType {
        SessionType::ScreenCastOnly
    }

    async fn notify_keyboard_keycode(&self, _keycode: i32, _pressed: bool) -> Result<()> {
        Err(anyhow!("Input not available in view-only mode"))
    }

    async fn notify_pointer_motion_absolute(
        &self,
        _stream_id: u32,
        _x: f64,
        _y: f64,
    ) -> Result<()> {
        Err(anyhow!("Input not available in view-only mode"))
    }

    async fn notify_pointer_button(&self, _button: i32, _pressed: bool) -> Result<()> {
        Err(anyhow!("Input not available in view-only mode"))
    }

    async fn notify_pointer_axis(&self, _dx: f64, _dy: f64) -> Result<()> {
        Err(anyhow!("Input not available in view-only mode"))
    }

    fn portal_clipboard(&self) -> Option<ClipboardComponents> {
        None
    }
}

/// ScreenCast-only strategy for view-only Flatpak sessions on wlroots
pub struct ScreenCastOnlyStrategy;

impl Default for ScreenCastOnlyStrategy {
    fn default() -> Self {
        Self::new()
    }
}

impl ScreenCastOnlyStrategy {
    pub fn new() -> Self {
        Self
    }

    /// Check if ScreenCast portal is available (without requiring RemoteDesktop)
    pub async fn is_available() -> bool {
        match Screencast::new().await {
            Ok(_) => true,
            Err(e) => {
                warn!("ScreenCast portal not available: {}", e);
                false
            }
        }
    }
}

#[async_trait]
impl SessionStrategy for ScreenCastOnlyStrategy {
    fn name(&self) -> &'static str {
        "ScreenCast-only (view-only)"
    }

    fn requires_initial_setup(&self) -> bool {
        true // Requires user to approve screen sharing
    }

    fn supports_unattended_restore(&self) -> bool {
        false // No token persistence for ScreenCast-only
    }

    async fn create_session(&self) -> Result<Arc<dyn SessionHandle>> {
        info!("Creating ScreenCast-only session (view-only mode)");

        let screencast = Screencast::new()
            .await
            .context("Failed to connect to ScreenCast portal")?;

        let session = screencast
            .create_session()
            .await
            .context("Failed to create ScreenCast session")?;

        screencast
            .select_sources(
                &session,
                CursorMode::Metadata,
                SourceType::Monitor.into(),
                false, // don't allow multiple sources
                None,  // no restore token
                PersistMode::DoNot,
            )
            .await
            .context("Failed to select ScreenCast sources")?;

        let response = screencast
            .start(&session, None)
            .await
            .context("Failed to start ScreenCast")?
            .response()
            .context("ScreenCast start rejected by user")?;

        let portal_streams = response.streams();
        if portal_streams.is_empty() {
            return Err(anyhow!("No streams available from ScreenCast"));
        }

        let streams: Vec<StreamInfo> = portal_streams
            .iter()
            .map(|s| {
                let (width, height) = s.size().unwrap_or((0, 0));
                let (x, y) = s.position().unwrap_or((0, 0));
                StreamInfo {
                    node_id: s.pipe_wire_node_id(),
                    width: width as u32,
                    height: height as u32,
                    position_x: x,
                    position_y: y,
                }
            })
            .collect();

        info!("ScreenCast session started: {} stream(s)", streams.len());
        for stream in &streams {
            info!(
                "  Stream: node_id={}, {}x{} at ({},{})",
                stream.node_id, stream.width, stream.height, stream.position_x, stream.position_y
            );
        }

        let fd = screencast
            .open_pipe_wire_remote(&session)
            .await
            .context("Failed to open PipeWire remote")?;

        use std::os::fd::AsRawFd;
        let raw_fd = fd.as_raw_fd();

        // Leak the OwnedFd to keep it alive for the session duration.
        // The FD is closed when the server shuts down.
        std::mem::forget(fd);

        info!("PipeWire FD: {}", raw_fd);

        let handle = ScreenCastOnlySessionHandle {
            pipewire_fd: raw_fd,
            streams,
        };

        Ok(Arc::new(handle))
    }

    async fn cleanup(&self, _session: &dyn SessionHandle) -> Result<()> {
        info!("ScreenCast-only session cleanup");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_session_type() {
        let handle = ScreenCastOnlySessionHandle {
            pipewire_fd: 0,
            streams: vec![],
        };
        assert_eq!(handle.session_type(), SessionType::ScreenCastOnly);
    }

    #[test]
    fn test_no_clipboard() {
        let handle = ScreenCastOnlySessionHandle {
            pipewire_fd: 0,
            streams: vec![],
        };
        assert!(handle.portal_clipboard().is_none());
    }

    #[tokio::test]
    async fn test_input_rejected() {
        let handle = ScreenCastOnlySessionHandle {
            pipewire_fd: 0,
            streams: vec![],
        };
        assert!(handle.notify_keyboard_keycode(42, true).await.is_err());
        assert!(handle
            .notify_pointer_motion_absolute(0, 100.0, 100.0)
            .await
            .is_err());
        assert!(handle.notify_pointer_button(1, true).await.is_err());
        assert!(handle.notify_pointer_axis(0.0, 1.0).await.is_err());
    }
}
