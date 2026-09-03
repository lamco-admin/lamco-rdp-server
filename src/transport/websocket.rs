//! WebSocket + RDCleanPath transport for browser/WASM clients.
//!
//! Eliminates `ws-rdp-proxy` from production for `lamco-rdp-wasm` and any other
//! RDCleanPath-aware client. See:
//! - `docs/design/transport/TRANSPORT-PHASE-3B-SDS-2026-05-16.md`
//! - `~/lamco-admin/shared/analysis/TRANSPORT-IRONRDP-PRE-AUTH-RESEARCH-2026-05-16.md`
//! - `docs/analysis/TRANSPORT-LOOPBACK-TLS-SPIKE-2026-05-16.md`
//!
//! Architecture: the listener accepts WSS connections (TLS-terminated at the
//! WS layer), performs the RDCleanPath PDU exchange synchronously, then yields
//! an `AcceptedConnection` with `AcceptorMode::PreAuthenticated` and a
//! `PreFedStream` that has the X.224 Connection Request pre-buffered (so
//! IronRDP's `accept_begin` reads it transparently) and swallows the X.224
//! Connection Confirm the acceptor produces (because we already sent the
//! Confirm to the client inside the RDCleanPath Response).
//!
//! After `accept_begin` returns `BeginResult::ShouldUpgrade`, the dispatcher
//! invokes `rdp_server.run_connection_with(stream, TransportTls::AlreadyDone)`
//! (upstream PR #1281, reshaped from our fork's original
//! `run_connection_pre_authenticated` patch) which skips the IronRDP-managed
//! TLS step.

use std::{
    io,
    net::SocketAddr,
    path::PathBuf,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};

use anyhow::{Context as _, Result, anyhow};
use futures::{Sink, SinkExt, Stream, StreamExt};
use ironrdp_rdcleanpath::{RDCleanPath, RDCleanPathPdu};
use serde::{Deserialize, Serialize};
use tokio::{
    io::{AsyncRead, AsyncWrite, ReadBuf},
    net::TcpListener as TokioTcpListener,
    sync::mpsc,
    task::JoinHandle,
};
use tokio_rustls::TlsAcceptor;
use tokio_tungstenite::{WebSocketStream, tungstenite::Message};
use tracing::{debug, info};

use super::listener::{AcceptedConnection, AcceptorMode, Listener, PeerAddr, TransportError};

// =============================================================================
// WsByteStream — WebSocket binary frames <-> AsyncRead+AsyncWrite adapter
// =============================================================================

/// Wraps a `WebSocketStream` as an `AsyncRead + AsyncWrite` byte stream.
///
/// Each `poll_write` call becomes one binary frame; reads concatenate binary
/// frame payloads into a contiguous byte stream. Text frames are logged and
/// ignored. Close frames terminate reads with EOF.
pub struct WsByteStream<S> {
    inner: WebSocketStream<S>,
    read_buf: Vec<u8>,
    read_pos: usize,
    eof: bool,
}

impl<S> WsByteStream<S>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    pub fn new(inner: WebSocketStream<S>) -> Self {
        Self {
            inner,
            read_buf: Vec::new(),
            read_pos: 0,
            eof: false,
        }
    }
}

impl<S> AsyncRead for WsByteStream<S>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        loop {
            // Drain pending buffer first.
            if self.read_pos < self.read_buf.len() {
                let remaining = &self.read_buf[self.read_pos..];
                let n = remaining.len().min(buf.remaining());
                buf.put_slice(&remaining[..n]);
                self.read_pos += n;
                return Poll::Ready(Ok(()));
            }
            if self.eof {
                return Poll::Ready(Ok(())); // EOF: returning Ok with no bytes
            }
            // Need more bytes — pull next WS frame.
            self.read_buf.clear();
            self.read_pos = 0;
            match Pin::new(&mut self.inner).poll_next(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(None) => {
                    self.eof = true;
                    return Poll::Ready(Ok(()));
                }
                Poll::Ready(Some(Err(e))) => {
                    return Poll::Ready(Err(io::Error::other(e)));
                }
                Poll::Ready(Some(Ok(Message::Binary(bytes)))) => {
                    self.read_buf = bytes.into();
                    // loop back to drain
                }
                Poll::Ready(Some(Ok(Message::Close(_)))) => {
                    self.eof = true;
                    return Poll::Ready(Ok(()));
                }
                Poll::Ready(Some(Ok(other))) => {
                    debug!(?other, "ignoring non-binary WS message");
                    // loop back; will poll again
                }
            }
        }
    }
}

impl<S> AsyncWrite for WsByteStream<S>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        // Each write becomes one binary frame. tokio-tungstenite's Sink is
        // ready-most-of-the-time; if not ready, we report Pending.
        match Pin::new(&mut self.inner).poll_ready(cx) {
            Poll::Pending => return Poll::Pending,
            Poll::Ready(Err(e)) => {
                return Poll::Ready(Err(io::Error::other(e)));
            }
            Poll::Ready(Ok(())) => {}
        }
        let msg = Message::Binary(buf.to_vec().into());
        match Pin::new(&mut self.inner).start_send(msg) {
            Ok(()) => Poll::Ready(Ok(buf.len())),
            Err(e) => Poll::Ready(Err(io::Error::other(e))),
        }
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match Pin::new(&mut self.inner).poll_flush(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(Ok(())) => Poll::Ready(Ok(())),
            Poll::Ready(Err(e)) => Poll::Ready(Err(io::Error::other(e))),
        }
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match Pin::new(&mut self.inner).poll_close(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(Ok(())) => Poll::Ready(Ok(())),
            Poll::Ready(Err(e)) => Poll::Ready(Err(io::Error::other(e))),
        }
    }
}

// =============================================================================
// PreFedStream — pre-read X.224 Request + swallow X.224 Confirm + pass-through
// =============================================================================

/// State machine for swallowing exactly one TPKT-framed PDU from the write
/// side. Used to discard the X.224 Connection Confirm that IronRDP's
/// `accept_begin` produces (we already sent the Confirm to the client
/// inside the RDCleanPath Response).
enum SwallowState {
    /// Accumulating the 4-byte TPKT header. Vec contains bytes collected so far.
    Header(Vec<u8>),
    /// Header parsed; consuming the body. Field is bytes remaining to swallow.
    Body(usize),
    /// Done swallowing; writes pass through to the inner stream.
    PassThrough,
}

/// Wraps an inner `AsyncRead + AsyncWrite` stream with:
/// - A pre-read buffer that yields the X.224 Connection Request bytes to the
///   first reads (so IronRDP's `accept_begin` reads it without ever touching
///   the underlying WS stream)
/// - A write-side state machine that swallows exactly one TPKT-framed PDU
///   (the X.224 Connection Confirm produced by `accept_begin`) before
///   pass-through writes resume
pub struct PreFedStream<S> {
    inner: S,
    pre_read: Vec<u8>,
    pre_read_pos: usize,
    swallow: SwallowState,
}

impl<S> PreFedStream<S>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    pub fn new(inner: S, x224_request_bytes: Vec<u8>) -> Self {
        Self {
            inner,
            pre_read: x224_request_bytes,
            pre_read_pos: 0,
            swallow: SwallowState::Header(Vec::with_capacity(4)),
        }
    }
}

impl<S> AsyncRead for PreFedStream<S>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        if self.pre_read_pos < self.pre_read.len() {
            let remaining = &self.pre_read[self.pre_read_pos..];
            let n = remaining.len().min(buf.remaining());
            buf.put_slice(&remaining[..n]);
            self.pre_read_pos += n;
            return Poll::Ready(Ok(()));
        }
        Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}

impl<S> AsyncWrite for PreFedStream<S>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        if buf.is_empty() {
            return Poll::Ready(Ok(0));
        }
        match &mut self.swallow {
            SwallowState::PassThrough => Pin::new(&mut self.inner).poll_write(cx, buf),
            SwallowState::Header(header_buf) => {
                let needed = 4 - header_buf.len();
                let take = buf.len().min(needed);
                header_buf.extend_from_slice(&buf[..take]);
                if header_buf.len() < 4 {
                    // Still need more for the header; consume what we got, report progress.
                    return Poll::Ready(Ok(take));
                }
                // Full TPKT header collected. Parse length (bytes 2-3, big-endian, total PDU length including header).
                let total_len = u16::from_be_bytes([header_buf[2], header_buf[3]]) as usize;
                if total_len < 4 {
                    return Poll::Ready(Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("invalid TPKT length {total_len}"),
                    )));
                }
                let body_remaining = total_len - 4;
                self.swallow = if body_remaining == 0 {
                    SwallowState::PassThrough
                } else {
                    SwallowState::Body(body_remaining)
                };
                Poll::Ready(Ok(take))
            }
            SwallowState::Body(remaining) => {
                let take = buf.len().min(*remaining);
                *remaining -= take;
                if *remaining == 0 {
                    self.swallow = SwallowState::PassThrough;
                }
                Poll::Ready(Ok(take))
            }
        }
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

// =============================================================================
// WebSocketTransportConfig + WebSocketListenerImpl
// =============================================================================

/// AF_INET WebSocket transport for RDCleanPath-aware clients.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebSocketTransportConfig {
    /// Default false. Set to true to enable the WSS listener.
    #[serde(default)]
    pub enabled: bool,
    /// Bind address for the WSS listener (e.g. `"[::]:3390"`). Defaults to
    /// `[::]:3390` (parallel to the standard RDP port 3389).
    #[serde(default = "default_ws_listen_addr")]
    pub listen_addr: String,
    /// Path to TLS certificate file. If absent, uses `security.cert_path`.
    #[serde(default)]
    pub cert_path: Option<PathBuf>,
    /// Path to TLS private key file. If absent, uses `security.key_path`.
    #[serde(default)]
    pub key_path: Option<PathBuf>,
}

impl Default for WebSocketTransportConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            listen_addr: default_ws_listen_addr(),
            cert_path: None,
            key_path: None,
        }
    }
}

fn default_ws_listen_addr() -> String {
    "[::]:3390".to_string()
}

/// WebSocket + RDCleanPath listener. A background task accepts WSS connections,
/// performs the RDCleanPath PDU exchange per connection, and forwards each
/// `PreAuthenticated` `AcceptedConnection` (stream:
/// `PreFedStream<WsByteStream<TlsStream>>`) to `accept()` over a channel.
pub struct WebSocketListenerImpl {
    /// Completed connections from the background accept task. Receiving is
    /// cancellation-safe: when the shared accept dispatcher drops this listener's
    /// `accept()` future (another transport won the select! race), the in-flight
    /// TLS/WS/RDCleanPath handshake keeps running in the background and its result
    /// stays queued for the next `accept()`, instead of being discarded mid-TLS
    /// as it was when the handshake ran inline in `accept()`.
    rx: mpsc::Receiver<Result<AcceptedConnection, TransportError>>,
    accept_task: JoinHandle<()>,
}

impl Drop for WebSocketListenerImpl {
    fn drop(&mut self) {
        // Stop accepting once the listener is gone (e.g. deployment teardown).
        self.accept_task.abort();
    }
}

impl WebSocketListenerImpl {
    pub async fn bind(
        listen_addr: SocketAddr,
        tls_acceptor: TlsAcceptor,
        server_cert_chain: Vec<Vec<u8>>,
        validator: Arc<dyn crate::transport::proxy_auth::ProxyAuthValidator>,
    ) -> Result<Self, TransportError> {
        crate::server::check_port_available(&listen_addr);
        let tcp = TokioTcpListener::bind(listen_addr)
            .await
            .map_err(TransportError::Io)?;
        info!(%listen_addr, "WebSocket+RDCleanPath listener bound");

        // Small buffer absorbs bursts; send().await backpressures the accept task
        // when the dispatcher (which serves one connection at a time) falls behind.
        let (tx, rx) = mpsc::channel(16);
        let accept_task = tokio::spawn(accept_loop(
            tcp,
            tls_acceptor,
            server_cert_chain,
            listen_addr,
            validator,
            tx,
        ));
        Ok(Self { rx, accept_task })
    }
}

#[async_trait::async_trait]
impl Listener for WebSocketListenerImpl {
    fn transport_name(&self) -> &'static str {
        "websocket"
    }

    async fn accept(&mut self) -> Result<Option<AcceptedConnection>, TransportError> {
        // Cancellation-safe: a dropped recv() leaves queued connections intact;
        // the handshake itself runs in accept_loop, off this future.
        match self.rx.recv().await {
            Some(result) => result.map(Some),
            // Sender dropped: the accept task ended (listener shutting down).
            None => Ok(None),
        }
    }
}

/// Background acceptor: TCP-accept then full WSS + RDCleanPath handshake per
/// connection, forwarding each result to `accept()` over `tx`. Running the
/// handshake here — rather than inline in the dispatcher-cancellable `accept()`
/// future — is what stops a lost select! race from discarding a connection that
/// was already dequeued and mid-TLS.
async fn accept_loop(
    tcp: TokioTcpListener,
    tls_acceptor: TlsAcceptor,
    server_cert_chain: Vec<Vec<u8>>,
    listen_addr: SocketAddr,
    validator: Arc<dyn crate::transport::proxy_auth::ProxyAuthValidator>,
    tx: mpsc::Sender<Result<AcceptedConnection, TransportError>>,
) {
    loop {
        let result = accept_one(
            &tcp,
            &tls_acceptor,
            &server_cert_chain,
            listen_addr,
            validator.as_ref(),
        )
        .await;
        if tx.send(result).await.is_err() {
            debug!("WebSocket accept task stopping: listener dropped");
            return;
        }
    }
}

async fn accept_one(
    tcp: &TokioTcpListener,
    tls_acceptor: &TlsAcceptor,
    server_cert_chain: &[Vec<u8>],
    listen_addr: SocketAddr,
    validator: &dyn crate::transport::proxy_auth::ProxyAuthValidator,
) -> Result<AcceptedConnection, TransportError> {
    let (tcp_stream, peer_addr) = tcp.accept().await.map_err(TransportError::Io)?;
    debug!(%peer_addr, "TCP accepted for WSS upgrade");

    // 1. TLS handshake (WSS terminator)
    let tls_stream = tls_acceptor
        .accept(tcp_stream)
        .await
        .map_err(|e| TransportError::Handshake(anyhow!("TLS accept failed: {e}")))?;

    // 2. WebSocket handshake. Capture Origin header for diagnostics.
    let mut origin: Option<String> = None;
    // The callback's Result shape is fixed by tokio-tungstenite's accept_hdr_async
    // signature, so the size of its Err variant is not ours to change.
    #[expect(
        clippy::result_large_err,
        reason = "Err type is imposed by tokio-tungstenite's handshake callback signature"
    )]
    let ws = tokio_tungstenite::accept_hdr_async(
        tls_stream,
        |req: &tokio_tungstenite::tungstenite::http::Request<()>,
         resp: tokio_tungstenite::tungstenite::http::Response<()>| {
            if let Some(o) = req.headers().get("origin").and_then(|v| v.to_str().ok()) {
                origin = Some(o.to_string());
            }
            Ok(resp)
        },
    )
    .await
    .map_err(|e| TransportError::Handshake(anyhow!("WS handshake failed: {e}")))?;

    let mut ws_byte_stream = WsByteStream::new(ws);

    // 3. RDCleanPath PDU exchange
    let x224_request_bytes = run_rdcleanpath_exchange(
        &mut ws_byte_stream,
        server_cert_chain,
        listen_addr,
        validator,
    )
    .await
    .map_err(|e| TransportError::Handshake(anyhow!("RDCleanPath exchange failed: {e}")))?;

    // 4. Wrap with PreFedStream — IronRDP's accept_begin reads the X.224 Request
    //    from pre_read, writes the X.224 Confirm which we swallow, then proceeds
    //    via the pass-through stream.
    let pre_fed = PreFedStream::new(ws_byte_stream, x224_request_bytes);

    Ok(AcceptedConnection {
        peer: PeerAddr::WebSocket {
            peer: peer_addr,
            origin,
        },
        stream: Box::new(pre_fed),
        mode: AcceptorMode::PreAuthenticated,
    })
}

/// Read RDCleanPath Request, validate, send Response with synthesized X.224
/// Confirm and our server cert chain. Returns the X.224 Connection Request
/// bytes for the PreFedStream to feed to IronRDP's `accept_begin`.
async fn run_rdcleanpath_exchange<S>(
    ws: &mut WsByteStream<S>,
    server_cert_chain: &[Vec<u8>],
    listen_addr: SocketAddr,
    validator: &dyn crate::transport::proxy_auth::ProxyAuthValidator,
) -> Result<Vec<u8>>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    // Read first WS binary frame as RDCleanPath Request.
    let msg_bytes = read_one_ws_binary_frame(&mut ws.inner)
        .await
        .context("reading RDCleanPath request frame")?;

    let pdu = RDCleanPathPdu::from_der(&msg_bytes).context("decoding RDCleanPath request PDU")?;

    let rdcleanpath = pdu.into_enum().context("interpreting RDCleanPath PDU")?;

    let RDCleanPath::Request {
        proxy_auth,
        x224_connection_request,
        destination,
        ..
    } = rdcleanpath
    else {
        anyhow::bail!("expected RDCleanPath Request, got Response or Err");
    };

    // proxy_auth: fail-closed validation. The two-layer console auth model puts
    // the PVE ticket + VM.Console ACL at pveproxy; on this direct WebSocket path
    // the `proxy_auth` field carries a deployment shared secret validated here.
    // An unconfigured deployment uses DenyAll, so it rejects rather than opens.
    validator
        .validate(proxy_auth.as_bytes(), &destination)
        .map_err(|rejected| anyhow!("{rejected}"))?;
    debug!(auth_len = proxy_auth.len(), %destination, "proxy_auth accepted");

    let x224_request_bytes = x224_connection_request.as_bytes().to_vec();
    debug!(
        %destination,
        x224_len = x224_request_bytes.len(),
        "RDCleanPath Request decoded"
    );

    // Synthesize X.224 Connection Confirm from the Request shape (echo src-ref).
    let x224_confirm = synthesize_x224_confirm(&x224_request_bytes)
        .context("synthesizing X.224 Connection Confirm")?;

    // Build and send RDCleanPath Response.
    let response = RDCleanPathPdu::new_response(
        listen_addr.to_string(),
        x224_confirm,
        server_cert_chain.iter().cloned(),
    )
    .map_err(|e| anyhow!("building RDCleanPath response: {e:?}"))?;

    let response_der = response
        .to_der()
        .map_err(|e| anyhow!("encoding RDCleanPath response: {e:?}"))?;

    ws.inner
        .send(Message::Binary(response_der.into()))
        .await
        .context("sending RDCleanPath response frame")?;

    info!("RDCleanPath exchange complete; handing off to IronRDP acceptor");
    Ok(x224_request_bytes)
}

/// Read exactly one binary WS frame, ignoring text/ping/pong/close-before-data.
async fn read_one_ws_binary_frame<S>(ws: &mut WebSocketStream<S>) -> Result<Vec<u8>>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    loop {
        match ws.next().await {
            Some(Ok(Message::Binary(bytes))) => return Ok(bytes.into()),
            Some(Ok(Message::Close(_))) => anyhow::bail!("WS closed before binary frame"),
            Some(Ok(other)) => {
                debug!(
                    ?other,
                    "ignoring non-binary WS message during RDCleanPath exchange"
                );
            }
            Some(Err(e)) => return Err(anyhow!("WS error: {e}")),
            None => anyhow::bail!("WS stream ended before binary frame"),
        }
    }
}

/// Synthesize an X.224 Connection Confirm PDU advertising PROTOCOL_SSL
/// selection (matching the RdpServerSecurity::Tls advertisement that the
/// IronRDP acceptor on the server side would also produce).
///
/// Echoes the dst-ref from the Request as our src-ref (per X.224 convention)
/// — most RDP clients tolerate either echoed or zero, but echoing is safer.
fn synthesize_x224_confirm(x224_request: &[u8]) -> Result<Vec<u8>> {
    // TPKT header (4 bytes): version=3, reserved=0, total_length big-endian
    // X.224 CC header (7 bytes): LI, code=CC (0xD0), dst-ref, src-ref, class-options=0
    // RDP_NEG_RSP (8 bytes): type=0x02, flags=0, length=8, selectedProtocol=1 (SSL)
    //
    // Parse the Request's src-ref to echo back as our dst-ref.
    if x224_request.len() < 11 {
        anyhow::bail!("X.224 Request too short ({} bytes)", x224_request.len());
    }
    // TPKT version check
    if x224_request[0] != 3 {
        anyhow::bail!(
            "X.224 Request has invalid TPKT version: {}",
            x224_request[0]
        );
    }
    // X.224 CR layout: TPKT(4) + LI(1) + Code(1) + DstRef(2) + SrcRef(2) + ClassOpt(1) + ...
    // Byte indices: TPKT=0..4, LI=4, Code=5, DstRef=6..8, SrcRef=8..10, ClassOpt=10
    let client_src_ref = [x224_request[8], x224_request[9]];

    let total_length: u16 = 19;
    let mut confirm = Vec::with_capacity(19);
    confirm.extend_from_slice(&[3, 0, (total_length >> 8) as u8, total_length as u8]);
    // X.224 CC: LI=14, code=0xD0 (CC), dst-ref=client's src-ref, src-ref=0, class-options=0
    confirm.extend_from_slice(&[14, 0xD0, client_src_ref[0], client_src_ref[1], 0, 0, 0]);
    // RDP_NEG_RSP: type=0x02, flags=0, length=8 (little-endian u16), selectedProtocol=1 (SSL) as u32
    confirm.extend_from_slice(&[0x02, 0, 8, 0, 1, 0, 0, 0]);
    debug_assert_eq!(confirm.len(), 19);
    Ok(confirm)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn synthesize_x224_confirm_basic_shape() {
        // Minimal X.224 Connection Request: TPKT + X.224 CR header (no negotiation).
        // TPKT(03 00 00 0B) + LI(06) + CR(E0) + DstRef(00 00) + SrcRef(AB CD) + ClassOpt(00)
        let req = vec![
            0x03, 0x00, 0x00, 0x0B, 0x06, 0xE0, 0x00, 0x00, 0x12, 0x34, 0x00,
        ];
        let confirm = synthesize_x224_confirm(&req).unwrap();
        assert_eq!(confirm.len(), 19);
        // TPKT version
        assert_eq!(confirm[0], 3);
        // Total length
        assert_eq!(u16::from_be_bytes([confirm[2], confirm[3]]), 19);
        // X.224 CC code
        assert_eq!(confirm[5], 0xD0);
        // dst-ref echoes Request's src-ref (12 34)
        assert_eq!(&confirm[6..8], &[0x12, 0x34]);
        // RDP_NEG_RSP type
        assert_eq!(confirm[11], 0x02);
        // selectedProtocol = SSL (1)
        assert_eq!(
            u32::from_le_bytes([confirm[15], confirm[16], confirm[17], confirm[18]]),
            1
        );
    }

    #[test]
    fn synthesize_x224_confirm_rejects_short_request() {
        let req = vec![0x03, 0x00, 0x00, 0x05];
        assert!(synthesize_x224_confirm(&req).is_err());
    }

    #[test]
    fn synthesize_x224_confirm_rejects_bad_tpkt() {
        let req = vec![0xFF; 16];
        assert!(synthesize_x224_confirm(&req).is_err());
    }
}
