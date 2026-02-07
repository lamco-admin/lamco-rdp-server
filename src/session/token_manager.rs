//! Session Token Persistence
//!
//! **Execution Path:** Multiple backends (Secret Service, TPM, Flatpak Secret, Encrypted File)
//! **Status:** Active (v1.0.0+)
//! **Platform:** Universal (adapts to deployment)
//! **Role:** Secure storage and retrieval of Portal restore tokens
//!
//! Handles secure storage and retrieval of portal restore tokens across
//! different credential storage backends with COMPLETE implementations.
//!
//! NO STUBS - All backends fully functional for production use.

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use tracing::{debug, info, warn};
use zeroize::Zeroizing;

use super::credentials::{detect_deployment_context, CredentialStorageMethod};
use super::flatpak_secret::FlatpakSecrets;
use super::secret_service::AsyncSecretServiceClient;
use super::tpm_store::AsyncTpmCredentialStore;

/// Token metadata for debugging and validation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenMetadata {
    /// When token was stored
    pub stored_at: String,
    /// Deployment context when stored
    pub deployment: String,
    /// Credential storage method used
    pub storage_method: String,
    /// Encryption type
    pub encryption: String,
}

/// Token Manager handles secure storage of session restore tokens
///
/// PRODUCTION-READY implementations for all backends:
/// - Flatpak Secret Manager (Secret Service or encrypted file)
/// - TPM 2.0 via systemd-creds (hardware-bound)
/// - Secret Service (GNOME Keyring, KWallet, KeePassXC)
/// - Encrypted File (machine-bound AES-256-GCM)
pub struct Tokens {
    storage_method: CredentialStorageMethod,
    storage_path: PathBuf,
    secret_service: Option<AsyncSecretServiceClient>,
    flatpak_manager: Option<FlatpakSecrets>,
    tpm_store: Option<AsyncTpmCredentialStore>,
}

impl Tokens {
    pub async fn new(method: CredentialStorageMethod) -> Result<Self> {
        info!("Initializing Tokens with method: {}", method);

        let storage_path = if Path::new("/.flatpak-info").exists() {
            let home = std::env::var("HOME").context("HOME not set")?;
            let xdg_data =
                std::env::var("XDG_DATA_HOME").unwrap_or_else(|_| format!("{}/.local/share", home));

            PathBuf::from(xdg_data)
                .join("lamco-rdp-server")
                .join("sessions")
        } else {
            dirs::data_local_dir()
                .unwrap_or_else(|| {
                    PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| ".".to_string()))
                        .join(".local/share")
                })
                .join("lamco-rdp-server")
                .join("sessions")
        };

        fs::create_dir_all(&storage_path).context("Failed to create session storage directory")?;

        debug!("Token storage path: {:?}", storage_path);

        let secret_service = match method {
            CredentialStorageMethod::GnomeKeyring
            | CredentialStorageMethod::KWallet
            | CredentialStorageMethod::KeePassXC => {
                match AsyncSecretServiceClient::connect().await {
                    Ok(client) => {
                        info!("{} client initialized successfully", method);
                        Some(client)
                    }
                    Err(e) => {
                        warn!("Failed to initialize Secret Service for {}: {}", method, e);
                        warn!("Falling back to encrypted file storage");
                        None
                    }
                }
            }
            _ => None,
        };

        let flatpak_manager = match method {
            CredentialStorageMethod::FlatpakSecretPortal => match FlatpakSecrets::new().await {
                Ok(manager) => {
                    info!("Flatpak secret manager initialized");
                    Some(manager)
                }
                Err(e) => {
                    warn!("Failed to initialize Flatpak secret manager: {}", e);
                    warn!("Falling back to encrypted file storage");
                    None
                }
            },
            _ => None,
        };

        let tpm_store = match method {
            CredentialStorageMethod::Tpm2 => match AsyncTpmCredentialStore::new().await {
                Ok(store) => {
                    info!("TPM 2.0 credential store initialized");
                    Some(store)
                }
                Err(e) => {
                    warn!("Failed to initialize TPM 2.0 store: {}", e);
                    warn!("Falling back to encrypted file storage");
                    None
                }
            },
            _ => None,
        };

        Ok(Self {
            storage_method: method,
            storage_path,
            secret_service,
            flatpak_manager,
            tpm_store,
        })
    }

    pub async fn save_token(&self, session_id: &str, token: &str) -> Result<()> {
        info!(
            "Saving restore token for session: {} (method: {})",
            session_id, self.storage_method
        );

        let key = format!("lamco-rdp-session-{}", session_id);
        let token_zeroized = Zeroizing::new(token.to_string());

        match self.storage_method {
            CredentialStorageMethod::GnomeKeyring
            | CredentialStorageMethod::KWallet
            | CredentialStorageMethod::KeePassXC => {
                if let Some(ref client) = self.secret_service {
                    let attrs = vec![
                        ("application".to_string(), "lamco-rdp-server".to_string()),
                        ("session_id".to_string(), session_id.to_string()),
                        ("type".to_string(), "portal-restore-token".to_string()),
                    ];

                    client
                        .store_secret(key, token_zeroized.to_string(), attrs)
                        .await
                        .context("Failed to store token in Secret Service")?;

                    info!("Token stored in {} successfully", self.storage_method);
                } else {
                    // Fallback to file
                    warn!("Secret Service client not initialized, using encrypted file");
                    self.save_token_to_file(session_id, &token_zeroized).await?;
                }
            }

            CredentialStorageMethod::FlatpakSecretPortal => {
                if let Some(ref manager) = self.flatpak_manager {
                    let stored = manager
                        .store_secret(
                            &key,
                            &token_zeroized,
                            &[("session_id", session_id), ("type", "portal-restore-token")],
                        )
                        .await
                        .context("Failed to store via Flatpak manager")?;

                    if stored {
                        info!("Token stored via Flatpak Secret Service");
                    } else {
                        // Fallback to file
                        self.save_token_to_file(session_id, &token_zeroized).await?;
                    }
                } else {
                    // Fallback to file
                    warn!("Flatpak manager not initialized, using encrypted file");
                    self.save_token_to_file(session_id, &token_zeroized).await?;
                }
            }

            CredentialStorageMethod::Tpm2 => {
                if let Some(ref store) = self.tpm_store {
                    store
                        .store(key, token_zeroized.as_bytes().to_vec())
                        .await
                        .context("Failed to store token in TPM")?;

                    info!("Token stored in TPM 2.0 bound storage successfully");
                } else {
                    // Fallback to file
                    warn!("TPM store not initialized, using encrypted file");
                    self.save_token_to_file(session_id, &token_zeroized).await?;
                }
            }

            CredentialStorageMethod::EncryptedFile => {
                self.save_token_to_file(session_id, &token_zeroized).await?;
            }

            CredentialStorageMethod::None => {
                return Err(anyhow!("No credential storage method configured"));
            }
        }

        self.save_token_metadata(session_id).await?;

        info!("Restore token saved successfully");
        Ok(())
    }

    pub async fn load_token(&self, session_id: &str) -> Result<Option<String>> {
        debug!(
            "Loading restore token for session: {} (method: {})",
            session_id, self.storage_method
        );

        let key = format!("lamco-rdp-session-{}", session_id);

        let token = match self.storage_method {
            CredentialStorageMethod::GnomeKeyring
            | CredentialStorageMethod::KWallet
            | CredentialStorageMethod::KeePassXC => {
                if let Some(ref client) = self.secret_service {
                    match client.lookup_secret(key).await {
                        Ok(token) => {
                            info!("Token loaded from {} successfully", self.storage_method);
                            Some(token)
                        }
                        Err(e) if Self::is_not_found_error(&e) => {
                            debug!("Token not found in Secret Service");
                            None
                        }
                        Err(e) => {
                            warn!("Error loading from Secret Service: {}", e);
                            // Try file fallback
                            self.load_token_from_file(session_id).await?
                        }
                    }
                } else {
                    // Fallback to file
                    self.load_token_from_file(session_id).await?
                }
            }

            CredentialStorageMethod::FlatpakSecretPortal => {
                if let Some(ref manager) = self.flatpak_manager {
                    match manager.retrieve_secret(&key).await {
                        Ok(Some(token)) => {
                            info!("Token loaded from Flatpak Secret Service");
                            Some(token)
                        }
                        Ok(None) => {
                            // Fallback to file
                            self.load_token_from_file(session_id).await?
                        }
                        Err(e) => {
                            warn!("Error loading from Flatpak manager: {}", e);
                            self.load_token_from_file(session_id).await?
                        }
                    }
                } else {
                    self.load_token_from_file(session_id).await?
                }
            }

            CredentialStorageMethod::Tpm2 => {
                if let Some(ref store) = self.tpm_store {
                    match store.load(key).await {
                        Ok(bytes) => {
                            let token = String::from_utf8(bytes)
                                .context("TPM credential contains invalid UTF-8")?;
                            info!("Token loaded from TPM 2.0 storage successfully");
                            Some(token)
                        }
                        Err(e) if Self::is_not_found_error(&e) => {
                            debug!("Token not found in TPM storage");
                            None
                        }
                        Err(e) => {
                            warn!("Error loading from TPM: {}", e);
                            // Try file fallback
                            self.load_token_from_file(session_id).await?
                        }
                    }
                } else {
                    self.load_token_from_file(session_id).await?
                }
            }

            CredentialStorageMethod::EncryptedFile => self.load_token_from_file(session_id).await?,

            CredentialStorageMethod::None => None,
        };

        if token.is_some() {
            info!("Restore token loaded successfully");
        } else {
            debug!("No restore token found for session: {}", session_id);
        }

        Ok(token)
    }

    pub async fn delete_token(&self, session_id: &str) -> Result<()> {
        info!("Deleting restore token for session: {}", session_id);

        let key = format!("lamco-rdp-session-{}", session_id);

        match self.storage_method {
            CredentialStorageMethod::GnomeKeyring
            | CredentialStorageMethod::KWallet
            | CredentialStorageMethod::KeePassXC => {
                if let Some(ref client) = self.secret_service {
                    client
                        .delete_secret(key)
                        .await
                        .context("Failed to delete from Secret Service")?;
                }
                // Also delete file backup if exists
                self.delete_token_file(session_id)?;
            }

            CredentialStorageMethod::FlatpakSecretPortal => {
                if let Some(ref manager) = self.flatpak_manager {
                    manager
                        .delete_secret(&key)
                        .await
                        .context("Failed to delete from Flatpak manager")?;
                }
                // Also delete file backup if exists
                self.delete_token_file(session_id)?;
            }

            CredentialStorageMethod::Tpm2 => {
                if let Some(ref store) = self.tpm_store {
                    store
                        .delete(key)
                        .await
                        .context("Failed to delete from TPM")?;
                }
                // Also delete file backup if exists
                self.delete_token_file(session_id)?;
            }

            CredentialStorageMethod::EncryptedFile => {
                self.delete_token_file(session_id)?;
            }

            CredentialStorageMethod::None => {
                warn!("No storage method configured");
            }
        }

        info!("Restore token deleted successfully");
        Ok(())
    }

    async fn save_token_to_file(&self, session_id: &str, token: &str) -> Result<()> {
        let encrypted = self.encrypt_token(token)?;
        let path = self.storage_path.join(format!("{}.token", session_id));

        fs::write(&path, &encrypted).context("Failed to write token file")?;

        // Restrict permissions (Unix only)
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let perms = fs::Permissions::from_mode(0o600);
            fs::set_permissions(&path, perms).context("Failed to set file permissions")?;
        }

        debug!("Token written to encrypted file: {:?}", path);
        Ok(())
    }

    async fn load_token_from_file(&self, session_id: &str) -> Result<Option<String>> {
        let path = self.storage_path.join(format!("{}.token", session_id));

        if !path.exists() {
            debug!("Token file does not exist: {:?}", path);
            return Ok(None);
        }

        let encrypted = fs::read(&path).context("Failed to read token file")?;
        let token = self.decrypt_token(&encrypted)?;

        debug!("Token loaded from file: {:?}", path);
        Ok(Some(token))
    }

    fn delete_token_file(&self, session_id: &str) -> Result<()> {
        let path = self.storage_path.join(format!("{}.token", session_id));
        if path.exists() {
            fs::remove_file(&path).context("Failed to delete token file")?;
        }

        let metadata_path = self.storage_path.join(format!("{}.json", session_id));
        if metadata_path.exists() {
            fs::remove_file(&metadata_path).context("Failed to delete metadata file")?;
        }

        Ok(())
    }

    fn encrypt_token(&self, token: &str) -> Result<Vec<u8>> {
        use aes_gcm::aead::{Aead, KeyInit, OsRng};
        use aes_gcm::{Aes256Gcm, Key, Nonce};

        let key_bytes = derive_machine_key()?;
        let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&key_bytes));

        let mut nonce_bytes = [0u8; 12];
        use aes_gcm::aead::rand_core::RngCore;
        OsRng.fill_bytes(&mut nonce_bytes);
        let nonce = Nonce::from_slice(&nonce_bytes);

        let ciphertext = cipher
            .encrypt(nonce, token.as_bytes())
            .map_err(|e| anyhow!("Encryption failed: {}", e))?;

        // Prepend nonce to ciphertext for storage
        let mut result = nonce_bytes.to_vec();
        result.extend(ciphertext);

        debug!("Token encrypted ({} bytes)", result.len());
        Ok(result)
    }

    fn decrypt_token(&self, data: &[u8]) -> Result<String> {
        if data.len() < 12 {
            return Err(anyhow!("Invalid encrypted data (too short)"));
        }

        use aes_gcm::aead::{Aead, KeyInit};
        use aes_gcm::{Aes256Gcm, Key, Nonce};

        let key_bytes = derive_machine_key()?;
        let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&key_bytes));

        let nonce = Nonce::from_slice(&data[..12]);
        let ciphertext = &data[12..];

        let plaintext = cipher
            .decrypt(nonce, ciphertext)
            .map_err(|e| anyhow!("Decryption failed: {}", e))?;

        String::from_utf8(plaintext).context("Token contains invalid UTF-8")
    }

    async fn save_token_metadata(&self, session_id: &str) -> Result<()> {
        let metadata = TokenMetadata {
            stored_at: chrono::Utc::now().to_rfc3339(),
            deployment: format!("{:?}", detect_deployment_context()),
            storage_method: format!("{}", self.storage_method),
            encryption: match self.storage_method {
                CredentialStorageMethod::Tpm2 => "TPM-Bound".to_string(),
                CredentialStorageMethod::FlatpakSecretPortal => "Host Keyring".to_string(),
                CredentialStorageMethod::GnomeKeyring
                | CredentialStorageMethod::KWallet
                | CredentialStorageMethod::KeePassXC => {
                    "AES-256-GCM (via Secret Service)".to_string()
                }
                CredentialStorageMethod::EncryptedFile => "AES-256-GCM (machine-bound)".to_string(),
                CredentialStorageMethod::None => "None".to_string(),
            },
        };

        let path = self.storage_path.join(format!("{}.json", session_id));
        let json = serde_json::to_string_pretty(&metadata)?;

        fs::write(&path, json).context("Failed to write metadata")?;

        debug!("Token metadata saved: {:?}", path);
        Ok(())
    }

    fn is_not_found_error(error: &anyhow::Error) -> bool {
        let error_str = error.to_string().to_lowercase();
        error_str.contains("not found")
            || error_str.contains("does not exist")
            || error_str.contains("no such")
    }
}

/// Derive a machine-specific encryption key with comprehensive fallback chain
///
/// Priority:
/// 1. /etc/machine-id (stable, unique per machine)
/// 2. /var/lib/dbus/machine-id (alternate location)
/// 3. hostname (weaker but functional)
/// 4. static salt (weakest - tokens still encrypted but not machine-bound)
fn derive_machine_key() -> Result<[u8; 32]> {
    use sha2::{Digest, Sha256};

    let mut hasher = Sha256::new();

    // Try machine-id (stable across reboots, unique per machine)
    if let Ok(machine_id) = fs::read_to_string("/etc/machine-id") {
        hasher.update(machine_id.trim().as_bytes());
        debug!("Using /etc/machine-id for key derivation");
    } else if let Ok(machine_id) = fs::read_to_string("/var/lib/dbus/machine-id") {
        // Fallback location on some systems
        hasher.update(machine_id.trim().as_bytes());
        debug!("Using /var/lib/dbus/machine-id for key derivation");
    } else if let Ok(hostname) = hostname::get() {
        // Weaker binding but still useful
        warn!("No machine-id found, using hostname for key derivation");
        warn!("This provides weaker security - tokens not uniquely machine-bound");
        hasher.update(hostname.to_string_lossy().as_bytes());
    } else {
        // Absolute worst case: static salt only
        warn!("No machine-id or hostname available");
        warn!("Using static salt for key derivation - WEAKEST SECURITY");
        warn!("Tokens will be encrypted but NOT machine-bound");
        warn!("Consider setting hostname or creating /etc/machine-id");
        hasher.update(b"lamco-rdp-server-static-fallback-key");
    }

    // Application-specific salt (always present)
    hasher.update(b"lamco-rdp-server-token-encryption-v1");

    let result = hasher.finalize();
    let mut key = [0u8; 32];
    key.copy_from_slice(&result);

    Ok(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_token_manager_creation() {
        let manager = Tokens::new(CredentialStorageMethod::EncryptedFile)
            .await
            .expect("Tokens creation failed");

        assert_eq!(
            manager.storage_method,
            CredentialStorageMethod::EncryptedFile
        );
        assert!(manager.storage_path.exists());
    }

    #[tokio::test]
    async fn test_token_save_load_roundtrip() {
        let manager = Tokens::new(CredentialStorageMethod::EncryptedFile)
            .await
            .expect("Tokens creation failed");

        let test_token = "test-restore-token-12345-abcdef";
        let session_id = "test-session";

        // Save token
        manager
            .save_token(session_id, test_token)
            .await
            .expect("Failed to save token");

        // Load token
        let loaded = manager
            .load_token(session_id)
            .await
            .expect("Failed to load token");

        assert_eq!(loaded, Some(test_token.to_string()));

        // Cleanup
        manager.delete_token(session_id).await.ok();
    }

    #[tokio::test]
    async fn test_token_not_found() {
        let manager = Tokens::new(CredentialStorageMethod::EncryptedFile)
            .await
            .expect("Tokens creation failed");

        let loaded = manager
            .load_token("nonexistent-session")
            .await
            .expect("Failed to query token");

        assert_eq!(loaded, None);
    }

    #[test]
    fn test_encryption_roundtrip() {
        let manager = Tokens {
            storage_method: CredentialStorageMethod::EncryptedFile,
            storage_path: PathBuf::from("/tmp"),
            secret_service: None,
            flatpak_manager: None,
            tpm_store: None,
        };

        let original = "my-secret-token";
        let encrypted = manager.encrypt_token(original).expect("Encryption failed");
        let decrypted = manager
            .decrypt_token(&encrypted)
            .expect("Decryption failed");

        assert_eq!(original, decrypted);
    }

    #[test]
    fn test_machine_key_derivation() {
        // Should not panic even if machine-id is missing
        let key1 = derive_machine_key().expect("Failed to derive key");
        let key2 = derive_machine_key().expect("Failed to derive key");

        // Key should be deterministic
        assert_eq!(key1, key2);
        assert_eq!(key1.len(), 32);
    }

    #[tokio::test]
    #[ignore] // Requires Secret Service running
    async fn test_secret_service_backend() {
        let manager = Tokens::new(CredentialStorageMethod::GnomeKeyring)
            .await
            .expect("Failed to create manager");

        let test_token = "secret-service-test-token";
        let session_id = "test-ss-session";

        // Save
        manager
            .save_token(session_id, test_token)
            .await
            .expect("Save failed");

        // Load
        let loaded = manager.load_token(session_id).await.expect("Load failed");
        assert_eq!(loaded, Some(test_token.to_string()));

        // Delete
        manager
            .delete_token(session_id)
            .await
            .expect("Delete failed");
    }

    #[tokio::test]
    #[ignore] // Requires TPM 2.0 hardware
    async fn test_tpm_backend() {
        let manager = Tokens::new(CredentialStorageMethod::Tpm2)
            .await
            .expect("Failed to create manager");

        let test_token = "tpm-test-token-12345";
        let session_id = "test-tpm-session";

        // Save
        manager
            .save_token(session_id, test_token)
            .await
            .expect("Save failed");

        // Load
        let loaded = manager.load_token(session_id).await.expect("Load failed");
        assert_eq!(loaded, Some(test_token.to_string()));

        // Delete
        manager
            .delete_token(session_id)
            .await
            .expect("Delete failed");
    }
}
