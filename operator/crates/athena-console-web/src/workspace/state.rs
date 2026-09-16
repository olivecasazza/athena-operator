//! Snapshot, projection, persistence, and viewport lifecycle for one workspace.

use std::cell::RefCell;
use std::rc::Rc;

use dioxus::prelude::*;
use panel_kit::store::LocalStorageLayoutStore;
use panel_kit::surface::{observe_viewport, surface_profile, viewport_size};
use panel_kit::{PanelKind, PanelWin};
use panel_kit_core::frame::{
    project_into, ChromeProjectionInput, Placement, ProjectedFrame, ProjectionBuffer,
    ProjectionInput, TileFillOrder, TileLayoutMetrics,
};
use panel_kit_core::persist::{restore_snapshot, LayoutError, RestoreContext, SavePolicy};
use panel_kit_core::reducer::{ResizePolicy, Snapshot, Viewport, WorkspaceEvent};
use panel_kit_core::{ChromeMetrics, Clamp, Mode, PanelCatalog, TileMetrics, Units};

use super::catalog::panel_catalog;
use super::events::workspace_event_handler;

/// Host-owned composable state and ports for one Athena workspace.
#[derive(Clone)]
pub(crate) struct PanelWorkspace<K: PanelKind> {
    pub(crate) storage_key: &'static str,
    pub(crate) snapshot: Signal<Snapshot<K>>,
    pub(crate) catalog: Rc<PanelCatalog<K>>,
    pub(crate) scratch: Rc<RefCell<ProjectionBuffer<K>>>,
    pub(super) store: Rc<LocalStorageLayoutStore>,
    pub(super) save_policy: SavePolicy,
    pub(super) tile_min_rows: Rc<Vec<(K, u8)>>,
}

/// Create one host-owned workspace from defaults and a stable localStorage key.
pub(crate) fn use_panel_workspace<K: PanelKind>(
    storage_key: &'static str,
    defaults: fn() -> Vec<PanelWin<K>>,
) -> PanelWorkspace<K> {
    let default_panels = use_hook(move || Rc::new(defaults()));
    let catalog = use_hook({
        let panels = default_panels.clone();
        move || panel_catalog(panels.as_slice())
    });
    let tile_min_rows = use_hook({
        let panels = default_panels.clone();
        move || {
            Rc::new(
                panels
                    .iter()
                    .map(|panel| (panel.kind, panel.tile_h))
                    .collect::<Vec<_>>(),
            )
        }
    });
    let default_snapshot = use_hook({
        let panels = default_panels.clone();
        move || {
            Rc::new(Snapshot::from_defaults(
                panels.as_ref().clone(),
                Mode::Floating,
                current_viewport(),
            ))
        }
    });
    let store = use_hook(move || Rc::new(LocalStorageLayoutStore::new(storage_key)));
    let snapshot = use_signal({
        let catalog = catalog.clone();
        let store = store.clone();
        let defaults = default_snapshot.clone();
        let tile_min_rows = tile_min_rows.clone();
        move || {
            let mut snapshot =
                restore_or_default(storage_key, &store, defaults.as_ref().clone(), &catalog);
            enforce_tile_minimums(&mut snapshot, &tile_min_rows);
            snapshot
        }
    });
    let scratch = use_hook({
        let panel_count = snapshot.peek().panels.len();
        move || {
            Rc::new(RefCell::new(ProjectionBuffer::with_panel_capacity(
                panel_count,
            )))
        }
    });

    PanelWorkspace {
        storage_key,
        snapshot,
        catalog,
        scratch,
        store,
        save_policy: SavePolicy::OnSettle,
        tile_min_rows,
    }
}

/// Subscribe a workspace to browser viewport changes.
pub(crate) fn mount_viewport_observer<K: PanelKind>(workspace: &PanelWorkspace<K>) {
    let emit = workspace_event_handler(workspace);
    let _status = observe_viewport(EventHandler::new(move |size: Viewport| {
        emit.call(WorkspaceEvent::ViewportChanged {
            size,
            policy: ResizePolicy::ScaleFloating,
        });
    }));
}

/// Project the current snapshot into reusable caller-owned scratch.
pub(crate) fn project_workspace<'frame, K: PanelKind>(
    snapshot: &Snapshot<K>,
    scratch: &'frame mut ProjectionBuffer<K>,
) -> ProjectedFrame<'frame, K> {
    let surface = surface_profile(snapshot.viewport.width);
    let chrome = ChromeProjectionInput::full(ChromeMetrics::WEB);
    let tile = TileLayoutMetrics::from_tile_metrics(TileMetrics::WEB, surface)
        .with_fill_order(TileFillOrder::ColumnMajor);

    project_into(
        ProjectionInput {
            snapshot,
            surface,
            chrome: &chrome,
            clamp: &Clamp::WEB,
            tile: &tile,
        },
        scratch,
    )
}

/// CSS class for the `.ws` area that contains projected panels.
pub(crate) fn workspace_area_class<K: PanelKind>(frame: &ProjectedFrame<'_, K>) -> &'static str {
    if frame
        .panels
        .iter()
        .any(|panel| matches!(panel.placement, Placement::Maximized))
    {
        "ws maxed"
    } else if frame.mode == Mode::Tiling {
        "ws tiling"
    } else {
        "ws floating"
    }
}

/// Keep authored tile-height floors after restoring old layouts or resizing.
pub(super) fn enforce_tile_minimums<K: PanelKind>(
    snapshot: &mut Snapshot<K>,
    minimums: &[(K, u8)],
) {
    for panel in &mut snapshot.panels {
        if let Some((_, minimum)) = minimums.iter().find(|(kind, _)| *kind == panel.kind) {
            panel.tile_h = panel.tile_h.max(*minimum);
        }
    }
}

fn current_viewport() -> Viewport {
    let (width, height) = viewport_size();
    Viewport {
        width,
        height,
        units: Units::CssPx,
    }
}

fn restore_or_default<K: PanelKind>(
    storage_key: &str,
    store: &LocalStorageLayoutStore,
    defaults: Snapshot<K>,
    catalog: &PanelCatalog<K>,
) -> Snapshot<K> {
    let context = RestoreContext {
        units: Units::CssPx,
        viewport: (defaults.viewport.width, defaults.viewport.height),
    };

    match restore_snapshot(store, defaults.clone(), catalog, context) {
        Ok(snapshot) => snapshot,
        Err(error) => {
            log_layout_error("restore layout", storage_key, &error);
            defaults
        }
    }
}

pub(super) fn log_layout_error(action: &str, storage_key: &str, error: &LayoutError) {
    eprintln!("panel-kit {action} failed for storage key `{storage_key}`: {error}");
}
