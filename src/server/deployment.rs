//! `WlrDirectDeployment` — `AcceptDeployment` for the lamco-rdp-server binary
//! running on a user desktop (the wlr-direct / Portal / Mutter Direct paths).
//!
//! Encapsulates the per-binary differences described in
//! `docs/design/transport/TRANSPORT-PHASE-1-RETROFIT-SDS-2026-05-16.md`:
//! TCP-only transport from TOML config, mpsc D-Bus event sink, optional PAM
//! validator, broadcast-channel shutdown, and a portal-validity closure that
//! delegates to `perform_disconnect_cleanup`.

use std::{future::Future, pin::Pin, sync::Arc};

use anyhow::{Context, Result};
use tokio::sync::{broadcast, mpsc};

use super::{LamcoDisplayHandler, is_standard_rdp_security, perform_disconnect_cleanup};
use crate::{
    config::Config,
    dbus::ServerEvent,
    health::HealthSubscriber,
    security::auth::PamValidator,
    session::strategy::{PipeWireAccess, SessionHandle, SessionLifecyclePolicy},
    transport::{
        AcceptDeployment, LamcoConnectionHandler, Listener,
        handler::{OnConnectFn, OnDisconnectFn},
    },
};

pub(crate) struct WlrDirectDeployment {
    config: Arc<Config>,
    display_handler: Arc<LamcoDisplayHandler>,
    health_subscriber: Option<HealthSubscriber>,
    event_tx: mpsc::UnboundedSender<ServerEvent>,
    pam_validator: Option<Arc<PamValidator>>,
    shutdown_broadcast: Arc<broadcast::Sender<()>>,
    /// Session handle whose lifecycle policy drives per-connect establishment
    /// and per-disconnect release (see `build_handler`).
    session_handle: Arc<dyn SessionHandle>,
}

impl WlrDirectDeployment {
    pub(crate) fn new(
        config: Arc<Config>,
        display_handler: Arc<LamcoDisplayHandler>,
        health_subscriber: Option<HealthSubscriber>,
        event_tx: mpsc::UnboundedSender<ServerEvent>,
        pam_validator: Option<Arc<PamValidator>>,
        shutdown_broadcast: Arc<broadcast::Sender<()>>,
        session_handle: Arc<dyn SessionHandle>,
    ) -> Self {
        Self {
            config,
            display_handler,
            health_subscriber,
            event_tx,
            pam_validator,
            shutdown_broadcast,
            session_handle,
        }
    }
}

#[async_trait::async_trait]
impl AcceptDeployment for WlrDirectDeployment {
    fn name(&self) -> &'static str {
        // Despite the type's name, this backs every desktop-sharing session
        // strategy (Portal, Mutter Direct, libei, wlr-direct alike), not just
        // wlr-direct — the log-visible label reflects that, matching how
        // QemuDeployment's name() is "qemu" rather than a specific strategy.
        "desktop"
    }

    async fn build_listeners(&mut self) -> Result<Vec<Box<dyn Listener>>> {
        let transports = self
            .config
            .server
            .transports
            .resolve(&self.config.server.listen_addr)
            .context("Failed to resolve transport configuration")?;

        // Security-posture audit for the resolved transports (issue #52). Standard
        // RDP Security is plaintext and only appropriate on isolated transports;
        // Hyper-V ESM (vsock) conversely *requires* it because VMConnect never does
        // a TLS handshake.
        let security_mode = self.config.security.security_mode.as_str();
        if is_standard_rdp_security(security_mode) {
            if let Some(tcp) = transports.tcp.as_ref()
                && !tcp.listen_addr.ip().is_loopback()
            {
                tracing::warn!(
                    addr = %tcp.listen_addr,
                    "security_mode=\"{security_mode}\" serves UNENCRYPTED RDP on a routable TCP \
                     address. Confine Standard RDP Security to vsock/loopback: disable \
                     [server.transports.tcp] or bind it to 127.0.0.1."
                );
            }
        } else if transports.vsock.is_some() {
            tracing::warn!(
                "vsock transport active (Hyper-V Enhanced Session Mode) but security_mode=\
                 \"{security_mode}\". VMConnect speaks Standard RDP Security and will fail the TLS \
                 handshake (\"received corrupt message ... InvalidContentType\"). Set \
                 security_mode=\"rdp\" to accept Enhanced Session Mode clients."
            );
        }
        #[cfg_attr(
            not(feature = "websocket"),
            expect(unused_mut, reason = "websocket cfg appends to listeners below")
        )]
        let mut listeners = transports
            .build_listeners()
            .await
            .context("Failed to bind one or more transport listeners")?;

        // WebSocket listener (Phase 3b) needs TLS context, so it's built here
        // (in the deployment) rather than inside ResolvedTransports::build_listeners.
        #[cfg(feature = "websocket")]
        if let Some(ws_cfg) = transports.websocket.as_ref() {
            use tokio_rustls::TlsAcceptor;

            use crate::{security::tls::TlsConfig, transport::websocket::WebSocketListenerImpl};

            let cert_path = ws_cfg
                .cert_path
                .as_ref()
                .unwrap_or(&self.config.security.cert_path);
            let key_path = ws_cfg
                .key_path
                .as_ref()
                .unwrap_or(&self.config.security.key_path);
            let tls_config = TlsConfig::from_files(cert_path, key_path).with_context(|| {
                format!(
                    "Failed to load WebSocket TLS identity from {} / {}",
                    cert_path.display(),
                    key_path.display()
                )
            })?;
            let tls_acceptor = TlsAcceptor::from(tls_config.server_config());
            let cert_chain_der = tls_config.cert_chain_der();

            // proxy_auth validator for the direct WebSocket path. Fail-closed:
            // the PVE ticket + VM.Console ACL are enforced at pveproxy (the
            // node-local unix-socket path); this path authenticates a deployment
            // shared secret carried in the RDCleanPath proxy_auth field. The WSS
            // listener is always TLS, so the secret never travels in clear.
            use crate::transport::proxy_auth::{
                AllowAllInsecure, DenyAll, ProxyAuthValidator, SharedSecretValidator,
            };
            let proxy_auth: std::sync::Arc<dyn ProxyAuthValidator> = if ws_cfg
                .proxy_auth_allow_insecure
            {
                tracing::warn!(
                    listen_addr = %ws_cfg.listen_addr,
                    "WebSocket proxy_auth validation disabled (allow_insecure) — accepting all \
                     RDCleanPath connections. Development only; never on a reachable listener."
                );
                std::sync::Arc::new(AllowAllInsecure)
            } else if let Some(secret_path) = ws_cfg.proxy_auth_secret_path.as_ref() {
                let mut secret = std::fs::read(secret_path).with_context(|| {
                    format!("reading proxy_auth secret from {}", secret_path.display())
                })?;
                // Trim a single trailing newline (handles `echo secret > file`).
                if secret.last() == Some(&b'\n') {
                    secret.pop();
                }
                if secret.last() == Some(&b'\r') {
                    secret.pop();
                }
                let validator = SharedSecretValidator::new(secret).ok_or_else(|| {
                    anyhow::anyhow!("proxy_auth secret file {} is empty", secret_path.display())
                })?;
                tracing::info!("WebSocket proxy_auth: shared-secret validator enabled");
                std::sync::Arc::new(validator)
            } else {
                tracing::warn!(
                    "WebSocket proxy_auth has no shared secret configured — rejecting all direct \
                     RDCleanPath connections (fail-closed). Set proxy_auth_secret_path to enable."
                );
                std::sync::Arc::new(DenyAll)
            };

            let ws_listener = WebSocketListenerImpl::bind(
                ws_cfg.listen_addr,
                tls_acceptor,
                cert_chain_der,
                proxy_auth,
            )
            .await
            .with_context(|| {
                format!(
                    "Failed to bind WebSocket listener on {}",
                    ws_cfg.listen_addr
                )
            })?;
            listeners.push(Box::new(ws_listener));
        }

        Ok(listeners)
    }

    fn build_handler(&mut self) -> LamcoConnectionHandler {
        let display_handler = self.display_handler.clone();
        let health_subscriber = self.health_subscriber.clone();

        // Disconnect: run the per-connection teardown, then release the
        // compositor session per policy. A PerConnection session is
        // re-established on the next connect, so its close is expected and never
        // a reason to stop accepting clients.
        let disc_session = Arc::clone(&self.session_handle);
        let on_disconnect: OnDisconnectFn = Arc::new(move |served: bool| {
            let dh = display_handler.clone();
            let hs = health_subscriber.clone();
            let session = Arc::clone(&disc_session);
            Box::pin(async move {
                let alive = perform_disconnect_cleanup(&dh, hs.as_ref(), served).await;
                if session.lifecycle_policy() == SessionLifecyclePolicy::PerConnection {
                    // Only release for a connection that actually used the
                    // session. A fast handshake-failure probe never served, so
                    // leaving its session intact lets the following real
                    // connection reuse it instead of churning a fresh session.
                    if served {
                        session.release_after_client().await;
                    }
                    true
                } else {
                    alive
                }
            })
        });

        // Connect: establish (or reuse) the compositor session, then rebind the
        // capture pipeline if re-establishment moved to a new PipeWire node.
        let conn_dh = self.display_handler.clone();
        let conn_session = Arc::clone(&self.session_handle);
        let on_connect: OnConnectFn = Arc::new(move || {
            let dh = conn_dh.clone();
            let session = Arc::clone(&conn_session);
            Box::pin(async move {
                let old_node = match session.pipewire_access() {
                    PipeWireAccess::NodeId(n) => n,
                    _ => 0,
                };
                match session.establish_for_client().await {
                    Ok((streams, reestablished)) => {
                        // Rebind capture only when the session was actually
                        // re-established. The compositor can reuse the node id
                        // for a brand-new stream, so rebind unconditionally in
                        // that case rather than trusting node-number equality.
                        if reestablished && let Some(s) = streams.first() {
                            dh.rebind_capture_node(old_node, s.node_id, s.width, s.height)
                                .await;
                        }
                        true
                    }
                    Err(e) => {
                        tracing::error!("Failed to establish session for incoming client: {e:#}");
                        false
                    }
                }
            })
        });

        LamcoConnectionHandler::new(
            self.event_tx.clone(),
            self.pam_validator.clone(),
            on_disconnect,
            Arc::clone(&self.shutdown_broadcast),
        )
        .with_on_connect(on_connect)
    }

    fn shutdown_signal(&mut self) -> Pin<Box<dyn Future<Output = ()> + Send + 'static>> {
        let mut rx = self.shutdown_broadcast.subscribe();
        Box::pin(async move {
            // Any recv outcome (Ok, Closed, Lagged) is treated as shutdown.
            let _ = rx.recv().await;
        })
    }
}
