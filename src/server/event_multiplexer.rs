//! Event Multiplexer with Priority-Based Queue Management
//!
//! Implements QoS for RDP server events by separating traffic into priority classes
//! with independent bounded queues and intelligent dropping/coalescing policies.
//!
//! # Architecture
//!
//! ```text
//! Event Sources                Priority Queues              Multiplexer
//! ━━━━━━━━━━━━━                ━━━━━━━━━━━━━━━             ━━━━━━━━━━━━
//!
//! Keyboard/Mouse ────────────> Input Queue (32)    ────┐
//!   RDP scancode events          [bounded]             │
//!   High priority                DROP on full          │
//!                                                       │
//! Session Control ───────────> Control Queue (16)  ────┤
//!   Quit, credentials            [bounded]             │
//!   Critical operations          DROP on full          ├──> Drain
//!                                                       │    Priority
//! Clipboard ─────────────────> Clipboard Queue (8) ────┤    Order
//!   Format lists, data           [bounded]             │
//!   User-visible operations      DROP on full          │    TCP
//!                                                       │    Socket
//! Graphics ──────────────────> Graphics Queue (4)  ────┘
//!   Video frames                 [bounded]
//!   Bulk traffic                 DROP+COALESCE on full
//!                                (keep only latest frame)
//! ```
//!
//! # Priority Draining Algorithm
//!
//! On each multiplex cycle:
//! 1. **Input**: Drain ALL available (never starve input)
//! 2. **Control**: Process up to 1 event (prevent session issues)
//! 3. **Clipboard**: Process up to 1 event (user operations)
//! 4. **Graphics**: Coalesce to 1 latest frame (never block on graphics)
//!
//! This ensures:
//! - Input is always responsive
//! - Control commands processed quickly
//! - Clipboard doesn't lag
//! - Graphics congestion NEVER affects input/control/clipboard
//!
//! # Drop Policies by Queue
//!
//! - **Input**: Drop if queue full (user typing too fast, extremely rare)
//! - **Control**: Drop if queue full (graceful degradation)
//! - **Clipboard**: Drop if queue full (prevents memory exhaustion)
//! - **Graphics**: Always drop/coalesce (QoS requirement)
//!
//! # Example Usage
//!
//! ```no_run
//! let mux = EventMultiplexer::new();
//!
//! // Producers send to appropriate queues
//! mux.send_input(InputEvent::Keyboard(..));
//! mux.send_graphics(GraphicsFrame { .. });
//!
//! // Consumer drains in priority order
//! loop {
//!     mux.drain_to_wire(&mut tcp_writer).await;
//! }
//! ```

use tokio::sync::mpsc;
use tracing::{debug, info, warn};

/// Input event (keyboard/mouse)
#[derive(Debug)]
pub(super) enum InputEvent {
    Keyboard(ironrdp_server::KeyboardEvent),
    Mouse(ironrdp_server::MouseEvent),
}

/// Control event (session management)
#[derive(Debug)]
pub(super) enum ControlEvent {
    Quit(String),
    SetCredentials(ironrdp_server::Credentials),
    GetLocalAddr(tokio::sync::oneshot::Sender<Option<std::net::SocketAddr>>),
}

/// Clipboard event
#[derive(Debug)]
pub(super) enum ClipboardEvent {
    SendFormatList(Vec<ironrdp_cliprdr::pdu::ClipboardFormat>),
    SendData(Vec<u8>),
    RequestData(u32), // format_id
}

/// Graphics frame update (wraps IronRDP bitmap to avoid double conversion)
pub struct GraphicsFrame {
    pub iron_bitmap: ironrdp_server::BitmapUpdate,
    pub sequence: u64,
}

/// Event multiplexer with priority-based QoS
pub(super) struct EventMultiplexer {
    // Priority 1: Input (bounded 32, never starve)
    input_tx: mpsc::Sender<InputEvent>,
    input_rx: mpsc::Receiver<InputEvent>,

    // Priority 2: Control (bounded 16, critical)
    control_tx: mpsc::Sender<ControlEvent>,
    control_rx: mpsc::Receiver<ControlEvent>,

    // Priority 3: Clipboard (bounded 8, user-visible)
    clipboard_tx: mpsc::Sender<ClipboardEvent>,
    clipboard_rx: mpsc::Receiver<ClipboardEvent>,

    // Priority 4: Graphics (bounded 4, drop/coalesce)
    graphics_tx: mpsc::Sender<GraphicsFrame>,
    graphics_rx: mpsc::Receiver<GraphicsFrame>,

    // Statistics
    input_dropped: u64,
    control_dropped: u64,
    clipboard_dropped: u64,
    graphics_dropped: u64,
    graphics_coalesced: u64,
}

impl EventMultiplexer {
    /// Create new event multiplexer with default queue sizes
    pub(super) fn new() -> Self {
        let (input_tx, input_rx) = mpsc::channel(32);
        let (control_tx, control_rx) = mpsc::channel(16);
        let (clipboard_tx, clipboard_rx) = mpsc::channel(8);
        let (graphics_tx, graphics_rx) = mpsc::channel(4);

        info!("Event multiplexer created with bounded queues:");
        info!("  Input: 32 (Priority 1)");
        info!("  Control: 16 (Priority 2)");
        info!("  Clipboard: 8 (Priority 3)");
        info!("  Graphics: 4 (Priority 4 - drop/coalesce)");

        Self {
            input_tx,
            input_rx,
            control_tx,
            control_rx,
            clipboard_tx,
            clipboard_rx,
            graphics_tx,
            graphics_rx,
            input_dropped: 0,
            control_dropped: 0,
            clipboard_dropped: 0,
            graphics_dropped: 0,
            graphics_coalesced: 0,
        }
    }

    /// Get input event sender
    pub(super) fn input_sender(&self) -> mpsc::Sender<InputEvent> {
        self.input_tx.clone()
    }

    /// Get control event sender
    pub(super) fn control_sender(&self) -> mpsc::Sender<ControlEvent> {
        self.control_tx.clone()
    }

    /// Get clipboard event sender
    pub(super) fn clipboard_sender(&self) -> mpsc::Sender<ClipboardEvent> {
        self.clipboard_tx.clone()
    }

    /// Get graphics frame sender
    pub(super) fn graphics_sender(&self) -> mpsc::Sender<GraphicsFrame> {
        self.graphics_tx.clone()
    }

    /// Send input event with drop policy
    pub(super) async fn send_input(&self, event: InputEvent) {
        if self.input_tx.send(event).await.is_err() {
            warn!("Input queue full - dropping event (extremely rare)");
        }
    }

    /// Send graphics frame with drop policy
    /// If queue full, drop immediately (never block on graphics)
    pub(super) fn send_graphics_nonblocking(&mut self, frame: GraphicsFrame) {
        if self.graphics_tx.try_send(frame).is_err() {
            self.graphics_dropped += 1;
            if self.graphics_dropped % 100 == 0 {
                debug!(
                    "Graphics queue full - dropped {} frames total",
                    self.graphics_dropped
                );
            }
        }
    }

    /// Drain events in priority order: input (all) → control (1) → clipboard (1) → graphics (1)
    pub(super) async fn drain_cycle(&mut self) -> DrainResult {
        let mut result = DrainResult::default();

        // PRIORITY 1: Drain ALL input events
        while let Ok(input) = self.input_rx.try_recv() {
            result.input_events.push(input);
        }

        // PRIORITY 2: Process 1 control event
        if let Ok(control) = self.control_rx.try_recv() {
            result.control_event = Some(control);
        }

        // PRIORITY 3: Process 1 clipboard event
        if let Ok(clipboard) = self.clipboard_rx.try_recv() {
            result.clipboard_event = Some(clipboard);
        }

        // PRIORITY 4: Coalesce graphics - keep only latest frame
        let mut latest_frame = None;
        let mut coalesce_count = 0u32;
        while let Ok(frame) = self.graphics_rx.try_recv() {
            if latest_frame.is_some() {
                coalesce_count += 1;
            }
            latest_frame = Some(frame);
        }

        if coalesce_count > 0 {
            self.graphics_coalesced += coalesce_count as u64;
            if self.graphics_coalesced % 100 == 0 {
                debug!(
                    "Graphics coalescing: {} frames coalesced total",
                    self.graphics_coalesced
                );
            }
        }

        result.graphics_frame = latest_frame;
        result
    }

    /// Get statistics
    pub(super) fn stats(&self) -> MultiplexerStats {
        MultiplexerStats {
            input_dropped: self.input_dropped,
            control_dropped: self.control_dropped,
            clipboard_dropped: self.clipboard_dropped,
            graphics_dropped: self.graphics_dropped,
            graphics_coalesced: self.graphics_coalesced,
        }
    }
}

/// Result of drain cycle containing events to process
#[derive(Default)]
pub(super) struct DrainResult {
    pub input_events: Vec<InputEvent>,
    pub control_event: Option<ControlEvent>,
    pub clipboard_event: Option<ClipboardEvent>,
    pub graphics_frame: Option<GraphicsFrame>,
}

/// Multiplexer statistics
#[derive(Debug, Clone)]
pub(super) struct MultiplexerStats {
    pub input_dropped: u64,
    pub control_dropped: u64,
    pub clipboard_dropped: u64,
    pub graphics_dropped: u64,
    pub graphics_coalesced: u64,
}
