//! Data-Control Clipboard Provider (Wayland ext-data-control / wlr-data-control)
//!
//! Bridges `xdg_desktop_portal_generic::ClipboardBackend` to the `ClipboardProvider`
//! trait. All ClipboardBackend methods are synchronous, so they run on
//! `tokio::task::spawn_blocking`.

use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};

use async_trait::async_trait;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};
use xdg_desktop_portal_generic::ClipboardBackend;

use crate::clipboard::{
    error::{ClipboardError, Result},
    provider::{ClipboardProvider, ClipboardProviderEvent},
};

/// Data-control clipboard provider.
///
/// Uses native Wayland data-control protocols (ext-data-control-v1 or
/// wlr-data-control-v1) via the portal-generic library. No Portal daemon
/// or D-Bus required.
pub struct DataControlClipboardProvider {
    /// The underlying clipboard backend from portal-generic
    backend: Arc<Mutex<Box<dyn ClipboardBackend>>>,
    /// Channel for sending events to the orchestrator
    event_tx: mpsc::UnboundedSender<ClipboardProviderEvent>,
    /// Receiver end (taken by subscribe())
    event_rx: std::sync::Mutex<Option<mpsc::UnboundedReceiver<ClipboardProviderEvent>>>,
    /// Shutdown signal
    shutdown: Arc<AtomicBool>,
}

impl DataControlClipboardProvider {
    /// Create a new data-control clipboard provider.
    pub fn new(backend: Arc<Mutex<Box<dyn ClipboardBackend>>>) -> Self {
        let (event_tx, event_rx) = mpsc::unbounded_channel();

        // Hook the selection-changed callback to emit events
        let tx_clone = event_tx.clone();
        {
            let mut guard = backend.lock().expect("backend lock not poisoned");
            guard.on_selection_changed(Box::new(move |mime_types| {
                // Data-control signals are always authoritative: the compositor only fires
                // the selection event when a DIFFERENT client takes ownership
                let _ = tx_clone.send(ClipboardProviderEvent::SelectionChanged {
                    mime_types,
                    force: true,
                });
            }));
        }

        info!("Data-control clipboard provider created");

        Self {
            backend,
            event_tx,
            event_rx: std::sync::Mutex::new(Some(event_rx)),
            shutdown: Arc::new(AtomicBool::new(false)),
        }
    }
}

#[async_trait]
impl ClipboardProvider for DataControlClipboardProvider {
    fn name(&self) -> &'static str {
        "data-control"
    }

    fn supports_file_transfer(&self) -> bool {
        // Data-control supports arbitrary MIME types including text/uri-list
        true
    }

    async fn announce_formats(&self, mime_types: Vec<String>) -> Result<()> {
        let backend = Arc::clone(&self.backend);

        tokio::task::spawn_blocking(move || {
            let mut guard = backend
                .lock()
                .map_err(|e| ClipboardError::PortalError(format!("Backend lock poisoned: {e}")))?;

            // Build ClipboardData with MIME types to announce (empty data map for delayed rendering)
            let data = xdg_desktop_portal_generic::types::ClipboardData {
                mime_types,
                data: std::collections::HashMap::new(),
            };

            guard
                .set_clipboard(data)
                .map_err(|e| ClipboardError::PortalError(format!("set_clipboard failed: {e}")))?;

            Ok(())
        })
        .await
        .map_err(|e| ClipboardError::PortalError(format!("announce_formats task panicked: {e}")))?
    }

    async fn read_data(&self, mime_type: &str) -> Result<Vec<u8>> {
        let backend = Arc::clone(&self.backend);
        let mime_owned = mime_type.to_string();

        tokio::task::spawn_blocking(move || {
            let guard = backend
                .lock()
                .map_err(|e| ClipboardError::PortalError(format!("Backend lock poisoned: {e}")))?;

            match guard.read_selection(&mime_owned) {
                Ok(Some(data)) => {
                    debug!("data-control: read {} bytes for {}", data.len(), mime_owned);
                    Ok(data)
                }
                Ok(None) => {
                    warn!("data-control: no data available for {}", mime_owned);
                    Ok(Vec::new())
                }
                Err(e) => Err(ClipboardError::PortalError(format!(
                    "read_selection failed: {e}"
                ))),
            }
        })
        .await
        .map_err(|e| ClipboardError::PortalError(format!("read_data task panicked: {e}")))?
    }

    async fn complete_transfer(
        &self,
        serial: u32,
        _mime_type: &str,
        _data: Vec<u8>,
        success: bool,
    ) -> Result<()> {
        let backend = Arc::clone(&self.backend);

        tokio::task::spawn_blocking(move || {
            let mut guard = backend
                .lock()
                .map_err(|e| ClipboardError::PortalError(format!("Backend lock poisoned: {e}")))?;

            guard
                .write_done(serial, success)
                .map_err(|e| ClipboardError::PortalError(format!("write_done failed: {e}")))?;

            Ok(())
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
        let backend = Arc::clone(&self.backend);
        tokio::task::spawn_blocking(move || {
            let guard = backend
                .lock()
                .map_err(|e| ClipboardError::PortalError(format!("Backend lock poisoned: {e}")))?;
            let protocol = guard.protocol_type();
            debug!("data-control health check: protocol={:?}", protocol);
            Ok(())
        })
        .await
        .map_err(|e| ClipboardError::PortalError(format!("health_check panicked: {e}")))?
    }

    async fn shutdown(&self) {
        self.shutdown.store(true, Ordering::Relaxed);
        debug!("Data-control clipboard provider shut down");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_provider_name_compiles() {
        fn assert_provider<T: ClipboardProvider>() {}
        assert_provider::<DataControlClipboardProvider>();
    }
}
