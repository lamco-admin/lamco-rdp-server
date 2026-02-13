# Lamco RDP Server Packaging

This directory contains packaging resources for various distribution methods.

## Directory Structure

```
packaging/
├── dbus/                          # D-Bus service files
│   ├── io.lamco.RdpServer.service         # Session bus activation (native)
│   ├── io.lamco.RdpServer.service.flatpak # Session bus activation (Flatpak)
│   └── io.lamco.RdpServer.System.conf     # System bus policy
├── flatpak/                       # Flatpak packaging
│   ├── io.lamco.RdpServer.yml             # Flatpak manifest
│   ├── io.lamco.RdpServer.desktop         # Desktop entry
│   └── io.lamco.RdpServer.metainfo.xml    # AppStream metadata
├── polkit/                        # PolicyKit authorization
│   └── io.lamco.RdpServer.policy          # System service authorization
└── systemd/                       # Systemd service units
    ├── lamco-rdp-server.service           # User service
    └── lamco-rdp-server-system.service    # System service
```

## Installation Methods

### 1. Flatpak (Recommended for Desktop Users)

Build and install locally:

```bash
# Install Flatpak SDK
flatpak install flathub org.freedesktop.Platform//24.08
flatpak install flathub org.freedesktop.Sdk//24.08
flatpak install flathub org.freedesktop.Sdk.Extension.rust-stable//24.08

# Build and install
cd packaging/flatpak
flatpak-builder --user --install build-dir io.lamco.RdpServer.yml

# Run
flatpak run io.lamco.RdpServer
```

**Note:** PAM authentication is not available in Flatpak due to sandbox restrictions.
Use "No Authentication" mode or connect from trusted networks only.

### 2. Native Installation (Full Features)

Install binaries and service files:

```bash
# Build
cargo build --release --features gui

# Install binaries
sudo install -Dm755 target/release/lamco-rdp-server /usr/bin/
sudo install -Dm755 target/release/lamco-rdp-server-gui /usr/bin/

# Install D-Bus service (for auto-start)
sudo cp packaging/dbus/io.lamco.RdpServer.service /usr/share/dbus-1/services/

# Install desktop entry
sudo cp packaging/flatpak/io.lamco.RdpServer.desktop /usr/share/applications/

# Install icon
sudo install -Dm644 assets/icons/io.lamco.RdpServer.svg \
    /usr/share/icons/hicolor/scalable/apps/
```

### 3. Systemd User Service

Run as a user service (starts with your session):

```bash
# Install user service
mkdir -p ~/.config/systemd/user
cp packaging/systemd/lamco-rdp-server.service ~/.config/systemd/user/

# Enable and start
systemctl --user daemon-reload
systemctl --user enable lamco-rdp-server
systemctl --user start lamco-rdp-server

# View logs
journalctl --user -u lamco-rdp-server -f
```

For auto-start on boot (before login):

```bash
sudo loginctl enable-linger $USER
```

### 4. Systemd System Service (Headless/Server)

Run as a system service with dedicated user:

```bash
# Create service user
sudo useradd -r -s /sbin/nologin lamco-rdp

# Install service files
sudo cp packaging/systemd/lamco-rdp-server-system.service /etc/systemd/system/
sudo cp packaging/dbus/io.lamco.RdpServer.System.conf /etc/dbus-1/system.d/
sudo cp packaging/polkit/io.lamco.RdpServer.policy /usr/share/polkit-1/actions/

# Reload and start
sudo systemctl daemon-reload
sudo systemctl enable lamco-rdp-server-system
sudo systemctl start lamco-rdp-server-system
```

**Note:** System service requires additional setup for graphical session access.

## D-Bus Service Activation

The D-Bus service file enables automatic server startup. When the GUI connects
to `io.lamco.RdpServer`, D-Bus will automatically launch the server if not running.

**Session Bus** (default): Used by user services and Flatpak.
- Service name: `io.lamco.RdpServer`
- Object path: `/io/lamco/RdpServer`

**System Bus**: Used by system service for multi-user access.
- Service name: `io.lamco.RdpServer.System`
- Requires polkit authorization

## Feature Availability by Installation Method

| Feature              | Flatpak | Native | Systemd User | Systemd System |
|---------------------|---------|--------|--------------|----------------|
| GUI                 | Yes     | Yes    | No*          | No*            |
| PAM Authentication  | No      | Yes    | Yes          | Yes            |
| Hardware Encoding   | Yes**   | Yes    | Yes          | Yes            |
| Wayland Portals     | Yes     | Yes    | Yes          | Limited        |
| Credential Storage  | Yes     | Yes    | Yes          | Manual         |
| Auto-start          | Yes     | Yes    | Yes          | Yes            |

\* GUI runs separately and connects via D-Bus
\** Requires GPU passthrough configuration

## Troubleshooting

### D-Bus service not starting

Check if the service file is installed correctly:

```bash
# Session bus
dbus-send --session --print-reply \
    --dest=org.freedesktop.DBus /org/freedesktop/DBus \
    org.freedesktop.DBus.ListActivatableNames | grep lamco
```

### GUI can't connect to server

Ensure the server is running or D-Bus activation is configured:

```bash
# Check if server is running
pgrep -f lamco-rdp-server

# Try manual D-Bus activation
dbus-send --session --print-reply \
    --dest=io.lamco.RdpServer /io/lamco/RdpServer \
    org.freedesktop.DBus.Peer.Ping
```

### Systemd service fails to start

Check logs for details:

```bash
# User service
journalctl --user -u lamco-rdp-server -e

# System service
sudo journalctl -u lamco-rdp-server-system -e
```
