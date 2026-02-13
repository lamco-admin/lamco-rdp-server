//! Hardware encoder factory with automatic backend selection
//!
//! This module provides the `create_hardware_encoder()` function which
//! automatically selects and initializes the best available hardware
//! encoding backend based on system capabilities and configuration.
//!
//! # Backend Priority
//!
//! By default, NVENC is preferred over VA-API when both are available
//! because NVENC typically offers lower latency. This can be overridden
//! via `HardwareEncodingConfig::prefer_nvenc`.
//!
//! # Fallback Behavior
//!
//! If a backend fails to initialize (e.g., no GPU, driver issues),
//! the factory tries the next available backend. If all backends fail,
//! an error is returned describing why each failed.

use tracing::{debug, info, warn};

#[cfg(feature = "nvenc")]
use super::nvenc::NvencEncoder;
#[cfg(feature = "vaapi")]
use super::vaapi::VaapiEncoder;
use super::{HardwareEncoder, HardwareEncoderError, HardwareEncoderResult, QualityPreset};
use crate::config::HardwareEncodingConfig;

/// Create a hardware encoder with automatic backend selection
///
/// Tries available backends in priority order and returns the first
/// one that successfully initializes. Returns an error if no backend
/// is available or all backends fail.
///
/// # Priority Order
///
/// 1. NVENC (if `prefer_nvenc` is true or VA-API unavailable)
/// 2. VA-API
/// 3. NVENC (fallback if VA-API preferred but failed)
///
/// # Example
///
/// ```rust,ignore
/// use lamco_rdp_server::config::HardwareEncodingConfig;
/// use lamco_rdp_server::egfx::hardware::create_hardware_encoder;
///
/// let config = HardwareEncodingConfig::default();
/// let encoder = create_hardware_encoder(&config, 1920, 1080)?;
/// println!("Using {} backend", encoder.backend_name());
/// ```
pub fn create_hardware_encoder(
    config: &HardwareEncodingConfig,
    width: u32,
    height: u32,
) -> HardwareEncoderResult<Box<dyn HardwareEncoder>> {
    // Check compile-time feature availability
    #[cfg(not(any(feature = "vaapi", feature = "nvenc")))]
    {
        return Err(HardwareEncoderError::NoBackendAvailable {
            reason: "No hardware encoding features enabled at compile time. \
                     Enable 'vaapi' and/or 'nvenc' features."
                .to_string(),
        });
    }

    // Parse quality preset
    let preset = QualityPreset::from_str(&config.quality_preset).unwrap_or_else(|| {
        warn!(
            "Invalid quality preset '{}', using 'balanced'",
            config.quality_preset
        );
        QualityPreset::Balanced
    });

    debug!(
        "Creating hardware encoder: {}x{}, preset={}, prefer_nvenc={}",
        width, height, preset, config.prefer_nvenc
    );

    let mut errors: Vec<String> = Vec::new();

    // Determine backend order based on preference
    let try_nvenc_first = config.prefer_nvenc;

    // First attempt based on preference
    #[cfg(feature = "nvenc")]
    if try_nvenc_first {
        match try_nvenc(config, width, height, preset) {
            Ok(encoder) => return Ok(encoder),
            Err(e) => {
                debug!("NVENC initialization failed: {}", e);
                errors.push(format!("NVENC: {}", e));
            }
        }
    }

    // Try VA-API
    #[cfg(feature = "vaapi")]
    {
        match try_vaapi(config, width, height, preset) {
            Ok(encoder) => return Ok(encoder),
            Err(e) => {
                debug!("VA-API initialization failed: {}", e);
                errors.push(format!("VA-API: {}", e));
            }
        }
    }

    // Try NVENC as fallback if not tried first
    #[cfg(feature = "nvenc")]
    if !try_nvenc_first {
        match try_nvenc(config, width, height, preset) {
            Ok(encoder) => return Ok(encoder),
            Err(e) => {
                debug!("NVENC initialization failed: {}", e);
                errors.push(format!("NVENC: {}", e));
            }
        }
    }

    // All backends failed
    let reason = if errors.is_empty() {
        "No hardware encoding features enabled".to_string()
    } else {
        errors.join("; ")
    };

    Err(HardwareEncoderError::NoBackendAvailable { reason })
}

/// Try to create a VA-API encoder
#[cfg(feature = "vaapi")]
fn try_vaapi(
    config: &HardwareEncodingConfig,
    width: u32,
    height: u32,
    preset: QualityPreset,
) -> HardwareEncoderResult<Box<dyn HardwareEncoder>> {
    info!(
        "Attempting VA-API encoder: {}x{}, device={:?}",
        width, height, config.vaapi_device
    );

    let encoder = VaapiEncoder::new(config, width, height, preset)?;

    info!(
        "✅ VA-API encoder initialized: driver={}, {}x{}",
        encoder.driver_name().unwrap_or("unknown"),
        width,
        height
    );

    Ok(Box::new(encoder))
}

/// Try to create an NVENC encoder
#[cfg(feature = "nvenc")]
fn try_nvenc(
    config: &HardwareEncodingConfig,
    width: u32,
    height: u32,
    preset: QualityPreset,
) -> HardwareEncoderResult<Box<dyn HardwareEncoder>> {
    info!("Attempting NVENC encoder: {}x{}", width, height);

    let encoder = NvencEncoder::new(config, width, height, preset)?;

    info!("✅ NVENC encoder initialized: {}x{}", width, height);

    Ok(Box::new(encoder))
}

/// Performs quick checks without actually initializing an encoder.
///
/// Returns (vaapi_available, nvenc_available)
pub fn probe_backends() -> (bool, bool) {
    let vaapi = probe_vaapi();
    let nvenc = probe_nvenc();

    debug!("Hardware encoding probe: vaapi={}, nvenc={}", vaapi, nvenc);

    (vaapi, nvenc)
}

/// Quick probe for VA-API availability
#[cfg(feature = "vaapi")]
fn probe_vaapi() -> bool {
    use std::path::Path;

    // Check if render device exists
    let render_devices = [
        "/dev/dri/renderD128",
        "/dev/dri/renderD129",
        "/dev/dri/renderD130",
    ];

    for device in &render_devices {
        if Path::new(device).exists() {
            debug!("VA-API: Found render device {}", device);
            return true;
        }
    }

    debug!("VA-API: No render devices found");
    false
}

#[cfg(not(feature = "vaapi"))]
fn probe_vaapi() -> bool {
    false
}

/// Quick probe for NVENC availability
#[cfg(feature = "nvenc")]
fn probe_nvenc() -> bool {
    use std::path::Path;

    // Check for NVIDIA driver presence
    let nvidia_indicators = [
        "/dev/nvidia0",
        "/dev/nvidiactl",
        "/proc/driver/nvidia/version",
    ];

    for indicator in &nvidia_indicators {
        if Path::new(indicator).exists() {
            debug!("NVENC: Found NVIDIA indicator {}", indicator);
            return true;
        }
    }

    debug!("NVENC: No NVIDIA driver indicators found");
    false
}

#[cfg(not(feature = "nvenc"))]
fn probe_nvenc() -> bool {
    false
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn test_config() -> HardwareEncodingConfig {
        HardwareEncodingConfig {
            enabled: true,
            vaapi_device: PathBuf::from("/dev/dri/renderD128"),
            enable_dmabuf_zerocopy: false,
            fallback_to_software: true,
            quality_preset: "balanced".to_string(),
            prefer_nvenc: true,
        }
    }

    #[test]
    fn test_probe_backends() {
        let (vaapi, nvenc) = probe_backends();
        // Just check it doesn't panic
        println!("Probe results: vaapi={}, nvenc={}", vaapi, nvenc);
    }

    #[test]
    #[cfg(not(any(feature = "vaapi", feature = "nvenc")))]
    fn test_no_backend_error() {
        let config = test_config();
        let result = create_hardware_encoder(&config, 1920, 1080);
        assert!(matches!(
            result,
            Err(HardwareEncoderError::NoBackendAvailable { .. })
        ));
    }

    #[test]
    fn test_quality_preset_parsing() {
        assert_eq!(QualityPreset::from_str("speed"), Some(QualityPreset::Speed));
        assert_eq!(QualityPreset::from_str("invalid"), None);
    }
}
