//! Async event stream over an EI socket, plus the client-side handshake.
//!
//! **Execution paths:** Mutter RemoteDesktop `ConnectToEIS` (`mutter_direct`)
//! and Portal RemoteDesktop `ConnectToEIS` (`libei`). **Status:** Active.
//!
//! reis ships a tokio stream of its own, but its `poll_next` returns `Pending`
//! straight after clearing readiness, without polling the descriptor again, so
//! that poll registers no waker. tokio keeps stale readiness on purpose, which
//! makes "ready, empty read, clear, pending" a routine sequence, and the next
//! bytes from the compositor then wake nobody. On 2026-09-02 this left a GNOME
//! server asleep inside the handshake with the compositor's reply unread on the
//! socket; the handshake runs on the accept task, so no later client could
//! connect either. This stream loops back to `poll_read_ready` after every
//! drain, so whenever it yields `Pending` a waker is in place.

use std::{
    io,
    pin::Pin,
    task::{Context, Poll, ready},
    time::Duration,
};

use anyhow::{Result, bail};
use futures_util::{Stream, StreamExt};
use reis::{
    PendingRequestResult, ei,
    handshake::{EiHandshaker, HandshakeResp},
};
use tokio::io::{Interest, unix::AsyncFd};
use tracing::debug;

/// A compositor answers the handshake within a millisecond on the same host;
/// anything approaching this is a dead socket, not a slow one.
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);

pub struct EisEventStream {
    fd: AsyncFd<ei::Context>,
}

impl EisEventStream {
    pub fn new(context: ei::Context) -> io::Result<Self> {
        AsyncFd::with_interest(context, Interest::READABLE).map(|fd| Self { fd })
    }
}

impl Stream for EisEventStream {
    type Item = io::Result<PendingRequestResult<ei::Event>>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        loop {
            if let Some(event) = this.fd.get_ref().pending_event() {
                return Poll::Ready(Some(Ok(event)));
            }
            let mut guard = match ready!(this.fd.poll_read_ready(cx)) {
                Ok(guard) => guard,
                Err(e) => return Poll::Ready(Some(Err(e))),
            };
            match guard.get_inner().read() {
                // Drained to WouldBlock. Go round again: hand out whatever is
                // now buffered, then poll readiness so a Pending carries a waker.
                Ok(_) => guard.clear_ready(),
                Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Poll::Ready(None),
                Err(e) => return Poll::Ready(Some(Err(e))),
            }
        }
    }
}

/// Run the EI client handshake as an input sender, bounded by `timeout`.
///
/// The compositor's seat and device events follow immediately after the
/// `connection` event on the same stream; the caller drains those.
pub async fn ei_handshake(
    events: &mut EisEventStream,
    name: &str,
    timeout: Duration,
) -> Result<HandshakeResp> {
    let handshake = async {
        let mut handshaker = EiHandshaker::new(name, ei::handshake::ContextType::Sender);
        while let Some(result) = events.next().await {
            let event = match result? {
                PendingRequestResult::Request(event) => event,
                PendingRequestResult::ParseError(err) => {
                    bail!("EIS parse error during handshake: {err}")
                }
                PendingRequestResult::InvalidObject(id) => {
                    debug!("[eis] event for unknown object {id} during handshake, ignoring");
                    continue;
                }
            };
            if let Some(resp) = handshaker.handle_event(event)? {
                return Ok(resp);
            }
        }
        bail!("EIS socket closed during handshake")
    };
    match tokio::time::timeout(timeout, handshake).await {
        Ok(result) => result,
        Err(_) => bail!("EIS handshake timed out after {}s", timeout.as_secs()),
    }
}
