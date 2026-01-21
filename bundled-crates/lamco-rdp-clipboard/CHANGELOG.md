# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.2.2] - 2025-12-24

### Added
- **CB_FILECLIP_NO_FILE_PATHS capability flag** for privacy
  - Prevents source file paths from being leaked in clipboard data
  - Enhances security for enterprise environments

### Changed
- Updated lamco-clipboard-core dependency to v0.4.0

## [0.2.1] - 2025-12-23

### Changed
- Updated lamco-clipboard-core dependency to v0.3.0 (adds FileGroupDescriptorW support)

## [0.2.0] - 2025-12-21

### Changed
- Updated lamco-clipboard-core dependency to v0.2.0

## [0.1.1] - 2025-12-17

### Fixed

- Fixed docs.rs build failure by replacing deprecated `doc_auto_cfg` with `doc_cfg`
  - The `doc_auto_cfg` feature was removed in Rust 1.92.0 and merged into `doc_cfg`

## [0.1.0] - 2025-01-13

### Added

- Initial release
- **`RdpCliprdrBackend`** - IronRDP `CliprdrBackend` implementation
  - Non-blocking event-based design for async processing
  - Supports all CLIPRDR operations: format list, data request/response, file transfer
  - Capability negotiation (long format names, file streaming, data locking)
- **`RdpCliprdrFactory`** - Factory for creating backend instances
  - Shared event channel across multiple RDP connections
  - Configurable temporary directory for file transfers
- **`ClipboardEvent`** enum for async event processing
  - Ready, RequestFormatList, NegotiatedCapabilities
  - RemoteCopy, FormatDataRequest, FormatDataResponse
  - FileContentsRequest, FileContentsResponse
  - Lock, Unlock
- **`ClipboardEventSender`** / **`ClipboardEventReceiver`** - Thread-safe event channel
- Re-exports of `lamco-clipboard-core` types for convenience
- Re-exports of commonly used IronRDP types

[0.2.0]: https://github.com/lamco-admin/lamco-rdp/releases/tag/lamco-rdp-clipboard-v0.2.0
[0.1.1]: https://github.com/lamco-admin/lamco-rdp/releases/tag/lamco-rdp-clipboard-v0.1.1
[0.1.0]: https://github.com/lamco-admin/lamco-rdp/releases/tag/lamco-rdp-clipboard-v0.1.0
