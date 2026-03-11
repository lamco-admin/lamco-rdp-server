//! Certificate Generation and Validation
//!
//! Generates self-signed TLS certificates for RDP server authentication
//! and provides X.509 certificate inspection via x509-parser.

use std::{
    fs,
    path::{Path, PathBuf},
};

use rcgen::{CertifiedKey, generate_simple_self_signed};
use serde::Serialize;
use time::{Duration, OffsetDateTime};
use x509_parser::pem::parse_x509_pem;

/// Certificate generation parameters
#[derive(Debug, Clone)]
pub struct CertGenParams {
    /// Common name (CN) for the certificate
    pub common_name: String,
    /// Organization name (O)
    pub organization: Option<String>,
    /// Organizational unit (OU)
    pub organizational_unit: Option<String>,
    /// Subject alternative names (hostnames/IPs)
    pub san_names: Vec<String>,
    /// Certificate validity in days
    pub validity_days: u32,
    /// Key size in bits (2048 or 4096)
    pub key_size: KeySize,
}

/// Supported key sizes
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeySize {
    Bits2048,
    Bits4096,
}

impl Default for CertGenParams {
    fn default() -> Self {
        let hostname = hostname::get()
            .map(|h| h.to_string_lossy().to_string())
            .unwrap_or_else(|_| "localhost".to_string());

        Self {
            common_name: hostname.clone(),
            organization: Some("Lamco RDP Server".to_string()),
            organizational_unit: Some("Self-Signed Certificate".to_string()),
            san_names: vec![hostname, "localhost".to_string(), "127.0.0.1".to_string()],
            validity_days: 365,
            key_size: KeySize::Bits2048,
        }
    }
}

/// Result of certificate generation
#[derive(Debug)]
pub struct GeneratedCertificate {
    /// PEM-encoded certificate
    pub certificate_pem: String,
    /// PEM-encoded private key
    pub private_key_pem: String,
    /// Certificate fingerprint (SHA-256)
    pub fingerprint: String,
    /// Expiration date
    pub expires: String,
}

/// Structured certificate details parsed from X.509.
///
/// Serializable for D-Bus, health endpoints, or management APIs.
#[derive(Debug, Clone, Serialize)]
pub struct CertificateDetails {
    /// Subject common name (CN)
    pub subject_cn: Option<String>,
    /// Full subject distinguished name (RFC 4514)
    pub subject: String,
    /// Issuer distinguished name
    pub issuer: String,
    /// Serial number (hex)
    pub serial: String,
    /// Not valid before (UTC)
    pub not_before: String,
    /// Not valid after (UTC)
    pub not_after: String,
    /// Whether the certificate is currently within its validity window
    pub is_currently_valid: bool,
    /// Days until expiration (negative if already expired)
    pub days_until_expiry: i64,
    /// SHA-256 fingerprint (colon-separated hex)
    pub fingerprint: String,
    /// Signature algorithm (e.g. "ecdsa-with-SHA256")
    pub signature_algorithm: String,
    /// Public key algorithm (e.g. "id-ecPublicKey")
    pub public_key_algorithm: String,
    /// Subject alternative names
    pub san: Vec<String>,
    /// Whether this is a self-signed certificate (subject == issuer)
    pub is_self_signed: bool,
    /// Path to the certificate file
    pub path: PathBuf,
}

/// Generate a self-signed certificate with the given parameters
pub fn generate_self_signed_certificate(
    cert_path: PathBuf,
    key_path: PathBuf,
    common_name: String,
    organization: Option<String>,
    valid_days: u32,
) -> Result<(), String> {
    let mut san_names = vec![common_name.clone()];
    if common_name != "localhost" {
        san_names.push("localhost".to_string());
    }
    san_names.push("127.0.0.1".to_string());

    let params = CertGenParams {
        common_name,
        organization,
        organizational_unit: Some("Self-Signed Certificate".to_string()),
        san_names,
        validity_days: valid_days,
        key_size: KeySize::Bits2048,
    };

    let cert = generate_certificate_internal(&params)?;
    save_certificate_files(&cert, &cert_path, &key_path)
}

/// Internal certificate generation from params
fn generate_certificate_internal(params: &CertGenParams) -> Result<GeneratedCertificate, String> {
    let CertifiedKey { cert, signing_key } = generate_simple_self_signed(params.san_names.clone())
        .map_err(|e| format!("Failed to generate certificate: {e}"))?;

    let certificate_pem = cert.pem();
    let private_key_pem = signing_key.serialize_pem();
    let fingerprint = sha256_fingerprint(cert.der());

    let now = OffsetDateTime::now_utc();
    let not_after = now + Duration::days(params.validity_days as i64);
    let expires = format_date(not_after);

    Ok(GeneratedCertificate {
        certificate_pem,
        private_key_pem,
        fingerprint,
        expires,
    })
}

/// SHA-256 fingerprint of DER-encoded certificate bytes
fn sha256_fingerprint(der: &[u8]) -> String {
    use sha2::{Digest, Sha256};

    let hash = Sha256::digest(der);
    hash.iter()
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join(":")
}

/// Format date for display
fn format_date(dt: OffsetDateTime) -> String {
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02} UTC",
        dt.year(),
        dt.month() as u8,
        dt.day(),
        dt.hour(),
        dt.minute(),
        dt.second()
    )
}

/// Save certificate and key to files
pub fn save_certificate_files(
    cert: &GeneratedCertificate,
    cert_path: &Path,
    key_path: &Path,
) -> Result<(), String> {
    if let Some(parent) = cert_path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create certificate directory: {e}"))?;
    }

    if let Some(parent) = key_path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("Failed to create key directory: {e}"))?;
    }

    fs::write(cert_path, &cert.certificate_pem)
        .map_err(|e| format!("Failed to write certificate file: {e}"))?;

    write_private_key(key_path, &cert.private_key_pem)?;

    Ok(())
}

/// Write private key file with appropriate permissions (Unix: 0600)
fn write_private_key(path: &Path, content: &str) -> Result<(), String> {
    fs::write(path, content).map_err(|e| format!("Failed to write private key file: {e}"))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let permissions = fs::Permissions::from_mode(0o600);
        fs::set_permissions(path, permissions)
            .map_err(|e| format!("Failed to set key file permissions: {e}"))?;
    }

    Ok(())
}

/// Parse a PEM certificate file and extract structured X.509 details.
pub fn inspect_certificate(cert_path: &Path) -> Result<CertificateDetails, String> {
    let pem_content =
        fs::read_to_string(cert_path).map_err(|e| format!("Failed to read certificate: {e}"))?;

    let (_, pem) =
        parse_x509_pem(pem_content.as_bytes()).map_err(|e| format!("Failed to parse PEM: {e}"))?;

    let (_, x509) = x509_parser::parse_x509_certificate(&pem.contents)
        .map_err(|e| format!("Failed to parse X.509 certificate: {e}"))?;

    let subject = x509.subject().to_string();
    let issuer = x509.issuer().to_string();

    let subject_cn = x509
        .subject()
        .iter_common_name()
        .next()
        .and_then(|cn| cn.as_str().ok().map(String::from));

    let not_before = x509.validity().not_before.to_rfc2822().unwrap_or_default();
    let not_after = x509.validity().not_after.to_rfc2822().unwrap_or_default();
    let is_currently_valid = x509.validity().is_valid();

    let now_epoch = OffsetDateTime::now_utc().unix_timestamp();
    let expiry_epoch = x509.validity().not_after.timestamp();
    let days_until_expiry = (expiry_epoch - now_epoch) / 86400;

    let serial = format_serial(x509.raw_serial());

    let fingerprint = sha256_fingerprint(&pem.contents);

    let signature_algorithm = x509.signature_algorithm.algorithm.to_id_string();

    let public_key_algorithm = x509.public_key().algorithm.algorithm.to_id_string();

    let san = x509
        .subject_alternative_name()
        .ok()
        .flatten()
        .map(|ext| {
            ext.value
                .general_names
                .iter()
                .map(|name| format!("{name}"))
                .collect()
        })
        .unwrap_or_default();

    let is_self_signed = subject == issuer;

    Ok(CertificateDetails {
        subject_cn,
        subject,
        issuer,
        serial,
        not_before,
        not_after,
        is_currently_valid,
        days_until_expiry,
        fingerprint,
        signature_algorithm,
        public_key_algorithm,
        san,
        is_self_signed,
        path: cert_path.to_path_buf(),
    })
}

/// Validate a private key file
pub fn validate_private_key_file(key_path: &Path) -> Result<(), String> {
    let pem_content =
        fs::read_to_string(key_path).map_err(|e| format!("Failed to read private key: {e}"))?;

    if pem_content.contains("-----BEGIN PRIVATE KEY-----")
        || pem_content.contains("-----BEGIN RSA PRIVATE KEY-----")
        || pem_content.contains("-----BEGIN EC PRIVATE KEY-----")
    {
        Ok(())
    } else {
        Err("File does not contain a valid PEM private key".to_string())
    }
}

/// Format a serial number as colon-separated hex
fn format_serial(raw: &[u8]) -> String {
    raw.iter()
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join(":")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_default_certificate() {
        let params = CertGenParams::default();
        let result = generate_certificate_internal(&params);
        assert!(result.is_ok());

        let cert = result.unwrap();
        assert!(cert.certificate_pem.contains("-----BEGIN CERTIFICATE-----"));
        assert!(cert.private_key_pem.contains("-----BEGIN"));
        assert!(!cert.fingerprint.is_empty());
    }

    #[test]
    fn test_fingerprint_format() {
        let params = CertGenParams::default();
        let cert = generate_certificate_internal(&params).unwrap();

        // SHA-256 = 32 bytes = 32 colon-separated hex pairs
        let parts: Vec<&str> = cert.fingerprint.split(':').collect();
        assert_eq!(parts.len(), 32);
        for part in &parts {
            assert_eq!(part.len(), 2);
            assert!(part.chars().all(|c| c.is_ascii_hexdigit()));
        }
    }

    #[test]
    fn test_inspect_generated_certificate() {
        let params = CertGenParams::default();
        let cert = generate_certificate_internal(&params).unwrap();

        let dir = tempfile::tempdir().unwrap();
        let cert_path = dir.path().join("cert.pem");
        let key_path = dir.path().join("key.pem");
        save_certificate_files(&cert, &cert_path, &key_path).unwrap();

        let details = inspect_certificate(&cert_path).unwrap();
        assert!(details.is_currently_valid);
        assert!(details.days_until_expiry > 360);
        assert!(details.is_self_signed);
        assert!(!details.fingerprint.is_empty());
        assert!(!details.san.is_empty());
    }

    #[test]
    fn test_certificate_details_serializable() {
        let params = CertGenParams::default();
        let cert = generate_certificate_internal(&params).unwrap();

        let dir = tempfile::tempdir().unwrap();
        let cert_path = dir.path().join("cert.pem");
        let key_path = dir.path().join("key.pem");
        save_certificate_files(&cert, &cert_path, &key_path).unwrap();

        let details = inspect_certificate(&cert_path).unwrap();
        let json = serde_json::to_string_pretty(&details).unwrap();
        assert!(json.contains("fingerprint"));
        assert!(json.contains("days_until_expiry"));
    }
}
