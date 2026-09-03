//! MS-RDPEI Server Factory Implementation
//!
//! Implements the `RdpeiServerFactory` trait from IronRDP, providing the
//! integration point between lamco-rdp-server and the IronRDP RdpServer
//! builder for multitouch input.
//!
//! `RdpeiHandler::touch()` is a synchronous callback (matching
//! `RdpServerInputHandler::mouse()`/`keyboard()`), so touch frames are
//! queued onto the same [`InputEvent`](super::input_handler::InputEvent)
//! channel `LamcoInputHandler`'s batching task already drains — there is no
//! separate injection path or coordinate-transformer instance for touch.

use ironrdp_server::{
    RdpeiHandler, RdpeiServer, RdpeiServerFactory, ServerEvent, ServerEventSender, TouchEventPdu,
};
use tokio::sync::mpsc;
use tracing::debug;

use super::input_handler::InputEvent;

pub struct LamcoRdpeiFactory {
    input_tx: mpsc::Sender<InputEvent>,
    event_sender: Option<mpsc::UnboundedSender<ServerEvent>>,
}

impl LamcoRdpeiFactory {
    pub fn new(input_tx: mpsc::Sender<InputEvent>) -> Self {
        Self {
            input_tx,
            event_sender: None,
        }
    }
}

impl ServerEventSender for LamcoRdpeiFactory {
    fn set_sender(&mut self, sender: mpsc::UnboundedSender<ServerEvent>) {
        self.event_sender = Some(sender);
    }
}

impl RdpeiServerFactory for LamcoRdpeiFactory {
    fn build_server(&self) -> RdpeiServer {
        RdpeiServer::new(Box::new(LamcoRdpeiHandler {
            input_tx: self.input_tx.clone(),
        }))
    }
}

struct LamcoRdpeiHandler {
    input_tx: mpsc::Sender<InputEvent>,
}

impl RdpeiHandler for LamcoRdpeiHandler {
    fn touch(&mut self, pdu: TouchEventPdu) {
        if let Err(e) = self.input_tx.try_send(InputEvent::Touch(pdu)) {
            // Touch frames carry state transitions (DOWN/UP) the per-contact
            // state machine must see in order — unlike a mouse Move, dropping
            // one here would leave a contact stuck engaged. try_send only
            // fails when the queue is genuinely full or the receiver is gone
            // (connection tearing down), both already logged at that layer;
            // this one line is enough to know it happened for this channel.
            tracing::error!("Failed to queue touch frame for batching: {e}");
        }
    }

    // No pen injection path exists anywhere in this stack (eis_common.rs has
    // no pen support), so pen() and dismiss_hovering() keep the trait's
    // debug-log-only defaults rather than pretending to handle them.
}

impl std::fmt::Debug for LamcoRdpeiFactory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LamcoRdpeiFactory").finish_non_exhaustive()
    }
}

/// Build the RDPEI factory sharing the same input queue as mouse/keyboard.
pub(crate) fn create_rdpei_factory(input_tx: mpsc::Sender<InputEvent>) -> LamcoRdpeiFactory {
    debug!("RDPEI factory created");
    LamcoRdpeiFactory::new(input_tx)
}
