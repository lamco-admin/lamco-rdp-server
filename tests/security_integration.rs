use lamco_rdp_server::security::{AuthMethod, CertificateGenerator, TlsConfig, UserAuthenticator};
use tempfile::TempDir;

#[test]
fn test_certificate_generation_and_loading() {
    let temp_dir = TempDir::new().unwrap();
    let cert_path = temp_dir.path().join("cert.pem");
    let key_path = temp_dir.path().join("key.pem");

    // Generate certificate
    CertificateGenerator::generate_and_save("test-server", 365, &cert_path, &key_path).unwrap();

    // Load TLS config
    let tls_config = TlsConfig::from_files(&cert_path, &key_path).unwrap();

    // Verify TLS config is valid (internally checks certificate chain)
    assert!(tls_config.verify().is_ok());
}

#[test]
fn test_authentication_none_method() {
    let auth = UserAuthenticator::new(AuthMethod::None, None);
    assert!(auth.authenticate("testuser", "testpass").unwrap());
}
