//! Capability probes for each subsystem
//!
//! This module contains specialized probes for detecting capabilities
//! in different subsystems.

mod display;
mod encoding;
mod input;
mod network;
mod rendering;
mod storage;

pub use display::{CompositorInfo, DisplayCapabilities, DisplayProbe, PortalInfo};
pub use encoding::{
    EncoderBackend, EncoderBackendType, EncoderCapabilities, EncodingCapabilities, EncodingProbe,
};
pub use input::{DeploymentType, InputCapabilities, InputProbe, InputStrategy, InputStrategyType};
pub use network::{NetworkCapabilities, NetworkProbe};
pub use rendering::{
    GpuInfo, GpuVendor, RenderingCapabilities, RenderingProbe, RenderingRecommendation,
};
pub use storage::{StorageBackend, StorageCapabilities, StorageProbe};

use std::path::Path;
use std::process::Command;

/// Common probe utilities
pub mod utils {
    use super::*;

    /// Check if a path exists
    pub fn path_exists(path: impl AsRef<Path>) -> bool {
        path.as_ref().exists()
    }

    /// Check if a command is available
    pub fn command_available(cmd: &str) -> bool {
        Command::new("which")
            .arg(cmd)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    /// Run a command and capture output
    pub fn run_command(cmd: &str, args: &[&str]) -> Result<String, std::io::Error> {
        let output = Command::new(cmd).args(args).output()?;

        if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout).to_string())
        } else {
            Err(std::io::Error::new(
                std::io::ErrorKind::Other,
                String::from_utf8_lossy(&output.stderr).to_string(),
            ))
        }
    }

    /// Detect virtualization type
    #[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
    pub enum VirtualizationType {
        /// Not running in a VM
        None,
        /// QEMU (often with KVM)
        Qemu,
        /// KVM hypervisor
        Kvm,
        /// Oracle VirtualBox
        VirtualBox,
        /// VMware products
        VMware,
        /// Microsoft Hyper-V
        HyperV,
        /// Xen hypervisor
        Xen,
        /// Docker container
        Docker,
        /// Podman container
        Podman,
        /// LXC/LXD container
        Lxc,
        /// Other virtualization
        Other(String),
    }

    impl VirtualizationType {
        /// Check if this virtualization typically has GPU passthrough
        pub fn has_gpu_passthrough(&self) -> bool {
            // Can't reliably determine this; assume no for safety
            matches!(self, Self::None)
        }

        /// Check if this is any form of virtualization
        pub fn is_virtualized(&self) -> bool {
            !matches!(self, Self::None)
        }
    }

    /// Detect what type of virtualization we're running under
    pub fn detect_virtualization() -> VirtualizationType {
        // Try systemd-detect-virt first (most reliable)
        if let Ok(output) = run_command("systemd-detect-virt", &[]) {
            let virt = output.trim().to_lowercase();
            return match virt.as_str() {
                "none" => VirtualizationType::None,
                "qemu" => VirtualizationType::Qemu,
                "kvm" => VirtualizationType::Kvm,
                "virtualbox" | "oracle" => VirtualizationType::VirtualBox,
                "vmware" => VirtualizationType::VMware,
                "microsoft" | "hyper-v" => VirtualizationType::HyperV,
                "xen" => VirtualizationType::Xen,
                "docker" => VirtualizationType::Docker,
                "podman" => VirtualizationType::Podman,
                "lxc" | "lxc-libvirt" => VirtualizationType::Lxc,
                other if !other.is_empty() => VirtualizationType::Other(other.to_string()),
                _ => VirtualizationType::None,
            };
        }

        // Fallback: check DMI
        if let Ok(content) = std::fs::read_to_string("/sys/devices/virtual/dmi/id/product_name") {
            let content = content.to_lowercase();
            if content.contains("virtualbox") {
                return VirtualizationType::VirtualBox;
            }
            if content.contains("vmware") {
                return VirtualizationType::VMware;
            }
            if content.contains("qemu") || content.contains("kvm") {
                return VirtualizationType::Qemu;
            }
        }

        // Check for container
        if Path::new("/.dockerenv").exists() {
            return VirtualizationType::Docker;
        }
        if let Ok(content) = std::fs::read_to_string("/proc/1/cgroup") {
            if content.contains("docker") {
                return VirtualizationType::Docker;
            }
            if content.contains("lxc") {
                return VirtualizationType::Lxc;
            }
        }

        VirtualizationType::None
    }

    /// Detect display server
    #[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
    pub enum DisplayServer {
        /// Wayland compositor
        Wayland,
        /// X11 server
        X11,
    }

    /// Detect which display server is in use
    pub fn detect_display_server() -> Option<DisplayServer> {
        if std::env::var("WAYLAND_DISPLAY").is_ok() {
            Some(DisplayServer::Wayland)
        } else if std::env::var("DISPLAY").is_ok() {
            Some(DisplayServer::X11)
        } else {
            None
        }
    }
}
