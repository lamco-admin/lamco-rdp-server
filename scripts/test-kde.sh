#!/bin/bash
# Test script for KDE VM (192.168.10.205)
# Usage: ./test-kde.sh [build|run|deploy]

set -e

KDE_VM="192.168.10.205"
REMOTE_USER="greg"
REMOTE_DIR="/home/greg/lamco-rdp-server-test"
LOG_FILE="kde-test-$(date +%Y%m%d-%H%M%S).log"

case "${1:-run}" in
    build)
        echo "Building lamco-rdp-server..."
        cargo build --release
        echo "Build complete: target/release/lamco-rdp-server"
        ;;

    run)
        echo "Running lamco-rdp-server with comprehensive logging..."
        echo "Log file: $LOG_FILE"
        echo ""
        echo "Starting server..."
        ./target/release/lamco-rdp-server \
            -c config.toml \
            -vv \
            --log-file "$LOG_FILE"
        ;;

    deploy)
        echo "Deploying to KDE VM at $KDE_VM..."

        # Build first
        echo "1. Building..."
        cargo build --release

        # Create remote directory
        echo "2. Creating remote directory..."
        ssh "${REMOTE_USER}@${KDE_VM}" "mkdir -p ${REMOTE_DIR}"

        # Copy binary
        echo "3. Copying binary..."
        scp target/release/lamco-rdp-server "${REMOTE_USER}@${KDE_VM}:${REMOTE_DIR}/"

        # Copy config
        echo "4. Copying config..."
        scp config.toml "${REMOTE_USER}@${KDE_VM}:${REMOTE_DIR}/"

        # Copy certs
        echo "5. Copying certificates..."
        scp -r certs "${REMOTE_USER}@${KDE_VM}:${REMOTE_DIR}/"

        # Copy run script
        echo "6. Copying run script..."
        scp run-server.sh "${REMOTE_USER}@${KDE_VM}:${REMOTE_DIR}/"
        ssh "${REMOTE_USER}@${KDE_VM}" "chmod +x ${REMOTE_DIR}/run-server.sh"

        echo ""
        echo "Deployed successfully!"
        echo ""
        echo "To run on KDE VM:"
        echo "  ssh ${REMOTE_USER}@${KDE_VM}"
        echo "  cd ${REMOTE_DIR}"
        echo "  ./run-server.sh"
        ;;

    ssh)
        echo "Connecting to KDE VM..."
        ssh "${REMOTE_USER}@${KDE_VM}"
        ;;

    logs)
        echo "Fetching logs from KDE VM..."
        ssh "${REMOTE_USER}@${KDE_VM}" "ls -lt ${REMOTE_DIR}/kde-test-*.log | head -5"
        echo ""
        echo "Latest log:"
        LATEST=$(ssh "${REMOTE_USER}@${KDE_VM}" "ls -t ${REMOTE_DIR}/kde-test-*.log | head -1")
        if [ -n "$LATEST" ]; then
            echo "Downloading $LATEST..."
            scp "${REMOTE_USER}@${KDE_VM}:${LATEST}" .
            echo "Downloaded to $(basename $LATEST)"
        fi
        ;;

    *)
        echo "Usage: $0 [build|run|deploy|ssh|logs]"
        echo ""
        echo "Commands:"
        echo "  build   - Build the server locally"
        echo "  run     - Run the server locally with logging"
        echo "  deploy  - Build and deploy to KDE VM (192.168.10.205)"
        echo "  ssh     - SSH into KDE VM"
        echo "  logs    - Fetch latest logs from KDE VM"
        echo ""
        echo "Example workflow:"
        echo "  ./test-kde.sh deploy"
        echo "  ./test-kde.sh ssh"
        echo "  # On VM: cd lamco-rdp-server-test && ./run-server.sh"
        exit 1
        ;;
esac
