# lamco-clipboard-core

[![Crates.io](https://img.shields.io/crates/v/lamco-clipboard-core.svg)](https://crates.io/crates/lamco-clipboard-core)
[![Documentation](https://docs.rs/lamco-clipboard-core/badge.svg)](https://docs.rs/lamco-clipboard-core)
[![License](https://img.shields.io/crates/l/lamco-clipboard-core.svg)](LICENSE-MIT)

Protocol-agnostic clipboard utilities for Rust.

This crate provides core clipboard functionality that can be used with any clipboard backend (Portal, X11, headless, etc.):

- **`ClipboardSink` trait** - Abstract clipboard backend interface with 7 async methods
- **`FormatConverter`** - MIME ↔ Windows clipboard format conversion
- **`LoopDetector`** - Prevent clipboard sync loops with SHA256 content hashing
- **`TransferEngine`** - Chunked transfer for large clipboard data with progress tracking

## Installation

```toml
[dependencies]
lamco-clipboard-core = "0.1"
```

## Feature Flags

```toml
[dependencies]
# Default - text conversion, loop detection, transfer engine
lamco-clipboard-core = "0.1"

# With image format conversion (PNG/JPEG/BMP ↔ DIB)
lamco-clipboard-core = { version = "0.1", features = ["image"] }
```

| Feature | Description |
|---------|-------------|
| `image` | Image format conversion - PNG, JPEG, BMP, GIF to/from Windows DIB format. Required for clipboard image sync. |

## Quick Start

```rust
use lamco_clipboard_core::{FormatConverter, LoopDetector};
use lamco_clipboard_core::formats::{mime_to_rdp_formats, rdp_format_to_mime, CF_UNICODETEXT};

// Convert MIME types to RDP clipboard formats
let formats = mime_to_rdp_formats(&["text/plain", "text/html"]);
println!("RDP formats: {:?}", formats);

// Convert RDP format back to MIME
let mime = rdp_format_to_mime(CF_UNICODETEXT);
assert_eq!(mime, Some("text/plain;charset=utf-8"));

// Prevent clipboard sync loops
let mut detector = LoopDetector::new();
if !detector.would_cause_loop(&formats) {
    // Safe to sync clipboard content
}
```

## Format Conversion

Convert between UTF-8 text and Windows Unicode format:

```rust
use lamco_clipboard_core::FormatConverter;

let converter = FormatConverter::new();

// UTF-8 → UTF-16LE (for CF_UNICODETEXT)
let unicode = converter.text_to_unicode("Hello, World!").unwrap();

// UTF-16LE → UTF-8
let text = converter.unicode_to_text(&unicode).unwrap();
assert_eq!(text, "Hello, World!");
```

Convert HTML to Windows CF_HTML format:

```rust
use lamco_clipboard_core::FormatConverter;

let converter = FormatConverter::new();
let html = "<b>Bold text</b>";

let cf_html = converter.html_to_cf_html(html).unwrap();
let recovered = converter.cf_html_to_html(&cf_html).unwrap();
assert_eq!(recovered, html);
```

## Loop Detection

Prevent infinite clipboard sync loops between local and remote clipboards:

```rust
use lamco_clipboard_core::{LoopDetector, ClipboardFormat, ClipboardSource};

let mut detector = LoopDetector::new();

// Record an operation from RDP
let formats = vec![ClipboardFormat::unicode_text()];
detector.record_formats(&formats, ClipboardSource::Rdp);

// Check if syncing back would cause a loop
if detector.would_cause_loop(&formats) {
    println!("Loop detected - skipping sync");
}

// Content-based deduplication
let data = b"Clipboard content";
detector.record_content(data, ClipboardSource::Rdp);

if detector.would_cause_content_loop(data, ClipboardSource::Local) {
    println!("Same content already synced");
}
```

## Chunked Transfers

Handle large clipboard data with progress tracking:

```rust
use lamco_clipboard_core::TransferEngine;

let mut engine = TransferEngine::new();

// Prepare data for chunked sending
let data = vec![0u8; 1024 * 1024]; // 1MB
let chunks = engine.prepare_send(&data).unwrap();

for (i, chunk) in chunks.iter().enumerate() {
    println!("Chunk {}/{}: {} bytes", i + 1, chunks.len(), chunk.len());
    // Send chunk over network/RDP...
}

// Get hash for integrity verification
let hash = engine.compute_hash(&data);
```

Receive chunked data:

```rust
use lamco_clipboard_core::TransferEngine;

let mut engine = TransferEngine::new();

// Start receiving
engine.start_receive(1000, Some("expected_hash".to_string())).unwrap();

// Receive chunks
engine.receive_chunk(vec![0u8; 500]).unwrap();
engine.receive_chunk(vec![0u8; 500]).unwrap();

// Check progress
if let Some(progress) = engine.progress() {
    println!("Progress: {:.1}%", progress.percentage());
}

// Finalize and verify integrity
let data = engine.finalize_receive().unwrap();
```

## ClipboardSink Trait

Implement this trait to create a clipboard backend:

```rust
use lamco_clipboard_core::{ClipboardSink, ClipboardResult, ClipboardChangeReceiver, FileInfo};

struct MyClipboard { /* ... */ }

impl ClipboardSink for MyClipboard {
    async fn announce_formats(&self, mime_types: Vec<String>) -> ClipboardResult<()> {
        // Announce available formats to clipboard peers
        Ok(())
    }

    async fn read_clipboard(&self, mime_type: &str) -> ClipboardResult<Vec<u8>> {
        // Read clipboard data for the given MIME type
        Ok(vec![])
    }

    async fn write_clipboard(&self, mime_type: &str, data: Vec<u8>) -> ClipboardResult<()> {
        // Write data to clipboard
        Ok(())
    }

    async fn subscribe_changes(&self) -> ClipboardResult<ClipboardChangeReceiver> {
        // Return a receiver for clipboard change notifications
        todo!()
    }

    async fn get_file_list(&self) -> ClipboardResult<Vec<FileInfo>> {
        // Get list of files in clipboard (for file transfer)
        Ok(vec![])
    }

    async fn read_file_chunk(&self, index: u32, offset: u64, size: u32) -> ClipboardResult<Vec<u8>> {
        // Read a chunk of a file (for MS-RDPECLIP FileContents)
        Ok(vec![])
    }

    async fn write_file(&self, path: &str, data: Vec<u8>) -> ClipboardResult<()> {
        // Write a received file to destination
        Ok(())
    }
}
```

## Image Conversion (requires `image` feature)

Convert between Windows DIB format and standard image formats:

```rust
use lamco_clipboard_core::image::{png_to_dib, dib_to_png, dib_dimensions};

// PNG → DIB (for sending to RDP client)
let png_data = std::fs::read("image.png").unwrap();
let dib_data = png_to_dib(&png_data).unwrap();

// DIB → PNG (for receiving from RDP client)
let png_result = dib_to_png(&dib_data).unwrap();

// Get dimensions without full decode
let (width, height) = dib_dimensions(&dib_data).unwrap();
println!("Image: {}x{}", width, height);
```

Supported formats: PNG, JPEG, BMP, GIF (read-only).

## Supported Formats

| Windows Format | Format ID | MIME Type |
|---------------|-----------|-----------|
| CF_UNICODETEXT | 13 | text/plain;charset=utf-8 |
| CF_TEXT | 1 | text/plain |
| CF_DIB | 8 | image/png |
| CF_HDROP | 15 | text/uri-list |
| HTML Format | 0xD010 | text/html |
| PNG | 0xD011 | image/png |
| JFIF | 0xD012 | image/jpeg |
| GIF | 0xD013 | image/gif |
| Rich Text Format | 0xD014 | text/rtf |

## About Lamco

Lamco is a collection of high-quality, production-ready Rust crates for building Remote Desktop Protocol (RDP) applications. Built on top of [IronRDP](https://github.com/Devolutions/IronRDP), Lamco provides idiomatic Rust APIs with a focus on safety, performance, and ease of use.

## License

Licensed under either of:

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or https://www.apache.org/licenses/LICENSE-2.0)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or https://opensource.org/licenses/MIT)

at your option.

## Contributing

Contributions are welcome! Please see the [main repository](https://github.com/lamco-admin/lamco-rdp) for contribution guidelines.
