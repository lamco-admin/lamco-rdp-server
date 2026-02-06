//! PipeWire Connection Helper for Mutter Node IDs
//!
//! Mutter provides PipeWire node IDs instead of file descriptors.
//! This module provides a helper to connect to PipeWire's default socket
//! and obtain an FD that can be used with our existing PipeWire infrastructure.
//!
//! NOTE: This is a temporary solution. Ideally, lamco-pipewire would support
//! both FD-based (portal) and socket-based (direct) connections. This helper
//! allows us to use Mutter without modifying lamco-pipewire's architecture.

use anyhow::{Context, Result};
use std::os::fd::{AsRawFd, RawFd};
use tracing::{debug, info};

/// Connect to PipeWire's default socket and return an FD
///
/// This establishes a connection to the PipeWire daemon running in the
/// user's session, similar to what the portal does but without portal mediation.
///
/// # Returns
///
/// Raw file descriptor connected to PipeWire daemon
///
/// # Errors
///
/// Returns error if:
/// - PipeWire socket not found
/// - Connection fails
/// - Socket permissions incorrect
pub fn connect_to_pipewire_daemon() -> Result<RawFd> {
    info!("Connecting to PipeWire default socket");

    // PipeWire socket is typically at $XDG_RUNTIME_DIR/pipewire-0
    let runtime_dir = std::env::var("XDG_RUNTIME_DIR").context("XDG_RUNTIME_DIR not set")?;

    let socket_path = format!("{}/pipewire-0", runtime_dir);

    debug!("Attempting to connect to PipeWire socket: {}", socket_path);

    // Use Unix domain socket to connect
    use std::os::unix::net::UnixStream;

    let stream = UnixStream::connect(&socket_path).context(format!(
        "Failed to connect to PipeWire socket: {}",
        socket_path
    ))?;

    let fd = stream.as_raw_fd();

    // Don't close the stream - we need the FD to stay open
    std::mem::forget(stream);

    info!("Connected to PipeWire daemon successfully, FD: {}", fd);

    Ok(fd)
}

/// Helper to get PipeWire FD from Mutter session
///
/// Mutter sessions provide node IDs but not FDs. This helper:
/// 1. Connects to PipeWire daemon
/// 2. Returns FD that can be used with lamco-pipewire
/// 3. The existing node_id can then be used to bind to specific stream
///
/// # Returns
///
/// Raw FD connected to PipeWire daemon
pub fn get_pipewire_fd_for_mutter() -> Result<RawFd> {
    connect_to_pipewire_daemon()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore] // Requires PipeWire running
    fn test_connect_to_pipewire_daemon() {
        match connect_to_pipewire_daemon() {
            Ok(fd) => {
                println!("Connected to PipeWire daemon, FD: {}", fd);
                // Note: FD is leaked intentionally (mem::forget)
            }
            Err(e) => {
                println!("PipeWire daemon not available: {}", e);
            }
        }
    }
}
