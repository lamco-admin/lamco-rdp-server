#!/bin/bash
# Create vendored source tarball for OBS builds
# This bundles all Rust dependencies for offline builds

set -e

VERSION="${1:-0.1.0}"
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"
WORKSPACE_DIR="/home/greg/wayland/lamco-rdp-workspace"
BUILD_DIR="/tmp/lamco-rdp-server-${VERSION}"
OUTPUT_DIR="${SCRIPT_DIR}"

echo "=== Creating vendored source tarball for lamco-rdp-server ${VERSION} ==="

# Clean and create build directory
rm -rf "${BUILD_DIR}"
mkdir -p "${BUILD_DIR}"

# Copy main project
echo "Copying main project..."
cp -r "${PROJECT_DIR}/src" "${BUILD_DIR}/"
cp -r "${PROJECT_DIR}/benches" "${BUILD_DIR}/" 2>/dev/null || true
cp "${PROJECT_DIR}/Cargo.toml" "${BUILD_DIR}/"
cp "${PROJECT_DIR}/Cargo.lock" "${BUILD_DIR}/" 2>/dev/null || true
cp "${PROJECT_DIR}/config.toml" "${BUILD_DIR}/" 2>/dev/null || true
cp "${PROJECT_DIR}/LICENSE" "${BUILD_DIR}/" 2>/dev/null || true
cp "${PROJECT_DIR}/README.md" "${BUILD_DIR}/" 2>/dev/null || true

# Copy bundled crates (lamco-clipboard-core, lamco-rdp-clipboard)
echo "Copying bundled crates..."
cp -r "${PROJECT_DIR}/bundled-crates" "${BUILD_DIR}/"

# Copy workspace with local dependencies
echo "Copying workspace with local dependencies..."
mkdir -p "${BUILD_DIR}/lamco-rdp-workspace"
cp "${WORKSPACE_DIR}/Cargo.toml" "${BUILD_DIR}/lamco-rdp-workspace/"
cp -r "${WORKSPACE_DIR}/crates" "${BUILD_DIR}/lamco-rdp-workspace/"
cp -r "${WORKSPACE_DIR}/src" "${BUILD_DIR}/lamco-rdp-workspace/" 2>/dev/null || true

# Update main Cargo.toml to use local workspace paths
echo "Patching Cargo.toml for vendored build..."
sed -i 's|path = "../lamco-rdp-workspace/|path = "lamco-rdp-workspace/|g' "${BUILD_DIR}/Cargo.toml"

# Vendor all dependencies from the main project directory
echo "Vendoring dependencies (this may take a while)..."
cd "${BUILD_DIR}"
mkdir -p .cargo

# Run cargo vendor
cargo vendor vendor 2>&1 | tee vendor.log || {
    echo "Warning: cargo vendor had errors, checking if vendor directory exists..."
    if [ ! -d "vendor" ] || [ -z "$(ls -A vendor 2>/dev/null)" ]; then
        echo "Error: vendor directory is empty or missing"
        cat vendor.log
        exit 1
    fi
}

# Patch vendored lamco-pipewire with local fixes (size=0 empty frame handling)
echo "Patching vendored lamco-pipewire with local fixes..."
if [ -f "${WORKSPACE_DIR}/crates/lamco-pipewire/src/pw_thread.rs" ] && [ -d "vendor/lamco-pipewire" ]; then
    cp "${WORKSPACE_DIR}/crates/lamco-pipewire/src/pw_thread.rs" "vendor/lamco-pipewire/src/pw_thread.rs"
    echo "  -> Patched pw_thread.rs (size=0 empty frame fix)"
fi

# Create .cargo/config.toml for vendored dependencies
cat > .cargo/config.toml << 'EOF'
[source.crates-io]
replace-with = "vendored-sources"

[source."git+https://github.com/lamco-admin/IronRDP?branch=master"]
git = "https://github.com/lamco-admin/IronRDP"
branch = "master"
replace-with = "vendored-sources"

[source."git+https://github.com/glamberson/IronRDP?branch=master"]
git = "https://github.com/glamberson/IronRDP"
branch = "master"
replace-with = "vendored-sources"

[source."git+https://github.com/lamco-admin/openh264-rs?branch=lamco-lower-msrv"]
git = "https://github.com/lamco-admin/openh264-rs"
branch = "lamco-lower-msrv"
replace-with = "vendored-sources"

[source."git+https://github.com/etemesi254/zune-image?branch=main"]
git = "https://github.com/etemesi254/zune-image"
branch = "main"
replace-with = "vendored-sources"

[source.vendored-sources]
directory = "vendor"
EOF

# Remove the vendor log
rm -f vendor.log

# Create the tarball
echo "Creating tarball..."
cd /tmp
tar cJf "${OUTPUT_DIR}/lamco-rdp-server-${VERSION}.tar.xz" "lamco-rdp-server-${VERSION}"

# Show size
echo "=== Created: ${OUTPUT_DIR}/lamco-rdp-server-${VERSION}.tar.xz ==="
ls -lh "${OUTPUT_DIR}/lamco-rdp-server-${VERSION}.tar.xz"

# Cleanup
rm -rf "${BUILD_DIR}"

echo "Done!"
