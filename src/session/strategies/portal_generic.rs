//! Embedded portal-generic Strategy: Native wlroots Video + Input + Clipboard
//!
//! This strategy uses the `xdg-desktop-portal-generic` crate as a library to
//! provide full screen capture, input injection, and clipboard support for
//! wlroots-based compositors without requiring an external portal daemon.
//!
//! # Protocols Used
//!
//! - **Capture**: ext-image-copy-capture-v1 or wlr-screencopy-v1
//! - **Input**: wlr-virtual-pointer + zwp-virtual-keyboard (or EIS bridge)
//! - **Clipboard**: ext-data-control-v1 or wlr-data-control-v1
//!
//! # Architecture
//!
//! ```text
//! PortalGenericStrategy
//!   ├─> WaylandConnection (global registry scan)
//!   ├─> PipeWireManager (frame delivery pipeline)
//!   └─> PortalGenericSessionHandle
//!       ├─> CaptureBackend → PipeWire streams (node IDs)
//!       ├─> InputBackend → virtual keyboard/pointer injection
//!       └─> ClipboardBackend → data-control read/write
//! ```
//!
//! # Advantages Over wlr-direct
//!
//! - Provides video capture (wlr-direct is input-only)
//! - Provides clipboard support via data-control protocols
//! - Single unified strategy instead of compositing Portal ScreenCast + wlr input
//!
//! # Limitations
//!
//! - Not Flatpak-compatible (requires direct Wayland socket access)
//! - Requires PipeWire running on the host

use std::{
    sync::{atomic::AtomicBool, Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result};
use async_trait::async_trait;
use tracing::{debug, info, warn};
use xdg_desktop_portal_generic::{
    pipewire::PipeWireManager,
    services::{
        capture::create_capture_backend,
        clipboard::create_clipboard_backend,
        input::{create_input_backend, InputBackendConfig},
    },
    types::{CursorMode, DeviceTypes, SourceType},
    wayland::WaylandConnection,
    InputBackend, InputEvent, KeyState, KeyboardEvent, PointerEvent,
};

use crate::{
    health::{HealthEvent, HealthReporter},
    session::strategy::{
        ClipboardComponents, PipeWireAccess, SessionHandle, SessionStrategy, SessionType,
        StreamInfo,
    },
};

/// Session strategy using embedded portal-generic backends.
///
/// Connects directly to the Wayland compositor as a client and provides
/// video capture, input injection, and clipboard via native protocols.
pub struct PortalGenericStrategy;

impl PortalGenericStrategy {
    pub fn new() -> Self {
        Self
    }

    /// Check if the compositor supports the required protocols.
    ///
    /// Tries to connect to Wayland and verifies that at least one capture
    /// protocol and one input protocol are available.
    pub async fn is_available() -> bool {
        // WaylandConnection::connect() is synchronous; run on blocking thread
        let result = tokio::task::spawn_blocking(|| {
            let conn = WaylandConnection::connect().ok()?;
            let protocols = conn.available_protocols();

            // Need at least one capture protocol
            let has_capture = protocols.ext_image_copy_capture || protocols.wlr_screencopy;
            // Need input protocols (virtual pointer + virtual keyboard)
            let has_input = protocols.wlr_virtual_pointer && protocols.zwp_virtual_keyboard;

            if has_capture && has_input {
                Some(())
            } else {
                debug!(
                    "[portal-generic] Missing protocols: capture={}, input={}",
                    has_capture, has_input
                );
                None
            }
        })
        .await;

        matches!(result, Ok(Some(())))
    }
}

impl Default for PortalGenericStrategy {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl SessionStrategy for PortalGenericStrategy {
    fn name(&self) -> &'static str {
        "portal-generic"
    }

    fn requires_initial_setup(&self) -> bool {
        // Direct protocol access, no user dialog
        false
    }

    fn supports_unattended_restore(&self) -> bool {
        // Always available when Wayland socket is accessible
        true
    }

    async fn create_session(&self) -> Result<Arc<dyn SessionHandle>> {
        info!("portal-generic: Creating session with embedded portal backend");

        // All Wayland and PipeWire setup is synchronous; run on blocking thread
        let (handle, wayland_stop) = tokio::task::spawn_blocking(|| -> Result<_> {
            // Connect to compositor and discover protocols
            let wayland = WaylandConnection::connect()
                .context("Failed to connect to Wayland display")?;
            let protocols = wayland.available_protocols().clone();
            let sources = wayland.state().get_sources();

            info!(
                "portal-generic: Connected, {} outputs, capture={}/{}  input={}/{}  clipboard={}/{}",
                sources.len(),
                if protocols.ext_image_copy_capture { "ext" } else { "-" },
                if protocols.wlr_screencopy { "wlr" } else { "-" },
                if protocols.wlr_virtual_pointer { "ptr" } else { "-" },
                if protocols.zwp_virtual_keyboard { "kbd" } else { "-" },
                if protocols.ext_data_control { "ext" } else { "-" },
                if protocols.wlr_data_control { "wlr" } else { "-" },
            );

            // Start PipeWire for frame delivery
            let pipewire_manager = Arc::new(PipeWireManager::start()
                .context("Failed to start PipeWire manager")?);

            // Spawn the Wayland event loop on a dedicated thread
            let (
                wayland_stop,
                _shared_wayland_state,
                capture_tx,
                clipboard_tx,
                shared_clipboard,
                _wayland_thread,
            ) = wayland.spawn_event_loop(Arc::clone(&pipewire_manager));

            // Create input backend
            let input_config = InputBackendConfig::from_env();
            let mut input_backend = create_input_backend(&input_config, &protocols)
                .map_err(|e| anyhow::anyhow!("Input backend: {e}"))?;

            // Create a default input context for this session
            let session_id = format!("lamco-rdp-{}", uuid::Uuid::new_v4());
            let devices = DeviceTypes {
                keyboard: true,
                pointer: true,
                touchscreen: false,
            };
            input_backend.create_context(&session_id, devices)
                .map_err(|e| anyhow::anyhow!("Input context: {e}"))?;

            // Create capture backend and start capture session
            let mut capture_backend = create_capture_backend(
                &protocols,
                sources,
                Arc::clone(&pipewire_manager),
                capture_tx,
            ).map_err(|e| anyhow::anyhow!("Capture backend: {e}"))?;

            // Request monitor capture with embedded cursor
            let capture_sources = capture_backend
                .get_sources(&[SourceType::Monitor])
                .map_err(|e| anyhow::anyhow!("Get sources: {e}"))?;

            let stream_infos = if capture_sources.is_empty() {
                warn!("portal-generic: No capturable sources found");
                vec![]
            } else {
                capture_backend
                    .create_capture_session(&capture_sources, CursorMode::Embedded)
                    .map_err(|e| anyhow::anyhow!("Create capture session: {e}"))?
            };

            // Convert portal-generic StreamInfo to our StreamInfo
            let streams: Vec<StreamInfo> = stream_infos
                .iter()
                .map(|s| StreamInfo {
                    node_id: s.node_id,
                    width: s.size.0,
                    height: s.size.1,
                    position_x: s.position.0,
                    position_y: s.position.1,
                })
                .collect();

            info!(
                "portal-generic: {} capture stream(s) created",
                streams.len()
            );
            for stream in &streams {
                info!(
                    "  Stream node_id={} {}x{} at ({},{})",
                    stream.node_id, stream.width, stream.height,
                    stream.position_x, stream.position_y
                );
            }

            // Create clipboard backend (optional, may not be available)
            let clipboard_backend = create_clipboard_backend(
                &protocols,
                clipboard_tx,
                shared_clipboard,
            );

            if clipboard_backend.is_some() {
                info!("portal-generic: Clipboard backend active");
            } else {
                warn!("portal-generic: No clipboard protocol available");
            }

            let handle = PortalGenericSessionHandle {
                session_id,
                input_backend: Arc::new(Mutex::new(input_backend)),
                _capture_backend: Arc::new(Mutex::new(capture_backend)),
                clipboard_backend: clipboard_backend.map(|cb| Arc::new(Mutex::new(cb))),
                _pipewire_manager: pipewire_manager,
                streams,
            };

            Ok((handle, wayland_stop))
        })
        .await
        .context("portal-generic: Setup task panicked")??;

        // Store the stop signal so we can clean up later
        // (The Arc<AtomicBool> keeps the Wayland event loop alive)
        let session = Arc::new(PortalGenericSessionWithStop {
            handle,
            _wayland_stop: wayland_stop,
            health_reporter: std::sync::OnceLock::new(),
        });

        Ok(session)
    }

    async fn cleanup(&self, _session: &dyn SessionHandle) -> Result<()> {
        info!("portal-generic: Session cleanup");
        // Resources are cleaned up on drop:
        // - Wayland event loop stopped via AtomicBool
        // - PipeWire streams destroyed
        // - Virtual devices released
        Ok(())
    }
}

/// Wrapper that owns the Wayland stop signal alongside the session handle.
struct PortalGenericSessionWithStop {
    handle: PortalGenericSessionHandle,
    _wayland_stop: Arc<AtomicBool>,
    health_reporter: std::sync::OnceLock<HealthReporter>,
}

impl Drop for PortalGenericSessionWithStop {
    fn drop(&mut self) {
        // Signal the Wayland event loop to stop
        self._wayland_stop
            .store(true, std::sync::atomic::Ordering::Relaxed);
        debug!("portal-generic: Wayland event loop stop signaled");
        if let Some(r) = self.health_reporter.get() {
            r.report(HealthEvent::SessionClosed {
                reason: "portal-generic session dropped".into(),
            });
        }
    }
}

#[async_trait]
impl SessionHandle for PortalGenericSessionWithStop {
    fn set_health_reporter(&self, reporter: HealthReporter) {
        let _ = self.health_reporter.set(reporter);
    }

    fn pipewire_access(&self) -> PipeWireAccess {
        self.handle.pipewire_access()
    }

    fn streams(&self) -> Vec<StreamInfo> {
        self.handle.streams()
    }

    fn session_type(&self) -> SessionType {
        self.handle.session_type()
    }

    async fn notify_keyboard_keycode(&self, keycode: i32, pressed: bool) -> Result<()> {
        self.handle.notify_keyboard_keycode(keycode, pressed).await
    }

    async fn notify_pointer_motion_absolute(&self, stream_id: u32, x: f64, y: f64) -> Result<()> {
        self.handle
            .notify_pointer_motion_absolute(stream_id, x, y)
            .await
    }

    async fn notify_pointer_button(&self, button: i32, pressed: bool) -> Result<()> {
        self.handle.notify_pointer_button(button, pressed).await
    }

    async fn notify_pointer_axis(&self, dx: f64, dy: f64) -> Result<()> {
        self.handle.notify_pointer_axis(dx, dy).await
    }

    fn portal_clipboard(&self) -> Option<ClipboardComponents> {
        self.handle.portal_clipboard()
    }

    fn clipboard_backend(
        &self,
    ) -> Option<Arc<Mutex<Box<dyn xdg_desktop_portal_generic::ClipboardBackend>>>> {
        self.handle.clipboard_backend()
    }
}

/// Session handle for the embedded portal-generic backend.
///
/// Bridges portal-generic's backend traits to the SessionHandle interface
/// expected by the RDP server's session management layer.
pub struct PortalGenericSessionHandle {
    session_id: String,
    input_backend: Arc<Mutex<Box<dyn InputBackend>>>,
    _capture_backend: Arc<Mutex<Box<dyn xdg_desktop_portal_generic::CaptureBackend>>>,
    clipboard_backend: Option<Arc<Mutex<Box<dyn xdg_desktop_portal_generic::ClipboardBackend>>>>,
    _pipewire_manager: Arc<PipeWireManager>,
    streams: Vec<StreamInfo>,
}

/// Get current time in microseconds for event timestamps.
fn current_time_usec() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros() as u64
}

#[async_trait]
impl SessionHandle for PortalGenericSessionHandle {
    fn pipewire_access(&self) -> PipeWireAccess {
        // Use the first stream's node ID for PipeWire access
        if let Some(stream) = self.streams.first() {
            PipeWireAccess::NodeId(stream.node_id)
        } else {
            warn!("portal-generic: No streams available for PipeWire access");
            PipeWireAccess::NodeId(0)
        }
    }

    fn streams(&self) -> Vec<StreamInfo> {
        self.streams.clone()
    }

    fn session_type(&self) -> SessionType {
        SessionType::PortalGeneric
    }

    async fn notify_keyboard_keycode(&self, keycode: i32, pressed: bool) -> Result<()> {
        let event = InputEvent::Keyboard(KeyboardEvent {
            keycode: keycode as u32,
            state: if pressed {
                KeyState::Pressed
            } else {
                KeyState::Released
            },
            time_usec: current_time_usec(),
        });

        let mut backend = self
            .input_backend
            .lock()
            .map_err(|e| anyhow::anyhow!("Input backend lock poisoned: {e}"))?;
        backend
            .inject_event(&self.session_id, event)
            .map_err(|e| anyhow::anyhow!("Keyboard inject: {e}"))?;

        Ok(())
    }

    async fn notify_pointer_motion_absolute(&self, stream_id: u32, x: f64, y: f64) -> Result<()> {
        let event = InputEvent::Pointer(PointerEvent::MotionAbsolute {
            x,
            y,
            stream: stream_id,
            time_usec: current_time_usec(),
        });

        let mut backend = self
            .input_backend
            .lock()
            .map_err(|e| anyhow::anyhow!("Input backend lock poisoned: {e}"))?;
        backend
            .inject_event(&self.session_id, event)
            .map_err(|e| anyhow::anyhow!("Pointer motion inject: {e}"))?;

        Ok(())
    }

    async fn notify_pointer_button(&self, button: i32, pressed: bool) -> Result<()> {
        let event = InputEvent::Pointer(PointerEvent::Button {
            button: button as u32,
            state: if pressed {
                xdg_desktop_portal_generic::ButtonState::Pressed
            } else {
                xdg_desktop_portal_generic::ButtonState::Released
            },
            time_usec: current_time_usec(),
        });

        let mut backend = self
            .input_backend
            .lock()
            .map_err(|e| anyhow::anyhow!("Input backend lock poisoned: {e}"))?;
        backend
            .inject_event(&self.session_id, event)
            .map_err(|e| anyhow::anyhow!("Pointer button inject: {e}"))?;

        Ok(())
    }

    async fn notify_pointer_axis(&self, dx: f64, dy: f64) -> Result<()> {
        let event = InputEvent::Pointer(PointerEvent::Scroll {
            dx,
            dy,
            time_usec: current_time_usec(),
        });

        let mut backend = self
            .input_backend
            .lock()
            .map_err(|e| anyhow::anyhow!("Input backend lock poisoned: {e}"))?;
        backend
            .inject_event(&self.session_id, event)
            .map_err(|e| anyhow::anyhow!("Pointer axis inject: {e}"))?;

        Ok(())
    }

    fn portal_clipboard(&self) -> Option<ClipboardComponents> {
        // portal-generic uses Wayland data-control, not Portal D-Bus clipboard
        None
    }

    fn clipboard_backend(
        &self,
    ) -> Option<Arc<Mutex<Box<dyn xdg_desktop_portal_generic::ClipboardBackend>>>> {
        self.clipboard_backend.as_ref().map(Arc::clone)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_current_time_usec() {
        let time = current_time_usec();
        assert!(time > 0);
    }

    #[test]
    fn test_strategy_name() {
        let strategy = PortalGenericStrategy::new();
        assert_eq!(strategy.name(), "portal-generic");
        assert!(!strategy.requires_initial_setup());
        assert!(strategy.supports_unattended_restore());
    }
}
