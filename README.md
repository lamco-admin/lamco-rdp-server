# lamco-rdp-server

**Wayland RDP Server - Portal-based remote desktop for Linux**

[![License](https://img.shields.io/badge/license-BUSL--1.1-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-1.77+-orange.svg)](https://www.rust-lang.org)

A high-performance RDP server for Linux desktops using XDG Desktop Portal for secure screen capture and input injection.

## Features

- **Portal-based capture** - Uses XDG ScreenCast and RemoteDesktop portals for secure, permission-controlled access
- **H.264 video streaming** - EGFX graphics pipeline with AVC420/AVC444 codec support
- **PipeWire integration** - Modern Linux audio/video capture via PipeWire
- **Clipboard sync** - Bidirectional clipboard with text, images, and file transfer
- **Multi-monitor support** - Virtual monitor resizing and positioning
- **Session persistence** - Restore tokens for passwordless reconnection

## Requirements

- Linux with Wayland compositor (GNOME, KDE, wlroots-based)
- XDG Desktop Portal with ScreenCast and RemoteDesktop support
- PipeWire for video capture
- Rust 1.77+

## Installation

```bash
# Clone the repository
git clone https://github.com/lamco-admin/lamco-rdp-server
cd lamco-rdp-server

# Build release binary
cargo build --release

# Install (optional)
sudo cp target/release/lamco-rdp-server /usr/local/bin/
```

## Usage

```bash
# Run with default config
lamco-rdp-server

# Run with custom config
lamco-rdp-server --config /path/to/config.toml

# Run diagnostics
lamco-rdp-server --diagnostics
```

## Configuration

Create a config file at `~/.config/lamco-rdp-server/config.toml`:

```toml
[server]
address = "0.0.0.0"
port = 3389

[video]
h264_bitrate = 10000
qp_min = 10
qp_max = 25

[egfx]
enabled = true
codec = "avc420"
```

## Architecture

```
lamco-rdp-server
  |-- Portal Session (permission handling)
  |-- PipeWire Manager (video capture)
  |-- Display Handler (H.264 encoding, EGFX streaming)
  |-- Input Handler (keyboard/mouse injection via libei)
  |-- Clipboard Manager (RDP clipboard protocol)
```

## Platform Support

| Desktop Environment | Status |
|---------------------|--------|
| GNOME 40+ | Fully supported |
| KDE Plasma 5.27+ | Supported |
| Sway / wlroots | Supported |
| Other Wayland | May work (requires Portal support) |

## Building from Source

### Dependencies

```bash
# Fedora/RHEL
sudo dnf install pipewire-devel pam-devel

# Debian/Ubuntu
sudo apt install libpipewire-0.3-dev libpam0g-dev

# Optional: H.264 encoding speedup
sudo dnf install nasm  # or: sudo apt install nasm
```

### Build Options

```bash
# Default build (PAM auth, H.264)
cargo build --release

# Without PAM authentication
cargo build --release --no-default-features --features h264

# With hardware encoding (VAAPI/NVENC)
cargo build --release --features vaapi
cargo build --release --features nvenc
```

## License

Business Source License 1.1 (BUSL-1.1)

See [LICENSE](LICENSE) for details.

## About

Developed by [Lamco](https://lamco.ai) for enterprise remote desktop solutions.

## Contributing

Contributions welcome. Please open an issue first to discuss significant changes.
