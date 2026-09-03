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
    sync::{Arc, Mutex, atomic::AtomicBool},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result};
use async_trait::async_trait;
use tracing::{debug, error, info, warn};
use xdg_desktop_portal_generic::{
    CaptureProtocol, InputBackend, InputEvent, KeyState, KeyboardEvent, PointerEvent,
    health::{self as portal_health, PortalHealthEvent},
    pipewire::PipeWireManager,
    services::{
        capture::{CapturePreference, create_capture_backend},
        clipboard::{ClipboardPreference, create_clipboard_backend},
        input::{InputBackendConfig, create_input_backend},
    },
    types::{CursorMode, DeviceTypes, SourceType},
    wayland::WaylandConnection,
};

use crate::{
    compositor::{
        CaptureBackend as ProfileCaptureBackend, CompositorProfile, Quirk, identify_compositor,
    },
    health::{HealthEvent, HealthReporter},
    session::strategy::{PipeWireAccess, SessionHandle, SessionStrategy, SessionType, StreamInfo},
};

/// Map a wl_shm pixel format (as delivered on the wlr-screencopy `buffer`
/// event) to its in-memory channel order. wl_shm names describe the 32-bit
/// little-endian word, so `Xrgb8888` stores as [B,G,R,X] (BGRx) and `Xbgr8888`
/// stores as [R,G,B,X] (RGBx). Treating an Xbgr8888 buffer as BGRx swaps R and
/// B — the blue→brown skew on sway/wlroots, where virtio-backed compositors
/// hand back Xbgr8888 rather than the Xrgb8888 that KDE/GNOME portals negotiate.
fn wl_shm_format_to_pixel_format(raw: u32) -> Option<lamco_pipewire::PixelFormat> {
    use lamco_pipewire::PixelFormat;
    use wayland_client::protocol::wl_shm::Format;

    match Format::try_from(raw).ok()? {
        Format::Argb8888 => Some(PixelFormat::BGRA),
        Format::Xrgb8888 => Some(PixelFormat::BGRx),
        Format::Abgr8888 => Some(PixelFormat::RGBA),
        Format::Xbgr8888 => Some(PixelFormat::RGBx),
        _ => None,
    }
}

/// Swap the red and blue bytes of every 32-bit pixel in place, converting
/// between RGBx/RGBA and BGRx/BGRA. The frame pipeline downstream assumes the
/// BGRx family and does no channel-order conversion of its own, so RGBx/RGBA
/// capture sources are normalized here rather than propagated as a label no
/// consumer reads.
fn swap_rb_in_place(data: &mut [u8]) {
    for pixel in data.chunks_exact_mut(4) {
        pixel.swap(0, 2);
    }
}

/// Session strategy using embedded portal-generic backends.
///
/// Connects directly to the Wayland compositor as a client and provides
/// video capture, input injection, and clipboard via native protocols.
pub struct PortalGenericStrategy {
    /// Capture-request pacing ceiling (fps), forwarded to
    /// `WaylandConnection::set_min_frame_interval`. `None` leaves the
    /// portal-generic crate's capture loop unpaced (its default).
    target_fps: Option<u32>,
}

impl PortalGenericStrategy {
    pub fn new(target_fps: Option<u32>) -> Self {
        Self { target_fps }
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
            // Need at least keyboard input (pointer optional -- can use uinput fallback
            // or run in keyboard-only mode on compositors like COSMIC)
            let has_keyboard = protocols.zwp_virtual_keyboard;
            let has_pointer = protocols.wlr_virtual_pointer;

            if has_capture && has_keyboard {
                if !has_pointer {
                    debug!(
                        "[portal-generic] No wlr-virtual-pointer — pointer will use uinput fallback or keyboard-only mode"
                    );
                }
                Some(())
            } else {
                debug!(
                    "[portal-generic] Missing protocols: capture={}, keyboard={}",
                    has_capture, has_keyboard
                );
                None
            }
        })
        .await;

        matches!(result, Ok(Some(())))
    }
}

impl PortalGenericStrategy {
    /// Build capture preferences from compositor profile and quirks.
    ///
    /// Merges: env override > quirk-derived hints > profile recommendation > default.
    /// The resulting preferences are passed to portal-generic's `create_capture_backend()`.
    fn build_capture_preferences() -> CapturePreference {
        // Start with env overrides (highest priority)
        let mut prefs = CapturePreference::from_env();

        // If env didn't set a preference, consult compositor profile
        if prefs.preferred.is_none() {
            let compositor = identify_compositor();
            let profile = CompositorProfile::for_compositor(&compositor);

            info!(
                "portal-generic: Compositor {:?}, recommended capture: {:?}",
                compositor, profile.recommended_capture
            );

            // Map server's CaptureBackend enum to portal-generic's CaptureProtocol
            prefs.preferred = match profile.recommended_capture {
                ProfileCaptureBackend::WlrScreencopy => Some(CaptureProtocol::WlrScreencopy),
                ProfileCaptureBackend::ExtImageCopyCapture => {
                    Some(CaptureProtocol::ExtImageCopyCapture)
                }
                ProfileCaptureBackend::Portal => None, // Let auto-detection decide
            };

            // Derive broken_protocols from quirks
            if profile.has_quirk(&Quirk::ExtCaptureIncomplete) {
                prefs
                    .broken_protocols
                    .push(CaptureProtocol::ExtImageCopyCapture);
            }
        }

        prefs
    }
}

impl Default for PortalGenericStrategy {
    fn default() -> Self {
        Self::new(None)
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

        let target_fps = self.target_fps;

        // All Wayland and PipeWire setup is synchronous; run on blocking thread
        let (handle, wayland_stop) = tokio::task::spawn_blocking(move || -> Result<_> {
            // Connect to compositor and discover protocols
            let mut wayland = WaylandConnection::connect()
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

            // Build capture preferences from compositor profile + quirks
            let capture_prefs = Self::build_capture_preferences();

            // Configure ext-capture handshake timeout before spawning event loop
            if capture_prefs.handshake_timeout_ms > 0 {
                wayland.set_ext_capture_handshake_timeout(
                    std::time::Duration::from_millis(capture_prefs.handshake_timeout_ms),
                );
            }

            // Pace capture requests to the configured fps ceiling. Without
            // this, wlr-screencopy/ext-image-copy-capture's capture_output
            // request has no rate limiting of its own — compositors that
            // don't throttle fulfillment to their repaint cycle (observed:
            // wayfire) get asked to render+copy a frame the instant the
            // previous one lands, wasting compositor-side work on frames a
            // downstream bounded channel has no room for and will drop.
            if let Some(fps) = target_fps {
                let interval = std::time::Duration::from_secs_f64(1.0 / f64::from(fps.max(1)));
                wayland.set_min_frame_interval(interval);
            }

            // When wlr-screencopy is preferred (or ext is broken), tell the Wayland
            // event loop to skip ext-capture even if the protocol is bound.
            // Sway 1.11 advertises ext-image-copy-capture but its SHM constraints
            // are incomplete, causing zero frames.
            if capture_prefs.preferred == Some(CaptureProtocol::WlrScreencopy)
                || capture_prefs.broken_protocols.contains(&CaptureProtocol::ExtImageCopyCapture)
            {
                wayland.set_force_wlr_screencopy(true);
            }

            // Create direct frame channel to bypass PipeWire buffer sharing
            // (PipeWire buffer data can't be shared across separate connections)
            let (frame_tx, frame_rx) = std::sync::mpsc::channel();

            // Create health event channel for capture metrics
            let (health_tx, health_rx) = portal_health::health_channel();
            wayland.set_health_sender(health_tx.clone());
            let input_health_tx = health_tx.clone();
            let clipboard_health_tx = health_tx;

            // Spawn the Wayland event loop with direct frame channel
            let (
                wayland_stop,
                _shared_wayland_state,
                capture_tx,
                clipboard_tx,
                shared_clipboard,
                _wayland_thread,
            ) = wayland.spawn_event_loop_with_frame_channel(
                Arc::clone(&pipewire_manager),
                Some(frame_tx),
            );

            // Create input backend — prefer wlr virtual input for wlroots compositors.
            // EIS bridge mode has issues on labwc; wlr-virtual-pointer/keyboard work directly.
            let input_config = {
                let mut cfg = InputBackendConfig::from_env();
                // Only override if env didn't explicitly set a preference
                if std::env::var("XDP_GENERIC_INPUT_PROTOCOL").is_err() {
                    cfg.preferred = xdg_desktop_portal_generic::services::input::InputProtocol::WlrVirtualInput;
                }
                cfg
            };
            let mut input_backend = create_input_backend(&input_config, &protocols)
                .map_err(|e| anyhow::anyhow!("Input backend: {e}"))?;

            // Wire health sender to input backend
            input_backend.set_health_sender(input_health_tx);

            // Create a default input context for this session
            let session_id = format!("lamco-rdp-{}", uuid::Uuid::new_v4());
            let devices = DeviceTypes {
                keyboard: true,
                pointer: true,
                touchscreen: false,
            };
            input_backend.create_context(&session_id, devices)
                .map_err(|e| anyhow::anyhow!("Input context: {e}"))?;

            // Create capture backend with server-informed preferences
            let mut capture_backend = create_capture_backend(
                &protocols,
                &capture_prefs,
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
            let clipboard_prefs = ClipboardPreference::from_env();
            let mut clipboard_backend = create_clipboard_backend(
                &protocols,
                &clipboard_prefs,
                clipboard_tx,
                shared_clipboard,
            );

            // Wire health sender to clipboard backend
            if let Some(ref mut cb) = clipboard_backend {
                cb.set_health_sender(clipboard_health_tx);
            }

            if clipboard_backend.is_some() {
                info!("portal-generic: Clipboard backend active");
            } else {
                warn!("portal-generic: No clipboard protocol available");
            }

            // COSMIC-class compositors expose a virtual keyboard but no
            // wlr-virtual-pointer, so the wlr backend comes up keyboard-only.
            // Inject the pointer at the kernel (/dev/uinput) as a trusted
            // application; fall back to keyboard-only if /dev/uinput is unusable.
            let (uinput_pointer, pointer_backend) = if !protocols.wlr_virtual_pointer
                && protocols.zwp_virtual_keyboard
            {
                match super::uinput_pointer::UinputPointer::new() {
                    Ok(p) => {
                        info!(
                            "COSMIC-class compositor: pointer via /dev/uinput (no wlr-virtual-pointer)"
                        );
                        (Some(Mutex::new(p)), "uinput")
                    }
                    Err(e) => {
                        warn!("uinput pointer unavailable ({e:#}); session will be keyboard-only");
                        (None, "none")
                    }
                }
            } else if protocols.wlr_virtual_pointer {
                (None, "wlr-virtual-pointer")
            } else {
                (None, "none")
            };

            let handle = PortalGenericSessionHandle {
                session_id,
                input_backend: Arc::new(Mutex::new(input_backend)),
                uinput_pointer,
                pointer_backend,
                _capture_backend: Arc::new(Mutex::new(capture_backend)),
                clipboard_backend: clipboard_backend.map(|cb| Arc::new(Mutex::new(cb))),
                _pipewire_manager: pipewire_manager,
                streams,
                frame_rx: std::sync::Mutex::new(Some(frame_rx)),
                health_rx: std::sync::Mutex::new(Some(health_rx)),
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
        let _ = self.health_reporter.set(reporter.clone());

        reporter.report(crate::health::HealthEvent::InputBackendSelected {
            backend: self.handle.pointer_backend.to_string(),
        });

        // Spawn portal health bridge: PortalHealthEvent → server HealthEvent
        let health_rx_opt: Option<portal_health::HealthReceiver> =
            self.handle.health_rx.lock().ok().and_then(
                |mut g: std::sync::MutexGuard<'_, Option<portal_health::HealthReceiver>>| g.take(),
            );
        if let Some(health_rx) = health_rx_opt {
            let reporter_for_bridge = reporter;
            tokio::spawn(async move {
                let mut rx: portal_health::HealthReceiver = health_rx;
                let mut last_eis_serial: Option<u32> = None;
                let mut consecutive_frame_failures: u32 = 0;

                while let Some(event) = rx.recv().await {
                    match event {
                        // --- Capture ---
                        PortalHealthEvent::FrameCaptured {
                            capture_latency,
                            frame_number,
                            ..
                        } => {
                            consecutive_frame_failures = 0;
                            if frame_number <= 3 || frame_number % 500 == 0 {
                                tracing::debug!(
                                    frame = frame_number,
                                    latency_us = capture_latency.as_micros() as u64,
                                    "Portal capture health"
                                );
                            }
                        }
                        PortalHealthEvent::FrameFailed { reason, .. } => {
                            consecutive_frame_failures += 1;
                            tracing::warn!(
                                reason,
                                consecutive = consecutive_frame_failures,
                                "Portal capture frame failed"
                            );
                            if consecutive_frame_failures >= 3 {
                                reporter_for_bridge.report(HealthEvent::VideoStreamStateChanged {
                                    state: crate::health::VideoStreamState::Error,
                                });
                            }
                        }
                        PortalHealthEvent::CaptureStateChanged { protocol, state } => {
                            let health_state = match state {
                                portal_health::CaptureState::Active => {
                                    tracing::info!(?protocol, "Capture active");
                                    crate::health::VideoStreamState::Streaming
                                }
                                portal_health::CaptureState::Paused => {
                                    tracing::warn!(?protocol, "Capture paused");
                                    crate::health::VideoStreamState::Paused
                                }
                                portal_health::CaptureState::Failed => {
                                    tracing::error!(?protocol, "Capture failed");
                                    crate::health::VideoStreamState::Error
                                }
                            };
                            reporter_for_bridge.report(HealthEvent::VideoStreamStateChanged {
                                state: health_state,
                            });
                        }

                        // --- EIS Protocol ---
                        PortalHealthEvent::EisFrameReceived {
                            last_serial,
                            time_usec,
                        } => {
                            if let Some(prev) = last_eis_serial {
                                let gap = last_serial.wrapping_sub(prev);
                                if gap > 1 {
                                    tracing::warn!(
                                        gap = gap - 1,
                                        prev_serial = prev,
                                        current_serial = last_serial,
                                        "EIS serial gap: {} events may have been lost",
                                        gap - 1
                                    );
                                }
                            }
                            last_eis_serial = Some(last_serial);
                            let _ = time_usec;
                        }
                        PortalHealthEvent::EisDeviceStateChanged {
                            emulating, serial, ..
                        } => {
                            tracing::info!(emulating, serial, "EIS device emulation state changed");
                        }

                        // --- Input ---
                        PortalHealthEvent::InputBatch {
                            events_forwarded,
                            events_failed,
                            protocol,
                        } => {
                            if events_failed > 0 {
                                reporter_for_bridge.report(HealthEvent::InputFailed {
                                    reason: format!(
                                        "{protocol:?}: {events_failed} failures in last {events_forwarded} events"
                                    ),
                                    permanent: false,
                                });
                            }
                            tracing::debug!(
                                forwarded = events_forwarded,
                                failed = events_failed,
                                protocol = ?protocol,
                                "Input batch health"
                            );
                        }
                        PortalHealthEvent::InputDisconnected {
                            reason,
                            recoverable,
                        } => {
                            reporter_for_bridge.report(HealthEvent::InputFailed {
                                reason: reason.clone(),
                                permanent: !recoverable,
                            });
                        }

                        // --- Clipboard ---
                        PortalHealthEvent::ClipboardSelectionChanged { format_count } => {
                            tracing::debug!(format_count, "Portal clipboard selection changed");
                        }
                        PortalHealthEvent::ClipboardTransferResult { success, bytes } => {
                            if !success {
                                reporter_for_bridge.report(HealthEvent::ClipboardFailed {
                                    reason: "Clipboard transfer failed".into(),
                                });
                            } else {
                                tracing::trace!(bytes, "Clipboard transfer completed");
                            }
                        }

                        // --- Session ---
                        PortalHealthEvent::SessionStateChanged { state } => match state {
                            portal_health::PortalSessionState::Closed => {
                                tracing::warn!("Portal-generic session closed");
                                reporter_for_bridge.report(HealthEvent::SessionClosed {
                                    reason: "portal-generic session state changed to closed".into(),
                                });
                            }
                            portal_health::PortalSessionState::Started => {
                                tracing::info!("Portal-generic session started");
                            }
                            portal_health::PortalSessionState::Init => {
                                tracing::debug!("Portal-generic session initializing");
                            }
                        },
                    }
                }
                tracing::debug!("Portal health bridge ended");
            });
            info!("Portal health bridge started");
        }
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

    async fn notify_keyboard_keysym(&self, keysym: u32, pressed: bool) -> Result<()> {
        self.handle.notify_keyboard_keysym(keysym, pressed).await
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

    fn clipboard_source(&self) -> crate::session::strategy::ClipboardSource {
        self.handle.clipboard_source()
    }
}

/// Session handle for the embedded portal-generic backend.
///
/// Bridges portal-generic's backend traits to the SessionHandle interface
/// expected by the RDP server's session management layer.
pub struct PortalGenericSessionHandle {
    session_id: String,
    input_backend: Arc<Mutex<Box<dyn InputBackend>>>,
    /// Application-owned `/dev/uinput` pointer, used only on compositors that
    /// expose a virtual keyboard but no `wlr-virtual-pointer` (COSMIC). When
    /// present, pointer events route here instead of the (pointer-less) wlr
    /// backend; keyboard always stays on `input_backend`.
    uinput_pointer: Option<Mutex<super::uinput_pointer::UinputPointer>>,
    /// Which pointer backend this session settled on ("uinput",
    /// "wlr-virtual-pointer", or "none") -- reported once via
    /// `HealthEvent::InputBackendSelected` when the health reporter attaches.
    pointer_backend: &'static str,
    _capture_backend: Arc<Mutex<Box<dyn xdg_desktop_portal_generic::CaptureBackend>>>,
    clipboard_backend: Option<Arc<Mutex<Box<dyn xdg_desktop_portal_generic::ClipboardBackend>>>>,
    _pipewire_manager: Arc<PipeWireManager>,
    streams: Vec<StreamInfo>,
    /// Direct frame channel receiver (taken once by the display handler).
    frame_rx:
        std::sync::Mutex<Option<std::sync::mpsc::Receiver<xdg_desktop_portal_generic::RawFrame>>>,
    /// Portal health event receiver (taken once to spawn bridge task).
    health_rx: std::sync::Mutex<Option<portal_health::HealthReceiver>>,
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
        // Use direct frame channel — PipeWire buffer sharing doesn't work
        // across separate connections (the buffer data pointer is NULL on the
        // consumer side because the source's ALLOC_BUFFERS creates MemPtr
        // buffers that can't be shared across address spaces).
        let raw_rx = self
            .frame_rx
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();

        let Some(raw_rx) = raw_rx else {
            warn!("portal-generic: Direct frame channel already taken, falling back to NodeId");
            let node_id = self.streams.first().map_or(0, |s| s.node_id);
            return PipeWireAccess::NodeId(node_id);
        };

        info!("portal-generic: Using direct frame channel (bypassing PipeWire)");

        // Bridge RawFrame (portal crate) -> RawFrameData (pipewire crate)
        let (tx, rx) = std::sync::mpsc::sync_channel(256);
        if let Err(e) = std::thread::Builder::new()
            .name("raw-frame-bridge".into())
            .spawn(move || {
                let mut last_format_raw: Option<u32> = None;
                while let Ok(raw) = raw_rx.recv() {
                    let src_format = wl_shm_format_to_pixel_format(raw.format_raw);
                    if last_format_raw != Some(raw.format_raw) {
                        match src_format {
                            Some(f) => info!(
                                format_raw = raw.format_raw,
                                capture_format = ?f,
                                "portal-generic: capture pixel format resolved"
                            ),
                            None => warn!(
                                format_raw = raw.format_raw,
                                "portal-generic: unmapped wl_shm format — assuming BGRx, colors may be wrong"
                            ),
                        }
                        last_format_raw = Some(raw.format_raw);
                    }

                    // Normalize to the BGRx family: display_handler and the bitmap
                    // converter treat all frames as BGRx/BGRA with no channel-order
                    // conversion, so an RGBx/RGBA source (wlroots/virtio over
                    // wlr-screencopy) must be swapped here. Relabeling alone leaves
                    // R and B transposed — the blue→brown skew on sway.
                    let mut data = raw.data;
                    let format = match src_format {
                        Some(lamco_pipewire::PixelFormat::RGBx) => {
                            swap_rb_in_place(&mut data);
                            Some(lamco_pipewire::PixelFormat::BGRx)
                        }
                        Some(lamco_pipewire::PixelFormat::RGBA) => {
                            swap_rb_in_place(&mut data);
                            Some(lamco_pipewire::PixelFormat::BGRA)
                        }
                        other => other,
                    };

                    let converted = lamco_pipewire::frame::RawFrameData {
                        data,
                        width: Some(raw.width),
                        height: Some(raw.height),
                        stride: Some(raw.stride),
                        format,
                    };
                    if tx.send(converted).is_err() {
                        break;
                    }
                }
                info!("portal-generic: raw-frame-bridge thread exited");
            })
        {
            error!("Failed to spawn raw-frame-bridge thread: {e}");
            let node_id = self.streams.first().map_or(0, |s| s.node_id);
            return PipeWireAccess::NodeId(node_id);
        }

        PipeWireAccess::DirectChannel(rx)
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

    async fn notify_keyboard_keysym(&self, keysym: u32, pressed: bool) -> Result<()> {
        let mut backend = self
            .input_backend
            .lock()
            .map_err(|e| anyhow::anyhow!("Input backend lock poisoned: {e}"))?;

        // Same resolution xdg_desktop_portal_generic's own NotifyKeyboardKeysym
        // D-Bus handler performs; InputBackend::keysym_to_keycode is public API
        // for exactly this purpose, so no change to that crate is needed here.
        let keycode = backend.keysym_to_keycode(keysym).ok_or_else(|| {
            anyhow::anyhow!("No keycode found for keysym 0x{keysym:04x} in current keymap")
        })?;

        let event = InputEvent::Keyboard(KeyboardEvent {
            keycode,
            state: if pressed {
                KeyState::Pressed
            } else {
                KeyState::Released
            },
            time_usec: current_time_usec(),
        });
        backend
            .inject_event(&self.session_id, event)
            .map_err(|e| anyhow::anyhow!("Keyboard inject: {e}"))?;

        Ok(())
    }

    async fn notify_pointer_motion_absolute(&self, stream_id: u32, x: f64, y: f64) -> Result<()> {
        // Caller (input_handler) passes pixel coordinates in the stream's frame.
        // Look up that stream's dimensions so the backend can normalize.
        let (x_extent, y_extent) = self
            .streams
            .iter()
            .find(|s| s.node_id == stream_id)
            .map_or((0, 0), |s| (s.width, s.height));

        // COSMIC: route to the kernel uinput pointer. Normalize to [0,1] within
        // the stream frame first (the device maps [0,1] onto its ABS range).
        if let Some(ref uinput) = self.uinput_pointer {
            let nx = if x_extent == 0 {
                x
            } else {
                x / f64::from(x_extent)
            };
            let ny = if y_extent == 0 {
                y
            } else {
                y / f64::from(y_extent)
            };
            return uinput
                .lock()
                .map_err(|e| anyhow::anyhow!("uinput pointer lock poisoned: {e}"))?
                .motion_absolute(nx, ny);
        }

        let event = InputEvent::Pointer(PointerEvent::MotionAbsolute {
            x,
            y,
            x_extent,
            y_extent,
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
        if let Some(ref uinput) = self.uinput_pointer {
            return uinput
                .lock()
                .map_err(|e| anyhow::anyhow!("uinput pointer lock poisoned: {e}"))?
                .button(button as u32, pressed);
        }

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
        if let Some(ref uinput) = self.uinput_pointer {
            return uinput
                .lock()
                .map_err(|e| anyhow::anyhow!("uinput pointer lock poisoned: {e}"))?
                .scroll(dx, dy);
        }

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

    fn clipboard_source(&self) -> crate::session::strategy::ClipboardSource {
        match self.clipboard_backend.as_ref() {
            Some(backend) => {
                crate::session::strategy::ClipboardSource::DataControl(Arc::clone(backend))
            }
            None => crate::session::strategy::ClipboardSource::None,
        }
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
        let strategy = PortalGenericStrategy::new(None);
        assert_eq!(strategy.name(), "portal-generic");
        assert!(!strategy.requires_initial_setup());
        assert!(strategy.supports_unattended_restore());
    }

    #[test]
    fn test_wl_shm_format_mapping() {
        use lamco_pipewire::PixelFormat;

        // wl_shm values from wayland.xml. The byte order below is the
        // little-endian in-memory layout; the RDP pipeline treats BGRx as the
        // no-conversion format, so RGBx/RGBA must be flagged to trigger a swap.
        assert_eq!(wl_shm_format_to_pixel_format(0), Some(PixelFormat::BGRA)); // argb8888
        assert_eq!(wl_shm_format_to_pixel_format(1), Some(PixelFormat::BGRx)); // xrgb8888
        // xbgr8888 = 0x34324258: the format sway/virtio actually delivers.
        assert_eq!(
            wl_shm_format_to_pixel_format(0x3432_4258),
            Some(PixelFormat::RGBx)
        );
        assert_eq!(
            wl_shm_format_to_pixel_format(0x3432_4241), // abgr8888
            Some(PixelFormat::RGBA)
        );
    }
}
