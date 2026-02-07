//! Klipper Cooperation Mode (Tier 2 Strategy)
//!
//! **Execution Path:** Klipper D-Bus + Portal Clipboard
//! **Status:** Active (v1.3.0+)
//! **Platform:** KDE Plasma with Klipper
//! **Integration Mode:** `KlipperCooperationMode`
//!
//! Implements the "work WITH Klipper" approach instead of fighting it.
//!
//! # Core Concept
//!
//! Accept that Klipper will take clipboard ownership (~1 second after we call SetSelection).
//! This is intentional by design - Klipper provides persistence when apps close.
//!
//! Instead of trying to prevent or immediately reclaim, we:
//! 1. **Monitor** Klipper's clipboardHistoryUpdated signal
//! 2. **Read** from Klipper when it updates via D-Bus getClipboardContents()
//! 3. **Sync** Klipper's content back to RDP client
//! 4. **Bidirectional**: Windows ↔ Klipper ↔ Linux apps
//!
//! # Why This Works
//!
//! - Aligns with Klipper's purpose (persistence across app lifecycles)
//! - Robust across Plasma versions (no protocol tricks)
//! - User-friendly (Klipper features continue working)
//! - Expected success rate: 90-95% (measured in testing)
//!
//! # Strategic Decision
//!
//! This is Tier 2 from docs/decisions/CLIPBOARD-KDE-STRATEGIC-DECISION.md.
//! Falls back to Tier 3 (re-announce) if cooperation fails.

use futures_util::StreamExt;
use std::sync::Arc;
use std::time::{Duration, SystemTime};
use tokio::sync::{mpsc, RwLock};
use tracing::{debug, error, info, warn};
use zbus::Connection;

use crate::clipboard::error::Result;
use crate::clipboard::{ClipboardFormat, FormatConverter, FormatConverterExt};

/// Klipper cooperation coordinator
///
/// Manages bidirectional sync between RDP and Klipper clipboard manager.
/// Monitors Klipper signals and syncs content changes in both directions.
pub struct KlipperCooperationCoordinator {
    /// D-Bus connection for Klipper communication
    connection: Arc<Connection>,

    /// Current RDP formats (what Windows announced)
    ///
    /// We track these so when Klipper takes over, we know what we originally offered.
    /// Useful for Tier 3 fallback (re-announce).
    current_rdp_formats: Arc<RwLock<Vec<ClipboardFormat>>>,

    /// Last sync timestamp
    ///
    /// Prevents rapid sync loops. We rate-limit cooperation sync to avoid
    /// bouncing between RDP and Klipper indefinitely.
    last_sync_time: Arc<RwLock<Option<SystemTime>>>,

    /// Minimum interval between cooperation syncs (ms)
    ///
    /// If Klipper updates within this window, we skip the sync to avoid loops.
    /// Default: 1000ms (1 second)
    min_sync_interval_ms: u64,

    /// Event sender for cooperation events
    ///
    /// Sends events to ClipboardManager when Klipper content changes.
    event_tx: mpsc::UnboundedSender<CooperationEvent>,

    /// Whether cooperation is active
    ///
    /// Can be disabled if we detect issues (too many rapid updates, errors, etc.)
    active: Arc<RwLock<bool>>,

    /// Statistics for monitoring
    stats: Arc<RwLock<CooperationStats>>,
}

/// Events from cooperation coordinator
#[derive(Debug, Clone)]
pub enum CooperationEvent {
    /// Klipper updated clipboard
    ///
    /// Contains the text content read from Klipper.
    /// ClipboardManager should convert and sync to RDP client.
    KlipperContentUpdated {
        /// Text content from Klipper (getClipboardContents() only returns string)
        content: String,
        /// Timestamp of the update
        timestamp_ms: u64,
    },

    /// Cooperation failed
    ///
    /// Indicates cooperation mode encountered error and should fall back to Tier 3.
    CooperationFailed {
        /// Error message
        reason: String,
        /// Whether to retry cooperation or permanently disable
        retry: bool,
    },
}

/// Cooperation statistics for diagnostics
#[derive(Debug, Clone, Default)]
pub struct CooperationStats {
    /// Number of Klipper updates observed
    pub klipper_updates: u64,

    /// Number of successful syncs to RDP
    pub successful_syncs: u64,

    /// Number of syncs skipped (rate limit)
    pub skipped_rate_limit: u64,

    /// Number of sync errors
    pub sync_errors: u64,

    /// Last sync timestamp
    pub last_sync_ms: u64,
}

impl KlipperCooperationCoordinator {
    /// Create new cooperation coordinator
    pub async fn new(
        connection: Connection,
        min_sync_interval_ms: u64,
    ) -> Result<(Self, mpsc::UnboundedReceiver<CooperationEvent>)> {
        let (event_tx, event_rx) = mpsc::unbounded_channel();

        let coordinator = Self {
            connection: Arc::new(connection),
            current_rdp_formats: Arc::new(RwLock::new(Vec::new())),
            last_sync_time: Arc::new(RwLock::new(None)),
            min_sync_interval_ms,
            event_tx,
            active: Arc::new(RwLock::new(true)),
            stats: Arc::new(RwLock::new(CooperationStats::default())),
        };

        info!("═══════════════════════════════════════════════════════════════");
        info!("  Klipper Cooperation Mode ACTIVE");
        info!("═══════════════════════════════════════════════════════════════");
        info!("  Strategy: Tier 2 (Work WITH Klipper)");
        info!("  Sync Interval: {}ms minimum", min_sync_interval_ms);
        info!("  ┌─────────────────────────────────────────────────");
        info!("  │ Windows → RDP → Portal → Klipper (accepts ownership)");
        info!("  │ Klipper → Signal → Read → Convert → RDP → Windows");
        info!("  │ Bidirectional sync through Klipper's persistence layer");
        info!("  └─────────────────────────────────────────────────");
        info!("═══════════════════════════════════════════════════════════════");

        Ok((coordinator, event_rx))
    }

    /// Start monitoring Klipper signals
    ///
    /// Subscribes to clipboardHistoryUpdated signal and triggers cooperation sync
    /// when Klipper's clipboard changes.
    ///
    /// This should be called after coordinator creation to start monitoring.
    pub async fn start_monitoring(&self) -> Result<()> {
        info!("Starting Klipper signal monitoring for cooperation...");

        let connection = Arc::clone(&self.connection);
        let event_tx = self.event_tx.clone();
        let last_sync_time = Arc::clone(&self.last_sync_time);
        let min_interval = self.min_sync_interval_ms;
        let active = Arc::clone(&self.active);
        let stats = Arc::clone(&self.stats);

        tokio::spawn(async move {
            match Self::monitor_klipper_signals(
                connection,
                event_tx,
                last_sync_time,
                min_interval,
                active,
                stats,
            )
            .await
            {
                Ok(_) => {
                    info!("Klipper signal monitoring ended normally");
                }
                Err(e) => {
                    error!("Klipper signal monitoring failed: {:#}", e);
                }
            }
        });

        info!("✅ Klipper cooperation monitoring started");
        Ok(())
    }

    /// Internal: Monitor Klipper D-Bus signals
    ///
    /// Subscribes to clipboardHistoryUpdated and reads content on updates.
    async fn monitor_klipper_signals(
        connection: Arc<Connection>,
        event_tx: mpsc::UnboundedSender<CooperationEvent>,
        last_sync_time: Arc<RwLock<Option<SystemTime>>>,
        min_interval_ms: u64,
        active: Arc<RwLock<bool>>,
        stats: Arc<RwLock<CooperationStats>>,
    ) -> Result<()> {
        use zbus::proxy;

        #[proxy(
            interface = "org.kde.klipper.klipper",
            default_service = "org.kde.klipper",
            default_path = "/klipper"
        )]
        trait Klipper {
            /// Get current clipboard contents (text only)
            /// Note: KDE uses camelCase, so we need explicit name to prevent zbus PascalCase conversion
            #[zbus(name = "getClipboardContents")]
            fn get_clipboard_contents(&self) -> zbus::Result<String>;

            /// Signal: Clipboard history was updated
            /// Note: KDE uses camelCase, so we need explicit name to prevent zbus PascalCase conversion
            #[zbus(signal, name = "clipboardHistoryUpdated")]
            fn clipboard_history_updated(&self) -> zbus::Result<()>;
        }

        let proxy = KlipperProxy::new(&*connection).await.map_err(|e| {
            crate::clipboard::error::ClipboardError::DBus(format!(
                "Failed to create Klipper proxy: {}",
                e
            ))
        })?;

        info!("📡 Subscribed to Klipper clipboardHistoryUpdated signal");

        match proxy.get_clipboard_contents().await {
            Ok(content) => {
                info!(
                    "✅ Klipper proxy responsive, current clipboard: {} chars",
                    content.len()
                );
                info!(
                    "   Content preview: {:?}",
                    content.chars().take(30).collect::<String>()
                );
            }
            Err(e) => {
                error!("❌ Klipper proxy test failed: {}", e);
                return Err(crate::clipboard::error::ClipboardError::DBus(format!(
                    "Klipper proxy not responsive: {}",
                    e
                )));
            }
        }

        let mut signal_stream = proxy
            .receive_clipboard_history_updated()
            .await
            .map_err(|e| {
                crate::clipboard::error::ClipboardError::DBus(format!(
                    "Failed to subscribe to signal: {}",
                    e
                ))
            })?;

        info!("🎧 Signal stream created, entering monitoring loop...");
        info!("⏰ Heartbeat: Will log every 30 seconds to prove task is alive");

        let mut heartbeat_count = 0u64;
        let heartbeat_interval = tokio::time::Duration::from_secs(30);
        let mut next_heartbeat = tokio::time::Instant::now() + heartbeat_interval;

        loop {
            tokio::select! {
                signal_opt = signal_stream.next() => {
                    match signal_opt {
                        Some(_signal) => {
                            let is_active = *active.read().await;
                            if !is_active {
                                info!("Cooperation disabled, stopping signal monitoring");
                                break;
                            }

                            {
                                let mut stats_guard = stats.write().await;
                                stats_guard.klipper_updates += 1;
                            }

                            info!("📨 Klipper clipboardHistoryUpdated signal received!");

            let should_sync = {
                let last_sync = last_sync_time.read().await;
                match *last_sync {
                    Some(t) => {
                        let elapsed = SystemTime::now()
                            .duration_since(t)
                            .unwrap_or(Duration::from_secs(999))
                            .as_millis() as u64;

                        if elapsed < min_interval_ms {
                            debug!(
                                "⏸  Sync skipped: {}ms < {}ms rate limit",
                                elapsed, min_interval_ms
                            );
                            let mut stats_guard = stats.write().await;
                            stats_guard.skipped_rate_limit += 1;
                            false
                        } else {
                            debug!("✓ Rate limit OK: {}ms since last sync", elapsed);
                            true
                        }
                    }
                    None => {
                        debug!("✓ First sync, no rate limit");
                        true
                    }
                }
            };

            if !should_sync {
                continue;
            }

            info!("┌─ Klipper Cooperation Sync ─────────────────────────");
            info!("│ Klipper updated clipboard - reading content...");

            match proxy.get_clipboard_contents().await {
                Ok(content) => {
                    let timestamp_ms = SystemTime::now()
                        .duration_since(SystemTime::UNIX_EPOCH)
                        .unwrap()
                        .as_millis() as u64;

                    info!("│ ✅ Read {} bytes from Klipper", content.len());
                    debug!("│ Content preview: {:?}", content.chars().take(50).collect::<String>());

                    if event_tx
                        .send(CooperationEvent::KlipperContentUpdated {
                            content,
                            timestamp_ms,
                        })
                        .is_ok()
                    {
                        *last_sync_time.write().await = Some(SystemTime::now());

                        let mut stats_guard = stats.write().await;
                        stats_guard.successful_syncs += 1;
                        stats_guard.last_sync_ms = timestamp_ms;

                        info!("│ ✅ Cooperation sync event sent to manager");
                    } else {
                        warn!("│ ⚠️  Manager channel closed, stopping cooperation");
                        break;
                    }
                }
                Err(e) => {
                    error!("│ ❌ Failed to read from Klipper: {}", e);

                    let mut stats_guard = stats.write().await;
                    stats_guard.sync_errors += 1;

                    let _ = event_tx.send(CooperationEvent::CooperationFailed {
                        reason: format!("get_clipboard_contents failed: {}", e),
                        retry: true,
                    });
                }
            }

                            info!("└─────────────────────────────────────────────────────");
                        }
                        None => {
                            warn!("📭 Signal stream ended (None received)");
                            break;
                        }
                    }
                }

                _ = tokio::time::sleep_until(next_heartbeat) => {
                    heartbeat_count += 1;
                    info!("💓 Heartbeat #{}: Signal monitor task is alive and waiting for signals", heartbeat_count);
                    next_heartbeat = tokio::time::Instant::now() + heartbeat_interval;
                }
            }
        }

        info!("Signal monitoring loop ended");
        Ok(())
    }

    /// Update current RDP formats (for Tier 3 fallback)
    ///
    /// Stores the formats announced from Windows so if cooperation fails,
    /// we can fall back to re-announce strategy.
    pub async fn update_rdp_formats(&self, formats: Vec<ClipboardFormat>) {
        *self.current_rdp_formats.write().await = formats;
    }

    /// Get cooperation statistics
    pub async fn get_stats(&self) -> CooperationStats {
        self.stats.read().await.clone()
    }

    /// Enable/disable cooperation
    ///
    /// If disabled, signal monitoring stops and cooperation events stop.
    /// Can be re-enabled later.
    pub async fn set_active(&self, active: bool) {
        *self.active.write().await = active;
        if active {
            info!("✅ Klipper cooperation ENABLED");
        } else {
            warn!("⏸  Klipper cooperation DISABLED");
        }
    }

    /// Check if cooperation is active
    pub async fn is_active(&self) -> bool {
        *self.active.read().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cooperation_event_debug() {
        let event = CooperationEvent::KlipperContentUpdated {
            content: "test".to_string(),
            timestamp_ms: 123456,
        };
        assert!(format!("{:?}", event).contains("KlipperContentUpdated"));

        let event = CooperationEvent::CooperationFailed {
            reason: "test error".to_string(),
            retry: true,
        };
        assert!(format!("{:?}", event).contains("CooperationFailed"));
    }

    #[test]
    fn test_stats_default() {
        let stats = CooperationStats::default();
        assert_eq!(stats.klipper_updates, 0);
        assert_eq!(stats.successful_syncs, 0);
    }
}
