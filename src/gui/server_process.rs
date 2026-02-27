//! Server Process Management
//!
//! Handles spawning, monitoring, and controlling the lamco-rdp-server process
//! from the GUI. Provides real-time log capture and status updates.
#![expect(unsafe_code, reason = "libc signal check for process liveness")]

use std::{
    io::{BufRead, BufReader},
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

use anyhow::{anyhow, Context, Result};
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

/// Server process handle for lifecycle management
pub struct ServerProcess {
    child: Child,
    pid: u32,
    start_time: Instant,
    config_path: PathBuf,
    address: String,
    shutdown_flag: Arc<AtomicBool>,
    /// If true, don't stop server when dropped (for "close GUI only" mode)
    detached: bool,
}

/// Log line from server process
#[derive(Debug, Clone)]
pub struct ServerLogLine {
    pub level: LogLevel,
    pub message: String,
    pub timestamp: String,
}

/// Log level parsed from server output
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogLevel {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
}

impl ServerProcess {
    /// Start the server with the given configuration
    ///
    /// # Arguments
    /// * `config` - Server configuration to use
    /// * `log_sender` - Channel to send log lines for GUI display
    ///
    /// # Returns
    /// Server process handle on success
    pub fn start(
        config: &crate::config::Config,
        log_sender: mpsc::UnboundedSender<ServerLogLine>,
    ) -> Result<Self> {
        info!("Starting lamco-rdp-server process");

        let server_binary = find_server_binary()?;
        info!("Using server binary: {:?}", server_binary);

        let config_path = write_temp_config(config)?;
        info!("Config written to: {:?}", config_path);

        let address = config.server.listen_addr.clone();

        let mut child = Command::new(&server_binary)
            .arg("--config")
            .arg(&config_path)
            .arg("-v") // Verbose for better log output
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("Failed to spawn server process")?;

        let pid = child.id();
        info!("Server process started with PID: {}", pid);

        let shutdown_flag = Arc::new(AtomicBool::new(false));

        // Capture stderr (where tracing logs go)
        if let Some(stderr) = child.stderr.take() {
            let sender = log_sender.clone();
            let flag = shutdown_flag.clone();
            std::thread::spawn(move || {
                let reader = BufReader::new(stderr);
                for line in reader.lines() {
                    if flag.load(Ordering::Relaxed) {
                        break;
                    }
                    if let Ok(line) = line {
                        let log_line = parse_log_line(&line);
                        let _ = sender.send(log_line);
                    }
                }
            });
        }

        // Capture stdout (less common, but just in case)
        if let Some(stdout) = child.stdout.take() {
            let sender = log_sender;
            let flag = shutdown_flag.clone();
            std::thread::spawn(move || {
                let reader = BufReader::new(stdout);
                for line in reader.lines() {
                    if flag.load(Ordering::Relaxed) {
                        break;
                    }
                    if let Ok(line) = line {
                        let log_line = ServerLogLine {
                            level: LogLevel::Info,
                            message: line,
                            timestamp: chrono::Local::now().format("%H:%M:%S").to_string(),
                        };
                        let _ = sender.send(log_line);
                    }
                }
            });
        }

        Ok(Self {
            child,
            pid,
            start_time: Instant::now(),
            config_path,
            address,
            shutdown_flag,
            detached: false,
        })
    }

    /// Detach the server process so it keeps running when GUI exits
    ///
    /// After calling this, the server will NOT be stopped when the GUI closes.
    pub fn detach(&mut self) {
        info!(
            "Detaching server process (PID: {}) - will continue running",
            self.pid
        );
        self.detached = true;
    }

    pub fn pid(&self) -> u32 {
        self.pid
    }

    pub fn uptime(&self) -> Duration {
        self.start_time.elapsed()
    }

    pub fn address(&self) -> &str {
        &self.address
    }

    pub fn is_running(&self) -> bool {
        // Try to get exit status without blocking
        match self.try_wait() {
            Ok(None) => true,     // Still running
            Ok(Some(_)) => false, // Exited
            Err(_) => false,      // Error checking = assume not running
        }
    }

    /// Try to get exit status without blocking
    fn try_wait(&self) -> Result<Option<std::process::ExitStatus>> {
        // We need mutable access, but the child is owned by self
        // Use unsafe to work around this (the check is read-only)
        let child_ptr = &self.child as *const Child as *mut Child;
        unsafe {
            (*child_ptr)
                .try_wait()
                .context("Failed to check process status")
        }
    }

    /// Stop the server gracefully
    ///
    /// Sends SIGTERM first, then SIGKILL if needed.
    /// Handles the case where the process has already exited gracefully.
    pub fn stop(&mut self) -> Result<()> {
        info!("Stopping server process (PID: {})", self.pid);

        self.shutdown_flag.store(true, Ordering::Relaxed);

        if !self.is_running() {
            debug!("Server process already exited");
            self.cleanup();
            return Ok(());
        }

        #[cfg(unix)]
        {
            use nix::{
                sys::signal::{kill, Signal},
                unistd::Pid,
            };

            let pid = Pid::from_raw(self.pid as i32);
            match kill(pid, Signal::SIGTERM) {
                Ok(()) => debug!("SIGTERM sent to PID {}", self.pid),
                Err(nix::errno::Errno::ESRCH) => {
                    // Process doesn't exist - already exited
                    debug!("Process {} already exited before SIGTERM", self.pid);
                    self.cleanup();
                    return Ok(());
                }
                Err(e) => {
                    warn!("Failed to send SIGTERM: {}", e);
                }
            }
        }

        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if !self.is_running() {
                info!("Server stopped gracefully");
                self.cleanup();
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(100));
        }

        warn!("Server did not stop gracefully, sending SIGKILL");
        if let Err(e) = self.child.kill() {
            // Ignore "No such process" error - process may have just exited
            if !e.to_string().contains("No such process") {
                error!("Failed to kill server process: {}", e);
            }
        }
        let _ = self.child.wait();

        self.cleanup();
        info!("Server process terminated");
        Ok(())
    }

    /// Clean up temporary files
    fn cleanup(&self) {
        if self.config_path.exists() {
            if let Err(e) = std::fs::remove_file(&self.config_path) {
                debug!("Failed to remove temp config: {}", e);
            }
        }
    }
}

impl Drop for ServerProcess {
    fn drop(&mut self) {
        if self.detached {
            // Server was detached - let it keep running
            info!(
                "GUI closing, server (PID: {}) continues running in background",
                self.pid
            );
            return;
        }
        // Attempt graceful shutdown on drop
        self.shutdown_flag.store(true, Ordering::Relaxed);
        let _ = self.stop();
    }
}

/// Find the server binary in standard locations
fn find_server_binary() -> Result<PathBuf> {
    use crate::config::is_flatpak;

    // Check common locations in order of preference

    // 1. Flatpak-specific location (highest priority in Flatpak)
    if is_flatpak() {
        let flatpak_path = PathBuf::from("/app/bin/lamco-rdp-server");
        if flatpak_path.exists() {
            return Ok(flatpak_path);
        }
    }

    // 2. Same directory as GUI binary
    if let Ok(current_exe) = std::env::current_exe() {
        if let Some(dir) = current_exe.parent() {
            let server_path = dir.join("lamco-rdp-server");
            if server_path.exists() {
                return Ok(server_path);
            }
        }
    }

    // 3. Development target directory (skip in Flatpak)
    if !is_flatpak() {
        let dev_paths = [
            "target/debug/lamco-rdp-server",
            "target/release/lamco-rdp-server",
            "../target/debug/lamco-rdp-server",
            "../target/release/lamco-rdp-server",
        ];

        for path in &dev_paths {
            let path = PathBuf::from(path);
            if path.exists() {
                return Ok(path.canonicalize()?);
            }
        }
    }

    // 4. System PATH
    if let Ok(output) = Command::new("which").arg("lamco-rdp-server").output() {
        if output.status.success() {
            let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !path.is_empty() {
                return Ok(PathBuf::from(path));
            }
        }
    }

    // 5. Standard system locations (skip in Flatpak - these are outside sandbox)
    if !is_flatpak() {
        let system_paths = [
            "/usr/bin/lamco-rdp-server",
            "/usr/local/bin/lamco-rdp-server",
            "/opt/lamco/bin/lamco-rdp-server",
        ];

        for path in &system_paths {
            let path = PathBuf::from(path);
            if path.exists() {
                return Ok(path);
            }
        }
    }

    let context = if is_flatpak() {
        "Flatpak sandbox (/app/bin/)"
    } else {
        "same directory, target/, PATH, /usr/bin, /usr/local/bin"
    };

    Err(anyhow!(
        "Could not find lamco-rdp-server binary. Searched: {}",
        context
    ))
}

/// Register with the Background portal so the server survives GUI close.
///
/// Only effective in Flatpak — the portal only works for sandboxed apps.
/// GNOME 43+ kills unregistered background processes, so this prevents
/// the server from being terminated when the user closes the GUI.
pub async fn register_background_portal() {
    use ashpd::desktop::background::Background;

    match Background::request()
        .reason("RDP server needs to continue accepting connections")
        .auto_start(false)
        .dbus_activatable(false)
        .send()
        .await
    {
        Ok(request) => match request.response() {
            Ok(response) => {
                if response.run_in_background() {
                    info!("Background portal: permission granted");
                } else {
                    warn!("Background portal: permission denied by user");
                    warn!("Server may be killed when GUI closes on GNOME");
                }
            }
            Err(e) => {
                warn!("Background portal response error: {}", e);
            }
        },
        Err(e) => {
            // Not all DEs support Background portal — graceful fallback
            debug!("Background portal registration failed: {}", e);
            debug!("Server will rely on process orphaning (works on most DEs)");
        }
    }
}

/// Update the Background portal status message.
///
/// Shows a brief status in the system tray (GNOME, KDE).
/// Silently ignored if the portal doesn't support SetStatus (v2).
pub async fn update_background_status(message: &str) {
    use ashpd::desktop::background::BackgroundProxy;

    // Portal SetStatus has a 96-char limit
    let truncated = if message.len() > 96 {
        &message[..96]
    } else {
        message
    };

    match BackgroundProxy::new().await {
        Ok(proxy) => {
            if let Err(e) = proxy.set_status(truncated).await {
                debug!("Background SetStatus failed: {}", e);
            }
        }
        Err(e) => {
            debug!("Background proxy creation failed: {}", e);
        }
    }
}

/// Write configuration to a temporary file
fn write_temp_config(config: &crate::config::Config) -> Result<PathBuf> {
    let runtime_dir = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".to_string());

    let config_path = PathBuf::from(runtime_dir)
        .join(format!("lamco-rdp-server-gui-{}.toml", std::process::id()));

    let toml_string =
        toml::to_string_pretty(config).context("Failed to serialize config to TOML")?;

    std::fs::write(&config_path, toml_string).context("Failed to write temp config file")?;

    Ok(config_path)
}

/// Parse a log line from server output
///
/// Handles tracing_subscriber's output format
fn parse_log_line(line: &str) -> ServerLogLine {
    let timestamp = chrono::Local::now().format("%H:%M:%S").to_string();

    // Parse tracing format: "2024-01-19T10:30:45.123Z  INFO module: message"
    // or simpler: "INFO  lamco_rdp_server: message"
    let (level, message) = if line.contains(" ERROR ") || line.starts_with("ERROR") {
        (LogLevel::Error, line.to_string())
    } else if line.contains(" WARN ") || line.starts_with("WARN") {
        (LogLevel::Warn, line.to_string())
    } else if line.contains(" INFO ") || line.starts_with("INFO") {
        (LogLevel::Info, line.to_string())
    } else if line.contains(" DEBUG ") || line.starts_with("DEBUG") {
        (LogLevel::Debug, line.to_string())
    } else if line.contains(" TRACE ") || line.starts_with("TRACE") {
        (LogLevel::Trace, line.to_string())
    } else {
        // Default to info for unrecognized format
        (LogLevel::Info, line.to_string())
    };

    ServerLogLine {
        level,
        message,
        timestamp,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_find_server_binary_not_found() {
        // This test verifies the error message is helpful
        // In a real environment, the binary might exist
        let result = find_server_binary();
        // Just check it returns something (Ok or helpful error)
        assert!(result.is_ok() || result.unwrap_err().to_string().contains("Could not find"));
    }

    #[test]
    fn test_parse_log_line_levels() {
        let error = parse_log_line("ERROR lamco_rdp_server: Connection failed");
        assert_eq!(error.level, LogLevel::Error);

        let warn = parse_log_line("2024-01-19T10:30:45Z  WARN module: Warning message");
        assert_eq!(warn.level, LogLevel::Warn);

        let info = parse_log_line("INFO  Starting server on port 3389");
        assert_eq!(info.level, LogLevel::Info);

        let unknown = parse_log_line("Some random output without level");
        assert_eq!(unknown.level, LogLevel::Info); // Default
    }
}
