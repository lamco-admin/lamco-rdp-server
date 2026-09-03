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
#[cfg(feature = "vulkan-video")]
use super::vulkan::VulkanVideoEncoder;
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
    #[cfg(not(any(feature = "vaapi", feature = "nvenc", feature = "vulkan-video")))]
    {
        return Err(HardwareEncoderError::NoBackendAvailable {
            reason: "No hardware encoding features enabled at compile time. \
                     Enable 'vaapi', 'nvenc', and/or 'vulkan-video' features."
                .to_string(),
        });
    }

    // Parse quality preset
    let preset = QualityPreset::from_name(&config.quality_preset).unwrap_or_else(|| {
        warn!(
            "Invalid quality preset '{}', using 'balanced'",
            config.quality_preset
        );
        QualityPreset::Balanced
    });

    debug!(
        "Creating hardware encoder: {}x{}, preset={}, backends={:?}",
        width, height, preset, config.backend_priority
    );

    let mut errors: Vec<String> = Vec::new();

    // Use backend_priority list for ordered selection
    for backend in &config.backend_priority {
        match backend.as_str() {
            #[cfg(feature = "vulkan-video")]
            "vulkan-video" => match try_vulkan_video(config, width, height, preset) {
                Ok(encoder) => return Ok(encoder),
                Err(e) => {
                    debug!("Vulkan Video initialization failed: {}", e);
                    errors.push(format!("Vulkan Video: {e}"));
                }
            },
            #[cfg(feature = "nvenc")]
            "nvenc" => match try_nvenc(config, width, height, preset) {
                Ok(encoder) => return Ok(encoder),
                Err(e) => {
                    debug!("NVENC initialization failed: {}", e);
                    errors.push(format!("NVENC: {e}"));
                }
            },
            #[cfg(feature = "vaapi")]
            "vaapi" => match try_vaapi(config, width, height, preset) {
                Ok(encoder) => return Ok(encoder),
                Err(e) => {
                    debug!("VA-API initialization failed: {}", e);
                    errors.push(format!("VA-API: {e}"));
                }
            },
            other => {
                debug!("Unknown backend '{}' in priority list, skipping", other);
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

/// Try to create a Vulkan Video encoder
#[cfg(feature = "vulkan-video")]
fn try_vulkan_video(
    config: &HardwareEncodingConfig,
    width: u32,
    height: u32,
    preset: QualityPreset,
) -> HardwareEncoderResult<Box<dyn HardwareEncoder>> {
    info!("Attempting Vulkan Video encoder: {}x{}", width, height);

    let encoder = VulkanVideoEncoder::new(config, width, height, preset)?;

    info!("Vulkan Video encoder initialized: {}x{}", width, height);

    Ok(Box::new(encoder))
}

#[cfg(test)]
mod tests {
    #[cfg(not(any(feature = "vaapi", feature = "nvenc")))]
    use std::path::PathBuf;

    use super::*;

    #[cfg(not(any(feature = "vaapi", feature = "nvenc")))]
    fn test_config() -> HardwareEncodingConfig {
        HardwareEncodingConfig {
            enabled: true,
            vaapi_device: PathBuf::from("/dev/dri/renderD128"),
            enable_dmabuf_zerocopy: false,
            fallback_to_software: true,
            quality_preset: "balanced".to_string(),
            prefer_nvenc: true,
            ..HardwareEncodingConfig::default()
        }
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
        assert_eq!(
            QualityPreset::from_name("speed"),
            Some(QualityPreset::Speed)
        );
        assert_eq!(QualityPreset::from_name("invalid"), None);
    }
}
