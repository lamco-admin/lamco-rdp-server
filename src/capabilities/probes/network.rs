//! Network capability probe
//!
//! Detects TLS and network capabilities.

use serde::{Deserialize, Serialize};
use tracing::{debug, info};

use crate::capabilities::state::ServiceLevel;

/// Network capabilities
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkCapabilities {
    /// TLS/SSL available
    pub tls_available: bool,
    /// Overall service level
    pub service_level: ServiceLevel,
}

/// Network probe
pub struct NetworkProbe;

impl NetworkProbe {
    pub async fn probe() -> NetworkCapabilities {
        info!("Probing network capabilities...");

        // TLS is always available (compiled in via rustls or openssl)
        let tls_available = true;
        debug!("TLS available: {}", tls_available);

        let service_level = if tls_available {
            ServiceLevel::Full
        } else {
            ServiceLevel::Unavailable
        };

        info!("Network service level: {:?}", service_level);

        NetworkCapabilities {
            tls_available,
            service_level,
        }
    }
}
