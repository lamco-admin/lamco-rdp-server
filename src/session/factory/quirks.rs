//! Initialization Quirks
//!
//! Platform and deployment-specific quirks that affect HOW services are initialized,
//! distinct from the Service Registry which tracks WHAT is available.
//!
//! # Design Rationale
//!
//! The Service Registry answers "Is clipboard available?" (ServiceLevel::Guaranteed).
//! But it doesn't answer "How should clipboard be initialized in Flatpak when
//! retrying after persistence rejection?"
//!
//! InitQuirks fill this gap by capturing initialization behaviors that vary by
//! deployment context, compositor, and session strategy.
//!
//! See: docs/analysis/CLIPBOARD-FLATPAK-ANALYSIS-20260128.md

use crate::{compositor::CompositorType, session::DeploymentContext};

/// Initialization quirks that affect session creation behavior
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum InitQuirk {
    /// Don't create multiple Clipboard D-Bus proxies in the same process.
    ///
    /// In Flatpak, creating multiple Clipboard proxies causes "Invalid state"
    /// errors because the Portal backend or xdg-dbus-proxy gets confused.
    ///
    /// When retrying after failure, reuse the existing ClipboardManager instead
    /// of creating a new one.
    ///
    /// Affected: Flatpak deployments
    SingleClipboardProxy,

    /// Session Close() is asynchronous - may need delay before retry.
    ///
    /// When a session fails and we retry, the previous session's Close() is
    /// sent asynchronously. In some cases, the retry may start before the
    /// previous session is fully cleaned up.
    ///
    /// Affected: All deployments using ashpd (Drop impl spawns async Close)
    AsyncSessionCleanup,

    /// GNOME Portal rejects persistence for RemoteDesktop sessions.
    ///
    /// GNOME's xdg-desktop-portal-gnome returns:
    /// "org.freedesktop.portal.Error.InvalidArgument: Remote desktop sessions cannot persist"
    ///
    /// When this quirk is present, the factory should:
    /// 1. Try with persistence first (might work on future versions)
    /// 2. On InvalidArgument, retry with PersistMode::DoNot
    ///
    /// Affected: GNOME compositor
    GnomePersistenceRejected,

    /// Clipboard manager must be created BEFORE first create_session() call.
    ///
    /// The XDG Portal spec requires that clipboard be requested during session
    /// setup (after SelectDevices/SelectSources, before Start). This means the
    /// ClipboardManager must exist before create_session() is called.
    ///
    /// Affected: All Portal-based strategies
    ClipboardBeforeSession,

    /// Mutter Direct API sessions can't be linked (screencast + remote desktop).
    ///
    /// The Mutter D-Bus API has separate RemoteDesktop and ScreenCast interfaces
    /// but they can't share a session. RemoteDesktop.CreateSession() takes no
    /// arguments, so we can't pass a session-id from ScreenCast.
    ///
    /// This makes the hybrid Mutter strategy non-functional.
    ///
    /// Affected: GNOME with Mutter Direct API strategy (dormant)
    MutterSessionLinkageBroken,

    /// Portal v4+ restore tokens require secure storage.
    ///
    /// Restore tokens allow avoiding permission dialogs on reconnect, but they
    /// should be stored securely. Without secure storage, tokens are stored in
    /// encrypted files with derived keys.
    ///
    /// Affected: All Portal-based strategies
    RequiresSecureTokenStorage,

    /// wlr-screencopy blocked by Flatpak sandbox.
    ///
    /// The wlr-screencopy protocol requires direct Wayland socket access,
    /// which is blocked by Flatpak's sandbox.
    ///
    /// Affected: Flatpak deployments on wlroots compositors
    WlrScreencopyBlockedBySandbox,

    /// PAM authentication blocked by Flatpak sandbox.
    ///
    /// PAM requires access to /etc/pam.d/ and PAM libraries, which are
    /// inaccessible from Flatpak sandbox.
    ///
    /// Affected: Flatpak deployments
    PamBlockedBySandbox,

    /// KDE Portal Clipboard has threading bugs (v6.3.90-6.5.x).
    ///
    /// xdg-desktop-portal-kde versions 6.3.90 through 6.5.x have critical
    /// threading violations in the clipboard implementation:
    ///
    /// 1. Crash: QMimeData accessed from Wayland thread (SIGSEGV)
    /// 2. Signal: SelectionTransfer emitted from wrong thread (D-Bus blocks)
    ///
    /// Result: Portal crashes when Klipper reads clipboard, and Windows→Linux
    /// clipboard fails (timeout). Makes Portal Clipboard completely unusable.
    ///
    /// Fixed in v6.6+ (KDE Bug 515465). NOT backported to 6.5.x branch.
    ///
    /// When this quirk is present, clipboard should be disabled entirely.
    ///
    /// Affected: KDE Plasma 6.3.90-6.5.x (May 2025 - February 2026)
    KdePortalClipboardThreadingBug,
}

impl InitQuirk {
    pub fn name(&self) -> &'static str {
        match self {
            Self::SingleClipboardProxy => "Single Clipboard Proxy",
            Self::AsyncSessionCleanup => "Async Session Cleanup",
            Self::GnomePersistenceRejected => "GNOME Persistence Rejected",
            Self::ClipboardBeforeSession => "Clipboard Before Session",
            Self::MutterSessionLinkageBroken => "Mutter Session Linkage Broken",
            Self::RequiresSecureTokenStorage => "Requires Secure Token Storage",
            Self::WlrScreencopyBlockedBySandbox => "wlr-screencopy Blocked",
            Self::PamBlockedBySandbox => "PAM Blocked",
            Self::KdePortalClipboardThreadingBug => "KDE Portal Clipboard Bug 515465",
        }
    }

    pub fn description(&self) -> &'static str {
        match self {
            Self::SingleClipboardProxy => "Don't create multiple Clipboard D-Bus proxies",
            Self::AsyncSessionCleanup => "Session Close() is asynchronous",
            Self::GnomePersistenceRejected => "GNOME rejects RemoteDesktop persistence",
            Self::ClipboardBeforeSession => "Create clipboard before session creation",
            Self::MutterSessionLinkageBroken => "Mutter API can't link sessions",
            Self::RequiresSecureTokenStorage => "Store restore tokens securely",
            Self::WlrScreencopyBlockedBySandbox => "Flatpak blocks wlr-screencopy",
            Self::PamBlockedBySandbox => "Flatpak blocks PAM authentication",
            Self::KdePortalClipboardThreadingBug => {
                "Portal Clipboard crashes (threading bugs, fixed in 6.6+)"
            }
        }
    }
}

/// Session strategy type for quirk selection
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionStrategyType {
    /// Portal + Token strategy (universal)
    PortalToken,
    /// Mutter Direct API strategy (GNOME only, dormant)
    MutterDirect,
    /// wlr-screencopy strategy (wlroots native)
    WlrScreencopy,
    /// libei/EIS strategy (wlroots via Portal)
    LibeiEis,
}

/// Registry of initialization quirks
///
/// Provides lookup of applicable quirks based on deployment context,
/// compositor type, and session strategy.
pub struct InitQuirkRegistry;

impl InitQuirkRegistry {
    pub fn quirks_for(
        deployment: DeploymentContext,
        compositor: &CompositorType,
        strategy: SessionStrategyType,
    ) -> Vec<InitQuirk> {
        let mut quirks = Vec::new();

        // === Deployment-based quirks ===

        if matches!(deployment, DeploymentContext::Flatpak) {
            // Flatpak-specific quirks
            quirks.push(InitQuirk::SingleClipboardProxy);
            quirks.push(InitQuirk::WlrScreencopyBlockedBySandbox);
            quirks.push(InitQuirk::PamBlockedBySandbox);
        }

        // === Compositor-based quirks ===

        if matches!(compositor, CompositorType::Gnome { .. }) {
            quirks.push(InitQuirk::GnomePersistenceRejected);
        }

        if matches!(compositor, CompositorType::Kde { .. }) {
            // Check for Portal Clipboard threading bug (KDE Bug 515465)
            // Affected: ALL current KDE versions with Portal Clipboard (v6.3.90-6.5.x)
            // Fixed in: v6.6.0 (NOT backported to 6.5.x)
            //
            // Apply quirk to ALL KDE since:
            // - Compositor version detection doesn't populate KDE version currently
            // - All KDE versions in the wild have the bug (6.3.90-6.5.x)
            // - Better to be safe (disable) than crash
            // - Can refine when proper version detection available
            match compositor {
                CompositorType::Kde { version: Some(v) } => {
                    if Self::is_affected_kde_version(v) {
                        quirks.push(InitQuirk::KdePortalClipboardThreadingBug);
                    }
                }
                CompositorType::Kde { version: None } => {
                    // No version info - assume affected (all current KDE has bug)
                    quirks.push(InitQuirk::KdePortalClipboardThreadingBug);
                }
                _ => {}
            }
        }

        // === Strategy-based quirks ===

        match strategy {
            SessionStrategyType::PortalToken | SessionStrategyType::LibeiEis => {
                // All Portal strategies need clipboard before session
                quirks.push(InitQuirk::ClipboardBeforeSession);
                quirks.push(InitQuirk::AsyncSessionCleanup);
                quirks.push(InitQuirk::RequiresSecureTokenStorage);
            }
            SessionStrategyType::MutterDirect => {
                quirks.push(InitQuirk::MutterSessionLinkageBroken);
            }
            SessionStrategyType::WlrScreencopy => {
                // wlr-screencopy has its own session management
            }
        }

        quirks
    }

    pub fn has_quirk(
        deployment: DeploymentContext,
        compositor: &CompositorType,
        strategy: SessionStrategyType,
        quirk: InitQuirk,
    ) -> bool {
        Self::quirks_for(deployment, compositor, strategy).contains(&quirk)
    }

    pub fn clipboard_quirks(
        deployment: DeploymentContext,
        compositor: &CompositorType,
        strategy: SessionStrategyType,
    ) -> Vec<InitQuirk> {
        Self::quirks_for(deployment, compositor, strategy)
            .into_iter()
            .filter(|q| {
                matches!(
                    q,
                    InitQuirk::SingleClipboardProxy
                        | InitQuirk::ClipboardBeforeSession
                        | InitQuirk::KdePortalClipboardThreadingBug
                )
            })
            .collect()
    }

    pub fn retry_quirks(
        deployment: DeploymentContext,
        compositor: &CompositorType,
        strategy: SessionStrategyType,
    ) -> Vec<InitQuirk> {
        Self::quirks_for(deployment, compositor, strategy)
            .into_iter()
            .filter(|q| {
                matches!(
                    q,
                    InitQuirk::SingleClipboardProxy
                        | InitQuirk::AsyncSessionCleanup
                        | InitQuirk::GnomePersistenceRejected
                )
            })
            .collect()
    }

    /// Check if KDE Plasma version is affected by Portal Clipboard bug
    ///
    /// Bug 515465: Threading violations in portal-kde clipboard implementation
    /// Affected: v6.3.90 through v6.5.x (May 2025 - February 2026)
    /// Fixed in: v6.6.0 (NOT backported to 6.5.x branch)
    fn is_affected_kde_version(version: &str) -> bool {
        // Parse version string (e.g., "6.5.5", "6.4.0", "6.3.90")
        let parts: Vec<&str> = version.split('.').collect();
        if parts.len() < 2 {
            return false; // Can't parse, assume not affected
        }

        let major = parts[0].parse::<u32>().unwrap_or(0);
        let minor = parts[1].parse::<u32>().unwrap_or(0);
        let patch = parts
            .get(2)
            .and_then(|p| p.parse::<u32>().ok())
            .unwrap_or(0);

        // Only Plasma 6.x affected
        if major != 6 {
            return false;
        }

        // Version is (major, minor, patch)
        let v = (major, minor, patch);

        // Bug introduced in 6.3.90, fixed in 6.6.0 (NOT backported to 6.5.x)
        // Affected: 6.3.90 <= version < 6.6.0
        let bug_start = (6, 3, 90); // First affected version
        let bug_end = (6, 6, 0); // First fixed version (6.6.0)

        v >= bug_start && v < bug_end
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_flatpak_quirks() {
        let quirks = InitQuirkRegistry::quirks_for(
            DeploymentContext::Flatpak,
            &CompositorType::Gnome {
                version: Some("46".to_string()),
            },
            SessionStrategyType::PortalToken,
        );

        assert!(quirks.contains(&InitQuirk::SingleClipboardProxy));
        assert!(quirks.contains(&InitQuirk::GnomePersistenceRejected));
        assert!(quirks.contains(&InitQuirk::ClipboardBeforeSession));
        assert!(quirks.contains(&InitQuirk::PamBlockedBySandbox));
    }

    #[test]
    fn test_native_gnome_quirks() {
        let quirks = InitQuirkRegistry::quirks_for(
            DeploymentContext::Native,
            &CompositorType::Gnome {
                version: Some("46".to_string()),
            },
            SessionStrategyType::PortalToken,
        );

        // Native doesn't have SingleClipboardProxy
        assert!(!quirks.contains(&InitQuirk::SingleClipboardProxy));
        // But still has GNOME persistence quirk
        assert!(quirks.contains(&InitQuirk::GnomePersistenceRejected));
    }

    #[test]
    fn test_native_wlroots_quirks() {
        let quirks = InitQuirkRegistry::quirks_for(
            DeploymentContext::Native,
            &CompositorType::Sway {
                version: Some("1.9".to_string()),
            },
            SessionStrategyType::PortalToken,
        );

        // wlroots doesn't have GNOME quirks
        assert!(!quirks.contains(&InitQuirk::GnomePersistenceRejected));
        // But still has portal quirks
        assert!(quirks.contains(&InitQuirk::ClipboardBeforeSession));
    }
}
