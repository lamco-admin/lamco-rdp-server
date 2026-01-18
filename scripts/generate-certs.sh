#!/bin/bash
# Generate self-signed certificates for wrd-server

set -e

CERT_DIR="${1:-/etc/wrd-server}"
COMMON_NAME="${2:-wrd-server}"
VALIDITY_DAYS="${3:-365}"

echo "Generating self-signed certificate..."
echo "Directory: $CERT_DIR"
echo "Common Name: $COMMON_NAME"
echo "Validity: $VALIDITY_DAYS days"

# Create directory
mkdir -p "$CERT_DIR"

# Generate certificate and key
openssl req -x509 \
    -newkey rsa:4096 \
    -nodes \
    -keyout "$CERT_DIR/key.pem" \
    -out "$CERT_DIR/cert.pem" \
    -days "$VALIDITY_DAYS" \
    -subj "/CN=$COMMON_NAME" \
    -addext "subjectAltName=DNS:$COMMON_NAME,DNS:localhost,IP:127.0.0.1"

# Set permissions
chmod 644 "$CERT_DIR/cert.pem"
chmod 600 "$CERT_DIR/key.pem"

echo "✓ Certificate generated successfully"
echo "  Certificate: $CERT_DIR/cert.pem"
echo "  Private Key: $CERT_DIR/key.pem"
