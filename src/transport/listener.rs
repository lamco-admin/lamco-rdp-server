//! Transport-listener trait + concrete implementations.
//!
//! Phase 1: `TcpListenerImpl::bind`.
//! Phase 2 (2026-05-16): `TcpListenerImpl::from_owned_fd` for systemd
//! socket activation, `UnixListenerImpl` for pveproxy paths,
//! `PeerAddr::Unix` variant. Phases 3a/3b/4 add vsock, WebSocket, and
//! socket-activated multi-fd listeners against the same `Listener` trait shape.

use std::{
    fmt,
    net::{IpAddr, SocketAddr},
    path::{Path, PathBuf},
};

use anyhow::Context;
use thiserror::Error;
use tokio::{
    io::{AsyncRead, AsyncWrite},
    net::{TcpListener as TokioTcpListener, UnixListener as TokioUnixListener},
};
use tracing::{info, warn};

/// The byte-stream surface IronRDP's `run_connection` consumes.
///
/// Blanket-implemented for any tokio `AsyncRead + AsyncWrite + Send + Sync + Unpin`.
pub trait AsyncRdpStream: AsyncRead + AsyncWrite + Send + Sync + Unpin {}
impl<T> AsyncRdpStream for T where T: AsyncRead + AsyncWrite + Send + Sync + Unpin {}

/// Best-effort peer identification surfaced from each transport.
///
/// Different transports expose different peer information. TCP and WebSocket
/// have a `SocketAddr`; Unix sockets have a path; vsock has a
/// (CID, port) pair. The enum keeps these distinguishable for logging and
/// diagnostics rather than flattening them through `String`.
///
/// All variants are always compiled (regardless of Cargo features) so that
/// match-exhaustiveness stays simple. Constructing a `Vsock` or `WebSocket`
/// value without the corresponding feature is harmless (it's just data).
#[derive(Debug, Clone)]
pub enum PeerAddr {
    Tcp(SocketAddr),
    /// Unix-domain socket. Holds the listener's bound path (Unix-domain client
    /// addresses are anonymous for connect-side, so the listener side is more
    /// useful for diagnostics).
    Unix(String),
    /// AF_VSOCK peer (host or guest in a Hyper-V / QEMU / KVM virtualization
    /// context). `cid` is the peer's Context ID (host = 2 by convention),
    /// `port` is the AF_VSOCK port number.
    Vsock {
        cid: u32,
        port: u32,
    },
    /// WebSocket (WSS) peer. `peer` is the underlying TCP socket address;
    /// `origin` is the HTTP `Origin` header if the client sent one
    /// (browsers always do; non-browser clients may not).
    WebSocket {
        peer: SocketAddr,
        origin: Option<String>,
    },
}

impl PeerAddr {
    /// Display form used for logs, `ServerEvent::ClientConnected.peer_address`,
    /// and PAM peer-IP setup logging.
    pub fn to_display(&self) -> String {
        match self {
            Self::Tcp(addr) => addr.to_string(),
            Self::Unix(path) => format!("unix:{path}"),
            Self::Vsock { cid, port } => format!("vsock:{cid}:{port}"),
            Self::WebSocket {
                peer,
                origin: Some(o),
            } => format!("wss:{peer} (origin:{o})"),
            Self::WebSocket { peer, origin: None } => format!("wss:{peer}"),
        }
    }

    /// IP address suitable for PAM rate limiting. `None` for transports without
    /// a meaningful IP (vsock, Unix).
    pub fn ip(&self) -> Option<IpAddr> {
        match self {
            Self::Tcp(addr) | Self::WebSocket { peer: addr, .. } => Some(addr.ip()),
            Self::Unix(_) | Self::Vsock { .. } => None,
        }
    }
}

/// Selects which `RdpServer` per-connection method the dispatcher calls.
///
/// Most transports use `Standard` — the byte stream is plain TCP/Unix/vsock and
/// IronRDP performs its own TLS handshake. WebSocket+RDCleanPath uses
/// `PreAuthenticated` — the byte stream has already been TLS-terminated at the
/// WSS layer, and IronRDP must skip the TLS upgrade step.
///
/// See the Phase 3b SDS and the spike + research analysis docs.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum AcceptorMode {
    /// Use `rdp_server.run_connection(stream)` — performs the full TLS
    /// handshake. Suitable for TCP, AF_VSOCK, Unix-domain transports where
    /// the byte stream is not pre-encrypted by a lower layer.
    #[default]
    Standard,
    /// Use `rdp_server.run_connection_with(stream, TransportTls::AlreadyDone)`
    /// — skips the TLS handshake because the caller's stream is already post-TLS
    /// (typically a WSS terminator did the TLS). The connecting client MUST
    /// speak a proxy protocol (RDCleanPath) that informs it of the
    /// lower-layer TLS; vanilla mstsc/xfreerdp will not work via this mode.
    PreAuthenticated,
}

impl fmt::Display for PeerAddr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_display())
    }
}

/// An inbound connection that has completed its transport-specific handshake
/// (if any) and is ready for the IronRDP acceptor.
pub struct AcceptedConnection {
    pub peer: PeerAddr,
    pub stream: Box<dyn AsyncRdpStream>,
    /// Selects which RdpServer per-connection method the dispatcher should
    /// invoke. Defaults to `Standard` for backward compatibility with the
    /// listeners that existed before Phase 3b.
    pub mode: AcceptorMode,
}

impl AcceptedConnection {
    /// Convenience constructor for the common case where the listener uses
    /// the Standard acceptor mode (TCP, Unix, vsock).
    pub fn standard(peer: PeerAddr, stream: Box<dyn AsyncRdpStream>) -> Self {
        Self {
            peer,
            stream,
            mode: AcceptorMode::Standard,
        }
    }
}

#[derive(Debug, Error)]
pub enum TransportError {
    #[error("transport I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("transport configuration error: {0}")]
    Config(String),
    #[error("transport handshake failed: {0}")]
    Handshake(#[source] anyhow::Error),
}

/// A configured transport listener that accepts client connections and yields
/// post-handshake byte streams ready for the IronRDP acceptor.
///
/// Implementors own their transport-specific handshake (TCP: none; WebSocket:
/// HTTP upgrade + RDCleanPath PDU exchange in Phase 3b).
#[async_trait::async_trait]
pub trait Listener: Send {
    /// Human-readable transport name for logs and diagnostics
    /// (e.g. `"tcp"`, `"vsock"`, `"unix"`, `"websocket"`).
    fn transport_name(&self) -> &'static str;

    /// Accept the next inbound connection.
    ///
    /// - `Ok(Some(_))`: accepted; caller hands the stream to the IronRDP acceptor.
    /// - `Ok(None)`: listener gracefully shut down; the dispatcher retires it.
    /// - `Err(_)`: transient I/O error; the dispatcher logs and continues.
    async fn accept(&mut self) -> Result<Option<AcceptedConnection>, TransportError>;
}

/// TCP listener with SO_REUSEADDR and IPv6 dual-stack support.
///
/// Ports the socket-creation logic that previously lived inline in
/// `src/server/mod.rs:1878-1917`.
pub struct TcpListenerImpl {
    listener: TokioTcpListener,
}

impl TcpListenerImpl {
    /// Bind a TCP listener with SO_REUSEADDR on the given address.
    ///
    /// Dispatches IPv4/IPv6 socket creation based on the address family.
    /// Calls `check_port_available` for diagnostics before attempting bind.
    pub async fn bind(listen_addr: SocketAddr) -> Result<Self, TransportError> {
        // Pre-bind port availability diagnostic (capability #3 in the SDS).
        // The helper is best-effort; bind below is authoritative.
        crate::server::check_port_available(&listen_addr);

        let socket = match listen_addr {
            SocketAddr::V4(_) => tokio::net::TcpSocket::new_v4()
                .context("Failed to create IPv4 TCP socket")
                .map_err(TransportError::Handshake)?,
            SocketAddr::V6(_) => tokio::net::TcpSocket::new_v6()
                .context("Failed to create IPv6 TCP socket")
                .map_err(TransportError::Handshake)?,
        };

        socket
            .set_reuseaddr(true)
            .context("Failed to set SO_REUSEADDR")
            .map_err(TransportError::Handshake)?;

        if let Err(e) = socket.bind(listen_addr) {
            warn!(
                "Failed to bind TCP listener to {}: {}. Running diagnostic check.",
                listen_addr, e
            );
            crate::server::check_port_available(&listen_addr);
            return Err(TransportError::Io(e));
        }

        let listener = socket
            .listen(128)
            .context("Failed to start TCP listener")
            .map_err(TransportError::Handshake)?;

        info!(
            "TCP listener bound to {} with SO_REUSEADDR",
            listener.local_addr().unwrap_or(listen_addr)
        );

        Ok(Self { listener })
    }

    /// Construct from a systemd-passed `OwnedFd`. Sets non-blocking mode and
    /// wraps the fd in a `tokio::net::TcpListener`. Used by the qemu binary
    /// when `LISTEN_FDS >= 1`.
    ///
    /// # Safety
    ///
    /// Caller must ensure `fd` is a valid listening TCP socket fd, typically
    /// guaranteed by systemd via `LISTEN_FDS`. `OwnedFd` enforces ownership
    /// transfer; the caller must not use the fd after this call.
    pub fn from_owned_fd(fd: std::os::fd::OwnedFd) -> Result<Self, TransportError> {
        let std_listener = std::net::TcpListener::from(fd);
        std_listener
            .set_nonblocking(true)
            .map_err(TransportError::Io)?;
        let listener = TokioTcpListener::from_std(std_listener).map_err(TransportError::Io)?;
        info!(
            local_addr = ?listener.local_addr().ok(),
            "TCP listener wrapped from systemd-passed fd"
        );
        Ok(Self { listener })
    }
}

#[async_trait::async_trait]
impl Listener for TcpListenerImpl {
    fn transport_name(&self) -> &'static str {
        "tcp"
    }

    async fn accept(&mut self) -> Result<Option<AcceptedConnection>, TransportError> {
        let (stream, peer) = self.listener.accept().await?;
        Ok(Some(AcceptedConnection::standard(
            PeerAddr::Tcp(peer),
            Box::new(stream),
        )))
    }
}

/// Unix-domain socket listener. Ports the `bind_unix_listener` logic
/// previously in `qemu/session.rs` (stale-socket cleanup, parent-dir check).
///
/// The qemu binary uses this for the pveproxy WebSocket relay path; the
/// listener's bound path is surfaced as `PeerAddr::Unix` for diagnostics
/// (Unix-domain client-side peer addresses are anonymous).
pub struct UnixListenerImpl {
    listener: TokioUnixListener,
    path: PathBuf,
}

impl UnixListenerImpl {
    /// Bind a Unix-domain socket at `path`. Removes any stale socket file
    /// already at that path. Requires the parent directory to exist
    /// (typically via systemd `RuntimeDirectory=`).
    pub fn bind(path: &Path) -> Result<Self, TransportError> {
        if path.exists() {
            std::fs::remove_file(path).map_err(|e| {
                TransportError::Config(format!(
                    "failed to remove stale socket at {}: {e}",
                    path.display()
                ))
            })?;
        }
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
            && !parent.exists()
        {
            return Err(TransportError::Config(format!(
                "Unix socket parent directory {} does not exist",
                parent.display()
            )));
        }

        let listener = TokioUnixListener::bind(path).map_err(TransportError::Io)?;
        info!(path = %path.display(), "Unix socket listener bound");
        Ok(Self {
            listener,
            path: path.to_path_buf(),
        })
    }

    /// Construct from a systemd-passed `OwnedFd` of kind `SOCK_STREAM AF_UNIX`.
    /// Sets non-blocking and wraps in `tokio::net::UnixListener`. Surfaces the
    /// bound path via `local_addr()` when available; abstract-namespace or
    /// unnamed sockets show as `<systemd-passed>`.
    ///
    /// # Safety
    ///
    /// Caller must ensure `fd` is a valid listening AF_UNIX socket fd,
    /// typically guaranteed by systemd via `LISTEN_FDS`. `OwnedFd` enforces
    /// ownership transfer; the caller must not use the fd after this call.
    pub fn from_owned_fd(fd: std::os::fd::OwnedFd) -> Result<Self, TransportError> {
        let std_listener = std::os::unix::net::UnixListener::from(fd);
        std_listener
            .set_nonblocking(true)
            .map_err(TransportError::Io)?;
        let listener = TokioUnixListener::from_std(std_listener).map_err(TransportError::Io)?;
        let path = listener
            .local_addr()
            .ok()
            .and_then(|a| a.as_pathname().map(Path::to_path_buf))
            .unwrap_or_else(|| PathBuf::from("<systemd-passed>"));
        info!(
            path = %path.display(),
            "Unix socket listener wrapped from systemd-passed fd"
        );
        Ok(Self { listener, path })
    }
}

#[async_trait::async_trait]
impl Listener for UnixListenerImpl {
    fn transport_name(&self) -> &'static str {
        "unix"
    }

    async fn accept(&mut self) -> Result<Option<AcceptedConnection>, TransportError> {
        let (stream, _addr) = self.listener.accept().await?;
        Ok(Some(AcceptedConnection::standard(
            PeerAddr::Unix(self.path.display().to_string()),
            Box::new(stream),
        )))
    }
}

/// AF_VSOCK listener. Enables RDP over the virtio-vsock socket family —
/// primarily for Hyper-V Enhanced Session Mode (lamco-rdp-server running
/// inside a Linux Hyper-V VM accepts connections from the Windows-host RDP
/// client via vsock) and future QEMU/Proxmox guest-agent transport.
///
/// Linux-only; gated on the `vsock` Cargo feature.
#[cfg(feature = "vsock")]
pub struct VsockListenerImpl {
    listener: tokio_vsock::VsockListener,
    port: u32,
}

#[cfg(feature = "vsock")]
impl VsockListenerImpl {
    /// Bind a vsock listener on the given port. Accepts from any peer
    /// (`VMADDR_CID_ANY`). Operator-side firewall or hypervisor policy
    /// handles access control; the server does not maintain a CID allowlist.
    pub fn bind(port: u32) -> Result<Self, TransportError> {
        let addr = tokio_vsock::VsockAddr::new(tokio_vsock::VMADDR_CID_ANY, port);
        let listener = tokio_vsock::VsockListener::bind(addr).map_err(TransportError::Io)?;
        info!(port, "vsock listener bound on VMADDR_CID_ANY");
        Ok(Self { listener, port })
    }

    /// Construct from a systemd-passed `OwnedFd` of kind `SOCK_STREAM AF_VSOCK`.
    /// Requires `ListenStream=vsock:<cid>:<port>` in the `.socket` unit
    /// (systemd 246+, Linux 5.7+).
    ///
    /// # Safety
    ///
    /// Caller must ensure `fd` is a valid listening AF_VSOCK socket fd,
    /// typically guaranteed by systemd via `LISTEN_FDS`. `tokio_vsock` panics
    /// internally if the fd is not a valid vsock listener; the calling code
    /// (`ActivatedFds`) validates the systemd protocol before reaching here.
    #[expect(
        unsafe_code,
        reason = "tokio_vsock::VsockListener::from_raw_fd is unsafe; safety justified inline"
    )]
    pub fn from_owned_fd(fd: std::os::fd::OwnedFd) -> Result<Self, TransportError> {
        use std::os::fd::{FromRawFd, IntoRawFd};

        let raw = fd.into_raw_fd();
        // SAFETY: raw came from a valid OwnedFd that systemd guarantees is a
        // listening AF_VSOCK socket. tokio-vsock's FromRawFd takes ownership.
        let listener = unsafe { tokio_vsock::VsockListener::from_raw_fd(raw) };
        let port = listener.local_addr().map_or(0, |a| a.port());
        info!(port, "vsock listener wrapped from systemd-passed fd");
        Ok(Self { listener, port })
    }
}

#[cfg(feature = "vsock")]
#[async_trait::async_trait]
impl Listener for VsockListenerImpl {
    fn transport_name(&self) -> &'static str {
        "vsock"
    }

    async fn accept(&mut self) -> Result<Option<AcceptedConnection>, TransportError> {
        let (stream, peer_addr) = self.listener.accept().await.map_err(TransportError::Io)?;
        let _ = self.port; // suppress dead-code lint in case port isn't otherwise read
        Ok(Some(AcceptedConnection::standard(
            PeerAddr::Vsock {
                cid: peer_addr.cid(),
                port: peer_addr.port(),
            },
            Box::new(stream),
        )))
    }
}

/// Best-effort detection of whether this process is running inside a Hyper-V
/// virtual machine. Used to auto-enable the vsock listener for Enhanced
/// Session Mode without operator configuration.
///
/// Checks `/sys/class/dmi/id/sys_vendor == "Microsoft Corporation"` AND
/// `/sys/class/dmi/id/product_name` contains `"Virtual Machine"`. Both must
/// be true; we never auto-enable on uncertainty (false negatives are
/// preferred to false positives — the latter would bind an unwanted vsock
/// listener, the former just means the operator must configure manually).
///
/// Returns `false` on non-Linux systems and on any I/O error reading DMI.
pub fn detect_hyperv() -> bool {
    let vendor = std::fs::read_to_string("/sys/class/dmi/id/sys_vendor")
        .ok()
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    let product = std::fs::read_to_string("/sys/class/dmi/id/product_name")
        .ok()
        .map(|s| s.trim().to_string())
        .unwrap_or_default();

    vendor == "Microsoft Corporation" && product.contains("Virtual Machine")
}

#[cfg(test)]
mod tests {
    use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};

    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpStream,
    };

    use super::*;

    #[tokio::test]
    async fn peer_addr_tcp_display_matches_socket_addr() {
        let addr: SocketAddr = "192.168.1.5:3389".parse().unwrap();
        let peer = PeerAddr::Tcp(addr);
        assert_eq!(peer.to_display(), "192.168.1.5:3389");
        assert_eq!(peer.ip(), Some(addr.ip()));
    }

    #[tokio::test]
    async fn tcp_listener_bind_accept_and_byte_roundtrip() {
        // Bind on 127.0.0.1:0 (any free port)
        let bind_addr: SocketAddr = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0));
        let mut listener = TcpListenerImpl::bind(bind_addr).await.unwrap();
        let bound_addr = listener.listener.local_addr().unwrap();

        // Spawn a client that connects and sends a few bytes.
        let connect_task = tokio::spawn(async move {
            let mut s = TcpStream::connect(bound_addr).await.unwrap();
            s.write_all(b"hello").await.unwrap();
            let mut buf = [0u8; 5];
            s.read_exact(&mut buf).await.unwrap();
            buf
        });

        // Accept the connection on the listener side, read+write to verify the
        // returned stream is a working AsyncRead+AsyncWrite.
        let accepted = listener.accept().await.unwrap().unwrap();
        match &accepted.peer {
            PeerAddr::Tcp(addr) => assert_eq!(addr.ip(), bind_addr.ip()),
            other => panic!("expected Tcp peer, got {other:?}"),
        }
        let mut stream = accepted.stream;
        let mut buf = [0u8; 5];
        stream.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"hello");
        stream.write_all(b"world").await.unwrap();
        stream.shutdown().await.ok();

        let client_recv = connect_task.await.unwrap();
        assert_eq!(&client_recv, b"world");
    }

    #[tokio::test]
    async fn transport_name_is_tcp() {
        let bind_addr: SocketAddr = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0));
        let listener = TcpListenerImpl::bind(bind_addr).await.unwrap();
        assert_eq!(listener.transport_name(), "tcp");
    }

    #[tokio::test]
    async fn peer_addr_unix_display_and_ip() {
        let peer = PeerAddr::Unix("/run/lamco/qemu-rdp-100.sock".into());
        assert_eq!(peer.to_display(), "unix:/run/lamco/qemu-rdp-100.sock");
        assert_eq!(peer.ip(), None);
    }

    #[tokio::test]
    async fn unix_listener_bind_accept_and_byte_roundtrip() {
        let tmp = tempfile::TempDir::new().unwrap();
        let sock_path = tmp.path().join("test.sock");
        let mut listener = UnixListenerImpl::bind(&sock_path).unwrap();
        assert_eq!(listener.transport_name(), "unix");

        let path_for_client = sock_path.clone();
        let connect_task = tokio::spawn(async move {
            let mut s = tokio::net::UnixStream::connect(&path_for_client)
                .await
                .unwrap();
            s.write_all(b"hello").await.unwrap();
            let mut buf = [0u8; 5];
            s.read_exact(&mut buf).await.unwrap();
            buf
        });

        let accepted = listener.accept().await.unwrap().unwrap();
        match &accepted.peer {
            PeerAddr::Unix(p) => assert_eq!(p, &sock_path.display().to_string()),
            other => panic!("expected Unix peer, got {other:?}"),
        }
        let mut stream = accepted.stream;
        let mut buf = [0u8; 5];
        stream.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"hello");
        stream.write_all(b"world").await.unwrap();
        stream.shutdown().await.ok();

        let client_recv = connect_task.await.unwrap();
        assert_eq!(&client_recv, b"world");
    }

    #[tokio::test]
    async fn unix_listener_from_owned_fd_accepts_connection() {
        // Mint a real listening Unix socket via std, hand its fd to
        // UnixListenerImpl::from_owned_fd, then verify it accepts.
        use std::os::fd::AsFd;

        let tmp = tempfile::TempDir::new().unwrap();
        let sock_path = tmp.path().join("from-fd.sock");
        let std_listener = std::os::unix::net::UnixListener::bind(&sock_path).unwrap();
        let fd: std::os::fd::OwnedFd = std_listener.as_fd().try_clone_to_owned().unwrap();
        // Keep the original std_listener dropped so only our OwnedFd holds the fd
        drop(std_listener);

        let mut listener = UnixListenerImpl::from_owned_fd(fd).unwrap();
        assert_eq!(listener.transport_name(), "unix");

        let path_for_client = sock_path.clone();
        let connect_task = tokio::spawn(async move {
            let mut s = tokio::net::UnixStream::connect(&path_for_client)
                .await
                .unwrap();
            s.write_all(b"ping").await.unwrap();
        });

        let accepted = listener.accept().await.unwrap().unwrap();
        match &accepted.peer {
            PeerAddr::Unix(_) => {}
            other => panic!("expected Unix peer, got {other:?}"),
        }
        let mut stream = accepted.stream;
        let mut buf = [0u8; 4];
        stream.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"ping");

        connect_task.await.unwrap();
    }

    #[tokio::test]
    async fn unix_listener_bind_cleans_stale_socket() {
        let tmp = tempfile::TempDir::new().unwrap();
        let sock_path = tmp.path().join("stale.sock");

        // Pre-existing stale file at the path.
        std::fs::write(&sock_path, b"stale").unwrap();
        assert!(sock_path.exists());

        // bind should remove it and succeed.
        let _listener = UnixListenerImpl::bind(&sock_path).unwrap();
    }

    #[tokio::test]
    async fn peer_addr_vsock_display_and_ip() {
        let peer = PeerAddr::Vsock { cid: 2, port: 3389 };
        assert_eq!(peer.to_display(), "vsock:2:3389");
        assert_eq!(peer.ip(), None);
    }

    #[test]
    fn detect_hyperv_does_not_panic_on_any_system() {
        // The function is best-effort and must never panic regardless of
        // /sys availability or DMI content. We don't assert the result —
        // CI runs in various environments (containers without /sys/class/dmi,
        // bare metal, KVM guests, etc.).
        let _ = detect_hyperv();
    }

    // Note: `VsockListenerImpl::from_owned_fd` is not unit-tested in isolation.
    // The natural test (bind, take fd, re-wrap) fails because the original
    // tokio-vsock listener already registered the fd with the tokio reactor;
    // re-registering returns EEXIST. A proper integration test would need a
    // vsock fd minted outside tokio (e.g. via libc directly). The codepath
    // is exercised by `QemuDeployment::build_listeners` when an activated
    // "vsock" fd is present.

    #[cfg(feature = "vsock")]
    #[tokio::test]
    async fn vsock_listener_bind_smoke() {
        // Smoke test only — real accept requires a peer endpoint which isn't
        // available in CI / dev environments without vhost_vsock loaded.
        // Verify bind either succeeds (CI with vsock kernel support) or fails
        // with an expected I/O error kind.
        // Use a high port number to avoid colliding with anything else.
        match VsockListenerImpl::bind(54321) {
            Ok(_) => {} // CI/dev with vsock kernel support
            Err(TransportError::Io(e)) => match e.kind() {
                std::io::ErrorKind::PermissionDenied
                | std::io::ErrorKind::AddrInUse
                | std::io::ErrorKind::AddrNotAvailable
                | std::io::ErrorKind::NotFound => {
                    // expected in environments without vsock kernel support
                }
                other => panic!("unexpected vsock bind I/O error kind: {other:?}: {e}"),
            },
            Err(e) => panic!("unexpected vsock bind error: {e}"),
        }
    }
}
