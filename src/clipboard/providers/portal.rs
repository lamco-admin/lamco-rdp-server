//! Portal-based Clipboard Provider
//!
//! Wraps `crate::portal::PortalClipboardManager` and `ashpd::desktop::Session`
//! into the `ClipboardProvider` trait. Manages the three listener tasks
//! (SelectionTransfer, SelectionOwnerChanged, D-Bus bridge) internally.

use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

use async_trait::async_trait;
use lamco_portal::dbus_clipboard::DbusClipboardBridge;
use tokio::sync::{mpsc, RwLock};
use tracing::{debug, error, info, warn};

use crate::clipboard::{
    error::{ClipboardError, Result},
    provider::{ClipboardProvider, ClipboardProviderEvent},
};

/// Portal clipboard provider.
///
/// Manages clipboard via Portal D-Bus APIs (SetSelection, SelectionRead,
/// SelectionWrite, SelectionOwnerChanged, SelectionTransfer).
///
/// For GNOME, also starts a D-Bus bridge listener as fallback for unreliable
/// Portal SelectionOwnerChanged signals.
pub struct PortalClipboardProvider {
    /// Portal clipboard manager from lamco-portal
    portal: Arc<crate::portal::PortalClipboardManager>,
    /// Portal RemoteDesktop session (shared with input handler)
    session: Arc<
        RwLock<
            ashpd::desktop::Session<
                'static,
                ashpd::desktop::remote_desktop::RemoteDesktop<'static>,
            >,
        >,
    >,
    /// Session validity — mirrors the flag on PortalSessionHandleImpl.
    /// When false, the compositor has destroyed the session and all
    /// Portal D-Bus calls will fail with "Invalid session".
    session_valid: Arc<AtomicBool>,
    /// Channel for sending events to the orchestrator
    event_tx: mpsc::UnboundedSender<ClipboardProviderEvent>,
    /// Receiver end (taken by subscribe())
    event_rx: std::sync::Mutex<Option<mpsc::UnboundedReceiver<ClipboardProviderEvent>>>,
    /// Shutdown signal for listener tasks
    shutdown: Arc<AtomicBool>,
    /// Shutdown broadcast for async select
    shutdown_broadcast: tokio::sync::broadcast::Sender<()>,
    /// Listener task handles
    task_handles: tokio::sync::Mutex<Vec<tokio::task::JoinHandle<()>>>,
    /// Recently written content hashes (loop suppression for D-Bus bridge)
    recently_written_hashes: Arc<RwLock<std::collections::HashMap<String, std::time::Instant>>>,
    /// Rate limit interval for D-Bus bridge events
    rate_limit_ms: u64,
}

impl PortalClipboardProvider {
    /// Create a new Portal clipboard provider.
    ///
    /// `portal`: Portal clipboard manager (may be None if Portal v1 without clipboard)
    /// `session`: Portal RemoteDesktop session
    /// `session_valid`: Shared validity flag from the Portal session handle
    /// `rate_limit_ms`: Minimum interval between forwarded D-Bus events
    pub async fn new(
        portal: Arc<crate::portal::PortalClipboardManager>,
        session: Arc<
            RwLock<
                ashpd::desktop::Session<
                    'static,
                    ashpd::desktop::remote_desktop::RemoteDesktop<'static>,
                >,
            >,
        >,
        session_valid: Arc<AtomicBool>,
        rate_limit_ms: u64,
    ) -> Self {
        let (event_tx, event_rx) = mpsc::unbounded_channel();
        let (shutdown_broadcast, _) = tokio::sync::broadcast::channel(16);

        let provider = Self {
            portal,
            session,
            session_valid,
            event_tx,
            event_rx: std::sync::Mutex::new(Some(event_rx)),
            shutdown: Arc::new(AtomicBool::new(false)),
            shutdown_broadcast,
            task_handles: tokio::sync::Mutex::new(Vec::new()),
            recently_written_hashes: Arc::new(RwLock::new(std::collections::HashMap::new())),
            rate_limit_ms,
        };

        provider.start_listeners().await;
        provider
    }

    /// Start all listener tasks (SelectionTransfer, SelectionOwnerChanged, D-Bus bridge)
    async fn start_listeners(&self) {
        self.start_selection_transfer_listener().await;
        self.start_owner_changed_listener().await;
        self.start_dbus_clipboard_listener().await;
    }

    /// SelectionTransfer listener: handles delayed rendering (Windows -> Linux paste)
    async fn start_selection_transfer_listener(&self) {
        let (transfer_tx, mut transfer_rx) = mpsc::unbounded_channel();

        match self
            .portal
            .start_selection_transfer_listener(transfer_tx)
            .await
        {
            Ok(()) => {
                let event_tx = self.event_tx.clone();
                let mut shutdown_rx = self.shutdown_broadcast.subscribe();

                let handle = tokio::spawn(async move {
                    // Track last forwarded time per MIME type to suppress compositor
                    // retries. The compositor sends SelectionTransfer every ~500ms with
                    // incrementing serials for the same MIME when a paste is held open.
                    // A 2s cooldown per MIME prevents flooding the RDP client with
                    // redundant requests while still allowing new MIME types through.
                    let mut last_forwarded: std::collections::HashMap<String, std::time::Instant> =
                        std::collections::HashMap::new();

                    loop {
                        let transfer_event = tokio::select! {
                            Some(event) = transfer_rx.recv() => event,
                            _ = shutdown_rx.recv() => {
                                info!("SelectionTransfer handler received shutdown");
                                break;
                            }
                        };

                        let now = std::time::Instant::now();

                        if let Some(last) = last_forwarded.get(&transfer_event.mime_type) {
                            if now.duration_since(*last).as_millis() < 2000 {
                                debug!(
                                    "Suppressing repeat SelectionTransfer for {} (serial {}, {}ms since last)",
                                    transfer_event.mime_type,
                                    transfer_event.serial,
                                    now.duration_since(*last).as_millis(),
                                );
                                continue;
                            }
                        }
                        last_forwarded.insert(transfer_event.mime_type.clone(), now);

                        info!(
                            "SelectionTransfer: {} (serial {})",
                            transfer_event.mime_type, transfer_event.serial
                        );

                        if event_tx
                            .send(ClipboardProviderEvent::SelectionTransfer {
                                serial: transfer_event.serial,
                                mime_type: transfer_event.mime_type,
                            })
                            .is_err()
                        {
                            break;
                        }
                    }
                });

                self.task_handles.lock().await.push(handle);
                info!("Portal SelectionTransfer listener started");
            }
            Err(e) => {
                error!("Failed to start SelectionTransfer listener: {e:#}");
                warn!("Delayed rendering (Windows -> Linux paste) will not work");
            }
        }
    }

    /// SelectionOwnerChanged listener: detects Linux clipboard changes
    async fn start_owner_changed_listener(&self) {
        let (owner_tx, mut owner_rx) = mpsc::unbounded_channel();

        match self.portal.start_owner_changed_listener(owner_tx).await {
            Ok(()) => {
                let event_tx = self.event_tx.clone();
                let mut shutdown_rx = self.shutdown_broadcast.subscribe();

                let handle = tokio::spawn(async move {
                    loop {
                        let mime_types = tokio::select! {
                            Some(types) = owner_rx.recv() => types,
                            _ = shutdown_rx.recv() => {
                                info!("SelectionOwnerChanged handler received shutdown");
                                break;
                            }
                        };

                        // Portal already filtered echoes (session_is_owner=true)
                        // so these are always external clipboard changes
                        debug!("SelectionOwnerChanged: {} MIME types", mime_types.len());

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
                });

                self.task_handles.lock().await.push(handle);
                debug!("Portal SelectionOwnerChanged listener started");
            }
            Err(e) => {
                error!("Failed to start SelectionOwnerChanged listener: {e:#}");
                warn!("Linux -> Windows clipboard flow will not work via Portal signals");
            }
        }
    }

    /// D-Bus clipboard bridge listener (GNOME fallback for unreliable Portal signals)
    async fn start_dbus_clipboard_listener(&self) {
        if !DbusClipboardBridge::is_available().await {
            debug!("GNOME clipboard extension not detected, D-Bus bridge inactive");
            return;
        }

        let bridge = match DbusClipboardBridge::connect().await {
            Ok(b) => b,
            Err(e) => {
                error!("Failed to connect to D-Bus clipboard bridge: {e}");
                return;
            }
        };

        let mut dbus_rx = bridge.subscribe();
        let event_tx = self.event_tx.clone();
        let recently_written_hashes = Arc::clone(&self.recently_written_hashes);
        let rate_limit_ms = self.rate_limit_ms;
        let mut shutdown_rx = self.shutdown_broadcast.subscribe();

        // Hash cleanup task
        let hashes_for_cleanup = Arc::clone(&self.recently_written_hashes);
        let mut shutdown_rx2 = self.shutdown_broadcast.subscribe();

        let cleanup_handle = tokio::spawn(async move {
            loop {
                tokio::select! {
                    () = tokio::time::sleep(tokio::time::Duration::from_secs(1)) => {
                        let mut hashes = hashes_for_cleanup.write().await;
                        let now = std::time::Instant::now();
                        hashes.retain(|_, written_at| {
                            now.duration_since(*written_at).as_millis() < 2000
                        });
                        while hashes.len() > 50 {
                            if let Some(oldest) = hashes
                                .iter()
                                .min_by_key(|(_, time)| *time)
                                .map(|(k, _)| k.clone())
                            {
                                hashes.remove(&oldest);
                            } else {
                                break;
                            }
                        }
                    }
                    _ = shutdown_rx2.recv() => break,
                }
            }
        });
        self.task_handles.lock().await.push(cleanup_handle);

        // D-Bus event forwarder
        // Keep the bridge alive by moving it into the task
        let forwarder_handle = tokio::spawn(async move {
            let _bridge = bridge; // prevent drop
            let mut last_forward_time: Option<std::time::Instant> = None;

            loop {
                tokio::select! {
                    Ok(dbus_event) = dbus_rx.recv() => {
                        // Rate limiting
                        if rate_limit_ms > 0 {
                            if let Some(last_time) = last_forward_time {
                                if last_time.elapsed().as_millis() < rate_limit_ms as u128 {
                                    continue;
                                }
                            }
                        }

                        // Loop suppression: skip events matching data we recently wrote
                        {
                            let hashes = recently_written_hashes.read().await;
                            if hashes.contains_key(&dbus_event.content_hash) {
                                debug!(
                                    "Loop suppressed: D-Bus hash {} matches recent write",
                                    &dbus_event.content_hash[..8.min(dbus_event.content_hash.len())]
                                );
                                continue;
                            }
                        }

                        last_forward_time = Some(std::time::Instant::now());

                        // D-Bus extension signals are authoritative
                        if event_tx
                            .send(ClipboardProviderEvent::SelectionChanged {
                                mime_types: dbus_event.mime_types,
                                force: true,
                            })
                            .is_err()
                        {
                            break;
                        }
                    }
                    _ = shutdown_rx.recv() => break,
                }
            }
        });
        self.task_handles.lock().await.push(forwarder_handle);

        info!("D-Bus clipboard bridge started (GNOME fallback)");
    }
}

#[async_trait]
impl ClipboardProvider for PortalClipboardProvider {
    fn name(&self) -> &'static str {
        "Portal"
    }

    fn supports_file_transfer(&self) -> bool {
        true
    }

    async fn announce_formats(&self, mime_types: Vec<String>) -> Result<()> {
        if !self.session_valid.load(Ordering::Acquire) {
            return Err(ClipboardError::PortalError(
                "Portal session invalid — cannot announce formats".into(),
            ));
        }
        let session_guard = self.session.read().await;
        self.portal
            .announce_rdp_formats(&session_guard, mime_types)
            .await
            .map_err(|e| ClipboardError::PortalError(format!("Failed to announce formats: {e}")))?;
        Ok(())
    }

    async fn read_data(&self, mime_type: &str) -> Result<Vec<u8>> {
        if !self.session_valid.load(Ordering::Acquire) {
            return Err(ClipboardError::PortalError(
                "Portal session invalid — cannot read clipboard".into(),
            ));
        }
        let session_guard = self.session.read().await;
        let data = self
            .portal
            .read_local_clipboard(&session_guard, mime_type)
            .await
            .map_err(|e| ClipboardError::PortalError(format!("Failed to read clipboard: {e}")))?;
        Ok(data)
    }

    async fn complete_transfer(
        &self,
        serial: u32,
        _mime_type: &str,
        data: Vec<u8>,
        success: bool,
    ) -> Result<()> {
        if !self.session_valid.load(Ordering::Acquire) {
            return Err(ClipboardError::PortalError(
                "Portal session invalid — cannot complete transfer".into(),
            ));
        }
        let session_guard = self.session.read().await;

        if success && !data.is_empty() {
            // write_selection_data calls selection_write_done internally
            // (both success and error paths), so no separate ack needed
            self.portal
                .write_selection_data(&session_guard, serial, data)
                .await
                .map_err(|e| ClipboardError::PortalError(format!("SelectionWrite failed: {e}")))?;
        } else {
            // No data to write — acknowledge failure to Portal directly
            self.portal
                .portal_clipboard()
                .selection_write_done(&session_guard, serial, success)
                .await
                .map_err(|e| {
                    ClipboardError::PortalError(format!("SelectionWriteDone failed: {e}"))
                })?;
        }

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
        if !self.session_valid.load(Ordering::Acquire) {
            return Err(ClipboardError::PortalError(
                "Portal session has been invalidated by compositor".into(),
            ));
        }
        Ok(())
    }

    async fn shutdown(&self) {
        self.shutdown.store(true, Ordering::Relaxed);
        let _ = self.shutdown_broadcast.send(());

        let mut handles = self.task_handles.lock().await;
        for handle in handles.drain(..) {
            handle.abort();
        }
        debug!("Portal clipboard provider shut down");
    }
}

impl Drop for PortalClipboardProvider {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Relaxed);
        let _ = self.shutdown_broadcast.send(());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_provider_name() {
        // Can't construct without real Portal, but we can test the trait requirement compiles
        fn assert_provider<T: ClipboardProvider>() {}
        assert_provider::<PortalClipboardProvider>();
    }
}
