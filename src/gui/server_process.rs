//! Server Process Management
//!
//! Handles spawning, monitoring, and controlling the lamco-rdp-server process
//! from the GUI. Provides real-time log capture and status updates.

use anyhow::{anyhow, Context, Result};
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
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

        // Find the server binary
        let server_binary = find_server_binary()?;
        info!("Using server binary: {:?}", server_binary);

        // Write config to a temporary file
        let config_path = write_temp_config(config)?;
        info!("Config written to: {:?}", config_path);

        // Build the address string for display
        let address = config.server.listen_addr.clone();

        // Spawn the server process
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

        // Capture stdout/stderr in background threads
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
        })
    }

    /// Get the server's PID
    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// Get server uptime
    pub fn uptime(&self) -> Duration {
        self.start_time.elapsed()
    }

    /// Get the listen address
    pub fn address(&self) -> &str {
        &self.address
    }

    /// Check if the server process is still running
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
    /// Sends SIGTERM first, then SIGKILL if needed
    pub fn stop(&mut self) -> Result<()> {
        info!("Stopping server process (PID: {})", self.pid);

        // Signal shutdown to log capture threads
        self.shutdown_flag.store(true, Ordering::Relaxed);

        // Send SIGTERM for graceful shutdown
        #[cfg(unix)]
        {
            use nix::sys::signal::{kill, Signal};
            use nix::unistd::Pid;

            let pid = Pid::from_raw(self.pid as i32);
            if let Err(e) = kill(pid, Signal::SIGTERM) {
                warn!("Failed to send SIGTERM: {}", e);
            }
        }

        // Wait up to 5 seconds for graceful shutdown
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if !self.is_running() {
                info!("Server stopped gracefully");
                self.cleanup();
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(100));
        }

        // Force kill if still running
        warn!("Server did not stop gracefully, sending SIGKILL");
        if let Err(e) = self.child.kill() {
            error!("Failed to kill server process: {}", e);
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
        // Attempt graceful shutdown on drop
        self.shutdown_flag.store(true, Ordering::Relaxed);
        let _ = self.stop();
    }
}

/// Find the server binary in standard locations
fn find_server_binary() -> Result<PathBuf> {
    // Check common locations in order of preference

    // 1. Same directory as GUI binary
    if let Ok(current_exe) = std::env::current_exe() {
        if let Some(dir) = current_exe.parent() {
            let server_path = dir.join("lamco-rdp-server");
            if server_path.exists() {
                return Ok(server_path);
            }
        }
    }

    // 2. Development target directory
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

    // 3. System PATH
    if let Ok(output) = Command::new("which").arg("lamco-rdp-server").output() {
        if output.status.success() {
            let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !path.is_empty() {
                return Ok(PathBuf::from(path));
            }
        }
    }

    // 4. Standard system locations
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

    Err(anyhow!(
        "Could not find lamco-rdp-server binary. Searched: same directory, target/, PATH, /usr/bin, /usr/local/bin"
    ))
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
