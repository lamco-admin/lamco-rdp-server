//! Generic fallback chain framework
//!
//! This module provides a generic framework for implementing fallback chains
//! where multiple strategies can be tried in order until one succeeds.

use async_trait::async_trait;
use std::time::{Duration, Instant};
use thiserror::Error;

use crate::capabilities::ServiceLevel;

/// Result of probing a strategy
///
/// Contains information about whether a strategy is viable and why.
#[derive(Debug, Clone)]
pub struct StrategyProbe {
    /// Is this strategy viable?
    pub is_viable: bool,
    /// Why or why not
    pub reason: String,
    /// Additional details for diagnostics
    pub details: Option<String>,
}

impl StrategyProbe {
    /// Create a viable probe result
    pub fn viable(reason: impl Into<String>) -> Self {
        Self {
            is_viable: true,
            reason: reason.into(),
            details: None,
        }
    }

    /// Create a non-viable probe result
    pub fn not_viable(reason: impl Into<String>) -> Self {
        Self {
            is_viable: false,
            reason: reason.into(),
            details: None,
        }
    }

    /// Add details to the probe result
    pub fn with_details(mut self, details: impl Into<String>) -> Self {
        self.details = Some(details.into());
        self
    }
}

/// Error during probing
#[derive(Debug, Error)]
pub enum ProbeError {
    /// Resource not found
    #[error("Not found: {0}")]
    NotFound(String),

    /// Feature not compiled in
    #[error("Not compiled: {0}")]
    NotCompiled(String),

    /// External command failed
    #[error("Command failed: {0}")]
    CommandFailed(String),

    /// D-Bus communication error
    #[error("D-Bus error: {0}")]
    DBus(String),

    /// IO error
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    /// Timeout during probe
    #[error("Timeout: {0}")]
    Timeout(String),

    /// Other error
    #[error("Other: {0}")]
    Other(String),
}

/// Error during instantiation
#[derive(Debug, Error)]
pub enum InstantiationError {
    /// Configuration error
    #[error("Configuration error: {0}")]
    Config(String),

    /// Resource unavailable
    #[error("Resource unavailable: {0}")]
    ResourceUnavailable(String),

    /// Permission denied
    #[error("Permission denied: {0}")]
    PermissionDenied(String),

    /// Other error
    #[error("Other: {0}")]
    Other(String),
}

/// Trait for fallback strategies
///
/// Implement this trait to define a strategy that can be tried as part of
/// a fallback chain.
#[async_trait]
pub trait FallbackStrategy<T>: Send + Sync {
    /// Human-readable name for this strategy
    fn name(&self) -> &str;

    /// Quick check if this strategy might work (fast, no I/O)
    ///
    /// This is called before probe() to quickly filter out strategies
    /// that definitely won't work.
    fn is_candidate(&self) -> bool {
        true
    }

    /// Actually probe/test this strategy (may be slow, does I/O)
    ///
    /// This performs a more thorough check to determine if the strategy
    /// is viable.
    async fn probe(&self) -> Result<StrategyProbe, ProbeError>;

    /// Instantiate the strategy for use
    ///
    /// This creates the actual instance after probing succeeds.
    async fn instantiate(&self) -> Result<T, InstantiationError>;

    /// Service level this strategy provides
    fn service_level(&self) -> ServiceLevel;
}

/// Record of an attempt to use a strategy
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AttemptResult {
    /// Strategy name
    pub strategy_name: String,
    /// Did it succeed?
    pub success: bool,
    /// Error message if failed
    pub error: Option<String>,
    /// How long the attempt took (in milliseconds)
    pub duration_ms: u64,
}

impl AttemptResult {
    /// Get duration as Duration type
    pub fn duration(&self) -> Duration {
        Duration::from_millis(self.duration_ms)
    }
}

impl AttemptResult {
    /// Create a successful attempt result
    pub fn success(name: impl Into<String>, duration: Duration) -> Self {
        Self {
            strategy_name: name.into(),
            success: true,
            error: None,
            duration_ms: duration.as_millis() as u64,
        }
    }

    /// Create a failed attempt result
    pub fn failure(name: impl Into<String>, error: impl Into<String>, duration: Duration) -> Self {
        Self {
            strategy_name: name.into(),
            success: false,
            error: Some(error.into()),
            duration_ms: duration.as_millis() as u64,
        }
    }
}

/// Error when all strategies fail
#[derive(Debug, Error)]
#[error("All {count} strategies failed")]
pub struct AllStrategiesFailed {
    /// Number of strategies tried
    pub count: usize,
    /// Results from each attempt
    pub attempts: Vec<AttemptResult>,
}

impl AllStrategiesFailed {
    /// Format a detailed report of all failures
    pub fn detailed_report(&self) -> String {
        let mut report = String::new();
        report.push_str(&format!("All {} strategies failed:\n", self.count));
        for (i, attempt) in self.attempts.iter().enumerate() {
            let status = if attempt.success { "✅" } else { "❌" };
            report.push_str(&format!(
                "  {}. {} {} ({}ms)\n",
                i + 1,
                status,
                attempt.strategy_name,
                attempt.duration_ms
            ));
            if let Some(err) = &attempt.error {
                report.push_str(&format!("      Error: {}\n", err));
            }
        }
        report
    }
}

/// Generic fallback chain executor
///
/// Executes a chain of strategies in order, returning the first successful
/// result or an error if all fail.
pub struct FallbackChain<T> {
    strategies: Vec<Box<dyn FallbackStrategy<T>>>,
    selected_index: Option<usize>,
    attempts: Vec<AttemptResult>,
}

impl<T> FallbackChain<T> {
    /// Create a new fallback chain
    pub fn new() -> Self {
        Self {
            strategies: Vec::new(),
            selected_index: None,
            attempts: Vec::new(),
        }
    }

    /// Add a strategy to the chain
    pub fn add<S: FallbackStrategy<T> + 'static>(mut self, strategy: S) -> Self {
        self.strategies.push(Box::new(strategy));
        self
    }

    /// Execute the fallback chain, trying each strategy in order
    ///
    /// Returns the successful result and its service level, or an error
    /// if all strategies fail.
    pub async fn execute(&mut self) -> Result<(T, ServiceLevel), AllStrategiesFailed> {
        self.attempts.clear();
        self.selected_index = None;

        for (i, strategy) in self.strategies.iter().enumerate() {
            // Quick candidate check
            if !strategy.is_candidate() {
                self.attempts.push(AttemptResult::failure(
                    strategy.name(),
                    "Not a candidate for this environment",
                    Duration::ZERO,
                ));
                continue;
            }

            let start = Instant::now();

            // Probe the strategy
            match strategy.probe().await {
                Ok(probe) if probe.is_viable => {
                    // Try to instantiate
                    match strategy.instantiate().await {
                        Ok(instance) => {
                            self.selected_index = Some(i);
                            self.attempts
                                .push(AttemptResult::success(strategy.name(), start.elapsed()));
                            return Ok((instance, strategy.service_level()));
                        }
                        Err(e) => {
                            self.attempts.push(AttemptResult::failure(
                                strategy.name(),
                                format!("Instantiation failed: {}", e),
                                start.elapsed(),
                            ));
                        }
                    }
                }
                Ok(probe) => {
                    self.attempts.push(AttemptResult::failure(
                        strategy.name(),
                        format!("Not viable: {}", probe.reason),
                        start.elapsed(),
                    ));
                }
                Err(e) => {
                    self.attempts.push(AttemptResult::failure(
                        strategy.name(),
                        format!("Probe error: {}", e),
                        start.elapsed(),
                    ));
                }
            }
        }

        Err(AllStrategiesFailed {
            count: self.strategies.len(),
            attempts: self.attempts.clone(),
        })
    }

    /// Get all attempts (for diagnostics)
    pub fn attempts(&self) -> &[AttemptResult] {
        &self.attempts
    }

    /// Get selected strategy index
    pub fn selected_index(&self) -> Option<usize> {
        self.selected_index
    }

    /// Get selected strategy name
    pub fn selected_name(&self) -> Option<&str> {
        self.selected_index
            .and_then(|i| self.strategies.get(i))
            .map(|s| s.name())
    }
}

impl<T> Default for FallbackChain<T> {
    fn default() -> Self {
        Self::new()
    }
}
