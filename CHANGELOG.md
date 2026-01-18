# Changelog

All notable changes to lamco-rdp-server will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.9.0] - 2026-01-18

### Added

**Multi-Strategy Session Persistence**
- Mutter Direct API strategy (GNOME, zero dialogs)
- wlr-direct strategy (wlroots native protocols, zero dialogs)
- Portal + libei/EIS strategy (Flatpak-compatible wlroots)
- Portal + Restore Tokens strategy (universal, Portal v4+)
- Basic Portal fallback strategy
- Automatic strategy selection based on compositor detection
- Encrypted credential storage (Secret Service, TPM 2.0, encrypted file)

**Service Advertisement Registry**
- 18 advertised services with 4-level guarantees (Guaranteed, BestEffort, Degraded, Unavailable)
- Runtime feature detection and translation (Wayland capabilities → RDP features)
- Compositor-specific profiles (GNOME, KDE, wlroots, COSMIC)
- Service-based decision making for codec selection, FPS tuning, cursor mode

**Video & Graphics**
- H.264 video encoding via EGFX graphics pipeline
- AVC420 (4:2:0 chroma) and AVC444 (4:4:4 chroma) codec support
- Hardware-accelerated encoding (VA-API for Intel/AMD, NVENC for NVIDIA)
- Damage region detection with SIMD optimization (90%+ bandwidth savings)
- Adaptive FPS (5-60 FPS based on screen activity)
- Latency governor (interactive/balanced/quality modes)
- Periodic IDR keyframe generation for artifact clearing

**Input & Interaction**
- Complete keyboard and mouse support (200+ key mappings)
- Multi-monitor coordinate transformation
- Predictive cursor with physics-based latency compensation
- Touch input support (experimental)

**Clipboard**
- Bidirectional clipboard synchronization
- Text, image, and file transfer support
- Loop detection and prevention
- FUSE-based on-demand file transfer
- Format conversion (15+ clipboard formats)
- Rate limiting for Portal compatibility

**Deployment & Compatibility**
- Flatpak packaging with sandbox permissions
- systemd user and system service files
- RPM and DEB package specifications
- Automatic compositor detection (GNOME, KDE, wlroots, COSMIC)
- Portal capability probing
- Deployment context detection (Flatpak, systemd, native)

**Configuration & Management**
- Comprehensive TOML configuration with all options
- Environment variable support
- CLI diagnostic commands (--show-capabilities, --persistence-status, --diagnose)
- User-friendly error messages with troubleshooting hints
- TLS 1.3 with automatic or manual certificate management

### Tested Platforms

- ✅ Ubuntu 24.04 LTS (GNOME 46, Portal v5) - Full RDP functionality
- ✅ RHEL 9.7 (GNOME 40, Portal v4) - Video and input working (no clipboard)
- ⚠️ Pop!_OS 24.04 COSMIC - Limited support (Portal RemoteDesktop not implemented)

See `docs/DISTRO-TESTING-MATRIX.md` for complete compatibility matrix.

### Known Issues

**GNOME Session Persistence**
- GNOME portal backend rejects persistence for RemoteDesktop sessions (policy decision)
- Mutter Direct API strategy bypasses this limitation on GNOME 42+
- Impact: Permission dialog required on each server restart with Portal strategy

**COSMIC Desktop**
- Portal RemoteDesktop interface not implemented in COSMIC
- Waiting on Smithay PR #1388 (Ei protocol support)
- Impact: No input injection available on COSMIC in Flatpak

**Portal Clipboard (Ubuntu 24.04)**
- xdg-desktop-portal-gnome may crash on complex Excel paste operations
- Impact: Session becomes unusable after crash
- Workaround: Avoid pasting Excel with 15+ formats

### Dependencies

**Published Lamco Crates:**
- lamco-wayland 0.2.3
- lamco-rdp 0.5.0
- lamco-portal 0.3.0
- lamco-pipewire 0.1.4
- lamco-video 0.1.2
- lamco-rdp-input 0.1.1

**Bundled Crates:**
- lamco-clipboard-core 0.5.0 (local path dependency)
- lamco-rdp-clipboard 0.2.2 (local path dependency)

**Forked Dependencies:**
- IronRDP fork (github.com/lamco-admin/IronRDP)
  - Includes: MS-RDPEGFX support (PR #1057 pending upstream)
  - Clipboard file transfer methods (PRs #1063-1066 merged upstream)

### License

Business Source License 1.1 (BUSL-1.1)
- Free for non-profits and small businesses (<3 employees, <$1M revenue)
- Commercial license required for larger organizations ($49.99/year or $99 perpetual)
- Automatically converts to Apache License 2.0 on December 31, 2028

### Build Requirements

- Rust 1.77+
- PipeWire 0.3.77+
- XDG Desktop Portal
- OpenSSL (for TLS)
- Optional: libva 1.20+ (VA-API hardware encoding)
- Optional: NVIDIA driver + CUDA (NVENC hardware encoding)

### Runtime Requirements

- Linux with Wayland compositor (GNOME 42+, KDE 6+, wlroots-based)
- XDG Desktop Portal with ScreenCast and RemoteDesktop support
- PipeWire for video capture
- D-Bus session bus

---

## Versioning

lamco-rdp-server follows Semantic Versioning (semver):
- MAJOR version for incompatible API changes
- MINOR version for backwards-compatible functionality additions
- PATCH version for backwards-compatible bug fixes

**Current:** v0.9.0 (pre-release, production-ready)
**Next:** v1.0.0 (stable release after field validation)
**Future:** v1.1.0+ (planned feature additions)
