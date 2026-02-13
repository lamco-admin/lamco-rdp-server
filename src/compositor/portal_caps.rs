//! Portal capability detection
//!
//! This module probes XDG Desktop Portal features to determine
//! available capabilities for screen capture, input injection,
//! and clipboard access.

use anyhow::{Context, Result};
use ashpd::desktop::screencast::{CursorMode as AshpdCursorMode, SourceType as AshpdSourceType};
use tracing::{debug, warn};
use zbus::Connection;

/// Cursor rendering mode
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorMode {
    /// Cursor is hidden from capture
    Hidden,
    /// Cursor is embedded in video frames
    Embedded,
    /// Cursor metadata sent separately
    Metadata,
}

impl From<AshpdCursorMode> for CursorMode {
    fn from(mode: AshpdCursorMode) -> Self {
        match mode {
            AshpdCursorMode::Hidden => Self::Hidden,
            AshpdCursorMode::Embedded => Self::Embedded,
            AshpdCursorMode::Metadata => Self::Metadata,
        }
    }
}

/// Source type for screen capture
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceType {
    /// Capture entire monitor
    Monitor,
    /// Capture single window
    Window,
    /// Capture virtual screen (for multi-monitor)
    Virtual,
}

impl From<AshpdSourceType> for SourceType {
    fn from(source: AshpdSourceType) -> Self {
        match source {
            AshpdSourceType::Monitor => Self::Monitor,
            AshpdSourceType::Window => Self::Window,
            AshpdSourceType::Virtual => Self::Virtual,
        }
    }
}

/// Portal capability information
#[derive(Debug, Clone, Default)]
pub struct PortalCapabilities {
    /// Portal interface version
    pub version: u32,

    /// ScreenCast portal available
    pub supports_screencast: bool,

    /// RemoteDesktop portal available
    pub supports_remote_desktop: bool,

    /// Clipboard portal available
    pub supports_clipboard: bool,

    /// Available cursor modes
    pub available_cursor_modes: Vec<CursorMode>,

    /// Available source types
    pub available_source_types: Vec<SourceType>,

    /// Portal backend name (gnome, kde, wlr, etc.)
    pub backend: Option<String>,

    // === Phase 2: Session Persistence ===
    /// Portal version supports restore tokens (v4+)
    pub supports_restore_tokens: bool,

    /// Maximum persist mode available (0=none, 1=transient, 2=permanent)
    pub max_persist_mode: u8,
}

impl PortalCapabilities {
    /// Probe all Portal capabilities
    ///
    /// This connects to the D-Bus session bus and queries the
    /// XDG Desktop Portal for available features.
    pub async fn probe() -> Result<Self> {
        debug!("Probing Portal capabilities...");

        let connection = Connection::session()
            .await
            .context("Failed to connect to D-Bus session bus")?;

        let mut caps = Self::default();

        // Probe ScreenCast portal
        caps.probe_screencast(&connection).await;

        // Probe RemoteDesktop portal
        caps.probe_remote_desktop(&connection).await;

        // Probe Clipboard portal
        caps.probe_clipboard(&connection).await;

        // Detect portal backend
        caps.detect_backend(&connection).await;

        // Probe restore token support (Phase 2)
        caps.probe_persistence_support();

        debug!("Portal probing complete: {:?}", caps);
        Ok(caps)
    }

    async fn probe_screencast(&mut self, connection: &Connection) {
        // Query ScreenCast portal interface
        match query_portal_property::<u32>(
            connection,
            "org.freedesktop.portal.ScreenCast",
            "version",
        )
        .await
        {
            Ok(version) => {
                self.version = version;
                self.supports_screencast = true;
                debug!("ScreenCast portal version: {}", version);

                // Query available source types
                if let Ok(source_types) = query_portal_property::<u32>(
                    connection,
                    "org.freedesktop.portal.ScreenCast",
                    "AvailableSourceTypes",
                )
                .await
                {
                    self.parse_source_types(source_types);
                }

                // Query available cursor modes
                if let Ok(cursor_modes) = query_portal_property::<u32>(
                    connection,
                    "org.freedesktop.portal.ScreenCast",
                    "AvailableCursorModes",
                )
                .await
                {
                    self.parse_cursor_modes(cursor_modes);
                }
            }
            Err(e) => {
                warn!("ScreenCast portal not available: {}", e);
                self.supports_screencast = false;
            }
        }
    }

    async fn probe_remote_desktop(&mut self, connection: &Connection) {
        match query_portal_property::<u32>(
            connection,
            "org.freedesktop.portal.RemoteDesktop",
            "version",
        )
        .await
        {
            Ok(version) => {
                self.supports_remote_desktop = true;
                debug!("RemoteDesktop portal version: {}", version);
            }
            Err(e) => {
                warn!("RemoteDesktop portal not available: {}", e);
                self.supports_remote_desktop = false;
            }
        }
    }

    async fn probe_clipboard(&mut self, connection: &Connection) {
        // Clipboard is part of RemoteDesktop portal (requires version >= 2)
        // Check if RemoteDesktop supports clipboard by checking version
        match query_portal_property::<u32>(
            connection,
            "org.freedesktop.portal.RemoteDesktop",
            "version",
        )
        .await
        {
            Ok(version) if version >= 2 => {
                self.supports_clipboard = true;
                debug!("Clipboard support available (RemoteDesktop v{})", version);
            }
            Ok(version) => {
                self.supports_clipboard = false;
                debug!("Clipboard not available (RemoteDesktop v{} < 2)", version);
            }
            Err(_) => {
                self.supports_clipboard = false;
            }
        }
    }

    async fn detect_backend(&mut self, connection: &Connection) {
        // Try to detect the portal backend from D-Bus
        // Different backends register different interfaces

        // Check for GNOME backend
        if portal_interface_exists(connection, "org.gnome.Mutter.ScreenCast").await {
            self.backend = Some("gnome".to_string());
            return;
        }

        // Check for KDE backend
        if portal_interface_exists(connection, "org.kde.KWin.ScreenShot2").await {
            self.backend = Some("kde".to_string());
            return;
        }

        // Check for wlr backend (wlroots-based)
        if portal_interface_exists(connection, "org.freedesktop.impl.portal.ScreenCast").await {
            // Generic portal - could be wlr or gtk
            self.backend = Some("generic".to_string());
        }
    }

    fn parse_source_types(&mut self, flags: u32) {
        self.available_source_types.clear();

        // SourceType flags from xdg-desktop-portal spec
        const MONITOR: u32 = 1;
        const WINDOW: u32 = 2;
        const VIRTUAL: u32 = 4;

        if flags & MONITOR != 0 {
            self.available_source_types.push(SourceType::Monitor);
        }
        if flags & WINDOW != 0 {
            self.available_source_types.push(SourceType::Window);
        }
        if flags & VIRTUAL != 0 {
            self.available_source_types.push(SourceType::Virtual);
        }
    }

    fn parse_cursor_modes(&mut self, flags: u32) {
        self.available_cursor_modes.clear();

        // CursorMode flags from xdg-desktop-portal spec
        const HIDDEN: u32 = 1;
        const EMBEDDED: u32 = 2;
        const METADATA: u32 = 4;

        if flags & HIDDEN != 0 {
            self.available_cursor_modes.push(CursorMode::Hidden);
        }
        if flags & EMBEDDED != 0 {
            self.available_cursor_modes.push(CursorMode::Embedded);
        }
        if flags & METADATA != 0 {
            self.available_cursor_modes.push(CursorMode::Metadata);
        }
    }

    /// Check if metadata cursor mode is available (best for RDP)
    pub fn supports_metadata_cursor(&self) -> bool {
        self.available_cursor_modes.contains(&CursorMode::Metadata)
    }

    /// Check if monitor capture is available
    pub fn supports_monitor_capture(&self) -> bool {
        self.available_source_types.contains(&SourceType::Monitor)
    }

    /// Check if window capture is available
    pub fn supports_window_capture(&self) -> bool {
        self.available_source_types.contains(&SourceType::Window)
    }

    /// Probe restore token support (Phase 2)
    ///
    /// Portal v4+ supports restore tokens for session persistence.
    /// This method sets the supports_restore_tokens and max_persist_mode fields.
    fn probe_persistence_support(&mut self) {
        // Check portal version
        if self.version >= 4 {
            self.supports_restore_tokens = true;
            self.max_persist_mode = 2; // ExplicitlyRevoked mode available
            debug!(
                "Portal v{} supports restore tokens (max persist mode: 2)",
                self.version
            );
        } else if self.version > 0 {
            self.supports_restore_tokens = false;
            self.max_persist_mode = 0;
            debug!("Portal v{} does not support restore tokens", self.version);
        } else {
            // Version unknown or portal unavailable
            self.supports_restore_tokens = false;
            self.max_persist_mode = 0;
            debug!("Portal version unknown, assuming no restore token support");
        }
    }
}

/// Query a D-Bus property from the Portal
async fn query_portal_property<T>(
    connection: &Connection,
    interface: &str,
    property: &str,
) -> Result<T>
where
    T: TryFrom<zbus::zvariant::OwnedValue>,
    T::Error: std::error::Error + Send + Sync + 'static,
{
    use zbus::names::InterfaceName;

    let proxy = zbus::fdo::PropertiesProxy::builder(connection)
        .destination("org.freedesktop.portal.Desktop")?
        .path("/org/freedesktop/portal/desktop")?
        .build()
        .await?;

    let interface_name = InterfaceName::try_from(interface)
        .map_err(|e| anyhow::anyhow!("Invalid interface name: {e}"))?;

    let value = proxy.get(interface_name, property).await?;
    T::try_from(value).map_err(|e| anyhow::anyhow!("Property conversion failed: {e}"))
}

/// Check if a D-Bus interface exists
async fn portal_interface_exists(connection: &Connection, interface: &str) -> bool {
    let result: Result<u32> = query_portal_property(connection, interface, "version").await;
    result.is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_source_types() {
        let mut caps = PortalCapabilities::default();

        // Monitor + Window
        caps.parse_source_types(3);
        assert_eq!(caps.available_source_types.len(), 2);
        assert!(caps.supports_monitor_capture());
        assert!(caps.supports_window_capture());

        // All types
        caps.parse_source_types(7);
        assert_eq!(caps.available_source_types.len(), 3);
    }

    #[test]
    fn test_parse_cursor_modes() {
        let mut caps = PortalCapabilities::default();

        // Embedded + Metadata
        caps.parse_cursor_modes(6);
        assert_eq!(caps.available_cursor_modes.len(), 2);
        assert!(caps.supports_metadata_cursor());

        // All modes
        caps.parse_cursor_modes(7);
        assert_eq!(caps.available_cursor_modes.len(), 3);
    }

    #[test]
    fn test_default_caps() {
        let caps = PortalCapabilities::default();
        assert_eq!(caps.version, 0);
        assert!(!caps.supports_screencast);
        assert!(!caps.supports_remote_desktop);
        assert!(!caps.supports_clipboard);
    }
}
