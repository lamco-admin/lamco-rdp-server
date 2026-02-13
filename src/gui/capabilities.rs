//! Capabilities Detection Module
//!
//! Detects system capabilities by running the server binary with --show-capabilities
//! and parsing the JSON output.

use std::{path::PathBuf, process::Command, time::SystemTime};

use crate::gui::state::{
    DeploymentContext, DetectedCapabilities, PlatformQuirk, ServiceInfo, ServiceLevel,
};

/// Detect system capabilities by running the server binary
pub fn detect_capabilities() -> Result<DetectedCapabilities, String> {
    let server_binary = find_server_binary()?;

    let output = Command::new(&server_binary)
        .arg("--show-capabilities")
        .arg("--format=json")
        .output()
        .map_err(|e| format!("Failed to run server binary: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("Server binary failed: {}", stderr));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);

    parse_capabilities_json(&stdout)
}

/// Find the server binary in common locations
fn find_server_binary() -> Result<PathBuf, String> {
    // Check locations in order of preference
    let candidates = [
        // Same directory as GUI binary
        std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.join("lamco-rdp-server"))),
        // System installation
        Some(PathBuf::from("/usr/bin/lamco-rdp-server")),
        Some(PathBuf::from("/usr/local/bin/lamco-rdp-server")),
        // Development target directory
        Some(PathBuf::from("./target/release/lamco-rdp-server")),
        Some(PathBuf::from("./target/debug/lamco-rdp-server")),
    ];

    for candidate in candidates.into_iter().flatten() {
        if candidate.exists() && candidate.is_file() {
            return Ok(candidate);
        }
    }

    Err("Could not find lamco-rdp-server binary".to_string())
}

/// Parse capabilities JSON output
fn parse_capabilities_json(json_str: &str) -> Result<DetectedCapabilities, String> {
    let json: serde_json::Value =
        serde_json::from_str(json_str).map_err(|e| format!("Failed to parse JSON: {}", e))?;

    let system = json
        .get("system")
        .ok_or("Missing 'system' section in capabilities")?;

    let compositor_name = system
        .get("compositor")
        .and_then(|v| v.as_str())
        .unwrap_or("Unknown")
        .to_string();

    let compositor_version = system
        .get("compositor_version")
        .and_then(|v| v.as_str())
        .map(String::from);

    let distribution = system
        .get("distribution")
        .and_then(|v| v.as_str())
        .unwrap_or("Unknown")
        .to_string();

    let kernel_version = system
        .get("kernel")
        .and_then(|v| v.as_str())
        .unwrap_or("Unknown")
        .to_string();

    let portals = json.get("portals").unwrap_or(&serde_json::Value::Null);

    let portal_version = portals.get("version").and_then(|v| v.as_u64()).unwrap_or(0) as u32;

    let portal_backend = portals
        .get("backend")
        .and_then(|v| v.as_str())
        .unwrap_or("Unknown")
        .to_string();

    let screencast_version = portals
        .get("screencast_version")
        .and_then(|v| v.as_u64())
        .map(|v| v as u32);

    let remote_desktop_version = portals
        .get("remote_desktop_version")
        .and_then(|v| v.as_u64())
        .map(|v| v as u32);

    let secret_portal_version = portals
        .get("secret_version")
        .and_then(|v| v.as_u64())
        .map(|v| v as u32);

    let deployment = json.get("deployment").unwrap_or(&serde_json::Value::Null);

    let deployment_context = parse_deployment_context(
        deployment
            .get("context")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown"),
        deployment.get("linger").and_then(|v| v.as_bool()),
    );

    let xdg_runtime_dir = PathBuf::from(
        deployment
            .get("xdg_runtime_dir")
            .and_then(|v| v.as_str())
            .unwrap_or("/run/user/1000"),
    );

    let quirks = json
        .get("quirks")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|q| {
                    Some(PlatformQuirk {
                        quirk_id: q.get("id")?.as_str()?.to_string(),
                        description: q.get("description")?.as_str()?.to_string(),
                        impact: q
                            .get("impact")
                            .and_then(|v| v.as_str())
                            .unwrap_or("Unknown")
                            .to_string(),
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    let persistence = json.get("persistence").unwrap_or(&serde_json::Value::Null);

    let persistence_strategy = persistence
        .get("strategy")
        .and_then(|v| v.as_str())
        .unwrap_or("Unknown")
        .to_string();

    let persistence_notes = persistence
        .get("notes")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|n| n.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    let services: Vec<ServiceInfo> = json
        .get("services")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(parse_service_info).collect())
        .unwrap_or_default();

    let guaranteed_count = services
        .iter()
        .filter(|s| s.level == ServiceLevel::Guaranteed)
        .count();
    let best_effort_count = services
        .iter()
        .filter(|s| s.level == ServiceLevel::BestEffort)
        .count();
    let degraded_count = services
        .iter()
        .filter(|s| s.level == ServiceLevel::Degraded)
        .count();
    let unavailable_count = services
        .iter()
        .filter(|s| s.level == ServiceLevel::Unavailable)
        .count();

    let hints = json.get("hints").unwrap_or(&serde_json::Value::Null);

    let recommended_fps = hints
        .get("recommended_fps")
        .and_then(|v| v.as_u64())
        .map(|v| v as u32);

    let recommended_codec = hints
        .get("recommended_codec")
        .and_then(|v| v.as_str())
        .map(String::from);

    let zero_copy_available = hints
        .get("zero_copy")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    // Derive available auth methods from services
    // PAM is available if PamAuthentication service is at least Degraded level
    let available_auth_methods = derive_auth_methods(&services);

    Ok(DetectedCapabilities {
        compositor_name,
        compositor_version,
        distribution,
        kernel_version,
        portal_version,
        portal_backend,
        screencast_version,
        remote_desktop_version,
        secret_portal_version,
        deployment_context,
        xdg_runtime_dir,
        quirks,
        persistence_strategy,
        persistence_notes,
        services,
        guaranteed_count,
        best_effort_count,
        degraded_count,
        unavailable_count,
        recommended_fps,
        recommended_codec,
        zero_copy_available,
        available_auth_methods,
        detected_at: SystemTime::now(),
    })
}

/// Parse deployment context from string
fn parse_deployment_context(context: &str, linger: Option<bool>) -> DeploymentContext {
    match context.to_lowercase().as_str() {
        "native" => DeploymentContext::Native,
        "flatpak" => DeploymentContext::Flatpak,
        "systemd-user" => DeploymentContext::SystemdUser {
            linger: linger.unwrap_or(false),
        },
        "systemd-system" | "systemd" => DeploymentContext::SystemdSystem,
        "initd" | "init.d" => DeploymentContext::InitD,
        _ => DeploymentContext::Unknown,
    }
}

/// Derive available authentication methods from services list
///
/// Checks PamAuthentication service level to determine if PAM is available.
/// "none" is always available and is the default (listed first).
fn derive_auth_methods(services: &[ServiceInfo]) -> Vec<String> {
    let mut methods = Vec::new();

    // "none" is always available and is the default
    methods.push("none".to_string());

    // Check if PAM authentication is available (at least Degraded level)
    let pam_available = services.iter().any(|s| {
        (s.id == "PamAuthentication" || s.id == "pam_authentication")
            && s.level != ServiceLevel::Unavailable
    });

    if pam_available {
        methods.push("pam".to_string());
    }

    methods
}

/// Parse a single service info from JSON
fn parse_service_info(value: &serde_json::Value) -> Option<ServiceInfo> {
    let id = value.get("id")?.as_str()?.to_string();
    let name = value.get("name")?.as_str()?.to_string();

    let level_str = value.get("level")?.as_str()?;
    let level = match level_str.to_lowercase().as_str() {
        "guaranteed" => ServiceLevel::Guaranteed,
        "best_effort" | "besteffort" => ServiceLevel::BestEffort,
        "degraded" => ServiceLevel::Degraded,
        "unavailable" => ServiceLevel::Unavailable,
        _ => ServiceLevel::Unavailable,
    };

    let wayland_source = value
        .get("wayland_source")
        .and_then(|v| v.as_str())
        .map(String::from);

    let rdp_capability = value
        .get("rdp_capability")
        .and_then(|v| v.as_str())
        .map(String::from);

    let notes = value
        .get("notes")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|n| n.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    Some(ServiceInfo {
        id,
        name,
        level,
        level_emoji: level.emoji().to_string(),
        wayland_source,
        rdp_capability,
        notes,
    })
}

/// Export capabilities to a JSON file
pub fn export_capabilities(
    caps: &DetectedCapabilities,
    path: &std::path::Path,
) -> Result<(), String> {
    let json = capabilities_to_json(caps)?;

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create directory: {}", e))?;
    }

    std::fs::write(path, json).map_err(|e| format!("Failed to write file: {}", e))?;

    Ok(())
}

/// Convert capabilities to JSON string
fn capabilities_to_json(caps: &DetectedCapabilities) -> Result<String, String> {
    let json = serde_json::json!({
        "system": {
            "compositor": caps.compositor_name,
            "compositor_version": caps.compositor_version,
            "distribution": caps.distribution,
            "kernel": caps.kernel_version,
        },
        "portals": {
            "version": caps.portal_version,
            "backend": caps.portal_backend,
            "screencast_version": caps.screencast_version,
            "remote_desktop_version": caps.remote_desktop_version,
            "secret_version": caps.secret_portal_version,
        },
        "deployment": {
            "context": format!("{}", caps.deployment_context),
            "xdg_runtime_dir": caps.xdg_runtime_dir.display().to_string(),
        },
        "persistence": {
            "strategy": caps.persistence_strategy,
            "notes": caps.persistence_notes,
        },
        "quirks": caps.quirks.iter().map(|q| {
            serde_json::json!({
                "id": q.quirk_id,
                "description": q.description,
                "impact": q.impact,
            })
        }).collect::<Vec<_>>(),
        "services": caps.services.iter().map(|s| {
            serde_json::json!({
                "id": s.id,
                "name": s.name,
                "level": format!("{}", s.level),
                "wayland_source": s.wayland_source,
                "rdp_capability": s.rdp_capability,
                "notes": s.notes,
            })
        }).collect::<Vec<_>>(),
        "summary": {
            "guaranteed": caps.guaranteed_count,
            "best_effort": caps.best_effort_count,
            "degraded": caps.degraded_count,
            "unavailable": caps.unavailable_count,
        },
        "hints": {
            "recommended_fps": caps.recommended_fps,
            "recommended_codec": caps.recommended_codec,
            "zero_copy": caps.zero_copy_available,
        },
        "authentication": {
            "available_methods": caps.available_auth_methods,
        },
        "detected_at": format!("{:?}", caps.detected_at),
    });

    serde_json::to_string_pretty(&json).map_err(|e| format!("Failed to serialize: {}", e))
}

/// Detect capabilities without running the binary (mock for testing/demo)
pub fn detect_capabilities_mock() -> DetectedCapabilities {
    DetectedCapabilities {
        compositor_name: "GNOME Shell".to_string(),
        compositor_version: Some("46.0".to_string()),
        distribution: "Ubuntu 24.04 LTS".to_string(),
        kernel_version: "6.8.0-40-generic".to_string(),
        portal_version: 1,
        portal_backend: "gnome".to_string(),
        screencast_version: Some(5),
        remote_desktop_version: Some(2),
        secret_portal_version: Some(1),
        deployment_context: DeploymentContext::Native,
        xdg_runtime_dir: PathBuf::from("/run/user/1000"),
        quirks: vec![],
        persistence_strategy: "Secret Service".to_string(),
        persistence_notes: vec!["GNOME Keyring available".to_string()],
        services: vec![
            ServiceInfo {
                id: "screen_capture".to_string(),
                name: "Screen Capture".to_string(),
                level: ServiceLevel::Guaranteed,
                level_emoji: "✅".to_string(),
                wayland_source: Some("ScreenCast Portal v5".to_string()),
                rdp_capability: Some("MS-RDPEGFX".to_string()),
                notes: vec![],
            },
            ServiceInfo {
                id: "keyboard_input".to_string(),
                name: "Keyboard Input".to_string(),
                level: ServiceLevel::Guaranteed,
                level_emoji: "✅".to_string(),
                wayland_source: Some("RemoteDesktop Portal v2".to_string()),
                rdp_capability: Some("Input PDUs".to_string()),
                notes: vec![],
            },
            ServiceInfo {
                id: "pointer_input".to_string(),
                name: "Pointer Input".to_string(),
                level: ServiceLevel::Guaranteed,
                level_emoji: "✅".to_string(),
                wayland_source: Some("RemoteDesktop Portal v2".to_string()),
                rdp_capability: Some("Input PDUs".to_string()),
                notes: vec![],
            },
            ServiceInfo {
                id: "clipboard".to_string(),
                name: "Clipboard Sync".to_string(),
                level: ServiceLevel::BestEffort,
                level_emoji: "🔶".to_string(),
                wayland_source: Some("Portal Clipboard".to_string()),
                rdp_capability: Some("CLIPRDR".to_string()),
                notes: vec!["File transfer requires Portal v46+".to_string()],
            },
            ServiceInfo {
                id: "audio".to_string(),
                name: "Audio Playback".to_string(),
                level: ServiceLevel::Unavailable,
                level_emoji: "❌".to_string(),
                wayland_source: None,
                rdp_capability: Some("RDPSND".to_string()),
                notes: vec!["Not yet implemented".to_string()],
            },
        ],
        guaranteed_count: 3,
        best_effort_count: 1,
        degraded_count: 0,
        unavailable_count: 1,
        recommended_fps: Some(30),
        recommended_codec: Some("avc420".to_string()),
        zero_copy_available: true,
        available_auth_methods: vec!["pam".to_string(), "none".to_string()],
        detected_at: SystemTime::now(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_deployment_context() {
        assert!(matches!(
            parse_deployment_context("native", None),
            DeploymentContext::Native
        ));
        assert!(matches!(
            parse_deployment_context("flatpak", None),
            DeploymentContext::Flatpak
        ));
        assert!(matches!(
            parse_deployment_context("systemd-user", Some(true)),
            DeploymentContext::SystemdUser { linger: true }
        ));
    }

    #[test]
    fn test_capabilities_mock() {
        let caps = detect_capabilities_mock();
        assert_eq!(caps.compositor_name, "GNOME Shell");
        assert_eq!(caps.guaranteed_count, 3);
        assert_eq!(caps.services.len(), 5);
    }

    #[test]
    fn test_capabilities_to_json() {
        let caps = detect_capabilities_mock();
        let json = capabilities_to_json(&caps).unwrap();
        assert!(json.contains("GNOME Shell"));
        assert!(json.contains("Screen Capture"));
    }
}
