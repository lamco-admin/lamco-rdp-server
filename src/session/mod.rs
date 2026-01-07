//! Session Persistence & Unattended Access
//!
//! This module implements multi-strategy session persistence to enable
//! unattended operation across different desktop environments.
//!
//! # Overview
//!
//! Session persistence solves the fundamental challenge of Wayland's security model:
//! explicit user consent for screen capture. Without persistence, every server
//! restart requires manual permission dialog interaction.
//!
//! # Architecture
//!
//! ```text
//! SessionStrategySelector
//!   ├─> Portal + Token Strategy (universal, portal v4+)
//!   ├─> Mutter Direct API (GNOME only, no dialog)
//!   └─> wlr-screencopy (wlroots only, no dialog)
//!
//! TokenManager
//!   ├─> Flatpak Secret Portal (Flatpak deployment)
//!   ├─> TPM 2.0 (systemd + TPM hardware)
//!   ├─> Secret Service (GNOME Keyring, KWallet, KeePassXC)
//!   └─> Encrypted File (universal fallback)
//! ```
//!
//! # Usage
//!
//! ```rust,no_run
//! use wrd_server::session::*;
//!
//! // Detect deployment and select strategy
//! let deployment = detect_deployment_context();
//! let (storage_method, _, _) = detect_credential_storage(&deployment).await?;
//!
//! // Create token manager
//! let token_manager = TokenManager::new(storage_method).await?;
//!
//! // Try to load existing token
//! let restore_token = token_manager.load_token("default").await?;
//!
//! // Use token in portal config
//! let portal_config = PortalConfig {
//!     persist_mode: PersistMode::ExplicitlyRevoked,
//!     restore_token,
//!     ..Default::default()
//! };
//!
//! // After session start, save new token
//! if let Some(new_token) = session_result_token {
//!     token_manager.save_token("default", &new_token).await?;
//! }
//! ```
//!
//! # Deployment Constraints
//!
//! Different deployment methods constrain available strategies:
//!
//! - **Flatpak:** Portal + Token ONLY (sandboxing blocks direct access)
//! - **Native:** All strategies available
//! - **systemd user:** All strategies available
//! - **systemd system:** Portal + Token only (D-Bus session complexity)
//!
//! See: docs/architecture/SESSION-PERSISTENCE-ARCHITECTURE.md

pub mod credentials;
pub mod flatpak_secret;
pub mod secret_service;
pub mod strategy;
pub mod token_manager;
pub mod tpm_store;

// Strategy implementations (Phase 3)
pub mod strategies {
    pub mod mutter_direct;
    pub mod portal_token;
    pub mod selector;

    pub use mutter_direct::MutterDirectStrategy;
    pub use portal_token::{PortalSessionHandleImpl, PortalTokenStrategy};
    pub use selector::SessionStrategySelector;
}

// Re-exports for convenience
pub use credentials::{
    detect_credential_storage, detect_deployment_context, CredentialStorageMethod,
    DeploymentContext, EncryptionType,
};
pub use flatpak_secret::FlatpakSecretManager;
pub use secret_service::AsyncSecretServiceClient;
pub use strategies::SessionStrategySelector;
pub use strategy::{PipeWireAccess, SessionConfig, SessionHandle, SessionStrategy, SessionType};
pub use token_manager::TokenManager;
pub use tpm_store::AsyncTpmCredentialStore;
