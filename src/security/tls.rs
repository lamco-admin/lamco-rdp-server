//! TLS 1.3 configuration and management
//!
//! Provides secure TLS termination for RDP connections using rustls.
//!
//! Uses IronRDP's re-exported rustls (v0.23) for version compatibility.

use anyhow::{Context, Result};
use ironrdp_server::tokio_rustls::rustls;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::ServerConfig;
use std::fs::File;
use std::io::BufReader;
use std::path::Path;
use std::sync::Arc;
use tracing::{debug, info};

/// TLS configuration wrapper
pub struct TlsConfig {
    /// Certificate chain (owned for lifetime management)
    #[allow(dead_code)]
    cert_chain: Vec<CertificateDer<'static>>,

    /// Private key (owned for lifetime management)
    #[allow(dead_code)]
    private_key: PrivateKeyDer<'static>,

    /// rustls ServerConfig
    server_config: Arc<ServerConfig>,
}

impl Clone for TlsConfig {
    fn clone(&self) -> Self {
        Self {
            // Clone certificates (CertificateDer is Clone)
            cert_chain: self.cert_chain.clone(),
            // Clone key using clone_key() method
            private_key: self.private_key.clone_key(),
            // Clone Arc
            server_config: Arc::clone(&self.server_config),
        }
    }
}

impl TlsConfig {
    /// Create TLS config from PEM files
    ///
    /// # Arguments
    ///
    /// * `cert_path` - Path to PEM certificate file
    /// * `key_path` - Path to PEM private key file
    ///
    /// # Returns
    ///
    /// A configured `TlsConfig` with TLS 1.3 enabled
    ///
    /// # Errors
    ///
    /// Returns error if:
    /// - Files cannot be opened
    /// - PEM parsing fails
    /// - No certificates or keys found
    /// - ServerConfig creation fails
    pub fn from_files(cert_path: &Path, key_path: &Path) -> Result<Self> {
        info!("Loading TLS configuration from files");
        debug!("Certificate: {:?}", cert_path);
        debug!("Private key: {:?}", key_path);

        // Load certificate chain
        let cert_file = File::open(cert_path).context("Failed to open certificate file")?;
        let mut cert_reader = BufReader::new(cert_file);

        let certs: Vec<CertificateDer<'static>> = rustls_pemfile::certs(&mut cert_reader)
            .collect::<Result<Vec<_>, _>>()
            .context("Failed to parse certificates")?;

        if certs.is_empty() {
            anyhow::bail!("No certificates found in file");
        }

        debug!("Loaded {} certificate(s)", certs.len());

        // Load private key
        let key_file = File::open(key_path).context("Failed to open private key file")?;
        let mut key_reader = BufReader::new(key_file);

        // rustls 0.23 uses rustls_pemfile::private_key() which auto-detects format
        let private_key = rustls_pemfile::private_key(&mut key_reader)
            .context("Failed to parse private key")?
            .ok_or_else(|| anyhow::anyhow!("No private key found in file"))?;

        debug!("Private key loaded successfully");

        // Create ServerConfig with modern rustls 0.23 API
        // Use builder pattern with defaults
        let server_config = ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(certs.clone(), private_key.clone_key())
            .context("Failed to configure certificate")?;

        info!("TLS 1.3 configuration created successfully");

        Ok(Self {
            cert_chain: certs,
            private_key,
            server_config: Arc::new(server_config),
        })
    }

    /// Get rustls ServerConfig
    ///
    /// Returns an Arc to the ServerConfig for use with tokio_rustls::TlsAcceptor
    pub fn server_config(&self) -> Arc<ServerConfig> {
        Arc::clone(&self.server_config)
    }

    /// Verify TLS configuration is valid
    ///
    /// Performs basic validation checks on the configuration.
    pub fn verify(&self) -> Result<()> {
        // Verify we have at least one certificate
        if self.cert_chain.is_empty() {
            anyhow::bail!("No certificates in chain");
        }

        info!("TLS configuration verified");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn get_test_cert_paths() -> (PathBuf, PathBuf) {
        (
            PathBuf::from("certs/test-cert.pem"),
            PathBuf::from("certs/test-key.pem"),
        )
    }

    #[test]
    fn test_tls_config_from_files() {
        let (cert_path, key_path) = get_test_cert_paths();

        // Skip if test certs don't exist
        if !cert_path.exists() || !key_path.exists() {
            eprintln!("Skipping test: test certificates not found");
            return;
        }

        let config = TlsConfig::from_files(&cert_path, &key_path).unwrap();
        assert!(!config.cert_chain.is_empty());
    }

    #[test]
    fn test_tls_config_verify() {
        let (cert_path, key_path) = get_test_cert_paths();

        if !cert_path.exists() || !key_path.exists() {
            return;
        }

        let config = TlsConfig::from_files(&cert_path, &key_path).unwrap();
        assert!(config.verify().is_ok());
    }
}
