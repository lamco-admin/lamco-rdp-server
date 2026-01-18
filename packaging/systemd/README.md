# systemd Service Files

Systemd service units for lamco-rdp-server deployment.

## Files

### lamco-rdp-server.service (User Service)
**Purpose:** Run as current user's graphical session service
**Location:** `~/.config/systemd/user/` or `/usr/lib/systemd/user/`
**Config:** `/etc/wrd-server/config.toml`

**Installation:**
```bash
# Copy to user systemd directory
mkdir -p ~/.config/systemd/user
cp lamco-rdp-server.service ~/.config/systemd/user/

# Enable and start
systemctl --user enable lamco-rdp-server.service
systemctl --user start lamco-rdp-server.service

# Check status
systemctl --user status lamco-rdp-server.service
journalctl --user -u lamco-rdp-server.service -f
```

### lamco-rdp-server@.service (Per-User Template)
**Purpose:** System-wide service that runs as specific user
**Location:** `/usr/lib/systemd/system/`
**Config:** `/home/{username}/.config/lamco-rdp-server/config.toml`

**Installation:**
```bash
# Copy to system systemd directory
sudo cp lamco-rdp-server@.service /usr/lib/systemd/system/

# Enable for specific user (e.g., "greg")
sudo systemctl enable lamco-rdp-server@greg.service
sudo systemctl start lamco-rdp-server@greg.service

# Check status
sudo systemctl status lamco-rdp-server@greg.service
sudo journalctl -u lamco-rdp-server@greg.service -f
```

## Requirements

Both service files require:
- User session with graphical environment
- XDG Desktop Portal running
- PipeWire running
- D-Bus session bus available

## Security Features

Both services include hardening:
- `NoNewPrivileges=yes` - Cannot gain privileges
- `ProtectSystem=strict` - Read-only system directories
- `ProtectHome=read-only` - Read-only home (clipboard file access)
- `PrivateTmp=yes` - Private /tmp namespace

## Automatic Restart

Services restart automatically on failure with 5-second delay.

## Logs

View logs with:
```bash
# User service
journalctl --user -u lamco-rdp-server.service -f

# System service (per-user)
sudo journalctl -u lamco-rdp-server@USERNAME.service -f
```
