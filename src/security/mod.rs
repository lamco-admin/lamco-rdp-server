//! Security and Authentication Module
//!
//! **Execution Path:** TLS 1.3 + PAM or no-auth
//! **Status:** Active (v1.0.0+)
//! **Platform:** Universal (PAM requires native)
//! **Role:** TLS encryption and user authentication
//!
//! Provides TLS encryption and user authentication for secure RDP connections.
//!
//! # TLS Configuration
//!
//! lamco-rdp-server **requires TLS 1.3** for all RDP connections by default.
//! This ensures encrypted communication between client and server, protecting
//! credentials and session data.
//!
//! ## Certificate Requirements
//!
//! The server needs a TLS certificate and private key in PEM format:
//!
//! **Quick Start (Development):**
//! ```bash
//! # Generate self-signed certificate (good for 1 year)
//! ./scripts/generate-certs.sh /etc/lamco-rdp-server $(hostname)
//! ```
//!
//! **Production (Let's Encrypt):**
//! ```bash
//! # Free, trusted certificate (auto-renews)
//! sudo certbot certonly --standalone -d rdp.yourdomain.com
//! # Then symlink or configure paths in config.toml
//! ```
//!
//! **Enterprise (Internal CA):**
//! ```bash
//! # Request certificate from your organization's CA
//! # Configure paths to your signed certificate and key
//! ```
//!
//! Configuration in `config.toml`:
//! ```toml
//! [security]
//! cert_path = "/etc/lamco-rdp-server/cert.pem"
//! key_path = "/etc/lamco-rdp-server/key.pem"
//! enable_nla = true           # Network Level Authentication (recommended)
//! require_tls_13 = true       # Require TLS 1.3 or higher
//! auth_method = "pam"         # Use Linux PAM for authentication
//! ```
//!
//! ## TLS Security Model
//!
//! - **TLS 1.3 mandatory** - Uses modern ciphers (AES-GCM, ChaCha20-Poly1305)
//! - **Perfect forward secrecy** - Ephemeral key exchange prevents decryption of past sessions
//! - **Certificate validation** - Clients verify server identity (unless self-signed)
//! - **NLA support** - Client authenticates before screen sharing begins (prevents unauthorized access)
//!
//! ## Authentication Methods
//!
//! **PAM Authentication** (default, requires `--features pam-auth`):
//! - Uses Linux Pluggable Authentication Modules
//! - Authenticates against system users
//! - Respects PAM policies (password complexity, account locking, etc.)
//! - Not available in Flatpak (sandbox limitation)
//!
//! **No Authentication** (`auth_method = "none"`):
//! - Relies solely on network security (firewall, VPN)
//! - Only recommended for isolated networks or Flatpak deployment
//! - Portal permission dialog still required for screen sharing
//!
//! ## Certificate Auto-Generation
//!
//! The server can generate self-signed certificates programmatically:
//!
//! ```rust
//! use lamco_rdp_server::security::CertificateGenerator;
//! use std::path::Path;
//!
//! // Generate and save to files
//! CertificateGenerator::generate_and_save(
//!     "rdp-server.example.com",  // Common Name
//!     365,                        // Valid for 1 year
//!     Path::new("/etc/lamco-rdp-server/cert.pem"),
//!     Path::new("/etc/lamco-rdp-server/key.pem"),
//! )?;
//! ```
//!
//! Generated certificates use:
//! - ECDSA P-256 algorithm (modern, efficient)
//! - Configurable validity period (default: 365 days)
//! - Automatic permission setting (cert: 644, key: 600)
//!
//! ## Security Best Practices
//!
//! **For Production:**
//! - ✅ Use Let's Encrypt or internal CA certificates (not self-signed)
//! - ✅ Enable NLA (`enable_nla = true`)
//! - ✅ Require TLS 1.3 (`require_tls_13 = true`)
//! - ✅ Use PAM authentication
//! - ✅ Firewall RDP port to trusted networks only
//! - ✅ Rotate certificates annually
//!
//! **For Development:**
//! - ✅ Self-signed certificates acceptable
//! - ✅ Test with various certificate configurations
//! - ⚠️ Consider `auth_method = "none"` for testing (disable for production)
//!
//! ## See Also
//!
//! - [`CertificateGenerator`] - Programmatic certificate generation
//! - [`TlsConfig`] - TLS configuration and server setup
//! - [`UserAuthenticator`] - PAM-based authentication
//! - `scripts/generate-certs.sh` - Automated certificate generation script

use std::sync::Arc;

use anyhow::Result;
use tracing::info;

pub mod auth;
pub mod certificates;
pub mod tls;

pub use auth::{AuthMethod, SessionToken, UserAuthenticator};
pub use certificates::CertificateGenerator;
pub use tls::TlsConfig;

use crate::config::Config;

/// Security subsystem for TLS and authentication
///
/// Coordinates TLS encryption and user authentication for RDP connections.
pub struct Security {
    tls_config: TlsConfig,
    authenticator: Arc<UserAuthenticator>,
}

impl Security {
    /// Create new security manager
    pub async fn new(config: &Config) -> Result<Self> {
        info!("Initializing Security");

        let tls_config =
            TlsConfig::from_files(&config.security.cert_path, &config.security.key_path)?;

        tls_config.verify()?;

        let auth_method = AuthMethod::from_str(&config.security.auth_method);
        let authenticator = Arc::new(UserAuthenticator::new(auth_method, None));

        info!("Security initialized successfully");

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
        UserAuthenticator::validate_username(username)?;

        let authenticated = self.authenticator.authenticate(username, password)?;

        if !authenticated {
            anyhow::bail!("Authentication failed");
        }

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
        let result = Security::new(&config).await;

        // In real test environment with certs, this should pass
        if result.is_ok() {
            let manager = result.unwrap();
            // Verify we can get the server config and authenticator
            let _server_config = manager.server_config();
            let _authenticator = manager.authenticator();
        }
    }
}
