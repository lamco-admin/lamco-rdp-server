//! TPM 2.0 Credential Storage via systemd-creds
//!
//! Implements secure token storage using TPM 2.0 hardware via systemd-creds.
//! Credentials are bound to the TPM and cannot be extracted or used on other machines.

use std::{path::PathBuf, process::Command};

use anyhow::{anyhow, Context, Result};
use tracing::{debug, info};

/// TPM 2.0 credential store using systemd-creds
pub struct TpmCredentialStore {
    storage_path: PathBuf,
}

impl TpmCredentialStore {
    pub fn new() -> Result<Self> {
        info!("Initializing TPM 2.0 credential store");

        Self::check_systemd_creds_available().context("systemd-creds not available")?;

        if !Self::has_tpm2()? {
            return Err(anyhow!("TPM 2.0 not available"));
        }

        let storage_path = PathBuf::from("/var/lib/systemd/credentials");

        info!("TPM 2.0 credential store initialized");

        Ok(Self { storage_path })
    }

    pub fn store(&self, name: &str, data: &[u8]) -> Result<()> {
        info!("Storing credential in TPM: {}", name);

        let temp_dir = std::env::temp_dir();
        let temp_file = temp_dir.join(format!("lamco-cred-{name}"));

        std::fs::write(&temp_file, data).context("Failed to write temporary credential file")?;

        let output_file = self.storage_path.join(format!("{name}.cred"));

        let status = Command::new("systemd-creds")
            .arg("encrypt")
            .arg(&temp_file)
            .arg(&output_file)
            .arg("--with-key=tpm2")
            .arg(format!("--name={name}"))
            .status()
            .context("Failed to execute systemd-creds encrypt")?;

        std::fs::remove_file(&temp_file).ok();

        if !status.success() {
            return Err(anyhow!(
                "systemd-creds encrypt failed with status: {status}"
            ));
        }

        info!("Credential stored in TPM-bound storage: {:?}", output_file);

        Ok(())
    }

    pub fn load(&self, name: &str) -> Result<Vec<u8>> {
        debug!("Loading credential from TPM: {}", name);

        let input_file = self.storage_path.join(format!("{name}.cred"));

        if !input_file.exists() {
            return Err(anyhow!("Credential not found: {name}"));
        }

        let temp_dir = std::env::temp_dir();
        let temp_file = temp_dir.join(format!("lamco-cred-decrypt-{name}"));

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
                "systemd-creds decrypt failed with status: {status}"
            ));
        }

        let data = std::fs::read(&temp_file).context("Failed to read decrypted credential")?;

        std::fs::remove_file(&temp_file).context("Failed to remove temporary file")?;

        debug!("Credential loaded successfully ({} bytes)", data.len());

        Ok(data)
    }

    pub fn delete(&self, name: &str) -> Result<()> {
        info!("Deleting TPM credential: {}", name);

        let cred_file = self.storage_path.join(format!("{name}.cred"));

        if cred_file.exists() {
            std::fs::remove_file(&cred_file).context("Failed to delete credential file")?;
            info!("TPM credential deleted successfully");
        } else {
            debug!("Credential not found (already deleted)");
        }

        Ok(())
    }

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
    pub async fn new() -> Result<Self> {
        let inner = tokio::task::spawn_blocking(TpmCredentialStore::new)
            .await
            .context("Failed to spawn blocking task")?
            .context("Failed to create TPM store")?;

        Ok(Self { inner })
    }

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
