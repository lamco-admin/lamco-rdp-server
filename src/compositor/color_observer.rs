//! Wayland color-management observer (`wp_color_management_v1`, read side).
//!
//! A server-lifetime Wayland client that reads each `wl_output`'s image
//! description: color primaries, transfer function, luminances, and mastering
//! display / HDR metadata (max content light level, max frame-average light
//! level). The encode path consumes this to know whether a source output is HDR
//! and to drive tone-mapping.
//!
//! We capture through PipeWire and own no `wl_surface`, so only the read side of
//! color-management applies. The surface-set, ICC/parametric creator, and
//! feedback interfaces target a surface the client renders, which we do not
//! have, so they are intentionally not wired up here.
//!
//! # Object graph
//!
//! ```text
//! wp_color_manager_v1
//!   └─ get_output(wl_output) ─> wp_color_management_output_v1
//!        └─ get_image_description() ─> wp_image_description_v1  (ready | failed)
//!             └─ get_information() ─> wp_image_description_info_v1
//!                  └─ primaries / tf_named / luminances / target_max_cll ... / done
//! ```

use std::{
    collections::HashMap,
    sync::{Arc, RwLock},
    thread::{self, JoinHandle},
};

use anyhow::{Context, Result};
use tracing::{debug, info, warn};
use wayland_client::{
    Connection, Dispatch, Proxy, QueueHandle, WEnum,
    globals::{GlobalListContents, registry_queue_init},
    protocol::{wl_output::WlOutput, wl_registry},
};
use wayland_protocols::wp::color_management::v1::client::{
    wp_color_management_output_v1::{self, WpColorManagementOutputV1},
    wp_color_manager_v1::{
        self, Primaries, TransferFunction as WlTransferFunction, WpColorManagerV1,
    },
    wp_image_description_info_v1::{self, WpImageDescriptionInfoV1},
    wp_image_description_v1::{self, WpImageDescriptionV1},
};

/// Transfer function of an output, distilled to what the encoder cares about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OutputTransfer {
    #[default]
    Unknown,
    /// Standard dynamic range (sRGB, BT.1886, gamma 2.2, and similar).
    Sdr,
    /// HDR, SMPTE ST 2084 (PQ).
    Pq,
    /// HDR, Hybrid Log-Gamma.
    Hlg,
}

/// CIE 1931 xy chromaticity, in the protocol's fixed-point units (value / 1e6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Chromaticity {
    pub x: i32,
    pub y: i32,
}

/// The color state of one output, as read from its image description.
#[derive(Debug, Clone, Default)]
pub struct OutputColorState {
    pub transfer: OutputTransfer,
    /// Named primaries when the compositor reported them (e.g. bt2020, srgb).
    pub primaries_named: Option<Primaries>,
    /// Explicit primaries chromaticities: [red, green, blue, white].
    pub primaries: Option<[Chromaticity; 4]>,
    /// Min / max / reference luminance in the protocol's units (0.0001 cd/m² for
    /// min, 1 cd/m² for max and reference).
    pub luminances: Option<(u32, u32, u32)>,
    /// Mastering display max content light level (cd/m²).
    pub max_cll: Option<u32>,
    /// Mastering display max frame-average light level (cd/m²).
    pub max_fall: Option<u32>,
}

impl OutputColorState {
    /// True when the output signals a high-dynamic-range transfer function.
    pub fn is_hdr(&self) -> bool {
        matches!(self.transfer, OutputTransfer::Pq | OutputTransfer::Hlg)
    }

    /// True when the output signals the BT.2020 wide gamut.
    pub fn is_wide_gamut(&self) -> bool {
        self.primaries_named == Some(Primaries::Bt2020)
    }
}

/// Handle to the color-management observer.
///
/// Holds the shared, live-updated map of per-output color state. Cloning shares
/// the same underlying map.
#[derive(Clone)]
pub struct ColorObserver {
    outputs: Arc<RwLock<HashMap<u32, OutputColorState>>>,
}

impl ColorObserver {
    /// Connect to the compositor, bind `wp_color_manager_v1`, and spawn a thread
    /// that keeps per-output color state current. Returns `None` (after logging)
    /// when there is no Wayland display or the compositor lacks color-management,
    /// so callers can degrade gracefully.
    pub fn spawn() -> Option<Self> {
        let conn = match Connection::connect_to_env() {
            Ok(conn) => conn,
            Err(e) => {
                debug!("[color_observer] no Wayland connection: {e}");
                return None;
            }
        };

        let outputs = Arc::new(RwLock::new(HashMap::new()));
        let shared = Arc::clone(&outputs);

        match spawn_loop(conn, shared) {
            Ok(_handle) => {
                info!("[color_observer] wp_color_management_v1 observer running");
                Some(Self { outputs })
            }
            Err(e) => {
                debug!("[color_observer] not starting: {e:#}");
                None
            }
        }
    }

    /// A snapshot of the current per-output color state, keyed by the output's
    /// registry name.
    pub fn snapshot(&self) -> HashMap<u32, OutputColorState> {
        self.outputs
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// True if any observed output is currently in an HDR mode.
    pub fn any_hdr(&self) -> bool {
        self.outputs
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .values()
            .any(OutputColorState::is_hdr)
    }

    /// The transfer function and peak luminance (cd/m²) of the first HDR output,
    /// if any. Peak falls back to mastering max-CLL, then max luminance, then a
    /// 1000 cd/m² default.
    pub fn hdr_source(&self) -> Option<(OutputTransfer, f32)> {
        self.outputs
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .values()
            .find(|s| s.is_hdr())
            .map(|s| {
                let peak = s
                    .max_cll
                    .map(|c| c as f32)
                    .or_else(|| s.luminances.map(|(_, max, _)| max as f32))
                    .unwrap_or(1000.0);
                (s.transfer, peak)
            })
    }
}

/// Per-output objects and the in-progress readout being accumulated from
/// `wp_image_description_info_v1` events before it is committed on `done`.
struct OutputEntry {
    #[expect(dead_code, reason = "kept alive so the wl_output proxy stays bound")]
    output: WlOutput,
    color_output: WpColorManagementOutputV1,
    pending: OutputColorState,
}

/// Dispatch state owned by the observer thread.
struct ColorState {
    manager: WpColorManagerV1,
    qh: QueueHandle<ColorState>,
    /// output registry name -> its objects + in-progress readout
    entries: HashMap<u32, OutputEntry>,
    /// image-description / info object id -> owning output registry name
    routing: HashMap<u32, u32>,
    /// committed results, shared with the ColorObserver handle
    results: Arc<RwLock<HashMap<u32, OutputColorState>>>,
}

impl ColorState {
    /// Begin (or refresh) reading the image description for one output.
    fn start_output(&mut self, name: u32) {
        let Some(entry) = self.entries.get(&name) else {
            return;
        };
        let desc = entry.color_output.get_image_description(&self.qh, name);
        self.routing.insert(desc.id().protocol_id(), name);
    }
}

fn spawn_loop(
    conn: Connection,
    results: Arc<RwLock<HashMap<u32, OutputColorState>>>,
) -> Result<JoinHandle<()>> {
    let (globals, mut event_queue) = registry_queue_init::<ColorState>(&conn)
        .context("failed to initialize Wayland registry")?;
    let qh = event_queue.handle();

    let manager: WpColorManagerV1 = globals
        .bind(&qh, 1..=1, ())
        .context("compositor does not expose wp_color_manager_v1")?;

    // Bind every wl_output present at startup and kick off a read for each.
    let mut state = ColorState {
        manager,
        qh: qh.clone(),
        entries: HashMap::new(),
        routing: HashMap::new(),
        results,
    };

    let output_names: Vec<(u32, u32)> = globals.contents().with_list(|list| {
        list.iter()
            .filter(|g| g.interface == "wl_output")
            .map(|g| (g.name, g.version.min(4)))
            .collect()
    });

    for (name, version) in output_names {
        bind_output(&globals, &mut state, name, version);
    }

    let handle = thread::Builder::new()
        .name("color-observer".into())
        .spawn(move || {
            loop {
                if let Err(e) = event_queue.blocking_dispatch(&mut state) {
                    warn!("[color_observer] dispatch ended: {e}");
                    break;
                }
            }
        })
        .context("failed to spawn color-observer thread")?;

    Ok(handle)
}

fn bind_output(
    globals: &wayland_client::globals::GlobalList,
    state: &mut ColorState,
    name: u32,
    version: u32,
) {
    let output: WlOutput = globals.registry().bind(name, version, &state.qh, name);
    let color_output = state.manager.get_output(&output, &state.qh, name);
    state.entries.insert(
        name,
        OutputEntry {
            output,
            color_output,
            pending: OutputColorState::default(),
        },
    );
    state.start_output(name);
}

// ---- Dispatch impls -------------------------------------------------------

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for ColorState {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _data: &GlobalListContents,
        _conn: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        match event {
            wl_registry::Event::Global {
                name,
                interface,
                version,
            } if interface == "wl_output" => {
                let output: WlOutput = registry.bind(name, version.min(4), qh, name);
                let color_output = state.manager.get_output(&output, qh, name);
                state.entries.insert(
                    name,
                    OutputEntry {
                        output,
                        color_output,
                        pending: OutputColorState::default(),
                    },
                );
                state.start_output(name);
            }
            wl_registry::Event::GlobalRemove { name } => {
                if state.entries.remove(&name).is_none() {
                    return;
                }
                state
                    .results
                    .write()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .remove(&name);
                state.routing.retain(|_, owner| *owner != name);
                debug!("[color_observer] output {name} removed");
            }
            _ => {}
        }
    }
}

impl Dispatch<WpColorManagerV1, ()> for ColorState {
    fn event(
        _state: &mut Self,
        _proxy: &WpColorManagerV1,
        event: wp_color_manager_v1::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        // The manager advertises which intents/features/named values it supports.
        // We read outputs regardless, so these are informational only.
        if let wp_color_manager_v1::Event::Done = event {
            debug!("[color_observer] manager finished advertising capabilities");
        }
    }
}

impl Dispatch<WlOutput, u32> for ColorState {
    fn event(
        _state: &mut Self,
        _proxy: &WlOutput,
        _event: wayland_client::protocol::wl_output::Event,
        _data: &u32,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        // Geometry/mode belong to the output observer; ignored here.
    }
}

impl Dispatch<WpColorManagementOutputV1, u32> for ColorState {
    fn event(
        state: &mut Self,
        _proxy: &WpColorManagementOutputV1,
        event: wp_color_management_output_v1::Event,
        data: &u32,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        // The output's image description changed: re-read it.
        if let wp_color_management_output_v1::Event::ImageDescriptionChanged = event {
            debug!("[color_observer] output {data} image description changed");
            if let Some(entry) = state.entries.get_mut(data) {
                entry.pending = OutputColorState::default();
            }
            state.start_output(*data);
        }
    }
}

impl Dispatch<WpImageDescriptionV1, u32> for ColorState {
    fn event(
        state: &mut Self,
        proxy: &WpImageDescriptionV1,
        event: wp_image_description_v1::Event,
        data: &u32,
        _conn: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        match event {
            wp_image_description_v1::Event::Ready { .. } => {
                let info = proxy.get_information(qh, *data);
                state.routing.insert(info.id().protocol_id(), *data);
            }
            wp_image_description_v1::Event::Failed { .. } => {
                debug!("[color_observer] output {data} image description failed");
            }
            _ => {}
        }
    }
}

impl Dispatch<WpImageDescriptionInfoV1, u32> for ColorState {
    fn event(
        state: &mut Self,
        _proxy: &WpImageDescriptionInfoV1,
        event: wp_image_description_info_v1::Event,
        data: &u32,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        let Some(entry) = state.entries.get_mut(data) else {
            return;
        };
        let p = &mut entry.pending;
        match event {
            wp_image_description_info_v1::Event::TfNamed { tf } => {
                p.transfer = match tf {
                    WEnum::Value(WlTransferFunction::St2084Pq) => OutputTransfer::Pq,
                    WEnum::Value(WlTransferFunction::Hlg) => OutputTransfer::Hlg,
                    WEnum::Value(_) => OutputTransfer::Sdr,
                    WEnum::Unknown(_) => OutputTransfer::Unknown,
                };
            }
            wp_image_description_info_v1::Event::PrimariesNamed {
                primaries: WEnum::Value(named),
            } => {
                p.primaries_named = Some(named);
            }
            wp_image_description_info_v1::Event::Primaries {
                r_x,
                r_y,
                g_x,
                g_y,
                b_x,
                b_y,
                w_x,
                w_y,
            } => {
                p.primaries = Some([
                    Chromaticity { x: r_x, y: r_y },
                    Chromaticity { x: g_x, y: g_y },
                    Chromaticity { x: b_x, y: b_y },
                    Chromaticity { x: w_x, y: w_y },
                ]);
            }
            wp_image_description_info_v1::Event::Luminances {
                min_lum,
                max_lum,
                reference_lum,
            } => {
                p.luminances = Some((min_lum, max_lum, reference_lum));
            }
            wp_image_description_info_v1::Event::TargetMaxCll { max_cll } => {
                p.max_cll = Some(max_cll);
            }
            wp_image_description_info_v1::Event::TargetMaxFall { max_fall } => {
                p.max_fall = Some(max_fall);
            }
            wp_image_description_info_v1::Event::Done => {
                let committed = entry.pending.clone();
                let hdr = committed.is_hdr();
                state
                    .results
                    .write()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .insert(*data, committed);
                info!(
                    "[color_observer] output {data}: transfer={:?} hdr={hdr}",
                    entry.pending.transfer
                );
            }
            _ => {}
        }
    }
}
