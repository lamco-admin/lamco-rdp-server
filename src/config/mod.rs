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

/// Check if running inside a Flatpak sandbox
pub fn is_flatpak() -> bool {
    // Check for FLATPAK_ID env var (set by Flatpak runtime)
    std::env::var("FLATPAK_ID").is_ok()
        // Also check for /.flatpak-info which exists in all Flatpak sandboxes
        || std::path::Path::new("/.flatpak-info").exists()
}

pub fn get_cert_config_dir() -> PathBuf {
    if is_flatpak() {
        // Flatpak: use XDG paths which are mapped to ~/.var/app/<app-id>/
        if let Some(config_dir) = dirs::config_dir() {
            return config_dir;
        }
        // Fallback for Flatpak (shouldn't happen but be safe)
        PathBuf::from("/app/config")
    } else {
        // Native: prefer user config if not root, otherwise /etc/
        let uid = unsafe { libc::getuid() };
        if uid == 0 {
            // Running as root - use system directory
            PathBuf::from("/etc/lamco-rdp-server")
        } else {
            // Running as user - use XDG config
            dirs::config_dir()
                .map(|d| d.join("lamco-rdp-server"))
                .unwrap_or_else(|| PathBuf::from("/etc/lamco-rdp-server"))
        }
    }
}

pub fn default_cert_path() -> PathBuf {
    get_cert_config_dir().join("cert.pem")
}

pub fn default_key_path() -> PathBuf {
    get_cert_config_dir().join("key.pem")
}

pub mod types;

// Use types from types.rs
use types::*;

// Re-export types needed by other modules
pub use types::AudioConfig;
pub use types::GuiStateConfig;
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
    /// GUI state configuration (persisted between sessions)
    /// Optional - not required for server operation
    #[serde(default)]
    pub gui_state: GuiStateConfig,
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
                cert_path: default_cert_path(),
                key_path: default_key_path(),
                enable_nla: false,
                auth_method: "none".to_string(),
                require_tls_13: false,
            },
            video: VideoConfig {
                target_fps: 30,
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
                kde_syncselection_hint: false, // Disabled by default (experimental)
                strategy_override: None,       // Automatic selection by default
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
            gui_state: GuiStateConfig::default(),
        })
    }

    /// Check if TLS certificates are configured and exist
    ///
    /// Returns `Ok(true)` if both cert and key exist,
    /// `Ok(false)` if they don't exist (need to be generated),
    /// `Err` if there's a more complex issue.
    pub fn check_certificates(&self) -> Result<bool> {
        let cert_exists = self.security.cert_path.exists();
        let key_exists = self.security.key_path.exists();

        match (cert_exists, key_exists) {
            (true, true) => Ok(true),
            (false, false) => Ok(false), // Neither exists - can generate
            (true, false) => {
                anyhow::bail!(
                    "Certificate exists but private key is missing: {:?}",
                    self.security.key_path
                )
            }
            (false, true) => {
                anyhow::bail!(
                    "Private key exists but certificate is missing: {:?}",
                    self.security.cert_path
                )
            }
        }
    }

    /// Validate configuration
    pub fn validate(&self) -> Result<()> {
        self.server
            .listen_addr
            .parse::<SocketAddr>()
            .context("Invalid listen address")?;

        if !self.security.cert_path.exists() {
            anyhow::bail!("Certificate not found: {:?}", self.security.cert_path);
        }
        if !self.security.key_path.exists() {
            anyhow::bail!("Private key not found: {:?}", self.security.key_path);
        }

        match self.video.cursor_mode.as_str() {
            "embedded" | "metadata" | "hidden" => {}
            _ => anyhow::bail!("Invalid cursor mode: {}", self.video.cursor_mode),
        }

        match self.cursor.mode.as_str() {
            "metadata" | "painted" | "hidden" | "predictive" => {}
            _ => anyhow::bail!("Invalid cursor strategy mode: {}", self.cursor.mode),
        }

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

        match self.damage_tracking.method.as_str() {
            "pipewire" | "diff" | "hybrid" => {}
            _ => anyhow::bail!(
                "Invalid damage tracking method: {}",
                self.damage_tracking.method
            ),
        }

        match self.hardware_encoding.quality_preset.as_str() {
            "speed" | "balanced" | "quality" => {}
            _ => anyhow::bail!(
                "Invalid quality preset: {}",
                self.hardware_encoding.quality_preset
            ),
        }

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
    fn test_config_validation_invalid_cursor_mode() {
        let mut config = Config::default_config().unwrap();
        config.video.cursor_mode = "invalid_mode".to_string();
        assert!(config.validate().is_err());
    }
}
