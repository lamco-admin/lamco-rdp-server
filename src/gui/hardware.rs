//! Hardware Detection Module
//!
//! Detects available GPUs and hardware encoding capabilities for VA-API and NVENC.

use std::{path::PathBuf, process::Command};

/// GPU information for hardware detection
#[derive(Debug, Clone)]
pub struct GpuInfo {
    pub name: String,
    pub device_path: Option<PathBuf>,
    pub encoder_type: String,
    pub is_available: bool,
    pub capabilities: Vec<String>,
}

impl GpuInfo {
    /// Convert to state::GpuInfo format
    pub fn to_state_gpu_info(&self) -> crate::gui::state::GpuInfo {
        crate::gui::state::GpuInfo {
            vendor: self.extract_vendor(),
            model: self.name.clone(),
            driver: self.encoder_type.clone(),
            vaapi_device: if self.encoder_type == "vaapi" {
                self.device_path.clone()
            } else {
                None
            },
            nvenc_available: self.encoder_type == "nvenc" && self.is_available,
            supports_h264: self
                .capabilities
                .iter()
                .any(|c| c.contains("H.264") || c.contains("NVENC")),
        }
    }

    fn extract_vendor(&self) -> String {
        if self.name.to_lowercase().contains("intel") {
            "Intel".to_string()
        } else if self.name.to_lowercase().contains("amd")
            || self.name.to_lowercase().contains("radeon")
        {
            "AMD".to_string()
        } else if self.name.to_lowercase().contains("nvidia") || self.encoder_type == "nvenc" {
            "NVIDIA".to_string()
        } else {
            "Unknown".to_string()
        }
    }
}

/// Detect all available GPUs with encoding capabilities
pub fn detect_gpus() -> Vec<GpuInfo> {
    let mut gpus = Vec::new();

    gpus.extend(detect_vaapi_devices());
    gpus.extend(detect_nvidia_devices());

    if gpus.is_empty() {
        gpus.push(GpuInfo {
            name: "Software Encoding (OpenH264)".to_string(),
            device_path: None,
            encoder_type: "software".to_string(),
            is_available: true,
            capabilities: vec!["H.264 (software)".to_string()],
        });
    }

    gpus
}

/// Detect VA-API capable devices
fn detect_vaapi_devices() -> Vec<GpuInfo> {
    let mut devices = Vec::new();

    let vaapi_paths = [
        "/dev/dri/renderD128",
        "/dev/dri/renderD129",
        "/dev/dri/renderD130",
        "/dev/dri/renderD131",
    ];

    for path in &vaapi_paths {
        let device_path = PathBuf::from(path);
        if device_path.exists() {
            if let Some(gpu_info) = probe_vaapi_device(&device_path) {
                devices.push(gpu_info);
            }
        }
    }

    devices
}

/// Probe a specific VA-API device for capabilities
fn probe_vaapi_device(device_path: &PathBuf) -> Option<GpuInfo> {
    let output = Command::new("vainfo")
        .arg("--display")
        .arg("drm")
        .arg("--device")
        .arg(device_path)
        .output()
        .ok()?;

    if !output.status.success() {
        // Device exists but vainfo failed - might still be usable
        return Some(GpuInfo {
            name: format!("VA-API Device ({})", device_path.display()),
            device_path: Some(device_path.clone()),
            encoder_type: "vaapi".to_string(),
            is_available: true,
            capabilities: vec!["H.264 (status unknown)".to_string()],
        });
    }

    let stdout = String::from_utf8_lossy(&output.stdout);

    let name = parse_vaapi_driver_name(&stdout)
        .unwrap_or_else(|| format!("VA-API Device ({})", device_path.display()));

    let capabilities = parse_vaapi_capabilities(&stdout);

    Some(GpuInfo {
        name,
        device_path: Some(device_path.clone()),
        encoder_type: "vaapi".to_string(),
        is_available: true,
        capabilities,
    })
}

/// Parse VA-API driver name from vainfo output
fn parse_vaapi_driver_name(output: &str) -> Option<String> {
    // Look for lines like "vainfo: Driver version: Intel iHD driver"
    for line in output.lines() {
        if line.contains("Driver version:") {
            let parts: Vec<&str> = line.split("Driver version:").collect();
            if parts.len() > 1 {
                let driver = parts[1].trim();
                if driver.contains("Intel") {
                    return Some(format!("Intel VA-API ({})", driver));
                } else if driver.contains("AMD") || driver.contains("radeon") {
                    return Some(format!("AMD VA-API ({})", driver));
                } else {
                    return Some(format!("VA-API ({})", driver));
                }
            }
        }
    }

    // Try alternate format: "libva info: va_getDriverName() returns 0"
    for line in output.lines() {
        if line.contains("vainfo: VA-API version:") {
            // Extract version
            if let Some(version_start) = line.find("version:") {
                let version = line[version_start + 8..].trim();
                return Some(format!("VA-API {}", version));
            }
        }
    }

    None
}

/// Parse VA-API capabilities from vainfo output
fn parse_vaapi_capabilities(output: &str) -> Vec<String> {
    let mut capabilities = Vec::new();

    // Look for H.264 encode profiles
    let has_h264_encode = output.contains("VAProfileH264")
        && (output.contains("VAEntrypointEncSlice") || output.contains("VAEntrypointEncSliceLP"));

    if has_h264_encode {
        if output.contains("VAProfileH264High") {
            capabilities.push("H.264 High Profile".to_string());
        }
        if output.contains("VAProfileH264Main") {
            capabilities.push("H.264 Main Profile".to_string());
        }
        if output.contains("VAProfileH264ConstrainedBaseline")
            || output.contains("VAProfileH264Baseline")
        {
            capabilities.push("H.264 Baseline Profile".to_string());
        }

        // Check for low-power encode (newer Intel)
        if output.contains("VAEntrypointEncSliceLP") {
            capabilities.push("Low-Power Encode".to_string());
        }
    }

    let has_hevc_encode = output.contains("VAProfileHEVC")
        && (output.contains("VAEntrypointEncSlice") || output.contains("VAEntrypointEncSliceLP"));

    if has_hevc_encode {
        capabilities.push("HEVC Encode".to_string());
    }

    let has_av1_encode = output.contains("VAProfileAV1") && output.contains("VAEntrypointEncSlice");

    if has_av1_encode {
        capabilities.push("AV1 Encode".to_string());
    }

    if capabilities.is_empty() {
        if output.contains("VAProfileH264") {
            capabilities.push("H.264 Decode Only".to_string());
        } else {
            capabilities.push("Limited capabilities".to_string());
        }
    }

    capabilities
}

/// Detect NVIDIA devices with NVENC support
fn detect_nvidia_devices() -> Vec<GpuInfo> {
    let mut devices = Vec::new();

    let output = Command::new("nvidia-smi")
        .arg("--query-gpu=index,name,driver_version")
        .arg("--format=csv,noheader,nounits")
        .output();

    if let Ok(output) = output {
        if output.status.success() {
            let stdout = String::from_utf8_lossy(&output.stdout);

            for line in stdout.lines() {
                let parts: Vec<&str> = line.split(',').map(|s| s.trim()).collect();
                if parts.len() >= 3 {
                    let index = parts[0];
                    let name = parts[1];
                    let driver_version = parts[2];

                    let capabilities = detect_nvenc_capabilities(index);

                    devices.push(GpuInfo {
                        name: format!("{} (Driver {})", name, driver_version),
                        device_path: Some(PathBuf::from(format!("/dev/nvidia{}", index))),
                        encoder_type: "nvenc".to_string(),
                        is_available: !capabilities.is_empty(),
                        capabilities,
                    });
                }
            }
        }
    }

    devices
}

/// Detect NVENC capabilities for a specific GPU
fn detect_nvenc_capabilities(gpu_index: &str) -> Vec<String> {
    let mut capabilities = Vec::new();

    let output = Command::new("nvidia-smi")
        .arg("-i")
        .arg(gpu_index)
        .arg("--query-gpu=encoder.stats.sessionCount,encoder.stats.averageFps")
        .arg("--format=csv,noheader,nounits")
        .output();

    if let Ok(output) = output {
        if output.status.success() {
            capabilities.push("NVENC H.264".to_string());
            capabilities.push("NVENC HEVC".to_string());

            // Check for AV1 support (RTX 40 series and newer)
            // This is a heuristic based on GPU name
            let name_output = Command::new("nvidia-smi")
                .arg("-i")
                .arg(gpu_index)
                .arg("--query-gpu=name")
                .arg("--format=csv,noheader")
                .output();

            if let Ok(name_output) = name_output {
                let name = String::from_utf8_lossy(&name_output.stdout);
                if name.contains("RTX 40") || name.contains("RTX 50") || name.contains("Ada") {
                    capabilities.push("NVENC AV1".to_string());
                }
            }
        }
    }

    // Fallback: if nvidia-smi works at all, assume basic NVENC
    if capabilities.is_empty() {
        let basic_check = Command::new("nvidia-smi")
            .arg("-i")
            .arg(gpu_index)
            .arg("-q")
            .output();

        if let Ok(output) = basic_check {
            if output.status.success() {
                capabilities.push("NVENC (capabilities unknown)".to_string());
            }
        }
    }

    capabilities
}

pub fn get_recommended_encoder() -> Option<GpuInfo> {
    let gpus = detect_gpus();

    // Prefer NVENC if available (typically faster)
    if let Some(nvenc) = gpus
        .iter()
        .find(|g| g.encoder_type == "nvenc" && g.is_available)
    {
        return Some(nvenc.clone());
    }

    if let Some(vaapi) = gpus
        .iter()
        .find(|g| g.encoder_type == "vaapi" && g.is_available)
    {
        return Some(vaapi.clone());
    }

    gpus.into_iter().find(|g| g.is_available)
}

/// Check if a specific VA-API device path is valid and has encode capability
pub fn validate_vaapi_device(device_path: &str) -> Result<(), String> {
    let path = PathBuf::from(device_path);

    if !path.exists() {
        return Err(format!("Device path does not exist: {}", device_path));
    }

    let output = Command::new("vainfo")
        .arg("--display")
        .arg("drm")
        .arg("--device")
        .arg(&path)
        .output();

    match output {
        Ok(result) => {
            if result.status.success() {
                let stdout = String::from_utf8_lossy(&result.stdout);
                if stdout.contains("VAEntrypointEncSlice")
                    || stdout.contains("VAEntrypointEncSliceLP")
                {
                    Ok(())
                } else {
                    Err("Device does not support H.264 encoding".to_string())
                }
            } else {
                let stderr = String::from_utf8_lossy(&result.stderr);
                Err(format!("vainfo failed: {}", stderr.trim()))
            }
        }
        Err(e) => Err(format!("Failed to run vainfo: {}", e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_gpus_returns_at_least_software() {
        let gpus = detect_gpus();
        assert!(
            !gpus.is_empty(),
            "Should at least return software encoder option"
        );
    }

    #[test]
    fn test_parse_vaapi_capabilities() {
        let sample_output = r#"
vainfo: VA-API version: 1.18 (libva 2.18.0)
vainfo: Driver version: Intel iHD driver for Intel(R) Gen Graphics - 23.2.4
vainfo: Supported profile and entrypoints
      VAProfileH264ConstrainedBaseline: VAEntrypointVLD
      VAProfileH264ConstrainedBaseline: VAEntrypointEncSlice
      VAProfileH264ConstrainedBaseline: VAEntrypointEncSliceLP
      VAProfileH264Main               : VAEntrypointVLD
      VAProfileH264Main               : VAEntrypointEncSlice
      VAProfileH264Main               : VAEntrypointEncSliceLP
      VAProfileH264High               : VAEntrypointVLD
      VAProfileH264High               : VAEntrypointEncSlice
      VAProfileH264High               : VAEntrypointEncSliceLP
"#;

        let caps = parse_vaapi_capabilities(sample_output);
        assert!(caps.contains(&"H.264 High Profile".to_string()));
        assert!(caps.contains(&"Low-Power Encode".to_string()));
    }
}
