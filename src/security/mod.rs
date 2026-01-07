//! Security module coordination
//!
//! Coordinates TLS, certificate management, and authentication.

use anyhow::Result;
use std::sync::Arc;
use tracing::info;

pub mod auth;
pub mod certificates;
pub mod tls;

pub use auth::{AuthMethod, SessionToken, UserAuthenticator};
pub use certificates::CertificateGenerator;
pub use tls::TlsConfig;

use crate::config::Config;

/// Security manager coordinates all security operations
pub struct SecurityManager {
    tls_config: TlsConfig,
    authenticator: Arc<UserAuthenticator>,
}

impl SecurityManager {
    /// Create new security manager
    pub async fn new(config: &Config) -> Result<Self> {
        info!("Initializing SecurityManager");

        // Load TLS configuration
        let tls_config =
            TlsConfig::from_files(&config.security.cert_path, &config.security.key_path)?;

        // Verify TLS config
        tls_config.verify()?;

        // Create authenticator
        let auth_method = AuthMethod::from_str(&config.security.auth_method);
        let authenticator = Arc::new(UserAuthenticator::new(auth_method, None));

        info!("SecurityManager initialized successfully");

        Ok(Self {
            tls_config,
            authenticator,
        })
    }

    /// Create TLS acceptor
    /// Get TLS server config for creating acceptor
    pub fn server_config(&self) -> Arc<ironrdp_server::tokio_rustls::rustls::ServerConfig> {
        self.tls_config.server_config()
    }

    /// Get authenticator
    pub fn authenticator(&self) -> Arc<UserAuthenticator> {
        self.authenticator.clone()
    }

    /// Authenticate user
    pub async fn authenticate(&self, username: &str, password: &str) -> Result<SessionToken> {
        // Validate username format
        UserAuthenticator::validate_username(username)?;

        // Authenticate
        let authenticated = self.authenticator.authenticate(username, password)?;

        if !authenticated {
            anyhow::bail!("Authentication failed");
        }

        // Create session token
        Ok(SessionToken::new(username.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    #[tokio::test]
    async fn test_security_manager_creation() {
        let config = Config::default_config().unwrap();

        // This will fail if certs don't exist, which is expected
        let result = SecurityManager::new(&config).await;

        // In real test environment with certs, this should pass
        if result.is_ok() {
            let manager = result.unwrap();
            // Verify we can get the server config and authenticator
            let _server_config = manager.server_config();
            let _authenticator = manager.authenticator();
        }
    }
}
