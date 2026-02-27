//! Flatpak Secret Storage Strategy
//!
//! **Execution Path:** Secret portal → Secret Service D-Bus → encrypted file fallback
//! **Status:** Active (v1.0.0+)
//! **Platform:** Flatpak-only
//! **Role:** Secure credential storage within sandbox constraints
//!
//! Three-level priority chain:
//! 1. Secret portal — sandbox-native, provides a master secret for HKDF key derivation
//! 2. Secret Service D-Bus — requires `--talk-name=org.freedesktop.secrets` permission
//! 3. Encrypted file — always available, uses machine-ID-derived key

use anyhow::{anyhow, Context, Result};
use tracing::{debug, info, warn};

use super::secret_service::AsyncSecretServiceClient;

/// Flatpak secret storage with three-level fallback chain
pub struct FlatpakSecrets {
    strategy: FlatpakSecretStrategy,
}

enum FlatpakSecretStrategy {
    /// Secret portal master secret → HKDF-SHA256 key derivation
    PortalDerived(Vec<u8>),
    /// Using host Secret Service (via sandbox permission)
    SecretService(AsyncSecretServiceClient),
    /// Fallback: encrypted file in app data dir
    EncryptedFileFallback,
}

impl FlatpakSecrets {
    pub async fn new() -> Result<Self> {
        info!("Initializing Flatpak secret manager");

        if !std::path::Path::new("/.flatpak-info").exists() {
            return Err(anyhow!("Not running in Flatpak"));
        }

        // Priority 1: Secret portal (sandbox-native, most secure)
        match Self::try_secret_portal().await {
            Ok(master_secret) => {
                info!("Flatpak: Using Secret portal for key derivation");
                return Ok(Self {
                    strategy: FlatpakSecretStrategy::PortalDerived(master_secret),
                });
            }
            Err(e) => {
                debug!("Secret portal unavailable: {}", e);
            }
        }

        // Priority 2: Secret Service D-Bus (may work with sandbox permission)
        match AsyncSecretServiceClient::connect().await {
            Ok(client) => {
                info!("Flatpak: Using host Secret Service");
                return Ok(Self {
                    strategy: FlatpakSecretStrategy::SecretService(client),
                });
            }
            Err(e) => {
                warn!("Flatpak: Cannot access host Secret Service: {}", e);
            }
        }

        // Priority 3: Encrypted file with machine-ID key (last resort)
        info!("Flatpak: Using encrypted file storage fallback");
        Ok(Self {
            strategy: FlatpakSecretStrategy::EncryptedFileFallback,
        })
    }

    /// Retrieve the master secret from the Secret portal.
    ///
    /// The portal returns a per-app secret via a Unix socket pair.
    /// This secret is stable across app restarts but unique per app ID.
    async fn try_secret_portal() -> Result<Vec<u8>> {
        let secret = ashpd::desktop::secret::retrieve()
            .await
            .context("Secret portal retrieval failed")?;

        if secret.is_empty() {
            return Err(anyhow!("Secret portal returned empty secret"));
        }

        debug!("Secret portal: received {} bytes", secret.len());
        Ok(secret)
    }

    /// Derive a 256-bit encryption key from the portal master secret.
    ///
    /// Uses HKDF-SHA256 with a domain-specific info string to ensure
    /// the derived key is unique to our token encryption use case.
    #[expect(
        clippy::expect_used,
        reason = "32 bytes is always valid for HKDF-SHA256"
    )]
    fn derive_encryption_key(master_secret: &[u8]) -> [u8; 32] {
        use hkdf::Hkdf;
        use sha2::Sha256;

        let hk = Hkdf::<Sha256>::new(None, master_secret);
        let mut key = [0u8; 32];
        hk.expand(b"lamco-rdp-server-token-encryption", &mut key)
            .expect("32 bytes is a valid HKDF-SHA256 output length");
        key
    }

    pub fn uses_secret_service(&self) -> bool {
        matches!(self.strategy, FlatpakSecretStrategy::SecretService(_))
    }

    pub fn uses_portal(&self) -> bool {
        matches!(self.strategy, FlatpakSecretStrategy::PortalDerived(_))
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
            FlatpakSecretStrategy::PortalDerived(_) => {
                // Portal provides a master secret for local encryption, not key-value storage.
                // Caller should use encrypted file backend with our derived key.
                debug!("Flatpak: Portal-derived strategy, caller should use file backend");
                Ok(false)
            }
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
            FlatpakSecretStrategy::PortalDerived(_) => {
                debug!("Flatpak: Portal-derived strategy, caller should use file backend");
                Ok(None)
            }
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
            FlatpakSecretStrategy::PortalDerived(_) => {
                debug!("Flatpak: Portal-derived strategy, caller should use file backend");
                Ok(false)
            }
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

    /// Get the portal-derived encryption key, if available.
    ///
    /// When the Secret portal is used, this returns a 256-bit key derived via
    /// HKDF-SHA256 from the portal's master secret. This key is more secure
    /// than the machine-ID-based fallback because it's sandbox-native and
    /// per-application.
    pub fn portal_derived_key(&self) -> Option<[u8; 32]> {
        match &self.strategy {
            FlatpakSecretStrategy::PortalDerived(master_secret) => {
                Some(Self::derive_encryption_key(master_secret))
            }
            _ => None,
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
    #[ignore = "Requires Flatpak environment"]
    async fn test_flatpak_secret_manager() {
        match FlatpakSecrets::new().await {
            Ok(manager) => {
                println!("Flatpak secret manager created");
                println!("Uses Secret Service: {}", manager.uses_secret_service());
            }
            Err(e) => {
                println!("Not in Flatpak or init failed: {e}");
            }
        }
    }
}
