//! Flatpak Secret Storage Strategy
//!
//! **Execution Path:** Secret Service D-Bus or encrypted file fallback
//! **Status:** Active (v1.0.0+)
//! **Platform:** Flatpak-only
//! **Role:** Secure credential storage within sandbox constraints
//!
//! For Flatpak deployments, we attempt to use the host's Secret Service
//! via sandbox permissions. If that fails (restrictive sandbox), we fall
//! back to encrypted file storage in the app data directory.
//!
//! The org.freedesktop.portal.Secret portal provides a *master secret*
//! for the app, not general key-value storage, so we use Secret Service
//! when possible, and encrypted files as fallback.

use anyhow::{anyhow, Context, Result};
use tracing::{debug, info, warn};

use super::secret_service::AsyncSecretServiceClient;

/// Flatpak secret storage
///
/// Attempts Secret Service first (with --talk-name=org.freedesktop.secrets permission),
/// falls back to encrypted file if unavailable.
pub struct FlatpakSecrets {
    strategy: FlatpakSecretStrategy,
}

enum FlatpakSecretStrategy {
    /// Using host Secret Service (via sandbox permission)
    SecretService(AsyncSecretServiceClient),
    /// Fallback: encrypted file in app data dir
    /// (handled by Tokens encrypted file backend)
    EncryptedFileFallback,
}

impl FlatpakSecrets {
    pub async fn new() -> Result<Self> {
        info!("Initializing Flatpak secret manager");

        if !std::path::Path::new("/.flatpak-info").exists() {
            return Err(anyhow!("Not running in Flatpak"));
        }

        // Try to connect to Secret Service (may work with --talk-name=org.freedesktop.secrets)
        match AsyncSecretServiceClient::connect().await {
            Ok(client) => {
                info!("Flatpak: Using host Secret Service (sandbox permission granted)");
                Ok(Self {
                    strategy: FlatpakSecretStrategy::SecretService(client),
                })
            }
            Err(e) => {
                warn!("Flatpak: Cannot access host Secret Service: {}", e);
                info!("Flatpak: Using encrypted file storage fallback");
                Ok(Self {
                    strategy: FlatpakSecretStrategy::EncryptedFileFallback,
                })
            }
        }
    }

    pub fn uses_secret_service(&self) -> bool {
        matches!(self.strategy, FlatpakSecretStrategy::SecretService(_))
    }

    /// Store a secret
    ///
    /// Returns Ok(true) if stored via Secret Service, Ok(false) if fallback needed
    pub async fn store_secret(
        &self,
        key: &str,
        value: &str,
        attributes: &[(&str, &str)],
    ) -> Result<bool> {
        match &self.strategy {
            FlatpakSecretStrategy::SecretService(client) => {
                let attrs: Vec<(String, String)> = attributes
                    .iter()
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .collect();

                client
                    .store_secret(key.to_string(), value.to_string(), attrs)
                    .await
                    .context("Failed to store secret via Secret Service")?;

                debug!("Flatpak: Stored secret via host Secret Service");
                Ok(true)
            }
            FlatpakSecretStrategy::EncryptedFileFallback => {
                debug!("Flatpak: Secret Service unavailable, caller should use file fallback");
                Ok(false)
            }
        }
    }

    /// Retrieve a secret
    ///
    /// Returns Ok(Some(secret)) if found via Secret Service, Ok(None) if fallback needed
    pub async fn retrieve_secret(&self, key: &str) -> Result<Option<String>> {
        match &self.strategy {
            FlatpakSecretStrategy::SecretService(client) => {
                match client.lookup_secret(key.to_string()).await {
                    Ok(secret) => {
                        debug!("Flatpak: Retrieved secret via host Secret Service");
                        Ok(Some(secret))
                    }
                    Err(e) if Self::is_not_found(&e) => {
                        debug!("Flatpak: Secret not found in host keyring");
                        Ok(None)
                    }
                    Err(e) => Err(e).context("Failed to retrieve secret from Secret Service"),
                }
            }
            FlatpakSecretStrategy::EncryptedFileFallback => {
                debug!("Flatpak: Secret Service unavailable, caller should use file fallback");
                Ok(None)
            }
        }
    }

    pub async fn delete_secret(&self, key: &str) -> Result<bool> {
        match &self.strategy {
            FlatpakSecretStrategy::SecretService(client) => {
                client
                    .delete_secret(key.to_string())
                    .await
                    .context("Failed to delete secret from Secret Service")?;

                debug!("Flatpak: Deleted secret from host Secret Service");
                Ok(true)
            }
            FlatpakSecretStrategy::EncryptedFileFallback => {
                debug!("Flatpak: Secret Service unavailable, caller should use file fallback");
                Ok(false)
            }
        }
    }

    fn is_not_found(error: &anyhow::Error) -> bool {
        let error_str = error.to_string().to_lowercase();
        error_str.contains("not found")
            || error_str.contains("does not exist")
            || error_str.contains("no such")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[ignore] // Requires Flatpak environment
    async fn test_flatpak_secret_manager() {
        match FlatpakSecrets::new().await {
            Ok(manager) => {
                println!("Flatpak secret manager created");
                println!("Uses Secret Service: {}", manager.uses_secret_service());
            }
            Err(e) => {
                println!("Not in Flatpak or init failed: {}", e);
            }
        }
    }
}
