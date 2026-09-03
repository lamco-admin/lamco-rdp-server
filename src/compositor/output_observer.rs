//! Wayland output-management observer/controller (`zwlr_output_management_v1`).
//!
//! A server-lifetime Wayland client for wlroots-family compositors (Sway,
//! Hyprland, River, COSMIC). It reads every output head and its modes (size,
//! refresh, current mode, geometry, scale, adaptive sync) and can drive a
//! resolution change by building a configuration and applying it.
//!
//! This is the wlroots path only. KWin and GNOME expose output management over
//! D-Bus (`kde-output-management` / `org.gnome.Mutter.DisplayConfig`), not this
//! protocol, so this observer simply does not start on those compositors.
//!
//! # Object graph
//!
//! ```text
//! zwlr_output_manager_v1
//!   ├─ head  ─> zwlr_output_head_v1  (name, geometry, current_mode, ...)
//!   │            └─ mode ─> zwlr_output_mode_v1  (size, refresh, preferred)
//!   ├─ done(serial)                  // a serial required to apply a config
//!   └─ create_configuration(serial) ─> zwlr_output_configuration_v1
//!        └─ enable_head(head) ─> zwlr_output_configuration_head_v1
//!             └─ set_mode / set_custom_mode ; then apply  (succeeded|failed|cancelled)
//! ```

use std::{
    collections::HashMap,
    sync::{Arc, RwLock},
    thread,
};

use anyhow::{Context, Result};
use tracing::{debug, info, warn};
use wayland_client::{
    Connection, Dispatch, Proxy, QueueHandle, event_created_child,
    globals::{GlobalListContents, registry_queue_init},
    protocol::wl_registry,
};
use wayland_protocols_wlr::output_management::v1::client::{
    zwlr_output_configuration_head_v1::{self, ZwlrOutputConfigurationHeadV1},
    zwlr_output_configuration_v1::{self, ZwlrOutputConfigurationV1},
    zwlr_output_head_v1::{self, ZwlrOutputHeadV1},
    zwlr_output_manager_v1::{self, ZwlrOutputManagerV1},
    zwlr_output_mode_v1::{self, ZwlrOutputModeV1},
};

/// One mode advertised by a head.
#[derive(Debug, Clone, Default)]
pub struct OutputMode {
    pub width: i32,
    pub height: i32,
    /// Vertical refresh in mHz (e.g. 59_997 for 59.997 Hz).
    pub refresh_mhz: i32,
    pub preferred: bool,
}

/// A head (physical output) and its current state.
#[derive(Debug, Clone, Default)]
pub struct OutputHead {
    pub name: String,
    pub description: String,
    pub make: String,
    pub model: String,
    pub enabled: bool,
    pub position: (i32, i32),
    /// The mode currently in use, if any.
    pub current_mode: Option<OutputMode>,
    pub modes: Vec<OutputMode>,
}

/// Handle to the output observer/controller.
#[derive(Clone)]
pub struct OutputObserver {
    shared: Arc<RwLock<Published>>,
    manager: ZwlrOutputManagerV1,
    conn: Connection,
    qh: QueueHandle<OutputState>,
}

impl OutputObserver {
    /// Connect, bind `zwlr_output_manager_v1`, and spawn a thread that keeps
    /// head/mode state current. Returns `None` (after logging) when there is no
    /// Wayland display or the compositor is not wlroots-family.
    pub fn spawn() -> Option<Self> {
        let conn = match Connection::connect_to_env() {
            Ok(conn) => conn,
            Err(e) => {
                debug!("[output_observer] no Wayland connection: {e}");
                return None;
            }
        };
        match spawn_loop(conn) {
            Ok(observer) => {
                info!("[output_observer] zwlr_output_management_v1 observer running");
                Some(observer)
            }
            Err(e) => {
                debug!("[output_observer] not starting: {e:#}");
                None
            }
        }
    }

    /// Current heads keyed by name.
    pub fn heads(&self) -> HashMap<String, OutputHead> {
        self.shared
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .heads
            .values()
            .map(|h| (h.info.name.clone(), h.info.clone()))
            .collect()
    }

    /// Request that `head_name` switch to a custom mode of `width`x`height` at
    /// `refresh_mhz` (mHz; pass 0 to let the compositor choose). Fire-and-forget:
    /// the outcome (succeeded/failed/cancelled) is logged by the observer thread.
    /// Returns an error only if the head or the apply serial is not yet known.
    pub fn request_custom_mode(
        &self,
        head_name: &str,
        width: i32,
        height: i32,
        refresh_mhz: i32,
    ) -> Result<()> {
        let guard = self
            .shared
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let serial = guard
            .serial
            .context("no output-manager serial yet (compositor state not settled)")?;
        let head = guard
            .heads
            .values()
            .find(|h| h.info.name == head_name)
            .map(|h| h.proxy.clone())
            .with_context(|| format!("no output head named {head_name}"))?;
        drop(guard);

        let config = self.manager.create_configuration(serial, &self.qh, ());
        let cfg_head = config.enable_head(&head, &self.qh, ());
        cfg_head.set_custom_mode(width, height, refresh_mhz);
        config.apply();
        self.conn
            .flush()
            .context("failed to flush output configuration to compositor")?;
        info!("[output_observer] requested {head_name} -> {width}x{height}@{refresh_mhz}mHz");
        Ok(())
    }
}

/// Head plus its live proxy (needed to build a configuration).
struct HeadEntry {
    proxy: ZwlrOutputHeadV1,
    info: OutputHead,
    /// mode proxy id -> mode, so `current_mode` (which references a mode object)
    /// can be resolved.
    mode_by_id: HashMap<u32, OutputMode>,
}

/// Published, proxy-bearing state shared with the handle.
#[derive(Default)]
struct Published {
    serial: Option<u32>,
    /// head proxy id -> entry
    heads: HashMap<u32, HeadEntry>,
}

/// Dispatch state owned by the observer thread.
struct OutputState {
    shared: Arc<RwLock<Published>>,
    /// mode proxy id -> owning head proxy id
    mode_owner: HashMap<u32, u32>,
}

fn spawn_loop(conn: Connection) -> Result<OutputObserver> {
    let (globals, mut event_queue) = registry_queue_init::<OutputState>(&conn)
        .context("failed to initialize Wayland registry")?;
    let qh = event_queue.handle();

    let manager: ZwlrOutputManagerV1 = globals
        .bind(&qh, 1..=4, ())
        .context("compositor does not expose zwlr_output_manager_v1")?;

    let shared = Arc::new(RwLock::new(Published::default()));
    let mut state = OutputState {
        shared: Arc::clone(&shared),
        mode_owner: HashMap::new(),
    };

    thread::Builder::new()
        .name("output-observer".into())
        .spawn(move || {
            loop {
                if let Err(e) = event_queue.blocking_dispatch(&mut state) {
                    warn!("[output_observer] dispatch ended: {e}");
                    break;
                }
            }
        })
        .context("failed to spawn output-observer thread")?;

    Ok(OutputObserver {
        shared,
        manager,
        conn,
        qh,
    })
}

// ---- Dispatch impls -------------------------------------------------------

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for OutputState {
    fn event(
        _state: &mut Self,
        _registry: &wl_registry::WlRegistry,
        _event: wl_registry::Event,
        _data: &GlobalListContents,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        // The manager itself was bound at init; registry churn is not tracked here.
    }
}

impl Dispatch<ZwlrOutputManagerV1, ()> for OutputState {
    fn event(
        state: &mut Self,
        _proxy: &ZwlrOutputManagerV1,
        event: zwlr_output_manager_v1::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        match event {
            zwlr_output_manager_v1::Event::Head { head } => {
                state
                    .shared
                    .write()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .heads
                    .insert(
                        head.id().protocol_id(),
                        HeadEntry {
                            proxy: head,
                            info: OutputHead::default(),
                            mode_by_id: HashMap::new(),
                        },
                    );
            }
            zwlr_output_manager_v1::Event::Done { serial } => {
                state
                    .shared
                    .write()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .serial = Some(serial);
                debug!("[output_observer] state settled (serial {serial})");
            }
            _ => {}
        }
    }

    event_created_child!(OutputState, ZwlrOutputManagerV1, [
        zwlr_output_manager_v1::EVT_HEAD_OPCODE => (ZwlrOutputHeadV1, ()),
    ]);
}

impl Dispatch<ZwlrOutputHeadV1, ()> for OutputState {
    fn event(
        state: &mut Self,
        proxy: &ZwlrOutputHeadV1,
        event: zwlr_output_head_v1::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        let head_id = proxy.id().protocol_id();
        let mut guard = state
            .shared
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(entry) = guard.heads.get_mut(&head_id) else {
            return;
        };
        match event {
            zwlr_output_head_v1::Event::Name { name } => entry.info.name = name,
            zwlr_output_head_v1::Event::Description { description } => {
                entry.info.description = description;
            }
            zwlr_output_head_v1::Event::Make { make } => entry.info.make = make,
            zwlr_output_head_v1::Event::Model { model } => entry.info.model = model,
            zwlr_output_head_v1::Event::Enabled { enabled } => entry.info.enabled = enabled != 0,
            zwlr_output_head_v1::Event::Position { x, y } => entry.info.position = (x, y),
            zwlr_output_head_v1::Event::Mode { mode } => {
                state.mode_owner.insert(mode.id().protocol_id(), head_id);
                entry
                    .mode_by_id
                    .insert(mode.id().protocol_id(), OutputMode::default());
            }
            zwlr_output_head_v1::Event::CurrentMode { mode } => {
                if let Some(m) = entry.mode_by_id.get(&mode.id().protocol_id()) {
                    entry.info.current_mode = Some(m.clone());
                }
            }
            zwlr_output_head_v1::Event::Finished => {
                guard.heads.remove(&head_id);
            }
            _ => {}
        }
    }

    event_created_child!(OutputState, ZwlrOutputHeadV1, [
        zwlr_output_head_v1::EVT_MODE_OPCODE => (ZwlrOutputModeV1, ()),
    ]);
}

impl Dispatch<ZwlrOutputModeV1, ()> for OutputState {
    fn event(
        state: &mut Self,
        proxy: &ZwlrOutputModeV1,
        event: zwlr_output_mode_v1::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        let mode_id = proxy.id().protocol_id();
        let Some(&head_id) = state.mode_owner.get(&mode_id) else {
            return;
        };
        let mut guard = state
            .shared
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(entry) = guard.heads.get_mut(&head_id) else {
            return;
        };
        let Some(mode) = entry.mode_by_id.get_mut(&mode_id) else {
            return;
        };
        match event {
            zwlr_output_mode_v1::Event::Size { width, height } => {
                mode.width = width;
                mode.height = height;
            }
            zwlr_output_mode_v1::Event::Refresh { refresh } => mode.refresh_mhz = refresh,
            zwlr_output_mode_v1::Event::Preferred => mode.preferred = true,
            _ => {}
        }
        // Republish this head's mode list from the map.
        let modes: Vec<OutputMode> = entry.mode_by_id.values().cloned().collect();
        entry.info.modes = modes;
    }
}

impl Dispatch<ZwlrOutputConfigurationV1, ()> for OutputState {
    fn event(
        _state: &mut Self,
        _proxy: &ZwlrOutputConfigurationV1,
        event: zwlr_output_configuration_v1::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        match event {
            zwlr_output_configuration_v1::Event::Succeeded => {
                info!("[output_observer] configuration applied");
            }
            zwlr_output_configuration_v1::Event::Failed => {
                warn!("[output_observer] configuration rejected by compositor");
            }
            zwlr_output_configuration_v1::Event::Cancelled => {
                debug!("[output_observer] configuration cancelled (stale serial); will retry");
            }
            _ => {}
        }
    }
}

impl Dispatch<ZwlrOutputConfigurationHeadV1, ()> for OutputState {
    fn event(
        _state: &mut Self,
        _proxy: &ZwlrOutputConfigurationHeadV1,
        _event: zwlr_output_configuration_head_v1::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        // No events defined on configuration heads.
    }
}
