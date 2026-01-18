# lamco-rdp-clipboard

[![Crates.io](https://img.shields.io/crates/v/lamco-rdp-clipboard.svg)](https://crates.io/crates/lamco-rdp-clipboard)
[![Documentation](https://docs.rs/lamco-rdp-clipboard/badge.svg)](https://docs.rs/lamco-rdp-clipboard)
[![License](https://img.shields.io/crates/l/lamco-rdp-clipboard.svg)](LICENSE-MIT)

IronRDP clipboard integration for Rust.

This crate provides the IronRDP `CliprdrBackend` implementation for RDP clipboard synchronization. It bridges between IronRDP's CLIPRDR static virtual channel and the protocol-agnostic `ClipboardSink` trait from `lamco-clipboard-core`.

## Architecture

```text
┌─────────────────────────────────────────────────────────────────────┐
│                          lamco-rdp-clipboard                        │
│  ┌─────────────────────┐         ┌───────────────────────────────┐  │
│  │ RdpCliprdrBackend   │◄───────►│   ClipboardSink (trait)       │  │
│  │ (CliprdrBackend)    │         │   - Portal                    │  │
│  └─────────────────────┘         │   - X11                       │  │
│            ▲                     │   - Memory (testing)          │  │
│            │                     └───────────────────────────────┘  │
│            │                                                        │
│            ▼                                                        │
│  ┌─────────────────────┐                                           │
│  │ ironrdp-cliprdr     │                                           │
│  │ (CLIPRDR SVC)       │                                           │
│  └─────────────────────┘                                           │
└─────────────────────────────────────────────────────────────────────┘
```

## Installation

```toml
[dependencies]
lamco-rdp-clipboard = "0.1"
```

## Usage

```rust
use lamco_rdp_clipboard::{RdpCliprdrFactory, ClipboardEvent};
use ironrdp_cliprdr::backend::CliprdrBackendFactory;

// Create a factory with temporary directory for file transfers
let factory = RdpCliprdrFactory::new("/tmp/clipboard");

// Subscribe to clipboard events
let receiver = factory.subscribe();

// Build a backend for IronRDP
let backend = factory.build_cliprdr_backend();

// Process events asynchronously
loop {
    for event in receiver.drain() {
        match event {
            ClipboardEvent::Ready => {
                println!("Clipboard ready");
            }
            ClipboardEvent::RemoteCopy { formats } => {
                println!("Remote copy: {} formats", formats.len());
            }
            ClipboardEvent::FormatDataRequest { format_id } => {
                println!("Data requested: {:?}", format_id);
            }
            _ => {}
        }
    }
}
```

## Event Types

The `ClipboardEvent` enum represents all clipboard operations:

| Event | Description |
|-------|-------------|
| `Ready` | Backend initialized, channel ready |
| `RequestFormatList` | Request to send local format list |
| `NegotiatedCapabilities` | Capabilities negotiated with server |
| `RemoteCopy` | Remote clipboard content changed |
| `FormatDataRequest` | Remote requests specific format data |
| `FormatDataResponse` | Remote sent requested data |
| `FileContentsRequest` | Remote requests file chunk |
| `FileContentsResponse` | Remote sent file chunk |
| `Lock` / `Unlock` | Clipboard lock operations |

## Non-blocking Design

The `CliprdrBackend` trait methods are called synchronously from the RDP message processing loop. This implementation queues events for asynchronous processing to avoid blocking.

```rust
use lamco_rdp_clipboard::{RdpCliprdrBackend, ClipboardEventSender};

let event_sender = ClipboardEventSender::new();
let receiver = event_sender.subscribe();

// Backend queues events instead of blocking
let backend = RdpCliprdrBackend::new("/tmp/clipboard".to_string(), event_sender);

// Process events in your async runtime
tokio::spawn(async move {
    loop {
        for event in receiver.drain() {
            // Handle event asynchronously...
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
});
```

## Multiple Connections

The factory pattern supports multiple RDP connections sharing a single event stream:

```rust
use lamco_rdp_clipboard::RdpCliprdrFactory;
use ironrdp_cliprdr::backend::CliprdrBackendFactory;

let factory = RdpCliprdrFactory::new("/tmp/clipboard");
let receiver = factory.subscribe();

// Each connection gets its own backend
let backend1 = factory.build_cliprdr_backend();
let backend2 = factory.build_cliprdr_backend();

// All events go to the same receiver
for event in receiver.drain() {
    // Handle events from all connections...
}
```

## Related Crates

- **[lamco-clipboard-core](https://crates.io/crates/lamco-clipboard-core)** - Protocol-agnostic clipboard utilities
- **[ironrdp-cliprdr](https://crates.io/crates/ironrdp-cliprdr)** - IronRDP CLIPRDR channel implementation

## About Lamco

Lamco is a collection of high-quality, production-ready Rust crates for building Remote Desktop Protocol (RDP) applications. Built on top of [IronRDP](https://github.com/Devolutions/IronRDP), Lamco provides idiomatic Rust APIs with a focus on safety, performance, and ease of use.

## License

Licensed under either of:

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or https://www.apache.org/licenses/LICENSE-2.0)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or https://opensource.org/licenses/MIT)

at your option.

## Contributing

Contributions are welcome! Please see the [main repository](https://github.com/lamco-admin/lamco-rdp) for contribution guidelines.
