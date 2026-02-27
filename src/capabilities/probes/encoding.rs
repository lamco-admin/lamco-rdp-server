//! Video encoding capability probe
//!
//! Detects available video encoders including hardware (VA-API, NVENC) and
//! software (OpenH264) options.

use std::{path::Path, time::Instant};

use serde::{Deserialize, Serialize};
use tracing::{debug, info};

use super::environment::run_command;
use crate::capabilities::{fallback::AttemptResult, state::ServiceLevel};

/// Encoding capabilities
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EncodingCapabilities {
    /// All detected encoder backends
    pub backends: Vec<EncoderBackend>,

    /// Selected backend for use
    pub selected: Option<EncoderBackend>,

    /// Is software encoding available?
    pub software_available: bool,

    /// Is hardware encoding available?
    pub hardware_available: bool,

    /// Overall service level
    pub service_level: ServiceLevel,

    /// Fallback chain attempts
    pub fallback_chain: Vec<AttemptResult>,
}

/// An encoder backend
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EncoderBackend {
    /// Type of encoder
    pub backend_type: EncoderBackendType,
    /// Device path if applicable
    pub device: Option<String>,
    /// Encoding capabilities
    pub capabilities: EncoderCapabilities,
    /// Service level this backend provides
    pub service_level: ServiceLevel,
}

/// Type of encoder backend
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum EncoderBackendType {
    /// VA-API hardware encoder
    VaApi {
        /// VA-API driver name
        driver: String,
    },
    /// NVIDIA NVENC hardware encoder
    Nvenc {
        /// GPU name
        gpu: String,
    },
    /// OpenH264 software encoder
    OpenH264,
}

impl EncoderBackendType {
    pub fn name(&self) -> &str {
        match self {
            Self::VaApi { .. } => "VA-API",
            Self::Nvenc { .. } => "NVENC",
            Self::OpenH264 => "OpenH264",
        }
    }

    pub fn is_hardware(&self) -> bool {
        matches!(self, Self::VaApi { .. } | Self::Nvenc { .. })
    }
}

/// Encoder capabilities
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EncoderCapabilities {
    /// Supports H.264 encoding
    pub h264: bool,
    /// Supported H.264 profiles
    pub h264_profiles: Vec<String>,
    /// Supports HEVC/H.265 encoding
    pub hevc: bool,
    /// Supports AV1 encoding
    pub av1: bool,
    /// Maximum supported resolution
    pub max_resolution: (u32, u32),
    /// Maximum supported frame rate
    pub max_fps: u32,
}

/// Encoding probe
pub struct EncodingProbe;

impl EncodingProbe {
    pub async fn probe() -> EncodingCapabilities {
        info!("Probing encoding capabilities...");

        let mut backends = Vec::new();
        let mut fallback_chain = Vec::new();

        let (vaapi_result, vaapi_attempt) = Self::probe_vaapi();
        fallback_chain.push(vaapi_attempt);
        if let Some(vaapi) = vaapi_result {
            backends.push(vaapi);
        }

        let (nvenc_result, nvenc_attempt) = Self::probe_nvenc();
        fallback_chain.push(nvenc_attempt);
        if let Some(nvenc) = nvenc_result {
            backends.push(nvenc);
        }

        let (openh264_result, openh264_attempt) = Self::probe_openh264();
        fallback_chain.push(openh264_attempt);
        if let Some(openh264) = openh264_result {
            backends.push(openh264);
        }

        let hardware_available = backends.iter().any(|b| b.backend_type.is_hardware());

        let software_available = backends
            .iter()
            .any(|b| matches!(b.backend_type, EncoderBackendType::OpenH264));

        let selected = backends.first().cloned();

        let service_level = if hardware_available {
            ServiceLevel::Full
        } else if software_available {
            ServiceLevel::Fallback
        } else {
            ServiceLevel::Unavailable
        };

        info!(
            "Encoding service level: {:?}, backends: {}, hardware: {}, software: {}",
            service_level,
            backends.len(),
            hardware_available,
            software_available
        );

        EncodingCapabilities {
            backends,
            selected,
            software_available,
            hardware_available,
            service_level,
            fallback_chain,
        }
    }

    fn probe_vaapi() -> (Option<EncoderBackend>, AttemptResult) {
        let start = Instant::now();

        for i in 128..=135 {
            let device = format!("/dev/dri/renderD{i}");
            if !Path::new(&device).exists() {
                continue;
            }

            match run_command("vainfo", &["--display", "drm", "--device", &device]) {
                Ok(output) => {
                    let driver = Self::parse_vaapi_driver(&output);
                    let caps = Self::parse_vaapi_caps(&output);

                    if caps.h264 {
                        debug!("VA-API found on {}: driver={}", device, driver);
                        return (
                            Some(EncoderBackend {
                                backend_type: EncoderBackendType::VaApi {
                                    driver: driver.clone(),
                                },
                                device: Some(device),
                                capabilities: caps,
                                service_level: ServiceLevel::Full,
                            }),
                            AttemptResult {
                                strategy_name: "VA-API".into(),
                                success: true,
                                error: None,
                                duration_ms: start.elapsed().as_millis() as u64,
                            },
                        );
                    }
                }
                Err(e) => {
                    debug!("vainfo failed for {}: {}", device, e);
                }
            }
        }

        (
            None,
            AttemptResult {
                strategy_name: "VA-API".into(),
                success: false,
                error: Some("No VA-API devices with H.264 encoding support found".into()),
                duration_ms: start.elapsed().as_millis() as u64,
            },
        )
    }

    fn parse_vaapi_driver(output: &str) -> String {
        for line in output.lines() {
            if line.contains("Driver version:") || line.contains("vainfo:") {
                if let Some(driver) = line.split(':').nth(1) {
                    return driver.trim().to_string();
                }
            }
        }
        "unknown".to_string()
    }

    fn parse_vaapi_caps(output: &str) -> EncoderCapabilities {
        let mut caps = EncoderCapabilities {
            max_resolution: (4096, 4096),
            max_fps: 60,
            ..EncoderCapabilities::default()
        };

        let _output_lower = output.to_lowercase();

        for line in output.lines() {
            let line_lower = line.to_lowercase();

            // Only match encode entrypoints, not decode
            if (line_lower.contains("h264") || line_lower.contains("h.264"))
                && (line_lower.contains("encslice") || line_lower.contains("enc"))
            {
                caps.h264 = true;

                if line_lower.contains("main") && !caps.h264_profiles.contains(&"main".to_string())
                {
                    caps.h264_profiles.push("main".into());
                }
                if line_lower.contains("high") && !caps.h264_profiles.contains(&"high".to_string())
                {
                    caps.h264_profiles.push("high".into());
                }
                if (line_lower.contains("baseline") || line_lower.contains("constrained"))
                    && !caps.h264_profiles.contains(&"baseline".to_string())
                {
                    caps.h264_profiles.push("baseline".into());
                }
            }

            if (line_lower.contains("hevc") || line_lower.contains("h.265"))
                && (line_lower.contains("encslice") || line_lower.contains("enc"))
            {
                caps.hevc = true;
            }

            if line_lower.contains("av1") && line_lower.contains("enc") {
                caps.av1 = true;
            }
        }

        // Default profiles if H.264 detected but no profiles parsed
        if caps.h264 && caps.h264_profiles.is_empty() {
            caps.h264_profiles = vec!["baseline".into(), "main".into(), "high".into()];
        }

        caps
    }

    fn probe_nvenc() -> (Option<EncoderBackend>, AttemptResult) {
        let start = Instant::now();

        if !Path::new("/dev/nvidia0").exists() {
            return (
                None,
                AttemptResult {
                    strategy_name: "NVENC".into(),
                    success: false,
                    error: Some("No NVIDIA device (/dev/nvidia0 not found)".into()),
                    duration_ms: start.elapsed().as_millis() as u64,
                },
            );
        }

        match run_command(
            "nvidia-smi",
            &["--query-gpu=name,driver_version", "--format=csv,noheader"],
        ) {
            Ok(output) => {
                let gpu_name = output
                    .lines()
                    .next()
                    .unwrap_or("Unknown NVIDIA GPU")
                    .trim()
                    .to_string();

                debug!("NVENC found: {}", gpu_name);

                (
                    Some(EncoderBackend {
                        backend_type: EncoderBackendType::Nvenc { gpu: gpu_name },
                        device: Some("/dev/nvidia0".into()),
                        capabilities: EncoderCapabilities {
                            h264: true,
                            h264_profiles: vec!["baseline".into(), "main".into(), "high".into()],
                            hevc: true,
                            av1: false, // Depends on GPU generation
                            max_resolution: (8192, 8192),
                            max_fps: 120,
                        },
                        service_level: ServiceLevel::Full,
                    }),
                    AttemptResult {
                        strategy_name: "NVENC".into(),
                        success: true,
                        error: None,
                        duration_ms: start.elapsed().as_millis() as u64,
                    },
                )
            }
            Err(e) => (
                None,
                AttemptResult {
                    strategy_name: "NVENC".into(),
                    success: false,
                    error: Some(format!("nvidia-smi failed: {e}")),
                    duration_ms: start.elapsed().as_millis() as u64,
                },
            ),
        }
    }

    fn probe_openh264() -> (Option<EncoderBackend>, AttemptResult) {
        let start = Instant::now();

        #[cfg(feature = "h264")]
        {
            debug!("OpenH264 software encoder available (h264 feature enabled)");
            (
                Some(EncoderBackend {
                    backend_type: EncoderBackendType::OpenH264,
                    device: None,
                    capabilities: EncoderCapabilities {
                        h264: true,
                        h264_profiles: vec!["baseline".into(), "main".into(), "high".into()],
                        hevc: false,
                        av1: false,
                        max_resolution: (4096, 4096),
                        max_fps: 60,
                    },
                    service_level: ServiceLevel::Fallback,
                }),
                AttemptResult {
                    strategy_name: "OpenH264".into(),
                    success: true,
                    error: None,
                    duration_ms: start.elapsed().as_millis() as u64,
                },
            )
        }

        #[cfg(not(feature = "h264"))]
        {
            (
                None,
                AttemptResult {
                    strategy_name: "OpenH264".into(),
                    success: false,
                    error: Some("h264 feature not compiled in".into()),
                    duration_ms: start.elapsed().as_millis() as u64,
                },
            )
        }
    }
}
