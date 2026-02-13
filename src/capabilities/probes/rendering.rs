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

use std::path::Path;

use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

use super::environment::{
    detect_display_server, detect_virtualization, run_command, DisplayServer, VirtualizationType,
};
use crate::capabilities::state::ServiceLevel;

/// Rendering capabilities
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RenderingCapabilities {
    /// Is a GPU available?
    pub gpu_available: bool,

    /// GPU information if available
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
    /// GPU name/description
    pub name: String,
    /// GPU vendor
    pub vendor: GpuVendor,
    /// Driver version
    pub driver: Option<String>,
    /// Is this a virtual GPU (virtio, QXL, etc)?
    pub is_virtual: bool,
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

        let wgpu_supported = if display_server.is_some() {
            Self::test_wgpu_compatibility().await
        } else {
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

        if let Ok(output) = run_command("glxinfo", &["-B"]) {
            if let Some(info) = Self::parse_glxinfo(&output) {
                return (true, Some(info));
            }
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
        if let Ok(output) = run_command("glxinfo", &["-B"]) {
            if output.contains("llvmpipe")
                || output.contains("softpipe")
                || output.contains("swrast")
            {
                return true;
            }
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
                        if (lower.contains("vga")
                            || lower.contains("3d")
                            || lower.contains("display"))
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

        if let Some(virt) = virtualization {
            if !wgpu_supported || is_virtual_gpu {
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
