//! Standalone Wayland Data-Control Clipboard Provider (via wl-clipboard-rs)
//!
//! Uses `wl-clipboard-rs` to interact with the Wayland clipboard via the
//! ext-data-control-v1 or wlr-data-control-v1 protocol. Unlike the
//! `DataControlClipboardProvider` (which wraps portal-generic's
//! `ClipboardBackend`), this provider is self-contained and composable
//! with any session strategy.
//!
//! # Clipboard Change Detection
//!
//! When the `wayland` feature is enabled, a persistent wlr-data-control-v1
//! connection receives compositor `selection` events for reliable clipboard
//! change detection. Without `wayland`, falls back to ephemeral polling via
//! `wl-clipboard-rs::paste::get_mime_types`.

use std::{
    collections::{HashMap, HashSet},
    io::Read,
    sync::{
        atomic::{AtomicBool, AtomicU32, Ordering},
        Arc, Mutex,
    },
};

use async_trait::async_trait;
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};
use wl_clipboard_rs::{copy as wl_copy, paste as wl_paste};

use crate::clipboard::{
    error::{ClipboardError, Result},
    provider::{ClipboardProvider, ClipboardProviderEvent},
};

/// Polling interval for clipboard change detection (persistent monitor
/// uses this as the roundtrip cadence; ephemeral fallback uses it as
/// the sleep between queries).
const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(250);

/// Standalone data-control clipboard provider.
///
/// Connects directly to the compositor's Wayland socket via `wl-clipboard-rs`.
/// No portal daemon or embedded backend required — works with any session
/// strategy as long as the compositor exposes ext-data-control-v1 or
/// wlr-data-control-v1.
pub struct WlClipboardProvider {
    /// Channel to emit SelectionTransfer events from announce_formats
    event_tx: mpsc::UnboundedSender<ClipboardProviderEvent>,
    /// Receiver end (taken by subscribe())
    event_rx: std::sync::Mutex<Option<mpsc::UnboundedReceiver<ClipboardProviderEvent>>>,
    /// Shutdown signal for the polling thread
    shutdown: Arc<AtomicBool>,
    /// Background polling thread handle
    poll_handle: Mutex<Option<std::thread::JoinHandle<()>>>,
    /// MIME types we most recently announced (to filter out our own changes)
    our_mime_types: Arc<Mutex<HashSet<String>>>,
    /// Accumulated data from RDP client for each format (built up across
    /// complete_transfer calls, used to call copy_multi with real bytes)
    pending_data: Arc<Mutex<HashMap<String, Vec<u8>>>>,
    /// Serial counter for SelectionTransfer events emitted by announce_formats
    next_serial: Arc<AtomicU32>,
}

impl Default for WlClipboardProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl WlClipboardProvider {
    pub fn new() -> Self {
        let (event_tx, event_rx) = mpsc::unbounded_channel();
        let shutdown = Arc::new(AtomicBool::new(false));
        let our_mime_types: Arc<Mutex<HashSet<String>>> = Arc::new(Mutex::new(HashSet::new()));

        let poll_shutdown = Arc::clone(&shutdown);
        let poll_tx = event_tx.clone();
        let poll_ours = Arc::clone(&our_mime_types);

        let poll_handle = std::thread::Builder::new()
            .name("wl-clipboard-monitor".into())
            .spawn(move || {
                clipboard_monitor_loop(poll_shutdown, poll_tx, poll_ours);
            })
            .ok();

        let wayland_display = std::env::var("WAYLAND_DISPLAY").unwrap_or_default();
        info!(
            "wl-clipboard-rs clipboard provider created (WAYLAND_DISPLAY={:?})",
            wayland_display
        );

        Self {
            event_tx,
            event_rx: std::sync::Mutex::new(Some(event_rx)),
            shutdown,
            poll_handle: Mutex::new(poll_handle),
            our_mime_types,
            pending_data: Arc::new(Mutex::new(HashMap::new())),
            next_serial: Arc::new(AtomicU32::new(1)),
        }
    }
}

// ─── Persistent data-control monitor (requires `wayland` feature) ──────────
//
// Maintains a single Wayland connection and listens for `selection` events
// from the compositor. Much more reliable than creating ephemeral connections
// for each poll, which can miss clipboard changes on some compositors.

#[cfg(feature = "wayland")]
mod monitor {
    use std::collections::HashMap;

    use wayland_client::{
        event_created_child,
        globals::{registry_queue_init, GlobalListContents},
        protocol::{
            wl_registry::WlRegistry,
            wl_seat::{self, WlSeat},
        },
        Connection, Dispatch, EventQueue, Proxy, QueueHandle,
    };
    use wayland_protocols_wlr::data_control::v1::client::{
        zwlr_data_control_device_v1::{self, ZwlrDataControlDeviceV1},
        zwlr_data_control_manager_v1::ZwlrDataControlManagerV1,
        zwlr_data_control_offer_v1::{self, ZwlrDataControlOfferV1},
    };

    use super::{debug, warn, HashSet};

    /// State for the persistent clipboard monitor.
    pub(super) struct MonitorState {
        /// MIME types for each pending offer (populated by Offer events)
        offers: HashMap<ZwlrDataControlOfferV1, Vec<String>>,
        /// Current selection's MIME types (set when Selection event fires)
        pub(super) current_mime_types: HashSet<String>,
        /// True when the selection changed since last check
        pub(super) selection_changed: bool,
        /// Whether we've seen at least one seat
        has_seat: bool,
    }

    impl MonitorState {
        fn new() -> Self {
            Self {
                offers: HashMap::new(),
                current_mime_types: HashSet::new(),
                selection_changed: false,
                has_seat: false,
            }
        }
    }

    // WlRegistry dispatch — no-op, globals are handled by registry_queue_init
    impl Dispatch<WlRegistry, GlobalListContents> for MonitorState {
        fn event(
            _state: &mut Self,
            _proxy: &WlRegistry,
            _event: <WlRegistry as wayland_client::Proxy>::Event,
            _data: &GlobalListContents,
            _conn: &Connection,
            _qh: &QueueHandle<Self>,
        ) {
        }
    }

    // WlSeat dispatch — track seat presence
    impl Dispatch<WlSeat, ()> for MonitorState {
        fn event(
            state: &mut Self,
            _proxy: &WlSeat,
            event: <WlSeat as wayland_client::Proxy>::Event,
            _data: &(),
            _conn: &Connection,
            _qh: &QueueHandle<Self>,
        ) {
            if let wl_seat::Event::Name { name } = event {
                debug!("wl-clipboard monitor: seat '{name}' found");
                state.has_seat = true;
            }
        }
    }

    // ZwlrDataControlManagerV1 dispatch — no events defined
    impl Dispatch<ZwlrDataControlManagerV1, ()> for MonitorState {
        fn event(
            _state: &mut Self,
            _proxy: &ZwlrDataControlManagerV1,
            _event: <ZwlrDataControlManagerV1 as wayland_client::Proxy>::Event,
            _data: &(),
            _conn: &Connection,
            _qh: &QueueHandle<Self>,
        ) {
        }
    }

    // ZwlrDataControlDeviceV1 dispatch — handles selection changes
    impl Dispatch<ZwlrDataControlDeviceV1, ()> for MonitorState {
        fn event(
            state: &mut Self,
            _proxy: &ZwlrDataControlDeviceV1,
            event: <ZwlrDataControlDeviceV1 as wayland_client::Proxy>::Event,
            _data: &(),
            _conn: &Connection,
            _qh: &QueueHandle<Self>,
        ) {
            match event {
                zwlr_data_control_device_v1::Event::DataOffer { id } => {
                    state.offers.insert(id, Vec::new());
                }
                zwlr_data_control_device_v1::Event::Selection { id } => {
                    let new_types: HashSet<String> = match id {
                        Some(offer) => state
                            .offers
                            .remove(&offer)
                            .unwrap_or_default()
                            .into_iter()
                            .collect(),
                        None => HashSet::new(),
                    };
                    if new_types != state.current_mime_types {
                        state.current_mime_types = new_types;
                        state.selection_changed = true;
                    }
                    // Clean up stale offers
                    state.offers.retain(|_, _| false);
                }
                zwlr_data_control_device_v1::Event::Finished => {
                    warn!("wl-clipboard monitor: data-control device finished (compositor restarted?)");
                }
                _ => {}
            }
        }

        event_created_child!(MonitorState, ZwlrDataControlDeviceV1, [
            zwlr_data_control_device_v1::EVT_DATA_OFFER_OPCODE => (ZwlrDataControlOfferV1, ()),
        ]);
    }

    // ZwlrDataControlOfferV1 dispatch — collects MIME types from offers
    impl Dispatch<ZwlrDataControlOfferV1, ()> for MonitorState {
        fn event(
            state: &mut Self,
            proxy: &ZwlrDataControlOfferV1,
            event: <ZwlrDataControlOfferV1 as wayland_client::Proxy>::Event,
            _data: &(),
            _conn: &Connection,
            _qh: &QueueHandle<Self>,
        ) {
            if let zwlr_data_control_offer_v1::Event::Offer { mime_type } = event {
                if let Some(types) = state.offers.get_mut(proxy) {
                    types.push(mime_type);
                }
            }
        }
    }

    /// Try to create a persistent data-control monitor.
    ///
    /// Returns the event queue and initial state, or None if the compositor
    /// doesn't support wlr-data-control-v1.
    pub(super) fn create_monitor() -> Option<(EventQueue<MonitorState>, MonitorState)> {
        let conn = match Connection::connect_to_env() {
            Ok(c) => c,
            Err(e) => {
                warn!("wl-clipboard monitor: Wayland connection failed: {e}");
                return None;
            }
        };

        let (globals, queue) = match registry_queue_init::<MonitorState>(&conn) {
            Ok(x) => x,
            Err(e) => {
                warn!("wl-clipboard monitor: registry init failed: {e}");
                return None;
            }
        };

        let qh = queue.handle();

        // Bind wlr-data-control-v1 manager
        let manager: ZwlrDataControlManagerV1 = match globals.bind(&qh, 1..=2, ()) {
            Ok(m) => m,
            Err(_) => {
                warn!("wl-clipboard monitor: compositor lacks wlr-data-control-v1");
                return None;
            }
        };

        // Find the first seat
        let registry = globals.registry();
        let seat: Option<WlSeat> = globals.contents().with_list(|list| {
            list.iter()
                .find(|g| g.interface == WlSeat::interface().name && g.version >= 2)
                .map(|g| registry.bind(g.name, 2, &qh, ()))
        });

        let Some(seat) = seat else {
            warn!("wl-clipboard monitor: no seat found");
            return None;
        };

        // Create a data-control device for the seat
        let _device = manager.get_data_device(&seat, &qh, ());

        let state = MonitorState::new();
        Some((queue, state))
    }
}

/// Clipboard monitor loop — runs on a dedicated thread.
///
/// With `wayland` feature: creates a persistent wlr-data-control-v1
/// connection and dispatches events via roundtrip. This reliably receives
/// `selection` events from the compositor.
///
/// Without `wayland` feature: falls back to ephemeral polling via
/// `wl-clipboard-rs::paste::get_mime_types`.
fn clipboard_monitor_loop(
    shutdown: Arc<AtomicBool>,
    tx: mpsc::UnboundedSender<ClipboardProviderEvent>,
    our_mime_types: Arc<Mutex<HashSet<String>>>,
) {
    #[cfg(feature = "wayland")]
    {
        if let Some((mut queue, mut state)) = monitor::create_monitor() {
            info!("wl-clipboard: persistent data-control monitor started");

            // Initial roundtrip to get current selection state
            if let Err(e) = queue.roundtrip(&mut state) {
                warn!("wl-clipboard monitor: initial roundtrip failed: {e}");
                // Fall through to ephemeral polling
            } else {
                if !state.current_mime_types.is_empty() {
                    debug!(
                        "wl-clipboard monitor: initial clipboard has {} types",
                        state.current_mime_types.len()
                    );
                } else {
                    debug!("wl-clipboard monitor: initial clipboard empty");
                }

                // Persistent monitor loop
                let mut last_emitted: HashSet<String> = HashSet::new();
                while !shutdown.load(Ordering::Relaxed) {
                    std::thread::sleep(POLL_INTERVAL);
                    if shutdown.load(Ordering::Relaxed) {
                        break;
                    }

                    match queue.roundtrip(&mut state) {
                        Ok(_) => {}
                        Err(e) => {
                            error!("wl-clipboard monitor: roundtrip error: {e}");
                            break;
                        }
                    }

                    if !state.selection_changed {
                        continue;
                    }
                    state.selection_changed = false;

                    let current = &state.current_mime_types;

                    // Skip if unchanged from what we last emitted
                    if *current == last_emitted {
                        continue;
                    }

                    // Filter out our own clipboard changes
                    let is_ours = {
                        let guard = our_mime_types
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                        !guard.is_empty() && *current == *guard
                    };

                    if is_ours {
                        debug!(
                            "wl-clipboard monitor: selection change is ours (filtered), {} types",
                            current.len()
                        );
                    } else if !current.is_empty() {
                        info!(
                            "wl-clipboard monitor: selection changed ({} types: {:?})",
                            current.len(),
                            current
                        );
                        let mime_list: Vec<String> = current.iter().cloned().collect();
                        let _ = tx.send(ClipboardProviderEvent::SelectionChanged {
                            mime_types: mime_list,
                            force: true,
                        });
                    } else {
                        debug!("wl-clipboard monitor: clipboard cleared");
                    }

                    last_emitted.clone_from(current);
                }

                debug!("wl-clipboard monitor thread exiting");
                return;
            }
        } else {
            info!(
                "wl-clipboard: persistent monitor unavailable, falling back to ephemeral polling"
            );
        }
    }

    // Ephemeral polling fallback (or non-wayland feature)
    clipboard_poll_loop_ephemeral(shutdown, tx, our_mime_types);
}

/// Ephemeral polling fallback for clipboard change detection.
///
/// Each poll creates a new Wayland connection via `wl_paste::get_mime_types`.
/// Less reliable than the persistent monitor but works without the `wayland`
/// feature crate dependency.
fn clipboard_poll_loop_ephemeral(
    shutdown: Arc<AtomicBool>,
    tx: mpsc::UnboundedSender<ClipboardProviderEvent>,
    our_mime_types: Arc<Mutex<HashSet<String>>>,
) {
    info!("wl-clipboard: ephemeral polling mode active");

    let mut last_mime_types: HashSet<String> = HashSet::new();
    let mut first_success = false;
    let mut error_streak: u32 = 0;

    while !shutdown.load(Ordering::Relaxed) {
        std::thread::sleep(POLL_INTERVAL);

        if shutdown.load(Ordering::Relaxed) {
            break;
        }

        let current = match wl_paste::get_mime_types(
            wl_paste::ClipboardType::Regular,
            wl_paste::Seat::Unspecified,
        ) {
            Ok(types) => {
                if !first_success {
                    debug!(
                        "wl-clipboard poll: first successful query ({} types)",
                        types.len()
                    );
                    first_success = true;
                }
                error_streak = 0;
                types
            }
            Err(wl_paste::Error::ClipboardEmpty) => {
                if !first_success {
                    debug!("wl-clipboard poll: first query returned ClipboardEmpty (protocol works, no content)");
                    first_success = true;
                }
                error_streak = 0;
                HashSet::new()
            }
            Err(wl_paste::Error::NoSeats) => {
                if !first_success {
                    warn!("wl-clipboard poll: first query returned NoSeats (data-control seat binding failed)");
                    first_success = true;
                }
                error_streak = 0;
                HashSet::new()
            }
            Err(e) => {
                error_streak += 1;
                if error_streak == 20 {
                    warn!("wl-clipboard poll: {error_streak} consecutive errors, latest: {e}");
                } else if error_streak.is_multiple_of(100) {
                    debug!("wl-clipboard poll: {error_streak} consecutive errors, latest: {e}");
                }
                continue;
            }
        };

        if current != last_mime_types {
            let is_ours = {
                let guard = our_mime_types
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                !guard.is_empty() && current == *guard
            };

            if is_ours {
                debug!(
                    "wl-clipboard poll: selection change is ours (filtered), {} types",
                    current.len()
                );
            } else if !current.is_empty() {
                info!(
                    "wl-clipboard poll: selection changed ({} types: {:?})",
                    current.len(),
                    current
                );
                let mime_list: Vec<String> = current.iter().cloned().collect();
                let _ = tx.send(ClipboardProviderEvent::SelectionChanged {
                    mime_types: mime_list,
                    force: true,
                });
            } else {
                debug!("wl-clipboard poll: clipboard cleared");
            }

            last_mime_types = current;
        }
    }

    debug!("wl-clipboard poll thread exiting");
}

#[async_trait]
impl ClipboardProvider for WlClipboardProvider {
    fn name(&self) -> &'static str {
        "wl-clipboard"
    }

    fn supports_file_transfer(&self) -> bool {
        // data-control supports arbitrary MIME types including text/uri-list
        true
    }

    async fn announce_formats(&self, mime_types: Vec<String>) -> Result<()> {
        if mime_types.is_empty() {
            return Ok(());
        }

        // Clear any previous pending data from a prior RDP copy
        {
            let mut guard = self
                .pending_data
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            guard.clear();
        }

        // Record what we're announcing so the poll thread can filter it out
        {
            let mut guard = self
                .our_mime_types
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            *guard = mime_types.iter().cloned().collect();
        }

        // wl-clipboard-rs requires data upfront (no delayed rendering callback).
        // Instead of copying empty bytes, emit SelectionTransfer events to trigger
        // the orchestrator's existing flow: it fetches data from the RDP client and
        // calls complete_transfer() with real bytes, which then calls copy_multi.
        debug!(
            "wl-clipboard: requesting eager data fetch for {} formats",
            mime_types.len()
        );

        for mime_type in &mime_types {
            let serial = self.next_serial.fetch_add(1, Ordering::Relaxed);
            let _ = self
                .event_tx
                .send(ClipboardProviderEvent::SelectionTransfer {
                    serial,
                    mime_type: mime_type.clone(),
                });
        }

        Ok(())
    }

    async fn read_data(&self, mime_type: &str) -> Result<Vec<u8>> {
        let mime_owned = mime_type.to_string();

        tokio::task::spawn_blocking(move || {
            let result = wl_paste::get_contents(
                wl_paste::ClipboardType::Regular,
                wl_paste::Seat::Unspecified,
                wl_paste::MimeType::Specific(&mime_owned),
            );

            match result {
                Ok((mut pipe, _actual_mime)) => {
                    let mut data = Vec::new();
                    pipe.read_to_end(&mut data).map_err(|e| {
                        ClipboardError::PortalError(format!("wl-clipboard read failed: {e}"))
                    })?;
                    debug!("wl-clipboard: read {} bytes for {}", data.len(), mime_owned);
                    Ok(data)
                }
                Err(wl_paste::Error::ClipboardEmpty | wl_paste::Error::NoMimeType) => {
                    warn!("wl-clipboard: no data available for {}", mime_owned);
                    Ok(Vec::new())
                }
                Err(e) => Err(ClipboardError::PortalError(format!(
                    "wl-clipboard get_contents failed: {e}"
                ))),
            }
        })
        .await
        .map_err(|e| ClipboardError::PortalError(format!("read_data task panicked: {e}")))?
    }

    async fn complete_transfer(
        &self,
        _serial: u32,
        mime_type: &str,
        data: Vec<u8>,
        success: bool,
    ) -> Result<()> {
        if !success || data.is_empty() {
            return Ok(());
        }

        // Accumulate this format's data
        let all_data = {
            let mut guard = self
                .pending_data
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            guard.insert(mime_type.to_string(), data);
            guard.clone()
        };

        debug!(
            "wl-clipboard: complete_transfer for {}, copying {} formats to Wayland clipboard",
            mime_type,
            all_data.len()
        );

        // Build sources from all accumulated data and copy to Wayland clipboard.
        // Each complete_transfer call re-copies everything so the clipboard always
        // has the full set of formats received so far.
        let sources: Vec<wl_copy::MimeSource> = all_data
            .into_iter()
            .map(|(mime, bytes)| wl_copy::MimeSource {
                source: wl_copy::Source::Bytes(bytes.into_boxed_slice()),
                mime_type: wl_copy::MimeType::Specific(mime),
            })
            .collect();

        // Update our_mime_types so the monitor thread filters out this change.
        // Include the text aliases that copy_multi will add automatically.
        {
            let mut guard = self
                .our_mime_types
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let mut expected: HashSet<String> = sources
                .iter()
                .map(|s| match &s.mime_type {
                    wl_copy::MimeType::Specific(m) => m.clone(),
                    _ => String::new(),
                })
                .collect();
            // copy_multi adds these aliases for any text source
            let has_text = expected.iter().any(|m| is_text_mime(m));
            if has_text {
                expected.insert("text/plain;charset=utf-8".into());
                expected.insert("text/plain".into());
                expected.insert("STRING".into());
                expected.insert("UTF8_STRING".into());
                expected.insert("TEXT".into());
            }
            *guard = expected;
        }

        let source_count = sources.len();
        let total_bytes: usize = sources
            .iter()
            .map(|s| match &s.source {
                wl_copy::Source::Bytes(b) => b.len(),
                _ => 0,
            })
            .sum();

        let first_mime = sources
            .first()
            .map(|s| match &s.mime_type {
                wl_copy::MimeType::Specific(m) => m.clone(),
                _ => String::new(),
            })
            .unwrap_or_default();

        tokio::task::spawn_blocking(move || {
            // Allow wl-clipboard-rs to add standard text aliases
            // (text/plain, UTF8_STRING, STRING, TEXT) so all Linux apps
            // can paste regardless of which MIME type they prefer.
            let opts = wl_copy::Options::new();

            match wl_copy::copy_multi(opts, sources) {
                Ok(()) => {
                    info!(
                        "wl-clipboard: copy_multi OK ({} sources, {} bytes total)",
                        source_count, total_bytes
                    );

                    // Self-paste test: verify the serve thread responds to
                    // paste requests. This exercises the full path through
                    // the compositor (data-control source → Send event →
                    // serve thread → pipe read).
                    match wl_paste::get_contents(
                        wl_paste::ClipboardType::Regular,
                        wl_paste::Seat::Unspecified,
                        wl_paste::MimeType::Specific(&first_mime),
                    ) {
                        Ok((mut pipe, _actual_mime)) => {
                            let mut buf = Vec::new();
                            match pipe.read_to_end(&mut buf) {
                                Ok(_) => {
                                    info!(
                                        "wl-clipboard: self-paste OK ({} bytes for {})",
                                        buf.len(),
                                        first_mime
                                    );
                                }
                                Err(e) => {
                                    warn!(
                                        "wl-clipboard: self-paste read failed: {e}"
                                    );
                                }
                            }
                        }
                        Err(e) => {
                            warn!(
                                "wl-clipboard: self-paste FAILED: {e} (serve thread may not be responding)"
                            );
                        }
                    }

                    Ok(())
                }
                Err(e) => {
                    error!("wl-clipboard: copy_multi FAILED: {e}");
                    Err(ClipboardError::PortalError(format!(
                        "wl-clipboard complete_transfer failed: {e}"
                    )))
                }
            }
        })
        .await
        .map_err(|e| ClipboardError::PortalError(format!("complete_transfer task panicked: {e}")))?
    }

    fn subscribe(&self) -> mpsc::UnboundedReceiver<ClipboardProviderEvent> {
        self.event_rx
            .lock()
            .expect("subscribe called from single thread")
            .take()
            .expect("subscribe() called more than once")
    }

    async fn health_check(&self) -> Result<()> {
        tokio::task::spawn_blocking(|| {
            // Attempt to query MIME types — validates the Wayland connection
            match wl_paste::get_mime_types(
                wl_paste::ClipboardType::Regular,
                wl_paste::Seat::Unspecified,
            ) {
                Ok(_) | Err(wl_paste::Error::ClipboardEmpty | wl_paste::Error::NoSeats) => {
                    debug!("wl-clipboard health check: OK");
                    Ok(())
                }
                Err(e) => Err(ClipboardError::PortalError(format!(
                    "wl-clipboard health check failed: {e}"
                ))),
            }
        })
        .await
        .map_err(|e| ClipboardError::PortalError(format!("health_check task panicked: {e}")))?
    }

    async fn shutdown(&self) {
        self.shutdown.store(true, Ordering::Relaxed);

        // Wait for the monitor thread to exit
        if let Ok(mut guard) = self.poll_handle.lock() {
            if let Some(handle) = guard.take() {
                let _ = handle.join();
            }
        }

        debug!("wl-clipboard provider shut down");
    }
}

/// Check if a MIME type is a text type (used to predict which aliases
/// copy_multi will add for echo-protection filtering).
fn is_text_mime(mime: &str) -> bool {
    mime.starts_with("text/") || mime == "STRING" || mime == "UTF8_STRING" || mime == "TEXT"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_provider_name_compiles() {
        fn assert_provider<T: ClipboardProvider>() {}
        assert_provider::<WlClipboardProvider>();
    }

    #[test]
    fn test_is_text_mime() {
        assert!(is_text_mime("text/plain"));
        assert!(is_text_mime("text/plain;charset=utf-8"));
        assert!(is_text_mime("UTF8_STRING"));
        assert!(is_text_mime("STRING"));
        assert!(is_text_mime("TEXT"));
        assert!(!is_text_mime("image/png"));
        assert!(!is_text_mime("application/octet-stream"));
    }
}
