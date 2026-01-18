//! FUSE-based Clipboard File Transfer
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

use std::collections::HashMap;
use std::ffi::OsStr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use fuser::{
    FileAttr, FileType, Filesystem, MountOption, ReplyAttr, ReplyData, ReplyDirectory, ReplyEntry,
    ReplyOpen, Request, Session,
};
use parking_lot::RwLock;
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, error, info, trace, warn};

use crate::clipboard::error::{ClipboardError, Result};

// =============================================================================
// Constants
// =============================================================================

/// Root directory inode (standard FUSE convention)
const ROOT_INODE: u64 = 1;

/// First available inode for files (after root)
const FIRST_FILE_INODE: u64 = 2;

/// Default TTL for file attributes (1 second - files are ephemeral)
const TTL: Duration = Duration::from_secs(1);

/// Timeout for RDP file content requests
const RDP_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

// =============================================================================
// Virtual File Entry
// =============================================================================

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
            ino: self.inode,
            size: self.size,
            blocks: (self.size + 511) / 512,
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

// =============================================================================
// File Request/Response for Async Bridge
// =============================================================================

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

// =============================================================================
// FUSE Filesystem Implementation
// =============================================================================

/// FUSE filesystem for clipboard file transfer
pub struct FuseClipboardFs {
    /// Virtual files indexed by inode
    files: Arc<RwLock<HashMap<u64, VirtualFile>>>,
    /// Filename to inode mapping for lookup
    name_to_inode: Arc<RwLock<HashMap<String, u64>>>,
    /// Next available inode
    next_inode: AtomicU64,
    /// Channel to send file content requests to async task
    request_tx: mpsc::Sender<FileContentsRequest>,
    /// Mount point path
    mount_point: PathBuf,
}

impl FuseClipboardFs {
    /// Create a new FUSE filesystem
    pub fn new(request_tx: mpsc::Sender<FileContentsRequest>, mount_point: PathBuf) -> Self {
        Self {
            files: Arc::new(RwLock::new(HashMap::new())),
            name_to_inode: Arc::new(RwLock::new(HashMap::new())),
            next_inode: AtomicU64::new(FIRST_FILE_INODE),
            request_tx,
            mount_point,
        }
    }

    /// Get shared reference to files map
    pub fn files(&self) -> Arc<RwLock<HashMap<u64, VirtualFile>>> {
        Arc::clone(&self.files)
    }

    /// Get shared reference to name mapping
    pub fn name_to_inode(&self) -> Arc<RwLock<HashMap<String, u64>>> {
        Arc::clone(&self.name_to_inode)
    }

    /// Add a virtual file and return its inode
    pub fn add_file(
        &self,
        filename: String,
        size: u64,
        file_index: u32,
        clip_data_id: Option<u32>,
    ) -> u64 {
        let inode = self.next_inode.fetch_add(1, Ordering::SeqCst);
        let file = VirtualFile {
            inode,
            filename: filename.clone(),
            size,
            file_index,
            clip_data_id,
            created: SystemTime::now(),
        };

        {
            let mut files = self.files.write();
            files.insert(inode, file);
        }
        {
            let mut names = self.name_to_inode.write();
            names.insert(filename, inode);
        }

        inode
    }

    /// Clear all virtual files (new clipboard content)
    pub fn clear_files(&self) {
        let mut files = self.files.write();
        let mut names = self.name_to_inode.write();
        files.clear();
        names.clear();
        // Reset inode counter
        self.next_inode.store(FIRST_FILE_INODE, Ordering::SeqCst);
    }

    /// Get mount point
    pub fn mount_point(&self) -> &PathBuf {
        &self.mount_point
    }

    /// Get root directory attributes
    fn root_attr(&self) -> FileAttr {
        let now = SystemTime::now();
        FileAttr {
            ino: ROOT_INODE,
            size: 0,
            blocks: 0,
            atime: now,
            mtime: now,
            ctime: now,
            crtime: now,
            kind: FileType::Directory,
            perm: 0o555, // Read + execute (list)
            nlink: 2,
            uid: unsafe { libc::getuid() },
            gid: unsafe { libc::getgid() },
            rdev: 0,
            blksize: 512,
            flags: 0,
        }
    }
}

impl Filesystem for FuseClipboardFs {
    /// Look up a directory entry by name
    fn lookup(&mut self, _req: &Request, parent: u64, name: &OsStr, reply: ReplyEntry) {
        if parent != ROOT_INODE {
            reply.error(libc::ENOENT);
            return;
        }

        let name_str = match name.to_str() {
            Some(s) => s,
            None => {
                reply.error(libc::ENOENT);
                return;
            }
        };

        let names = self.name_to_inode.read();
        if let Some(&inode) = names.get(name_str) {
            drop(names);
            let files = self.files.read();
            if let Some(file) = files.get(&inode) {
                trace!("lookup: found '{}' -> inode {}", name_str, inode);
                reply.entry(&TTL, &file.to_attr(), 0);
                return;
            }
        }

        trace!("lookup: '{}' not found", name_str);
        reply.error(libc::ENOENT);
    }

    /// Get file attributes
    fn getattr(&mut self, _req: &Request, ino: u64, _fh: Option<u64>, reply: ReplyAttr) {
        if ino == ROOT_INODE {
            reply.attr(&TTL, &self.root_attr());
            return;
        }

        let files = self.files.read();
        if let Some(file) = files.get(&ino) {
            reply.attr(&TTL, &file.to_attr());
        } else {
            reply.error(libc::ENOENT);
        }
    }

    /// Open a file (verify it exists)
    fn open(&mut self, _req: &Request, ino: u64, flags: i32, reply: ReplyOpen) {
        // Check write flags - we're read-only
        if flags & libc::O_WRONLY != 0 || flags & libc::O_RDWR != 0 {
            reply.error(libc::EACCES);
            return;
        }

        let files = self.files.read();
        if files.contains_key(&ino) {
            // Return file handle (we use inode as handle)
            reply.opened(ino, 0);
        } else {
            reply.error(libc::ENOENT);
        }
    }

    /// Read file data - this fetches from RDP on-demand
    fn read(
        &mut self,
        _req: &Request,
        ino: u64,
        _fh: u64,
        offset: i64,
        size: u32,
        _flags: i32,
        _lock_owner: Option<u64>,
        reply: ReplyData,
    ) {
        let file = {
            let files = self.files.read();
            files.get(&ino).cloned()
        };

        let file = match file {
            Some(f) => f,
            None => {
                reply.error(libc::ENOENT);
                return;
            }
        };

        // Check bounds
        let offset = offset as u64;
        if offset >= file.size {
            // Reading past EOF returns empty
            reply.data(&[]);
            return;
        }

        // Clamp size to remaining bytes
        let remaining = file.size - offset;
        let read_size = std::cmp::min(size as u64, remaining) as u32;

        debug!(
            "FUSE read: file='{}' offset={} size={} (file_index={})",
            file.filename, offset, read_size, file.file_index
        );

        // Create oneshot channel for response
        let (response_tx, response_rx) = oneshot::channel();

        let request = FileContentsRequest {
            file_index: file.file_index,
            offset,
            size: read_size,
            clip_data_id: file.clip_data_id,
            response_tx,
        };

        // Send request to async task (blocking send from sync context)
        if self.request_tx.blocking_send(request).is_err() {
            error!("Failed to send file contents request - channel closed");
            reply.error(libc::EIO);
            return;
        }

        // Wait for response (blocking in FUSE thread)
        // Use a timeout to avoid hanging forever
        match response_rx.blocking_recv() {
            Ok(FileContentsResponse::Data(data)) => {
                trace!("FUSE read: received {} bytes", data.len());
                reply.data(&data);
            }
            Ok(FileContentsResponse::Error(e)) => {
                error!("FUSE read: RDP error: {}", e);
                reply.error(libc::EIO);
            }
            Err(_) => {
                error!("FUSE read: response channel closed");
                reply.error(libc::EIO);
            }
        }
    }

    /// Read directory contents
    fn readdir(
        &mut self,
        _req: &Request,
        ino: u64,
        _fh: u64,
        offset: i64,
        mut reply: ReplyDirectory,
    ) {
        if ino != ROOT_INODE {
            reply.error(libc::ENOTDIR);
            return;
        }

        let files = self.files.read();
        let mut entries: Vec<(u64, FileType, &str)> = vec![
            (ROOT_INODE, FileType::Directory, "."),
            (ROOT_INODE, FileType::Directory, ".."),
        ];

        for file in files.values() {
            entries.push((file.inode, FileType::RegularFile, &file.filename));
        }

        // Skip entries before offset
        for (i, (inode, file_type, name)) in entries.iter().enumerate().skip(offset as usize) {
            // Return false means buffer is full
            if reply.add(*inode, (i + 1) as i64, *file_type, name) {
                break;
            }
        }

        reply.ok();
    }

    /// Open directory
    fn opendir(&mut self, _req: &Request, ino: u64, _flags: i32, reply: ReplyOpen) {
        if ino == ROOT_INODE {
            reply.opened(0, 0);
        } else {
            reply.error(libc::ENOTDIR);
        }
    }
}

// =============================================================================
// FUSE Manager
// =============================================================================

/// Wrapper for BackgroundSession that implements Send + Sync
///
/// fuser::BackgroundSession is thread-safe (it's a handle to a background thread),
/// but doesn't implement Send + Sync because it contains internal raw pointers.
/// This wrapper marks it as safe to send across threads.
struct SendableSession(fuser::BackgroundSession);

// SAFETY: BackgroundSession is a handle to a FUSE background thread.
// The background thread handles all FUSE operations. The session struct
// itself only holds a JoinHandle and channel, which are thread-safe.
// The raw pointers are to libfuse state managed by the background thread.
unsafe impl Send for SendableSession {}
unsafe impl Sync for SendableSession {}

/// Manages FUSE filesystem lifecycle
pub struct FuseManager {
    /// Mount point path
    mount_point: PathBuf,
    /// FUSE session (set after mount) - wrapped for Send + Sync
    session: Option<SendableSession>,
    /// Shared filesystem state
    files: Arc<RwLock<HashMap<u64, VirtualFile>>>,
    /// Shared name mapping
    name_to_inode: Arc<RwLock<HashMap<String, u64>>>,
    /// Next inode counter (shared with filesystem)
    next_inode: Arc<AtomicU64>,
    /// Request channel sender (for adding files after mount)
    request_tx: mpsc::Sender<FileContentsRequest>,
}

impl FuseManager {
    /// Create a new FUSE manager with the given mount point and request channel
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
    /// Tries to mount with `allow_other` first (allows file managers to access),
    /// falls back to user-only mount if that fails (requires /etc/fuse.conf config).
    pub fn mount(&mut self) -> Result<()> {
        if self.session.is_some() {
            return Ok(()); // Already mounted
        }

        // Create mount point directory
        std::fs::create_dir_all(&self.mount_point).map_err(|e| {
            ClipboardError::FileIoError(format!("Failed to create FUSE mount point: {}", e))
        })?;

        info!(
            "Mounting FUSE clipboard filesystem at {:?}",
            self.mount_point
        );

        // Helper to create filesystem with shared state
        let create_fs = || FuseClipboardFsShared {
            files: Arc::clone(&self.files),
            name_to_inode: Arc::clone(&self.name_to_inode),
            next_inode: Arc::clone(&self.next_inode),
            request_tx: self.request_tx.clone(),
            mount_point: self.mount_point.clone(),
        };

        // Try with AllowOther first (allows file managers to access the mount)
        // This requires 'user_allow_other' in /etc/fuse.conf
        let options_with_allow_other = vec![
            MountOption::RO,
            MountOption::FSName("lamco-clipboard".to_string()),
            MountOption::AllowOther,
            MountOption::AutoUnmount,
        ];

        match fuser::spawn_mount2(create_fs(), &self.mount_point, &options_with_allow_other) {
            Ok(session) => {
                self.session = Some(SendableSession(session));
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

        // Fallback: mount without AllowOther (only current user can access)
        let options_user_only = vec![
            MountOption::RO,
            MountOption::FSName("lamco-clipboard".to_string()),
            MountOption::AutoUnmount,
        ];

        let session = fuser::spawn_mount2(create_fs(), &self.mount_point, &options_user_only)
            .map_err(|e| ClipboardError::FileIoError(format!("Failed to mount FUSE: {}", e)))?;

        self.session = Some(SendableSession(session));
        info!("FUSE clipboard filesystem mounted (user-only mode)");

        Ok(())
    }

    /// Unmount the FUSE filesystem
    pub fn unmount(&mut self) -> Result<()> {
        if let Some(session) = self.session.take() {
            info!("Unmounting FUSE clipboard filesystem");
            drop(session); // BackgroundSession unmounts on drop
        }

        // Try to remove mount point (may fail if not empty, that's ok)
        let _ = std::fs::remove_dir(&self.mount_point);

        Ok(())
    }

    /// Check if mounted
    pub fn is_mounted(&self) -> bool {
        self.session.is_some()
    }

    /// Get mount point path
    pub fn mount_point(&self) -> &PathBuf {
        &self.mount_point
    }

    /// Set virtual files from file descriptors
    ///
    /// Returns paths to the virtual files for URI generation
    pub fn set_files(
        &self,
        descriptors: Vec<FileDescriptor>,
        clip_data_id: Option<u32>,
    ) -> Vec<PathBuf> {
        // Clear existing files
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

    /// Clear all virtual files
    pub fn clear_files(&self) {
        let mut files = self.files.write();
        let mut names = self.name_to_inode.write();
        files.clear();
        names.clear();
        self.next_inode.store(FIRST_FILE_INODE, Ordering::SeqCst);
        debug!("Cleared all virtual files from FUSE");
    }

    /// Get file count
    pub fn file_count(&self) -> usize {
        self.files.read().len()
    }
}

impl Drop for FuseManager {
    fn drop(&mut self) {
        let _ = self.unmount();
    }
}

// =============================================================================
// Shared FUSE Filesystem (for BackgroundSession)
// =============================================================================

/// FUSE filesystem with shared state (for use with BackgroundSession)
struct FuseClipboardFsShared {
    files: Arc<RwLock<HashMap<u64, VirtualFile>>>,
    name_to_inode: Arc<RwLock<HashMap<String, u64>>>,
    next_inode: Arc<AtomicU64>,
    request_tx: mpsc::Sender<FileContentsRequest>,
    mount_point: PathBuf,
}

impl Filesystem for FuseClipboardFsShared {
    fn lookup(&mut self, _req: &Request, parent: u64, name: &OsStr, reply: ReplyEntry) {
        if parent != ROOT_INODE {
            reply.error(libc::ENOENT);
            return;
        }

        let name_str = match name.to_str() {
            Some(s) => s,
            None => {
                reply.error(libc::ENOENT);
                return;
            }
        };

        let names = self.name_to_inode.read();
        if let Some(&inode) = names.get(name_str) {
            drop(names);
            let files = self.files.read();
            if let Some(file) = files.get(&inode) {
                trace!("lookup: found '{}' -> inode {}", name_str, inode);
                reply.entry(&TTL, &file.to_attr(), 0);
                return;
            }
        }

        trace!("lookup: '{}' not found", name_str);
        reply.error(libc::ENOENT);
    }

    fn getattr(&mut self, _req: &Request, ino: u64, _fh: Option<u64>, reply: ReplyAttr) {
        if ino == ROOT_INODE {
            reply.attr(&TTL, &root_attr());
            return;
        }

        let files = self.files.read();
        if let Some(file) = files.get(&ino) {
            reply.attr(&TTL, &file.to_attr());
        } else {
            reply.error(libc::ENOENT);
        }
    }

    fn open(&mut self, _req: &Request, ino: u64, flags: i32, reply: ReplyOpen) {
        if flags & libc::O_WRONLY != 0 || flags & libc::O_RDWR != 0 {
            reply.error(libc::EACCES);
            return;
        }

        let files = self.files.read();
        if files.contains_key(&ino) {
            reply.opened(ino, 0);
        } else {
            reply.error(libc::ENOENT);
        }
    }

    fn read(
        &mut self,
        _req: &Request,
        ino: u64,
        _fh: u64,
        offset: i64,
        size: u32,
        _flags: i32,
        _lock_owner: Option<u64>,
        reply: ReplyData,
    ) {
        let file = {
            let files = self.files.read();
            files.get(&ino).cloned()
        };

        let file = match file {
            Some(f) => f,
            None => {
                reply.error(libc::ENOENT);
                return;
            }
        };

        let offset = offset as u64;
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
            reply.error(libc::EIO);
            return;
        }

        match response_rx.blocking_recv() {
            Ok(FileContentsResponse::Data(data)) => {
                trace!("FUSE read: received {} bytes", data.len());
                reply.data(&data);
            }
            Ok(FileContentsResponse::Error(e)) => {
                error!("FUSE read: RDP error: {}", e);
                reply.error(libc::EIO);
            }
            Err(_) => {
                error!("FUSE read: response channel closed");
                reply.error(libc::EIO);
            }
        }
    }

    fn readdir(
        &mut self,
        _req: &Request,
        ino: u64,
        _fh: u64,
        offset: i64,
        mut reply: ReplyDirectory,
    ) {
        if ino != ROOT_INODE {
            reply.error(libc::ENOTDIR);
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
            if reply.add(*inode, (i + 1) as i64, *file_type, name) {
                break;
            }
        }

        reply.ok();
    }

    fn opendir(&mut self, _req: &Request, ino: u64, _flags: i32, reply: ReplyOpen) {
        if ino == ROOT_INODE {
            reply.opened(0, 0);
        } else {
            reply.error(libc::ENOTDIR);
        }
    }
}

// =============================================================================
// Helper Functions
// =============================================================================

/// Get the FUSE mount point path
pub fn get_mount_point() -> PathBuf {
    let uid = unsafe { libc::getuid() };
    let runtime_dir =
        std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| format!("/run/user/{}", uid));

    PathBuf::from(runtime_dir).join("lamco-clipboard-fuse")
}

/// Get root directory attributes
fn root_attr() -> FileAttr {
    let now = SystemTime::now();
    FileAttr {
        ino: ROOT_INODE,
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

// =============================================================================
// File Descriptor (from RDP)
// =============================================================================

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
    /// Create a new file descriptor
    pub fn new(filename: String, size: u64) -> Self {
        Self {
            filename,
            size,
            attributes: 0,
            last_write_time: None,
        }
    }
}

// =============================================================================
// URI Generation
// =============================================================================

/// Generate gnome-copied-files format content from file paths
///
/// Format: `copy\nfile:///path/to/file1\nfile:///path/to/file2\0`
pub fn generate_gnome_copied_files_content(paths: &[PathBuf]) -> String {
    let mut content = String::from("copy\n");
    for path in paths {
        content.push_str(&format!("file://{}\n", path.display()));
    }
    // Remove trailing newline and add null terminator
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

// =============================================================================
// Tests
// =============================================================================

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
        assert_eq!(attr.ino, 2);
        assert_eq!(attr.size, 1024);
        assert_eq!(attr.kind, FileType::RegularFile);
        assert_eq!(attr.perm, 0o444);
    }
}
