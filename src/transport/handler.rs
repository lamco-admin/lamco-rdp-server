//! `LamcoConnectionHandler` — holds the per-connection lifecycle state and
//! emits the events that the wlr-direct accept loop previously emitted inline.
//!
//! Phase 1 invokes the async equivalents (`on_accept_async` / `on_disconnected_async`)
//! directly from the `AcceptDispatcher`. The sync `ironrdp_server::ConnectionHandler`
//! trait is implemented for semantic alignment with upstream, but Phase 1 doesn't
//! route through it — `on_disconnect` (Portal-validity check) is async and the
//! sync trait method can't await it without blocking the executor.
//! See SDS §4.3 Option γ.

use std::{
    future::Future,
    pin::Pin,
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

#[cfg(test)]
use ironrdp_server::ServerErrorExt as _;
use ironrdp_server::{ConnectionHandler, PostConnectionAction, ServerError};
use tokio::sync::{Mutex as AsyncMutex, mpsc};
use tracing::{error, info, warn};
use uuid::Uuid;

use super::listener::PeerAddr;
use crate::{dbus::ServerEvent, security::auth::PamValidator};

/// Callback type for the "is the Portal session still alive?" check that the
/// wlr-direct path runs after each disconnect. Lives in `LamcoRdpServer` as
/// `on_disconnect(&self) -> bool`; the handler holds a closure wrapping it
/// rather than a typed reference, to avoid pulling the entire server type into
/// this module.
/// The `bool` argument is `served`: whether the connection actually used the
/// compositor session. It is `false` for fast handshake-failure probes (clients
/// like mstsc open a throwaway connection before the real one), letting the
/// handler skip per-connection session release so the following real connection
/// reuses the session instead of churning a fresh one.
pub type OnDisconnectFn =
    Arc<dyn Fn(bool) -> Pin<Box<dyn Future<Output = bool> + Send>> + Send + Sync>;

/// Callback type for establishing the compositor session when a client
/// connects, per the backend's session-lifecycle policy (see
/// `SessionHandle::establish_for_client`). Returns `false` to reject the
/// connection when the session can't be established. Held as a closure for the
/// same decoupling reason as [`OnDisconnectFn`].
pub type OnConnectFn = Arc<dyn Fn() -> Pin<Box<dyn Future<Output = bool> + Send>> + Send + Sync>;

/// Per-connection state cached between `on_accept_async` and `on_disconnected_async`.
struct ClientState {
    client_id: String,
    start: Instant,
}

pub struct LamcoConnectionHandler {
    event_tx: mpsc::UnboundedSender<ServerEvent>,
    pam_validator: Option<Arc<PamValidator>>,
    on_disconnect: OnDisconnectFn,
    /// Establishes the compositor session for an incoming client (per lifecycle
    /// policy). Defaults to a no-op that accepts; the deployment installs the
    /// real closure via [`Self::with_on_connect`].
    on_connect: OnConnectFn,
    /// Broadcast the server-wide graceful-shutdown signal when the compositor
    /// destroys the session (the server can't recover without a restart).
    shutdown_tx: Arc<tokio::sync::broadcast::Sender<()>>,
    current_client: AsyncMutex<Option<ClientState>>,
}

impl LamcoConnectionHandler {
    pub fn new(
        event_tx: mpsc::UnboundedSender<ServerEvent>,
        pam_validator: Option<Arc<PamValidator>>,
        on_disconnect: OnDisconnectFn,
        shutdown_tx: Arc<tokio::sync::broadcast::Sender<()>>,
    ) -> Self {
        Self {
            event_tx,
            pam_validator,
            on_disconnect,
            on_connect: Arc::new(|| Box::pin(async { true })),
            shutdown_tx,
            current_client: AsyncMutex::new(None),
        }
    }

    /// Install the session-establishment closure invoked on each client accept.
    #[must_use]
    pub fn with_on_connect(mut self, on_connect: OnConnectFn) -> Self {
        self.on_connect = on_connect;
        self
    }

    /// Called by the `AcceptDispatcher` immediately after a transport listener
    /// returns a connection, before `RdpServer::run_connection` is invoked.
    ///
    /// Returns `false` to reject the connection (the stream is dropped).
    pub async fn on_accept_async(&mut self, peer: &PeerAddr) -> bool {
        let client_id = format!("rdp-{}", Uuid::new_v4());
        let start = Instant::now();
        let start_timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        // Capability #6: PAM peer-IP setup for rate limiting. Only meaningful
        // for transports with an IP-shaped peer (TCP, WebSocket).
        if let (Some(validator), Some(ip)) = (self.pam_validator.as_ref(), peer.ip()) {
            validator.set_peer_ip(ip);
        }

        // Capability #4: ClientConnected event for D-Bus / monitoring consumers.
        let _ = self.event_tx.send(ServerEvent::ClientConnected {
            client_id: client_id.clone(),
            peer_address: peer.to_display(),
            timestamp: start_timestamp,
        });

        // Establish the compositor session for this client per the backend's
        // lifecycle policy (Persistent backends reuse; PerConnection re-creates)
        // and rebind capture if it moved. Reject the connection if it fails —
        // serving a client we can't capture or inject for is pointless.
        if !(self.on_connect)().await {
            warn!(
                "Session establishment failed for {} — rejecting connection",
                peer.to_display()
            );
            return false;
        }

        *self.current_client.lock().await = Some(ClientState { client_id, start });

        true
    }

    /// Called by the `AcceptDispatcher` after `RdpServer::run_connection` returns.
    ///
    /// Returns `PostConnectionAction::Stop` if the Portal session has been
    /// destroyed by the compositor (we cannot accept further clients without
    /// user interaction). Returns `Continue` otherwise.
    pub async fn on_disconnected_async(
        &mut self,
        peer: &PeerAddr,
        duration: Duration,
        error: Option<&ServerError>,
    ) -> PostConnectionAction {
        let peer_display = peer.to_display();
        let state = self.current_client.lock().await.take();

        // Capability #7: classify short-lived resets vs real failures.
        // mstsc.exe probes with a brief connection before the real session,
        // and macOS Remote Desktop does similar — these are not errors.
        if let Some(e) = error {
            // `{e:#}` gives "I/O error" and stops: the wrapper's Display does
            // not walk to the source. Walk it here, or every transport failure
            // in the field is reported without the one detail that identifies
            // it.
            let msg = {
                let mut out = format!("{e}");
                let mut src: Option<&(dyn std::error::Error + 'static)> =
                    std::error::Error::source(e);
                while let Some(cause) = src {
                    use std::fmt::Write as _;
                    let _ = write!(out, ": {cause}");
                    src = std::error::Error::source(cause);
                }
                out
            };
            let is_reset = msg.contains("Connection reset by peer") || msg.contains("os error 104");
            // A scripted client that finishes its command chain closes the
            // socket without a Disconnect PDU, so the read in flight ends
            // mid-frame. That is how the connection was always going to end,
            // not a fault, and logging it at ERROR made every automated run
            // look like it had failed. Classify on the io::ErrorKind rather
            // than on message text: the display form is just "I/O error".
            // TLS adds its own spelling of this: rustls surfaces a peer that
            // vanished mid-record as ErrorKind::Other carrying "peer closed
            // connection without sending TLS close_notify", so the kind alone
            // is not enough to recognise it.
            let (is_peer_closed, io_detail) = match e.kind() {
                ironrdp_server::ServerErrorKind::Io(io_err) => {
                    let detail = io_err.to_string();
                    let closed = matches!(
                        io_err.kind(),
                        std::io::ErrorKind::UnexpectedEof
                            | std::io::ErrorKind::BrokenPipe
                            | std::io::ErrorKind::ConnectionAborted
                    ) || detail.contains("close_notify")
                        || detail.contains("unexpected eof")
                        || detail.contains("early eof");
                    (closed, detail)
                }
                _ => (
                    msg.contains("not enough bytes")
                        || msg.contains("early eof")
                        || msg.contains("unexpected end of file"),
                    msg.clone(),
                ),
            };

            if is_peer_closed && !is_reset {
                info!(
                    "Client {} closed the connection after {:.1}s without a disconnect PDU: {}",
                    peer_display,
                    duration.as_secs_f64(),
                    io_detail
                );
            } else if is_reset && duration < Duration::from_secs(1) {
                warn!(
                    "Connection from {} reset during handshake (likely client probe, lasted {:.0}ms)",
                    peer_display,
                    duration.as_secs_f64() * 1000.0
                );
            } else if is_reset {
                error!(
                    "Connection from {} reset after {:.1}s (active session lost)",
                    peer_display,
                    duration.as_secs_f64()
                );
            } else {
                error!(
                    "Connection error from {} after {:.1}s: {}",
                    peer_display,
                    duration.as_secs_f64(),
                    msg
                );
            }
        }

        // Capability #5: ClientDisconnected event.
        if let Some(s) = state {
            let _ = self.event_tx.send(ServerEvent::ClientDisconnected {
                client_id: s.client_id,
                reason: "Connection ended".into(),
                duration_seconds: s.start.elapsed().as_secs(),
            });
        }

        // Capability #9: prune PAM rate-limit entries between connections.
        if let Some(ref validator) = self.pam_validator {
            validator.prune_stale_entries();
        }

        // Capability #8: Portal session validity check. Returns false if the
        // compositor destroyed the Portal session — in that case the accept
        // loop must stop because no further client can succeed without user
        // re-authorization.
        // A fast handshake failure never used the compositor session — treat it
        // as a probe so the disconnect handler skips session release and the
        // following real connection reuses the session (avoids churn).
        let served = !(error.is_some() && duration < Duration::from_secs(1));
        let portal_alive = (self.on_disconnect)(served).await;
        if !portal_alive {
            let _ = self.event_tx.send(ServerEvent::StatusChanged {
                old: "running".into(),
                new: "stopped".into(),
                message: "Session invalidated by compositor".into(),
            });
            // The compositor destroyed the session (e.g. the user clicked GNOME's
            // "stop sharing"); it can't be recreated without a fresh portal grant,
            // so the server is no longer useful. Stopping the accept loop alone
            // leaves the process alive (D-Bus/health/PipeWire tasks keep the
            // runtime up) — which shows as "still running" in the GUI. Trigger a
            // full graceful shutdown so the process exits cleanly.
            warn!(
                "Compositor destroyed the session — shutting the server down (restart to re-share)"
            );
            let _ = self.shutdown_tx.send(());
            return PostConnectionAction::Stop;
        }

        PostConnectionAction::Continue
    }
}

/// Upstream-trait conformance for future migration to `RdpServer::run()` if it
/// ever supports multi-listener. Phase 1 does not route through these methods
/// (the dispatcher calls the async equivalents directly) — see SDS §4.3.
impl ConnectionHandler for LamcoConnectionHandler {
    fn on_accept(&mut self, _peer: std::net::SocketAddr) -> bool {
        unreachable!(
            "LamcoConnectionHandler sync on_accept invoked; Phase 1 uses on_accept_async via AcceptDispatcher"
        )
    }

    fn on_disconnected(
        &mut self,
        _peer: std::net::SocketAddr,
        _duration: Duration,
        _error: Option<&ServerError>,
    ) -> PostConnectionAction {
        unreachable!(
            "LamcoConnectionHandler sync on_disconnected invoked; Phase 1 uses on_disconnected_async via AcceptDispatcher"
        )
    }
}

#[cfg(test)]
mod tests {
    use std::{
        net::{Ipv4Addr, SocketAddr, SocketAddrV4},
        sync::atomic::{AtomicBool, Ordering},
    };

    use super::*;

    fn test_peer() -> PeerAddr {
        PeerAddr::Tcp(SocketAddr::V4(SocketAddrV4::new(
            Ipv4Addr::new(192, 168, 1, 5),
            12345,
        )))
    }

    fn always_alive_callback() -> OnDisconnectFn {
        Arc::new(|_served| Box::pin(async { true }))
    }

    fn always_dead_callback() -> OnDisconnectFn {
        Arc::new(|_served| Box::pin(async { false }))
    }

    #[tokio::test]
    async fn on_accept_emits_client_connected_event() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut handler = LamcoConnectionHandler::new(
            tx,
            None,
            always_alive_callback(),
            Arc::new(tokio::sync::broadcast::channel::<()>(1).0),
        );
        let peer = test_peer();

        let accepted = handler.on_accept_async(&peer).await;
        assert!(accepted);

        let event = rx.recv().await.expect("expected ClientConnected event");
        match event {
            ServerEvent::ClientConnected {
                peer_address,
                client_id,
                timestamp: _,
            } => {
                assert_eq!(peer_address, "192.168.1.5:12345");
                assert!(client_id.starts_with("rdp-"));
            }
            other => panic!("expected ClientConnected, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn on_disconnected_emits_event_and_continues_when_session_alive() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut handler = LamcoConnectionHandler::new(
            tx,
            None,
            always_alive_callback(),
            Arc::new(tokio::sync::broadcast::channel::<()>(1).0),
        );
        let peer = test_peer();

        let _ = handler.on_accept_async(&peer).await;
        let _ = rx.recv().await; // drain ClientConnected

        let action = handler
            .on_disconnected_async(&peer, Duration::from_secs(2), None)
            .await;
        assert_eq!(action, PostConnectionAction::Continue);

        let event = rx.recv().await.expect("expected ClientDisconnected event");
        assert!(matches!(event, ServerEvent::ClientDisconnected { .. }));
    }

    #[tokio::test]
    async fn on_disconnected_returns_stop_when_portal_dead() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut handler = LamcoConnectionHandler::new(
            tx,
            None,
            always_dead_callback(),
            Arc::new(tokio::sync::broadcast::channel::<()>(1).0),
        );
        let peer = test_peer();

        let _ = handler.on_accept_async(&peer).await;
        // Drain accumulated events (ClientConnected) without blocking.
        while rx.try_recv().is_ok() {}

        let action = handler
            .on_disconnected_async(&peer, Duration::from_secs(1), None)
            .await;
        assert_eq!(action, PostConnectionAction::Stop);

        // Expect ClientDisconnected followed by StatusChanged: "stopped".
        let mut saw_disconnect = false;
        let mut saw_stopped = false;
        while let Ok(ev) = rx.try_recv() {
            match ev {
                ServerEvent::ClientDisconnected { .. } => saw_disconnect = true,
                ServerEvent::StatusChanged { new, .. } if new == "stopped" => saw_stopped = true,
                _ => {}
            }
        }
        assert!(saw_disconnect, "expected ClientDisconnected event");
        assert!(saw_stopped, "expected StatusChanged stopped event");
    }

    #[tokio::test]
    async fn on_disconnected_classifies_short_lived_reset_as_probe() {
        // This test exercises the warn! path; we can't easily capture tracing
        // output here, but we can at least confirm the call doesn't panic and
        // the action is Continue (probe is not a fatal error).
        let (tx, _rx) = mpsc::unbounded_channel();
        let mut handler = LamcoConnectionHandler::new(
            tx,
            None,
            always_alive_callback(),
            Arc::new(tokio::sync::broadcast::channel::<()>(1).0),
        );
        let peer = test_peer();

        let _ = handler.on_accept_async(&peer).await;
        let err = ServerError::reason("test", "Connection reset by peer (os error 104)");
        let action = handler
            .on_disconnected_async(&peer, Duration::from_millis(50), Some(&err))
            .await;
        assert_eq!(action, PostConnectionAction::Continue);
    }

    #[tokio::test]
    async fn on_disconnect_callback_actually_invoked() {
        // Sanity: the callback closure must be called exactly once per
        // on_disconnected_async invocation.
        let invoked = Arc::new(AtomicBool::new(false));
        let invoked_clone = invoked.clone();
        let cb: OnDisconnectFn = Arc::new(move |_served| {
            let inv = invoked_clone.clone();
            Box::pin(async move {
                inv.store(true, Ordering::SeqCst);
                true
            })
        });
        let (tx, _rx) = mpsc::unbounded_channel();
        let mut handler = LamcoConnectionHandler::new(
            tx,
            None,
            cb,
            Arc::new(tokio::sync::broadcast::channel::<()>(1).0),
        );

        let _ = handler
            .on_disconnected_async(&test_peer(), Duration::from_secs(1), None)
            .await;
        assert!(invoked.load(Ordering::SeqCst));
    }
}
