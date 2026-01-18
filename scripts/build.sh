#!/bin/bash
# Build script

set -e

echo "Building WRD-Server..."

# Format check
echo "Checking formatting..."
cargo fmt -- --check || {
    echo "ERROR: Code not formatted. Run 'cargo fmt'"
    exit 1
}

# Clippy check
echo "Running clippy..."
cargo clippy -- -D warnings || {
    echo "ERROR: Clippy warnings found"
    exit 1
}

# Build
echo "Building..."
cargo build --all-features

echo "✓ Build successful"
