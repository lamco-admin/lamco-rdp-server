//! Credential Storage & Deployment Detection
//!
//! Detects the deployment context (Flatpak, systemd, native, etc.) and
//! determines the best available method for storing session tokens securely.

use anyhow::{anyhow, Context, Result};
use std::path::Path;
use std::process::Command;
use tracing::{debug, info, warn};

/// Deployment context affecting available strategies
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum DeploymentContext {
    /// Native system package (full access)
    Native,
    /// Flatpak sandbox (restricted)
    Flatpak,
    /// systemd user service
    SystemdUser {
        /// loginctl enable-linger active
        linger_enabled: bool,
    },
    /// systemd system service (multi-user)
    SystemdSystem,
    /// Non-systemd init (OpenRC, runit, etc.)
    InitD,
}

impl std::fmt::Display for DeploymentContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Native => write!(f, "Native Package"),
            Self::Flatpak => write!(f, "Flatpak"),
            Self::SystemdUser { linger_enabled } => {
                write!(f, "systemd User Service (linger: {})", linger_enabled)
            }
            Self::SystemdSystem => write!(f, "systemd System Service"),
            Self::InitD => write!(f, "initd/OpenRC"),
        }
    }
}

/// Credential storage method
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum CredentialStorageMethod {
    /// No secure storage available (should not happen)
    None,
    /// GNOME Keyring via libsecret (direct access)
    GnomeKeyring,
    /// KDE Wallet via libsecret/kwallet (direct access)
    KWallet,
    /// KeePassXC via Secret Service (direct access)
    KeePassXC,
    /// Flatpak Secret Portal (sandboxed access to host keyring)
    FlatpakSecretPortal,
    /// TPM 2.0 bound storage via systemd-creds
    Tpm2,
    /// Encrypted file with machine-bound key
    EncryptedFile,
}

impl std::fmt::Display for CredentialStorageMethod {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::None => write!(f, "None"),
            Self::GnomeKeyring => write!(f, "GNOME Keyring"),
            Self::KWallet => write!(f, "KDE Wallet"),
            Self::KeePassXC => write!(f, "KeePassXC"),
            Self::FlatpakSecretPortal => write!(f, "Flatpak Secret Portal"),
            Self::Tpm2 => write!(f, "TPM 2.0"),
            Self::EncryptedFile => write!(f, "Encrypted File"),
        }
    }
}

/// Encryption type used for token storage
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum EncryptionType {
    /// No encryption (should not be used)
    None,
    /// AES-256-GCM
    Aes256Gcm,
    /// ChaCha20-Poly1305
    ChaCha20Poly1305,
    /// TPM-bound (key never leaves TPM)
    TpmBound,
    /// Host keyring encryption (Flatpak Secret Portal)
    HostKeyring,
}

impl std::fmt::Display for EncryptionType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::None => write!(f, "None"),
            Self::Aes256Gcm => write!(f, "AES-256-GCM"),
            Self::ChaCha20Poly1305 => write!(f, "ChaCha20-Poly1305"),
            Self::TpmBound => write!(f, "TPM-Bound"),
            Self::HostKeyring => write!(f, "Host Keyring"),
        }
    }
}

/// Secret Service backend type
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SecretServiceBackend {
    GnomeKeyring,
    KWallet,
    KeePassXC,
}

/// Detect deployment context
///
/// Automatically determines how the application is being run:
/// - Flatpak sandbox
/// - systemd user service
/// - systemd system service
/// - initd/OpenRC
/// - Native package
pub fn detect_deployment_context() -> DeploymentContext {
    debug!("Detecting deployment context...");

    // Check if running in Flatpak
    if Path::new("/.flatpak-info").exists() {
        info!("Detected Flatpak deployment");
        return DeploymentContext::Flatpak;
    }

    // Check if running as systemd service
    if let Ok(_invocation_id) = std::env::var("INVOCATION_ID") {
        // systemd sets INVOCATION_ID for all units

        // Check if user or system service
        if std::env::var("XDG_RUNTIME_DIR").is_ok() {
            // User service has XDG_RUNTIME_DIR

            // Check if linger is enabled
            let linger_enabled = check_linger_enabled();

            info!("Detected systemd user service (linger: {})", linger_enabled);
            return DeploymentContext::SystemdUser { linger_enabled };
        } else {
            // System service lacks user environment
            info!("Detected systemd system service");
            return DeploymentContext::SystemdSystem;
        }
    }

    // Check for systemd presence (even if not running as service)
    if Path::new("/run/systemd/system").exists() {
        debug!("systemd available but not running as service");
        // Running directly, not as service
        return DeploymentContext::Native;
    }

    // Check for OpenRC
    if Path::new("/run/openrc").exists() {
        info!("Detected OpenRC init system");
        return DeploymentContext::InitD;
    }

    // Default: assume native package
    info!("Detected native package deployment");
    DeploymentContext::Native
}

/// Check if loginctl enable-linger is active for current user
fn check_linger_enabled() -> bool {
    let uid = unsafe { libc::getuid() };

    // Try to get username
    let username = std::env::var("USER")
        .or_else(|_| std::env::var("LOGNAME"))
        .unwrap_or_else(|_| uid.to_string());

    let linger_path = format!("/var/lib/systemd/linger/{}", username);

    Path::new(&linger_path).exists()
}

/// Detect best available credential storage method
///
/// Returns: (storage method, encryption type, is accessible)
pub async fn detect_credential_storage(
    deployment: &DeploymentContext,
) -> (CredentialStorageMethod, EncryptionType, bool) {
    debug!(
        "Detecting credential storage for deployment: {}",
        deployment
    );

    // FLATPAK-SPECIFIC DETECTION
    if matches!(deployment, DeploymentContext::Flatpak) {
        return detect_flatpak_credential_storage().await;
    }

    // TPM 2.0 only available with systemd (not in Flatpak or initd)
    match deployment {
        DeploymentContext::SystemdUser { .. } | DeploymentContext::SystemdSystem => {
            if let Ok(has_tpm) = check_tpm2_available() {
                if has_tpm {
                    let accessible = check_systemd_creds_accessible();
                    info!("TPM 2.0 detected, will use systemd-creds for storage");
                    return (
                        CredentialStorageMethod::Tpm2,
                        EncryptionType::TpmBound,
                        accessible,
                    );
                }
            }
        }
        _ => {} // TPM via systemd-creds not available
    }

    // Secret Service API (GNOME Keyring, KWallet, KeePassXC)
    // Not directly available in Flatpak (must use portal)
    if !matches!(deployment, DeploymentContext::Flatpak) {
        if let Ok(service) = detect_secret_service().await {
            let (method, encryption) = match service {
                SecretServiceBackend::GnomeKeyring => (
                    CredentialStorageMethod::GnomeKeyring,
                    EncryptionType::Aes256Gcm,
                ),
                SecretServiceBackend::KWallet => {
                    (CredentialStorageMethod::KWallet, EncryptionType::Aes256Gcm)
                }
                SecretServiceBackend::KeePassXC => (
                    CredentialStorageMethod::KeePassXC,
                    EncryptionType::ChaCha20Poly1305,
                ),
            };

            let accessible = check_secret_service_unlocked().await;
            info!(
                "Secret Service detected: {} (unlocked: {})",
                method, accessible
            );
            return (method, encryption, accessible);
        }
    }

    // Encrypted file fallback (always available)
    info!("Using encrypted file storage (Secret Service not available)");
    (
        CredentialStorageMethod::EncryptedFile,
        EncryptionType::Aes256Gcm,
        true, // Always accessible
    )
}

/// Flatpak-specific credential storage detection
async fn detect_flatpak_credential_storage() -> (CredentialStorageMethod, EncryptionType, bool) {
    debug!("Detecting Flatpak credential storage...");

    // Priority 1: Flatpak Secret Portal (recommended)
    if check_flatpak_secret_portal_available().await {
        info!("Flatpak: Using Secret Portal for credential storage");
        return (
            CredentialStorageMethod::FlatpakSecretPortal,
            EncryptionType::HostKeyring,
            true,
        );
    }

    // Priority 2: Encrypted file in app data directory
    // Location: ~/.var/app/org.lamco.RdpServer/data/lamco-rdp-server/
    info!("Flatpak: Using encrypted file storage (Secret Portal unavailable)");
    (
        CredentialStorageMethod::EncryptedFile,
        EncryptionType::Aes256Gcm,
        true, // Always available in Flatpak app data dir
    )
}

/// Check if TPM 2.0 is available via systemd-creds
fn check_tpm2_available() -> Result<bool> {
    debug!("Checking for TPM 2.0 availability...");

    // Check via systemd-creds has-tpm2 (systemd 250+)
    let output = Command::new("systemd-creds")
        .arg("has-tpm2")
        .output()
        .context("Failed to run systemd-creds")?;

    let has_tpm =
        output.status.success() && String::from_utf8_lossy(&output.stdout).trim() == "yes";

    debug!("TPM 2.0 available: {}", has_tpm);
    Ok(has_tpm)
}

/// Check if systemd-creds is accessible
fn check_systemd_creds_accessible() -> bool {
    // Try to run systemd-creds --help
    Command::new("systemd-creds")
        .arg("--help")
        .output()
        .map(|out| out.status.success())
        .unwrap_or(false)
}

/// Detect Secret Service backend
async fn detect_secret_service() -> Result<SecretServiceBackend> {
    debug!("Detecting Secret Service backend...");

    let connection = zbus::Connection::session()
        .await
        .context("Failed to connect to D-Bus session")?;

    // Check if Secret Service is available
    let proxy = zbus::fdo::DBusProxy::new(&connection)
        .await
        .context("Failed to create D-Bus proxy")?;

    let names = proxy
        .list_names()
        .await
        .context("Failed to list D-Bus names")?;

    if !names
        .iter()
        .any(|n| n.as_str() == "org.freedesktop.secrets")
    {
        return Err(anyhow!("Secret Service not available on D-Bus"));
    }

    // Detect which backend is providing it
    if names.iter().any(|n| n.starts_with("org.gnome.keyring")) {
        debug!("Detected GNOME Keyring");
        Ok(SecretServiceBackend::GnomeKeyring)
    } else if names.iter().any(|n| n.starts_with("org.kde.kwalletd")) {
        debug!("Detected KDE Wallet");
        Ok(SecretServiceBackend::KWallet)
    } else if names.iter().any(|n| n.as_str().contains("keepassxc")) {
        debug!("Detected KeePassXC");
        Ok(SecretServiceBackend::KeePassXC)
    } else {
        // Generic Secret Service (assume GNOME-like behavior)
        debug!("Detected generic Secret Service backend");
        Ok(SecretServiceBackend::GnomeKeyring)
    }
}

/// Check if Secret Service is unlocked and accessible
async fn check_secret_service_unlocked() -> bool {
    // Use the actual Secret Service client to check
    use super::secret_service::AsyncSecretServiceClient;

    match AsyncSecretServiceClient::connect().await {
        Ok(client) => {
            // Try a simple operation to verify it's unlocked
            // If locked, this will fail
            tokio::task::spawn_blocking(move || {
                super::secret_service::check_secret_service_unlocked()
            })
            .await
            .unwrap_or(false)
        }
        Err(_) => false,
    }
}

/// Check if Flatpak Secret Portal is available
async fn check_flatpak_secret_portal_available() -> bool {
    debug!("Checking for Flatpak Secret Portal...");

    let connection = match zbus::Connection::session().await {
        Ok(conn) => conn,
        Err(e) => {
            debug!("Failed to connect to D-Bus: {}", e);
            return false;
        }
    };

    let proxy = match zbus::fdo::DBusProxy::new(&connection).await {
        Ok(p) => p,
        Err(e) => {
            debug!("Failed to create D-Bus proxy: {}", e);
            return false;
        }
    };

    match proxy.list_names().await {
        Ok(names) => {
            let available = names
                .iter()
                .any(|n| n.as_str() == "org.freedesktop.portal.Secret");
            debug!("Flatpak Secret Portal available: {}", available);
            available
        }
        Err(e) => {
            debug!("Failed to list D-Bus names: {}", e);
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_deployment_detection() {
        // Should not panic
        let deployment = detect_deployment_context();
        assert!(matches!(
            deployment,
            DeploymentContext::Native
                | DeploymentContext::Flatpak
                | DeploymentContext::SystemdUser { .. }
                | DeploymentContext::SystemdSystem
                | DeploymentContext::InitD
        ));
    }

    #[test]
    fn test_linger_check() {
        // Should not panic
        let _ = check_linger_enabled();
    }

    #[tokio::test]
    async fn test_credential_storage_detection() {
        // Should not panic
        let deployment = detect_deployment_context();
        let (method, encryption, accessible) = detect_credential_storage(&deployment).await;

        // Should always return something
        assert!(
            !matches!(method, CredentialStorageMethod::None),
            "Should never return None, always have EncryptedFile fallback"
        );

        println!(
            "Detected: {} (encryption: {}, accessible: {})",
            method, encryption, accessible
        );
    }
}
