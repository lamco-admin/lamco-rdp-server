//! Compositor D-Bus Name Watcher
//!
//! Monitors the compositor's D-Bus well-known name via `NameOwnerChanged` signals.
//! When the name disappears (empty new_owner), the compositor has crashed or restarted,
//! and we report `HealthEvent::CompositorLost` to the health monitor.

use futures_util::StreamExt;
use tracing::{debug, error, info, warn};

use super::{HealthEvent, HealthReporter};
use crate::session::SessionType;

/// D-Bus well-known names to watch per session strategy
fn compositor_bus_names(session_type: SessionType) -> Vec<&'static str> {
    match session_type {
        SessionType::MutterDirect => vec![
            "org.gnome.Mutter.ScreenCast",
            "org.gnome.Mutter.RemoteDesktop",
        ],
        SessionType::Portal | SessionType::Libei | SessionType::ScreenCastOnly => {
            // Portal sessions go through xdg-desktop-portal, which proxies to
            // the compositor. Monitor the portal daemon itself.
            vec!["org.freedesktop.portal.Desktop"]
        }
        SessionType::WlrDirect => {
            // wlr-direct uses Portal ScreenCast for video capture
            vec!["org.freedesktop.portal.Desktop"]
        }
        SessionType::PortalGeneric => {
            // Wayland-native strategy, no D-Bus runtime dependency
            vec![]
        }
    }
}

/// Start a background task that watches for compositor D-Bus name loss.
///
/// Returns a `JoinHandle` for the watcher task. The task runs until the shutdown
/// signal is received or all watched names have disappeared.
pub async fn start_compositor_watcher(
    session_type: SessionType,
    reporter: HealthReporter,
    shutdown: tokio::sync::broadcast::Receiver<()>,
) -> Option<tokio::task::JoinHandle<()>> {
    let names = compositor_bus_names(session_type);
    if names.is_empty() {
        debug!("No compositor D-Bus names to watch for {session_type}");
        return None;
    }

    let connection = match zbus::Connection::session().await {
        Ok(c) => c,
        Err(e) => {
            warn!("Cannot connect to session bus for compositor watching: {e}");
            return None;
        }
    };

    info!("Starting compositor D-Bus name watcher for: {:?}", names);

    let handle = tokio::spawn(watch_names(connection, names, reporter, shutdown));
    Some(handle)
}

async fn watch_names(
    connection: zbus::Connection,
    names: Vec<&'static str>,
    reporter: HealthReporter,
    mut shutdown: tokio::sync::broadcast::Receiver<()>,
) {
    // Build a proxy to org.freedesktop.DBus to receive NameOwnerChanged
    #[expect(
        clippy::expect_used,
        reason = "static well-known D-Bus names are always valid"
    )]
    let dbus_proxy: zbus::Proxy<'_> = match zbus::ProxyBuilder::new(&connection)
        .interface("org.freedesktop.DBus")
        .expect("valid interface")
        .path("/org/freedesktop/DBus")
        .expect("valid path")
        .destination("org.freedesktop.DBus")
        .expect("valid destination")
        .build()
        .await
    {
        Ok(p) => p,
        Err(e) => {
            warn!("Failed to create org.freedesktop.DBus proxy: {e}");
            return;
        }
    };

    let signal_stream = match dbus_proxy.receive_signal("NameOwnerChanged").await {
        Ok(s) => s,
        Err(e) => {
            warn!("Failed to subscribe to NameOwnerChanged: {e}");
            return;
        }
    };

    futures_util::pin_mut!(signal_stream);

    loop {
        tokio::select! {
            Some(msg) = signal_stream.next() => {
                // NameOwnerChanged(name: s, old_owner: s, new_owner: s)
                let args: (String, String, String) = match msg.body().deserialize() {
                    Ok(a) => a,
                    Err(e) => {
                        debug!("Failed to deserialize NameOwnerChanged: {e}");
                        continue;
                    }
                };
                let (name, old_owner, new_owner) = args;

                // Only care about names we're watching, and only when they disappear
                if names.contains(&name.as_str()) && new_owner.is_empty() && !old_owner.is_empty() {
                    error!(
                        "Compositor D-Bus name '{}' disappeared (owner '{}' gone)",
                        name, old_owner
                    );
                    reporter.report(HealthEvent::CompositorLost {
                        bus_name: name,
                    });
                }
            }
            _ = shutdown.recv() => {
                debug!("Compositor watcher received shutdown");
                break;
            }
        }
    }

    info!("Compositor D-Bus name watcher stopped");
}
