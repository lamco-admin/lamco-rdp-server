//! Storage capability probe
//!
//! Detects available credential storage backends.

use serde::{Deserialize, Serialize};
use tracing::{debug, info};

use crate::capabilities::state::ServiceLevel;

/// Storage capabilities
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StorageCapabilities {
    /// Available storage backends
    pub backends: Vec<StorageBackend>,
    /// Selected backend
    pub selected: Option<StorageBackend>,
    /// Overall service level
    pub service_level: ServiceLevel,
}

/// Storage backend type
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum StorageBackend {
    /// freedesktop.org Secret Service (GNOME Keyring, KWallet)
    SecretService,
    /// TPM 2.0 sealed storage
    Tpm,
    /// Encrypted file (PBKDF2 + AES)
    EncryptedFile,
    /// Flatpak Secret Portal
    FlatpakSecret,
}

impl StorageBackend {
    pub fn name(&self) -> &str {
        match self {
            Self::SecretService => "Secret Service (Keyring)",
            Self::Tpm => "TPM 2.0",
            Self::EncryptedFile => "Encrypted File",
            Self::FlatpakSecret => "Flatpak Secret Portal",
        }
    }

    pub fn service_level(&self) -> ServiceLevel {
        match self {
            Self::SecretService => ServiceLevel::Full,
            Self::Tpm => ServiceLevel::Full,
            Self::FlatpakSecret => ServiceLevel::Degraded,
            Self::EncryptedFile => ServiceLevel::Fallback,
        }
    }
}

/// Storage probe
pub struct StorageProbe;

impl StorageProbe {
    pub async fn probe() -> StorageCapabilities {
        info!("Probing storage capabilities...");

        let mut backends = Vec::new();

        if std::env::var("FLATPAK_ID").is_ok() {
            backends.push(StorageBackend::FlatpakSecret);
            debug!("Flatpak Secret Portal available");
        }

        if Self::check_secret_service() {
            backends.push(StorageBackend::SecretService);
            debug!("Secret Service available");
        }

        if Self::check_tpm() {
            backends.push(StorageBackend::Tpm);
            debug!("TPM 2.0 available");
        }

        // Encrypted file is always available as fallback
        backends.push(StorageBackend::EncryptedFile);
        debug!("Encrypted file storage always available");

        backends.sort_by(|a, b| b.service_level().cmp(&a.service_level()));

        let selected = backends.first().cloned();
        let service_level = selected
            .as_ref()
            .map_or(ServiceLevel::Fallback, StorageBackend::service_level);

        info!(
            "Storage service level: {:?}, backend: {:?}",
            service_level, selected
        );

        StorageCapabilities {
            backends,
            selected,
            service_level,
        }
    }

    fn check_secret_service() -> bool {
        // Heuristic: assume available if GNOME or KDE (both ship a Secret Service provider)
        let desktop = std::env::var("XDG_CURRENT_DESKTOP")
            .unwrap_or_default()
            .to_lowercase();

        if desktop.contains("gnome") || desktop.contains("kde") || desktop.contains("plasma") {
            return true;
        }

        // Could also check for gnome-keyring or kwallet processes
        false
    }

    fn check_tpm() -> bool {
        std::path::Path::new("/dev/tpm0").exists() || std::path::Path::new("/dev/tpmrm0").exists()
    }
}
