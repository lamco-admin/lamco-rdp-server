# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.5.0] - 2025-12-30

### Added
- **DIBV5 format support** for transparent image clipboard operations
  - `CF_DIBV5` (format 17) - Windows BITMAPV5HEADER format constant
  - `png_to_dibv5()` - Convert PNG to DIBV5 with alpha channel preservation
  - `dibv5_to_png()` - Convert DIBV5 back to PNG
  - `jpeg_to_dibv5()` - Convert JPEG to DIBV5
  - `dibv5_to_jpeg()` - Convert DIBV5 to JPEG
  - `has_transparency()` - Detect if image has transparent pixels
  - Full BITMAPV5HEADER parsing with color mask support

### Changed
- `mime_to_rdp_formats()` now announces CF_DIBV5 for PNG sources (higher fidelity than CF_DIB)

## [0.4.0] - 2025-12-24

### Added
- **RTF format support** for Rich Text Format clipboard content
  - `validate_rtf()` - Validate RTF document structure
  - `is_rtf()` - Quick format detection
  - `text_to_rtf()` - Plain text to RTF conversion
  - `rtf_to_text()` - RTF to plain text extraction with proper group/destination handling
- **Synthesized format support** for legacy Windows compatibility
  - `CF_TEXT` (format 1) - ANSI text using Windows-1252 codepage
  - `CF_OEMTEXT` (format 7) - DOS text using CP437 codepage
  - `text_to_ansi()` / `ansi_to_text()` - Windows-1252 conversion
  - `text_to_oem()` / `oem_to_text()` - CP437 conversion
  - Full codepage lookup tables for special character handling

### Changed
- `mime_to_rdp_formats()` now announces CF_TEXT and CF_OEMTEXT alongside CF_UNICODETEXT

## [0.3.0] - 2025-12-23

### Added
- **FileGroupDescriptorW support** for RDP clipboard file transfer
  - `FileDescriptor` struct for parsing/building FILEDESCRIPTORW structures (592 bytes each)
  - `FileDescriptorFlags` for metadata field validation
  - `FileDescriptor::build()` to create descriptors from local files
  - `parse_list()` and `build_list()` for multiple file handling
  - `CF_FILEGROUPDESCRIPTORW` (49430) and `CF_FILECONTENTS` (49338) format constants
- **Cross-platform filename sanitization module** (`sanitize.rs`)
  - Windows reserved name handling (CON, PRN, COM1-9, LPT1-9, AUX, NUL)
  - Invalid character filtering/replacement (\/:*?"<>|)
  - Trailing dots/spaces cleanup (Windows compatibility)
  - Line ending conversion (LF ↔ CRLF)
  - Path component extraction and validation

### Changed
- Updated `mime_to_rdp_formats()` to advertise FileGroupDescriptorW for file URIs
- Updated `rdp_format_to_mime()` to handle FileGroupDescriptorW format

## [0.2.0] - 2025-12-21

### Added
- `image` feature for image format conversion (PNG, JPEG, GIF, BMP)

## [0.1.1] - 2025-12-17

### Fixed

- Fixed docs.rs build failure by replacing deprecated `doc_auto_cfg` with `doc_cfg`
  - The `doc_auto_cfg` feature was removed in Rust 1.92.0 and merged into `doc_cfg`
- Fixed code formatting issues in image module

## [0.1.0] - 2025-01-13

### Added

- Initial release
- **`ClipboardSink` trait** - Protocol-agnostic clipboard backend interface
  - 7 async methods: `announce_formats`, `read_clipboard`, `write_clipboard`, `subscribe_changes`, `get_file_list`, `read_file_chunk`, `write_file`
  - `FileInfo` struct for file transfer metadata
  - `ClipboardChange` notification struct
  - `ClipboardChangeReceiver` for change subscriptions
- **Format conversion** (`formats` module)
  - Windows clipboard format constants (CF_UNICODETEXT, CF_DIB, CF_HTML, etc.)
  - `ClipboardFormat` struct with ID and optional name
  - `mime_to_rdp_formats()` - Convert MIME types to RDP formats
  - `rdp_format_to_mime()` - Convert RDP format IDs to MIME types
  - `FormatConverter` for data conversion:
    - UTF-8 ↔ UTF-16LE (CF_UNICODETEXT)
    - HTML ↔ CF_HTML format
    - URI list ↔ HDROP format
- **Loop detection** (`loop_detector` module)
  - `LoopDetector` - Prevent clipboard sync loops
  - SHA256-based format and content hashing
  - Configurable time window (default: 500ms)
  - `ClipboardSource` enum (Rdp, Local)
- **Transfer engine** (`transfer` module)
  - `TransferEngine` - Chunked transfers for large data
  - Progress tracking with ETA calculation
  - SHA256 integrity verification
  - Configurable chunk size, max size, and timeout

[0.5.0]: https://github.com/lamco-admin/lamco-rdp/releases/tag/lamco-clipboard-core-v0.5.0
[0.4.0]: https://github.com/lamco-admin/lamco-rdp/releases/tag/lamco-clipboard-core-v0.4.0
[0.3.0]: https://github.com/lamco-admin/lamco-rdp/releases/tag/lamco-clipboard-core-v0.3.0
[0.2.0]: https://github.com/lamco-admin/lamco-rdp/releases/tag/lamco-clipboard-core-v0.2.0
[0.1.1]: https://github.com/lamco-admin/lamco-rdp/releases/tag/lamco-clipboard-core-v0.1.1
[0.1.0]: https://github.com/lamco-admin/lamco-rdp/releases/tag/lamco-clipboard-core-v0.1.0
