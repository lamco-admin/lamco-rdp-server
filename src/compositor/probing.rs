//! Compositor detection and capability probing
//!
//! This module implements the actual detection logic to identify
//! the running compositor and probe its capabilities.

use std::{fs, process::Command};

use anyhow::Result;
use tracing::{debug, info, warn};

use super::{
    capabilities::{CompositorCapabilities, CompositorType, WaylandGlobal},
    portal_caps::PortalCapabilities,
};

/// Probe all compositor capabilities
///
/// This is the main entry point for capability detection. It:
/// 1. Identifies the compositor from environment
/// 2. Probes Portal capabilities
/// 3. Optionally enumerates Wayland globals
/// 4. Generates a profile with recommended settings
///
/// # Example
///
/// ```no_run
/// use lamco_rdp_server::compositor::probe_capabilities;
///
/// async fn check_compositor() -> anyhow::Result<()> {
///     let caps = probe_capabilities().await?;
///     println!("Running on: {}", caps.compositor);
///     Ok(())
/// }
/// ```
pub async fn probe_capabilities() -> Result<CompositorCapabilities> {
    info!("Probing compositor capabilities...");

    // Step 1: Identify compositor from environment
    let compositor = identify_compositor();
    info!("Detected compositor: {}", compositor);

    // Step 2: Probe Portal capabilities
    let portal = match PortalCapabilities::probe().await {
        Ok(caps) => caps,
        Err(e) => {
            warn!("Failed to probe Portal capabilities: {}", e);
            PortalCapabilities::default()
        }
    };

    // Step 3: Enumerate Wayland globals (if possible)
    let wayland_globals = enumerate_wayland_globals().unwrap_or_default();
    debug!("Found {} Wayland globals", wayland_globals.len());

    // Step 4: Create capability structure (includes profile generation and deployment detection)
    let mut capabilities = CompositorCapabilities::new(compositor, portal, wayland_globals);

    // Step 5: Detect credential storage (Phase 2)
    let (storage_method, encryption, accessible) =
        crate::session::detect_credential_storage(&capabilities.deployment).await;
    capabilities.credential_storage_method = storage_method;
    capabilities.credential_encryption = encryption;
    capabilities.credential_storage_accessible = accessible;

    debug!(
        "Credential storage detected: {} (encryption: {}, accessible: {})",
        storage_method, encryption, accessible
    );

    // Step 6: Detect clipboard manager (Klipper, CopyQ, etc.)
    let clipboard_manager =
        crate::services::DetectedSystemClipboardManager::detect(&capabilities.compositor).await;
    capabilities.clipboard_manager = Some(clipboard_manager);

    // Log summary
    capabilities.log_summary();

    Ok(capabilities)
}

/// Identify the running compositor
///
/// Detection order:
/// 1. Check XDG_CURRENT_DESKTOP (most reliable)
/// 2. Check DESKTOP_SESSION
/// 3. Check compositor-specific env vars
/// 4. Check for running processes
/// 5. Fall back to Unknown
pub fn identify_compositor() -> CompositorType {
    // Check XDG_CURRENT_DESKTOP first (most standardized)
    if let Ok(desktop) = std::env::var("XDG_CURRENT_DESKTOP") {
        let desktop_lower = desktop.to_lowercase();
        debug!("XDG_CURRENT_DESKTOP: {}", desktop);

        if desktop_lower.contains("gnome") {
            return CompositorType::Gnome {
                version: detect_gnome_version(),
            };
        }
        if desktop_lower.contains("kde") || desktop_lower.contains("plasma") {
            return CompositorType::Kde {
                version: detect_kde_version(),
            };
        }
        if desktop_lower.contains("sway") {
            return CompositorType::Sway {
                version: detect_sway_version(),
            };
        }
        if desktop_lower.contains("hyprland") {
            return CompositorType::Hyprland {
                version: detect_hyprland_version(),
            };
        }
        if desktop_lower.contains("cosmic") {
            return CompositorType::Cosmic;
        }
        if desktop_lower.contains("weston") {
            return CompositorType::Weston;
        }
    }

    // Check DESKTOP_SESSION
    if let Ok(session) = std::env::var("DESKTOP_SESSION") {
        let session_lower = session.to_lowercase();
        debug!("DESKTOP_SESSION: {}", session);

        if session_lower.contains("gnome") || session_lower.contains("ubuntu") {
            return CompositorType::Gnome {
                version: detect_gnome_version(),
            };
        }
        if session_lower.contains("plasma") || session_lower.contains("kde") {
            return CompositorType::Kde {
                version: detect_kde_version(),
            };
        }
        if session_lower.contains("sway") {
            return CompositorType::Sway {
                version: detect_sway_version(),
            };
        }
    }

    // Check compositor-specific environment variables
    if std::env::var("SWAYSOCK").is_ok() {
        return CompositorType::Sway {
            version: detect_sway_version(),
        };
    }

    if std::env::var("HYPRLAND_INSTANCE_SIGNATURE").is_ok() {
        return CompositorType::Hyprland {
            version: detect_hyprland_version(),
        };
    }

    // Check for running compositor processes
    if is_process_running("gnome-shell") {
        return CompositorType::Gnome {
            version: detect_gnome_version(),
        };
    }

    if is_process_running("kwin_wayland") {
        return CompositorType::Kde {
            version: detect_kde_version(),
        };
    }

    if is_process_running("sway") {
        return CompositorType::Sway {
            version: detect_sway_version(),
        };
    }

    if is_process_running("Hyprland") {
        return CompositorType::Hyprland {
            version: detect_hyprland_version(),
        };
    }

    if is_process_running("weston") {
        return CompositorType::Weston;
    }

    if is_process_running("cosmic-comp") {
        return CompositorType::Cosmic;
    }

    // Check for any wlroots-based compositor
    if let Some(name) = detect_wlroots_compositor() {
        return CompositorType::Wlroots { name };
    }

    // Fall back to unknown with whatever info we have
    let session_info = std::env::var("XDG_CURRENT_DESKTOP")
        .or_else(|_| std::env::var("DESKTOP_SESSION"))
        .ok();

    warn!("Could not identify compositor, using fallback");
    CompositorType::Unknown { session_info }
}

/// Detect GNOME Shell version
fn detect_gnome_version() -> Option<String> {
    // Try gnome-shell --version
    Command::new("gnome-shell")
        .arg("--version")
        .output()
        .ok()
        .and_then(|output| {
            if output.status.success() {
                let stdout = String::from_utf8_lossy(&output.stdout);
                // Output is like "GNOME Shell 46.0"
                stdout
                    .split_whitespace()
                    .last()
                    .map(std::string::ToString::to_string)
            } else {
                None
            }
        })
}

/// Detect KDE Plasma version
fn detect_kde_version() -> Option<String> {
    // Try plasmashell --version
    Command::new("plasmashell")
        .arg("--version")
        .output()
        .ok()
        .and_then(|output| {
            if output.status.success() {
                let stdout = String::from_utf8_lossy(&output.stdout);
                // Output is like "plasmashell 6.0.0"
                stdout
                    .split_whitespace()
                    .last()
                    .map(std::string::ToString::to_string)
            } else {
                None
            }
        })
}

/// Detect Sway version
fn detect_sway_version() -> Option<String> {
    Command::new("sway")
        .arg("--version")
        .output()
        .ok()
        .and_then(|output| {
            if output.status.success() {
                let stdout = String::from_utf8_lossy(&output.stdout);
                // Output is like "sway version 1.9"
                stdout
                    .split_whitespace()
                    .last()
                    .map(std::string::ToString::to_string)
            } else {
                None
            }
        })
}

/// Detect Hyprland version
fn detect_hyprland_version() -> Option<String> {
    Command::new("hyprctl")
        .arg("version")
        .output()
        .ok()
        .and_then(|output| {
            if output.status.success() {
                let stdout = String::from_utf8_lossy(&output.stdout);
                // Parse version from hyprctl output
                for line in stdout.lines() {
                    if line.starts_with("Hyprland") || line.contains("version") {
                        return line
                            .split_whitespace()
                            .find(|s| s.chars().next().is_some_and(|c| c.is_ascii_digit()))
                            .map(std::string::ToString::to_string);
                    }
                }
                None
            } else {
                None
            }
        })
}

/// Check if a process is running
fn is_process_running(name: &str) -> bool {
    Command::new("pgrep")
        .arg("-x")
        .arg(name)
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

/// Detect wlroots-based compositor from running processes
fn detect_wlroots_compositor() -> Option<String> {
    // Common wlroots-based compositors
    const WLROOTS_COMPOSITORS: &[&str] =
        &["labwc", "wayfire", "river", "dwl", "cage", "hikari", "phoc"];

    for compositor in WLROOTS_COMPOSITORS {
        if is_process_running(compositor) {
            return Some((*compositor).to_string());
        }
    }

    None
}

/// OS release information from /etc/os-release
#[derive(Debug, Clone, Default)]
pub struct OsRelease {
    /// Distribution ID (e.g., "rhel", "fedora", "ubuntu", "debian")
    pub id: String,
    /// Version ID (e.g., "9", "40", "24.04")
    pub version_id: String,
    /// Full name (e.g., "Red Hat Enterprise Linux 9.4")
    pub name: String,
    /// Pretty name for display
    pub pretty_name: String,
    /// ID-like chain (e.g., "rhel fedora" for RHEL)
    pub id_like: Vec<String>,
}

impl OsRelease {
    /// Check if this OS is RHEL or a RHEL derivative
    pub fn is_rhel_family(&self) -> bool {
        self.id == "rhel" || self.id_like.iter().any(|s| s == "rhel")
    }

    /// Check if this is specifically RHEL 9.x
    pub fn is_rhel9(&self) -> bool {
        self.id == "rhel" && self.version_id.starts_with('9')
    }

    /// Check if this is RHEL 8.x
    pub fn is_rhel8(&self) -> bool {
        self.id == "rhel" && self.version_id.starts_with('8')
    }

    /// Get major version as integer
    pub fn major_version(&self) -> Option<u32> {
        self.version_id
            .split('.')
            .next()
            .and_then(|v| v.parse().ok())
    }
}

/// Detect OS release information from /etc/os-release
///
/// This parses the standard os-release file to identify the Linux distribution
/// and version. This is critical for platform-specific quirks like the
/// AVC444 blur issue on RHEL 9.
///
/// # Returns
///
/// Returns `Some(OsRelease)` with distribution info, or `None` if detection fails.
///
/// # Example
///
/// ```no_run
/// use lamco_rdp_server::compositor::probing::detect_os_release;
///
/// if let Some(os) = detect_os_release() {
///     if os.is_rhel9() {
///         println!("Running on RHEL 9 - AVC444 quirks apply");
///     }
/// }
/// ```
pub fn detect_os_release() -> Option<OsRelease> {
    // Try /etc/os-release first (standard location)
    let content = fs::read_to_string("/etc/os-release")
        .or_else(|_| fs::read_to_string("/usr/lib/os-release"))
        .ok()?;

    let mut release = OsRelease::default();

    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        if let Some((key, value)) = line.split_once('=') {
            // Remove quotes from value
            let value = value.trim_matches('"').trim_matches('\'');

            match key {
                "ID" => release.id = value.to_lowercase(),
                "VERSION_ID" => release.version_id = value.to_string(),
                "NAME" => release.name = value.to_string(),
                "PRETTY_NAME" => release.pretty_name = value.to_string(),
                "ID_LIKE" => {
                    release.id_like = value.split_whitespace().map(str::to_lowercase).collect();
                }
                _ => {}
            }
        }
    }

    if release.id.is_empty() {
        debug!("Could not parse OS ID from os-release");
        return None;
    }

    debug!(
        "Detected OS: {} {} (ID_LIKE: {:?})",
        release.id, release.version_id, release.id_like
    );

    Some(release)
}

/// Enumerate Wayland globals via wlr-randr or similar tools
///
/// Note: This is a best-effort enumeration. Full Wayland global
/// enumeration would require linking wayland-client and doing
/// a registry roundtrip. For most use cases, the Portal-based
/// detection is sufficient.
fn enumerate_wayland_globals() -> Result<Vec<WaylandGlobal>> {
    // Try to get info from wlr-randr or other tools
    // This is a simplified implementation - full Wayland enumeration
    // would require wayland-client dependency

    let mut globals = Vec::new();

    // Check for wlr-randr (indicates wlroots protocols)
    if Command::new("which")
        .arg("wlr-randr")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
    {
        globals.push(WaylandGlobal {
            interface: "zwlr_screencopy_manager_v1".to_string(),
            version: 3,
            name: 0,
        });
    }

    // Check for slurp (indicates wlr protocols)
    if Command::new("which")
        .arg("slurp")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
    {
        globals.push(WaylandGlobal {
            interface: "zwlr_layer_shell_v1".to_string(),
            version: 4,
            name: 0,
        });
    }

    // Standard protocols we can assume exist in any Wayland compositor
    globals.push(WaylandGlobal {
        interface: "wl_compositor".to_string(),
        version: 5,
        name: 1,
    });
    globals.push(WaylandGlobal {
        interface: "wl_shm".to_string(),
        version: 1,
        name: 2,
    });
    globals.push(WaylandGlobal {
        interface: "xdg_wm_base".to_string(),
        version: 5,
        name: 3,
    });

    Ok(globals)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_identify_compositor() {
        // This test depends on the actual environment
        // Just verify it doesn't panic and returns something
        let compositor = identify_compositor();
        println!("Detected compositor: {:?}", compositor);
    }

    #[test]
    fn test_compositor_type_display() {
        let gnome = CompositorType::Gnome {
            version: Some("46.0".to_string()),
        };
        assert_eq!(gnome.to_string(), "GNOME 46.0");

        let unknown = CompositorType::Unknown { session_info: None };
        assert_eq!(unknown.to_string(), "Unknown");
    }

    #[test]
    fn test_enumerate_wayland_globals() {
        // Should not panic, even if tools aren't available
        let globals = enumerate_wayland_globals().unwrap_or_default();
        // We always add some standard protocols
        assert!(!globals.is_empty());
    }
}
