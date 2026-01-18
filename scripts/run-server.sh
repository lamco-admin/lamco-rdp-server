#!/bin/bash
# Run lamco-rdp-server with proper logging
# This script is deployed to test VMs

set -e

LOG_FILE="kde-test-$(date +%Y%m%d-%H%M%S).log"

echo "========================================="
echo "  lamco-rdp-server - KDE Test"
echo "========================================="
echo ""
echo "VM: $(hostname)"
echo "IP: $(hostname -I | awk '{print $1}')"
echo "Date: $(date)"
echo "Log: $LOG_FILE"
echo ""
echo "========================================="
echo ""

# Check D-Bus session
if [ -z "$DBUS_SESSION_BUS_ADDRESS" ]; then
    echo "⚠️  Setting D-Bus session address..."
    export DBUS_SESSION_BUS_ADDRESS="unix:path=/run/user/$(id -u)/bus"
fi

# Check PipeWire
if ! systemctl --user is-active pipewire >/dev/null 2>&1; then
    echo "⚠️  PipeWire not running, starting..."
    systemctl --user start pipewire
    sleep 1
fi

# Check for config
if [ ! -f config.toml ]; then
    echo "❌ Error: config.toml not found"
    echo "   Run from the deployment directory"
    exit 1
fi

# Check for certificates
if [ ! -f certs/cert.pem ] || [ ! -f certs/key.pem ]; then
    echo "❌ Error: TLS certificates not found"
    echo "   Expected: certs/cert.pem and certs/key.pem"
    exit 1
fi

echo "✅ Pre-flight checks passed"
echo ""
echo "Starting server..."
echo "  - Logging to: $LOG_FILE"
echo "  - Listening on: 0.0.0.0:3389"
echo "  - Log level: DEBUG (all subsystems)"
echo ""
echo "Press Ctrl+C to stop"
echo ""
echo "========================================="
echo ""

# Run the server
./lamco-rdp-server \
    -c config.toml \
    -vv \
    --log-file "$LOG_FILE" \
    2>&1 | tee console-output.log
