//! Capability probes for each subsystem.

mod display;
mod encoding;
mod input;
mod network;
mod rendering;
mod storage;

use std::{path::Path, process::Command};

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

/// Environment detection (virtualization, display server, command availability)
pub mod environment {
    use super::{Command, Path};

    pub fn path_exists(path: impl AsRef<Path>) -> bool {
        path.as_ref().exists()
    }

    pub fn command_available(cmd: &str) -> bool {
        Command::new("which")
            .arg(cmd)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    pub fn run_command(cmd: &str, args: &[&str]) -> Result<String, std::io::Error> {
        let output = Command::new(cmd).args(args).output()?;

        if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout).to_string())
        } else {
            Err(std::io::Error::other(
                String::from_utf8_lossy(&output.stderr).to_string(),
            ))
        }
    }

    #[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
    pub enum VirtualizationType {
        None,
        Qemu,
        Kvm,
        VirtualBox,
        VMware,
        HyperV,
        Xen,
        Docker,
        Podman,
        Lxc,
        Other(String),
    }

    impl VirtualizationType {
        pub fn has_gpu_passthrough(&self) -> bool {
            matches!(self, Self::None)
        }

        pub fn is_virtualized(&self) -> bool {
            !matches!(self, Self::None)
        }
    }

    pub fn detect_virtualization() -> VirtualizationType {
        // Most reliable detection method
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

    #[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
    pub enum DisplayServer {
        Wayland,
        X11,
    }

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
