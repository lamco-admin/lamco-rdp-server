//! Secret Service Backend (GNOME Keyring, KWallet, KeePassXC)
//!
//! Implements secure token storage using the org.freedesktop.secrets D-Bus API.
//! This works with GNOME Keyring, KDE Wallet, and KeePassXC.
//!
//! PRODUCTION-READY implementation using secret-service v5.x async API.

use anyhow::{anyhow, Context, Result};
use secret_service::{EncryptionType, SecretService};
use std::collections::HashMap;
use tracing::{debug, info, warn};
use zeroize::Zeroizing;

/// Async Secret Service client wrapper
///
/// Provides secure token storage via org.freedesktop.secrets D-Bus interface.
/// Compatible with GNOME Keyring, KWallet (via Secret Service), and KeePassXC.
pub struct AsyncSecretServiceClient {
    // Note: secret-service v5.x is already async with tokio
}

impl AsyncSecretServiceClient {
    /// Connect to Secret Service
    ///
    /// This connects to org.freedesktop.secrets on the session D-Bus.
    /// Works with GNOME Keyring, KWallet, KeePassXC, and other compatible backends.
    pub async fn connect() -> Result<Self> {
        debug!("Connecting to Secret Service (org.freedesktop.secrets)...");

        // Test connection by attempting to connect
        // Use Dh (Diffie-Hellman) for secure session encryption
        let _service = SecretService::connect(EncryptionType::Dh)
            .await
            .context("Failed to connect to Secret Service")?;

        info!("Connected to Secret Service successfully");

        Ok(Self {})
    }

    /// Store a secret
    ///
    /// # Arguments
    ///
    /// * `key` - Unique identifier for this secret
    /// * `secret` - The secret value to store (will be zeroized after use)
    /// * `attributes` - Key-value attributes for searching/filtering
    ///
    /// # Returns
    ///
    /// Ok(()) if secret was stored successfully
    pub async fn store_secret(
        &self,
        key: String,
        secret: String,
        attributes: Vec<(String, String)>,
    ) -> Result<()> {
        info!("Storing secret in Secret Service: {}", key);

        // Connect to service
        let service = SecretService::connect(EncryptionType::Dh)
            .await
            .context("Failed to connect to Secret Service")?;

        // Get the default collection (usually "login" keyring)
        let collection = service
            .get_default_collection()
            .await
            .context("Failed to get default Secret Service collection")?;

        // Check if collection is locked
        if collection.is_locked().await.unwrap_or(true) {
            warn!("Secret Service collection is locked, attempting unlock");

            // Try to unlock
            collection
                .unlock()
                .await
                .context("Failed to unlock Secret Service collection")?;

            // Verify unlock
            if collection.is_locked().await.unwrap_or(true) {
                return Err(anyhow!("Collection remains locked after unlock attempt"));
            }

            info!("Secret Service collection unlocked successfully");
        }

        // Build attributes map
        let mut attrs = HashMap::new();
        attrs.insert("application", "lamco-rdp-server");
        attrs.insert("key", key.as_str());

        for (k, v) in &attributes {
            attrs.insert(k.as_str(), v.as_str());
        }

        // Create label for the secret
        let label = format!("lamco-rdp-server: {}", key);

        // Convert secret to bytes (zeroize after use)
        let secret_bytes = Zeroizing::new(secret.as_bytes().to_vec());

        // Store secret (Secret Service handles encryption automatically)
        collection
            .create_item(
                &label,
                attrs,
                secret_bytes.as_ref(),
                true, // Replace if exists
                "text/plain",
            )
            .await
            .context("Failed to create Secret Service item")?;

        info!("Secret stored successfully in Secret Service");

        Ok(())
    }

    /// Retrieve a secret
    ///
    /// # Arguments
    ///
    /// * `key` - Unique identifier for the secret
    ///
    /// # Returns
    ///
    /// The secret value if found
    pub async fn lookup_secret(&self, key: String) -> Result<String> {
        debug!("Looking up secret in Secret Service: {}", key);

        // Connect to service
        let service = SecretService::connect(EncryptionType::Dh)
            .await
            .context("Failed to connect to Secret Service")?;

        // Search for item by attributes
        let mut search_attrs = HashMap::new();
        search_attrs.insert("application", "lamco-rdp-server");
        search_attrs.insert("key", key.as_str());

        let items = service
            .search_items(search_attrs)
            .await
            .context("Failed to search Secret Service")?;

        if items.unlocked.is_empty() && items.locked.is_empty() {
            return Err(anyhow!("Secret not found: {}", key));
        }

        // Try unlocked items first
        if !items.unlocked.is_empty() {
            let item = &items.unlocked[0];
            let secret_bytes = item
                .get_secret()
                .await
                .context("Failed to retrieve secret value")?;

            let secret = String::from_utf8(secret_bytes.to_vec())
                .context("Secret contains invalid UTF-8")?;

            debug!("Secret retrieved successfully from unlocked item");
            return Ok(secret);
        }

        // Try locked items (will require unlock)
        if !items.locked.is_empty() {
            let item = &items.locked[0];

            // Try to unlock
            item.unlock()
                .await
                .context("Failed to unlock secret item")?;

            let secret_bytes = item
                .get_secret()
                .await
                .context("Failed to retrieve secret value after unlock")?;

            let secret = String::from_utf8(secret_bytes.to_vec())
                .context("Secret contains invalid UTF-8")?;

            debug!("Secret retrieved successfully after unlock");
            return Ok(secret);
        }

        Err(anyhow!("Secret not found: {}", key))
    }

    /// Delete a secret
    ///
    /// # Arguments
    ///
    /// * `key` - Unique identifier for the secret
    ///
    /// # Returns
    ///
    /// Ok(()) if secret was deleted or didn't exist
    pub async fn delete_secret(&self, key: String) -> Result<()> {
        info!("Deleting secret from Secret Service: {}", key);

        // Connect to service
        let service = SecretService::connect(EncryptionType::Dh)
            .await
            .context("Failed to connect to Secret Service")?;

        // Search for item
        let mut search_attrs = HashMap::new();
        search_attrs.insert("application", "lamco-rdp-server");
        search_attrs.insert("key", key.as_str());

        let items = service
            .search_items(search_attrs)
            .await
            .context("Failed to search Secret Service")?;

        // Delete all matching items (unlocked and locked)
        for item in items.unlocked.iter().chain(items.locked.iter()) {
            item.delete()
                .await
                .context("Failed to delete secret item")?;
        }

        if !items.unlocked.is_empty() || !items.locked.is_empty() {
            info!("Secret deleted successfully");
        } else {
            debug!("Secret not found (already deleted)");
        }

        Ok(())
    }
}

/// Check if Secret Service is available and unlocked (blocking version for sync contexts)
pub fn check_secret_service_unlocked() -> bool {
    // This is called from blocking context, use runtime handle
    let handle = tokio::runtime::Handle::try_current();

    match handle {
        Ok(h) => h.block_on(async {
            match SecretService::connect(EncryptionType::Dh).await {
                Ok(service) => match service.get_default_collection().await {
                    Ok(collection) => !collection.is_locked().await.unwrap_or(true),
                    Err(_) => false,
                },
                Err(_) => false,
            }
        }),
        Err(_) => {
            // No runtime, can't check async
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[ignore] // Requires Secret Service running
    async fn test_secret_service_connection() {
        let result = AsyncSecretServiceClient::connect().await;
        match result {
            Ok(_client) => {
                println!("Secret Service connected successfully");
            }
            Err(e) => {
                println!("Secret Service not available: {}", e);
            }
        }
    }

    #[tokio::test]
    #[ignore] // Requires Secret Service running
    async fn test_secret_roundtrip() {
        let client = AsyncSecretServiceClient::connect()
            .await
            .expect("Secret Service not available");

        let key = "test-token-roundtrip".to_string();
        let secret = "test-secret-value-12345".to_string();

        // Store
        client
            .store_secret(
                key.clone(),
                secret.clone(),
                vec![("test".to_string(), "true".to_string())],
            )
            .await
            .expect("Failed to store");

        // Retrieve
        let retrieved = client
            .lookup_secret(key.clone())
            .await
            .expect("Failed to retrieve");
        assert_eq!(retrieved, secret);

        // Delete
        client.delete_secret(key).await.expect("Failed to delete");
    }
}
