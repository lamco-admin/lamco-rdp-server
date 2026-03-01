#!/bin/bash
# Regenerate RPM Provides/License and Debian copyright from a vendor tarball.
#
# This is the canonical procedure for updating packaging metadata after
# cargo update, dependency changes, or new releases. Run this any time
# the vendor tarball is rebuilt.
#
# Usage:
#   ./packaging/scripts/regenerate-packaging-metadata.sh TARBALL
#
# Example:
#   ./packaging/scripts/regenerate-packaging-metadata.sh \
#       ~/lamco-admin/staging/lamco-rdp-server/v1.4.0/lamco-rdp-server-1.4.0.tar.xz
#
# What it does:
#   1. Regenerates RPM spec Provides: bundled(crate(...)) + License: tag
#   2. Regenerates Debian copyright with full vendor coverage
#   3. Cross-validates license sets between RPM and Debian
#   4. Reports any discrepancies
#
# Prerequisites:
#   - Python 3.11+ (for tomllib)
#   - cargo and rustc in PATH (for cargo metadata)
#   - Run from the project root (where Cargo.toml lives)

set -euo pipefail

TARBALL="${1:?Usage: $0 TARBALL_PATH}"
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"

if [[ ! -f "$TARBALL" ]]; then
    echo "Error: tarball not found: $TARBALL" >&2
    exit 1
fi

if [[ ! -f "$PROJECT_DIR/Cargo.toml" ]]; then
    echo "Error: not in project root (no Cargo.toml)" >&2
    exit 1
fi

echo "=== Regenerating RPM Provides and License ==="
python3 "$SCRIPT_DIR/generate-rpm-provides.py" \
    --tarball "$TARBALL" \
    --update-spec "$PROJECT_DIR/packaging/rpmfusion/lamco-rdp-server.spec"

echo ""
echo "=== Regenerating Debian copyright ==="
python3 "$SCRIPT_DIR/generate-debian-copyright.py" \
    --tarball "$TARBALL" \
    --no-api

echo ""
echo "=== Cross-validation ==="

# Extract license atoms from RPM License tag
rpm_licenses=$(grep "^License:" "$PROJECT_DIR/packaging/rpmfusion/lamco-rdp-server.spec" | head -1 | \
    sed 's/License: *//' | tr ' ' '\n' | grep -v -E '^(AND|WITH|$)')

# Extract license atoms from debian/copyright
deb_licenses=$(grep "^License:" "$PROJECT_DIR/packaging/debian/copyright" | \
    sed 's/License: *//' | tr ' ' '\n' | grep -v -E '^(AND|OR|WITH|$)' | sed 's/[()]//g')

rpm_sorted=$(echo "$rpm_licenses" | sort -u)
deb_sorted=$(echo "$deb_licenses" | sort -u)

rpm_only=$(comm -23 <(echo "$rpm_sorted") <(echo "$deb_sorted"))
deb_only=$(comm -13 <(echo "$rpm_sorted") <(echo "$deb_sorted"))

if [[ -z "$rpm_only" && -z "$deb_only" ]]; then
    echo "License sets match between RPM and Debian"
else
    if [[ -n "$rpm_only" ]]; then
        echo "WARNING: In RPM but not Debian:"
        echo "$rpm_only" | sed 's/^/  /'
    fi
    if [[ -n "$deb_only" ]]; then
        echo "WARNING: In Debian but not RPM:"
        echo "$deb_only" | sed 's/^/  /'
    fi
fi

# Count comparison
rpm_provides=$(grep -c "^Provides:" "$PROJECT_DIR/packaging/rpmfusion/lamco-rdp-server.spec")
deb_stanzas=$(grep -c "^Files:" "$PROJECT_DIR/packaging/debian/copyright")

echo ""
echo "=== Summary ==="
echo "RPM Provides:     $rpm_provides crates"
echo "Debian stanzas:   $deb_stanzas Files blocks"
echo "Spec updated:     packaging/rpmfusion/lamco-rdp-server.spec"
echo "Copyright updated: packaging/debian/copyright"
