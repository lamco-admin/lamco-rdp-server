//! Authentication module using PAM
//!
//! Provides user authentication against system accounts using PAM
//! (Pluggable Authentication Modules). Implements IronRDP's
//! `CredentialValidator` trait for inline validation during the
//! RDP handshake.
//!
//! Uses the `nonstick` crate for PAM bindings, which provides type-safe
//! wrappers around libpam without requiring build-time bindgen/libclang.

use std::{
    collections::HashMap,
    net::IpAddr,
    sync::Mutex,
    time::{Duration, Instant},
};

use anyhow::Result;
use ironrdp_server::{CredentialValidator, Credentials};
#[cfg(feature = "pam-auth")]
use nonstick::{AuthnFlags, ConversationAdapter, Transaction, TransactionBuilder};
use tracing::{info, warn};
use zeroize::Zeroize;

/// PAM conversation handler that provides pre-set credentials.
///
/// Returns the username for visible prompts and the password for masked
/// prompts. Created fresh for each authentication attempt and consumed
/// by the PAM transaction.
#[cfg(feature = "pam-auth")]
struct PasswordConvo {
    username: String,
    password: String,
}

#[cfg(feature = "pam-auth")]
impl ConversationAdapter for PasswordConvo {
    fn prompt(
        &self,
        _request: impl AsRef<std::ffi::OsStr>,
    ) -> nonstick::Result<std::ffi::OsString> {
        Ok(std::ffi::OsString::from(&self.username))
    }

    fn masked_prompt(
        &self,
        _request: impl AsRef<std::ffi::OsStr>,
    ) -> nonstick::Result<std::ffi::OsString> {
        Ok(std::ffi::OsString::from(&self.password))
    }

    fn error_msg(&self, message: impl AsRef<std::ffi::OsStr>) {
        warn!("PAM error: {:?}", message.as_ref());
    }

    fn info_msg(&self, message: impl AsRef<std::ffi::OsStr>) {
        info!("PAM info: {:?}", message.as_ref());
    }
}

/// Authentication method
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthMethod {
    /// PAM authentication
    Pam,
    /// No authentication (development only)
    None,
}

impl AuthMethod {
    /// Parse authentication method from string
    #[expect(
        clippy::should_implement_trait,
        reason = "infallible parse — FromStr requires Result"
    )]
    pub fn from_str(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "pam" => Self::Pam,
            "none" => Self::None,
            _ => {
                warn!("Unknown auth method '{}', defaulting to 'none'", s);
                Self::None
            }
        }
    }
}

/// Per-IP rate limit state
struct RateLimitEntry {
    failures: u32,
    last_attempt: Instant,
}

/// Rate limiter for authentication attempts.
///
/// Tracks failed attempts per IP and enforces exponential backoff:
/// - 1st-3rd failures: no delay
/// - 4th failure: 5s lockout
/// - 5th failure: 15s lockout
/// - 6th+ failures: 60s lockout
struct RateLimiter {
    entries: Mutex<HashMap<IpAddr, RateLimitEntry>>,
}

impl RateLimiter {
    fn new() -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
        }
    }

    /// Check if an IP is currently rate-limited. Returns the remaining
    /// lockout duration if blocked, or None if the attempt is allowed.
    fn check(&self, ip: IpAddr) -> Option<Duration> {
        let entries = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(entry) = entries.get(&ip) {
            let lockout = match entry.failures {
                0..=3 => return None,
                4 => Duration::from_secs(5),
                5 => Duration::from_secs(15),
                _ => Duration::from_secs(60),
            };
            lockout.checked_sub(entry.last_attempt.elapsed())
        } else {
            None
        }
    }

    /// Record a failed authentication attempt
    fn record_failure(&self, ip: IpAddr) {
        let mut entries = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let entry = entries.entry(ip).or_insert(RateLimitEntry {
            failures: 0,
            last_attempt: Instant::now(),
        });
        entry.failures += 1;
        entry.last_attempt = Instant::now();
    }

    /// Clear rate limit state for an IP after successful auth
    fn clear(&self, ip: IpAddr) {
        let mut entries = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        entries.remove(&ip);
    }

    /// Prune stale entries older than the maximum lockout window
    fn prune_stale(&self) {
        let mut entries = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        entries.retain(|_, entry| entry.last_attempt.elapsed() < Duration::from_secs(120));
    }
}

/// PAM credential validator implementing IronRDP's `CredentialValidator` trait.
///
/// Validates credentials received during the RDP TLS handshake against
/// system accounts via PAM. Includes per-IP rate limiting with exponential
/// backoff.
pub struct PamValidator {
    service_name: String,
    rate_limiter: RateLimiter,
    /// IP of the currently connecting client (set before each connection)
    peer_ip: Mutex<Option<IpAddr>>,
}

impl PamValidator {
    pub fn new(service_name: Option<String>) -> Self {
        let service_name = service_name.unwrap_or_else(|| "lamco-rdp-server".to_string());
        info!("PAM validator initialized (service: {})", service_name);

        Self {
            service_name,
            rate_limiter: RateLimiter::new(),
            peer_ip: Mutex::new(None),
        }
    }

    /// Set the peer IP for the current connection attempt.
    /// Call this before each `run_connection` so rate limiting tracks the right IP.
    pub fn set_peer_ip(&self, ip: IpAddr) {
        *self
            .peer_ip
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(ip);
    }

    /// Periodically prune stale rate limit entries (call from a timer or between connections)
    pub fn prune_stale_entries(&self) {
        self.rate_limiter.prune_stale();
    }

    #[cfg(feature = "pam-auth")]
    fn do_pam_auth(&self, username: &str, password: &str) -> Result<bool> {
        let convo = PasswordConvo {
            username: username.to_string(),
            password: password.to_string(),
        };

        let mut txn = TransactionBuilder::new_with_service(&self.service_name)
            .username(username)
            .build(convo.into_conversation())
            .map_err(|e| anyhow::anyhow!("Failed to start PAM transaction: {e}"))?;

        if let Err(e) = txn.authenticate(AuthnFlags::DISALLOW_NULL_AUTHTOK) {
            warn!("PAM: authentication failed for '{}': {}", username, e);
            return Ok(false);
        }

        info!("PAM: user '{}' authenticated successfully", username);

        // Verify the account is valid (not expired, not locked)
        if let Err(e) = txn.account_management(AuthnFlags::empty()) {
            warn!("PAM: account check failed for '{}': {}", username, e);
            return Ok(false);
        }

        // pam_end is called when `txn` drops (LibPamTransaction::drop)
        Ok(true)
    }

    #[cfg(not(feature = "pam-auth"))]
    fn do_pam_auth(&self, username: &str, _password: &str) -> Result<bool> {
        warn!(
            "PAM authentication requested but pam-auth feature not enabled (user: '{}')",
            username
        );
        Ok(false)
    }
}

impl CredentialValidator for PamValidator {
    fn validate(&self, credentials: &Credentials) -> Result<bool> {
        let username = &credentials.username;

        // Validate username format before touching PAM
        validate_username(username)?;

        // Check rate limiter
        let peer_ip = self
            .peer_ip
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .unwrap_or(IpAddr::from([0, 0, 0, 0]));

        if let Some(remaining) = self.rate_limiter.check(peer_ip) {
            warn!(
                "Rate limited: {} ({}s remaining) for user '{}'",
                peer_ip,
                remaining.as_secs(),
                username
            );
            return Ok(false);
        }

        // Extract password, do PAM auth, then zeroize the copy
        let mut password = credentials.password.clone();
        let result = self.do_pam_auth(username, &password);
        password.zeroize();

        match &result {
            Ok(true) => self.rate_limiter.clear(peer_ip),
            Ok(false) => self.rate_limiter.record_failure(peer_ip),
            Err(_) => self.rate_limiter.record_failure(peer_ip),
        }

        result
    }
}

/// User authenticator (legacy interface, kept for Security subsystem)
pub struct UserAuthenticator {
    method: AuthMethod,
    #[cfg_attr(
        not(feature = "pam-auth"),
        expect(dead_code, reason = "used only with pam-auth feature")
    )]
    service_name: String,
}

impl UserAuthenticator {
    /// Create new authenticator
    pub fn new(method: AuthMethod, service_name: Option<String>) -> Self {
        let service_name = service_name.unwrap_or_else(|| "lamco-rdp-server".to_string());

        info!(
            "Initializing authenticator: {:?}, service: {}",
            method, service_name
        );

        Self {
            method,
            service_name,
        }
    }

    /// Authenticate user with password
    pub fn authenticate(&self, username: &str, password: &str) -> Result<bool> {
        match self.method {
            AuthMethod::Pam => self.authenticate_pam(username, password),
            AuthMethod::None => {
                warn!("Authentication disabled (development mode)");
                Ok(true)
            }
        }
    }

    /// Authenticate using PAM
    #[cfg(feature = "pam-auth")]
    fn authenticate_pam(&self, username: &str, password: &str) -> Result<bool> {
        info!("Authenticating user '{}' via PAM", username);

        let convo = PasswordConvo {
            username: username.to_string(),
            password: password.to_string(),
        };

        let mut txn = TransactionBuilder::new_with_service(&self.service_name)
            .username(username)
            .build(convo.into_conversation())
            .map_err(|e| anyhow::anyhow!("Failed to start PAM transaction: {e}"))?;

        if let Err(e) = txn.authenticate(AuthnFlags::DISALLOW_NULL_AUTHTOK) {
            warn!("Authentication failed for user '{}': {}", username, e);
            return Ok(false);
        }

        info!("User '{}' authenticated successfully", username);

        if let Err(e) = txn.account_management(AuthnFlags::empty()) {
            warn!("Account check failed for user '{}': {}", username, e);
            return Ok(false);
        }

        Ok(true)
    }

    /// Authenticate using PAM (stub when PAM is not enabled)
    #[cfg(not(feature = "pam-auth"))]
    fn authenticate_pam(&self, username: &str, _password: &str) -> Result<bool> {
        warn!(
            "PAM authentication requested but feature not enabled for user '{}'",
            username
        );
        Ok(false)
    }
}

/// Validate username format (prevent injection attacks)
pub fn validate_username(username: &str) -> Result<()> {
    if username.is_empty() {
        anyhow::bail!("Username cannot be empty");
    }

    if username.len() > 32 {
        anyhow::bail!("Username too long (max 32 characters)");
    }

    // Allow dots for domain\user format (RDP clients send "domain\user" or "user@domain")
    if !username
        .chars()
        .all(|c| c.is_alphanumeric() || c == '_' || c == '-' || c == '.' || c == '\\' || c == '@')
    {
        anyhow::bail!("Username contains invalid characters");
    }

    Ok(())
}

/// Session token for authenticated sessions
#[derive(Debug, Clone)]
pub struct SessionToken {
    token: String,
    username: String,
    created_at: std::time::SystemTime,
}

impl SessionToken {
    /// Create new session token
    pub fn new(username: String) -> Self {
        use uuid::Uuid;

        let token = Uuid::new_v4().to_string();
        let created_at = std::time::SystemTime::now();

        info!("Created session token for user '{}'", username);

        Self {
            token,
            username,
            created_at,
        }
    }

    /// Get token string
    pub fn token(&self) -> &str {
        &self.token
    }

    /// Get username
    pub fn username(&self) -> &str {
        &self.username
    }

    /// Check if token is expired
    pub fn is_expired(&self, max_age: std::time::Duration) -> bool {
        match self.created_at.elapsed() {
            Ok(elapsed) => elapsed > max_age,
            Err(_) => true, // Clock went backwards, consider expired
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_auth_method_from_str() {
        assert_eq!(AuthMethod::from_str("pam"), AuthMethod::Pam);
        assert_eq!(AuthMethod::from_str("none"), AuthMethod::None);
        assert_eq!(AuthMethod::from_str("invalid"), AuthMethod::None);
    }

    #[test]
    fn test_validate_username() {
        assert!(validate_username("validuser").is_ok());
        assert!(validate_username("user_123").is_ok());
        assert!(validate_username("user-name").is_ok());
        assert!(validate_username("user.name").is_ok());
        assert!(validate_username("DOMAIN\\user").is_ok());
        assert!(validate_username("user@domain.com").is_ok());

        assert!(validate_username("").is_err());
        assert!(validate_username(&"a".repeat(33)).is_err());
        assert!(validate_username("invalid;user").is_err());
        assert!(validate_username("user$(cmd)").is_err());
    }

    #[test]
    fn test_rate_limiter_allows_initial() {
        let limiter = RateLimiter::new();
        let ip: IpAddr = "10.0.0.1".parse().unwrap();
        assert!(limiter.check(ip).is_none());
    }

    #[test]
    fn test_rate_limiter_allows_after_few_failures() {
        let limiter = RateLimiter::new();
        let ip: IpAddr = "10.0.0.1".parse().unwrap();

        for _ in 0..3 {
            limiter.record_failure(ip);
        }
        // 3 failures: still allowed
        assert!(limiter.check(ip).is_none());
    }

    #[test]
    fn test_rate_limiter_blocks_after_threshold() {
        let limiter = RateLimiter::new();
        let ip: IpAddr = "10.0.0.1".parse().unwrap();

        for _ in 0..4 {
            limiter.record_failure(ip);
        }
        // 4th failure: 5s lockout
        assert!(limiter.check(ip).is_some());
    }

    #[test]
    fn test_rate_limiter_clears_on_success() {
        let limiter = RateLimiter::new();
        let ip: IpAddr = "10.0.0.1".parse().unwrap();

        for _ in 0..5 {
            limiter.record_failure(ip);
        }
        assert!(limiter.check(ip).is_some());

        limiter.clear(ip);
        assert!(limiter.check(ip).is_none());
    }

    #[test]
    fn test_rate_limiter_independent_ips() {
        let limiter = RateLimiter::new();
        let ip1: IpAddr = "10.0.0.1".parse().unwrap();
        let ip2: IpAddr = "10.0.0.2".parse().unwrap();

        for _ in 0..5 {
            limiter.record_failure(ip1);
        }
        assert!(limiter.check(ip1).is_some());
        assert!(limiter.check(ip2).is_none());
    }

    #[test]
    fn test_session_token_creation() {
        let token = SessionToken::new("testuser".to_string());
        assert_eq!(token.username(), "testuser");
        assert!(!token.token().is_empty());
        assert!(!token.is_expired(std::time::Duration::from_secs(3600)));
    }

    #[test]
    fn test_none_auth() {
        let auth = UserAuthenticator::new(AuthMethod::None, None);
        assert!(auth.authenticate("anyuser", "anypass").unwrap());
    }

    #[test]
    fn test_pam_validator_creation() {
        let validator = PamValidator::new(None);
        validator.set_peer_ip("127.0.0.1".parse().unwrap());
        // Can't test actual PAM validation without system accounts,
        // but construction and rate limiter should work
        validator.prune_stale_entries();
    }
}
