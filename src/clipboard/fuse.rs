//! FUSE-based Clipboard File Transfer
//!
//! **Execution Path:** FUSE filesystem + RDP FileContents protocol
//! **Status:** Active (v1.0.0+)
//! **Platform:** Native only (FUSE not available in Flatpak sandbox)
//! **Feature:** Requires native deployment, falls back to staging in Flatpak
//!
//! Implements a virtual filesystem for on-demand clipboard file transfer.
//! When Windows copies files, we create virtual file entries in FUSE.
//! When Linux reads (pastes), we fetch data from Windows via RDP on-demand.
//!
//! # Architecture
//!
//! ```text
//! Windows Copy -> FormatList(FileGroupDescriptorW) -> Create Virtual Files
//! Linux Paste  -> read(inode) -> FileContentsRequest -> RDP -> FileContentsResponse
//! ```
//!
//! # Sync/Async Bridge
//!
//! FUSE callbacks are synchronous (called from kernel), but RDP is async.
//! We use channels to bridge: FUSE blocks on oneshot, async task fetches data.
#![expect(unsafe_code, reason = "libc::getuid/getgid for FUSE file ownership")]

use std::{
    collections::HashMap,
    ffi::OsStr,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, SystemTime},
};

use fuser::{
    Config, Errno, FileAttr, FileHandle, FileType, Filesystem, FopenFlags, Generation, INodeNo,
    LockOwner, MountOption, OpenFlags, ReplyAttr, ReplyData, ReplyDirectory, ReplyEntry, ReplyOpen,
    Request, SessionACL,
};
use parking_lot::RwLock;
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, error, info, trace};

use crate::clipboard::error::{ClipboardError, Result};

/// Root directory inode (standard FUSE convention)
const ROOT_INODE: u64 = 1;

/// First available inode for files (after root)
const FIRST_FILE_INODE: u64 = 2;

/// Default TTL for file attributes (1 second - files are ephemeral)
const TTL: Duration = Duration::from_secs(1);

#[expect(
    dead_code,
    reason = "will be used when async file content requests are wired"
)]
/// Timeout for RDP file content requests
const RDP_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// A virtual file in the FUSE filesystem
#[derive(Debug, Clone)]
pub struct VirtualFile {
    /// Unique inode number
    pub inode: u64,
    /// Filename (from FileGroupDescriptorW)
    pub filename: String,
    /// File size in bytes
    pub size: u64,
    /// Index in the FileGroupDescriptorW list (for FileContentsRequest)
    pub file_index: u32,
    /// Clipboard data ID for locking (optional)
    pub clip_data_id: Option<u32>,
    /// Creation time
    pub created: SystemTime,
}

impl VirtualFile {
    /// Create file attributes for FUSE
    fn to_attr(&self) -> FileAttr {
        let now = self.created;
        FileAttr {
            ino: INodeNo(self.inode),
            size: self.size,
            blocks: self.size.div_ceil(512),
            atime: now,
            mtime: now,
            ctime: now,
            crtime: now,
            kind: FileType::RegularFile,
            perm: 0o444, // Read-only
            nlink: 1,
            uid: unsafe { libc::getuid() },
            gid: unsafe { libc::getgid() },
            rdev: 0,
            blksize: 512,
            flags: 0,
        }
    }
}

/// Request for file contents (sent from FUSE to async task)
#[derive(Debug)]
pub struct FileContentsRequest {
    /// Index in FileGroupDescriptorW list
    pub file_index: u32,
    /// Offset in file
    pub offset: u64,
    /// Number of bytes to read
    pub size: u32,
    /// Clipboard data ID for locking
    pub clip_data_id: Option<u32>,
    /// Channel to send response back
    pub response_tx: oneshot::Sender<FileContentsResponse>,
}

/// Response with file contents (or error)
#[derive(Debug)]
pub enum FileContentsResponse {
    /// File data chunk
    Data(Vec<u8>),
    /// Error fetching data
    Error(String),
}

/// FUSE filesystem for clipboard file transfer
///
/// All state is behind Arc<RwLock<>> so that the fuser 0.17 `&self` Filesystem
/// trait works naturally with concurrent access from multiple event loop threads.
struct ClipboardFs {
    files: Arc<RwLock<HashMap<u64, VirtualFile>>>,
    name_to_inode: Arc<RwLock<HashMap<String, u64>>>,
    #[expect(dead_code, reason = "inode allocation for dynamically added files")]
    next_inode: Arc<AtomicU64>,
    request_tx: mpsc::Sender<FileContentsRequest>,
    #[expect(dead_code, reason = "needed for cleanup/unmount logging")]
    mount_point: PathBuf,
}

impl Filesystem for ClipboardFs {
    fn lookup(&self, _req: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEntry) {
        if parent.0 != ROOT_INODE {
            reply.error(Errno::ENOENT);
            return;
        }

        let name_str = match name.to_str() {
            Some(s) => s,
            None => {
                reply.error(Errno::ENOENT);
                return;
            }
        };

        let names = self.name_to_inode.read();
        if let Some(&inode) = names.get(name_str) {
            drop(names);
            let files = self.files.read();
            if let Some(file) = files.get(&inode) {
                trace!("lookup: found '{}' -> inode {}", name_str, inode);
                reply.entry(&TTL, &file.to_attr(), Generation(0));
                return;
            }
        }

        trace!("lookup: '{}' not found", name_str);
        reply.error(Errno::ENOENT);
    }

    fn getattr(&self, _req: &Request, ino: INodeNo, _fh: Option<FileHandle>, reply: ReplyAttr) {
        if ino.0 == ROOT_INODE {
            reply.attr(&TTL, &root_attr());
            return;
        }

        let files = self.files.read();
        if let Some(file) = files.get(&ino.0) {
            reply.attr(&TTL, &file.to_attr());
        } else {
            reply.error(Errno::ENOENT);
        }
    }

    fn open(&self, _req: &Request, ino: INodeNo, flags: OpenFlags, reply: ReplyOpen) {
        if flags.0 & libc::O_WRONLY != 0 || flags.0 & libc::O_RDWR != 0 {
            reply.error(Errno::EACCES);
            return;
        }

        let files = self.files.read();
        if files.contains_key(&ino.0) {
            reply.opened(FileHandle(ino.0), FopenFlags::empty());
        } else {
            reply.error(Errno::ENOENT);
        }
    }

    fn read(
        &self,
        _req: &Request,
        ino: INodeNo,
        _fh: FileHandle,
        offset: u64,
        size: u32,
        _flags: OpenFlags,
        _lock_owner: Option<LockOwner>,
        reply: ReplyData,
    ) {
        let file = {
            let files = self.files.read();
            files.get(&ino.0).cloned()
        };

        let file = match file {
            Some(f) => f,
            None => {
                reply.error(Errno::ENOENT);
                return;
            }
        };

        if offset >= file.size {
            reply.data(&[]);
            return;
        }

        let remaining = file.size - offset;
        let read_size = std::cmp::min(size as u64, remaining) as u32;

        debug!(
            "FUSE read: file='{}' offset={} size={} (file_index={})",
            file.filename, offset, read_size, file.file_index
        );

        let (response_tx, response_rx) = oneshot::channel();

        let request = FileContentsRequest {
            file_index: file.file_index,
            offset,
            size: read_size,
            clip_data_id: file.clip_data_id,
            response_tx,
        };

        if self.request_tx.blocking_send(request).is_err() {
            error!("Failed to send file contents request - channel closed");
            reply.error(Errno::EIO);
            return;
        }

        match response_rx.blocking_recv() {
            Ok(FileContentsResponse::Data(data)) => {
                trace!("FUSE read: received {} bytes", data.len());
                reply.data(&data);
            }
            Ok(FileContentsResponse::Error(e)) => {
                error!("FUSE read: RDP error: {}", e);
                reply.error(Errno::EIO);
            }
            Err(_) => {
                error!("FUSE read: response channel closed");
                reply.error(Errno::EIO);
            }
        }
    }

    fn readdir(
        &self,
        _req: &Request,
        ino: INodeNo,
        _fh: FileHandle,
        offset: u64,
        mut reply: ReplyDirectory,
    ) {
        if ino.0 != ROOT_INODE {
            reply.error(Errno::ENOTDIR);
            return;
        }

        let files = self.files.read();
        let mut entries: Vec<(u64, FileType, String)> = vec![
            (ROOT_INODE, FileType::Directory, ".".to_string()),
            (ROOT_INODE, FileType::Directory, "..".to_string()),
        ];

        for file in files.values() {
            entries.push((file.inode, FileType::RegularFile, file.filename.clone()));
        }

        for (i, (inode, file_type, name)) in entries.iter().enumerate().skip(offset as usize) {
            if reply.add(INodeNo(*inode), (i + 1) as u64, *file_type, name) {
                break;
            }
        }

        reply.ok();
    }

    fn opendir(&self, _req: &Request, ino: INodeNo, _flags: OpenFlags, reply: ReplyOpen) {
        if ino.0 == ROOT_INODE {
            reply.opened(FileHandle(0), FopenFlags::empty());
        } else {
            reply.error(Errno::ENOTDIR);
        }
    }
}

/// FUSE mount lifecycle manager
///
/// Manages mounting/unmounting FUSE filesystem and virtual file state.
/// Provides on-demand file content fetching via RDP protocol.
pub struct FuseMount {
    /// Mount point path
    mount_point: PathBuf,
    /// FUSE session (set after mount)
    session: Option<fuser::BackgroundSession>,
    /// Shared filesystem state
    files: Arc<RwLock<HashMap<u64, VirtualFile>>>,
    /// Shared name mapping
    name_to_inode: Arc<RwLock<HashMap<String, u64>>>,
    /// Next inode counter (shared with filesystem)
    next_inode: Arc<AtomicU64>,
    /// Request channel sender (for adding files after mount)
    request_tx: mpsc::Sender<FileContentsRequest>,
}

impl FuseMount {
    pub fn new(request_tx: mpsc::Sender<FileContentsRequest>) -> Result<Self> {
        let mount_point = get_mount_point();
        Ok(Self {
            mount_point,
            session: None,
            files: Arc::new(RwLock::new(HashMap::new())),
            name_to_inode: Arc::new(RwLock::new(HashMap::new())),
            next_inode: Arc::new(AtomicU64::new(FIRST_FILE_INODE)),
            request_tx,
        })
    }

    /// Mount the FUSE filesystem
    ///
    /// Tries to mount with `AllowOther` first (allows file managers to access),
    /// falls back to user-only mount if that fails (requires /etc/fuse.conf config).
    pub fn mount(&mut self) -> Result<()> {
        if self.session.is_some() {
            return Ok(()); // Already mounted
        }

        std::fs::create_dir_all(&self.mount_point).map_err(|e| {
            ClipboardError::FileIoError(format!("Failed to create FUSE mount point: {e}"))
        })?;

        info!(
            "Mounting FUSE clipboard filesystem at {:?}",
            self.mount_point
        );

        let create_fs = || ClipboardFs {
            files: Arc::clone(&self.files),
            name_to_inode: Arc::clone(&self.name_to_inode),
            next_inode: Arc::clone(&self.next_inode),
            request_tx: self.request_tx.clone(),
            mount_point: self.mount_point.clone(),
        };

        // Try with AllowOther first (allows file managers to access the mount).
        // This requires 'user_allow_other' in /etc/fuse.conf.
        let mut config_allow_other = Config::default();
        config_allow_other.mount_options = vec![
            MountOption::RO,
            MountOption::FSName("lamco-clipboard".to_string()),
            MountOption::AutoUnmount,
        ];
        config_allow_other.acl = SessionACL::All;

        match fuser::spawn_mount2(create_fs(), &self.mount_point, &config_allow_other) {
            Ok(session) => {
                self.session = Some(session);
                info!("FUSE clipboard filesystem mounted (allow_other enabled)");
                return Ok(());
            }
            Err(e) => {
                debug!(
                    "FUSE mount with allow_other failed ({}), retrying without it",
                    e
                );
            }
        }

        // Fallback: mount without AllowOther (only current user can access).
        // AutoUnmount requires allow_other or allow_root in fuser 0.17,
        // so we drop it in user-only mode and rely on Drop for cleanup.
        let mut config_user_only = Config::default();
        config_user_only.mount_options = vec![
            MountOption::RO,
            MountOption::FSName("lamco-clipboard".to_string()),
        ];
        config_user_only.acl = SessionACL::Owner;

        let session = fuser::spawn_mount2(create_fs(), &self.mount_point, &config_user_only)
            .map_err(|e| ClipboardError::FileIoError(format!("Failed to mount FUSE: {e}")))?;

        self.session = Some(session);
        info!("FUSE clipboard filesystem mounted (user-only mode)");

        Ok(())
    }

    /// Unmount the FUSE filesystem
    pub fn unmount(&mut self) -> Result<()> {
        if let Some(session) = self.session.take() {
            info!("Unmounting FUSE clipboard filesystem");
            drop(session); // BackgroundSession unmounts on drop
        }

        let _ = std::fs::remove_dir(&self.mount_point);

        Ok(())
    }

    pub fn is_mounted(&self) -> bool {
        self.session.is_some()
    }

    pub fn mount_point(&self) -> &PathBuf {
        &self.mount_point
    }

    pub fn set_files(
        &self,
        descriptors: Vec<FileDescriptor>,
        clip_data_id: Option<u32>,
    ) -> Vec<PathBuf> {
        {
            let mut files = self.files.write();
            let mut names = self.name_to_inode.write();
            files.clear();
            names.clear();
        }
        self.next_inode.store(FIRST_FILE_INODE, Ordering::SeqCst);

        let mut paths = Vec::with_capacity(descriptors.len());

        for (index, desc) in descriptors.into_iter().enumerate() {
            let inode = self.next_inode.fetch_add(1, Ordering::SeqCst);
            let file = VirtualFile {
                inode,
                filename: desc.filename.clone(),
                size: desc.size,
                file_index: index as u32,
                clip_data_id,
                created: SystemTime::now(),
            };

            debug!(
                "Adding virtual file: '{}' size={} inode={} index={}",
                desc.filename, desc.size, inode, index
            );

            {
                let mut files = self.files.write();
                files.insert(inode, file);
            }
            {
                let mut names = self.name_to_inode.write();
                names.insert(desc.filename.clone(), inode);
            }

            paths.push(self.mount_point.join(&desc.filename));
        }

        info!("Created {} virtual files in FUSE", paths.len());
        paths
    }

    pub fn clear_files(&self) {
        let mut files = self.files.write();
        let mut names = self.name_to_inode.write();
        files.clear();
        names.clear();
        self.next_inode.store(FIRST_FILE_INODE, Ordering::SeqCst);
        debug!("Cleared all virtual files from FUSE");
    }

    pub fn file_count(&self) -> usize {
        self.files.read().len()
    }
}

impl Drop for FuseMount {
    fn drop(&mut self) {
        let _ = self.unmount();
    }
}

pub fn get_mount_point() -> PathBuf {
    let uid = unsafe { libc::getuid() };
    let runtime_dir =
        std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| format!("/run/user/{uid}"));

    PathBuf::from(runtime_dir).join("lamco-clipboard-fuse")
}

fn root_attr() -> FileAttr {
    let now = SystemTime::now();
    FileAttr {
        ino: INodeNo(ROOT_INODE),
        size: 0,
        blocks: 0,
        atime: now,
        mtime: now,
        ctime: now,
        crtime: now,
        kind: FileType::Directory,
        perm: 0o555,
        nlink: 2,
        uid: unsafe { libc::getuid() },
        gid: unsafe { libc::getgid() },
        rdev: 0,
        blksize: 512,
        flags: 0,
    }
}

/// File descriptor from FileGroupDescriptorW
#[derive(Debug, Clone)]
pub struct FileDescriptor {
    /// Filename (UTF-8)
    pub filename: String,
    /// File size in bytes
    pub size: u64,
    /// File attributes (from Windows)
    pub attributes: u32,
    /// Last write time (Windows FILETIME)
    pub last_write_time: Option<u64>,
}

impl FileDescriptor {
    pub fn new(filename: String, size: u64) -> Self {
        Self {
            filename,
            size,
            attributes: 0,
            last_write_time: None,
        }
    }
}

/// Generate gnome-copied-files format content from file paths
///
/// Format: `copy\nfile:///path/to/file1\nfile:///path/to/file2\0`
pub fn generate_gnome_copied_files_content(paths: &[PathBuf]) -> String {
    let mut content = String::from("copy\n");
    for path in paths {
        content.push_str(&format!("file://{}\n", path.display()));
    }
    if content.ends_with('\n') {
        content.pop();
    }
    content.push('\0');
    content
}

/// Generate text/uri-list format content from file paths
///
/// Format: `file:///path/to/file1\r\nfile:///path/to/file2\r\n`
pub fn generate_uri_list_content(paths: &[PathBuf]) -> String {
    let mut content = String::new();
    for path in paths {
        content.push_str(&format!("file://{}\r\n", path.display()));
    }
    content
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_mount_point() {
        let path = get_mount_point();
        assert!(path.to_string_lossy().contains("lamco-clipboard-fuse"));
    }

    #[test]
    fn test_file_descriptor() {
        let desc = FileDescriptor::new("test.txt".to_string(), 1024);
        assert_eq!(desc.filename, "test.txt");
        assert_eq!(desc.size, 1024);
    }

    #[test]
    fn test_generate_gnome_copied_files() {
        let paths = vec![
            PathBuf::from("/tmp/test/file1.txt"),
            PathBuf::from("/tmp/test/file2.txt"),
        ];
        let content = generate_gnome_copied_files_content(&paths);
        assert!(content.starts_with("copy\n"));
        assert!(content.contains("file:///tmp/test/file1.txt"));
        assert!(content.contains("file:///tmp/test/file2.txt"));
        assert!(content.ends_with('\0'));
    }

    #[test]
    fn test_generate_uri_list() {
        let paths = vec![PathBuf::from("/tmp/test/file1.txt")];
        let content = generate_uri_list_content(&paths);
        assert_eq!(content, "file:///tmp/test/file1.txt\r\n");
    }

    #[test]
    fn test_virtual_file_attr() {
        let file = VirtualFile {
            inode: 2,
            filename: "test.txt".to_string(),
            size: 1024,
            file_index: 0,
            clip_data_id: None,
            created: SystemTime::now(),
        };
        let attr = file.to_attr();
        assert_eq!(attr.ino, INodeNo(2));
        assert_eq!(attr.size, 1024);
        assert_eq!(attr.kind, FileType::RegularFile);
        assert_eq!(attr.perm, 0o444);
    }
}
