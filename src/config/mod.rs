//! Configuration management
//!
//! Handles loading, validation, and merging of configuration from:
//! - TOML files
//! - Environment variables
//! - CLI arguments
#![expect(
    unsafe_code,
    reason = "getuid() for root detection, set_var() for portal env bridge"
)]

use std::{net::SocketAddr, path::PathBuf};

use anyhow::{Context, Result};
use ashpd::desktop::{
    remote_desktop::DeviceType,
    screencast::{CursorMode, SourceType},
};
use enumflags2::BitFlags;
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

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
            dirs::config_dir().map_or_else(
                || PathBuf::from("/etc/lamco-rdp-server"),
                |d| d.join("lamco-rdp-server"),
            )
        }
    }
}

/// Resolve log directory, enforcing sandbox containment in Flatpak.
///
/// In Flatpak mode the configured log_dir is ignored — logs always go to
/// the sandbox data directory. In native mode the configured path is used,
/// falling back to XDG_DATA_HOME/lamco-rdp-server/logs.
pub fn resolve_log_dir(configured: &Option<PathBuf>) -> PathBuf {
    if is_flatpak() {
        // Sandbox: XDG_DATA_HOME is ~/.var/app/<app-id>/data in Flatpak
        dirs::data_dir()
            .unwrap_or_else(|| PathBuf::from("/app/data"))
            .join("logs")
    } else {
        configured.clone().unwrap_or_else(|| {
            dirs::data_dir().map_or_else(
                || PathBuf::from("/tmp/lamco-rdp-server"),
                |d| d.join("lamco-rdp-server/logs"),
            )
        })
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
// Re-export types needed by other modules
use types::{
    AdvancedVideoConfig, CaptureProtocolConfig, ClipboardConfig, DamageTrackingConfig,
    DisplayConfig, EgfxConfig, InputConfig, LoggingConfig, MultiMonitorConfig, NotificationConfig,
    PerformanceConfig, SecurityConfig, ServerConfig, VideoConfig, VideoPipelineConfig,
};
pub use types::{
    AudioConfig, CursorConfig, CursorPredictorConfig, GuiStateConfig, HardwareEncodingConfig,
};

/// Current config file version. Bumped when breaking changes require migration.
const CURRENT_CONFIG_VERSION: u32 = 1;

/// Main configuration structure
#[expect(
    clippy::unsafe_derive_deserialize,
    reason = "unsafe in this module (getuid, set_var) is unrelated to deserialized fields"
)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// Config file format version (for migration support)
    #[serde(default = "default_config_version")]
    pub config_version: u32,
    /// Server configuration
    #[serde(default)]
    pub server: ServerConfig,
    /// Security configuration
    #[serde(default)]
    pub security: SecurityConfig,
    /// Video configuration
    #[serde(default)]
    pub video: VideoConfig,
    /// Video pipeline configuration
    #[serde(default)]
    pub video_pipeline: VideoPipelineConfig,
    /// Capture protocol configuration (portal-generic strategy)
    #[serde(default)]
    pub capture: CaptureProtocolConfig,
    /// Input configuration
    #[serde(default)]
    pub input: InputConfig,
    /// Clipboard configuration
    #[serde(default)]
    pub clipboard: ClipboardConfig,
    /// Multi-monitor configuration
    #[serde(default)]
    pub multimon: MultiMonitorConfig,
    /// Performance configuration
    #[serde(default)]
    pub performance: PerformanceConfig,
    /// Logging configuration
    #[serde(default)]
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
    /// Notification configuration (Flatpak portal notifications)
    #[serde(default)]
    pub notifications: NotificationConfig,
    /// GUI state configuration (persisted between sessions)
    /// Optional - not required for server operation
    #[serde(default)]
    pub gui_state: GuiStateConfig,
}

fn default_config_version() -> u32 {
    CURRENT_CONFIG_VERSION
}

impl Config {
    /// Load configuration with layered overrides.
    ///
    /// Priority (highest wins):
    /// 1. Environment variables (`LAMCO_` prefix, `__` for nesting)
    /// 2. TOML config file
    /// 3. Compiled-in defaults (via `#[serde(default)]`)
    ///
    /// Environment variable examples:
    /// - `LAMCO_SERVER__LISTEN_ADDR=0.0.0.0:3390` overrides `server.listen_addr`
    /// - `LAMCO_EGFX__ENABLED=false` overrides `egfx.enabled`
    /// - `LAMCO_VIDEO__TARGET_FPS=60` overrides `video.target_fps`
    pub fn load(path: &str) -> Result<Self> {
        let mut table = match std::fs::read_to_string(path) {
            Ok(content) => content
                .parse::<toml::Table>()
                .context("Failed to parse config file")?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                debug!("Config file not found at {path}, using defaults");
                toml::Table::new()
            }
            Err(e) => return Err(e).context("Failed to read config file"),
        };

        // Snapshot before env overlays so we can fall back if they cause errors
        let base_table = table.clone();

        // Overlay LAMCO_* env vars: LAMCO_SECTION__KEY → table["section"]["key"]
        let mut env_overrides = Vec::new();
        for (key, value) in std::env::vars() {
            let Some(suffix) = key.strip_prefix("LAMCO_") else {
                continue;
            };
            let parts: Vec<String> = suffix.split("__").map(str::to_lowercase).collect();
            if parts.is_empty() {
                continue;
            }

            let mut current = &mut table;
            for part in &parts[..parts.len() - 1] {
                current = current
                    .entry(part)
                    .or_insert_with(|| toml::Value::Table(toml::Table::new()))
                    .as_table_mut()
                    .with_context(|| format!("Config path conflict for env var {key}"))?;
            }

            // parts is non-empty (guarded above)
            let field = &parts[parts.len() - 1];
            let path_str = if parts.len() > 1 {
                format!("{}.{field}", parts[..parts.len() - 1].join("."))
            } else {
                field.clone()
            };
            debug!("Config env override: {key} -> {path_str}");
            current.insert(field.clone(), infer_toml_value(&value));
            env_overrides.push(key.clone());
        }

        // Try deserialization with env overlays applied
        let mut config: Config = match toml::Value::Table(table).try_into() {
            Ok(config) => config,
            Err(e) if env_overrides.is_empty() => {
                return Err(e).context("Failed to deserialize config");
            }
            Err(e) => {
                // Env overrides caused a type mismatch; fall back to file + defaults only
                warn!(
                    "Config env overrides caused deserialization error, ignoring them: {e}. \
                     Set vars: {}",
                    env_overrides.join(", ")
                );
                toml::Value::Table(base_table)
                    .try_into()
                    .context("Failed to deserialize config (without env overrides)")?
            }
        };

        config.migrate();
        config.validate()?;
        Ok(config)
    }

    /// Apply any necessary migrations for older config versions.
    fn migrate(&mut self) {
        // v0 (implicit): enable_nla field used instead of security_mode
        if self.security.enable_nla && self.security.security_mode == "auto" {
            self.security.security_mode = "hybrid".to_string();
        }

        self.config_version = CURRENT_CONFIG_VERSION;
    }

    /// Create default configuration
    pub fn default_config() -> Result<Self> {
        Ok(Config::default())
    }

    /// Generate a commented TOML config file with all defaults.
    pub fn generate_default_toml() -> Result<String> {
        let config = Config::default();
        toml::to_string_pretty(&config).context("Failed to serialize default config")
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
                    "Certificate exists but private key is missing: {}",
                    self.security.key_path.display()
                )
            }
            (false, true) => {
                anyhow::bail!(
                    "Private key exists but certificate is missing: {}",
                    self.security.cert_path.display()
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
            anyhow::bail!(
                "Certificate not found: {}",
                self.security.cert_path.display()
            );
        }
        if !self.security.key_path.exists() {
            anyhow::bail!(
                "Private key not found: {}",
                self.security.key_path.display()
            );
        }

        match self.security.auth_method.as_str() {
            "none" | "pam" => {}
            _ => anyhow::bail!(
                "Invalid auth_method: {} (expected none or pam)",
                self.security.auth_method
            ),
        }

        match self.security.security_mode.as_str() {
            "tls" | "hybrid" | "auto" => {}
            _ => anyhow::bail!(
                "Invalid security mode: {} (expected tls, hybrid, or auto)",
                self.security.security_mode
            ),
        }

        // PAM requires TLS-only mode. CredSSP/Hybrid uses NTLM challenge-response
        // which requires the server to already know the password — incompatible with
        // PAM's validate-on-receipt model. Same approach as xrdp.
        if self.security.auth_method == "pam" && self.security.security_mode != "tls" {
            anyhow::bail!(
                "auth_method=pam requires security_mode=tls (CredSSP/Hybrid is \
                 incompatible with PAM authentication)"
            );
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

    /// Export protocol preferences as environment variables.
    ///
    /// The portal-generic library reads `XDP_GENERIC_*` env vars for protocol
    /// selection. This bridges config.toml values into that env-var interface,
    /// but only if the user hasn't already set an env override (env wins).
    pub fn export_protocol_env_vars(&self) {
        // SAFETY: called from main() before tokio runtime starts, single-threaded
        unsafe {
            // Capture protocol
            if std::env::var("XDP_GENERIC_CAPTURE_PROTOCOL").is_err()
                && self.capture.protocol != "auto"
            {
                std::env::set_var("XDP_GENERIC_CAPTURE_PROTOCOL", &self.capture.protocol);
            }
            if std::env::var("XDP_GENERIC_CAPTURE_NO_FALLBACK").is_err()
                && !self.capture.allow_fallback
            {
                std::env::set_var("XDP_GENERIC_CAPTURE_NO_FALLBACK", "1");
            }
            if std::env::var("XDP_GENERIC_CAPTURE_TIMEOUT_MS").is_err()
                && self.capture.handshake_timeout_ms != 5000
            {
                std::env::set_var(
                    "XDP_GENERIC_CAPTURE_TIMEOUT_MS",
                    self.capture.handshake_timeout_ms.to_string(),
                );
            }

            // Clipboard protocol
            if std::env::var("XDP_GENERIC_CLIPBOARD_PROTOCOL").is_err()
                && self.clipboard.protocol != "auto"
            {
                std::env::set_var("XDP_GENERIC_CLIPBOARD_PROTOCOL", &self.clipboard.protocol);
            }
            if std::env::var("XDP_GENERIC_CLIPBOARD_NO_FALLBACK").is_err()
                && !self.clipboard.allow_fallback
            {
                std::env::set_var("XDP_GENERIC_CLIPBOARD_NO_FALLBACK", "1");
            }
        }
    }

    /// Override config with CLI arguments
    pub fn with_overrides(mut self, listen: Option<String>, port: u16) -> Self {
        if let Some(listen_addr) = listen {
            self.server.listen_addr = format!("{listen_addr}:{port}");
        } else if let Ok(mut addr) = self.server.listen_addr.parse::<SocketAddr>() {
            addr.set_port(port);
            self.server.listen_addr = addr.to_string();
        }

        self
    }

    /// Convert server configuration to Portal configuration
    ///
    /// Maps relevant server settings to `lamco_portal::PortalConfig` for
    /// screen capture and input injection via XDG Desktop Portals.
    ///
    /// Portal RemoteDesktop always requests Keyboard + Pointer devices
    /// so the compositor grants both input types regardless of which
    /// injection protocol the server ends up using.
    pub fn to_portal_config(&self) -> lamco_portal::PortalConfig {
        // Map cursor mode from string to enum
        let cursor_mode = match self.video.cursor_mode.to_lowercase().as_str() {
            "embedded" => CursorMode::Embedded,
            "hidden" => CursorMode::Hidden,
            _ => CursorMode::Metadata, // Default for "metadata" or invalid
        };

        // Always request both keyboard and pointer from the portal.
        // The injection protocol (libei vs wlr) is decided separately.
        let mut devices: BitFlags<DeviceType> = DeviceType::Keyboard | DeviceType::Pointer;
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
        Self {
            config_version: CURRENT_CONFIG_VERSION,
            server: ServerConfig::default(),
            security: SecurityConfig::default(),
            video: VideoConfig::default(),
            video_pipeline: VideoPipelineConfig::default(),
            capture: CaptureProtocolConfig::default(),
            input: InputConfig::default(),
            clipboard: ClipboardConfig::default(),
            multimon: MultiMonitorConfig::default(),
            performance: PerformanceConfig::default(),
            logging: LoggingConfig::default(),
            egfx: EgfxConfig::default(),
            damage_tracking: DamageTrackingConfig::default(),
            hardware_encoding: HardwareEncodingConfig::default(),
            display: DisplayConfig::default(),
            advanced_video: AdvancedVideoConfig::default(),
            cursor: CursorConfig::default(),
            audio: AudioConfig::default(),
            notifications: NotificationConfig::default(),
            gui_state: GuiStateConfig::default(),
        }
    }
}

/// Infer a TOML value type from an environment variable string.
///
/// Tries bool → integer → float → string, matching the coercion
/// order that layered config libraries use.
fn infer_toml_value(s: &str) -> toml::Value {
    if let Ok(b) = s.parse::<bool>() {
        return toml::Value::Boolean(b);
    }
    if let Ok(i) = s.parse::<i64>() {
        return toml::Value::Integer(i);
    }
    if let Ok(f) = s.parse::<f64>() {
        return toml::Value::Float(f);
    }
    toml::Value::String(s.to_string())
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

    #[test]
    fn test_infer_toml_value_bool() {
        assert_eq!(infer_toml_value("true"), toml::Value::Boolean(true));
        assert_eq!(infer_toml_value("false"), toml::Value::Boolean(false));
    }

    #[test]
    fn test_infer_toml_value_integer() {
        assert_eq!(infer_toml_value("42"), toml::Value::Integer(42));
        assert_eq!(infer_toml_value("0"), toml::Value::Integer(0));
        assert_eq!(infer_toml_value("-1"), toml::Value::Integer(-1));
    }

    #[test]
    fn test_infer_toml_value_float() {
        assert_eq!(infer_toml_value("3.14"), toml::Value::Float(3.14));
    }

    #[test]
    fn test_infer_toml_value_string() {
        assert_eq!(
            infer_toml_value("hello"),
            toml::Value::String("hello".into())
        );
        assert_eq!(
            infer_toml_value("0.0.0.0:3389"),
            toml::Value::String("0.0.0.0:3389".into())
        );
    }

    #[test]
    fn test_infer_toml_value_ordering() {
        // "0" parses as integer, not bool or string
        assert_eq!(infer_toml_value("0"), toml::Value::Integer(0));
        // "1" parses as integer, not bool
        assert_eq!(infer_toml_value("1"), toml::Value::Integer(1));
        // "1.0" parses as float, not string
        assert_eq!(infer_toml_value("1.0"), toml::Value::Float(1.0));
        // IP:port stays string (colon prevents numeric parse)
        assert!(matches!(
            infer_toml_value("127.0.0.1:3389"),
            toml::Value::String(_)
        ));
    }
}
