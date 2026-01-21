//! Configuration management
//!
//! Handles loading, validation, and merging of configuration from:
//! - TOML files
//! - Environment variables
//! - CLI arguments

use anyhow::{Context, Result};
use ashpd::desktop::remote_desktop::DeviceType;
use ashpd::desktop::screencast::{CursorMode, SourceType};
use enumflags2::BitFlags;
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::path::PathBuf;

pub mod types;

// Use types from types.rs
use types::*;

// Re-export types needed by other modules
pub use types::AudioConfig;
pub use types::HardwareEncodingConfig;
pub use types::{CursorConfig, CursorPredictorConfig};

/// Main configuration structure
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// Server configuration
    pub server: ServerConfig,
    /// Security configuration
    pub security: SecurityConfig,
    /// Video configuration
    pub video: VideoConfig,
    /// Video pipeline configuration
    pub video_pipeline: VideoPipelineConfig,
    /// Input configuration
    pub input: InputConfig,
    /// Clipboard configuration
    pub clipboard: ClipboardConfig,
    /// Multi-monitor configuration
    pub multimon: MultiMonitorConfig,
    /// Performance configuration
    pub performance: PerformanceConfig,
    /// Logging configuration
    pub logging: LoggingConfig,
    /// EGFX configuration
    #[serde(default)]
    pub egfx: EgfxConfig,
    /// Damage tracking configuration
    #[serde(default)]
    pub damage_tracking: DamageTrackingConfig,
    /// Hardware encoding configuration
    #[serde(default)]
    pub hardware_encoding: HardwareEncodingConfig,
    /// Display control configuration
    #[serde(default)]
    pub display: DisplayConfig,
    /// Advanced video configuration
    #[serde(default)]
    pub advanced_video: AdvancedVideoConfig,
    /// Cursor handling configuration (Premium)
    #[serde(default)]
    pub cursor: CursorConfig,
    /// Audio configuration (RDPSND)
    #[serde(default)]
    pub audio: AudioConfig,
}

impl Config {
    /// Load configuration from file
    pub fn load(path: &str) -> Result<Self> {
        let content = std::fs::read_to_string(path)
            .context(format!("Failed to read config file: {}", path))?;

        let config: Config = toml::from_str(&content).context("Failed to parse config file")?;

        config.validate()?;
        Ok(config)
    }

    /// Create default configuration
    pub fn default_config() -> Result<Self> {
        Ok(Config {
            server: ServerConfig {
                listen_addr: "0.0.0.0:3389".to_string(),
                max_connections: 10,
                session_timeout: 0,
                use_portals: true,
            },
            security: SecurityConfig {
                cert_path: PathBuf::from("/etc/lamco-rdp-server/cert.pem"),
                key_path: PathBuf::from("/etc/lamco-rdp-server/key.pem"),
                enable_nla: true,
                auth_method: "pam".to_string(),
                require_tls_13: true,
            },
            video: VideoConfig {
                encoder: "auto".to_string(),
                vaapi_device: PathBuf::from("/dev/dri/renderD128"),
                target_fps: 30,
                bitrate: 4000,
                damage_tracking: true,
                cursor_mode: "metadata".to_string(),
            },
            video_pipeline: VideoPipelineConfig::default(),
            input: InputConfig {
                use_libei: true,
                keyboard_layout: "auto".to_string(),
                enable_touch: false,
            },
            clipboard: ClipboardConfig {
                enabled: true,
                max_size: 10485760, // 10 MB
                rate_limit_ms: 200, // Max 5 events/second
                allowed_types: vec![],
            },
            multimon: MultiMonitorConfig {
                enabled: true,
                max_monitors: 4,
            },
            performance: PerformanceConfig {
                encoder_threads: 0,
                network_threads: 0,
                buffer_pool_size: 16,
                zero_copy: true,
                adaptive_fps: AdaptiveFpsConfig::default(),
                latency: LatencyConfig::default(),
            },
            logging: LoggingConfig {
                level: "info".to_string(),
                log_dir: None,
                metrics: true,
            },
            egfx: EgfxConfig::default(),
            damage_tracking: DamageTrackingConfig::default(),
            hardware_encoding: HardwareEncodingConfig::default(),
            display: DisplayConfig::default(),
            advanced_video: AdvancedVideoConfig::default(),
            cursor: CursorConfig::default(),
            audio: AudioConfig::default(),
        })
    }

    /// Validate configuration
    pub fn validate(&self) -> Result<()> {
        // Validate listen address
        self.server
            .listen_addr
            .parse::<SocketAddr>()
            .context("Invalid listen address")?;

        // Validate cert paths exist
        if !self.security.cert_path.exists() {
            anyhow::bail!("Certificate not found: {:?}", self.security.cert_path);
        }
        if !self.security.key_path.exists() {
            anyhow::bail!("Private key not found: {:?}", self.security.key_path);
        }

        // Validate encoder choice
        match self.video.encoder.as_str() {
            "vaapi" | "openh264" | "auto" => {}
            _ => anyhow::bail!("Invalid encoder: {}", self.video.encoder),
        }

        // Validate cursor mode (video config - legacy)
        match self.video.cursor_mode.as_str() {
            "embedded" | "metadata" | "hidden" => {}
            _ => anyhow::bail!("Invalid cursor mode: {}", self.video.cursor_mode),
        }

        // Validate cursor config (premium cursor strategies)
        match self.cursor.mode.as_str() {
            "metadata" | "painted" | "hidden" | "predictive" => {}
            _ => anyhow::bail!("Invalid cursor strategy mode: {}", self.cursor.mode),
        }

        // Validate EGFX configuration
        match self.egfx.zgfx_compression.as_str() {
            "never" | "auto" | "always" => {}
            _ => anyhow::bail!(
                "Invalid ZGFX compression mode: {}",
                self.egfx.zgfx_compression
            ),
        }

        match self.egfx.codec.as_str() {
            "avc420" | "avc444" | "auto" => {}
            _ => anyhow::bail!("Invalid EGFX codec: {}", self.egfx.codec),
        }

        // Validate damage tracking method
        match self.damage_tracking.method.as_str() {
            "pipewire" | "diff" | "hybrid" => {}
            _ => anyhow::bail!(
                "Invalid damage tracking method: {}",
                self.damage_tracking.method
            ),
        }

        // Validate hardware encoding quality preset
        match self.hardware_encoding.quality_preset.as_str() {
            "speed" | "balanced" | "quality" => {}
            _ => anyhow::bail!(
                "Invalid quality preset: {}",
                self.hardware_encoding.quality_preset
            ),
        }

        // Validate QP ranges
        if self.egfx.qp_min > self.egfx.qp_max {
            anyhow::bail!(
                "qp_min ({}) cannot be greater than qp_max ({})",
                self.egfx.qp_min,
                self.egfx.qp_max
            );
        }

        if self.egfx.qp_default < self.egfx.qp_min || self.egfx.qp_default > self.egfx.qp_max {
            anyhow::bail!(
                "qp_default ({}) must be between qp_min ({}) and qp_max ({})",
                self.egfx.qp_default,
                self.egfx.qp_min,
                self.egfx.qp_max
            );
        }

        Ok(())
    }

    /// Override config with CLI arguments
    pub fn with_overrides(mut self, listen: Option<String>, port: u16) -> Self {
        if let Some(listen_addr) = listen {
            self.server.listen_addr = format!("{}:{}", listen_addr, port);
        } else {
            // Just update port
            if let Ok(mut addr) = self.server.listen_addr.parse::<SocketAddr>() {
                addr.set_port(port);
                self.server.listen_addr = addr.to_string();
            }
        }

        self
    }

    /// Convert server configuration to Portal configuration
    ///
    /// Maps relevant server settings to `lamco_portal::PortalConfig` for
    /// screen capture and input injection via XDG Desktop Portals.
    ///
    /// # Mapping
    ///
    /// | Server Config | Portal Config |
    /// |--------------|---------------|
    /// | video.cursor_mode | cursor_mode |
    /// | multimon.enabled | allow_multiple |
    /// | input.use_libei | devices (Keyboard + Pointer) |
    /// | input.enable_touch | devices (+ Touchscreen) |
    pub fn to_portal_config(&self) -> lamco_portal::PortalConfig {
        // Map cursor mode from string to enum
        let cursor_mode = match self.video.cursor_mode.to_lowercase().as_str() {
            "embedded" => CursorMode::Embedded,
            "hidden" => CursorMode::Hidden,
            _ => CursorMode::Metadata, // Default for "metadata" or invalid
        };

        // Build device flags based on input configuration
        let mut devices: BitFlags<DeviceType> = DeviceType::Keyboard.into();
        if self.input.use_libei {
            devices |= DeviceType::Pointer;
        }
        if self.input.enable_touch {
            devices |= DeviceType::Touchscreen;
        }

        // Source types - always allow both monitors and windows
        let source_type: BitFlags<SourceType> = SourceType::Monitor | SourceType::Window;

        lamco_portal::PortalConfig::builder()
            .cursor_mode(cursor_mode)
            .source_type(source_type)
            .devices(devices)
            .allow_multiple(self.multimon.enabled)
            .build()
    }
}

impl Default for Config {
    fn default() -> Self {
        Self::default_config().expect("Failed to create default config")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = Config::default_config().unwrap();
        assert_eq!(config.server.listen_addr, "0.0.0.0:3389");
        assert!(config.server.use_portals);
        assert_eq!(config.video.target_fps, 30);
    }

    #[test]
    fn test_config_validation_invalid_address() {
        let mut config = Config::default_config().unwrap();
        config.server.listen_addr = "invalid".to_string();
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_config_validation_invalid_encoder() {
        let mut config = Config::default_config().unwrap();
        config.video.encoder = "invalid_encoder".to_string();
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_config_validation_invalid_cursor_mode() {
        let mut config = Config::default_config().unwrap();
        config.video.cursor_mode = "invalid_mode".to_string();
        assert!(config.validate().is_err());
    }
}
