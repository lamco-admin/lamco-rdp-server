//! Mutter D-Bus Clipboard Provider
//!
//! Bridges the Mutter RemoteDesktop session's clipboard methods
//! (EnableClipboard, SetSelection, SelectionRead, SelectionWrite)
//! to the `ClipboardProvider` trait.

use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU64, Ordering},
};

use async_trait::async_trait;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use crate::{
    clipboard::{
        error::{ClipboardError, Result},
        provider::{ClipboardProvider, ClipboardProviderEvent},
    },
    mutter::clipboard::MutterClipboard,
};

/// Mutter D-Bus clipboard provider.
///
/// Uses org.gnome.Mutter.RemoteDesktop session clipboard methods directly.
/// GNOME-specific, zero-dialog clipboard sharing.
pub struct MutterClipboardProvider {
    /// Mutter clipboard manager
    clipboard_mgr: Arc<MutterClipboard>,
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
    /// Session generation each signal listener last (re)subscribed on. Compared
    /// against `clipboard_mgr.session_generation()` in `health_check` so a listener
    /// left on a dead session surfaces as unhealthy instead of silently healthy.
    owner_gen: Arc<AtomicU64>,
    transfer_gen: Arc<AtomicU64>,
}

impl MutterClipboardProvider {
    /// Create a Mutter clipboard provider from a clipboard manager.
    ///
    /// Enables clipboard and starts signal listeners.
    pub(crate) async fn new(
        clipboard_mgr: Arc<MutterClipboard>,
    ) -> std::result::Result<Self, anyhow::Error> {
        let (event_tx, event_rx) = mpsc::unbounded_channel();
        let (shutdown_broadcast, _) = tokio::sync::broadcast::channel(16);

        // Enable clipboard on the Mutter session (skip if already enabled
        // by the session manager during setup — Mutter rejects duplicate calls)
        if !clipboard_mgr.is_enabled().await {
            clipboard_mgr.enable().await?;
        }

        let provider = Self {
            clipboard_mgr,
            event_tx,
            event_rx: std::sync::Mutex::new(Some(event_rx)),
            shutdown: Arc::new(AtomicBool::new(false)),
            shutdown_broadcast,
            task_handles: tokio::sync::Mutex::new(Vec::new()),
            owner_gen: Arc::new(AtomicU64::new(0)),
            transfer_gen: Arc::new(AtomicU64::new(0)),
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
        let event_tx = self.event_tx.clone();
        let mut shutdown_rx = self.shutdown_broadcast.subscribe();
        // Subscribe to rebind BEFORE spawning so no session re-establishment can
        // slip past between spawn and the first `recv`.
        let mut rebind_rx = self.clipboard_mgr.subscribe_rebind();
        let clipboard_mgr = Arc::clone(&self.clipboard_mgr);
        let owner_gen = Arc::clone(&self.owner_gen);

        // The Mutter RemoteDesktop session is re-established on every reconnect
        // (MutterClipboard::rebind). Re-subscription is driven by the rebind
        // signal — NOT by stream-end, because destroying the session leaves the
        // zbus signal-match stream open-but-silent, so `next()` never returns
        // `None` and a stream-end-only listener would sit on the dead session
        // forever (Linux→RDP copy silently dead on the 2nd+ connection).
        let handle = tokio::spawn(async move {
            use futures_util::StreamExt;
            'outer: loop {
                let generation = clipboard_mgr.session_generation();
                let mut stream = match clipboard_mgr.subscribe_selection_owner_changed().await {
                    Ok(s) => {
                        info!(
                            generation,
                            "[mutter-clipboard] SelectionOwnerChanged (L→W) listener bound to live session"
                        );
                        owner_gen.store(generation, Ordering::Release);
                        let _ = event_tx.send(ClipboardProviderEvent::ListenerHealth {
                            healthy: true,
                            reason: "SelectionOwnerChanged (L→W) listener bound to live session"
                                .to_string(),
                        });
                        s
                    }
                    Err(e) => {
                        warn!(
                            generation,
                            "[mutter-clipboard] SelectionOwnerChanged subscribe failed \
                             (Linux→RDP copy-detect down), retrying: {e}"
                        );
                        let _ = event_tx.send(ClipboardProviderEvent::ListenerHealth {
                            healthy: false,
                            reason: format!(
                                "SelectionOwnerChanged (L→W) listener could not bind: {e}"
                            ),
                        });
                        tokio::select! {
                            () = tokio::time::sleep(std::time::Duration::from_millis(500)) => continue 'outer,
                            _ = shutdown_rx.recv() => break 'outer,
                        }
                    }
                };
                loop {
                    tokio::select! {
                        // Session re-established — drop the (now-dead) stream and
                        // re-subscribe on the new session path.
                        _ = rebind_rx.recv() => {
                            info!(
                                "[mutter-clipboard] SelectionOwnerChanged listener: session \
                                 re-established — re-subscribing on new session"
                            );
                            continue 'outer;
                        }
                        msg = stream.next() => {
                            let Some(msg) = msg else {
                                info!("[mutter-clipboard] SelectionOwnerChanged stream ended — re-subscribing");
                                tokio::select! {
                                    () = tokio::time::sleep(std::time::Duration::from_millis(500)) => {}
                                    _ = shutdown_rx.recv() => break 'outer,
                                }
                                continue 'outer;
                            };
                            let mime_types = parse_selection_owner_changed(&msg);
                            if !mime_types.is_empty() {
                                debug!("Mutter SelectionOwnerChanged: {} types", mime_types.len());
                                if event_tx
                                    .send(ClipboardProviderEvent::SelectionChanged {
                                        mime_types,
                                        force: true,
                                    })
                                    .is_err()
                                {
                                    break 'outer;
                                }
                            }
                        }
                        _ = shutdown_rx.recv() => break 'outer,
                    }
                }
            }
        });

        self.task_handles.lock().await.push(handle);
    }

    async fn start_transfer_listener(&self) {
        let event_tx = self.event_tx.clone();
        let mut shutdown_rx = self.shutdown_broadcast.subscribe();
        let mut rebind_rx = self.clipboard_mgr.subscribe_rebind();
        let clipboard_mgr = Arc::clone(&self.clipboard_mgr);
        let transfer_gen = Arc::clone(&self.transfer_gen);

        // Mirrors the owner-changed listener: re-subscribe on the rebind signal so
        // RDP→Linux paste (SelectionTransfer) survives reconnects instead of dying
        // silently on the destroyed session.
        let handle = tokio::spawn(async move {
            use futures_util::StreamExt;
            'outer: loop {
                let generation = clipboard_mgr.session_generation();
                let mut stream = match clipboard_mgr.subscribe_selection_transfer().await {
                    Ok(s) => {
                        info!(
                            generation,
                            "[mutter-clipboard] SelectionTransfer (W→L) listener bound to live session"
                        );
                        transfer_gen.store(generation, Ordering::Release);
                        let _ = event_tx.send(ClipboardProviderEvent::ListenerHealth {
                            healthy: true,
                            reason: "SelectionTransfer (W→L) listener bound to live session"
                                .to_string(),
                        });
                        s
                    }
                    Err(e) => {
                        warn!(
                            generation,
                            "[mutter-clipboard] SelectionTransfer subscribe failed \
                             (RDP→Linux paste down), retrying: {e}"
                        );
                        let _ = event_tx.send(ClipboardProviderEvent::ListenerHealth {
                            healthy: false,
                            reason: format!("SelectionTransfer (W→L) listener could not bind: {e}"),
                        });
                        tokio::select! {
                            () = tokio::time::sleep(std::time::Duration::from_millis(500)) => continue 'outer,
                            _ = shutdown_rx.recv() => break 'outer,
                        }
                    }
                };
                loop {
                    tokio::select! {
                        _ = rebind_rx.recv() => {
                            info!(
                                "[mutter-clipboard] SelectionTransfer listener: session \
                                 re-established — re-subscribing on new session"
                            );
                            continue 'outer;
                        }
                        msg = stream.next() => {
                            let Some(msg) = msg else {
                                info!("[mutter-clipboard] SelectionTransfer stream ended — re-subscribing");
                                tokio::select! {
                                    () = tokio::time::sleep(std::time::Duration::from_millis(500)) => {}
                                    _ = shutdown_rx.recv() => break 'outer,
                                }
                                continue 'outer;
                            };
                            if let Some((serial, mime_type)) = parse_selection_transfer(&msg) {
                                debug!("Mutter SelectionTransfer: {} (serial {})", mime_type, serial);
                                if event_tx
                                    .send(ClipboardProviderEvent::SelectionTransfer { serial, mime_type })
                                    .is_err()
                                {
                                    break 'outer;
                                }
                            }
                        }
                        _ = shutdown_rx.recv() => break 'outer,
                    }
                }
            }
        });

        self.task_handles.lock().await.push(handle);
    }
}

/// Parse MIME types from a Mutter SelectionOwnerChanged D-Bus message.
///
/// The signal body contains options dict with "mime-types" key.
/// Mutter wraps the string array in a tuple: the GVariant format is `(^as)`,
/// so the value is a Structure containing an Array of strings.
fn parse_selection_owner_changed(msg: &zbus::Message) -> Vec<String> {
    match msg
        .body()
        .deserialize::<std::collections::HashMap<String, zbus::zvariant::OwnedValue>>()
    {
        Ok(options) => {
            if let Some(value) = options.get("mime-types") {
                return extract_string_array(value);
            }
            Vec::new()
        }
        Err(e) => {
            debug!("Failed to parse SelectionOwnerChanged body: {e}");
            Vec::new()
        }
    }
}

/// Extract a string array from a zvariant Value.
///
/// Handles both bare arrays (`as`) and Mutter's tuple-wrapped format (`(as)`).
fn extract_string_array(value: &zbus::zvariant::Value<'_>) -> Vec<String> {
    use zbus::zvariant::Value;

    // Try bare array first (as)
    if let Value::Array(arr) = value {
        let types: Vec<String> = arr
            .iter()
            .filter_map(|item| {
                if let Value::Str(s) = item {
                    Some(s.to_string())
                } else {
                    None
                }
            })
            .collect();
        if !types.is_empty() {
            return types;
        }
    }

    // Mutter sends (^as) — a tuple wrapping the string array
    if let Value::Structure(s) = value {
        for field in s.fields() {
            let types = extract_string_array(field);
            if !types.is_empty() {
                return types;
            }
        }
    }

    Vec::new()
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
            .map_err(|e| {
                ClipboardError::PortalError(format!("Mutter SetSelection failed: {e:#}"))
            })?;
        Ok(())
    }

    async fn read_data(&self, mime_type: &str) -> Result<Vec<u8>> {
        self.clipboard_mgr
            .read_selection(mime_type)
            .await
            .map_err(|e| ClipboardError::PortalError(format!("Mutter SelectionRead failed: {e:#}")))
    }

    async fn on_remote_gone(&self) -> Result<()> {
        // Mutter has no "release ownership but keep listening" primitive: an
        // empty SetSelection is rejected ("Failed to set selection"), and
        // DisableClipboard would tear down the SelectionOwnerChanged listener the
        // next client needs. There is nothing safe to call — ownership transfers
        // naturally when a local app next copies, and the orchestrator's
        // rdp_ready gate already prevents serving a disconnected remote's
        // clipboard. So this is intentionally a no-op on the Mutter path.
        Ok(())
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
                ClipboardError::PortalError(format!("Mutter SelectionWrite failed: {e:#}"))
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
        if !self.clipboard_mgr.is_enabled().await {
            return Err(ClipboardError::PortalError(
                "Mutter clipboard not enabled".to_string(),
            ));
        }
        // The method path (SetSelection/read/write rebuilds a proxy from the
        // current path each call) can look healthy while the signal listeners are
        // stranded on a destroyed session — the silent copy/paste-dead failure.
        // Verify both listeners are bound to the current session generation.
        let current = self.clipboard_mgr.session_generation();
        let owner = self.owner_gen.load(Ordering::Acquire);
        let transfer = self.transfer_gen.load(Ordering::Acquire);
        if owner != current || transfer != current {
            return Err(ClipboardError::PortalError(format!(
                "clipboard signal listeners stale (session gen {current}; SelectionOwnerChanged \
                 gen {owner}, SelectionTransfer gen {transfer}) — copy/paste would be silently dead"
            )));
        }
        Ok(())
    }

    async fn shutdown(&self) {
        self.shutdown.store(true, Ordering::Relaxed);
        let _ = self.shutdown_broadcast.send(());

        if let Err(e) = self.clipboard_mgr.disable().await {
            // Expected when the compositor already destroyed the session (e.g.
            // the user clicked GNOME's "stop sharing"): the RemoteDesktop D-Bus
            // object is gone, so DisableClipboard cannot succeed. Best-effort
            // cleanup — not worth a warning.
            debug!("Mutter clipboard disable skipped (session likely already closed): {e}");
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
