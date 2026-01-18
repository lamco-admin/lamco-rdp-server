#
# spec file for package lamco-rdp-server
#
# Copyright (c) 2026 Lamco <contact@lamco.ai>
# License: BUSL-1.1
#

Name:           lamco-rdp-server
Version:        0.1.0
Release:        1%{?dist}
Summary:        Wayland RDP server for Linux desktop sharing

License:        BUSL-1.1
URL:            https://lamco.ai
Source0:        %{name}-%{version}.tar.xz

# Rust toolchain
BuildRequires:  rust >= 1.77
BuildRequires:  cargo >= 1.77

# System libraries
BuildRequires:  pkgconfig
BuildRequires:  gcc
BuildRequires:  make
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
- Clipboard synchronization
- Keyboard and mouse input
- Platform quirk detection (RHEL 9, etc.)

%prep
%setup -q

%build
# Use vendored dependencies
export CARGO_HOME="$PWD/.cargo"
export CARGO_TARGET_DIR="$PWD/target"

# Build release binary
cargo build --release --offline --features "default,vaapi"

%install
install -Dm755 target/release/%{name} %{buildroot}%{_bindir}/%{name}

# Config directory
install -dm755 %{buildroot}%{_sysconfdir}/%{name}

# Default config
install -Dm644 config.toml %{buildroot}%{_sysconfdir}/%{name}/config.toml || true

# Systemd service
install -dm755 %{buildroot}%{_userunitdir}
cat > %{buildroot}%{_userunitdir}/%{name}.service << 'EOF'
[Unit]
Description=Lamco RDP Server
Documentation=https://lamco.ai
After=graphical-session.target
Wants=xdg-desktop-portal.service

[Service]
Type=simple
ExecStart=%{_bindir}/%{name}
Restart=on-failure
RestartSec=5

[Install]
WantedBy=graphical-session.target
EOF

%files
%license LICENSE
%doc README.md
%{_bindir}/%{name}
%dir %{_sysconfdir}/%{name}
%config(noreplace) %{_sysconfdir}/%{name}/config.toml
%{_userunitdir}/%{name}.service

%changelog
* Tue Jan 14 2026 Greg <greg@lamco.ai> - 0.1.0-1
- Initial package
- RHEL 9 platform quirk detection (AVC444 disabled, clipboard unavailable)
- Multi-platform support via OBS
