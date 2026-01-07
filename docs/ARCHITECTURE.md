# Architecture

This document describes the high-level architecture of lamco-rdp-server.

## Overview

lamco-rdp-server is a Wayland RDP server that enables remote desktop access to Linux desktops. It uses the XDG Desktop Portal for secure, permission-controlled screen capture and input injection.

```
┌─────────────────────────────────────────────────────────────────┐
│                        RDP Clients                               │
│              (Windows mstsc, FreeRDP, Remmina)                   │
└─────────────────────────────┬───────────────────────────────────┘
                              │ RDP Protocol (TCP/3389)
                              ▼
┌─────────────────────────────────────────────────────────────────┐
│                     lamco-rdp-server                             │
│  ┌──────────────┐  ┌──────────────┐  ┌──────────────────────┐   │
│  │   IronRDP    │  │    EGFX      │  │     Clipboard        │   │
│  │   Server     │◄─┤   H.264      │  │     Manager          │   │
│  │   Core       │  │   Pipeline   │  │   (bidirectional)    │   │
│  └──────────────┘  └──────────────┘  └──────────────────────┘   │
│         ▲                 ▲                    ▲                 │
│         │                 │                    │                 │
│  ┌──────┴─────────────────┴────────────────────┴───────────┐    │
│  │                  Session Manager                         │    │
│  │          (Portal Strategy / Mutter Direct)               │    │
│  └──────────────────────────┬──────────────────────────────┘    │
└─────────────────────────────┼───────────────────────────────────┘
                              │ D-Bus
                              ▼
┌─────────────────────────────────────────────────────────────────┐
│                    XDG Desktop Portal                            │
│              (xdg-desktop-portal + backend)                      │
│  ┌──────────────┐  ┌──────────────┐  ┌──────────────────────┐   │
│  │  ScreenCast  │  │RemoteDesktop │  │    Clipboard         │   │
│  │   Portal     │  │   Portal     │  │    Portal            │   │
│  └──────────────┘  └──────────────┘  └──────────────────────┘   │
└─────────────────────────────┬───────────────────────────────────┘
                              │
                              ▼
┌─────────────────────────────────────────────────────────────────┐
│                   Wayland Compositor                             │
│              (GNOME Mutter / KDE KWin / wlroots)                 │
└─────────────────────────────────────────────────────────────────┘
```

## Core Components

### Session Layer (`src/session/`)

The session layer provides abstraction over different access methods:

- **Portal Strategy**: Uses XDG Portal for cross-compositor support
- **Mutter Direct**: Uses GNOME-specific D-Bus APIs (zero permission dialogs)

Key types:
- `SessionHandle`: Trait for session access (video, input, clipboard)
- `TokenManager`: Secure storage for portal restore tokens
- `CredentialStorage`: Secret Service / TPM / encrypted file backends

### Video Pipeline (`src/egfx/`)

H.264 video encoding via the EGFX (Enhanced Graphics) RDP extension:

```
PipeWire Frame → BGRA→YUV444 → H.264 Encode → EGFX PDU → RDP Client
     │              │              │
     ▼              ▼              ▼
  DMA-BUF      ColorMatrix      OpenH264
  capture      BT.601/709       AVC420/AVC444
```

Key features:
- **AVC420**: Standard H.264 (4:2:0 chroma)
- **AVC444**: Full-color H.264 (4:4:4 chroma via dual streams)
- **Damage tracking**: Only encode changed regions
- **Adaptive FPS**: 5-30 FPS based on content activity

### Input Handler (`src/server/input_handler.rs`)

Bridges RDP input events to Wayland via Portal:

- Keyboard: Scancode translation, XKB keymap
- Mouse: Coordinate transformation, multi-monitor support
- Touch: Not yet implemented

### Clipboard Manager (`src/clipboard/`)

Bidirectional clipboard sync:

```
Windows Clipboard ←→ RDP Cliprdr ←→ ClipboardManager ←→ Portal Clipboard
                                          │
                                          ▼
                                    Linux Apps
```

Supported formats:
- Text (CF_UNICODETEXT ↔ text/plain)
- Images (CF_DIB ↔ image/png)
- Files (FileGroupDescriptorW ↔ text/uri-list)

### Compositor Detection (`src/compositor/`)

Automatic detection and adaptation:

| Compositor | Detection | Notes |
|------------|-----------|-------|
| GNOME (Mutter) | XDG_CURRENT_DESKTOP | Supports Mutter Direct API |
| KDE (KWin) | XDG_CURRENT_DESKTOP | Full Portal support |
| Sway | SWAYSOCK | wlroots Portal |
| Hyprland | HYPRLAND_INSTANCE_SIGNATURE | wlroots Portal |

## Data Flow

### Video Capture

1. Portal provides PipeWire file descriptor
2. PipeWire stream delivers BGRA frames
3. Damage detection identifies changed tiles
4. Color conversion (BGRA → YUV)
5. H.264 encoding (OpenH264)
6. EGFX PDU packaging
7. RDP channel transmission

### Input Injection

1. RDP client sends input PDU
2. Server translates coordinates (multi-monitor aware)
3. Session handle injects via Portal/Mutter
4. Compositor processes input

## Security Model

- **Portal Permissions**: User grants access via system dialog
- **Restore Tokens**: Stored securely for reconnection
- **TLS**: All RDP traffic encrypted
- **PAM**: Optional password authentication

## Configuration

See `~/.config/lamco-rdp-server/config.toml`:

```toml
[server]
address = "0.0.0.0"
port = 3389

[video]
codec = "avc444"          # avc420 or avc444
h264_bitrate = 10000      # kbps
qp_min = 10
qp_max = 25

[performance]
adaptive_fps = true
min_fps = 5
max_fps = 30
```

## Module Reference

| Module | Purpose |
|--------|---------|
| `server` | Main server, connection handling |
| `session` | Session strategies, token management |
| `egfx` | H.264 encoding, EGFX protocol |
| `clipboard` | Clipboard synchronization |
| `compositor` | Compositor detection and capabilities |
| `damage` | Tile-based change detection |
| `performance` | Adaptive FPS, latency governor |
| `telemetry` | Diagnostics, metrics, error formatting |
