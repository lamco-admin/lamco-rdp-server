//! GUI rendering capability probe
//!
//! This probe detects GPU availability and determines whether the GUI can
//! be rendered using hardware acceleration, software rendering, or not at all.
//!
//! # Critical for VM Support
//!
//! VMs without GPU passthrough typically have virtual GPUs (virtio-gpu, QXL)
//! that don't support the advanced shader features required by wgpu/iced.
//! This probe detects such situations and recommends software rendering.
#![expect(
    unsafe_code,
    reason = "DRM_IOCTL_VIRTGPU_GETPARAM for Venus capset detection"
)]

use std::path::Path;

use serde::{Deserialize, Serialize};
#[cfg(feature = "gui")]
use tracing::warn;
use tracing::{debug, info};

use super::environment::{
    DisplayServer, VirtualizationType, detect_display_server, detect_virtualization, run_command,
};
use crate::capabilities::state::ServiceLevel;

/// Rendering capabilities
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RenderingCapabilities {
    /// Is a GPU available?
    pub gpu_available: bool,

    // TODO: Support multiple GPUs with role tracking (display vs compute vs offload).
    // Currently probe_gpu() returns only the compositor's active GPU via glxinfo.
    // Systems with GPU passthrough (e.g., virgl display + AMD compute) need per-GPU
    // identity to make correct decisions downstream (buffer transforms, encoding offload).
    /// GPU information if available (compositor's active GPU only)
    pub gpu_info: Option<GpuInfo>,

    /// Is wgpu supported on this system?
    pub wgpu_supported: bool,

    /// Is software rendering available?
    pub software_available: bool,

    /// Detected virtualization
    pub virtualization: Option<VirtualizationType>,

    /// Detected display server
    pub display_server: Option<DisplayServer>,

    /// Overall service level
    pub service_level: ServiceLevel,

    /// Recommendation for rendering approach
    pub recommendation: RenderingRecommendation,

    /// Reason for fallback (if applicable)
    pub fallback_reason: Option<String>,
}

/// GPU information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GpuInfo {
    /// GPU name/description (OpenGL renderer string)
    pub name: String,
    /// GPU vendor
    pub vendor: GpuVendor,
    /// Driver version
    pub driver: Option<String>,
    /// Is this a virtual GPU (virtio, QXL, virgl, llvmpipe, etc)?
    pub is_virtual: bool,
}

impl GpuInfo {
    /// Check if this is a virgl GPU (virtio-gpu with GL acceleration).
    ///
    /// virgl proxies GL commands to the host GPU through QEMU/KVM.
    /// KWin's screencast plugin has a known issue where it produces
    /// 180-rotated MemFd buffers on virgl because grabTexture()
    /// mishandles the GL Y-axis convention. Same class as KDE Bug 485827.
    pub fn is_virgl(&self) -> bool {
        self.name.to_lowercase().starts_with("virgl")
    }
}

/// Check if the compositor's display GPU is virtio-gpu (virgl) via sysfs.
///
/// This is independent of the GL context environment — the server process
/// may force llvmpipe for its own rendering while the compositor uses virgl.
/// We check DRM cards with active connectors to find the display GPU.
pub fn is_display_gpu_virgl() -> bool {
    let drm_path = std::path::Path::new("/sys/class/drm");
    let Ok(entries) = std::fs::read_dir(drm_path) else {
        return false;
    };

    for entry in entries.flatten() {
        let name = entry.file_name();
        let name_str = name.to_string_lossy();

        // Look for connector entries (e.g., "card0-Virtual-1", "card0-HDMI-A-1")
        // A connected connector means this card drives a display
        if !name_str.contains('-') || name_str.starts_with("render") {
            continue;
        }

        // Check if this connector is active
        let status_path = entry.path().join("status");
        let status = std::fs::read_to_string(&status_path).unwrap_or_default();
        if !status.trim().eq_ignore_ascii_case("connected") {
            continue;
        }

        // Extract the card name (e.g., "card0" from "card0-Virtual-1")
        let card_name = name_str.split('-').next().unwrap_or("");
        let driver_link = drm_path.join(card_name).join("device/driver");

        if let Ok(target) = std::fs::read_link(&driver_link) {
            let driver = target
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            if driver == "virtio-pci" || driver == "virtio-gpu" {
                tracing::debug!(
                    "Display GPU is virtio ({}) via connector {}",
                    driver,
                    name_str
                );
                return true;
            }
        }
    }

    false
}

/// Raw kernel UAPI layout for `DRM_IOCTL_VIRTGPU_GETPARAM`.
///
/// Matches `struct drm_virtgpu_getparam` in `linux/drm/virtgpu_drm.h` exactly
/// (two `__u64` fields, no padding needed).
#[repr(C)]
struct DrmVirtgpuGetparam {
    param: u64,
    value: u64,
}

/// `VIRTGPU_PARAM_SUPPORTED_CAPSET_IDs` — asks the driver for the bitmask of
/// capset IDs the host virtio-gpu backend advertised at probe time.
/// (`linux/drm/virtgpu_drm.h`)
const VIRTGPU_PARAM_SUPPORTED_CAPSET_IDS: u64 = 7;

/// Capset ID for Venus (Vulkan-over-virtio), used as a bit position in the
/// mask returned above. (`linux/virtio_gpu.h`: `VIRTIO_GPU_CAPSET_VENUS`)
const VIRTIO_GPU_CAPSET_VENUS: u64 = 4;

// DRM_IOCTL_VIRTGPU_GETPARAM = DRM_IOWR(DRM_COMMAND_BASE + DRM_VIRTGPU_GETPARAM, ...)
// DRM_IOCTL_BASE = 'd', DRM_COMMAND_BASE = 0x40, DRM_VIRTGPU_GETPARAM = 0x03.
nix::ioctl_readwrite!(virtgpu_getparam_ioctl, b'd', 0x43, DrmVirtgpuGetparam);

/// Check whether a specific `/dev/dri/renderD*` node's virtio-gpu backing
/// actually advertises Venus capability, via a single `GETPARAM` ioctl.
///
/// This queries a bitmask the kernel driver populates once at device probe
/// time from the host's `VIRTIO_GPU_CMD_GET_CAPSET_INFO` handshake — no host
/// round-trip, microseconds. Returns `None` if the node can't be opened or
/// the ioctl fails (e.g. not a virtio-gpu device at all).
fn query_venus_capset(render_node: &Path) -> Option<bool> {
    use std::os::fd::AsRawFd;

    let file = std::fs::File::open(render_node).ok()?;

    // `value` is NOT a scalar output slot despite the flat two-`__u64`
    // struct shape — the kernel's virtio_gpu_getparam_ioctl() ends every
    // branch with `copy_to_user(u64_to_user_ptr(param->value), &value,
    // sizeof(int))`, so `value` must hold the ADDRESS of a caller-owned
    // 4-byte (`int`-sized) output buffer, not the result itself. Leaving it
    // 0 makes the kernel copy_to_user(NULL, ...), which unconditionally
    // faults regardless of param ID. Mesa's virgl_drm_winsys.c uses this
    // exact out-param pattern.
    let mut out_value: i32 = 0;
    let mut arg = DrmVirtgpuGetparam {
        param: VIRTGPU_PARAM_SUPPORTED_CAPSET_IDS,
        value: (&raw mut out_value) as u64,
    };
    // SAFETY: `arg` is a valid, correctly-sized `drm_virtgpu_getparam` for
    // the duration of this call, and `arg.value` points at `out_value`,
    // which outlives the call. The DRM driver reads `param` and writes 4
    // bytes to the address in `value`, matching DRM_IOCTL_VIRTGPU_GETPARAM's
    // documented contract; `file` stays open (and the fd valid) throughout.
    unsafe { virtgpu_getparam_ioctl(file.as_raw_fd(), &raw mut arg) }.ok()?;

    Some(u64::from(out_value as u32) & (1 << VIRTIO_GPU_CAPSET_VENUS) != 0)
}

/// Find the render node paired with a connected virtio-gpu display card.
///
/// `cardN`'s connector directories live directly under `/sys/class/drm/`,
/// but the render node shares `cardN`'s `device/drm/` parent — walk that to
/// find the sibling `renderD*` entry.
fn connected_virtio_render_node() -> Option<std::path::PathBuf> {
    let drm_path = Path::new("/sys/class/drm");
    let entries = std::fs::read_dir(drm_path).ok()?;

    for entry in entries.flatten() {
        let name = entry.file_name();
        let name_str = name.to_string_lossy();

        if !name_str.contains('-') || name_str.starts_with("render") {
            continue;
        }

        let status = std::fs::read_to_string(entry.path().join("status")).unwrap_or_default();
        if !status.trim().eq_ignore_ascii_case("connected") {
            continue;
        }

        let card_name = name_str.split('-').next().unwrap_or("");
        let driver_link = drm_path.join(card_name).join("device/driver");
        let Ok(target) = std::fs::read_link(&driver_link) else {
            continue;
        };
        let driver = target
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        if driver != "virtio-pci" && driver != "virtio-gpu" {
            continue;
        }

        let render_dir = drm_path.join(card_name).join("device/drm");
        let Ok(render_entries) = std::fs::read_dir(&render_dir) else {
            continue;
        };
        for render_entry in render_entries.flatten() {
            let render_name = render_entry.file_name();
            let render_name_str = render_name.to_string_lossy();
            if render_name_str.starts_with("render") {
                return Some(Path::new("/dev/dri").join(render_name_str.as_ref()));
            }
        }
    }

    None
}

/// Check whether the connected display's virtio-gpu backing actually has
/// Venus (Vulkan-over-virtio) capability, rather than treating every
/// virtio-gpu driver match the same way. Plain 2D or virgl-3D-without-Venus
/// virtio-gpu genuinely can't back CPU-readable DMA-BUF export; Venus-capable
/// setups (`venus=on,blob=on` on the host) can. Unlike `is_display_gpu_virgl`
/// (kept as-is for its original GUI wgpu-probe-skip use), this queries the
/// kernel's real capset bitmask instead of matching on driver name alone.
pub fn is_display_gpu_venus_capable() -> bool {
    let Some(render_node) = connected_virtio_render_node() else {
        return false;
    };
    let capable = query_venus_capset(&render_node).unwrap_or(false);
    tracing::debug!(
        "Venus capset check on {}: {}",
        render_node.display(),
        capable
    );
    capable
}

/// GPU vendor identification
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum GpuVendor {
    /// Intel integrated/discrete
    Intel,
    /// AMD/ATI
    Amd,
    /// NVIDIA
    Nvidia,
    /// VirtIO virtual GPU
    VirtIO,
    /// QXL virtual GPU
    Qxl,
    /// Other/unknown
    Other(String),
}

/// Rendering recommendation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RenderingRecommendation {
    /// Use GPU-accelerated rendering
    UseGpu {
        /// Why GPU is recommended
        reason: String,
    },
    /// Use software rendering
    UseSoftware {
        /// Why software rendering is needed
        reason: String,
    },
    /// Don't attempt GUI
    NoGui {
        /// Why GUI is unavailable
        reason: String,
        /// Suggestion for the user
        suggestion: String,
    },
}

/// Rendering probe
pub struct RenderingProbe;

impl RenderingProbe {
    pub async fn probe() -> RenderingCapabilities {
        info!("Probing rendering capabilities...");

        let display_server = detect_display_server();
        debug!("Display server: {:?}", display_server);

        let virtualization = {
            let v = detect_virtualization();
            if v == VirtualizationType::None {
                None
            } else {
                Some(v)
            }
        };
        debug!("Virtualization: {:?}", virtualization);

        let (gpu_available, gpu_info) = Self::probe_gpu();
        debug!("GPU available: {}, info: {:?}", gpu_available, gpu_info);

        let software_available = Self::check_software_rendering();
        debug!("Software rendering available: {}", software_available);

        let wgpu_supported =
            if display_server.is_some() && !gpu_info.as_ref().is_some_and(|g| g.is_virtual) {
                Self::test_wgpu_compatibility().await
            } else {
                // A virtual display GPU (virtio/QXL/llvmpipe) can't drive wgpu
                // hardware rendering; skip the adapter probe, which otherwise
                // re-initializes a GLES context before failing (slow GUI start on
                // VMs). Go straight to the software recommendation.
                false
            };
        debug!("wgpu supported: {}", wgpu_supported);

        let (recommendation, fallback_reason) = Self::determine_recommendation(
            display_server.as_ref(),
            virtualization.as_ref(),
            gpu_available,
            wgpu_supported,
            software_available,
            gpu_info.as_ref(),
        );

        let service_level = match &recommendation {
            RenderingRecommendation::UseGpu { .. } => ServiceLevel::Full,
            RenderingRecommendation::UseSoftware { .. } => ServiceLevel::Fallback,
            RenderingRecommendation::NoGui { .. } => ServiceLevel::Unavailable,
        };

        info!(
            "Rendering service level: {:?}, recommendation: {:?}",
            service_level, recommendation
        );

        RenderingCapabilities {
            gpu_available,
            gpu_info,
            wgpu_supported,
            software_available,
            virtualization,
            display_server,
            service_level,
            recommendation,
            fallback_reason,
        }
    }

    fn probe_gpu() -> (bool, Option<GpuInfo>) {
        if !Path::new("/dev/dri").exists() {
            debug!("No /dev/dri - no GPU available");
            return (false, None);
        }

        if let Ok(output) = run_command("glxinfo", &["-B"])
            && let Some(info) = Self::parse_glxinfo(&output)
        {
            return (true, Some(info));
        }

        if let Ok(output) = run_command("lspci", &[]) {
            for line in output.lines() {
                if line.contains("VGA") || line.contains("3D") || line.contains("Display") {
                    let vendor = Self::parse_gpu_vendor(line);
                    let is_virtual = line.to_lowercase().contains("virtio")
                        || line.to_lowercase().contains("qxl")
                        || line.to_lowercase().contains("cirrus")
                        || line.to_lowercase().contains("bochs");

                    return (
                        true,
                        Some(GpuInfo {
                            name: line.to_string(),
                            vendor,
                            driver: None,
                            is_virtual,
                        }),
                    );
                }
            }
        }

        // /dev/dri exists but can't identify GPU
        (true, None)
    }

    fn parse_glxinfo(output: &str) -> Option<GpuInfo> {
        let mut name = None;
        let mut vendor = GpuVendor::Other("Unknown".into());
        let mut driver = None;

        for line in output.lines() {
            if line.contains("OpenGL renderer string:") {
                name = line.split(':').nth(1).map(|s| s.trim().to_string());
            }

            if line.contains("OpenGL vendor string:") {
                let vendor_str = line.split(':').nth(1).map(|s| s.trim().to_lowercase());
                vendor = match vendor_str.as_deref() {
                    Some(v) if v.contains("intel") => GpuVendor::Intel,
                    Some(v) if v.contains("amd") || v.contains("ati") => GpuVendor::Amd,
                    Some(v) if v.contains("nvidia") => GpuVendor::Nvidia,
                    Some(v) => GpuVendor::Other(v.to_string()),
                    None => GpuVendor::Other("Unknown".into()),
                };
            }

            if line.contains("OpenGL version string:") {
                driver = line.split(':').nth(1).map(|s| s.trim().to_string());
            }
        }

        let is_virtual = name.as_ref().is_some_and(|n| {
            let n = n.to_lowercase();
            n.contains("llvmpipe")
                || n.contains("softpipe")
                || n.contains("virtio")
                || n.contains("virgl")
                || n.contains("qxl")
                || n.contains("swrast")
        });

        name.map(|name| GpuInfo {
            name,
            vendor,
            driver,
            is_virtual,
        })
    }

    fn parse_gpu_vendor(line: &str) -> GpuVendor {
        let line_lower = line.to_lowercase();
        if line_lower.contains("intel") {
            GpuVendor::Intel
        } else if line_lower.contains("amd")
            || line_lower.contains("ati")
            || line_lower.contains("radeon")
        {
            GpuVendor::Amd
        } else if line_lower.contains("nvidia") {
            GpuVendor::Nvidia
        } else if line_lower.contains("virtio") {
            GpuVendor::VirtIO
        } else if line_lower.contains("qxl") {
            GpuVendor::Qxl
        } else {
            GpuVendor::Other("Unknown".into())
        }
    }

    fn check_software_rendering() -> bool {
        if let Ok(output) = run_command("glxinfo", &["-B"])
            && (output.contains("llvmpipe")
                || output.contains("softpipe")
                || output.contains("swrast"))
        {
            return true;
        }

        if Path::new("/usr/lib/x86_64-linux-gnu/dri").exists()
            || Path::new("/usr/lib64/dri").exists()
            || Path::new("/usr/lib/dri").exists()
        {
            return true;
        }

        // Mesa is almost always present on modern Linux
        true
    }

    async fn test_wgpu_compatibility() -> bool {
        // Lightweight heuristic -- no window created, just adapter availability
        #[cfg(feature = "gui")]
        {
            use std::time::Duration;

            let result = tokio::time::timeout(Duration::from_secs(5), async {
                tokio::task::spawn_blocking(|| {
                    if std::env::var("LIBGL_ALWAYS_SOFTWARE").is_ok() {
                        return true;
                    }

                    let has_dri = Path::new("/dev/dri/card0").exists()
                        || Path::new("/dev/dri/renderD128").exists();

                    if !has_dri {
                        return false;
                    }

                    if let Ok(output) = run_command("lspci", &[]) {
                        let lower = output.to_lowercase();
                        if lower.contains("vga")
                            || lower.contains("3d")
                            || lower.contains("display")
                        {
                            if lower.contains("virtio")
                                || lower.contains("qxl")
                                || lower.contains("cirrus")
                                || lower.contains("bochs")
                            {
                                return false;
                            }
                            return true;
                        }
                    }

                    true
                })
                .await
                .unwrap_or(false)
            })
            .await;

            match result {
                Ok(supported) => supported,
                Err(_) => {
                    warn!("wgpu compatibility check timed out");
                    false
                }
            }
        }

        #[cfg(not(feature = "gui"))]
        {
            false
        }
    }

    fn determine_recommendation(
        display_server: Option<&DisplayServer>,
        virtualization: Option<&VirtualizationType>,
        gpu_available: bool,
        wgpu_supported: bool,
        software_available: bool,
        gpu_info: Option<&GpuInfo>,
    ) -> (RenderingRecommendation, Option<String>) {
        if display_server.is_none() {
            return (
                RenderingRecommendation::NoGui {
                    reason: "No display server available (DISPLAY/WAYLAND_DISPLAY not set)".into(),
                    suggestion: "Run in a graphical session or use CLI: lamco-rdp-server".into(),
                },
                None,
            );
        }

        let is_virtual_gpu = gpu_info.is_some_and(|g| g.is_virtual);

        if let Some(virt) = virtualization
            && (!wgpu_supported || is_virtual_gpu)
        {
            if software_available {
                let reason = format!(
                    "Virtual machine ({:?}) with virtual GPU ({:?})",
                    virt,
                    gpu_info.map_or(&"unknown".to_string(), |g| &g.name)
                );
                return (
                    RenderingRecommendation::UseSoftware {
                        reason: reason.clone(),
                    },
                    Some(reason),
                );
            } else {
                return (
                        RenderingRecommendation::NoGui {
                            reason: format!(
                                "Virtual machine ({virt:?}) without GPU passthrough or software rendering"
                            ),
                            suggestion: "Enable 3D acceleration in VM settings, install mesa-dri-drivers, or use CLI".into(),
                        },
                        None,
                    );
            }
        }

        if gpu_available && wgpu_supported && !is_virtual_gpu {
            return (
                RenderingRecommendation::UseGpu {
                    reason: format!(
                        "Hardware GPU available: {}",
                        gpu_info.map_or("Unknown", |g| g.name.as_str())
                    ),
                },
                None,
            );
        }

        if gpu_available && !wgpu_supported && software_available {
            return (
                RenderingRecommendation::UseSoftware {
                    reason: "GPU available but not compatible with wgpu; using software rendering"
                        .into(),
                },
                Some("GPU present but wgpu incompatible".into()),
            );
        }

        if !gpu_available && software_available {
            return (
                RenderingRecommendation::UseSoftware {
                    reason: "No GPU detected; using software rendering".into(),
                },
                Some("No GPU detected".into()),
            );
        }

        (
            RenderingRecommendation::NoGui {
                reason: "No GPU and no software rendering available".into(),
                suggestion:
                    "Install mesa-dri-drivers for software rendering or use CLI: lamco-rdp-server"
                        .into(),
            },
            None,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drm_virtgpu_getparam_matches_kernel_uapi_layout() {
        // struct drm_virtgpu_getparam { __u64 param; __u64 value; } — two
        // u64s, no padding. A mismatch here means the ioctl call would read
        // or write past the kernel's expected struct bounds.
        assert_eq!(std::mem::size_of::<DrmVirtgpuGetparam>(), 16);
        assert_eq!(std::mem::align_of::<DrmVirtgpuGetparam>(), 8);
    }

    #[test]
    fn venus_capable_check_does_not_panic_without_virtio_gpu() {
        // Dev machines and CI runners have no virtio-gpu render node; this
        // must degrade to `false`, never panic.
        let _ = is_display_gpu_venus_capable();
    }
}
