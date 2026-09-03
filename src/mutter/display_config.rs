//! Mutter DisplayConfig client, enough of it to place a capture area.
//!
//! `RecordArea` takes a rectangle in stage coordinates, so choosing it over
//! `RecordMonitor` means resolving a connector to that rectangle ourselves.
//! `org.gnome.Mutter.DisplayConfig.GetCurrentState` is the only place that
//! mapping exists: a logical monitor carries the stage position and the scale,
//! and the physical monitor carries the current mode's pixel size.
//!
//! This is deliberately a narrow reader. We do not configure displays here, and
//! the interface's `ApplyMonitorsConfig` half is not wrapped.

use std::collections::HashMap;

use anyhow::{Context, Result, anyhow};
use tracing::debug;
use zbus::zvariant::OwnedValue;

/// `(connector, vendor, product, serial)`.
type MonitorSpec = (String, String, String, String);

/// `(id, width, height, refresh, preferred_scale, supported_scales, properties)`.
type Mode = (
    String,
    i32,
    i32,
    f64,
    f64,
    Vec<f64>,
    HashMap<String, OwnedValue>,
);

/// `(spec, modes, properties)`.
type Monitor = (MonitorSpec, Vec<Mode>, HashMap<String, OwnedValue>);

/// `(x, y, scale, transform, primary, monitors, properties)`.
type LogicalMonitor = (
    i32,
    i32,
    f64,
    u32,
    bool,
    Vec<MonitorSpec>,
    HashMap<String, OwnedValue>,
);

/// `(serial, monitors, logical_monitors, properties)`.
type CurrentState = (
    u32,
    Vec<Monitor>,
    Vec<LogicalMonitor>,
    HashMap<String, OwnedValue>,
);

/// A capture area in stage coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StageArea {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

/// Resolve a monitor connector to its rectangle in stage coordinates.
///
/// Returns the connector's logical position together with its current mode
/// divided by the logical scale, which is what the stage is laid out in. Under
/// fractional scaling the division is rounded, so the area can differ from the
/// compositor's own rounding by a pixel; that is why the caller should prefer
/// this only where an exact monitor stream is not required.
///
/// `connector` of `None` selects the primary logical monitor, falling back to
/// the first one when no monitor is marked primary.
pub async fn stage_area_for_connector(
    connection: &zbus::Connection,
    connector: Option<&str>,
) -> Result<StageArea> {
    let proxy = zbus::Proxy::new(
        connection,
        "org.gnome.Mutter.DisplayConfig",
        "/org/gnome/Mutter/DisplayConfig",
        "org.gnome.Mutter.DisplayConfig",
    )
    .await
    .context("Failed to create DisplayConfig proxy")?;

    let reply = proxy
        .call_method("GetCurrentState", &())
        .await
        .context("Failed to call GetCurrentState")?;
    let (_serial, monitors, logical_monitors, _props): CurrentState = reply
        .body()
        .deserialize()
        .context("Failed to deserialize GetCurrentState")?;

    let logical = match connector {
        Some(name) => logical_monitors
            .iter()
            .find(|lm| lm.5.iter().any(|spec| spec.0 == name))
            .ok_or_else(|| anyhow!("No logical monitor carries connector {name}"))?,
        None => logical_monitors
            .iter()
            .find(|lm| lm.4)
            .or_else(|| logical_monitors.first())
            .ok_or_else(|| anyhow!("Compositor reported no logical monitors"))?,
    };

    // A logical monitor can drive several connectors when mirroring; they share
    // the rectangle, so the first one answers the question.
    let spec_name = connector
        .map(ToOwned::to_owned)
        .or_else(|| logical.5.first().map(|spec| spec.0.clone()))
        .ok_or_else(|| anyhow!("Logical monitor lists no connectors"))?;

    let monitor = monitors
        .iter()
        .find(|m| m.0.0 == spec_name)
        .ok_or_else(|| anyhow!("No monitor entry for connector {spec_name}"))?;

    let current = monitor
        .1
        .iter()
        .find(|mode| {
            mode.6
                .get("is-current")
                .and_then(|v| bool::try_from(v.try_clone().ok()?).ok())
                .unwrap_or(false)
        })
        .ok_or_else(|| anyhow!("Monitor {spec_name} reports no current mode"))?;

    let scale = if logical.2 > 0.0 { logical.2 } else { 1.0 };
    let area = StageArea {
        x: logical.0,
        y: logical.1,
        width: (f64::from(current.1) / scale).round() as i32,
        height: (f64::from(current.2) / scale).round() as i32,
    };

    debug!(
        connector = %spec_name,
        mode = %current.0,
        scale,
        "Resolved stage area {}x{} at ({},{})",
        area.width, area.height, area.x, area.y
    );

    Ok(area)
}

/// Whether the area a stream was created for still matches the compositor.
///
/// An area stream fixes its rectangle when it is created and Mutter never
/// updates it, so a resolution, scale or layout change on the host leaves the
/// stream capturing the wrong region with no error anywhere. A monitor stream
/// follows its monitor and needs none of this.
///
/// Returns the current area when it differs, so the caller can log what moved
/// and rebuild against it. Returns `None` when it still matches, and an error
/// when the compositor cannot be asked, which the caller should treat as "no
/// evidence of change" rather than as a reason to tear a session down.
pub async fn area_moved(
    connection: &zbus::Connection,
    connector: &str,
    created_for: StageArea,
) -> Result<Option<StageArea>> {
    let current = stage_area_for_connector(connection, Some(connector)).await?;
    Ok((current != created_for).then_some(current))
}

/// Number of logical monitors the compositor is currently driving.
///
/// Area capture stands in for exactly one monitor, so the multi-monitor case
/// has to be recognised before choosing it rather than discovered afterwards.
pub async fn logical_monitor_count(connection: &zbus::Connection) -> Result<usize> {
    let proxy = zbus::Proxy::new(
        connection,
        "org.gnome.Mutter.DisplayConfig",
        "/org/gnome/Mutter/DisplayConfig",
        "org.gnome.Mutter.DisplayConfig",
    )
    .await
    .context("Failed to create DisplayConfig proxy")?;

    let reply = proxy
        .call_method("GetCurrentState", &())
        .await
        .context("Failed to call GetCurrentState")?;
    let (_serial, _monitors, logical_monitors, _props): CurrentState =
        reply
            .body()
            .deserialize()
            .context("Failed to deserialize GetCurrentState")?;

    Ok(logical_monitors.len())
}
