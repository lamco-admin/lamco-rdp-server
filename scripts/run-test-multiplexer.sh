#!/bin/bash
# Test script for Phase 1 Multiplexer integration
# Tests graphics queue with drop/coalesce policy + video diagnostics

cd ~/wayland/wrd-server-specs

echo "================================================"
echo "Running wrd-server with Phase 1 Multiplexer"
echo "================================================"
echo ""
echo "New Features:"
echo "  ✓ Graphics queue (bounded 4, drop/coalesce)"
echo "  ✓ Non-blocking frame delivery"
echo "  ✓ Enhanced video diagnostics"
echo ""
echo "What to test:"
echo "  1. Video streaming (should work as before)"
echo "  2. Clipboard both directions"
echo "  3. Input responsiveness during heavy graphics"
echo "  4. Frame coalescing stats in logs"
echo ""
echo "Log file: multiplexer-test-$(date +%Y%m%d-%H%M%S).log"
echo "================================================"
echo ""

# Kill any existing wrd-server instances
pkill -f wrd-server 2>/dev/null
sleep 1

# Run with timestamped log
./target/release/wrd-server -c config.toml 2>&1 | tee multiplexer-test-$(date +%Y%m%d-%H%M%S).log
