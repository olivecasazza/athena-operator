//! Snapshot, projection, persistence, and viewport lifecycle for one workspace.

use std::cell::RefCell;
use std::rc::Rc;

use dioxus::prelude::*;
use panel_kit::store::LocalStorageLayoutStore;
use panel_kit::surface::{observe_viewport, surface_profile, viewport_size};
use panel_kit::{PanelKind, PanelWin};
use panel_kit_core::frame::{
    ChromeProjectionInput, Placement, ProjectedFrame, ProjectionBuffer, ProjectionInput,
    TileFillOrder, TileLayoutMetrics, project_into,
};
use panel_kit_core::persist::{LayoutError, RestoreContext, SavePolicy, restore_snapshot};
use panel_kit_core::reducer::{ResizePolicy, Snapshot, Viewport, WorkspaceEvent};
use panel_kit_core::{ChromeMetrics, Clamp, Mode, PanelCatalog, TileMetrics, Units};

use super::catalog::panel_catalog;
use super::events::workspace_event_handler;
use super::views::{ViewsSpec, forget_layout, load_registry, save_registry, store_view};
use panel_kit_core::views::SavedViews;

/// Host-owned composable state and ports for one Athena workspace.
#[derive(Clone)]
pub(crate) struct PanelWorkspace<K: PanelKind + 'static> {
    /// localStorage key the active layout persists under (per view when
    /// views are enabled).
    pub(crate) store_key: Signal<String>,
    pub(crate) snapshot: Signal<Snapshot<K>>,
    pub(crate) catalog: Rc<PanelCatalog<K>>,
    pub(crate) scratch: Rc<RefCell<ProjectionBuffer<K>>>,
    pub(super) save_policy: SavePolicy,
    pub(super) tile_min_rows: Rc<Vec<(K, u8)>>,
    pub(crate) views: Option<&'static ViewsSpec<K>>,
    pub(crate) registry: Signal<SavedViews>,
    defaults: Rc<Vec<PanelWin<K>>>,
}

/// Create one host-owned workspace from defaults and a stable localStorage
/// key. With `views`, `storage_key` is ignored: each named view persists
/// under the views key scheme and starts from its preset.
pub(crate) fn use_panel_workspace<K: PanelKind + 'static>(
    storage_key: &'static str,
    defaults: fn() -> Vec<PanelWin<K>>,
    views: Option<&'static ViewsSpec<K>>,
) -> PanelWorkspace<K> {
    let default_panels = use_hook(move || Rc::new(defaults()));
    let catalog = use_hook({
        let panels = default_panels.clone();
        move || panel_catalog(panels.as_slice())
    });
    // Authored heights become tile-row floors for the single-layout
    // workspace. Presets own their spans, so views get no floors.
    let tile_min_rows = use_hook({
        let panels = default_panels.clone();
        move || {
            if views.is_some() {
                return Rc::new(Vec::new());
            }
            Rc::new(
                panels
                    .iter()
                    .map(|panel| (panel.kind, panel.tile_h))
                    .collect::<Vec<_>>(),
            )
        }
    });
    let registry = use_signal(move || match views {
        Some(spec) => {
            let stored = load_registry(&spec.registry_key());
            stored
                .map(|r| spec.sanitize(r))
                .unwrap_or_else(|| spec.fresh_registry())
        }
        None => SavedViews::new(&["Main"]),
    });
    let store_key = use_signal(move || match views {
        Some(spec) => spec.layout_key(&registry.peek().active),
        None => storage_key.to_string(),
    });
    let snapshot = use_signal({
        let catalog = catalog.clone();
        let panels = default_panels.clone();
        let tile_min_rows = tile_min_rows.clone();
        move || {
            let key = store_key.peek().clone();
            let defaults = view_defaults(views, &registry.peek().active, &panels);
            let mut snapshot = restore_or_default(&key, defaults, &catalog);
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
        store_key,
        snapshot,
        catalog,
        scratch,
        save_policy: SavePolicy::OnSettle,
        tile_min_rows,
        views,
        registry,
        defaults: default_panels,
    }
}

/// Default snapshot for `view` (its preset) or the plain defaults.
fn view_defaults<K: PanelKind>(
    views: Option<&ViewsSpec<K>>,
    view: &str,
    panels: &[PanelWin<K>],
) -> Snapshot<K> {
    let (panels, mode) = match views {
        Some(spec) => spec.panels_for(view, panels),
        None => (panels.to_vec(), Mode::Floating),
    };
    Snapshot::from_defaults(panels, mode, current_viewport())
}

impl<K: PanelKind + 'static> PanelWorkspace<K> {
    fn store(&self) -> LocalStorageLayoutStore {
        LocalStorageLayoutStore::new(self.store_key.peek().clone())
    }

    pub(super) fn with_store<R>(&self, f: impl FnOnce(&LocalStorageLayoutStore, &str) -> R) -> R {
        let key = self.store_key.peek().clone();
        f(&self.store(), &key)
    }

    /// Persist the current snapshot as the active view, then load `name`.
    pub(crate) fn switch_view(&self, name: &str) {
        let Some(spec) = self.views else { return };
        let mut registry = self.registry;
        let mut next = registry.peek().clone();
        if next.active == name || next.activate(name).is_err() {
            return;
        }
        store_view(
            spec,
            &registry.peek().active,
            &self.snapshot.peek(),
            &self.catalog,
        );
        save_registry(&spec.registry_key(), &next);
        self.load(spec, &next.active);
        registry.set(next);
    }

    /// Restore the active view to its preset and forget its stored layout.
    pub(crate) fn reset_view(&self) {
        let Some(spec) = self.views else { return };
        let active = self.registry.peek().active.clone();
        forget_layout(&spec.layout_key(&active));
        let mut snapshot = self.snapshot;
        let viewport = snapshot.peek().viewport;
        let mut fresh = view_defaults(Some(spec), &active, &self.defaults);
        fresh.viewport = viewport;
        snapshot.set(fresh);
    }

    /// Create a custom view from the current snapshot and switch to it.
    pub(crate) fn save_view_as(&self, name: &str) -> Result<(), String> {
        let Some(spec) = self.views else {
            return Err("views disabled".into());
        };
        let mut registry = self.registry;
        let mut next = registry.peek().clone();
        next.add(name).map_err(|e| e.to_string())?;
        let name = next.views.last().cloned().unwrap_or_default();
        store_view(
            spec,
            &registry.peek().active,
            &self.snapshot.peek(),
            &self.catalog,
        );
        store_view(spec, &name, &self.snapshot.peek(), &self.catalog);
        next.activate(&name).map_err(|e| e.to_string())?;
        save_registry(&spec.registry_key(), &next);
        let mut store_key = self.store_key;
        store_key.set(spec.layout_key(&name));
        registry.set(next);
        Ok(())
    }

    /// Delete a custom view (presets are permanent).
    pub(crate) fn delete_view(&self, name: &str) {
        let Some(spec) = self.views else { return };
        if spec.is_preset(name) {
            return;
        }
        let mut registry = self.registry;
        let mut next = registry.peek().clone();
        let was_active = next.active == name;
        if next.remove(name).is_err() {
            return;
        }
        forget_layout(&spec.layout_key(name));
        save_registry(&spec.registry_key(), &next);
        if was_active {
            self.load(spec, &next.active);
        }
        registry.set(next);
    }

    fn load(&self, spec: &ViewsSpec<K>, view: &str) {
        let key = spec.layout_key(view);
        let mut store_key = self.store_key;
        store_key.set(key.clone());
        let mut snapshot = self.snapshot;
        let viewport = snapshot.peek().viewport;
        let mut defaults = view_defaults(Some(spec), view, &self.defaults);
        defaults.viewport = viewport;
        let mut next = restore_or_default(&key, defaults, &self.catalog);
        enforce_tile_minimums(&mut next, &self.tile_min_rows);
        snapshot.set(next);
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
        .with_fill_order(TileFillOrder::RowMajor);

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
    defaults: Snapshot<K>,
    catalog: &PanelCatalog<K>,
) -> Snapshot<K> {
    let store = LocalStorageLayoutStore::new(storage_key);
    let context = RestoreContext {
        units: Units::CssPx,
        viewport: (defaults.viewport.width, defaults.viewport.height),
    };

    match restore_snapshot(&store, defaults.clone(), catalog, context) {
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
