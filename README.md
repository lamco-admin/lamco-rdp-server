# lamco-rdp-server

### Wayland-native RDP server for Linux desktop sharing

Connect to your Linux desktop from any RDP client (Windows, macOS, Linux, iOS, Android). Built in Rust on [IronRDP](https://github.com/Devolutions/IronRDP) with native Wayland support via XDG Desktop Portal and PipeWire.

**[Product Page](https://www.lamco.ai/products/lamco-rdp-server/)** &nbsp;|&nbsp; **[Download](https://www.lamco.ai/download/)** &nbsp;|&nbsp; **[Open Source Crates](https://www.lamco.ai/open-source/)**

---

## Highlights

- **Wayland-first** -- XDG Desktop Portal for screen capture and input, no X11 required
- **H.264 via EGFX** -- AVC420 and AVC444 for crystal-clear text at full chroma resolution
- **Hardware encoding** -- VA-API (Intel/AMD) and NVENC (NVIDIA) support
- **Adaptive streaming** -- SIMD-optimized damage detection, 5-60 FPS based on activity
- **Clipboard sync** -- bidirectional text and image clipboard via Portal or Klipper
- **GUI configuration** -- graphical settings tool built with iced
- **355 tests passing** -- comprehensive test coverage across all modules

## Downloads

Pre-built packages are available from [GitHub Releases](https://github.com/lamco-admin/lamco-rdp-server/releases) and [lamco.ai/download](https://www.lamco.ai/download/).

| Format | Distro | Install |
|--------|--------|---------|
| **Snap** | Any Linux | `sudo snap install lamco-rdp-server` |
| **AUR** | Arch Linux | `yay -S lamco-rdp-server` |
| **Flatpak** | Any Linux | `flatpak install --user lamco-rdp-server-*.flatpak` |
| **RPM** | Fedora 42+ | `sudo dnf install ./lamco-rdp-server-*.fc42.x86_64.rpm` |
| **RPM** | openSUSE Tumbleweed | `sudo zypper install ./lamco-rdp-server-*.suse-tw.x86_64.rpm` |
| **RPM** | RHEL 9 / AlmaLinux 9 | `sudo dnf install ./lamco-rdp-server-*.el9.x86_64.rpm` |
| **DEB** | Debian 13 (Trixie) | `sudo dpkg -i lamco-rdp-server_*_amd64.deb` |
| **Source** | Any (Rust 1.85+) | `cargo build --release --offline` |

The source tarball on the Releases page includes vendored dependencies for offline builds.

## Platform Support

| Desktop Environment | Video | Input | Clipboard | Deployment |
|---------------------|:-----:|:-----:|:---------:|------------|
| **GNOME 45+** (Ubuntu 24.04, Fedora 42) | AVC444 | Portal+EIS | Portal | Flatpak or native |
| **GNOME 40-44** (RHEL 9, AlmaLinux 9) | AVC444 | Portal+EIS | -- | Flatpak or native |
| **KDE Plasma 6.3+** (openSUSE TW, Debian 13) | AVC444 | Portal+EIS | Klipper | Flatpak or native |
| **Sway / River** (wlroots) | AVC444 | wlr-direct | wl-clipboard | Native only |
| **Hyprland** (official portal) | AVC444 | wlr-direct | wl-clipboard | Native only |
| **Hyprland** (hypr-remote community portal) | AVC444 | Portal | Portal | Flatpak or native |

**Notes:**
- GNOME 40-44 (RHEL 9) lacks Portal clipboard because RemoteDesktop v1 predates the clipboard API.
- KDE Portal clipboard has a known bug ([KDE#515465](https://bugs.kde.org/show_bug.cgi?id=515465)) on Plasma 6.3.90-6.5.5. Klipper D-Bus cooperation works on all KDE versions as a fallback.
- wlroots compositors need native install for input and clipboard; Flatpak provides video-only on these desktops.
- COSMIC and Niri support is blocked on upstream [Smithay libei](https://github.com/Smithay/smithay/pull/1388).

For the full compatibility matrix with portal versions, session persistence, and deployment recommendations, see the [product page](https://www.lamco.ai/products/lamco-rdp-server/).

## Quick Start

```bash
# Generate TLS certificates
./scripts/generate-certs.sh

# Start the server
lamco-rdp-server -c config.toml -vv

# Or use the GUI
lamco-rdp-server-gui
```

Then connect from any RDP client (Windows Remote Desktop, FreeRDP, Remmina, etc.) to port 3389.

## Building from Source

**Requirements:** Rust 1.85+, OpenSSL dev, PipeWire dev, `nasm` (optional, 3x faster OpenH264)

```bash
cargo build --release                                    # software H.264
cargo build --release --features gui                     # with configuration GUI
cargo build --release --features "gui,vaapi"             # with VA-API hardware encoding
cargo build --release --features "gui,wayland,libei"     # full-featured for wlroots
```

| Feature flag | What it enables |
|-------------|-----------------|
| `gui` | Graphical configuration tool (iced) |
| `vaapi` | VA-API hardware encoding (Intel/AMD) |
| `nvenc` | NVENC hardware encoding (NVIDIA) |
| `wayland` | Native wlroots protocol support (wlr-direct) |
| `libei` | Portal + EIS input for Flatpak on wlroots |
| `pam-auth` | PAM authentication (native only, not in Flatpak) |

## Architecture

```
lamco-rdp-server/
  src/
    server/         RDP listener, TLS, session management
    rdp/            Channel multiplexing (EGFX, clipboard, audio, input)
    egfx/           H.264 encoding pipeline (OpenH264, VA-API, NVENC)
    clipboard/      Clipboard orchestration (Portal, Klipper, wl-clipboard)
    damage/         SIMD tile-based frame differencing
    session/        XDG Desktop Portal strategies and persistence
    gui/            Configuration GUI (iced)
  bundled-crates/
    lamco-clipboard-core/     Clipboard protocol core
    lamco-rdp-clipboard/      IronRDP clipboard backend
  packaging/        Flatpak manifest, systemd units, polkit, D-Bus config
```

## Open Source Foundation

lamco-rdp-server is built on a set of published Rust crates available on [crates.io](https://crates.io/search?q=lamco):

| Crate | Purpose |
|-------|---------|
| [lamco-portal](https://crates.io/crates/lamco-portal) | XDG Desktop Portal integration |
| [lamco-pipewire](https://crates.io/crates/lamco-pipewire) | PipeWire screen capture with DMA-BUF |
| [lamco-video](https://crates.io/crates/lamco-video) | Video frame processing |
| [lamco-rdp](https://crates.io/crates/lamco-rdp) | Core RDP protocol types |
| [lamco-rdp-input](https://crates.io/crates/lamco-rdp-input) | Input event translation (200+ key mappings) |
| [lamco-wayland](https://crates.io/crates/lamco-wayland) | Wayland protocol bindings |

These crates are MIT/Apache-2.0 licensed. See [lamco.ai/open-source](https://www.lamco.ai/open-source/) for documentation and details.

The server also depends on a [fork of IronRDP](https://github.com/lamco-admin/IronRDP) that adds MS-RDPEGFX Graphics Pipeline Extension and clipboard file transfer support. Contributions to upstream IronRDP are in progress.

## Troubleshooting

**First connection fails, second succeeds** -- Normal TLS behavior. The RDP client rejects the self-signed certificate on first attempt, then retries after accepting it. The acceptance is cached for subsequent connections.

**Clipboard not working (Flatpak)** -- Portal clipboard requires RemoteDesktop v2 (GNOME 45+, KDE Plasma 6.3+). On RHEL 9 and other older distributions with RemoteDesktop v1, clipboard is unavailable in Portal mode.

**Permission dialog on every start** -- GNOME deliberately does not persist RemoteDesktop sessions. This is a compositor policy decision, not a bug. KDE Plasma supports session tokens.

**"Unknown (not in Wayland session?)"** -- Cosmetic. Flatpak sandboxes hide `XDG_CURRENT_DESKTOP`. The server queries D-Bus directly for portal capabilities regardless.

## License

[Business Source License 1.1 (BSL)](LICENSE)

- **Free** for non-profits and small businesses (<3 employees, <$1M revenue)
- **Commercial license:** $49.99/year or $99.00 perpetual per server
- **Converts** to Apache License 2.0 on 2028-12-31

See [lamco.ai/products/lamco-rdp-server](https://www.lamco.ai/products/lamco-rdp-server/) for pricing details.

## Contributing

Contributions welcome. Please open an issue before starting significant work.
