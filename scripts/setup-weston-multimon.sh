#!/bin/bash
# Setup Weston for Multimonitor Testing
# Run this ON the test VM (192.168.10.205)

set -e

echo "═══════════════════════════════════════"
echo "  Weston Multimonitor Setup"
echo "═══════════════════════════════════════"
echo ""

# Step 1: Install weston
echo "Step 1: Installing weston..."
sudo apt update
sudo apt install -y weston

echo "✅ Weston installed"
echo ""

# Step 2: Create weston config for 2 outputs
echo "Step 2: Creating weston config for 2 virtual outputs..."

mkdir -p ~/.config/weston

cat > ~/.config/weston/weston.ini <<'EOF'
[core]
# Use DRM backend (creates virtual outputs)
backend=drm-backend.so
# Or use wayland backend if nested
# backend=wayland-backend.so

[shell]
# Panel and background
background-image=/usr/share/backgrounds/warty-final-ubuntu.png
background-type=scale-crop
panel-position=top

# Output 1 (Left monitor)
[output]
name=VIRTUAL1
mode=1920x1080@60
transform=normal

# Output 2 (Right monitor)
[output]
name=VIRTUAL2
mode=1920x1080@60
transform=normal
# Position to the right of output 1
# Weston will arrange them automatically

[keyboard]
keymap_model=pc105

[launcher]
icon=/usr/share/pixmaps/weston.png
path=/usr/bin/weston-terminal
EOF

echo "✅ Weston config created at ~/.config/weston/weston.ini"
echo ""

# Step 3: Create helper script to run weston
cat > ~/run-weston.sh <<'EOF'
#!/bin/bash
# Run Weston nested in GNOME

echo "Starting Weston nested compositor with 2 virtual outputs..."
echo ""
echo "Weston will create a window with 2 virtual monitors"
echo "Close the Weston window or press Ctrl+C here to stop"
echo ""

# Run weston as nested wayland compositor
# This creates 2 virtual outputs that our RDP server can use
weston --width=3840 --height=1080 &

WESTON_PID=$!
echo "Weston PID: $WESTON_PID"
echo ""
echo "Weston is running!"
echo "New Wayland display socket: $XDG_RUNTIME_DIR/wayland-1 (probably)"
echo ""
echo "Press Ctrl+C to stop weston"

wait $WESTON_PID
EOF

chmod +x ~/run-weston.sh

echo "✅ Created ~/run-weston.sh"
echo ""

# Step 4: Create helper script to run RDP server in weston
cat > ~/run-server-weston.sh <<'EOF'
#!/bin/bash
# Run RDP server using Weston's outputs (multimonitor test)

set -e

LOG_FILE="weston-multimon-test-$(date +%Y%m%d-%H%M%S).log"

echo "═══════════════════════════════════════"
echo "  RDP Server - Weston Multimonitor Test"
echo "═══════════════════════════════════════"
echo ""
echo "This will run the RDP server using Weston's virtual displays"
echo "Weston must already be running (./run-weston.sh in another terminal)"
echo ""
echo "Log file: $LOG_FILE"
echo ""

# Find weston's wayland socket
# It's usually wayland-1 when running nested
WESTON_DISPLAY="${WAYLAND_DISPLAY:-wayland-0}"

# Check if weston is running
if ! pgrep -x weston > /dev/null; then
    echo "❌ ERROR: Weston is not running!"
    echo "   Start weston first: ./run-weston.sh"
    exit 1
fi

echo "✅ Weston is running"

# Weston creates wayland-1 socket typically
if [ -S "$XDG_RUNTIME_DIR/wayland-1" ]; then
    export WAYLAND_DISPLAY=wayland-1
    echo "✅ Using Weston display: $WAYLAND_DISPLAY"
else
    echo "⚠️  Warning: wayland-1 socket not found, using $WAYLAND_DISPLAY"
fi

# Run RDP server
echo ""
echo "Starting RDP server..."
echo "  - Listening on: 0.0.0.0:3389"
echo "  - Log level: DEBUG (-vv)"
echo "  - Wayland display: $WAYLAND_DISPLAY"
echo ""
echo "Look for: 'Total streams from Portal: 2' in logs"
echo ""
echo "Press Ctrl+C to stop"
echo ""

./lamco-rdp-server \
    -c config.toml \
    -vv \
    --log-file "$LOG_FILE" \
    2>&1 | tee console-weston.log
EOF

chmod +x ~/run-server-weston.sh

echo "✅ Created ~/run-server-weston.sh"
echo ""

echo "═══════════════════════════════════════"
echo "  Setup Complete!"
echo "═══════════════════════════════════════"
echo ""
echo "Next steps:"
echo ""
echo "1. Start weston (in separate terminal/tmux):"
echo "   ./run-weston.sh"
echo ""
echo "2. Run RDP server (in another terminal):"
echo "   ./run-server-weston.sh"
echo ""
echo "3. Connect with RDP client"
echo "   Should see 2 virtual monitors!"
echo ""
EOF

chmod +x setup-weston-multimon.sh

echo "✅ Created setup script"
