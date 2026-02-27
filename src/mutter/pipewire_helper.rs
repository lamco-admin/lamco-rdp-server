//! PipeWire Connection Helper for Mutter Node IDs
//!
//! Mutter provides PipeWire node IDs instead of file descriptors.
//! This module provides a helper to connect to PipeWire's default socket
//! and obtain an FD that can be used with our existing PipeWire infrastructure.

use std::os::fd::{IntoRawFd, RawFd};

use anyhow::{Context, Result};
use tracing::{debug, info};

/// Connect to PipeWire's default socket and return an FD
///
/// Establishes a connection to the PipeWire daemon running in the user's
/// session, similar to what the portal does but without portal mediation.
///
/// The returned FD is intentionally leaked from the UnixStream so that it
/// outlives this function. The caller is responsible for eventually closing it
/// (typically when the PipeWire core takes ownership).
pub fn connect_to_pipewire_daemon() -> Result<RawFd> {
    info!("Connecting to PipeWire default socket");

    let runtime_dir = std::env::var("XDG_RUNTIME_DIR").context("XDG_RUNTIME_DIR not set")?;
    let socket_path = format!("{runtime_dir}/pipewire-0");

    debug!("Attempting to connect to PipeWire socket: {}", socket_path);

    use std::os::unix::net::UnixStream;

    let stream = UnixStream::connect(&socket_path).context(format!(
        "Failed to connect to PipeWire socket: {socket_path}"
    ))?;

    // Transfer ownership of the FD out of the UnixStream.
    // into_raw_fd() consumes the stream without closing the FD.
    let fd = stream.into_raw_fd();

    info!("Connected to PipeWire daemon, FD: {}", fd);

    Ok(fd)
}

/// Helper to get PipeWire FD from Mutter session
///
/// Mutter sessions provide node IDs but not FDs. This helper connects to the
/// PipeWire daemon and returns an FD that can be used with lamco-pipewire.
/// The node_id is then used to bind to the specific stream.
pub fn get_pipewire_fd_for_mutter() -> Result<RawFd> {
    connect_to_pipewire_daemon()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "Requires PipeWire running"]
    fn test_connect_to_pipewire_daemon() {
        match connect_to_pipewire_daemon() {
            Ok(fd) => {
                println!("Connected to PipeWire daemon, FD: {fd}");
                // FD is intentionally leaked (same as production use)
            }
            Err(e) => {
                println!("PipeWire daemon not available: {e}");
            }
        }
    }
}
