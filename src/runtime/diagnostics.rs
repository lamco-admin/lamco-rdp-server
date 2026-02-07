//! System Diagnostics and Status Reporting
//!
//! Provides runtime diagnostics, status reporting, and system information
//! for debugging and monitoring.

use std::time::{Duration, Instant};
use sysinfo::System;
use tracing::info;

/// System information for diagnostics
#[derive(Debug, Clone)]
pub struct SystemInfo {
    /// Operating system name (e.g., "Linux", "Ubuntu")
    pub os_name: String,
    /// Operating system version string
    pub os_version: String,

    /// Kernel version string
    pub kernel_version: String,

    /// Number of logical CPU cores
    pub cpu_count: usize,

    /// Total system memory in megabytes
    pub total_memory_mb: u64,

    /// System hostname
    pub hostname: String,
}

impl SystemInfo {
    /// Gather system information
    pub fn gather() -> Self {
        let mut sys = System::new_all();
        sys.refresh_all();

        Self {
            os_name: System::name().unwrap_or_else(|| "Unknown".to_string()),
            os_version: System::os_version().unwrap_or_else(|| "Unknown".to_string()),
            kernel_version: System::kernel_version().unwrap_or_else(|| "Unknown".to_string()),
            cpu_count: sys.cpus().len(),
            total_memory_mb: sys.total_memory() / 1024 / 1024,
            hostname: System::host_name().unwrap_or_else(|| "Unknown".to_string()),
        }
    }

    /// Log system information
    pub fn log(&self) {
        info!("=== System Information ===");
        info!("  OS: {} {}", self.os_name, self.os_version);
        info!("  Kernel: {}", self.kernel_version);
        info!("  Hostname: {}", self.hostname);
        info!("  CPUs: {}", self.cpu_count);
        info!("  Memory: {} MB", self.total_memory_mb);
    }
}

/// Runtime statistics for server
#[derive(Debug, Clone)]
pub struct RuntimeStats {
    /// Server start time
    pub start_time: Instant,

    /// Active connections
    pub active_connections: usize,

    /// Total connections served
    pub total_connections: u64,

    /// Current FPS
    pub current_fps: f64,

    /// Average latency in ms
    pub avg_latency_ms: f64,

    /// Memory usage in MB
    pub memory_usage_mb: u64,

    /// CPU usage percent
    pub cpu_usage_percent: f64,
}

impl RuntimeStats {
    /// Create new runtime stats
    pub fn new() -> Self {
        Self {
            start_time: Instant::now(),
            active_connections: 0,
            total_connections: 0,
            current_fps: 0.0,
            avg_latency_ms: 0.0,
            memory_usage_mb: 0,
            cpu_usage_percent: 0.0,
        }
    }

    /// Get uptime
    pub fn uptime(&self) -> Duration {
        self.start_time.elapsed()
    }

    /// Format uptime as string
    pub fn uptime_string(&self) -> String {
        let secs = self.uptime().as_secs();
        let hours = secs / 3600;
        let minutes = (secs % 3600) / 60;
        let seconds = secs % 60;
        format!("{:02}:{:02}:{:02}", hours, minutes, seconds)
    }

    /// Log current status
    pub fn log_status(&self) {
        info!("=== Server Status ===");
        info!("  Uptime: {}", self.uptime_string());
        info!("  Active connections: {}", self.active_connections);
        info!("  Total connections: {}", self.total_connections);
        info!("  Current FPS: {:.1}", self.current_fps);
        info!("  Avg latency: {:.1} ms", self.avg_latency_ms);
        info!("  Memory: {} MB", self.memory_usage_mb);
        info!("  CPU: {:.1}%", self.cpu_usage_percent);
    }
}

impl Default for RuntimeStats {
    fn default() -> Self {
        Self::new()
    }
}

/// Detect Wayland compositor
pub fn detect_compositor() -> Option<String> {
    // Try to get compositor from environment
    if let Ok(compositor) = std::env::var("XDG_CURRENT_DESKTOP") {
        return Some(compositor);
    }

    // Try to detect from Wayland display
    if let Ok(display) = std::env::var("WAYLAND_DISPLAY") {
        return Some(format!("Wayland ({})", display));
    }

    None
}

/// Detect Portal backend
pub fn detect_portal_backend() -> Option<String> {
    // Check which portal backend is installed
    let backends = vec![
        ("/usr/libexec/xdg-desktop-portal-gnome", "GNOME"),
        ("/usr/libexec/xdg-desktop-portal-kde", "KDE"),
        ("/usr/libexec/xdg-desktop-portal-wlr", "wlroots"),
        ("/usr/lib/xdg-desktop-portal-gnome", "GNOME"),
        ("/usr/lib/xdg-desktop-portal-kde", "KDE"),
    ];

    for (path, name) in backends {
        if std::path::Path::new(path).exists() {
            return Some(name.to_string());
        }
    }

    None
}

/// Get PipeWire version
pub fn get_pipewire_version() -> Option<String> {
    // Try to execute pipewire --version
    std::process::Command::new("pipewire")
        .arg("--version")
        .output()
        .ok()
        .and_then(|output| {
            String::from_utf8(output.stdout)
                .ok()
                .map(|s| s.trim().to_string())
        })
}

/// Log complete diagnostics on startup
pub fn log_startup_diagnostics() {
    info!("╔════════════════════════════════════════════════════════════╗");
    info!("║          Startup Diagnostics                              ║");
    info!("╚════════════════════════════════════════════════════════════╝");

    // System info
    let sys_info = SystemInfo::gather();
    sys_info.log();

    // Environment
    info!("=== Environment ===");
    if let Some(compositor) = detect_compositor() {
        info!("  Compositor: {}", compositor);
    } else {
        info!("  Compositor: Unknown (not in Wayland session?)");
    }

    if let Some(portal) = detect_portal_backend() {
        info!("  Portal Backend: {}", portal);
    } else {
        info!("  Portal Backend: Not detected");
    }

    if let Some(pw_version) = get_pipewire_version() {
        info!("  PipeWire: {}", pw_version);
    } else {
        info!("  PipeWire: Not found in PATH");
    }

    info!("=== Server Configuration ===");
    info!("  Version: {}", env!("CARGO_PKG_VERSION"));
    #[cfg(debug_assertions)]
    info!("  Build: debug");
    #[cfg(not(debug_assertions))]
    info!("  Build: release");

    info!("╚════════════════════════════════════════════════════════════╝");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_system_info_gather() {
        let info = SystemInfo::gather();
        assert!(!info.os_name.is_empty());
        assert!(info.cpu_count > 0);
        assert!(info.total_memory_mb > 0);
    }

    #[test]
    fn test_runtime_stats() {
        let stats = RuntimeStats::new();
        assert_eq!(stats.active_connections, 0);
        assert!(stats.uptime().as_secs() < 1);
    }

    #[test]
    fn test_uptime_string_format() {
        let stats = RuntimeStats {
            start_time: Instant::now() - Duration::from_secs(3661),
            ..Default::default()
        };
        let uptime = stats.uptime_string();
        // Should be "01:01:01" or close
        assert!(uptime.contains(':'));
    }
}
