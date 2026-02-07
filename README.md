# lamco-rdp-server

**Professional RDP Server for Wayland/Linux Desktop Sharing**

Production-ready RDP server that provides secure remote desktop access to Linux systems running Wayland, using the XDG Desktop Portal for screen capture and input injection.

## Overview

`lamco-rdp-server` is a modern, production-tested remote desktop server for Wayland-based Linux desktops. It implements the Remote Desktop Protocol (RDP) with native Wayland support via XDG Desktop Portal and PipeWire, enabling secure remote access without X11 dependencies.

Built in Rust with a focus on security, performance, and compatibility with modern Linux desktop environments (GNOME, KDE Plasma, etc.).

## Repository Purpose

This is the **development repository** containing clean source code only. For releases, packaging, and distribution, see the [lamco-admin](https://github.com/lamco-admin/lamco-admin) pipeline.

**What's here:**
- Source code (`src/`)
- Bundled crates (`bundled-crates/`)
- Tests (`tests/`)
- Benchmarks (`benches/`)
- Cargo configuration

**What's NOT here:**
- Flatpak manifests (see lamco-admin/projects/lamco-rdp-server/packaging/)
- OBS/RPM specs (see lamco-admin)
- Vendor tarballs (generated during build)
- Release artifacts

## Features

### Core Features
- **RDP Protocol Support**: Full RDP 10.x server implementation via IronRDP
- **Wayland Native**: Portal mode using XDG Desktop Portal (no X11 required)
- **PipeWire Screen Capture**: Zero-copy DMA-BUF support for efficient streaming
- **H.264 Video Encoding**: EGFX channel with AVC420/AVC444 codec support
- **Secure Authentication**: TLS 1.3 and Network Level Authentication (NLA)
- **Input Handling**: Full keyboard and mouse support with 200+ key mappings
- **Clipboard Sharing**: Bidirectional clipboard sync (text and images)
- **Multi-Monitor**: Layout negotiation and display management
- **Damage Detection**: SIMD-optimized tile-based frame differencing

### Optional Features
- **GUI Configuration**: `--features gui` adds graphical configuration interface
- **Hardware Encoding (VA-API)**: Intel/AMD GPU acceleration (`--features vaapi`)
- **Hardware Encoding (NVENC)**: NVIDIA GPU acceleration (`--features nvenc`)
- **wlr-direct**: Native wlroots protocols (`--features wayland`)
- **libei/EIS**: Portal + EIS for Flatpak wlroots support (`--features libei`)

## Building

### Prerequisites

- Rust 1.77 or later
- OpenSSL development libraries
- PipeWire development libraries
- For H.264: `nasm` (3x speedup for OpenH264)

### Development Build

```bash
# Default build (software H.264 encoding)
cargo build

# With GUI
cargo build --features gui

# Release build
cargo build --release
```

### Feature Combinations

```bash
# Native wlroots compositor support
cargo build --features "wayland"

# Flatpak-compatible build
cargo build --no-default-features --features "h264,libei"

# Full-featured native build
cargo build --features "gui,wayland,libei,vaapi"
```

## Running

### Prerequisites for Running

1. **TLS Certificates** in `certs/` directory (or generate with `./scripts/generate-certs.sh`)
2. **D-Bus Session**: `export DBUS_SESSION_BUS_ADDRESS="unix:path=/run/user/$(id -u)/bus"`
3. **PipeWire** running for screen capture

### Development Run

```bash
# Run with local configuration
cargo run -- -c config.toml.example -vv

# Run GUI
cargo run --features gui --bin lamco-rdp-server-gui
```

## Testing

```bash
# Run all tests
cargo test

# Run with output
cargo test -- --nocapture

# Run benchmarks
cargo bench
```

## Project Structure

```
lamco-rdp-server-dev/
├── src/
│   ├── lib.rs          # Library root
│   ├── main.rs         # CLI entry point
│   ├── gui/            # GUI application (optional)
│   ├── server/         # Main server implementation
│   ├── rdp/            # RDP channel management
│   ├── egfx/           # EGFX video pipeline
│   ├── clipboard/      # Clipboard orchestration
│   ├── damage/         # Damage region detection
│   ├── session/        # Session persistence
│   └── ...
├── bundled-crates/     # Locally bundled dependencies
│   ├── lamco-clipboard-core/
│   └── lamco-rdp-clipboard/
├── tests/              # Integration tests
├── benches/            # Performance benchmarks
├── Cargo.toml          # Dependencies and features
└── config.toml.example # Example configuration
```

## Dependencies

### Published Lamco Crates (crates.io)
- `lamco-wayland` - Wayland protocol bindings
- `lamco-rdp` - Core RDP utilities
- `lamco-portal` - XDG Desktop Portal integration
- `lamco-pipewire` - PipeWire screen capture
- `lamco-video` - Video frame processing
- `lamco-rdp-input` - Input event translation

### Bundled Crates
- `lamco-clipboard-core` - Clipboard protocol core
- `lamco-rdp-clipboard` - IronRDP clipboard backend

These are bundled because they implement traits from our IronRDP fork.

### Forked Dependencies
**IronRDP Fork:** `https://github.com/lamco-admin/IronRDP`
- Includes MS-RDPEGFX Graphics Pipeline Extension
- Clipboard file transfer methods

## Release Process

Releases are managed through the lamco-admin pipeline:

1. **Development**: Work in this repo
2. **Build**: `lamco-admin/pipelines/lamco-rdp-server/build/` creates vendor tarballs, Flatpak bundles
3. **Test**: `lamco-admin/pipelines/lamco-rdp-server/test/` deploys to VMs for verification
4. **Publish**: `lamco-admin/pipelines/lamco-rdp-server/publish/` creates GitHub releases, triggers OBS

See lamco-admin for detailed pipeline documentation.

## Troubleshooting

### First Connection Fails, Second Succeeds

**Symptom**: The first RDP connection attempt fails with "connection reset by peer" or error 0x904, but the second connection succeeds.

**Cause**: This is expected TLS certificate behavior, not a bug. The RDP client initially rejects the server's self-signed certificate as untrusted. The client then reconnects accepting the certificate, and the second connection succeeds.

**Resolution**: This is normal operation. The certificate acceptance is cached by the client for future connections.

### Clipboard Not Working (Flatpak)

**Symptom**: Clipboard sync doesn't work when running as Flatpak.

**Cause**: Portal clipboard support requires Portal RemoteDesktop v2 or higher. Older Portal versions (v1, found on RHEL 9 and some older distributions) don't expose the clipboard API.

**Resolution**:
- Upgrade to a distribution with Portal v2+ (GNOME 44+, KDE Plasma 5.27+)
- For RHEL 9, clipboard is not available in Portal mode

### "Unknown (not in Wayland session?)" in Diagnostics

**Symptom**: Server logs show "Compositor: Unknown (not in Wayland session?)"

**Cause**: The `XDG_CURRENT_DESKTOP` environment variable is not set, which can happen inside Flatpak sandboxes.

**Resolution**: This is usually cosmetic. The server now queries D-Bus directly for Portal version to determine clipboard support, bypassing environment variable detection.

### Permission Dialog Appears Every Time

**Symptom**: The screen sharing permission dialog appears on every server start.

**Cause**: Some Portal backends (notably GNOME's) don't support session persistence for RemoteDesktop sessions. This is a deliberate policy, not a bug.

**Resolution**: This is expected behavior on GNOME. The server automatically detects this and continues without persistence.

## License

`lamco-rdp-server` is licensed under the **Business Source License 1.1 (BSL)**.

- Free for non-profits and small businesses (<3 employees, <$1M revenue)
- Commercial license: $49.99/year or $99.00 perpetual per server
- Converts to Apache License 2.0 on 2028-12-31

See [LICENSE](LICENSE) for complete terms.

## Contributing

Contributions are welcome! Please open an issue before starting significant work.
