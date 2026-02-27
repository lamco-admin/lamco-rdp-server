#![expect(
    unsafe_code,
    reason = "dlopen, dlsym, and raw FFI function pointer calls"
)]

//! OpenH264 library loading with version detection.
//!
//! Loads the OpenH264 shared library dynamically, queries its version via
//! `WelsGetCodecVersion()`, and determines the correct ABI generation.

use std::{os::raw::c_int, path::PathBuf};

use tracing::{debug, info, warn};

use super::ffi_types::{ISVCEncoder, OpenH264Version};

/// ABI generation determined from runtime version detection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AbiGeneration {
    /// OpenH264 2.3.1 through 2.5.x (soname .so.7)
    Abi7,
    /// OpenH264 2.6.0+ (soname .so.8)
    Abi8,
}

impl std::fmt::Display for AbiGeneration {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AbiGeneration::Abi7 => write!(f, "ABI 7"),
            AbiGeneration::Abi8 => write!(f, "ABI 8"),
        }
    }
}

/// Detected OpenH264 capabilities based on version.
#[derive(Debug, Clone)]
pub(crate) struct OpenH264Capabilities {
    pub version: (u32, u32, u32),
    pub abi: AbiGeneration,
    #[expect(dead_code, reason = "informational capability field")]
    pub supports_psnr: bool,
    pub library_path: String,
}

impl std::fmt::Display for OpenH264Capabilities {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "OpenH264 {}.{}.{} ({}) from {}",
            self.version.0, self.version.1, self.version.2, self.abi, self.library_path,
        )
    }
}

/// Resolved function pointers from the loaded library.
pub(crate) struct OpenH264Api {
    pub capabilities: OpenH264Capabilities,
    _library: libloading::Library,
    pub create_encoder: unsafe extern "C" fn(*mut *mut ISVCEncoder) -> c_int,
    pub destroy_encoder: unsafe extern "C" fn(*mut ISVCEncoder),
}

// The library handle and function pointers are safe to send between threads.
// The library remains loaded for the lifetime of the OpenH264Api.
unsafe impl Send for OpenH264Api {}
unsafe impl Sync for OpenH264Api {}

impl OpenH264Api {
    /// Create a new encoder via the loaded library.
    ///
    /// Returns a raw encoder pointer that must be destroyed via `destroy_encoder`.
    pub(crate) unsafe fn create_encoder_instance(&self) -> Result<*mut ISVCEncoder, String> {
        let mut encoder_ptr: *mut ISVCEncoder = std::ptr::null_mut();
        let ret = unsafe { (self.create_encoder)(std::ptr::addr_of_mut!(encoder_ptr)) };
        if ret != 0 || encoder_ptr.is_null() {
            return Err(format!("WelsCreateSVCEncoder failed with code {ret}"));
        }
        Ok(encoder_ptr)
    }

    /// Destroy an encoder instance.
    pub(crate) unsafe fn destroy_encoder_instance(&self, encoder: *mut ISVCEncoder) {
        unsafe { (self.destroy_encoder)(encoder) }
    }
}

/// Directories to scan for `libopenh264.so*` at runtime.
const SEARCH_DIRS: &[&str] = &[
    // Flatpak app extension (org.freedesktop.Platform.openh264)
    "/app/lib/openh264/extra",
    "/app/lib/openh264/extra/lib",
    // Flatpak runtime extension (older runtimes)
    "/usr/lib/extensions/openh264/extra",
    "/usr/lib/extensions/openh264/extra/lib",
    // Debian/Ubuntu multiarch
    "/usr/lib/x86_64-linux-gnu",
    "/usr/lib/aarch64-linux-gnu",
    // Fedora/RHEL
    "/usr/lib64",
    // Arch/generic
    "/usr/lib",
];

/// Load the OpenH264 library, detect its version, and resolve symbols.
///
/// Search order:
/// 1. `OPENH264_LIBRARY_PATH` environment variable
/// 2. Well-known directories (Flatpak, distro-specific)
/// 3. `LD_LIBRARY_PATH` directories
pub(crate) fn load_openh264() -> Result<OpenH264Api, String> {
    // Allow explicit path override
    if let Ok(explicit_path) = std::env::var("OPENH264_LIBRARY_PATH") {
        match try_load(&explicit_path) {
            Ok(api) => return Ok(api),
            Err(e) => {
                warn!("OPENH264_LIBRARY_PATH={explicit_path} set but failed: {e}");
            }
        }
    }

    // Scan well-known directories
    for dir in SEARCH_DIRS {
        if let Some(lib_path) = find_openh264_in_dir(dir) {
            let path_str = lib_path.display().to_string();
            match try_load(&path_str) {
                Ok(api) => return Ok(api),
                Err(e) => {
                    debug!("Found {path_str} but failed: {e}");
                }
            }
        }
    }

    // Scan LD_LIBRARY_PATH
    if let Ok(ld_path) = std::env::var("LD_LIBRARY_PATH") {
        for dir in ld_path.split(':') {
            if dir.is_empty() {
                continue;
            }
            if let Some(lib_path) = find_openh264_in_dir(dir) {
                let path_str = lib_path.display().to_string();
                match try_load(&path_str) {
                    Ok(api) => return Ok(api),
                    Err(e) => {
                        debug!("Found {path_str} in LD_LIBRARY_PATH but failed: {e}");
                    }
                }
            }
        }
    }

    let is_flatpak = std::path::Path::new("/.flatpak-info").exists();
    let hint = if is_flatpak {
        "Install the OpenH264 Flatpak extension: \
         flatpak install flathub org.freedesktop.Platform.openh264"
    } else {
        "Install the Cisco OpenH264 binary: \
         libopenh264-7 (Debian/Ubuntu), openh264 (Fedora), or openh264 (Arch)"
    };
    Err(format!("OpenH264 library not found. {hint}"))
}

/// Scan a directory for `libopenh264.so*` files.
#[expect(
    clippy::unwrap_used,
    reason = "file_name/to_str guaranteed by filter; vec non-empty after early return"
)]
fn find_openh264_in_dir(dir: &str) -> Option<PathBuf> {
    let entries = std::fs::read_dir(dir).ok()?;

    let mut candidates: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("libopenh264.so"))
        })
        .collect();

    if candidates.is_empty() {
        return None;
    }

    // Prefer unversioned, then higher versions
    candidates.sort_by(|a, b| {
        let a_name = a.file_name().unwrap().to_str().unwrap();
        let b_name = b.file_name().unwrap().to_str().unwrap();
        let a_ver = a_name.strip_prefix("libopenh264.so.").unwrap_or("");
        let b_ver = b_name.strip_prefix("libopenh264.so.").unwrap_or("");
        match (a_ver.is_empty(), b_ver.is_empty()) {
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            _ => b_ver.cmp(a_ver),
        }
    });

    Some(candidates.into_iter().next().unwrap())
}

/// Load a specific library path, detect version, resolve symbols.
#[expect(unsafe_code, reason = "dlopen of system-managed OpenH264 binary")]
fn try_load(path: &str) -> Result<OpenH264Api, String> {
    // Safety: loading a system-managed library binary (distro package or
    // Flatpak extension), not an arbitrary blob.
    let lib =
        unsafe { libloading::Library::new(path) }.map_err(|e| format!("dlopen {path}: {e}"))?;

    // Resolve version function first
    let get_version: libloading::Symbol<'_, unsafe extern "C" fn() -> OpenH264Version> =
        unsafe { lib.get(b"WelsGetCodecVersion\0") }
            .map_err(|e| format!("symbol WelsGetCodecVersion: {e}"))?;

    let version = unsafe { get_version() };

    // Determine ABI generation from version
    let abi = classify_abi(version.uMajor, version.uMinor)?;

    info!(
        "Loaded OpenH264 {}.{}.{} ({abi}) from {path}",
        version.uMajor, version.uMinor, version.uRevision
    );
    info!("OpenH264 Video Codec provided by Cisco Systems, Inc.");

    // Resolve encoder creation/destruction symbols
    let create_encoder: libloading::Symbol<
        '_,
        unsafe extern "C" fn(*mut *mut ISVCEncoder) -> c_int,
    > = unsafe { lib.get(b"WelsCreateSVCEncoder\0") }
        .map_err(|e| format!("symbol WelsCreateSVCEncoder: {e}"))?;

    let destroy_encoder: libloading::Symbol<'_, unsafe extern "C" fn(*mut ISVCEncoder)> =
        unsafe { lib.get(b"WelsDestroySVCEncoder\0") }
            .map_err(|e| format!("symbol WelsDestroySVCEncoder: {e}"))?;

    let capabilities = OpenH264Capabilities {
        version: (version.uMajor, version.uMinor, version.uRevision),
        abi,
        supports_psnr: version.uMajor > 2 || (version.uMajor == 2 && version.uMinor >= 6),
        library_path: path.to_string(),
    };

    // Leak the symbols to get 'static function pointers, then forget them.
    // The Library will be kept alive in the OpenH264Api struct.
    let create_fn = *create_encoder;
    let destroy_fn = *destroy_encoder;

    Ok(OpenH264Api {
        capabilities,
        _library: lib,
        create_encoder: create_fn,
        destroy_encoder: destroy_fn,
    })
}

/// Map a version number to an ABI generation.
fn classify_abi(major: u32, minor: u32) -> Result<AbiGeneration, String> {
    if major < 2 {
        return Err(format!(
            "OpenH264 {major}.{minor} is too old (minimum 2.3.1)"
        ));
    }
    if major == 2 && minor < 3 {
        return Err(format!(
            "OpenH264 2.{minor} is too old (minimum 2.3.1, ABI 7)"
        ));
    }
    if major == 2 && minor < 6 {
        Ok(AbiGeneration::Abi7)
    } else {
        // 2.6.0+ or 3.x+: ABI 8 (forward-compat assumption)
        Ok(AbiGeneration::Abi8)
    }
}
