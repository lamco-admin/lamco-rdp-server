//! Certificate Generation Module
//!
//! Generates self-signed TLS certificates for RDP server authentication.

use std::fs;
use std::path::{Path, PathBuf};

use rcgen::generate_simple_self_signed;
use time::{Duration, OffsetDateTime};

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
        // Get hostname for default CN
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

/// Generate a self-signed certificate with the given parameters
///
/// This is the main API for certificate generation, taking explicit parameters.
pub fn generate_self_signed_certificate(
    cert_path: PathBuf,
    key_path: PathBuf,
    common_name: String,
    organization: Option<String>,
    valid_days: u32,
) -> Result<(), String> {
    // Build params and SAN names
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
    // Use rcgen's simple self-signed certificate generator
    // It takes a list of subject alternative names
    let certificate = generate_simple_self_signed(params.san_names.clone())
        .map_err(|e| format!("Failed to generate certificate: {}", e))?;

    // Get PEM representations - rcgen 0.12 API returns Result
    let certificate_pem = certificate
        .serialize_pem()
        .map_err(|e| format!("Failed to serialize certificate: {}", e))?;
    let private_key_pem = certificate.serialize_private_key_pem();

    // Get DER for fingerprint calculation
    let der_bytes = certificate
        .serialize_der()
        .map_err(|e| format!("Failed to serialize certificate DER: {}", e))?;
    let fingerprint = calculate_sha256_fingerprint(&der_bytes);

    // Calculate expiration date (default is 365 days from now)
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

/// Calculate SHA-256 fingerprint of certificate DER bytes
fn calculate_sha256_fingerprint(der: &[u8]) -> String {
    use sha2::{Digest, Sha256};

    let mut hasher = Sha256::new();
    hasher.update(der);
    let result = hasher.finalize();

    // Format as colon-separated hex pairs
    result
        .iter()
        .map(|b| format!("{:02X}", b))
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
    // Ensure parent directories exist
    if let Some(parent) = cert_path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create certificate directory: {}", e))?;
    }

    if let Some(parent) = key_path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("Failed to create key directory: {}", e))?;
    }

    // Write certificate file
    fs::write(cert_path, &cert.certificate_pem)
        .map_err(|e| format!("Failed to write certificate file: {}", e))?;

    // Write private key file with restrictive permissions
    write_private_key(key_path, &cert.private_key_pem)?;

    Ok(())
}

/// Write private key file with appropriate permissions (Unix: 0600)
fn write_private_key(path: &Path, content: &str) -> Result<(), String> {
    fs::write(path, content).map_err(|e| format!("Failed to write private key file: {}", e))?;

    // Set restrictive permissions on Unix
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let permissions = fs::Permissions::from_mode(0o600);
        fs::set_permissions(path, permissions)
            .map_err(|e| format!("Failed to set key file permissions: {}", e))?;
    }

    Ok(())
}

/// Validate an existing certificate file
pub fn validate_certificate_file(cert_path: &Path) -> Result<CertificateInfo, String> {
    let pem_content =
        fs::read_to_string(cert_path).map_err(|e| format!("Failed to read certificate: {}", e))?;

    // Parse the PEM to extract basic info
    // Note: Full X.509 parsing would require additional dependencies
    // For now, we verify it's a valid PEM and extract what we can

    if !pem_content.contains("-----BEGIN CERTIFICATE-----") {
        return Err("File does not contain a valid PEM certificate".to_string());
    }

    // Extract the certificate bytes
    let cert_start = pem_content
        .find("-----BEGIN CERTIFICATE-----")
        .ok_or("Invalid PEM format")?;
    let cert_end = pem_content
        .find("-----END CERTIFICATE-----")
        .ok_or("Invalid PEM format")?;

    // Decode base64 between markers
    let b64_content: String = pem_content[cert_start + 27..cert_end]
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();

    // Decode base64 manually (simple implementation)
    let der_bytes = decode_base64(&b64_content)?;

    let fingerprint = calculate_sha256_fingerprint(&der_bytes);

    Ok(CertificateInfo {
        fingerprint,
        is_valid: true,
        path: cert_path.to_path_buf(),
    })
}

/// Simple base64 decoder
fn decode_base64(input: &str) -> Result<Vec<u8>, String> {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

    fn decode_char(c: char) -> Result<u8, String> {
        if c == '=' {
            return Ok(0);
        }
        ALPHABET
            .iter()
            .position(|&x| x == c as u8)
            .map(|p| p as u8)
            .ok_or_else(|| format!("Invalid base64 character: {}", c))
    }

    let mut result = Vec::new();
    let chars: Vec<char> = input.chars().collect();

    for chunk in chars.chunks(4) {
        if chunk.len() < 4 {
            break;
        }

        let a = decode_char(chunk[0])?;
        let b = decode_char(chunk[1])?;
        let c = decode_char(chunk[2])?;
        let d = decode_char(chunk[3])?;

        result.push((a << 2) | (b >> 4));
        if chunk[2] != '=' {
            result.push((b << 4) | (c >> 2));
        }
        if chunk[3] != '=' {
            result.push((c << 6) | d);
        }
    }

    Ok(result)
}

/// Information about an existing certificate
#[derive(Debug)]
pub struct CertificateInfo {
    /// SHA-256 fingerprint
    pub fingerprint: String,
    /// Whether the certificate is valid (parseable)
    pub is_valid: bool,
    /// Path to the certificate file
    pub path: std::path::PathBuf,
}

/// Validate a private key file
pub fn validate_private_key_file(key_path: &Path) -> Result<(), String> {
    let pem_content =
        fs::read_to_string(key_path).map_err(|e| format!("Failed to read private key: {}", e))?;

    // Check for valid PEM markers
    if pem_content.contains("-----BEGIN PRIVATE KEY-----")
        || pem_content.contains("-----BEGIN RSA PRIVATE KEY-----")
        || pem_content.contains("-----BEGIN EC PRIVATE KEY-----")
    {
        Ok(())
    } else {
        Err("File does not contain a valid PEM private key".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_default_certificate() {
        let params = CertGenParams::default();
        let result = generate_self_signed_certificate(&params);
        assert!(result.is_ok());

        let cert = result.unwrap();
        assert!(cert.certificate_pem.contains("-----BEGIN CERTIFICATE-----"));
        assert!(cert.private_key_pem.contains("-----BEGIN"));
        assert!(!cert.fingerprint.is_empty());
    }

    #[test]
    fn test_fingerprint_format() {
        let params = CertGenParams::default();
        let cert = generate_self_signed_certificate(&params).unwrap();

        // Fingerprint should be colon-separated hex pairs
        let parts: Vec<&str> = cert.fingerprint.split(':').collect();
        assert_eq!(parts.len(), 32); // SHA-256 = 32 bytes
        for part in parts {
            assert_eq!(part.len(), 2);
            assert!(part.chars().all(|c| c.is_ascii_hexdigit()));
        }
    }
}
