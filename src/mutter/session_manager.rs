//! Mutter Session Manager
//!
//! High-level API for creating and managing Mutter ScreenCast + RemoteDesktop sessions.
//! This provides a unified interface similar to PortalManager but using Mutter's
//! direct D-Bus APIs instead of going through the XDG Portal.

use std::{collections::HashMap, sync::Arc};

use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};
use zbus::zvariant::{OwnedObjectPath, Value};

/// How to record a monitor on GNOME.
///
/// Mutter's monitor source copies from the scanout buffer and stops recording
/// entirely when a fullscreen surface takes the display plane
/// (GNOME/mutter#3903). Its area source re-paints the stage into its own
/// framebuffer and is unaffected. The cost of an area stream is that its
/// rectangle and scale are fixed at creation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RecordMode {
    /// Prefer an area stream while the compositor still has the bug.
    #[default]
    Auto,
    /// Always record an area.
    Area,
    /// Always record the monitor, accepting the freeze.
    Monitor,
}

impl RecordMode {
    /// Parse the `capture.gnome_record_mode` setting, defaulting to `Auto` for
    /// anything unrecognised rather than refusing to start a session.
    #[must_use]
    pub fn from_config(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "area" => Self::Area,
            "monitor" => Self::Monitor,
            _ => Self::Auto,
        }
    }

    /// Whether this mode should resolve a stage area and record that.
    ///
    /// `Auto` prefers an area, because every released Mutter still carries
    /// GNOME/mutter#3903 and a monitor stream freezes for the length of any
    /// fullscreen video. The caller narrows this further: an area stands in
    /// for a single monitor, so the multi-monitor case falls back on its own,
    /// as does a compositor that will not tell us the rectangle.
    ///
    /// When mutter!5276 ships, `Auto` should consult the compositor version
    /// and prefer the monitor stream again on versions that carry it, since a
    /// monitor stream follows resolution and layout changes without the
    /// rebuild an area needs.
    #[must_use]
    pub fn prefers_area(self) -> bool {
        matches!(self, Self::Auto | Self::Area)
    }
}

/// What a Mutter session is actually capturing.
///
/// Recorded on the session handle because the answer decides several things
/// later: where the stream's geometry comes from, whether a host monitor
/// change invalidates the stream, and what a re-established session must
/// recreate. Deriving it after the fact from stream parameters is not possible,
/// because an area stream does not report its own size.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CaptureSource {
    /// A monitor by connector. Follows that monitor's mode and layout.
    Monitor { connector: String },
    /// A fixed rectangle of the stage, standing in for a monitor.
    ///
    /// Immune to the direct-scanout freeze (GNOME/mutter#3903) because Mutter's
    /// area source re-paints the stage rather than copying the scanout buffer.
    /// The rectangle does not follow the monitor, so a layout change has to
    /// rebuild the stream: see `MutterSession::area_is_current`.
    Area {
        connector: String,
        area: super::StageArea,
    },
    /// A virtual monitor, for headless operation.
    Virtual,
}

impl CaptureSource {
    /// The connector this source stands for, if any.
    #[must_use]
    pub fn connector(&self) -> Option<&str> {
        match self {
            Self::Monitor { connector } | Self::Area { connector, .. } => Some(connector),
            Self::Virtual => None,
        }
    }

    /// The rectangle a stream was created for, when the source fixes one.
    #[must_use]
    pub fn area(&self) -> Option<super::StageArea> {
        match self {
            Self::Area { area, .. } => Some(*area),
            _ => None,
        }
    }

    /// Short name for logs and the capability surface.
    #[must_use]
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Monitor { .. } => "monitor",
            Self::Area { .. } => "area",
            Self::Virtual => "virtual",
        }
    }
}

use super::{
    clipboard::MutterClipboard,
    remote_desktop::{MutterRemoteDesktop, MutterRemoteDesktopSession},
    screencast::{MutterScreenCast, MutterScreenCastSession, MutterScreenCastStream},
};

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
    /// Connection (kept alive for session, crate-internal for input injection)
    pub(crate) connection: zbus::Connection,
    /// Mutter clipboard manager (available when clipboard is enabled)
    pub clipboard: Option<Arc<MutterClipboard>>,
    /// What this session is capturing, and for an area, the rectangle it was
    /// created for.
    pub capture_source: CaptureSource,
}

/// Mutter Session Manager
///
/// Manages Mutter ScreenCast and RemoteDesktop sessions without portal dialogs.
/// This is GNOME-specific and requires non-sandboxed D-Bus access.
pub struct MutterSession {
    connection: zbus::Connection,
}

impl MutterSession {
    /// Create a new Mutter session manager
    ///
    /// # Returns
    ///
    /// Manager if Mutter APIs are available
    pub async fn new() -> Result<Self> {
        info!("Initializing Mutter session manager");

        let connection = zbus::Connection::session()
            .await
            .context("Failed to connect to D-Bus session")?;

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
    /// Session linkage order is critical: RemoteDesktop must be created first,
    /// then its SessionId is passed to ScreenCast.CreateSession so that input
    /// injection targets the correct screencast stream.
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
        record_mode: RecordMode,
        virtual_is_platform: bool,
    ) -> Result<MutterSessionHandle> {
        info!("Creating Mutter session (RemoteDesktop first, then linked ScreenCast)");

        // Step 1: Create RemoteDesktop session first
        let rd_proxy = MutterRemoteDesktop::new(&self.connection).await?;

        let rd_session_path = rd_proxy
            .create_session()
            .await
            .context("Failed to create Mutter RemoteDesktop session")?;

        info!(
            "Mutter RemoteDesktop session created: {:?}",
            rd_session_path
        );

        let rd_session_proxy =
            MutterRemoteDesktopSession::new(&self.connection, rd_session_path.clone()).await?;

        // Read the SessionId property from the RemoteDesktop session
        let rd_session_id = rd_session_proxy
            .session_id()
            .await
            .context("Failed to read RemoteDesktop SessionId property")?;

        info!("RemoteDesktop session ID: {}", rd_session_id);

        // Step 2: Create ScreenCast session linked to the RemoteDesktop session
        let screencast_proxy = MutterScreenCast::new(&self.connection).await?;

        let mut sc_properties = HashMap::new();
        sc_properties.insert(
            "remote-desktop-session-id".to_string(),
            Value::new(rd_session_id),
        );

        let screencast_session_path = screencast_proxy
            .create_session(sc_properties)
            .await
            .context("Failed to create linked Mutter ScreenCast session")?;

        info!(
            "Mutter ScreenCast session created (linked to RD): {:?}",
            screencast_session_path
        );

        let session_proxy =
            MutterScreenCastSession::new(&self.connection, screencast_session_path.clone()).await?;

        // Step 3: Set up recording source
        // Remembered because an area stream does not report its own size the
        // way a monitor stream does, and we already know it: see below.
        // The chosen source is carried on the handle: an area stream does not
        // report its own size, a host layout change invalidates it, and a
        // re-established session has to recreate the same thing.
        let mut capture_source = CaptureSource::Virtual;
        let stream_path = if let Some(connector) = monitor_connector {
            // Cursor mode: 2 = metadata (separate from video)
            let mut properties = HashMap::new();
            properties.insert("cursor-mode".to_string(), Value::new(2u32));

            // An area stream keeps producing frames while a fullscreen surface
            // is in direct scanout, where a monitor stream stops dead
            // (GNOME/mutter#3903). Resolving the connector's rectangle is what
            // buys that, so a failure to resolve it falls back rather than
            // failing the session.
            // An area stands in for exactly one monitor. With more than one
            // logical monitor the compositor is laying out a desktop an area
            // cannot represent, so recognise that here rather than capturing
            // one screen and calling it the session.
            let single_monitor = match crate::mutter::logical_monitor_count(&self.connection).await
            {
                Ok(1) => true,
                Ok(n) => {
                    info!(
                        "{n} logical monitors present: recording the monitor, since an area stands in for one"
                    );
                    false
                }
                Err(e) => {
                    warn!("Could not count logical monitors ({e:#}); recording the monitor");
                    false
                }
            };

            let area = if record_mode.prefers_area() && single_monitor {
                match crate::mutter::stage_area_for_connector(&self.connection, Some(connector))
                    .await
                {
                    Ok(area) => Some(area),
                    Err(e) => {
                        warn!(
                            "Could not resolve a stage area for {connector} ({e:#}); recording the monitor instead"
                        );
                        None
                    }
                }
            } else {
                None
            };

            capture_source = match area {
                Some(area) => CaptureSource::Area {
                    connector: connector.to_owned(),
                    area,
                },
                None => CaptureSource::Monitor {
                    connector: connector.to_owned(),
                },
            };
            match area {
                Some(area) => {
                    info!(
                        "Recording area {}x{} at ({},{}) for monitor {} (immune to the direct-scanout freeze, GNOME/mutter#3903)",
                        area.width, area.height, area.x, area.y, connector
                    );
                    session_proxy
                        .record_area(area.x, area.y, area.width, area.height, properties)
                        .await
                        .context("Failed to record area")?
                }
                None => {
                    info!("Recording monitor: {}", connector);
                    session_proxy
                        .record_monitor(connector, properties)
                        .await
                        .context("Failed to record monitor")?
                }
            }
        } else {
            info!("Recording virtual monitor (headless mode)");

            let mut properties = HashMap::new();
            properties.insert("cursor-mode".to_string(), Value::new(2u32));

            // Mutter documents is-platform as meaning the output is not treated
            // as a shared screen but "as if it was a real monitor", which is
            // what a headless session's only display actually is. Off by
            // default because it changes how the compositor presents the
            // session. Mutter's handler looks up only the keys it knows, so a
            // build predating the property ignores it rather than failing.
            if virtual_is_platform {
                info!("Marking the virtual monitor as part of the platform (is-platform)");
                properties.insert("is-platform".to_string(), Value::new(true));
            }

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

        // Step 4: Start the RemoteDesktop session (which also starts linked ScreenCast)
        rd_session_proxy
            .start()
            .await
            .context("Failed to start RemoteDesktop session")?;

        info!("Mutter RemoteDesktop session started (linked ScreenCast also active)");

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
                    ));
                }
            };

        let params = stream_proxy
            .parameters()
            .await
            .context("Failed to get stream parameters")?;

        // A dimension of zero is not a size, and advertising a 0x0 desktop makes
        // a strict client abandon the connection during finalize rather than
        // fail loudly. Mutter's area stream reports exactly that, where its
        // monitor stream reports the real thing, so treat zero as absent.
        let reported = |v: Option<i32>| v.filter(|n| *n > 0);
        let (fallback_w, fallback_h) = capture_source
            .area()
            .map_or((1920, 1080), |area| (area.width, area.height));

        let stream_info = MutterStreamInfo {
            node_id,
            width: reported(params.width).unwrap_or(fallback_w) as u32,
            height: reported(params.height).unwrap_or(fallback_h) as u32,
            position_x: params
                .position_x
                .unwrap_or_else(|| capture_source.area().map_or(0, |area| area.x)),
            position_y: params
                .position_y
                .unwrap_or_else(|| capture_source.area().map_or(0, |area| area.y)),
        };

        if reported(params.width).is_none() || reported(params.height).is_none() {
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

        // Step 5: Try to enable clipboard
        let clipboard = {
            let mgr = MutterClipboard::new(self.connection.clone(), rd_session_path.clone());
            match mgr.enable().await {
                Ok(()) => {
                    info!("Mutter clipboard enabled");
                    Some(Arc::new(mgr))
                }
                Err(e) => {
                    info!("Mutter clipboard not available: {}", e);
                    None
                }
            }
        };

        // EIS input is established lazily by the Mutter Direct strategy on the
        // first client connection (and reconnected on socket death). Opening it
        // here, before any client, let the compositor's idle timeout close it.

        let handle = MutterSessionHandle {
            screencast_session: screencast_session_path,
            remote_desktop_session: rd_session_path,
            streams: vec![stream_path],
            stream_info: vec![stream_info],
            connection: self.connection.clone(),
            clipboard,
            capture_source,
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
        self.stream_info.first().map_or(0, |s| s.node_id)
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

        // #57: do NOT swallow Stop failures. If Mutter's Stop fails, the session
        // is not torn down and accumulates in the daemon across reconnects, a
        // suspected cause of the eventual permanent zero-frame capture failure.
        if let Ok(sc_session) = self.screencast_session().await
            && let Err(e) = sc_session.stop().await
        {
            warn!("Mutter ScreenCast Stop failed (session may leak in the daemon): {e:#}");
        }

        if let Ok(rd_session) = self.remote_desktop_session().await
            && let Err(e) = rd_session.stop().await
        {
            warn!("Mutter RemoteDesktop Stop failed (session may leak in the daemon): {e:#}");
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
    #[ignore = "Requires GNOME with Mutter running"]
    async fn test_mutter_session_creation() {
        match MutterSession::new().await {
            Ok(_manager) => {
                println!("Mutter session manager created");

                // Try to create a session (this will work but we need to clean up)
                // Skipped in automated tests
            }
            Err(e) => {
                println!("Mutter not available: {e}");
            }
        }
    }

    #[tokio::test]
    #[ignore = "Requires GNOME with actual monitor"]
    async fn test_mutter_monitor_capture() {
        let _manager = MutterSession::new().await.expect("Mutter not available");

        // This would require knowing actual monitor connectors
        // Skipped in automated tests
    }
}
