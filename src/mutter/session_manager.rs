//! Mutter Session Manager
//!
//! High-level API for creating and managing Mutter ScreenCast + RemoteDesktop sessions.
//! This provides a unified interface similar to PortalManager but using Mutter's
//! direct D-Bus APIs instead of going through the XDG Portal.

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::os::fd::AsRawFd;
use tracing::{debug, info};
use zbus::zvariant::{OwnedObjectPath, Value};

use super::remote_desktop::{MutterRemoteDesktop, MutterRemoteDesktopSession};
use super::screencast::{MutterScreenCast, MutterScreenCastSession, MutterScreenCastStream};

/// Stream information from Mutter
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MutterStreamInfo {
    /// PipeWire node ID
    pub node_id: u32,
    /// Stream width
    pub width: u32,
    /// Stream height
    pub height: u32,
    /// X position in global coordinate space
    pub position_x: i32,
    /// Y position in global coordinate space
    pub position_y: i32,
}

/// Mutter session handle (analogous to PortalSessionHandle)
pub struct MutterSessionHandle {
    /// ScreenCast session
    pub screencast_session: OwnedObjectPath,
    /// RemoteDesktop session
    pub remote_desktop_session: OwnedObjectPath,
    /// Stream object paths
    pub streams: Vec<OwnedObjectPath>,
    /// Stream information
    pub stream_info: Vec<MutterStreamInfo>,
    /// Connection (kept alive for session, public for input injection)
    pub connection: zbus::Connection,
}

/// Mutter Session Manager
///
/// Manages Mutter ScreenCast and RemoteDesktop sessions without portal dialogs.
/// This is GNOME-specific and requires non-sandboxed D-Bus access.
pub struct MutterSessionManager {
    connection: zbus::Connection,
}

impl MutterSessionManager {
    /// Create a new Mutter session manager
    ///
    /// # Returns
    ///
    /// Manager if Mutter APIs are available
    pub async fn new() -> Result<Self> {
        info!("Initializing Mutter session manager");

        // Connect to session D-Bus
        let connection = zbus::Connection::session()
            .await
            .context("Failed to connect to D-Bus session")?;

        // Verify Mutter APIs are available
        if !super::is_mutter_api_available().await {
            return Err(anyhow!(
                "Mutter ScreenCast and RemoteDesktop APIs not available"
            ));
        }

        info!("Mutter session manager initialized successfully");

        Ok(Self { connection })
    }

    /// Create a complete Mutter session (ScreenCast + RemoteDesktop)
    ///
    /// This creates both a ScreenCast session (for video) and a RemoteDesktop
    /// session (for input injection) without triggering any permission dialogs.
    ///
    /// # Arguments
    ///
    /// * `monitor_connector` - Optional monitor connector (e.g., "HDMI-1"). If None, uses virtual monitor.
    ///
    /// # Returns
    ///
    /// Session handle with PipeWire access and input capabilities
    pub async fn create_session(
        &self,
        monitor_connector: Option<&str>,
    ) -> Result<MutterSessionHandle> {
        info!("Creating Mutter session (ScreenCast + RemoteDesktop)");

        // Create ScreenCast session
        let screencast_proxy = MutterScreenCast::new(&self.connection).await?;

        let sc_properties = HashMap::new();
        let screencast_session_path = screencast_proxy
            .create_session(sc_properties)
            .await
            .context("Failed to create Mutter ScreenCast session")?;

        info!(
            "Mutter ScreenCast session created: {:?}",
            screencast_session_path
        );

        // Create session proxy
        let session_proxy =
            MutterScreenCastSession::new(&self.connection, screencast_session_path.clone()).await?;

        // Record monitor or virtual screen
        let stream_path = if let Some(connector) = monitor_connector {
            info!("Recording monitor: {}", connector);

            // Cursor mode: 2 = metadata (separate from video)
            let mut properties = HashMap::new();
            properties.insert("cursor-mode".to_string(), Value::new(2u32));

            session_proxy
                .record_monitor(connector, properties)
                .await
                .context("Failed to record monitor")?
        } else {
            info!("Recording virtual monitor (headless mode)");

            // For virtual monitor, we can specify resolution
            let mut properties = HashMap::new();
            properties.insert("cursor-mode".to_string(), Value::new(2u32));
            // Could add: width, height for virtual monitor

            session_proxy
                .record_virtual(properties)
                .await
                .context("Failed to record virtual monitor")?
        };

        info!("Stream created: {:?}", stream_path);

        // Get stream proxy BEFORE starting (need to subscribe to signal first)
        let stream_proxy =
            MutterScreenCastStream::new(&self.connection, stream_path.clone()).await?;

        // Subscribe to PipeWireStreamAdded signal BEFORE calling Start()
        let mut signal_stream = stream_proxy
            .subscribe_for_node_id()
            .await
            .context("Failed to subscribe to PipeWireStreamAdded signal")?;

        // Start the ScreenCast session (this triggers PipeWireStreamAdded signal)
        session_proxy
            .start()
            .await
            .context("Failed to start ScreenCast session")?;

        info!("Mutter ScreenCast session started successfully");

        // Wait for PipeWireStreamAdded signal with timeout
        use futures_util::stream::StreamExt;
        let node_id =
            match tokio::time::timeout(tokio::time::Duration::from_secs(5), signal_stream.next())
                .await
            {
                Ok(Some(signal)) => {
                    let body = signal.body();
                    let node_id: u32 = body
                        .deserialize()
                        .context("Failed to deserialize PipeWireStreamAdded signal")?;
                    tracing::info!("Received PipeWire node ID {} from signal", node_id);
                    node_id
                }
                Ok(None) => return Err(anyhow::anyhow!("PipeWireStreamAdded signal stream ended")),
                Err(_) => {
                    return Err(anyhow::anyhow!(
                        "Timeout waiting for PipeWireStreamAdded signal (5s)"
                    ))
                }
            };

        let params = stream_proxy
            .parameters()
            .await
            .context("Failed to get stream parameters")?;

        let stream_info = MutterStreamInfo {
            node_id,
            width: params.width.unwrap_or(1920) as u32,
            height: params.height.unwrap_or(1080) as u32,
            position_x: params.position_x.unwrap_or(0),
            position_y: params.position_y.unwrap_or(0),
        };

        // Log dimension source
        if params.width.is_none() || params.height.is_none() {
            info!(
                "Stream dimensions not provided by Mutter, using defaults: {}x{}",
                stream_info.width, stream_info.height
            );
            info!("  Actual dimensions will be obtained from PipeWire stream metadata");
        }

        info!(
            "Stream info: {}x{} at ({}, {}), PipeWire node: {}",
            stream_info.width,
            stream_info.height,
            stream_info.position_x,
            stream_info.position_y,
            stream_info.node_id
        );

        // Create RemoteDesktop session for input injection
        let rd_proxy = MutterRemoteDesktop::new(&self.connection).await?;

        // RemoteDesktop CreateSession takes NO arguments on this GNOME version
        let rd_session_path = rd_proxy
            .create_session()
            .await
            .context("Failed to create Mutter RemoteDesktop session")?;

        info!(
            "Mutter RemoteDesktop session created: {:?}",
            rd_session_path
        );

        // Start RemoteDesktop session
        let rd_session_proxy =
            MutterRemoteDesktopSession::new(&self.connection, rd_session_path.clone()).await?;

        rd_session_proxy
            .start()
            .await
            .context("Failed to start RemoteDesktop session")?;

        info!("Mutter RemoteDesktop session started successfully");

        // Create session handle
        let handle = MutterSessionHandle {
            screencast_session: screencast_session_path,
            remote_desktop_session: rd_session_path,
            streams: vec![stream_path],
            stream_info: vec![stream_info],
            connection: self.connection.clone(),
        };

        info!("Mutter session created successfully (NO DIALOG REQUIRED)");

        Ok(handle)
    }
}

impl MutterSessionHandle {
    /// Get PipeWire node ID for video capture
    ///
    /// This node ID can be used to connect to PipeWire and receive video frames
    pub fn pipewire_node_id(&self) -> u32 {
        self.stream_info.first().map(|s| s.node_id).unwrap_or(0)
    }

    /// Get stream information
    pub fn streams(&self) -> &[MutterStreamInfo] {
        &self.stream_info
    }

    /// Get RemoteDesktop session for input injection
    pub async fn remote_desktop_session(&self) -> Result<MutterRemoteDesktopSession<'_>> {
        MutterRemoteDesktopSession::new(&self.connection, self.remote_desktop_session.clone()).await
    }

    /// Get ScreenCast session
    pub async fn screencast_session(&self) -> Result<MutterScreenCastSession<'_>> {
        MutterScreenCastSession::new(&self.connection, self.screencast_session.clone()).await
    }

    /// Stop all sessions
    pub async fn stop(&self) -> Result<()> {
        info!("Stopping Mutter sessions");

        // Stop ScreenCast
        if let Ok(sc_session) = self.screencast_session().await {
            sc_session.stop().await.ok();
        }

        // Stop RemoteDesktop
        if let Ok(rd_session) = self.remote_desktop_session().await {
            rd_session.stop().await.ok();
        }

        info!("Mutter sessions stopped");

        Ok(())
    }
}

impl Drop for MutterSessionHandle {
    fn drop(&mut self) {
        debug!("MutterSessionHandle dropped - sessions will be cleaned up by Mutter");
        // Mutter automatically cleans up sessions when D-Bus objects are released
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[ignore] // Requires GNOME with Mutter running
    async fn test_mutter_session_creation() {
        match MutterSessionManager::new().await {
            Ok(manager) => {
                println!("Mutter session manager created");

                // Try to create a session (this will work but we need to clean up)
                // Skipped in automated tests
            }
            Err(e) => {
                println!("Mutter not available: {}", e);
            }
        }
    }

    #[tokio::test]
    #[ignore] // Requires GNOME with actual monitor
    async fn test_mutter_monitor_capture() {
        let manager = MutterSessionManager::new()
            .await
            .expect("Mutter not available");

        // This would require knowing actual monitor connectors
        // Skipped in automated tests
    }
}
