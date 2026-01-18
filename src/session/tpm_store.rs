//! TPM 2.0 Credential Storage via systemd-creds
//!
//! Implements secure token storage using TPM 2.0 hardware via systemd-creds.
//! Credentials are bound to the TPM and cannot be extracted or used on other machines.

use anyhow::{anyhow, Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;
use tracing::{debug, info, warn};

/// TPM 2.0 credential store using systemd-creds
pub struct TpmCredentialStore {
    storage_path: PathBuf,
}

impl TpmCredentialStore {
    /// Create a new TPM credential store
    ///
    /// # Returns
    ///
    /// Configured store if TPM 2.0 is available via systemd-creds
    pub fn new() -> Result<Self> {
        info!("Initializing TPM 2.0 credential store");

        // Verify systemd-creds is available
        Self::check_systemd_creds_available().context("systemd-creds not available")?;

        // Verify TPM 2.0 is available
        if !Self::has_tpm2()? {
            return Err(anyhow!("TPM 2.0 not available"));
        }

        // Use systemd credential storage directory
        let storage_path = PathBuf::from("/var/lib/systemd/credentials");

        info!("TPM 2.0 credential store initialized");

        Ok(Self { storage_path })
    }

    /// Store a credential in TPM-bound storage
    ///
    /// # Arguments
    ///
    /// * `name` - Credential name (unique identifier)
    /// * `data` - Data to store (will be TPM-encrypted)
    ///
    /// # Returns
    ///
    /// Ok(()) if credential was stored successfully
    pub fn store(&self, name: &str, data: &[u8]) -> Result<()> {
        info!("Storing credential in TPM: {}", name);

        // Create a temporary file for the credential data
        let temp_dir = std::env::temp_dir();
        let temp_file = temp_dir.join(format!("lamco-cred-{}", name));

        // Write data to temp file
        std::fs::write(&temp_file, data).context("Failed to write temporary credential file")?;

        // Use systemd-creds to encrypt and store
        // Format: systemd-creds encrypt <input-file> <output-file> --with-key=tpm2 --name=<name>
        let output_file = self.storage_path.join(format!("{}.cred", name));

        let status = Command::new("systemd-creds")
            .arg("encrypt")
            .arg(&temp_file)
            .arg(&output_file)
            .arg("--with-key=tpm2")
            .arg(format!("--name={}", name))
            .status()
            .context("Failed to execute systemd-creds encrypt")?;

        // Clean up temp file
        std::fs::remove_file(&temp_file).ok();

        if !status.success() {
            return Err(anyhow!(
                "systemd-creds encrypt failed with status: {}",
                status
            ));
        }

        info!("Credential stored in TPM-bound storage: {:?}", output_file);

        Ok(())
    }

    /// Load a credential from TPM-bound storage
    ///
    /// # Arguments
    ///
    /// * `name` - Credential name
    ///
    /// # Returns
    ///
    /// Credential data if found
    pub fn load(&self, name: &str) -> Result<Vec<u8>> {
        debug!("Loading credential from TPM: {}", name);

        let input_file = self.storage_path.join(format!("{}.cred", name));

        if !input_file.exists() {
            return Err(anyhow!("Credential not found: {}", name));
        }

        // Create temp file for decrypted output
        let temp_dir = std::env::temp_dir();
        let temp_file = temp_dir.join(format!("lamco-cred-decrypt-{}", name));

        // Use systemd-creds to decrypt
        // Format: systemd-creds decrypt <input-file> <output-file>
        let status = Command::new("systemd-creds")
            .arg("decrypt")
            .arg(&input_file)
            .arg(&temp_file)
            .status()
            .context("Failed to execute systemd-creds decrypt")?;

        if !status.success() {
            // Clean up on failure
            std::fs::remove_file(&temp_file).ok();
            return Err(anyhow!(
                "systemd-creds decrypt failed with status: {}",
                status
            ));
        }

        // Read decrypted data
        let data = std::fs::read(&temp_file).context("Failed to read decrypted credential")?;

        // Clean up temp file
        std::fs::remove_file(&temp_file).context("Failed to remove temporary file")?;

        debug!("Credential loaded successfully ({} bytes)", data.len());

        Ok(data)
    }

    /// Delete a TPM-bound credential
    ///
    /// # Arguments
    ///
    /// * `name` - Credential name
    ///
    /// # Returns
    ///
    /// Ok(()) if credential was deleted or didn't exist
    pub fn delete(&self, name: &str) -> Result<()> {
        info!("Deleting TPM credential: {}", name);

        let cred_file = self.storage_path.join(format!("{}.cred", name));

        if cred_file.exists() {
            std::fs::remove_file(&cred_file).context("Failed to delete credential file")?;
            info!("TPM credential deleted successfully");
        } else {
            debug!("Credential not found (already deleted)");
        }

        Ok(())
    }

    /// Check if systemd-creds command is available
    fn check_systemd_creds_available() -> Result<()> {
        let output = Command::new("systemd-creds")
            .arg("--version")
            .output()
            .context("systemd-creds command not found")?;

        if !output.status.success() {
            return Err(anyhow!("systemd-creds command failed"));
        }

        debug!(
            "systemd-creds available: {}",
            String::from_utf8_lossy(&output.stdout).trim()
        );

        Ok(())
    }

    /// Check if TPM 2.0 is available
    fn has_tpm2() -> Result<bool> {
        debug!("Checking for TPM 2.0...");

        let output = Command::new("systemd-creds")
            .arg("has-tpm2")
            .output()
            .context("Failed to check TPM 2.0 availability")?;

        let has_tpm =
            output.status.success() && String::from_utf8_lossy(&output.stdout).trim() == "yes";

        debug!("TPM 2.0 available: {}", has_tpm);

        Ok(has_tpm)
    }
}

/// Async wrapper for TPM credential store
pub struct AsyncTpmCredentialStore {
    inner: TpmCredentialStore,
}

impl AsyncTpmCredentialStore {
    /// Create new TPM store (async)
    pub async fn new() -> Result<Self> {
        let inner = tokio::task::spawn_blocking(|| TpmCredentialStore::new())
            .await
            .context("Failed to spawn blocking task")?
            .context("Failed to create TPM store")?;

        Ok(Self { inner })
    }

    /// Store credential (async)
    pub async fn store(&self, name: String, data: Vec<u8>) -> Result<()> {
        let storage_path = self.inner.storage_path.clone();

        tokio::task::spawn_blocking(move || {
            let store = TpmCredentialStore { storage_path };
            store.store(&name, &data)
        })
        .await
        .context("Failed to spawn blocking task")?
        .context("Failed to store TPM credential")
    }

    /// Load credential (async)
    pub async fn load(&self, name: String) -> Result<Vec<u8>> {
        let storage_path = self.inner.storage_path.clone();

        tokio::task::spawn_blocking(move || {
            let store = TpmCredentialStore { storage_path };
            store.load(&name)
        })
        .await
        .context("Failed to spawn blocking task")?
        .context("Failed to load TPM credential")
    }

    /// Delete credential (async)
    pub async fn delete(&self, name: String) -> Result<()> {
        let storage_path = self.inner.storage_path.clone();

        tokio::task::spawn_blocking(move || {
            let store = TpmCredentialStore { storage_path };
            store.delete(&name)
        })
        .await
        .context("Failed to spawn blocking task")?
        .context("Failed to delete TPM credential")
    }
}

#[cfg(test)]
mod tpm_tests {
    use super::*;

    #[test]
    #[ignore] // Requires TPM 2.0 hardware and systemd-creds
    fn test_tpm_availability() {
        match TpmCredentialStore::new() {
            Ok(_) => println!("TPM 2.0 store available"),
            Err(e) => println!("TPM 2.0 not available: {}", e),
        }
    }

    #[test]
    #[ignore] // Requires TPM 2.0 hardware
    fn test_tpm_roundtrip() {
        let store = TpmCredentialStore::new().expect("TPM not available");

        let name = "test-tpm-cred";
        let data = b"test-secret-data-12345";

        // Store
        store.store(name, data).expect("Failed to store");

        // Load
        let loaded = store.load(name).expect("Failed to load");
        assert_eq!(loaded, data);

        // Delete
        store.delete(name).expect("Failed to delete");

        // Verify deleted
        assert!(store.load(name).is_err());
    }
}
