//! Mutter D-Bus Clipboard Provider
//!
//! Bridges the Mutter RemoteDesktop session's clipboard methods
//! (EnableClipboard, SetSelection, SelectionRead, SelectionWrite)
//! to the `ClipboardProvider` trait.

use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

use async_trait::async_trait;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use crate::{
    clipboard::{
        error::{ClipboardError, Result},
        provider::{ClipboardProvider, ClipboardProviderEvent},
    },
    mutter::clipboard::MutterClipboardManager,
};

/// Mutter D-Bus clipboard provider.
///
/// Uses org.gnome.Mutter.RemoteDesktop session clipboard methods directly.
/// GNOME-specific, zero-dialog clipboard sharing.
pub struct MutterClipboardProvider {
    /// Mutter clipboard manager
    clipboard_mgr: Arc<MutterClipboardManager>,
    /// Channel for sending events to the orchestrator
    event_tx: mpsc::UnboundedSender<ClipboardProviderEvent>,
    /// Receiver end (taken by subscribe())
    event_rx: std::sync::Mutex<Option<mpsc::UnboundedReceiver<ClipboardProviderEvent>>>,
    /// Shutdown signal
    shutdown: Arc<AtomicBool>,
    /// Shutdown broadcast for async tasks
    shutdown_broadcast: tokio::sync::broadcast::Sender<()>,
    /// Task handles for cleanup
    task_handles: tokio::sync::Mutex<Vec<tokio::task::JoinHandle<()>>>,
}

impl MutterClipboardProvider {
    /// Create a Mutter clipboard provider from a clipboard manager.
    ///
    /// Enables clipboard and starts signal listeners.
    pub(crate) async fn new(
        clipboard_mgr: Arc<MutterClipboardManager>,
    ) -> std::result::Result<Self, anyhow::Error> {
        let (event_tx, event_rx) = mpsc::unbounded_channel();
        let (shutdown_broadcast, _) = tokio::sync::broadcast::channel(16);

        // Enable clipboard on the Mutter session
        clipboard_mgr.enable().await?;

        let provider = Self {
            clipboard_mgr,
            event_tx,
            event_rx: std::sync::Mutex::new(Some(event_rx)),
            shutdown: Arc::new(AtomicBool::new(false)),
            shutdown_broadcast,
            task_handles: tokio::sync::Mutex::new(Vec::new()),
        };

        provider.start_listeners().await;
        Ok(provider)
    }

    /// Start SelectionOwnerChanged and SelectionTransfer listeners
    async fn start_listeners(&self) {
        self.start_owner_changed_listener().await;
        self.start_transfer_listener().await;
    }

    async fn start_owner_changed_listener(&self) {
        use futures_util::StreamExt;

        match self.clipboard_mgr.subscribe_selection_owner_changed().await {
            Ok(mut stream) => {
                let event_tx = self.event_tx.clone();
                let mut shutdown_rx = self.shutdown_broadcast.subscribe();

                let handle = tokio::spawn(async move {
                    loop {
                        tokio::select! {
                            msg = stream.next() => {
                                let Some(msg) = msg else { break };
                                // Parse MIME types from the D-Bus message body
                                let mime_types = parse_selection_owner_changed(&msg);
                                if !mime_types.is_empty() {
                                    debug!(
                                        "Mutter SelectionOwnerChanged: {} types",
                                        mime_types.len()
                                    );
                                    if event_tx
                                        .send(ClipboardProviderEvent::SelectionChanged {
                                            mime_types,
                                            force: true,
                                        })
                                        .is_err()
                                    {
                                        break;
                                    }
                                }
                            }
                            _ = shutdown_rx.recv() => break,
                        }
                    }
                });

                self.task_handles.lock().await.push(handle);
                info!("Mutter SelectionOwnerChanged listener started");
            }
            Err(e) => {
                warn!("Failed to subscribe to Mutter SelectionOwnerChanged: {e}");
            }
        }
    }

    async fn start_transfer_listener(&self) {
        use futures_util::StreamExt;

        match self.clipboard_mgr.subscribe_selection_transfer().await {
            Ok(mut stream) => {
                let event_tx = self.event_tx.clone();
                let mut shutdown_rx = self.shutdown_broadcast.subscribe();

                let handle = tokio::spawn(async move {
                    loop {
                        tokio::select! {
                            msg = stream.next() => {
                                let Some(msg) = msg else { break };
                                // Parse serial and MIME type from the D-Bus message
                                if let Some((serial, mime_type)) = parse_selection_transfer(&msg) {
                                    debug!(
                                        "Mutter SelectionTransfer: {} (serial {})",
                                        mime_type, serial
                                    );
                                    if event_tx
                                        .send(ClipboardProviderEvent::SelectionTransfer {
                                            serial,
                                            mime_type,
                                        })
                                        .is_err()
                                    {
                                        break;
                                    }
                                }
                            }
                            _ = shutdown_rx.recv() => break,
                        }
                    }
                });

                self.task_handles.lock().await.push(handle);
                info!("Mutter SelectionTransfer listener started");
            }
            Err(e) => {
                warn!("Failed to subscribe to Mutter SelectionTransfer: {e}");
            }
        }
    }
}

/// Parse MIME types from a Mutter SelectionOwnerChanged D-Bus message.
///
/// The signal body contains options dict with "mime-types" key.
fn parse_selection_owner_changed(msg: &zbus::Message) -> Vec<String> {
    // Mutter sends: SelectionOwnerChanged(options: a{sv})
    // options["mime-types"] = as (array of strings)
    match msg
        .body()
        .deserialize::<std::collections::HashMap<String, zbus::zvariant::OwnedValue>>()
    {
        Ok(options) => {
            if let Some(value) = options.get("mime-types") {
                // OwnedValue derefs to Value<'static>; extract array elements as strings
                if let zbus::zvariant::Value::Array(arr) = &**value {
                    let mut types = Vec::new();
                    for item in arr.iter() {
                        if let zbus::zvariant::Value::Str(s) = item {
                            types.push(s.to_string());
                        }
                    }
                    if !types.is_empty() {
                        return types;
                    }
                }
            }
            Vec::new()
        }
        Err(e) => {
            debug!("Failed to parse SelectionOwnerChanged body: {e}");
            Vec::new()
        }
    }
}

/// Parse serial and MIME type from a Mutter SelectionTransfer D-Bus message.
///
/// The signal body contains: (mime_type: s, serial: u)
fn parse_selection_transfer(msg: &zbus::Message) -> Option<(u32, String)> {
    match msg.body().deserialize::<(String, u32)>() {
        Ok((mime_type, serial)) => Some((serial, mime_type)),
        Err(e) => {
            debug!("Failed to parse SelectionTransfer body: {e}");
            None
        }
    }
}

#[async_trait]
impl ClipboardProvider for MutterClipboardProvider {
    fn name(&self) -> &'static str {
        "Mutter"
    }

    fn supports_file_transfer(&self) -> bool {
        // Mutter clipboard supports arbitrary MIME types including file URIs,
        // but in practice it's best-effort for binary formats
        true
    }

    async fn announce_formats(&self, mime_types: Vec<String>) -> Result<()> {
        self.clipboard_mgr
            .set_selection(&mime_types)
            .await
            .map_err(|e| ClipboardError::PortalError(format!("Mutter SetSelection failed: {e}")))?;
        Ok(())
    }

    async fn read_data(&self, mime_type: &str) -> Result<Vec<u8>> {
        self.clipboard_mgr
            .read_selection(mime_type)
            .await
            .map_err(|e| ClipboardError::PortalError(format!("Mutter SelectionRead failed: {e}")))
    }

    async fn complete_transfer(
        &self,
        serial: u32,
        _mime_type: &str,
        data: Vec<u8>,
        _success: bool,
    ) -> Result<()> {
        self.clipboard_mgr
            .write_selection(serial, &data)
            .await
            .map_err(|e| {
                ClipboardError::PortalError(format!("Mutter SelectionWrite failed: {e}"))
            })?;
        Ok(())
    }

    #[expect(
        clippy::expect_used,
        reason = "subscribe() is a one-shot initialization call"
    )]
    fn subscribe(&self) -> mpsc::UnboundedReceiver<ClipboardProviderEvent> {
        self.event_rx
            .lock()
            .expect("subscribe called from single thread")
            .take()
            .expect("subscribe() called more than once")
    }

    async fn health_check(&self) -> Result<()> {
        if self.clipboard_mgr.is_enabled().await {
            Ok(())
        } else {
            Err(ClipboardError::PortalError(
                "Mutter clipboard not enabled".to_string(),
            ))
        }
    }

    async fn shutdown(&self) {
        self.shutdown.store(true, Ordering::Relaxed);
        let _ = self.shutdown_broadcast.send(());

        if let Err(e) = self.clipboard_mgr.disable().await {
            warn!("Failed to disable Mutter clipboard on shutdown: {e}");
        }

        let mut handles = self.task_handles.lock().await;
        for handle in handles.drain(..) {
            handle.abort();
        }
        debug!("Mutter clipboard provider shut down");
    }
}

impl Drop for MutterClipboardProvider {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Relaxed);
        let _ = self.shutdown_broadcast.send(());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_provider_compiles() {
        fn assert_provider<T: ClipboardProvider>() {}
        assert_provider::<MutterClipboardProvider>();
    }
}
