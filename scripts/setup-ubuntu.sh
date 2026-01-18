#!/bin/bash
# WRD-Server Setup Script for Ubuntu 24.04
# Run this on your Ubuntu VM at 192.168.10.205

set -e

echo "╔════════════════════════════════════════════════════════════╗"
echo "║          WRD-Server Ubuntu Setup                           ║"
echo "╚════════════════════════════════════════════════════════════╝"
echo ""

# Check we're on Ubuntu
if [ ! -f /etc/lsb-release ]; then
    echo "ERROR: This script is for Ubuntu only"
    exit 1
fi

source /etc/lsb-release
echo "Detected: $DISTRIB_DESCRIPTION"
echo ""

# Check Wayland
if [ -z "$WAYLAND_DISPLAY" ]; then
    echo "WARNING: Not running in Wayland session"
    echo "  You must log out and select 'Ubuntu on Wayland' or 'GNOME on Wayland'"
    echo "  at the login screen, then run this script again."
    echo ""
    read -p "Continue anyway? (y/N) " -n 1 -r
    echo
    if [[ ! $REPLY =~ ^[Yy]$ ]]; then
        exit 1
    fi
fi

echo "Step 1: Installing system dependencies..."
sudo apt update
sudo apt install -y \
    build-essential \
    pkg-config \
    git \
    curl \
    clang \
    llvm-dev \
    libclang-dev \
    libc6-dev \
    libpipewire-0.3-dev \
    libspa-0.2-dev \
    libssl-dev \
    libpam0g-dev \
    libdbus-1-dev \
    pipewire \
    wireplumber \
    xdg-desktop-portal \
    xdg-desktop-portal-gnome

# Set libclang path for bindgen
export LIBCLANG_PATH=/usr/lib/x86_64-linux-gnu
echo 'export LIBCLANG_PATH=/usr/lib/x86_64-linux-gnu' >> ~/.bashrc

echo ""
echo "Step 2: Verifying PipeWire..."
systemctl --user enable pipewire pipewire-pulse wireplumber
systemctl --user start pipewire pipewire-pulse wireplumber
sleep 2

if systemctl --user is-active --quiet pipewire; then
    PIPEWIRE_VERSION=$(pipewire --version 2>/dev/null || echo "unknown")
    echo "  ✓ PipeWire is running: $PIPEWIRE_VERSION"
else
    echo "  ✗ PipeWire failed to start"
    echo "    Run: systemctl --user status pipewire"
    exit 1
fi

echo ""
echo "Step 3: Verifying Portal..."
if systemctl --user is-active --quiet xdg-desktop-portal; then
    echo "  ✓ Portal is running"
else
    echo "  Starting Portal..."
    systemctl --user start xdg-desktop-portal
    sleep 2
fi

if busctl --user list | grep -q org.freedesktop.portal.Desktop; then
    echo "  ✓ Portal is available on D-Bus"
else
    echo "  ✗ Portal not available"
    exit 1
fi

echo ""
echo "Step 4: Installing Rust..."
if command -v cargo &> /dev/null; then
    RUST_VERSION=$(rustc --version)
    echo "  ✓ Rust already installed: $RUST_VERSION"
else
    echo "  Installing Rust..."
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
    source "$HOME/.cargo/env"
    echo "  ✓ Rust installed"
fi

echo ""
echo "Step 5: Cloning repository..."
cd ~
if [ -d "wayland-rdp" ]; then
    echo "  Directory exists, pulling latest..."
    cd wayland-rdp
    git pull
else
    echo "  Cloning repository..."
    git clone https://github.com/lamco-admin/wayland-rdp.git
    cd wayland-rdp
fi

echo ""
echo "Step 6: Building wrd-server (this will take a few minutes)..."
cargo build --release

echo ""
echo "Step 7: Generating test certificates..."
mkdir -p certs
if [ ! -f certs/cert.pem ]; then
    openssl req -x509 -newkey rsa:4096 -nodes \
        -keyout certs/key.pem \
        -out certs/cert.pem \
        -days 365 \
        -subj "/CN=wrd-test/O=Testing/C=US"
    echo "  ✓ Certificates generated"
else
    echo "  ✓ Certificates already exist"
fi

echo ""
echo "Step 8: Creating configuration..."
cat > config.toml <<'EOF'
[server]
listen_addr = "0.0.0.0:3389"
max_connections = 5
session_timeout = 0
use_portals = true

[security]
cert_path = "certs/cert.pem"
key_path = "certs/key.pem"
require_tls = true

[video]
max_fps = 30
enable_damage_tracking = true
EOF

echo "  ✓ Configuration created: config.toml"

echo ""
echo "Step 9: Firewall configuration..."
if command -v ufw &> /dev/null; then
    sudo ufw allow 3389/tcp
    echo "  ✓ Firewall rule added (port 3389)"
fi

echo ""
echo "╔════════════════════════════════════════════════════════════╗"
echo "║          Setup Complete!                                   ║"
echo "╚════════════════════════════════════════════════════════════╝"
echo ""
echo "Next steps:"
echo ""
echo "  1. Run the server:"
echo "     cd ~/wayland-rdp"
echo "     ./target/release/wrd-server -c config.toml -vv"
echo ""
echo "  2. When the permission dialog appears, click 'Allow' or 'Share'"
echo ""
echo "  3. Connect from Windows RDP client:"
echo "     mstsc.exe"
echo "     Computer: 192.168.10.205:3389"
echo "     [Accept certificate warning]"
echo ""
echo "  4. You should see your Ubuntu desktop!"
echo ""
echo "For troubleshooting, see: ~/wayland-rdp/TESTING-ENVIRONMENT-RECOMMENDATIONS.md"
echo ""
