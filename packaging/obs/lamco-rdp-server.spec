#
# spec file for package lamco-rdp-server
#
# Copyright (c) 2026 Lamco Development <office@lamco.io>
# License: BUSL-1.1
#

Name:           lamco-rdp-server
Version:        1.4.0
Release:        1%{?dist}
Summary:        Wayland RDP server for Linux desktop sharing with GUI

License:        BUSL-1.1
URL:            https://www.lamco.ai/products/lamco-rdp-server/
Source0:        %{name}-%{version}.tar.xz

# Disable debuginfo — we override RUSTFLAGS to strip symbols (OOM workaround)
%global debug_package %{nil}

# Rust toolchain (MSRV 1.88: iced 0.14 requires edition 2024 features)
BuildRequires:  rust >= 1.88
BuildRequires:  cargo >= 1.88

# System libraries
BuildRequires:  pkgconfig
BuildRequires:  gcc
BuildRequires:  gcc-c++
BuildRequires:  make
BuildRequires:  cmake
BuildRequires:  nasm

# PipeWire
BuildRequires:  pkgconfig(libpipewire-0.3)
BuildRequires:  pkgconfig(libspa-0.2)

# Wayland/Portal
BuildRequires:  pkgconfig(wayland-client)
BuildRequires:  pkgconfig(xkbcommon)

# D-Bus
BuildRequires:  pkgconfig(dbus-1)

# VA-API (hardware encoding)
BuildRequires:  pkgconfig(libva) >= 1.20.0

# PAM (authentication)
BuildRequires:  pam-devel

# OpenSSL (TLS)
BuildRequires:  pkgconfig(openssl)

# FUSE (clipboard file transfer)
BuildRequires:  pkgconfig(fuse3)

# Clang for bindgen
BuildRequires:  clang
BuildRequires:  clang-devel

# Runtime dependencies
Requires:       pipewire
Requires:       xdg-desktop-portal
Requires:       pam

# Weak dependencies for hardware encoding
Recommends:     libva
Recommends:     intel-media-driver
Recommends:     mesa-va-drivers

%description
lamco-rdp-server is a high-performance RDP server for Wayland-based Linux
desktops. It uses XDG Desktop Portals for secure screen capture and input
injection, enabling remote desktop access without requiring root privileges.

Features:
- H.264 video encoding via EGFX channel (AVC420/AVC444)
- Hardware-accelerated encoding (VA-API, NVENC)
- Multi-monitor support
- Clipboard synchronization (Portal + KDE Klipper cooperation)
- Keyboard and mouse input
- Platform quirk detection (RHEL 9, KDE, etc.)
- Full-featured configuration GUI (10-tab interface)
- Graceful shutdown with explicit PipeWire cleanup

%prep
%setup -q

%build
# Use vendored dependencies
export CARGO_HOME="$PWD/.cargo"
export CARGO_TARGET_DIR="$PWD/target"

# Keep peak memory under 4GB for OBS KVM workers.
# Fedora injects -Cdebuginfo=2 -Ccodegen-units=1 via %build_rustflags,
# which causes OOM. Override RUSTFLAGS directly to control memory usage.
export CARGO_PROFILE_RELEASE_LTO=off
export CARGO_PROFILE_RELEASE_CODEGEN_UNITS=16
export RUSTFLAGS="-Copt-level=3 -Ccodegen-units=16 -Cstrip=symbols"

# Build release binaries (server + GUI)
cargo build --release --offline --features "default,vaapi,gui"

%install
install -Dm755 target/release/%{name} %{buildroot}%{_bindir}/%{name}
install -Dm755 target/release/%{name}-gui %{buildroot}%{_bindir}/%{name}-gui

# Config directory (server creates default config on first run)
install -dm755 %{buildroot}%{_sysconfdir}/%{name}

# Systemd user service
install -Dm644 packaging/systemd/%{name}.service %{buildroot}%{_userunitdir}/%{name}.service

# Desktop integration (from data/ directory in source)
install -Dm644 data/io.lamco.rdp-server.desktop %{buildroot}%{_datadir}/applications/io.lamco.rdp-server.desktop
install -Dm644 data/io.lamco.rdp-server.metainfo.xml %{buildroot}%{_datadir}/metainfo/io.lamco.rdp-server.metainfo.xml
install -Dm644 data/icons/io.lamco.rdp-server.svg %{buildroot}%{_datadir}/icons/hicolor/scalable/apps/io.lamco.rdp-server.svg
for size in 48 64 128 256; do
    install -Dm644 data/icons/io.lamco.rdp-server-${size}.png \
        %{buildroot}%{_datadir}/icons/hicolor/${size}x${size}/apps/io.lamco.rdp-server.png
done

%files
%license LICENSE
%doc README.md
%{_bindir}/%{name}
%{_bindir}/%{name}-gui
%dir %{_sysconfdir}/%{name}
%{_userunitdir}/%{name}.service
%{_datadir}/applications/io.lamco.rdp-server.desktop
%{_datadir}/metainfo/io.lamco.rdp-server.metainfo.xml
%{_datadir}/icons/hicolor/scalable/apps/io.lamco.rdp-server.svg
%{_datadir}/icons/hicolor/*/apps/io.lamco.rdp-server.png

%changelog
* Tue Feb 24 2026 Greg Lamberson <greg@lamco.io> - 1.4.0-1
- Clipboard provider trait rearchitecture with backend abstraction
  (Portal, Mutter D-Bus, wlr data-control providers)
- Enhanced wlroots compositor support via xdg-desktop-portal-generic
- MSRV raised to 1.88 (iced 0.14 requirement)
- Removed ironrdp-graphics vendor patch (cast_signed available at 1.88)

* Sat Feb 15 2026 Greg Lamberson <greg@lamco.io> - 1.3.1-1
- Flathub packaging and metadata for Flatpak submission
- Clippy pedantic linting pass (deny-level pedantic warnings)
- iced 0.14 to 0.13 downgrade for distro Rust compatibility
- Rustfmt and editorconfig standardization
- Portal protocol compliance audit and roadmap
- OBS build procedure documentation and fixes

* Fri Feb 07 2026 Greg Lamberson <greg@lamco.io> - 1.3.0-1
- KDE Klipper clipboard cooperation mode (direct D-Bus integration)
- Session factory with automatic platform quirk detection
- EGFX reconnection fix (black screen on reconnect)
- Portal session crash fixes (session validity tracking)
- Graceful shutdown (Ctrl-C handler, explicit PipeWire shutdown)
- Flatpak log file creation fallback
- GUI reorganization (wired settings, server detach mode)

* Sun Jan 19 2026 Greg Lamberson <greg@lamco.io> - 1.0.0-1
- Major release with full-featured configuration GUI
- 10-tab graphical interface for all configuration options
- Professional dark theme with Lamco branding
- Server process management (start/stop/restart)
- TLS certificate generation wizard
- Live log viewer with filtering
- Real-time configuration validation
- Import/Export configuration files
- Hardware detection and capability display

* Sat Jan 18 2026 Greg Lamberson <greg@lamco.io> - 0.9.0-1
- Initial public release with core remote desktop functionality

* Tue Jan 14 2026 Greg Lamberson <greg@lamco.io> - 0.1.0-1
- Initial package
- RHEL 9 platform quirk detection (AVC444 disabled, clipboard unavailable)
- Multi-platform support via OBS
