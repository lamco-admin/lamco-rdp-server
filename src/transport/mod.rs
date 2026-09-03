//! Unified multi-transport accept layer for lamco-rdp-server.
//!
//! See `~/lamco-admin/shared/strategy/TRANSPORT-LAYER-DECISION-2026-05-16.md`
//! and `docs/design/transport/TRANSPORT-PHASE-1-SDS-2026-05-16.md`
//! for the architectural rationale.
//!
//! Three-tier contract: `Transport → Handshake → Byte stream → IronRDP acceptor`.
//! Phase 1 ships only TCP; later phases add AF_VSOCK, Unix sockets, and
//! WebSocket+RDCleanPath without changing the public trait shape.
//!
//! Deployment abstraction (`AcceptDeployment` trait) added by Phase-1-retrofit
//! 2026-05-16 — encapsulates per-binary differences in listener construction,
//! handler wiring, and shutdown signaling so the dispatcher stays binary-agnostic.

pub mod config;
pub mod handler;
pub mod listener;
pub mod proxy_auth;
pub mod socket_activation;
#[cfg(feature = "websocket")]
pub mod websocket;

use std::{
    future::Future,
    pin::Pin,
    time::{Duration, Instant},
};

use anyhow::Result;
pub use config::TransportsConfig;
use futures::stream::{FuturesUnordered, StreamExt};
pub use handler::LamcoConnectionHandler;
use ironrdp_server::{PostConnectionAction, RdpServer};
pub use listener::{AcceptedConnection, AsyncRdpStream, Listener, PeerAddr, TransportError};
pub use proxy_auth::{AllowAllInsecure, DenyAll, ProxyAuthValidator, SharedSecretValidator};
pub use socket_activation::{ActivatedFds, ActivationError};
use tracing::{debug, error, info, warn};

/// Configures and runs an `AcceptDispatcher` for a specific binary's runtime
/// context (desktop, qemu, future vsock-only, future WebSocket gateway, etc).
///
/// Encapsulates the per-binary differences in: which transports to bind,
/// what state the per-connection handler needs (event sink, PAM validator,
/// portal-validity closure), and how to detect shutdown. The dispatcher
/// consumes this uniform interface and doesn't care which binary it's running
/// inside.
///
/// Implementors today: `crate::server::deployment::WlrDirectDeployment`, which
/// despite its name backs every desktop-sharing session strategy (Portal,
/// Mutter Direct, libei, wlr-direct alike) — not just the wlr-direct one.
/// Phase 2 adds `crate::qemu::deployment::QemuDeployment`.
#[async_trait::async_trait]
pub trait AcceptDeployment: Send {
    /// Diagnostic name surfaced in logs (e.g. `"desktop"`, `"qemu"`).
    fn name(&self) -> &'static str;

    /// Bind all listeners for this deployment. Called once at dispatcher
    /// startup. Encapsulates LISTEN_FDS handling, socket activation,
    /// transport-config source (TOML schema for wlr-direct, programmatic for
    /// qemu), and Cargo-feature gating.
    async fn build_listeners(&mut self) -> Result<Vec<Box<dyn Listener>>>;

    /// Construct the per-connection handler with deployment-appropriate
    /// wiring (event sink, PAM validator, portal-validity closure). Called
    /// once.
    fn build_handler(&mut self) -> LamcoConnectionHandler;

    /// Future that resolves when this deployment's shutdown is signaled.
    /// Abstracts away the native channel type — `broadcast::Receiver` for
    /// wlr-direct, `watch::Receiver` for qemu, etc.
    fn shutdown_signal(&mut self) -> Pin<Box<dyn Future<Output = ()> + Send + 'static>>;
}

/// Drives the unified accept loop. Consumes a deployment, borrows the
/// `RdpServer` for the loop's duration, and yields when shutdown fires or a
/// connection-handler hook returns `PostConnectionAction::Stop`.
///
/// Phase 1 (TCP only). Phase 2 adds Unix. Phases 3a / 3b add AF_VSOCK and
/// WebSocket+RDCleanPath — all without changing this entry point.
pub struct AcceptDispatcher;

impl AcceptDispatcher {
    /// Build everything from the deployment and run the accept loop.
    ///
    /// The deployment's `build_listeners`, `build_handler`, and
    /// `shutdown_signal` methods are each called exactly once before the loop
    /// starts.
    pub async fn run(
        mut deployment: impl AcceptDeployment,
        rdp_server: &mut RdpServer,
    ) -> Result<()> {
        let dep_name = deployment.name();
        let mut listeners = deployment.build_listeners().await?;
        let mut handler = deployment.build_handler();
        let mut shutdown = deployment.shutdown_signal();

        if listeners.is_empty() {
            anyhow::bail!("AcceptDispatcher ({dep_name}): no listeners configured");
        }

        let transport_summary: Vec<&'static str> =
            listeners.iter().map(|l| l.transport_name()).collect();
        info!(
            deployment = dep_name,
            transports = ?transport_summary,
            count = listeners.len(),
            "Accept dispatcher started"
        );

        loop {
            // Build accept futures across all listeners; race them against shutdown.
            let mut accept_futures: FuturesUnordered<_> = listeners
                .iter_mut()
                .enumerate()
                .map(|(idx, l)| {
                    let name = l.transport_name();
                    async move { (idx, name, l.accept().await) }
                })
                .collect();

            let (transport_name, accept_result) = tokio::select! {
                () = &mut shutdown => {
                    info!(deployment = dep_name, "Shutdown signal received: stopping accept dispatcher");
                    return Ok(());
                }
                Some((_idx, name, result)) = accept_futures.next() => (name, result),
            };
            // Explicit drop (not just end-of-scope) to release the &mut borrow
            // on listeners before the drain step below needs its own &mut.
            drop(accept_futures);

            match accept_result {
                Ok(Some(accepted)) => {
                    let AcceptedConnection { peer, stream, mode } = accepted;
                    debug!(
                        transport = transport_name,
                        peer = %peer.to_display(),
                        ?mode,
                        "Connection accepted"
                    );

                    // on_accept_async: PAM peer-IP setup, ClientConnected event,
                    // cache client state for matching on_disconnected.
                    let accept_ok = handler.on_accept_async(&peer).await;
                    if !accept_ok {
                        debug!(
                            transport = transport_name,
                            peer = %peer.to_display(),
                            "Connection rejected by handler"
                        );
                        continue;
                    }

                    let start = Instant::now();
                    let conn_result = match mode {
                        crate::transport::listener::AcceptorMode::Standard => {
                            rdp_server.run_connection(stream).await
                        }
                        crate::transport::listener::AcceptorMode::PreAuthenticated => {
                            // Stream is already TLS-terminated (typically WSS); skip the
                            // IronRDP-managed TLS upgrade. Upstream PR #1281 reshaped this
                            // from a dedicated run_connection_pre_authenticated method into
                            // run_connection_with + TransportTls::AlreadyDone.
                            rdp_server
                                .run_connection_with(
                                    stream,
                                    ironrdp_server::TransportTls::AlreadyDone,
                                )
                                .await
                        }
                    };
                    let duration = start.elapsed();

                    // on_disconnected_async: classify error, emit ClientDisconnected,
                    // prune PAM rate limits, check Portal validity.
                    let action = handler
                        .on_disconnected_async(&peer, duration, conn_result.as_ref().err())
                        .await;

                    if matches!(action, PostConnectionAction::Stop) {
                        info!("Handler requested stop: terminating accept loop");
                        return Ok(());
                    }

                    // #57-adjacent: run_connection() above occupies this loop for
                    // the entire session, so the OS backlog is the only thing
                    // absorbing connection attempts that arrive while we're busy.
                    // A client that gives up before we ever get back to accept()
                    // leaves an unaccepted, already-CLOSE-WAIT socket sitting
                    // there — under sustained rapid reconnects that fills the
                    // backlog and starves every later attempt. Drain and
                    // immediately drop anything that queued up during the
                    // session we just finished, rather than serving each one a
                    // full (likely-abandoned) connection attempt in turn.
                    // Bounded so a genuine connection flood can't stall the
                    // loop indefinitely; each attempt costs at most 1ms.
                    let mut drained = 0u32;
                    'drain: for l in listeners.iter_mut() {
                        for _ in 0..32 {
                            match tokio::time::timeout(Duration::from_millis(1), l.accept()).await {
                                Ok(Ok(Some(_))) => drained += 1,
                                _ => continue 'drain,
                            }
                        }
                    }
                    if drained > 0 {
                        warn!(
                            deployment = dep_name,
                            drained,
                            "Dropped stale connection attempts queued while busy with the prior session"
                        );
                    }
                }
                Ok(None) => {
                    warn!(
                        transport = transport_name,
                        "Listener gracefully closed; continuing with remaining listeners"
                    );
                    // TODO Phase 4+: remove this listener from the Vec rather than re-poll it.
                    // For Phase 1 (single TCP listener), graceful close is server shutdown.
                    return Ok(());
                }
                Err(e) => {
                    error!(transport = transport_name, error = %e, "Accept failed");
                    // Transient errors are expected (e.g. EMFILE). Continue.
                }
            }
        }
    }
}

/// Re-export of upstream lifecycle types for convenience.
pub use ironrdp_server::ConnectionHandler as UpstreamConnectionHandler;
pub use ironrdp_server::PostConnectionAction as UpstreamPostConnectionAction;

#[cfg(test)]
mod tests {
    use std::{sync::Arc, time::Duration};

    use tokio::sync::{Mutex, oneshot};

    use super::*;
    use crate::transport::{
        handler::OnDisconnectFn,
        listener::{AcceptedConnection, AsyncRdpStream, PeerAddr, TransportError},
    };

    /// Mock listener that yields one prepared connection then closes. Useful
    /// for driving the dispatcher in tests without sockets.
    struct MockListener {
        stream: Option<Box<dyn AsyncRdpStream>>,
        peer: PeerAddr,
    }

    #[async_trait::async_trait]
    impl Listener for MockListener {
        fn transport_name(&self) -> &'static str {
            "mock"
        }

        async fn accept(&mut self) -> Result<Option<AcceptedConnection>, TransportError> {
            match self.stream.take() {
                Some(stream) => Ok(Some(AcceptedConnection::standard(
                    self.peer.clone(),
                    stream,
                ))),
                None => {
                    // After our single connection, block forever (real listener
                    // would block on `accept()`; we simulate by never resolving).
                    std::future::pending::<()>().await;
                    unreachable!()
                }
            }
        }
    }

    /// Minimal deployment for build-sequence verification. Does NOT exercise the
    /// dispatcher loop (that requires a real RdpServer); only checks that
    /// `build_listeners` and `build_handler` and `shutdown_signal` are wired up.
    struct MockDeployment {
        listener: Option<Box<dyn Listener>>,
        event_tx: tokio::sync::mpsc::UnboundedSender<crate::dbus::ServerEvent>,
        shutdown_tx: Arc<Mutex<Option<oneshot::Sender<()>>>>,
        shutdown_rx: Arc<Mutex<Option<oneshot::Receiver<()>>>>,
    }

    impl MockDeployment {
        fn new(
            listener: Box<dyn Listener>,
            event_tx: tokio::sync::mpsc::UnboundedSender<crate::dbus::ServerEvent>,
        ) -> Self {
            let (tx, rx) = oneshot::channel();
            Self {
                listener: Some(listener),
                event_tx,
                shutdown_tx: Arc::new(Mutex::new(Some(tx))),
                shutdown_rx: Arc::new(Mutex::new(Some(rx))),
            }
        }

        async fn fire_shutdown(&self) {
            if let Some(tx) = self.shutdown_tx.lock().await.take() {
                let _ = tx.send(());
            }
        }
    }

    #[async_trait::async_trait]
    impl AcceptDeployment for MockDeployment {
        fn name(&self) -> &'static str {
            "mock"
        }

        async fn build_listeners(&mut self) -> Result<Vec<Box<dyn Listener>>> {
            Ok(self.listener.take().into_iter().collect())
        }

        fn build_handler(&mut self) -> LamcoConnectionHandler {
            let cb: OnDisconnectFn = Arc::new(|_served| Box::pin(async { true }));
            LamcoConnectionHandler::new(
                self.event_tx.clone(),
                None,
                cb,
                Arc::new(tokio::sync::broadcast::channel::<()>(1).0),
            )
        }

        fn shutdown_signal(&mut self) -> Pin<Box<dyn Future<Output = ()> + Send + 'static>> {
            let rx_slot = self.shutdown_rx.clone();
            Box::pin(async move {
                let rx = rx_slot.lock().await.take();
                if let Some(rx) = rx {
                    let _ = rx.await;
                }
            })
        }
    }

    #[tokio::test]
    async fn deployment_build_sequence() {
        // Construct a mock listener with one prepared duplex stream pair.
        let (a, _b) = tokio::io::duplex(64);
        let peer = PeerAddr::Tcp("127.0.0.1:0".parse().unwrap());
        let mock_listener = MockListener {
            stream: Some(Box::new(a)),
            peer,
        };
        let (event_tx, _event_rx) = tokio::sync::mpsc::unbounded_channel();

        let mut deployment = MockDeployment::new(Box::new(mock_listener), event_tx);

        // Verify each build_* method works as expected.
        assert_eq!(deployment.name(), "mock");

        let listeners = deployment.build_listeners().await.unwrap();
        assert_eq!(listeners.len(), 1);
        assert_eq!(listeners[0].transport_name(), "mock");

        let _handler = deployment.build_handler();

        let shutdown = deployment.shutdown_signal();
        // Firing the oneshot resolves the future.
        deployment.fire_shutdown().await;
        tokio::time::timeout(Duration::from_millis(100), shutdown)
            .await
            .expect("shutdown_signal should resolve after fire_shutdown");
    }
}
