//! # lamco-rdp-clipboard
//!
//! IronRDP clipboard integration for Rust.
//!
//! This crate provides the IronRDP [`CliprdrBackend`](ironrdp_cliprdr::backend::CliprdrBackend)
//! implementation for RDP clipboard synchronization. It bridges between IronRDP's clipboard
//! channel and the protocol-agnostic [`ClipboardSink`] trait.
//!
//! ## Architecture
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────────────┐
//! │                          lamco-rdp-clipboard                        │
//! │  ┌─────────────────────┐         ┌───────────────────────────────┐  │
//! │  │ RdpCliprdrBackend   │◄───────►│   ClipboardSink (trait)       │  │
//! │  │ (CliprdrBackend)    │         │   - Portal                    │  │
//! │  └─────────────────────┘         │   - X11                       │  │
//! │            ▲                     │   - Memory (testing)          │  │
//! │            │                     └───────────────────────────────┘  │
//! │            │                                                        │
//! │            ▼                                                        │
//! │  ┌─────────────────────┐                                           │
//! │  │ ironrdp-cliprdr     │                                           │
//! │  │ (CLIPRDR SVC)       │                                           │
//! │  └─────────────────────┘                                           │
//! └─────────────────────────────────────────────────────────────────────┘
//! ```
//!
//! ## Usage
//!
//! ```rust,ignore
//! use lamco_rdp_clipboard::{RdpCliprdrBackend, RdpCliprdrFactory};
//! use lamco_clipboard_core::ClipboardSink;
//!
//! // Create a factory with your clipboard sink implementation
//! let factory = RdpCliprdrFactory::new(my_clipboard_sink);
//!
//! // Use with IronRDP
//! let backend = factory.build_cliprdr_backend();
//! ```
//!
//! ## Non-blocking Design
//!
//! The [`CliprdrBackend`](ironrdp_cliprdr::backend::CliprdrBackend) trait methods are called
//! synchronously from the RDP message processing loop. To avoid blocking, this implementation
//! queues events for asynchronous processing and provides a separate event processing loop.

#![cfg_attr(docsrs, feature(doc_cfg))]
#![deny(missing_docs)]

mod backend;
mod error;
mod event;
mod factory;

pub use backend::RdpCliprdrBackend;
pub use error::{ClipboardRdpError, ClipboardRdpResult};
pub use event::{ClipboardEvent, ClipboardEventReceiver, ClipboardEventSender};
pub use factory::RdpCliprdrFactory;

// Re-export core types for convenience
pub use lamco_clipboard_core;
pub use lamco_clipboard_core::{ClipboardFormat, ClipboardSink, FormatConverter, LoopDetector};

// Re-export IronRDP types commonly needed
pub use ironrdp_cliprdr::backend::{ClipboardMessage, ClipboardMessageProxy};
pub use ironrdp_cliprdr::pdu::{
    ClipboardGeneralCapabilityFlags, FileContentsRequest, FileContentsResponse, FormatDataRequest, FormatDataResponse,
};
