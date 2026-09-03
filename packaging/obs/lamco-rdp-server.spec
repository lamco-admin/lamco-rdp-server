#
# spec file for package lamco-rdp-server
#
# Copyright (c) 2026 Lamco Development <office@lamco.io>
# License: BUSL-1.1
#

Name:           lamco-rdp-server
# Named app-<app-id>, not %{name}, so xdg-desktop-portal's
# sd_pid_get_user_unit()-based app-id derivation resolves us to
# io.lamco.rdp-server instead of the empty string, which is what portal
# restore-token scoping keys on.
%global unitname app-io.lamco.rdp-server
Version:        1.4.5
Release:        1%{?dist}
Summary:        Wayland RDP server for Linux desktop sharing with GUI

License:        BUSL-1.1
URL:            https://www.lamco.ai/products/lamco-rdp-server/
Source0:        %{name}-%{version}.tar.xz

# Vendored cros-libva 0.0.13 missing fields added in libva >= 2.22
Patch0:         cros-libva-vp9-compat.patch

# Disable debuginfo — we override RUSTFLAGS to strip symbols (OOM workaround)
%global debug_package %{nil}

# Rust toolchain (MSRV 1.94: edition 2024 plus the IronRDP 0.17 floor)
BuildRequires:  rust >= 1.94
BuildRequires:  cargo >= 1.94

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

# Icon/desktop integration (OBS check-filelist requires directory ownership)
BuildRequires:  hicolor-icon-theme

# Runtime dependencies
Requires:       pipewire
Requires:       xdg-desktop-portal
Requires:       pam
Requires:       hicolor-icon-theme
# fusermount3 helper + /etc/fuse.conf owner (clipboard file-transfer FUSE mount)
Requires:       fuse3

# Weak dependencies for hardware encoding
Recommends:     libva
Recommends:     intel-media-driver
Recommends:     mesa-va-drivers
# vainfo (libva-utils) lets the server detect the GPU H.264 encoder at
# startup; without it VA-API is undetected and encoding silently uses software.
Recommends:     libva-utils


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
%autosetup -p1

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

# Build release binaries (server + GUI).
# vsock + websocket activate the AF_VSOCK (Hyper-V Enhanced
# Session Mode) and WebSocket+RDCleanPath transport listeners introduced
# in v1.4.4 — pure Rust additions, no extra system-library BuildRequires.
cargo build --release --offline --features "default,vaapi,gui,vsock,websocket"

%install
install -Dm755 target/release/%{name} %{buildroot}%{_bindir}/%{name}
install -Dm755 target/release/%{name}-gui %{buildroot}%{_bindir}/%{name}-gui

# Config directory (server creates default config on first run)
install -dm755 %{buildroot}%{_sysconfdir}/%{name}

# Systemd user service
install -Dm644 packaging/systemd/%{unitname}.service %{buildroot}%{_userunitdir}/%{unitname}.service

# Desktop integration (from data/ directory in source)
install -Dm644 data/io.lamco.rdp-server.desktop %{buildroot}%{_datadir}/applications/io.lamco.rdp-server.desktop
install -Dm644 data/io.lamco.rdp-server.metainfo.xml %{buildroot}%{_datadir}/metainfo/io.lamco.rdp-server.metainfo.xml
install -Dm644 data/icons/io.lamco.rdp-server.svg %{buildroot}%{_datadir}/icons/hicolor/scalable/apps/io.lamco.rdp-server.svg
for size in 48 64 128 256; do
    install -Dm644 data/icons/io.lamco.rdp-server-${size}.png \
        %{buildroot}%{_datadir}/icons/hicolor/${size}x${size}/apps/io.lamco.rdp-server.png
done

%post
# Clipboard file-transfer mounts a read-only FUSE filesystem with allow_other so
# the desktop file manager can read pasted files. allow_other requires
# user_allow_other in fuse.conf, which only root can set — hence here, not from
# the unprivileged user service. Idempotent; left in place on uninstall since
# other FUSE software may rely on it.
if [ -f %{_sysconfdir}/fuse.conf ]; then
    grep -qsE '^[[:space:]]*user_allow_other([[:space:]]|$)' %{_sysconfdir}/fuse.conf || \
        echo 'user_allow_other' >> %{_sysconfdir}/fuse.conf
fi

# %posttrans runs once both old and new files are on disk during an
# upgrade, which is the only point where "was the old unit enabled" is
# answerable. Best-effort: only reaches users with an active systemd
# --user manager right now; anyone not logged in during the upgrade
# needs to re-enable manually.
%posttrans
OLD_UNIT="%{name}.service"
NEW_UNIT="%{unitname}.service"
for socket in /run/user/*/systemd/private; do
    [ -S "$socket" ] || continue
    uid="${socket#/run/user/}"
    uid="${uid%%/*}"
    user="$(getent passwd "$uid" | cut -d: -f1)"
    [ -n "$user" ] || continue
    if systemctl --user --machine="${user}@" is-enabled "$OLD_UNIT" >/dev/null 2>&1; then
        systemctl --user --machine="${user}@" disable "$OLD_UNIT" >/dev/null 2>&1 || true
        systemctl --user --machine="${user}@" enable "$NEW_UNIT" >/dev/null 2>&1 || true
    fi
done
exit 0

%files
%license LICENSE
%license licenses/OpenH264-BINARY_LICENSE.txt
%doc README.md
%{_bindir}/%{name}
%{_bindir}/%{name}-gui
%dir %{_sysconfdir}/%{name}
%{_userunitdir}/%{unitname}.service
%{_datadir}/applications/io.lamco.rdp-server.desktop
%{_datadir}/metainfo/io.lamco.rdp-server.metainfo.xml
%{_datadir}/icons/hicolor/scalable/apps/io.lamco.rdp-server.svg
%{_datadir}/icons/hicolor/*/apps/io.lamco.rdp-server.png

%changelog
* Wed Sep 02 2026 Greg Lamberson <greg@lamco.io> - 1.4.5-1
- New upstream release 1.4.5
- Community Edition sandboxes: the GUI can start the server (D-Bus name grant, non-fatal registration)
- Area capture on GNOME so a fullscreen video no longer freezes (capture.gnome_record_mode)
- VA-API hardware encoding now drives EGFX H.264; desktop audio capture now starts and stays in sync
- Fix a GNOME server that could stop accepting connections (EIS handshake lost wakeup); fix the first connect after an audio client disconnects failing on a stale RDPSND wave
- Server-driven cursor shapes, MS-RDPEI multitouch, client keyboard layout, RTT reporting
- Fix crash on client disconnect; fix listen port resetting to 3389; Hyper-V ESM via security_mode rdp
- Rename systemd user unit to app-io.lamco.rdp-server.service; %posttrans migrates enabled state
- Requires: fuse3 (%post enables user_allow_other); Recommends: libva-utils for VA-API detection
- MSRV 1.94; libpipewire >= 0.3.62; no debuginfo subpackage on ppc64le and EL

* Fri Jul 03 2026 Greg Lamberson <greg@lamco.io> - 1.4.4-1
- New upstream release 1.4.4
- Unified multi-transport: AF_VSOCK (Hyper-V) and experimental WebSocket/RDCleanPath
- Vulkan Video H.264 encoder; HTTP metrics and health server
- Per-connection session lifecycle on GNOME; KDE, sway, and COSMIC fixes; many GUI fixes
- Linux-to-Windows clipboard file copy; systemd unit hardening fix for PAM
- MSRV 1.89, Rust edition 2024; Licensor Lamco Development LLC, Change Date 2029-06-01

* Wed Mar 12 2026 Greg Lamberson <greg@lamco.io> - 1.4.2-3
- Add hicolor-icon-theme dep for OBS directory ownership check

* Wed Mar 12 2026 Greg Lamberson <greg@lamco.io> - 1.4.2-2
- Add cros-libva VP9 compat patch for libva >= 2.22

* Mon Mar 10 2026 Greg Lamberson <greg@lamco.io> - 1.4.2-1
- Fix Unicode keyboard mapping: map Unicode events to evdev keycodes
- PipeWire stream DRIVER flag: ensure frames at negotiated framerate
- Fix Wayland WouldBlock handling in event loop
- Fix MIME charset handling for clipboard text negotiation
- Upgrade pipewire-rs to 0.9.2 via lamco-pipewire 0.3.1
- Fix PipeWire format negotiation for audio capture

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
