//! Configuration Validation Module
//!
//! Validates configuration parameters and provides detailed error/warning messages.

use std::net::SocketAddr;
use std::path::Path;

use crate::config::Config;
use crate::gui::state::{ValidationError, ValidationResult, ValidationWarning};

/// Validate a complete configuration
pub fn validate_config(config: &Config) -> ValidationResult {
    let mut errors = Vec::new();
    let mut warnings = Vec::new();

    // Validate server section
    validate_server_config(config, &mut errors, &mut warnings);

    // Validate security section
    validate_security_config(config, &mut errors, &mut warnings);

    // Validate video section
    validate_video_config(config, &mut errors, &mut warnings);

    // Validate input section
    validate_input_config(config, &mut errors, &mut warnings);

    // Validate clipboard section
    validate_clipboard_config(config, &mut errors, &mut warnings);

    // Validate performance section
    validate_performance_config(config, &mut errors, &mut warnings);

    // Validate EGFX section
    validate_egfx_config(config, &mut errors, &mut warnings);

    // Validate damage tracking section
    validate_damage_tracking_config(config, &mut errors, &mut warnings);

    // Validate hardware encoding section
    validate_hardware_encoding_config(config, &mut errors, &mut warnings);

    // Validate display section
    validate_display_config(config, &mut errors, &mut warnings);

    // Validate logging section
    validate_logging_config(config, &mut errors, &mut warnings);

    // Cross-section validation
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
    // Validate listen address
    if config.server.listen_addr.parse::<SocketAddr>().is_err() {
        errors.push(ValidationError {
            field: "server.listen_addr".to_string(),
            message: format!("Invalid listen address: '{}'", config.server.listen_addr),
        });
    }

    // Warn about privileged port
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

    // Validate max_connections
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
    // Check certificate file exists
    if !config.security.cert_path.exists() {
        errors.push(ValidationError {
            field: "security.cert_path".to_string(),
            message: format!(
                "Certificate file not found: {}",
                config.security.cert_path.display()
            ),
        });
    } else {
        // Verify it's readable and valid PEM
        if let Err(e) = validate_pem_file(&config.security.cert_path, "CERTIFICATE") {
            errors.push(ValidationError {
                field: "security.cert_path".to_string(),
                message: e,
            });
        }
    }

    // Check private key file exists
    if !config.security.key_path.exists() {
        errors.push(ValidationError {
            field: "security.key_path".to_string(),
            message: format!(
                "Private key file not found: {}",
                config.security.key_path.display()
            ),
        });
    } else {
        // Verify it's readable and valid PEM
        if let Err(e) = validate_pem_file(&config.security.key_path, "PRIVATE KEY") {
            errors.push(ValidationError {
                field: "security.key_path".to_string(),
                message: e,
            });
        }

        // Check key file permissions (should be restrictive)
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

    // Validate auth method
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

    // Warn if NLA is disabled
    if !config.security.enable_nla {
        warnings.push(ValidationWarning {
            field: "security.enable_nla".to_string(),
            message: "Network Level Authentication is disabled. This reduces security.".to_string(),
        });
    }

    // Warn if TLS 1.3 is disabled
    if !config.security.require_tls_13 {
        warnings.push(ValidationWarning {
            field: "security.require_tls_13".to_string(),
            message: "TLS 1.3 requirement is disabled. Older protocols may be vulnerable."
                .to_string(),
        });
    }
}

/// Validate video configuration
fn validate_video_config(
    config: &Config,
    errors: &mut Vec<ValidationError>,
    warnings: &mut Vec<ValidationWarning>,
) {
    // Validate encoder choice
    match config.video.encoder.as_str() {
        "auto" | "vaapi" | "openh264" | "nvenc" => {}
        _ => {
            errors.push(ValidationError {
                field: "video.encoder".to_string(),
                message: format!(
                    "Invalid encoder: '{}'. Valid options: auto, vaapi, openh264, nvenc",
                    config.video.encoder
                ),
            });
        }
    }

    // Check VA-API device if specified
    if config.video.encoder == "vaapi" && !config.video.vaapi_device.exists() {
        errors.push(ValidationError {
            field: "video.vaapi_device".to_string(),
            message: format!(
                "VA-API device not found: {}",
                config.video.vaapi_device.display()
            ),
        });
    }

    // Validate FPS
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

    // Validate bitrate
    if config.video.bitrate < 100 {
        warnings.push(ValidationWarning {
            field: "video.bitrate".to_string(),
            message: "Bitrate below 100 kbps will result in very poor quality".to_string(),
        });
    } else if config.video.bitrate > 50000 {
        warnings.push(ValidationWarning {
            field: "video.bitrate".to_string(),
            message: "Bitrate above 50 Mbps may exceed network capacity".to_string(),
        });
    }

    // Validate cursor mode
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
    // Validate keyboard layout
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
    // Validate max size
    if config.clipboard.max_size > 100 * 1024 * 1024 {
        warnings.push(ValidationWarning {
            field: "clipboard.max_size".to_string(),
            message: "Clipboard max size above 100 MB may cause memory issues".to_string(),
        });
    }

    // Validate rate limit
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
    // Validate thread counts
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

    // Validate buffer pool size
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

    // Validate codec
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

    // Validate ZGFX compression
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

    // Validate H.264 level
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

    // Validate QP range
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

    // Validate bitrate
    if config.egfx.h264_bitrate < 100 {
        warnings.push(ValidationWarning {
            field: "egfx.h264_bitrate".to_string(),
            message: "H.264 bitrate below 100 kbps will result in very poor quality".to_string(),
        });
    }

    // Validate AVC444 aux bitrate ratio
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
    // Validate method
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

    // Validate diff threshold (0.0-1.0 range)
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

    // Validate quality preset
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

    // Check VA-API device exists if hardware encoding is enabled
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
    // Validate allowed resolutions format
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

    // Warn if both resize is allowed and specific resolutions are set
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
    // Validate log level
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

    // Validate log directory if specified
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

    // Warn about trace level in production
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
    // Warn if EGFX is enabled but H.264 encoder is not configured
    if config.egfx.enabled && config.video.encoder == "none" {
        warnings.push(ValidationWarning {
            field: "egfx + video.encoder".to_string(),
            message: "EGFX is enabled but no encoder is configured".to_string(),
        });
    }

    // Warn about performance impact of certain combinations
    if config.video.damage_tracking
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
