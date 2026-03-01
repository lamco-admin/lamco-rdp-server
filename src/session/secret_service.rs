//! Secret Service Backend (GNOME Keyring, KWallet, KeePassXC)
//!
//! Implements secure token storage using the org.freedesktop.secrets D-Bus API.
//! This works with GNOME Keyring, KDE Wallet, and KeePassXC.
//!
//! PRODUCTION-READY implementation using secret-service v5.x async API.

use std::collections::HashMap;

use anyhow::{anyhow, Context, Result};
use secret_service::{EncryptionType, SecretService};
use tracing::{debug, info};
use zeroize::Zeroizing;

/// Connect to Secret Service with DH encryption, falling back to Plain.
/// KWallet's Secret Service shim doesn't always support DH key exchange.
async fn connect_secret_service() -> Result<SecretService<'static>> {
    match SecretService::connect(EncryptionType::Dh).await {
        Ok(s) => Ok(s),
        Err(dh_err) => {
            debug!(
                "Secret Service DH connection failed: {}, trying Plain",
                dh_err
            );
            SecretService::connect(EncryptionType::Plain)
                .await
                .context("Failed to connect to Secret Service (tried DH and Plain)")
        }
    }
}

/// Get a usable (unlocked) collection. Tries default first, then falls back to
/// any unlocked collection. KWallet doesn't always expose a "default" collection
/// since it uses named wallets (e.g. "kdewallet").
async fn get_usable_collection<'a>(
    service: &'a SecretService<'a>,
) -> Result<secret_service::Collection<'a>> {
    // Try default collection first
    if let Ok(collection) = service.get_default_collection().await {
        if !collection.is_locked().await.unwrap_or(true) {
            return Ok(collection);
        }
        // Try unlocking default
        if collection.unlock().await.is_ok() && !collection.is_locked().await.unwrap_or(true) {
            return Ok(collection);
        }
    }

    // Fall back to first unlocked collection (covers KWallet's named wallets)
    if let Ok(collections) = service.get_all_collections().await {
        for collection in collections {
            if !collection.is_locked().await.unwrap_or(true) {
                debug!("Using non-default unlocked collection");
                return Ok(collection);
            }
        }
    }

    // Last resort: try to get default and unlock it
    let collection = service
        .get_default_collection()
        .await
        .context("No Secret Service collection available")?;

    collection
        .unlock()
        .await
        .context("Failed to unlock Secret Service collection")?;

    Ok(collection)
}

/// Async Secret Service client wrapper
///
/// Provides secure token storage via org.freedesktop.secrets D-Bus interface.
/// Compatible with GNOME Keyring, KWallet (via Secret Service), and KeePassXC.
pub struct AsyncSecretServiceClient {
    // Note: secret-service v5.x is already async with tokio
}

impl AsyncSecretServiceClient {
    pub async fn connect() -> Result<Self> {
        debug!("Connecting to Secret Service (org.freedesktop.secrets)...");

        let _service = connect_secret_service().await?;

        info!("Connected to Secret Service successfully");

        Ok(Self {})
    }

    pub async fn store_secret(
        &self,
        key: String,
        secret: String,
        attributes: Vec<(String, String)>,
    ) -> Result<()> {
        info!("Storing secret in Secret Service: {}", key);

        let service = connect_secret_service().await?;
        let collection = get_usable_collection(&service).await?;

        let mut attrs = HashMap::new();
        attrs.insert("application", "lamco-rdp-server");
        attrs.insert("key", key.as_str());

        for (k, v) in &attributes {
            attrs.insert(k.as_str(), v.as_str());
        }

        let label = format!("lamco-rdp-server: {key}");

        let secret_bytes = Zeroizing::new(secret.as_bytes().to_vec());

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

    pub async fn lookup_secret(&self, key: String) -> Result<String> {
        debug!("Looking up secret in Secret Service: {}", key);

        let service = connect_secret_service().await?;

        let mut search_attrs = HashMap::new();
        search_attrs.insert("application", "lamco-rdp-server");
        search_attrs.insert("key", key.as_str());

        let items = service
            .search_items(search_attrs)
            .await
            .context("Failed to search Secret Service")?;

        if items.unlocked.is_empty() && items.locked.is_empty() {
            return Err(anyhow!("Secret not found: {key}"));
        }

        if !items.unlocked.is_empty() {
            let item = &items.unlocked[0];
            let secret_bytes = item
                .get_secret()
                .await
                .context("Failed to retrieve secret value")?;

            let secret =
                String::from_utf8(secret_bytes.clone()).context("Secret contains invalid UTF-8")?;

            debug!("Secret retrieved successfully from unlocked item");
            return Ok(secret);
        }

        if !items.locked.is_empty() {
            let item = &items.locked[0];

            item.unlock()
                .await
                .context("Failed to unlock secret item")?;

            let secret_bytes = item
                .get_secret()
                .await
                .context("Failed to retrieve secret value after unlock")?;

            let secret =
                String::from_utf8(secret_bytes.clone()).context("Secret contains invalid UTF-8")?;

            debug!("Secret retrieved successfully after unlock");
            return Ok(secret);
        }

        Err(anyhow!("Secret not found: {key}"))
    }

    pub async fn delete_secret(&self, key: String) -> Result<()> {
        info!("Deleting secret from Secret Service: {}", key);

        let service = connect_secret_service().await?;

        let mut search_attrs = HashMap::new();
        search_attrs.insert("application", "lamco-rdp-server");
        search_attrs.insert("key", key.as_str());

        let items = service
            .search_items(search_attrs)
            .await
            .context("Failed to search Secret Service")?;

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
        Ok(h) => h.block_on(async { check_unlocked_async().await }),
        Err(_) => {
            // No runtime, can't check async
            false
        }
    }
}

/// Check if any Secret Service collection is unlocked.
/// Tries DH encryption first, falls back to Plain. KWallet's Secret Service
/// shim doesn't always support DH key exchange, and may not have a "default"
/// collection (KWallet uses named wallets like "kdewallet" instead).
async fn check_unlocked_async() -> bool {
    // Try DH first (more secure), fall back to Plain (needed for some KWallet versions)
    let service = match SecretService::connect(EncryptionType::Dh).await {
        Ok(s) => s,
        Err(_) => match SecretService::connect(EncryptionType::Plain).await {
            Ok(s) => {
                debug!("Secret Service: DH failed, connected with Plain encryption");
                s
            }
            Err(e) => {
                debug!("Secret Service connection failed: {}", e);
                return false;
            }
        },
    };

    // Try default collection first
    if let Ok(collection) = service.get_default_collection().await {
        if let Ok(locked) = collection.is_locked().await {
            if !locked {
                return true;
            }
        }
    }

    // KWallet may not expose a "default" collection. Check all collections
    // for at least one unlocked wallet.
    if let Ok(collections) = service.get_all_collections().await {
        for collection in &collections {
            if let Ok(locked) = collection.is_locked().await {
                if !locked {
                    debug!("Secret Service: found unlocked collection (non-default)");
                    return true;
                }
            }
        }
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[ignore = "Requires Secret Service running"]
    async fn test_secret_service_connection() {
        let result = AsyncSecretServiceClient::connect().await;
        match result {
            Ok(_client) => {
                println!("Secret Service connected successfully");
            }
            Err(e) => {
                println!("Secret Service not available: {e}");
            }
        }
    }

    #[tokio::test]
    #[ignore = "Requires Secret Service running"]
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
