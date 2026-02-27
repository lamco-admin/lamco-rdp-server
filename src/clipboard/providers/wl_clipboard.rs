//! Standalone Wayland Data-Control Clipboard Provider (via wl-clipboard-rs)
//!
//! Uses `wl-clipboard-rs` to interact with the Wayland clipboard via the
//! ext-data-control-v1 or wlr-data-control-v1 protocol. Unlike the
//! `DataControlClipboardProvider` (which wraps portal-generic's
//! `ClipboardBackend`), this provider is self-contained and composable
//! with any session strategy.
//!
//! # Limitations
//!
//! `wl-clipboard-rs` has no built-in clipboard change notification — it's
//! designed for one-shot copy/paste operations. We poll for changes on a
//! background thread, comparing MIME type sets to detect ownership changes.

use std::{
    collections::HashSet,
    io::Read,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
};

use async_trait::async_trait;
use tokio::sync::mpsc;
use tracing::{debug, info, trace, warn};
use wl_clipboard_rs::{copy as wl_copy, paste as wl_paste};

use crate::clipboard::{
    error::{ClipboardError, Result},
    provider::{ClipboardProvider, ClipboardProviderEvent},
};

/// Polling interval for clipboard change detection.
const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(250);

/// Standalone data-control clipboard provider.
///
/// Connects directly to the compositor's Wayland socket via `wl-clipboard-rs`.
/// No portal daemon or embedded backend required — works with any session
/// strategy as long as the compositor exposes ext-data-control-v1 or
/// wlr-data-control-v1.
pub struct WlClipboardProvider {
    /// Kept alive to prevent the channel from closing (poll thread holds a clone)
    _event_tx: mpsc::UnboundedSender<ClipboardProviderEvent>,
    /// Receiver end (taken by subscribe())
    event_rx: std::sync::Mutex<Option<mpsc::UnboundedReceiver<ClipboardProviderEvent>>>,
    /// Shutdown signal for the polling thread
    shutdown: Arc<AtomicBool>,
    /// Background polling thread handle
    poll_handle: Mutex<Option<std::thread::JoinHandle<()>>>,
    /// MIME types we most recently announced (to filter out our own changes)
    our_mime_types: Arc<Mutex<HashSet<String>>>,
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

        // Spawn a polling thread to detect clipboard changes.
        // wl-clipboard-rs creates its own Wayland connection per call, so
        // each poll is independent and thread-safe.
        let poll_shutdown = Arc::clone(&shutdown);
        let poll_tx = event_tx.clone();
        let poll_ours = Arc::clone(&our_mime_types);

        let poll_handle = std::thread::Builder::new()
            .name("wl-clipboard-poll".into())
            .spawn(move || {
                clipboard_poll_loop(poll_shutdown, poll_tx, poll_ours);
            })
            .ok();

        info!("wl-clipboard-rs clipboard provider created");

        Self {
            _event_tx: event_tx,
            event_rx: std::sync::Mutex::new(Some(event_rx)),
            shutdown,
            poll_handle: Mutex::new(poll_handle),
            our_mime_types,
        }
    }
}

/// Polling loop that runs on a dedicated thread.
///
/// Periodically queries the Wayland clipboard for available MIME types
/// and emits `SelectionChanged` events when the set changes.
fn clipboard_poll_loop(
    shutdown: Arc<AtomicBool>,
    tx: mpsc::UnboundedSender<ClipboardProviderEvent>,
    our_mime_types: Arc<Mutex<HashSet<String>>>,
) {
    let mut last_mime_types: HashSet<String> = HashSet::new();

    while !shutdown.load(Ordering::Relaxed) {
        std::thread::sleep(POLL_INTERVAL);

        if shutdown.load(Ordering::Relaxed) {
            break;
        }

        let current = match wl_paste::get_mime_types(
            wl_paste::ClipboardType::Regular,
            wl_paste::Seat::Unspecified,
        ) {
            Ok(types) => types,
            Err(wl_paste::Error::ClipboardEmpty | wl_paste::Error::NoSeats) => HashSet::new(),
            Err(e) => {
                trace!("wl-clipboard poll error (transient): {e}");
                continue;
            }
        };

        if current != last_mime_types {
            // Check if this change is from our own announce_formats() call
            let is_ours = {
                let guard = our_mime_types
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                !guard.is_empty() && current == *guard
            };

            if !is_ours && !current.is_empty() {
                debug!(
                    "wl-clipboard: selection changed ({} MIME types)",
                    current.len()
                );
                let mime_list: Vec<String> = current.iter().cloned().collect();
                let _ = tx.send(ClipboardProviderEvent::SelectionChanged {
                    mime_types: mime_list,
                    force: true,
                });
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

        // Record what we're announcing so the poll thread can filter it out
        {
            let mut guard = self
                .our_mime_types
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            *guard = mime_types.iter().cloned().collect();
        }

        // wl-clipboard-rs copy: provide empty data for each MIME type.
        // The copy thread serves paste requests by calling back — but we need
        // the data available upfront. For delayed rendering in RDP, we announce
        // empty data initially. The actual data transfer happens via
        // complete_transfer() when a Linux app pastes.
        //
        // Since wl-clipboard-rs doesn't support delayed rendering natively,
        // we announce with placeholder data. The real content comes through
        // when the RDP client responds to a SelectionTransfer event.
        let sources: Vec<wl_copy::MimeSource> = mime_types
            .into_iter()
            .map(|mt| wl_copy::MimeSource {
                source: wl_copy::Source::Bytes(Box::new([])),
                mime_type: wl_copy::MimeType::Specific(mt),
            })
            .collect();

        tokio::task::spawn_blocking(move || {
            let mut opts = wl_copy::Options::new();
            opts.omit_additional_text_mime_types(true);

            wl_copy::copy_multi(opts, sources).map_err(|e| {
                ClipboardError::PortalError(format!("wl-clipboard copy_multi failed: {e}"))
            })
        })
        .await
        .map_err(|e| ClipboardError::PortalError(format!("announce_formats task panicked: {e}")))?
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

        let mime_owned = mime_type.to_string();

        // Re-copy with actual data for this MIME type
        tokio::task::spawn_blocking(move || {
            let mut opts = wl_copy::Options::new();
            opts.omit_additional_text_mime_types(true);

            let source = wl_copy::MimeSource {
                source: wl_copy::Source::Bytes(data.into_boxed_slice()),
                mime_type: wl_copy::MimeType::Specific(mime_owned),
            };

            wl_copy::copy_multi(opts, vec![source]).map_err(|e| {
                ClipboardError::PortalError(format!("wl-clipboard complete_transfer failed: {e}"))
            })
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

        // Wait for the polling thread to exit
        if let Ok(mut guard) = self.poll_handle.lock() {
            if let Some(handle) = guard.take() {
                let _ = handle.join();
            }
        }

        debug!("wl-clipboard provider shut down");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_provider_name_compiles() {
        fn assert_provider<T: ClipboardProvider>() {}
        assert_provider::<WlClipboardProvider>();
    }
}
