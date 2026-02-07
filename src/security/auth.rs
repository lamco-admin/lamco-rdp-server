//! Authentication module using PAM
//!
//! Provides user authentication against system accounts using PAM
//! (Pluggable Authentication Modules).

#[cfg(feature = "pam-auth")]
use anyhow::Context;
use anyhow::Result;
#[cfg(feature = "pam-auth")]
use pam::Authenticator;
use tracing::{info, warn};

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
    #[allow(clippy::should_implement_trait)] // We don't want FromStr trait as this never fails
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

/// User authenticator
pub struct UserAuthenticator {
    method: AuthMethod,
    #[allow(dead_code)] // Used when pam-auth feature is enabled
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

        let mut auth = Authenticator::with_password(&self.service_name)
            .context("Failed to create PAM authenticator")?;

        auth.get_handler().set_credentials(username, password);

        match auth.authenticate() {
            Ok(_) => {
                info!("User '{}' authenticated successfully", username);

                // Open session (required for complete authentication)
                if let Err(e) = auth.open_session() {
                    warn!("Failed to open PAM session: {}", e);
                    // Continue anyway - authentication succeeded
                }

                Ok(true)
            }
            Err(e) => {
                warn!("Authentication failed for user '{}': {}", username, e);
                Ok(false)
            }
        }
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

    /// Validate username format
    pub fn validate_username(username: &str) -> Result<()> {
        if username.is_empty() {
            anyhow::bail!("Username cannot be empty");
        }

        if username.len() > 32 {
            anyhow::bail!("Username too long (max 32 characters)");
        }

        if !username
            .chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '-')
        {
            anyhow::bail!("Username contains invalid characters");
        }

        Ok(())
    }
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
        // Unknown methods default to None (safe default)
        assert_eq!(AuthMethod::from_str("invalid"), AuthMethod::None);
    }

    #[test]
    fn test_validate_username() {
        assert!(UserAuthenticator::validate_username("validuser").is_ok());
        assert!(UserAuthenticator::validate_username("user_123").is_ok());
        assert!(UserAuthenticator::validate_username("user-name").is_ok());

        assert!(UserAuthenticator::validate_username("").is_err());
        assert!(UserAuthenticator::validate_username("a".repeat(33).as_str()).is_err());
        assert!(UserAuthenticator::validate_username("invalid@user").is_err());
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
}
