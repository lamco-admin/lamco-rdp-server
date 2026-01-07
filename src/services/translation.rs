//! Compositor to Service translation
//!
//! This module contains the logic to translate detected compositor
//! capabilities into advertised services with appropriate levels.

use crate::compositor::{
    BufferType, CompositorCapabilities, CompositorType, CursorMode, Quirk, SourceType,
};

use super::{
    rdp_capabilities::RdpCapability,
    service::{AdvertisedService, PerformanceHints, ServiceId, ServiceLevel},
    wayland_features::{DamageMethod, DrmFormat, WaylandFeature},
};

/// Translate compositor capabilities into a list of advertised services
pub(super) fn translate_capabilities(caps: &CompositorCapabilities) -> Vec<AdvertisedService> {
    let mut services = Vec::new();

    // Damage Tracking
    services.push(translate_damage_tracking(caps));

    // DMA-BUF Zero-Copy
    services.push(translate_dmabuf(caps));

    // Explicit Sync
    services.push(translate_explicit_sync(caps));

    // Fractional Scaling
    services.push(translate_fractional_scaling(caps));

    // Metadata Cursor
    services.push(translate_metadata_cursor(caps));

    // Multi-Monitor
    services.push(translate_multi_monitor(caps));

    // Window Capture
    services.push(translate_window_capture(caps));

    // HDR Color Space (future - currently unavailable)
    services.push(AdvertisedService::unavailable(ServiceId::HdrColorSpace));

    // Clipboard
    services.push(translate_clipboard(caps));

    // Remote Input
    services.push(translate_remote_input(caps));

    // Video Capture (PipeWire)
    services.push(translate_video_capture(caps));

    // === Phase 2: Session Persistence Services ===

    // Session Persistence (portal restore tokens)
    services.push(translate_session_persistence(caps));

    // Direct Compositor API (GNOME Mutter)
    services.push(translate_direct_compositor_api(caps));

    // Credential Storage
    services.push(translate_credential_storage(caps));

    // wlr-screencopy (wlroots bypass)
    services.push(translate_wlr_screencopy(caps));

    // Unattended Access (aggregate capability)
    services.push(translate_unattended_access(caps));

    services
}

fn translate_damage_tracking(caps: &CompositorCapabilities) -> AdvertisedService {
    let profile = &caps.profile;

    let (level, method, hints) = if profile.supports_damage_hints {
        // Compositor provides native damage hints
        let method = if caps.compositor.is_wlroots_based() {
            DamageMethod::NativeScreencopy
        } else {
            DamageMethod::Portal
        };

        (ServiceLevel::Guaranteed, method, true)
    } else {
        // Fall back to frame differencing
        (ServiceLevel::BestEffort, DamageMethod::FrameDiff, false)
    };

    let feature = WaylandFeature::DamageTracking {
        method,
        compositor_hints: hints,
    };

    let mut service = if level == ServiceLevel::Guaranteed {
        AdvertisedService::guaranteed(ServiceId::DamageTracking, feature)
    } else {
        AdvertisedService::best_effort(ServiceId::DamageTracking, feature)
    };

    // Set performance hints based on method
    let mut perf = PerformanceHints::default();
    match method {
        DamageMethod::NativeScreencopy => {
            perf.latency_overhead_ms = Some(1);
            perf.simd_available = true;
        }
        DamageMethod::Portal => {
            perf.latency_overhead_ms = Some(2);
            perf.simd_available = true;
        }
        DamageMethod::FrameDiff | DamageMethod::Hybrid => {
            perf.latency_overhead_ms = Some(5);
            perf.simd_available = true; // Our SIMD tile comparison
        }
    }
    service.performance = perf;

    service
}

fn translate_dmabuf(caps: &CompositorCapabilities) -> AdvertisedService {
    let profile = &caps.profile;

    // Check for DMA-BUF quirk
    if profile.has_quirk(&Quirk::PoorDmaBufSupport) {
        return AdvertisedService::unavailable(ServiceId::DmaBufZeroCopy)
            .with_note("Compositor has unreliable DMA-BUF support");
    }

    match profile.recommended_buffer_type {
        BufferType::DmaBuf => {
            let feature = WaylandFeature::DmaBufZeroCopy {
                formats: vec![DrmFormat::Argb8888, DrmFormat::Xrgb8888],
                supports_modifiers: true,
            };

            AdvertisedService::guaranteed(ServiceId::DmaBufZeroCopy, feature)
                .with_rdp_capability(RdpCapability::egfx_full())
                .with_performance(PerformanceHints::zero_copy())
        }
        BufferType::Any => {
            // DMA-BUF may work but not preferred
            let feature = WaylandFeature::DmaBufZeroCopy {
                formats: vec![DrmFormat::Argb8888],
                supports_modifiers: false,
            };

            AdvertisedService::best_effort(ServiceId::DmaBufZeroCopy, feature)
                .with_rdp_capability(RdpCapability::egfx_avc420())
                .with_performance(PerformanceHints::memcpy())
        }
        BufferType::MemFd => AdvertisedService::unavailable(ServiceId::DmaBufZeroCopy)
            .with_note("Compositor prefers MemFd buffers"),
    }
}

fn translate_explicit_sync(caps: &CompositorCapabilities) -> AdvertisedService {
    if caps.profile.supports_explicit_sync {
        let feature = WaylandFeature::ExplicitSync { version: 1 };
        AdvertisedService::guaranteed(ServiceId::ExplicitSync, feature)
    } else {
        AdvertisedService::unavailable(ServiceId::ExplicitSync)
    }
}

fn translate_fractional_scaling(caps: &CompositorCapabilities) -> AdvertisedService {
    if caps.has_fractional_scale() {
        let feature = WaylandFeature::FractionalScaling { max_scale: 3.0 };
        AdvertisedService::guaranteed(ServiceId::FractionalScaling, feature).with_rdp_capability(
            RdpCapability::DesktopComposition {
                multi_mon: false,
                max_monitors: 1,
                scaling: true,
            },
        )
    } else {
        AdvertisedService::unavailable(ServiceId::FractionalScaling)
    }
}

fn translate_metadata_cursor(caps: &CompositorCapabilities) -> AdvertisedService {
    let portal = &caps.portal;
    let profile = &caps.profile;

    // Check if metadata cursor mode is available
    let has_metadata = portal
        .available_cursor_modes
        .contains(&CursorMode::Metadata);

    // Check for cursor quirks
    let needs_composite = profile.has_quirk(&Quirk::NeedsExplicitCursorComposite);

    if has_metadata && !needs_composite {
        let feature = WaylandFeature::MetadataCursor {
            has_hotspot: true,
            has_shape_updates: true,
        };

        AdvertisedService::guaranteed(ServiceId::MetadataCursor, feature)
            .with_rdp_capability(RdpCapability::cursor_metadata())
    } else if has_metadata {
        // Metadata available but needs workaround
        let feature = WaylandFeature::MetadataCursor {
            has_hotspot: true,
            has_shape_updates: false,
        };

        AdvertisedService::degraded(
            ServiceId::MetadataCursor,
            feature,
            "Requires explicit cursor compositing",
        )
        .with_rdp_capability(RdpCapability::cursor_painted())
    } else {
        AdvertisedService::unavailable(ServiceId::MetadataCursor)
            .with_rdp_capability(RdpCapability::cursor_painted())
    }
}

fn translate_multi_monitor(caps: &CompositorCapabilities) -> AdvertisedService {
    let portal = &caps.portal;
    let profile = &caps.profile;

    let has_virtual = portal.available_source_types.contains(&SourceType::Virtual);
    let has_monitor = portal.available_source_types.contains(&SourceType::Monitor);

    if !has_monitor && !has_virtual {
        return AdvertisedService::unavailable(ServiceId::MultiMonitor);
    }

    // Check for multi-monitor quirks
    let has_position_quirk = profile.has_quirk(&Quirk::MultiMonitorPositionQuirk);
    let restart_on_resize = profile.has_quirk(&Quirk::RestartCaptureOnResize);

    let max_monitors = if has_virtual { 16 } else { 4 };

    let feature = WaylandFeature::MultiMonitor {
        max_monitors,
        virtual_source: has_virtual,
    };

    let rdp_cap = RdpCapability::DesktopComposition {
        multi_mon: true,
        max_monitors,
        scaling: caps.has_fractional_scale(),
    };

    if has_position_quirk {
        AdvertisedService::degraded(
            ServiceId::MultiMonitor,
            feature,
            "Monitor positions may be incorrect",
        )
        .with_rdp_capability(rdp_cap)
    } else if restart_on_resize {
        AdvertisedService::best_effort(ServiceId::MultiMonitor, feature)
            .with_rdp_capability(rdp_cap)
            .with_note("Capture restarts on resolution change")
    } else {
        AdvertisedService::guaranteed(ServiceId::MultiMonitor, feature).with_rdp_capability(rdp_cap)
    }
}

fn translate_window_capture(caps: &CompositorCapabilities) -> AdvertisedService {
    let portal = &caps.portal;

    let has_window = portal.available_source_types.contains(&SourceType::Window);

    if has_window {
        let feature = WaylandFeature::WindowCapture {
            has_toplevel_export: caps.compositor.is_wlroots_based(),
        };

        // wlroots has better window capture via toplevel export
        if caps.compositor.is_wlroots_based() {
            AdvertisedService::guaranteed(ServiceId::WindowCapture, feature)
        } else {
            AdvertisedService::best_effort(ServiceId::WindowCapture, feature)
        }
    } else {
        AdvertisedService::unavailable(ServiceId::WindowCapture)
    }
}

fn translate_clipboard(caps: &CompositorCapabilities) -> AdvertisedService {
    let portal = &caps.portal;

    if portal.supports_clipboard {
        let feature = WaylandFeature::Clipboard {
            portal_version: portal.version,
        };

        let has_extra_handshake = caps.profile.has_quirk(&Quirk::ClipboardExtraHandshake);

        if has_extra_handshake {
            AdvertisedService::degraded(
                ServiceId::Clipboard,
                feature,
                "Requires extra handshake for paste",
            )
            .with_rdp_capability(RdpCapability::clipboard_standard(10 * 1024 * 1024))
        } else {
            AdvertisedService::guaranteed(ServiceId::Clipboard, feature)
                .with_rdp_capability(RdpCapability::clipboard_standard(10 * 1024 * 1024))
        }
    } else {
        AdvertisedService::unavailable(ServiceId::Clipboard)
    }
}

fn translate_remote_input(caps: &CompositorCapabilities) -> AdvertisedService {
    if caps.portal.supports_remote_desktop {
        let feature = WaylandFeature::RemoteInput {
            uses_libei: true,
            keyboard: true,
            pointer: true,
            touch: false, // Touch typically requires additional setup
        };

        AdvertisedService::guaranteed(ServiceId::RemoteInput, feature)
            .with_rdp_capability(RdpCapability::input_full())
    } else {
        AdvertisedService::unavailable(ServiceId::RemoteInput)
    }
}

fn translate_video_capture(caps: &CompositorCapabilities) -> AdvertisedService {
    if caps.portal.supports_screencast {
        let buffer_type = match caps.profile.recommended_buffer_type {
            BufferType::DmaBuf => "dmabuf",
            BufferType::MemFd => "memfd",
            BufferType::Any => "any",
        };

        let feature = WaylandFeature::PipeWireStream {
            node_id: None,
            buffer_type: buffer_type.to_string(),
        };

        let mut perf = PerformanceHints::default();
        perf.recommended_fps = Some(caps.profile.recommended_fps_cap);
        perf.zero_copy_available = caps.profile.recommended_buffer_type == BufferType::DmaBuf;

        AdvertisedService::guaranteed(ServiceId::VideoCapture, feature)
            .with_performance(perf)
            .with_rdp_capability(RdpCapability::egfx_avc420())
    } else {
        AdvertisedService::unavailable(ServiceId::VideoCapture)
    }
}

// ============================================================================
// PHASE 2: Session Persistence Translation Functions
// ============================================================================

fn translate_session_persistence(caps: &CompositorCapabilities) -> AdvertisedService {
    use crate::services::wayland_features::TokenStorageMethod;
    use crate::session::CredentialStorageMethod;

    let portal = &caps.portal;

    // Use cached credential storage from caps (detected during probing)
    let cred_method = caps.credential_storage_method;
    let accessible = caps.credential_storage_accessible;

    let token_storage = match cred_method {
        CredentialStorageMethod::Tpm2 => TokenStorageMethod::Tpm2SystemdCreds,
        CredentialStorageMethod::GnomeKeyring
        | CredentialStorageMethod::KWallet
        | CredentialStorageMethod::KeePassXC => TokenStorageMethod::SecretService,
        CredentialStorageMethod::FlatpakSecretPortal => TokenStorageMethod::FlatpakSecretPortal,
        CredentialStorageMethod::EncryptedFile => TokenStorageMethod::EncryptedFile,
        CredentialStorageMethod::None => TokenStorageMethod::None,
    };

    let feature = WaylandFeature::SessionPersistence {
        restore_token_supported: portal.supports_restore_tokens,
        max_persist_mode: portal.max_persist_mode,
        token_storage,
        portal_version: portal.version,
    };

    // Determine service level based on token support + storage
    let level = match (portal.supports_restore_tokens, accessible, token_storage) {
        // Portal v4+ with working storage
        (true, true, TokenStorageMethod::Tpm2SystemdCreds) => ServiceLevel::Guaranteed,
        (true, true, TokenStorageMethod::SecretService) => ServiceLevel::Guaranteed,
        (true, true, TokenStorageMethod::FlatpakSecretPortal) => ServiceLevel::Guaranteed,
        (true, true, TokenStorageMethod::EncryptedFile) => ServiceLevel::BestEffort,
        // Portal supports tokens but storage issues
        (true, false, _) => ServiceLevel::Degraded,
        (true, true, TokenStorageMethod::None) => ServiceLevel::Degraded,
        // Portal doesn't support tokens
        (false, _, _) => ServiceLevel::Unavailable,
    };

    // Create service with appropriate constructor
    let service = match level {
        ServiceLevel::Guaranteed => {
            AdvertisedService::guaranteed(ServiceId::SessionPersistence, feature)
        }
        ServiceLevel::BestEffort => {
            AdvertisedService::best_effort(ServiceId::SessionPersistence, feature)
        }
        ServiceLevel::Degraded => {
            let note = if !portal.supports_restore_tokens {
                format!(
                    "Portal v{} does not support restore tokens (requires v4+)",
                    portal.version
                )
            } else if !accessible {
                "Credential storage exists but is not accessible (locked?)".to_string()
            } else {
                "Degraded session persistence".to_string()
            };
            AdvertisedService::degraded(ServiceId::SessionPersistence, feature, &note)
        }
        ServiceLevel::Unavailable => {
            return AdvertisedService::unavailable(ServiceId::SessionPersistence).with_note(
                &format!("Portal v{} does not support restore tokens", portal.version),
            );
        }
    };

    service
}

fn translate_direct_compositor_api(caps: &CompositorCapabilities) -> AdvertisedService {
    // Mutter Direct API: DORMANT - Tested and found broken on GNOME 40 and 46
    //
    // Test Results:
    //   GNOME 40.10 (RHEL 9):     ScreenCast works, RemoteDesktop fails (1,137 input errors)
    //   GNOME 46.0 (Ubuntu 24.04): ScreenCast broken, RemoteDesktop fails
    //
    // Root Cause: RemoteDesktop and ScreenCast sessions cannot be linked
    //   - RemoteDesktop.CreateSession() takes no arguments (can't pass session-id)
    //   - ScreenCast doesn't expose SessionId property
    //   - Input injection fails: "No screen cast active" or silent failures
    //
    // Portal Strategy works universally on all tested GNOME versions.
    //
    // Code preserved in src/mutter/ (not deleted) in case GNOME fixes session linkage.
    // To re-enable: Change Unavailable → BestEffort and test thoroughly.

    match &caps.compositor {
        CompositorType::Gnome { version } => {
            AdvertisedService::unavailable(ServiceId::DirectCompositorAPI).with_note(&format!(
            "Mutter API non-functional (tested on GNOME 40, 46 - session linkage broken). GNOME {}",
            version.as_deref().unwrap_or("unknown")
        ))
        }
        _ => AdvertisedService::unavailable(ServiceId::DirectCompositorAPI)
            .with_note("Only implemented for GNOME compositor"),
    }
}

fn translate_credential_storage(caps: &CompositorCapabilities) -> AdvertisedService {
    let deployment = &caps.deployment;

    // Use cached credential storage from caps (detected during probing)
    let method = caps.credential_storage_method;
    let encryption = caps.credential_encryption;
    let accessible = caps.credential_storage_accessible;

    let feature = WaylandFeature::CredentialStorage {
        method,
        is_accessible: accessible,
        encryption,
    };

    use crate::session::CredentialStorageMethod;

    let level = match (method, accessible) {
        (CredentialStorageMethod::Tpm2, true) => ServiceLevel::Guaranteed,
        (CredentialStorageMethod::GnomeKeyring, true) => ServiceLevel::Guaranteed,
        (CredentialStorageMethod::KWallet, true) => ServiceLevel::Guaranteed,
        (CredentialStorageMethod::KeePassXC, true) => ServiceLevel::Guaranteed,
        (CredentialStorageMethod::FlatpakSecretPortal, true) => ServiceLevel::Guaranteed,
        (CredentialStorageMethod::EncryptedFile, true) => ServiceLevel::BestEffort,
        (_, false) => ServiceLevel::Degraded, // Storage exists but locked
        (CredentialStorageMethod::None, _) => ServiceLevel::Unavailable,
    };

    let note = match (deployment, method) {
        (
            crate::session::DeploymentContext::Flatpak,
            CredentialStorageMethod::FlatpakSecretPortal,
        ) => Some("Using Flatpak Secret Portal (host keyring via sandbox)".to_string()),
        (crate::session::DeploymentContext::Flatpak, _) => {
            Some("Using encrypted file (Secret Portal unavailable)".to_string())
        }
        _ => None,
    };

    // Create service with appropriate constructor
    let service = match level {
        ServiceLevel::Guaranteed => {
            let mut s = AdvertisedService::guaranteed(ServiceId::CredentialStorage, feature);
            if let Some(n) = note {
                s = s.with_note(&n);
            }
            s
        }
        ServiceLevel::BestEffort => {
            let mut s = AdvertisedService::best_effort(ServiceId::CredentialStorage, feature);
            if let Some(n) = note {
                s = s.with_note(&n);
            }
            s
        }
        ServiceLevel::Degraded => AdvertisedService::degraded(
            ServiceId::CredentialStorage,
            feature,
            note.as_deref().unwrap_or("Credential storage degraded"),
        ),
        ServiceLevel::Unavailable => AdvertisedService::unavailable(ServiceId::CredentialStorage),
    };

    service
}

fn translate_wlr_screencopy(caps: &CompositorCapabilities) -> AdvertisedService {
    use crate::session::DeploymentContext;

    // Not available in Flatpak (no direct Wayland socket access)
    if matches!(caps.deployment, DeploymentContext::Flatpak) {
        return AdvertisedService::unavailable(ServiceId::WlrScreencopy)
            .with_note("wlr-screencopy blocked by Flatpak sandbox");
    }

    if !caps.compositor.is_wlroots_based() {
        return AdvertisedService::unavailable(ServiceId::WlrScreencopy)
            .with_note("Only available on wlroots-based compositors");
    }

    // Check for wlr-screencopy-unstable-v1 protocol
    if let Some(version) = caps.get_protocol_version("zwlr_screencopy_manager_v1") {
        let feature = WaylandFeature::WlrScreencopy {
            version,
            dmabuf_supported: caps.has_protocol("linux_dmabuf_v1", 1),
            damage_supported: version >= 3, // Damage tracking added in v3
        };

        AdvertisedService::guaranteed(ServiceId::WlrScreencopy, feature)
            .with_note("Direct capture without portal permission dialog")
    } else {
        AdvertisedService::unavailable(ServiceId::WlrScreencopy)
            .with_note("wlr-screencopy protocol not found")
    }
}

fn translate_unattended_access(caps: &CompositorCapabilities) -> AdvertisedService {
    // Get dependent service levels
    let session_persist_level = translate_session_persistence(caps).level;
    let direct_api_level = translate_direct_compositor_api(caps).level;
    let wlr_screencopy_level = translate_wlr_screencopy(caps).level;
    let cred_storage_level = translate_credential_storage(caps).level;

    // Can we avoid dialog?
    let can_avoid_dialog = session_persist_level >= ServiceLevel::BestEffort
        || direct_api_level >= ServiceLevel::BestEffort
        || wlr_screencopy_level >= ServiceLevel::Guaranteed;

    // Can we store credentials?
    let can_store_credentials = cred_storage_level >= ServiceLevel::BestEffort;

    let feature = WaylandFeature::UnattendedAccess {
        can_avoid_dialog,
        can_store_credentials,
    };

    // Determine overall level
    let (level, note) = match (can_avoid_dialog, can_store_credentials) {
        (true, true) => (
            ServiceLevel::Guaranteed,
            "Full unattended operation available",
        ),
        (true, false) => (
            ServiceLevel::BestEffort,
            "Dialog avoidance available, credential storage limited",
        ),
        (false, true) => (
            ServiceLevel::Degraded,
            "Credential storage available, but dialog required each session",
        ),
        (false, false) => (
            ServiceLevel::Unavailable,
            "Manual intervention required for each session",
        ),
    };

    // Create service with appropriate constructor
    match level {
        ServiceLevel::Guaranteed => {
            AdvertisedService::guaranteed(ServiceId::UnattendedAccess, feature).with_note(note)
        }
        ServiceLevel::BestEffort => {
            AdvertisedService::best_effort(ServiceId::UnattendedAccess, feature).with_note(note)
        }
        ServiceLevel::Degraded => {
            AdvertisedService::degraded(ServiceId::UnattendedAccess, feature, note)
        }
        ServiceLevel::Unavailable => {
            AdvertisedService::unavailable(ServiceId::UnattendedAccess).with_note(note)
        }
    }
}

// Helper functions for translation

#[allow(dead_code)] // Reserved for future service detection
fn check_dbus_interface_sync(interface: &str) -> bool {
    // Check if D-Bus interface exists synchronously
    // Uses blocking to avoid nested runtime issues when called from async context

    // Use std::thread to avoid tokio runtime nesting issues
    let interface = interface.to_string();

    std::thread::scope(|s| {
        let handle = s.spawn(move || {
            // Create a new tokio runtime for this thread
            let rt = tokio::runtime::Runtime::new().ok()?;
            rt.block_on(async {
                let conn = zbus::Connection::session().await.ok()?;
                let proxy = zbus::fdo::DBusProxy::new(&conn).await.ok()?;
                let names = proxy.list_names().await.ok()?;
                Some(names.iter().any(|n| n.as_str().contains(&interface)))
            })
        });

        handle.join().ok().flatten().unwrap_or(false)
    })
}

#[allow(dead_code)] // Reserved for future GNOME version checks
fn parse_gnome_version(version_str: &str) -> Option<f32> {
    // Parse "46.0" or "46.2" to 46.0, 46.2
    version_str
        .split('.')
        .take(2)
        .collect::<Vec<_>>()
        .join(".")
        .parse::<f32>()
        .ok()
}

// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compositor::PortalCapabilities;

    fn make_gnome_caps() -> CompositorCapabilities {
        let compositor = CompositorType::Gnome {
            version: Some("46.0".to_string()),
        };

        let mut portal = PortalCapabilities::default();
        portal.supports_screencast = true;
        portal.supports_remote_desktop = true;
        portal.supports_clipboard = true;
        portal.version = 5;
        portal.available_cursor_modes = vec![CursorMode::Metadata, CursorMode::Embedded];
        portal.available_source_types = vec![SourceType::Monitor, SourceType::Window];

        CompositorCapabilities::new(compositor, portal, vec![])
    }

    #[test]
    fn test_gnome_translation() {
        let caps = make_gnome_caps();
        let services = translate_capabilities(&caps);

        // Check damage tracking
        let damage = services.iter().find(|s| s.id == ServiceId::DamageTracking);
        assert!(damage.is_some());
        assert!(damage.unwrap().level.is_reliable());

        // Check metadata cursor
        let cursor = services.iter().find(|s| s.id == ServiceId::MetadataCursor);
        assert!(cursor.is_some());
        assert_eq!(cursor.unwrap().level, ServiceLevel::Guaranteed);

        // Check clipboard
        let clipboard = services.iter().find(|s| s.id == ServiceId::Clipboard);
        assert!(clipboard.is_some());
        assert!(clipboard.unwrap().level.is_usable());
    }

    #[test]
    fn test_service_count() {
        let caps = make_gnome_caps();
        let services = translate_capabilities(&caps);

        // Should have all service types
        assert_eq!(services.len(), ServiceId::all().len());
    }
}
