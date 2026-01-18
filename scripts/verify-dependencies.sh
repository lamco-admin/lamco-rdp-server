#!/bin/bash
# Verify all dependencies are installed and at correct versions
# For WRD-Server with IronRDP v0.9 architecture

set -e

echo "======================================================================"
echo "WRD-Server Dependency Verification (IronRDP v0.9 Architecture)"
echo "======================================================================"
echo ""

ERRORS=0
WARNINGS=0

# ============================================================================
# RUST TOOLCHAIN
# ============================================================================
echo "Checking Rust toolchain..."
echo -n "  Rust version: "
if ! command -v rustc &> /dev/null; then
    echo "ERROR: rustc not found"
    ((ERRORS++))
else
    RUST_VERSION=$(rustc --version | awk '{print $2}')
    echo "$RUST_VERSION"
    if [ "$(printf '%s\n' "1.75.0" "$RUST_VERSION" | sort -V | head -n1)" != "1.75.0" ]; then
        echo "  ERROR: Rust 1.75.0+ required, found $RUST_VERSION"
        ((ERRORS++))
    else
        echo "  ✓ Rust version OK (>= 1.75.0)"
    fi
fi

echo -n "  Cargo version: "
if ! command -v cargo &> /dev/null; then
    echo "ERROR: cargo not found"
    ((ERRORS++))
else
    CARGO_VERSION=$(cargo --version | awk '{print $2}')
    echo "$CARGO_VERSION"
    echo "  ✓ Cargo found"
fi

echo ""

# ============================================================================
# SYSTEM DEPENDENCIES
# ============================================================================
echo "Checking system dependencies..."

# PipeWire (REQUIRED)
echo -n "  PipeWire: "
if ! pkg-config --exists libpipewire-0.3; then
    echo "ERROR: libpipewire-0.3 development files not found"
    ((ERRORS++))
else
    PIPEWIRE_VERSION=$(pkg-config --modversion libpipewire-0.3)
    echo "$PIPEWIRE_VERSION"
    if [ "$(printf '%s\n' "0.3.77" "$PIPEWIRE_VERSION" | sort -V | head -n1)" != "0.3.77" ]; then
        echo "  WARNING: PipeWire 0.3.77+ recommended for best compatibility"
        ((WARNINGS++))
    else
        echo "  ✓ PipeWire version OK (>= 0.3.77)"
    fi
fi

# Wayland (REQUIRED)
echo -n "  Wayland: "
if ! pkg-config --exists wayland-client; then
    echo "ERROR: wayland-client development files not found"
    ((ERRORS++))
else
    WAYLAND_VERSION=$(pkg-config --modversion wayland-client)
    echo "$WAYLAND_VERSION"
    echo "  ✓ Wayland found"
fi

# D-Bus (REQUIRED for ashpd)
echo -n "  D-Bus: "
if ! pkg-config --exists dbus-1; then
    echo "ERROR: dbus-1 development files not found"
    ((ERRORS++))
else
    DBUS_VERSION=$(pkg-config --modversion dbus-1)
    echo "$DBUS_VERSION"
    echo "  ✓ D-Bus found"
fi

# OpenH264 (OPTIONAL - for software encoding)
echo -n "  OpenH264: "
if ! pkg-config --exists openh264; then
    echo "NOT FOUND (optional - for software H.264 encoding)"
else
    OPENH264_VERSION=$(pkg-config --modversion openh264)
    echo "$OPENH264_VERSION"
    echo "  ✓ OpenH264 found (software encoding available)"
fi

# VA-API (OPTIONAL - for hardware encoding)
echo -n "  VA-API: "
if ! pkg-config --exists libva; then
    echo "NOT FOUND (optional - for hardware H.264 encoding)"
else
    LIBVA_VERSION=$(pkg-config --modversion libva)
    echo "$LIBVA_VERSION"
    echo "  ✓ VA-API found (hardware encoding available)"

    # Check if vainfo works
    if command -v vainfo &> /dev/null; then
        echo -n "  VA-API driver: "
        if vainfo &> /dev/null; then
            echo "✓ Driver loaded and functional"
        else
            echo "WARNING: VA-API installed but driver not available"
            ((WARNINGS++))
        fi
    fi
fi

# PAM (OPTIONAL - for authentication)
echo -n "  PAM: "
if [ ! -f /usr/include/security/pam_appl.h ]; then
    echo "NOT FOUND (optional - for PAM authentication)"
else
    echo "✓ PAM development files found"
fi

echo ""

# ============================================================================
# RUNTIME SERVICES (if in graphical session)
# ============================================================================
if [ -n "$WAYLAND_DISPLAY" ]; then
    echo "Checking runtime services (Wayland session detected)..."

    # PipeWire service
    echo -n "  PipeWire service: "
    if systemctl --user is-active --quiet pipewire; then
        echo "✓ Running"
    else
        echo "ERROR: Not running (required for screen capture)"
        ((ERRORS++))
    fi

    # WirePlumber
    echo -n "  WirePlumber service: "
    if systemctl --user is-active --quiet wireplumber; then
        echo "✓ Running"
    else
        echo "WARNING: Not running (PipeWire session manager)"
        ((WARNINGS++))
    fi

    # xdg-desktop-portal
    echo -n "  xdg-desktop-portal: "
    if systemctl --user is-active --quiet xdg-desktop-portal; then
        echo "✓ Running"
    else
        echo "ERROR: Not running (required for screen capture)"
        ((ERRORS++))
    fi

    # Check for compositor-specific portal backend
    echo -n "  Portal backend: "
    if systemctl --user is-active --quiet xdg-desktop-portal-gnome; then
        echo "✓ GNOME portal running"
    elif systemctl --user is-active --quiet xdg-desktop-portal-kde; then
        echo "✓ KDE portal running"
    elif systemctl --user is-active --quiet xdg-desktop-portal-wlr; then
        echo "✓ wlroots portal running"
    elif systemctl --user is-active --quiet xdg-desktop-portal-hyprland; then
        echo "✓ Hyprland portal running"
    else
        echo "WARNING: No compositor-specific portal backend detected"
        ((WARNINGS++))
    fi

    echo ""
else
    echo "Not in Wayland session - skipping runtime checks"
    echo ""
fi

# ============================================================================
# CARGO DEPENDENCIES CHECK
# ============================================================================
echo "Checking Cargo.toml for correct IronRDP dependencies..."
if [ -f "Cargo.toml" ]; then
    echo -n "  ironrdp-server: "
    if grep -q 'ironrdp-server.*0\.9' Cargo.toml; then
        echo "✓ Found (v0.9.x)"
    else
        echo "ERROR: Not found or incorrect version (expected 0.9.0)"
        ((ERRORS++))
    fi

    echo -n "  ironrdp-server helper feature: "
    if grep -q 'ironrdp-server.*features.*helper' Cargo.toml; then
        echo "✓ Enabled"
    else
        echo "WARNING: helper feature not found (recommended)"
        ((WARNINGS++))
    fi

    echo -n "  Obsolete ironrdp crate: "
    if grep -q '^ironrdp = ' Cargo.toml; then
        echo "ERROR: Found obsolete 'ironrdp' dependency (use ironrdp-server instead)"
        ((ERRORS++))
    else
        echo "✓ Not present (good)"
    fi

    echo -n "  Obsolete ironrdp-connector: "
    if grep -q 'ironrdp-connector' Cargo.toml; then
        echo "WARNING: Found obsolete 'ironrdp-connector' (not needed with ironrdp-server)"
        ((WARNINGS++))
    else
        echo "✓ Not present (good)"
    fi

    echo ""
else
    echo "  WARNING: Cargo.toml not found in current directory"
    ((WARNINGS++))
    echo ""
fi

# ============================================================================
# SUMMARY
# ============================================================================
echo "======================================================================"
echo "Verification Summary"
echo "======================================================================"
echo "Errors:   $ERRORS"
echo "Warnings: $WARNINGS"
echo ""

if [ $ERRORS -eq 0 ] && [ $WARNINGS -eq 0 ]; then
    echo "✓ All checks passed! System is ready for WRD-Server development."
    exit 0
elif [ $ERRORS -eq 0 ]; then
    echo "⚠ Verification passed with warnings. Review warnings above."
    exit 0
else
    echo "✗ Verification failed. Fix errors above before proceeding."
    exit 1
fi
