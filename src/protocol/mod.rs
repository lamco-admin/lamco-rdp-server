//! Protocol Utilities
//!
//! RDP protocol helper functions and types for message handling and encoding.
//!
//! # Overview
//!
//! This module provides utilities for working with the RDP protocol at a low level.
//! Most users interact with the protocol through higher-level APIs in the `server`
//! module, but this module exposes utilities for:
//!
//! - Binary encoding/decoding helpers
//! - Protocol constant definitions
//! - Message validation
//! - Protocol version negotiation utilities
//!
//! ## Protocol Support
//!
//! lamco-rdp-server implements RDP 10.x through the IronRDP library:
//!
//! - **RDP 5.0**: Basic bitmap remoting
//! - **RDP 6.0**: Bitmap compression
//! - **RDP 7.0**: RemoteFX codec
//! - **RDP 8.0**: H.264/AVC codec support
//! - **RDP 10.x**: EGFX Graphics Pipeline Extension (H.264 with advanced features)
//!
//! ## Extensions Used
//!
//! - **MS-RDPEGFX**: Graphics Pipeline Extension for H.264/AVC420/AVC444 streaming
//! - **MS-RDPECLIP**: Clipboard Virtual Channel Extension for bidirectional clipboard
//! - **MS-RDPEDISP**: Display Control Virtual Channel Extension for multi-monitor
//! - **MS-RDPEDYC**: Dynamic Virtual Channel for EGFX and clipboard file transfer
//!
//! ## Security
//!
//! All protocol communication is encrypted via TLS 1.3. The RDP protocol itself
//! provides additional layers:
//!
//! - **NLA (Network Level Authentication)**: Client authenticates before session starts
//! - **TLS wrapping**: All RDP PDUs are transmitted over TLS
//! - **CredSSP**: Credential Security Support Provider for NLA
//!
//! ## Integration with IronRDP
//!
//! This module wraps and extends [IronRDP](https://github.com/Devolutions/IronRDP),
//! a complete Rust implementation of the RDP protocol. We use a fork that includes:
//!
//! - MS-RDPEGFX Graphics Pipeline support (PR #1057 pending upstream)
//! - Clipboard file transfer methods (PRs #1063-1066 merged upstream)
//!
//! See `Cargo.toml` [patch.crates-io] section for fork details.
