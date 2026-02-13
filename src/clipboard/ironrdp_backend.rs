//! IronRDP Clipboard Backend Factory
//!
//! **Execution Path:** IronRDP CLIPRDR protocol
//! **Status:** Active (v1.0.0+)
//! **Platform:** Universal (RDP protocol)
//! **Role:** Bridges IronRDP backend events to ClipboardOrchestrator
//!
//! Server-specific factory wrapping lamco-rdp-clipboard's backend.
//! Integrates with the server's ClipboardOrchestrator for event routing.

use std::sync::Arc;

use ironrdp_cliprdr::backend::CliprdrBackendFactory;
use ironrdp_server::ServerEventSender;
// Re-export library backend and types
pub use lamco_rdp_clipboard::{
    ClipboardEvent, ClipboardEventReceiver, ClipboardEventSender, ClipboardGeneralCapabilityFlags,
    RdpCliprdrBackend, RdpCliprdrFactory as LibRdpCliprdrFactory,
};
use tokio::sync::{mpsc, Mutex};
use tracing::{debug, info};

use crate::clipboard::manager::ClipboardOrchestrator;

/// Server-specific clipboard backend factory
///
/// Wraps [`LibRdpCliprdrFactory`] from lamco-rdp-clipboard and integrates
/// with the server's [`ClipboardOrchestrator`] for event routing.
///
/// # Example
///
/// ```ignore
/// use lamco_rdp_server::clipboard::{ClipboardOrchestrator, LamcoCliprdrFactory};
/// use std::sync::Arc;
/// use tokio::sync::Mutex;
///
/// let manager = Arc::new(Mutex::new(ClipboardOrchestrator::new(config).await?));
/// let factory = LamcoCliprdrFactory::new(manager);
///
/// // Pass factory to IronRDP server builder
/// ```
pub struct LamcoCliprdrFactory {
    /// Clipboard manager shared across connections
    clipboard_manager: Arc<Mutex<ClipboardOrchestrator>>,

    /// Event sender for clipboard events
    event_sender: ClipboardEventSender,

    /// Server event sender for IronRDP (set via ServerEventSender trait)
    server_event_sender: Option<mpsc::UnboundedSender<ironrdp_server::ServerEvent>>,
}

impl LamcoCliprdrFactory {
    pub fn new(clipboard_manager: Arc<Mutex<ClipboardOrchestrator>>) -> Self {
        let event_sender = ClipboardEventSender::new();
        let event_receiver = event_sender.subscribe();

        info!("Created LamcoCliprdrFactory with event channel");

        // Start event bridge task to forward RDP backend events to ClipboardManager
        // This is critical - without it, RDP clipboard events (FormatList, DataRequest, etc.)
        // would be sent to the broadcast channel but never reach ClipboardManager!
        Self::start_event_bridge(event_receiver, Arc::clone(&clipboard_manager));

        Self {
            clipboard_manager,
            event_sender,
            server_event_sender: None,
        }
    }

    /// Start the event bridge task
    ///
    /// This task polls the ClipboardEventReceiver and forwards RDP backend events
    /// to the ClipboardManager's internal event queue, converting between the
    /// ironrdp clipboard types and lamco clipboard types.
    fn start_event_bridge(
        receiver: ClipboardEventReceiver,
        clipboard_manager: Arc<Mutex<ClipboardOrchestrator>>,
    ) {
        use lamco_clipboard_core::ClipboardFormat;

        tokio::spawn(async move {
            info!("🔗 RDP clipboard event bridge task started");

            loop {
                // Poll for events (ClipboardEventReceiver uses try_recv, not async recv)
                if let Some(rdp_event) = receiver.try_recv() {
                    let mgr = clipboard_manager.lock().await;
                    let manager_tx = mgr.event_sender();
                    drop(mgr);

                    match rdp_event {
                        ClipboardEvent::RemoteCopy { formats } => {
                            info!(
                                "🔗 Bridge: RDP RemoteCopy ({} formats) → ClipboardManager",
                                formats.len()
                            );

                            let converted: Vec<ClipboardFormat> = formats
                                .iter()
                                .map(|f| {
                                    // ClipboardFormatName has a .value() method to get the inner string
                                    let name_str = f.name().map(|n| {
                                        let val = n.value().to_string();
                                        info!("📝 Format name: {:?} -> value: {}", n, val);
                                        val
                                    });
                                    ClipboardFormat {
                                        id: f.id().value(),
                                        name: name_str,
                                    }
                                })
                                .collect();

                            let _ = manager_tx
                                .send(crate::clipboard::ClipboardEvent::RdpFormatList(converted))
                                .await;
                        }

                        ClipboardEvent::FormatDataRequest { format_id } => {
                            info!(
                                "🔗 Bridge: RDP FormatDataRequest (format {}) → ClipboardManager",
                                format_id.value()
                            );
                            let _ = manager_tx
                                .send(crate::clipboard::ClipboardEvent::RdpDataRequest(
                                    format_id.value(),
                                    None,
                                ))
                                .await;
                        }

                        ClipboardEvent::FormatDataResponse { data, is_error } => {
                            if is_error {
                                // Error response is expected when client doesn't have the format
                                debug!("🔗 Bridge: RDP FormatDataResponse (format unavailable) → ClipboardManager");
                                let _ = manager_tx
                                    .send(crate::clipboard::ClipboardEvent::RdpDataError)
                                    .await;
                            } else {
                                info!("🔗 Bridge: RDP FormatDataResponse ({} bytes) → ClipboardManager", data.len());
                                let _ = manager_tx
                                    .send(crate::clipboard::ClipboardEvent::RdpDataResponse(data))
                                    .await;
                            }
                        }

                        ClipboardEvent::FileContentsRequest {
                            stream_id,
                            index,
                            position,
                            size,
                            is_size_request,
                        } => {
                            info!("🔗 Bridge: RDP FileContentsRequest (stream={}, index={}, pos={}, size={}, size_req={}) → ClipboardManager",
                                stream_id, index, position, size, is_size_request);
                            let _ = manager_tx
                                .send(crate::clipboard::ClipboardEvent::RdpFileContentsRequest {
                                    stream_id,
                                    list_index: index,
                                    position,
                                    size,
                                    is_size_request,
                                })
                                .await;
                        }

                        ClipboardEvent::FileContentsResponse {
                            stream_id,
                            data,
                            is_error,
                        } => {
                            if is_error {
                                info!("🔗 Bridge: RDP FileContentsResponse ERROR (stream={}) → ClipboardManager", stream_id);
                            } else {
                                info!("🔗 Bridge: RDP FileContentsResponse (stream={}, {} bytes) → ClipboardManager", stream_id, data.len());
                            }
                            let _ = manager_tx
                                .send(crate::clipboard::ClipboardEvent::RdpFileContentsResponse {
                                    stream_id,
                                    data,
                                    is_error,
                                })
                                .await;
                        }

                        ClipboardEvent::Ready => {
                            info!("🔗 Bridge: RDP clipboard Ready → ClipboardManager");
                            let _ = manager_tx
                                .send(crate::clipboard::ClipboardEvent::RdpReady)
                                .await;
                        }

                        ClipboardEvent::RequestFormatList => {
                            // This is essentially the same as Ready - re-announce Linux clipboard
                            info!("🔗 Bridge: RDP RequestFormatList → ClipboardManager (treating as Ready)");
                            let _ = manager_tx
                                .send(crate::clipboard::ClipboardEvent::RdpReady)
                                .await;
                        }

                        _ => {
                            // Other events (NegotiatedCapabilities, Lock, Unlock) not critical yet
                        }
                    }
                } else {
                    // No events available, sleep briefly to avoid busy loop
                    tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
                }
            }
        });

        info!("✅ RDP clipboard event bridge started - backend events will reach manager");
    }

    /// Get a clone of the event sender
    ///
    /// Use this to create additional backends that share the same event channel.
    pub fn event_sender(&self) -> ClipboardEventSender {
        self.event_sender.clone()
    }
}

impl CliprdrBackendFactory for LamcoCliprdrFactory {
    fn build_cliprdr_backend(&self) -> Box<dyn ironrdp_cliprdr::backend::CliprdrBackend> {
        debug!("Building clipboard backend for new connection");

        let backend = RdpCliprdrBackend::new(
            "/tmp/lamco-clipboard".to_string(),
            self.event_sender.clone(),
        );

        Box::new(backend)
    }
}

impl ServerEventSender for LamcoCliprdrFactory {
    fn set_sender(&mut self, sender: mpsc::UnboundedSender<ironrdp_server::ServerEvent>) {
        info!("Clipboard factory received server event sender");
        self.server_event_sender = Some(sender.clone());

        let manager = Arc::clone(&self.clipboard_manager);
        let sender_clone = sender;
        tokio::spawn(async move {
            if let Ok(mgr) = manager.try_lock() {
                mgr.set_server_event_sender(sender_clone).await;
            }
        });
    }
}

impl ironrdp_server::CliprdrServerFactory for LamcoCliprdrFactory {}

impl std::fmt::Debug for LamcoCliprdrFactory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LamcoCliprdrFactory")
            .field("has_server_sender", &self.server_event_sender.is_some())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clipboard::manager::ClipboardOrchestratorConfig;

    #[tokio::test]
    async fn test_factory_creation() {
        let config = ClipboardOrchestratorConfig::default();
        let manager = Arc::new(Mutex::new(
            ClipboardOrchestrator::new(config).await.unwrap(),
        ));

        let factory = LamcoCliprdrFactory::new(manager);
        let _backend = factory.build_cliprdr_backend();
        // Backend created successfully
    }

    #[tokio::test]
    async fn test_factory_with_bridge() {
        let config = ClipboardOrchestratorConfig::default();
        let manager = Arc::new(Mutex::new(
            ClipboardOrchestrator::new(config).await.unwrap(),
        ));

        let factory = LamcoCliprdrFactory::new(manager);

        // Factory should be created successfully
        // Event bridge starts automatically in new()
        let _backend = factory.build_cliprdr_backend();
    }
}
