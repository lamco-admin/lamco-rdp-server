//! Configuration Validation Module
//!
//! Validates configuration parameters and provides detailed error/warning messages.

use std::{net::SocketAddr, path::Path};

use crate::{
    config::Config,
    gui::state::{ValidationError, ValidationResult, ValidationWarning},
};

/// Validate a complete configuration
pub fn validate_config(config: &Config) -> ValidationResult {
    let mut errors = Vec::new();
    let mut warnings = Vec::new();

    validate_server_config(config, &mut errors, &mut warnings);
    validate_security_config(config, &mut errors, &mut warnings);
    validate_video_config(config, &mut errors, &mut warnings);
    validate_input_config(config, &mut errors, &mut warnings);
    validate_clipboard_config(config, &mut errors, &mut warnings);
    validate_performance_config(config, &mut errors, &mut warnings);
    validate_egfx_config(config, &mut errors, &mut warnings);
    validate_damage_tracking_config(config, &mut errors, &mut warnings);
    validate_hardware_encoding_config(config, &mut errors, &mut warnings);
    validate_display_config(config, &mut errors, &mut warnings);
    validate_logging_config(config, &mut errors, &mut warnings);
    validate_cross_section(config, &mut errors, &mut warnings);

    ValidationResult {
        is_valid: errors.is_empty(),
        errors,
        warnings,
    }
}

/// Validate server configuration
fn validate_server_config(
    config: &Config,
    errors: &mut Vec<ValidationError>,
    warnings: &mut Vec<ValidationWarning>,
) {
    if config.server.listen_addr.parse::<SocketAddr>().is_err() {
        errors.push(ValidationError {
            field: "server.listen_addr".to_string(),
            message: format!("Invalid listen address: '{}'", config.server.listen_addr),
        });
    }

    if let Ok(addr) = config.server.listen_addr.parse::<SocketAddr>() {
        if addr.port() < 1024 {
            warnings.push(ValidationWarning {
                field: "server.listen_addr".to_string(),
                message: format!(
                    "Port {} requires root privileges on most systems",
                    addr.port()
                ),
            });
        }
    }

    if config.server.max_connections == 0 {
        errors.push(ValidationError {
            field: "server.max_connections".to_string(),
            message: "max_connections must be at least 1".to_string(),
        });
    } else if config.server.max_connections > 100 {
        warnings.push(ValidationWarning {
            field: "server.max_connections".to_string(),
            message: "More than 100 connections may impact performance".to_string(),
        });
    }
}

/// Validate security configuration
fn validate_security_config(
    config: &Config,
    errors: &mut Vec<ValidationError>,
    warnings: &mut Vec<ValidationWarning>,
) {
    if !config.security.cert_path.exists() {
        errors.push(ValidationError {
            field: "security.cert_path".to_string(),
            message: format!(
                "Certificate file not found: {}",
                config.security.cert_path.display()
            ),
        });
    } else {
        if let Err(e) = validate_pem_file(&config.security.cert_path, "CERTIFICATE") {
            errors.push(ValidationError {
                field: "security.cert_path".to_string(),
                message: e,
            });
        }
    }

    if !config.security.key_path.exists() {
        errors.push(ValidationError {
            field: "security.key_path".to_string(),
            message: format!(
                "Private key file not found: {}",
                config.security.key_path.display()
            ),
        });
    } else {
        if let Err(e) = validate_pem_file(&config.security.key_path, "PRIVATE KEY") {
            errors.push(ValidationError {
                field: "security.key_path".to_string(),
                message: e,
            });
        }

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Ok(metadata) = std::fs::metadata(&config.security.key_path) {
                let mode = metadata.permissions().mode();
                if mode & 0o077 != 0 {
                    warnings.push(ValidationWarning {
                        field: "security.key_path".to_string(),
                        message:
                            "Private key file has permissive permissions. Recommended: chmod 600"
                                .to_string(),
                    });
                }
            }
        }
    }

    match config.security.auth_method.as_str() {
        "pam" | "none" | "password" => {}
        _ => {
            errors.push(ValidationError {
                field: "security.auth_method".to_string(),
                message: format!(
                    "Invalid auth method: '{}'. Valid options: pam, none, password",
                    config.security.auth_method
                ),
            });
        }
    }

    // Note: NLA disabled is the default (required for no-auth mode and Flatpak)
    // Only warn if PAM auth is enabled but NLA is disabled
    if config.security.auth_method == "pam" && !config.security.enable_nla {
        warnings.push(ValidationWarning {
            field: "security.enable_nla".to_string(),
            message: "PAM authentication enabled but NLA is disabled. Consider enabling NLA for better security.".to_string(),
        });
    }

    // Note: TLS 1.3 requirement disabled is the default for compatibility
    // This is informational, not a security warning for most use cases
}

/// Validate video configuration
fn validate_video_config(
    config: &Config,
    errors: &mut Vec<ValidationError>,
    warnings: &mut Vec<ValidationWarning>,
) {
    // Note: Encoder and bitrate are in hardware_encoding and egfx sections

    if config.video.target_fps == 0 {
        errors.push(ValidationError {
            field: "video.target_fps".to_string(),
            message: "target_fps must be at least 1".to_string(),
        });
    } else if config.video.target_fps > 120 {
        warnings.push(ValidationWarning {
            field: "video.target_fps".to_string(),
            message: "FPS above 120 may cause excessive CPU/bandwidth usage".to_string(),
        });
    }

    match config.video.cursor_mode.as_str() {
        "embedded" | "metadata" | "hidden" => {}
        _ => {
            errors.push(ValidationError {
                field: "video.cursor_mode".to_string(),
                message: format!(
                    "Invalid cursor mode: '{}'. Valid options: embedded, metadata, hidden",
                    config.video.cursor_mode
                ),
            });
        }
    }
}

/// Validate input configuration
fn validate_input_config(
    config: &Config,
    _errors: &mut [ValidationError],
    warnings: &mut Vec<ValidationWarning>,
) {
    let valid_layouts = [
        "auto", "us", "gb", "de", "fr", "es", "it", "pt", "nl", "pl", "ru", "jp", "kr", "cn",
    ];
    if config.input.keyboard_layout != "auto"
        && !valid_layouts.contains(&config.input.keyboard_layout.as_str())
    {
        warnings.push(ValidationWarning {
            field: "input.keyboard_layout".to_string(),
            message: format!(
                "Unknown keyboard layout: '{}'. Common values: {}",
                config.input.keyboard_layout,
                valid_layouts.join(", ")
            ),
        });
    }

    // Warn about libei requirement for wlroots
    if !config.input.use_libei {
        warnings.push(ValidationWarning {
            field: "input.use_libei".to_string(),
            message: "libei is disabled. This may cause input issues on wlroots compositors."
                .to_string(),
        });
    }
}

/// Validate clipboard configuration
fn validate_clipboard_config(
    config: &Config,
    _errors: &mut [ValidationError],
    warnings: &mut Vec<ValidationWarning>,
) {
    if config.clipboard.max_size > 100 * 1024 * 1024 {
        warnings.push(ValidationWarning {
            field: "clipboard.max_size".to_string(),
            message: "Clipboard max size above 100 MB may cause memory issues".to_string(),
        });
    }

    if config.clipboard.rate_limit_ms < 10 {
        warnings.push(ValidationWarning {
            field: "clipboard.rate_limit_ms".to_string(),
            message: "Rate limit below 10ms may cause performance issues".to_string(),
        });
    }
}

/// Validate performance configuration
fn validate_performance_config(
    config: &Config,
    _errors: &mut Vec<ValidationError>,
    warnings: &mut Vec<ValidationWarning>,
) {
    if config.performance.encoder_threads > 32 {
        warnings.push(ValidationWarning {
            field: "performance.encoder_threads".to_string(),
            message: "More than 32 encoder threads rarely improves performance".to_string(),
        });
    }

    if config.performance.network_threads > 16 {
        warnings.push(ValidationWarning {
            field: "performance.network_threads".to_string(),
            message: "More than 16 network threads rarely improves performance".to_string(),
        });
    }

    if config.performance.buffer_pool_size < 4 {
        warnings.push(ValidationWarning {
            field: "performance.buffer_pool_size".to_string(),
            message: "Buffer pool below 4 may cause frame drops".to_string(),
        });
    } else if config.performance.buffer_pool_size > 64 {
        warnings.push(ValidationWarning {
            field: "performance.buffer_pool_size".to_string(),
            message: "Buffer pool above 64 wastes memory with minimal benefit".to_string(),
        });
    }
}

/// Validate EGFX configuration
fn validate_egfx_config(
    config: &Config,
    errors: &mut Vec<ValidationError>,
    warnings: &mut Vec<ValidationWarning>,
) {
    if !config.egfx.enabled {
        return; // Skip validation if disabled
    }

    match config.egfx.codec.as_str() {
        "auto" | "avc420" | "avc444" => {}
        _ => {
            errors.push(ValidationError {
                field: "egfx.codec".to_string(),
                message: format!(
                    "Invalid codec: '{}'. Valid options: auto, avc420, avc444",
                    config.egfx.codec
                ),
            });
        }
    }

    match config.egfx.zgfx_compression.as_str() {
        "never" | "auto" | "always" => {}
        _ => {
            errors.push(ValidationError {
                field: "egfx.zgfx_compression".to_string(),
                message: format!(
                    "Invalid ZGFX compression: '{}'. Valid options: never, auto, always",
                    config.egfx.zgfx_compression
                ),
            });
        }
    }

    let valid_levels = ["auto", "3.0", "3.1", "4.0", "4.1", "5.0", "5.1", "5.2"];
    if !valid_levels.contains(&config.egfx.h264_level.as_str()) {
        errors.push(ValidationError {
            field: "egfx.h264_level".to_string(),
            message: format!(
                "Invalid H.264 level: '{}'. Valid options: {}",
                config.egfx.h264_level,
                valid_levels.join(", ")
            ),
        });
    }

    if config.egfx.qp_min > 51 || config.egfx.qp_max > 51 || config.egfx.qp_default > 51 {
        errors.push(ValidationError {
            field: "egfx.qp".to_string(),
            message: "QP values must be between 0 and 51".to_string(),
        });
    }

    if config.egfx.qp_min > config.egfx.qp_max {
        errors.push(ValidationError {
            field: "egfx.qp_min".to_string(),
            message: format!(
                "qp_min ({}) cannot be greater than qp_max ({})",
                config.egfx.qp_min, config.egfx.qp_max
            ),
        });
    }

    if config.egfx.qp_default < config.egfx.qp_min || config.egfx.qp_default > config.egfx.qp_max {
        errors.push(ValidationError {
            field: "egfx.qp_default".to_string(),
            message: format!(
                "qp_default ({}) must be between qp_min ({}) and qp_max ({})",
                config.egfx.qp_default, config.egfx.qp_min, config.egfx.qp_max
            ),
        });
    }

    if config.egfx.h264_bitrate < 100 {
        warnings.push(ValidationWarning {
            field: "egfx.h264_bitrate".to_string(),
            message: "H.264 bitrate below 100 kbps will result in very poor quality".to_string(),
        });
    }

    if config.egfx.avc444_aux_bitrate_ratio < 0.1 || config.egfx.avc444_aux_bitrate_ratio > 1.0 {
        warnings.push(ValidationWarning {
            field: "egfx.avc444_aux_bitrate_ratio".to_string(),
            message: "AVC444 aux bitrate ratio should be between 0.1 and 1.0".to_string(),
        });
    }
}

/// Validate damage tracking configuration
fn validate_damage_tracking_config(
    config: &Config,
    errors: &mut Vec<ValidationError>,
    warnings: &mut Vec<ValidationWarning>,
) {
    match config.damage_tracking.method.as_str() {
        "pipewire" | "diff" | "hybrid" => {}
        _ => {
            errors.push(ValidationError {
                field: "damage_tracking.method".to_string(),
                message: format!(
                    "Invalid damage tracking method: '{}'. Valid options: pipewire, diff, hybrid",
                    config.damage_tracking.method
                ),
            });
        }
    }

    if config.damage_tracking.diff_threshold > 1.0 {
        warnings.push(ValidationWarning {
            field: "damage_tracking.diff_threshold".to_string(),
            message: "Diff threshold should be in 0.0-1.0 range. Values above 1.0 effectively disable damage tracking.".to_string(),
        });
    }
}

/// Validate hardware encoding configuration
fn validate_hardware_encoding_config(
    config: &Config,
    errors: &mut Vec<ValidationError>,
    warnings: &mut Vec<ValidationWarning>,
) {
    if !config.hardware_encoding.enabled {
        return;
    }

    match config.hardware_encoding.quality_preset.as_str() {
        "speed" | "balanced" | "quality" => {}
        _ => {
            errors.push(ValidationError {
                field: "hardware_encoding.quality_preset".to_string(),
                message: format!(
                    "Invalid quality preset: '{}'. Valid options: speed, balanced, quality",
                    config.hardware_encoding.quality_preset
                ),
            });
        }
    }

    if config.hardware_encoding.enabled && !config.hardware_encoding.vaapi_device.exists() {
        warnings.push(ValidationWarning {
            field: "hardware_encoding.vaapi_device".to_string(),
            message: format!(
                "VA-API device not found: {}. Hardware encoding may not work.",
                config.hardware_encoding.vaapi_device.display()
            ),
        });
    }
}

/// Validate display configuration
fn validate_display_config(
    config: &Config,
    errors: &mut Vec<ValidationError>,
    warnings: &mut Vec<ValidationWarning>,
) {
    for res in &config.display.allowed_resolutions {
        // Check format like "1920x1080"
        let parts: Vec<&str> = res.split('x').collect();
        if parts.len() != 2 || parts[0].parse::<u32>().is_err() || parts[1].parse::<u32>().is_err()
        {
            errors.push(ValidationError {
                field: "display.allowed_resolutions".to_string(),
                message: format!(
                    "Invalid resolution format: '{}'. Expected format: WIDTHxHEIGHT (e.g., 1920x1080)",
                    res
                ),
            });
        }
    }

    if config.display.allow_resize && !config.display.allowed_resolutions.is_empty() {
        warnings.push(ValidationWarning {
            field: "display.allowed_resolutions".to_string(),
            message: "Both dynamic resize and specific resolutions are set. Clients will be restricted to listed resolutions.".to_string(),
        });
    }
}

/// Validate logging configuration
fn validate_logging_config(
    config: &Config,
    errors: &mut Vec<ValidationError>,
    warnings: &mut Vec<ValidationWarning>,
) {
    match config.logging.level.to_lowercase().as_str() {
        "trace" | "debug" | "info" | "warn" | "error" => {}
        _ => {
            errors.push(ValidationError {
                field: "logging.level".to_string(),
                message: format!(
                    "Invalid log level: '{}'. Valid options: trace, debug, info, warn, error",
                    config.logging.level
                ),
            });
        }
    }

    if let Some(ref log_dir) = config.logging.log_dir {
        if !log_dir.exists() {
            warnings.push(ValidationWarning {
                field: "logging.log_dir".to_string(),
                message: format!("Log directory does not exist: {}", log_dir.display()),
            });
        } else if !log_dir.is_dir() {
            errors.push(ValidationError {
                field: "logging.log_dir".to_string(),
                message: format!("Log path is not a directory: {}", log_dir.display()),
            });
        }
    }

    if config.logging.level.to_lowercase() == "trace" {
        warnings.push(ValidationWarning {
            field: "logging.level".to_string(),
            message: "Trace logging generates high volume output. Use for debugging only."
                .to_string(),
        });
    }
}

/// Cross-section validation for related settings
fn validate_cross_section(
    config: &Config,
    _errors: &mut Vec<ValidationError>,
    warnings: &mut Vec<ValidationWarning>,
) {
    if config.damage_tracking.enabled
        && config.damage_tracking.method == "diff"
        && config.video.target_fps > 60
    {
        warnings.push(ValidationWarning {
            field: "damage_tracking + target_fps".to_string(),
            message: "Diff-based damage tracking at >60 FPS may impact CPU performance".to_string(),
        });
    }

    // Warn about AVC444 compatibility
    if config.egfx.codec == "avc444" {
        warnings.push(ValidationWarning {
            field: "egfx.codec".to_string(),
            message: "AVC444 requires FreeRDP 2.x or Windows 10+. Older clients may not work."
                .to_string(),
        });
    }
}

/// Validate a PEM file contains the expected type
fn validate_pem_file(path: &Path, expected_type: &str) -> Result<(), String> {
    let content = std::fs::read_to_string(path).map_err(|e| format!("Cannot read file: {}", e))?;

    let begin_marker = format!("-----BEGIN {}-----", expected_type);

    // Also accept more specific markers
    let has_valid_markers = content.contains(&begin_marker)
        || content.contains(&format!("-----BEGIN RSA {}-----", expected_type))
        || content.contains(&format!("-----BEGIN EC {}-----", expected_type))
        || content.contains("-----BEGIN PRIVATE KEY-----") && expected_type == "PRIVATE KEY"
        || content.contains("-----BEGIN CERTIFICATE-----") && expected_type == "CERTIFICATE";

    if !has_valid_markers {
        return Err(format!(
            "File does not contain valid PEM {} markers",
            expected_type
        ));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_default_config() {
        // Skip this test in CI where cert files don't exist
        let config = Config::default();
        let result = validate_config(&config);
        // Default config has errors because certs don't exist
        // but should have valid structure
        assert!(!result.errors.is_empty() || !result.warnings.is_empty());
    }

    #[test]
    fn test_validate_server_address() {
        let mut config = Config::default();
        config.server.listen_addr = "invalid".to_string();
        let result = validate_config(&config);
        assert!(result
            .errors
            .iter()
            .any(|e| e.field == "server.listen_addr"));
    }

    #[test]
    fn test_validate_qp_range() {
        let mut config = Config::default();
        config.egfx.enabled = true;
        config.egfx.qp_min = 40;
        config.egfx.qp_max = 20;
        let result = validate_config(&config);
        assert!(result.errors.iter().any(|e| e.field == "egfx.qp_min"));
    }
}
