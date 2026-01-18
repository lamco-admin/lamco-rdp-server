#!/bin/bash
# Development environment setup script

set -e

echo "Setting up WRD-Server development environment..."

# Check Rust installation
if ! command -v cargo &> /dev/null; then
    echo "Error: Rust not installed. Install from https://rustup.rs/"
    exit 1
fi

echo "✓ Rust found: $(rustc --version)"

# Install required components
rustup component add clippy rustfmt llvm-tools-preview

# Create necessary directories
mkdir -p certs
mkdir -p logs

# Generate test certificates if they don't exist
if [ ! -f certs/test-cert.pem ]; then
    echo "Generating test certificates..."
    openssl req -x509 -newkey rsa:4096 -nodes \
        -keyout certs/test-key.pem \
        -out certs/test-cert.pem \
        -days 365 \
        -subj "/CN=wrd-server-test"
    echo "✓ Test certificates generated"
fi

# Check system dependencies
echo "Checking system dependencies..."
for pkg in libwayland-dev libpipewire-0.3-dev libva-dev; do
    if ! pkg-config --exists ${pkg%-dev} 2>/dev/null; then
        echo "WARNING: $pkg not found (optional for build, required for runtime)"
    fi
done

echo ""
echo "Setup complete!"
echo "Run 'cargo build' to build the project"
