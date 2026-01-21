//! Certificate generation and management
//!
//! Provides utilities for generating self-signed certificates
//! and managing certificate lifecycle.

use anyhow::{Context, Result};
use rcgen::{Certificate, CertificateParams, DistinguishedName, DnType, KeyPair};
use std::fs;
use std::path::Path;
use tracing::{info, warn};

/// Certificate generator
pub struct CertificateGenerator;

impl CertificateGenerator {
    /// Generate self-signed certificate
    pub fn generate_self_signed(common_name: &str, validity_days: u32) -> Result<(String, String)> {
        info!("Generating self-signed certificate for '{}'", common_name);

        let mut params = CertificateParams::default();

        let mut distinguished_name = DistinguishedName::new();
        distinguished_name.push(DnType::CommonName, common_name);
        params.distinguished_name = distinguished_name;

        params.not_before = time::OffsetDateTime::now_utc();
        params.not_after =
            time::OffsetDateTime::now_utc() + time::Duration::days(validity_days as i64);

        // Generate key pair (use ECDSA P-256 algorithm)
        let key_pair = KeyPair::generate(&rcgen::PKCS_ECDSA_P256_SHA256)
            .context("Failed to generate key pair")?;
        params.key_pair = Some(key_pair);

        // Generate certificate
        let cert = Certificate::from_params(params).context("Failed to generate certificate")?;

        // Serialize to PEM
        let cert_pem = cert
            .serialize_pem()
            .context("Failed to serialize certificate")?;
        let key_pem = cert.serialize_private_key_pem();

        info!("Self-signed certificate generated successfully");

        Ok((cert_pem, key_pem))
    }

    /// Generate and save self-signed certificate to files
    pub fn generate_and_save(
        common_name: &str,
        validity_days: u32,
        cert_path: &Path,
        key_path: &Path,
    ) -> Result<()> {
        info!("Generating and saving certificate to {:?}", cert_path);

        if cert_path.exists() || key_path.exists() {
            warn!("Certificate or key file already exists, will overwrite");
        }

        // Generate certificate
        let (cert_pem, key_pem) = Self::generate_self_signed(common_name, validity_days)?;

        if let Some(parent) = cert_path.parent() {
            fs::create_dir_all(parent).context("Failed to create certificate directory")?;
        }
        if let Some(parent) = key_path.parent() {
            fs::create_dir_all(parent).context("Failed to create key directory")?;
        }

        // Write certificate
        fs::write(cert_path, cert_pem.as_bytes()).context("Failed to write certificate")?;

        // Write private key (with restricted permissions)
        fs::write(key_path, key_pem.as_bytes()).context("Failed to write private key")?;

        // Unix: Restrict key to owner-only (mode 600)
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(key_path)?.permissions();
            perms.set_mode(0o600); // rw-------
            fs::set_permissions(key_path, perms)?;
        }

        info!("Certificate and key saved successfully");

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_generate_self_signed() {
        let (cert_pem, key_pem) =
            CertificateGenerator::generate_self_signed("test-server", 365).unwrap();

        assert!(cert_pem.contains("BEGIN CERTIFICATE"));
        assert!(key_pem.contains("BEGIN PRIVATE KEY"));
    }

    #[test]
    fn test_generate_and_save() {
        let temp_dir = TempDir::new().unwrap();
        let cert_path = temp_dir.path().join("cert.pem");
        let key_path = temp_dir.path().join("key.pem");

        CertificateGenerator::generate_and_save("test-server", 365, &cert_path, &key_path).unwrap();

        assert!(cert_path.exists());
        assert!(key_path.exists());

        // Verify contents
        let cert_content = fs::read_to_string(cert_path).unwrap();
        assert!(cert_content.contains("BEGIN CERTIFICATE"));

        let key_content = fs::read_to_string(key_path).unwrap();
        assert!(key_content.contains("BEGIN PRIVATE KEY"));
    }
}
