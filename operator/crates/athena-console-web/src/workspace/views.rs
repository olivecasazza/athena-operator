//! Named views: preset panel combinations + layout + mode, one stored layout
//! per view, switched from the top bar.
//!
//! Built on panel-kit's `views` registry and key scheme: the registry lives at
//! `{base}:views`, each view's layout at `{base}:view:{name}`. Presets are
//! code-defined (which panels are visible, their tile spans and order); any
//! user edit persists into the active view, and "Reset" returns it to its
//! preset. Custom views start from the current snapshot.

use panel_kit::store::LocalStorageLayoutStore;
use panel_kit::{PanelKind, PanelWin};
use panel_kit_core::persist::persist_snapshot;
use panel_kit_core::reducer::Snapshot;
use panel_kit_core::views::{SavedViews, view_layout_key, views_registry_key};
use panel_kit_core::{Mode, WinState};

/// One code-defined view: visible panels in tiling order with their spans.
pub(crate) struct ViewPreset<K: 'static> {
    pub name: &'static str,
    pub mode: Mode,
    /// (panel, tile_w, tile_h). Every panel not listed is minimized.
    pub panels: &'static [(K, u8, u8)],
}

/// Views configuration for one workspace.
pub(crate) struct ViewsSpec<K: 'static> {
    pub base: &'static str,
    pub presets: &'static [ViewPreset<K>],
}

impl<K: PanelKind> ViewsSpec<K> {
    pub fn preset(&self, name: &str) -> Option<&ViewPreset<K>> {
        self.presets.iter().find(|p| p.name == name)
    }

    pub fn is_preset(&self, name: &str) -> bool {
        self.preset(name).is_some()
    }

    /// Initial registry: all presets, first active.
    pub fn fresh_registry(&self) -> SavedViews {
        let names: Vec<&str> = self.presets.iter().map(|p| p.name).collect();
        SavedViews::new(&names)
    }

    /// Re-add any preset missing from a stored registry, keeping custom views.
    pub fn sanitize(&self, registry: SavedViews) -> SavedViews {
        let mut registry = registry.sanitize();
        for p in self.presets {
            if !registry.views.iter().any(|v| v == p.name) {
                let _ = registry.add(p.name);
            }
        }
        registry
    }

    pub fn layout_key(&self, view: &str) -> String {
        view_layout_key(self.base, view)
    }

    pub fn registry_key(&self) -> String {
        views_registry_key(self.base)
    }

    /// Panels for `view`: the preset's panels first (tiling order), then every
    /// other default panel minimized. Custom/unknown views use the first preset.
    pub fn panels_for(&self, view: &str, defaults: &[PanelWin<K>]) -> (Vec<PanelWin<K>>, Mode) {
        let preset = self.preset(view).unwrap_or(&self.presets[0]);
        let mut out = Vec::with_capacity(defaults.len());
        for &(kind, w, h) in preset.panels {
            if let Some(d) = defaults.iter().find(|d| d.kind == kind) {
                let mut win = d.with_tile(w, h);
                win.state = WinState::Floating;
                out.push(win);
            }
        }
        for d in defaults {
            if !preset.panels.iter().any(|&(k, _, _)| k == d.kind) {
                let mut win = *d;
                win.state = WinState::Minimized;
                out.push(win);
            }
        }
        (out, preset.mode)
    }
}

#[cfg(target_arch = "wasm32")]
pub(crate) fn load_registry(key: &str) -> Option<SavedViews> {
    let storage = web_sys::window()?.local_storage().ok()??;
    let raw = storage.get_item(key).ok()??;
    serde_json::from_str(&raw).ok()
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn load_registry(_key: &str) -> Option<SavedViews> {
    None
}

#[cfg(target_arch = "wasm32")]
pub(crate) fn save_registry(key: &str, registry: &SavedViews) {
    let Some(storage) = web_sys::window().and_then(|w| w.local_storage().ok().flatten()) else {
        return;
    };
    if let Ok(raw) = serde_json::to_string(registry) {
        let _ = storage.set_item(key, &raw);
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn save_registry(_key: &str, _registry: &SavedViews) {}

#[cfg(target_arch = "wasm32")]
pub(crate) fn forget_layout(key: &str) {
    if let Some(storage) = web_sys::window().and_then(|w| w.local_storage().ok().flatten()) {
        let _ = storage.remove_item(key);
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn forget_layout(_key: &str) {}

/// Persist `snapshot` as `view`'s stored layout.
pub(crate) fn store_view<K: PanelKind>(
    spec: &ViewsSpec<K>,
    view: &str,
    snapshot: &Snapshot<K>,
    catalog: &panel_kit_core::PanelCatalog<K>,
) {
    let key = spec.layout_key(view);
    if let Err(error) = persist_snapshot(
        &LocalStorageLayoutStore::new(key.as_str()),
        snapshot,
        catalog,
    ) {
        super::state::log_layout_error("save view layout", &key, &error);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use panel_kit::LayoutBuilder;
    use serde::{Deserialize, Serialize};

    #[derive(Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Debug)]
    enum P {
        A,
        B,
        C,
    }

    impl PanelKind for P {
        fn title(self) -> &'static str {
            "p"
        }
    }

    static SPEC: ViewsSpec<P> = ViewsSpec {
        base: "t",
        presets: &[
            ViewPreset {
                name: "One",
                mode: Mode::Tiling,
                panels: &[(P::B, 3, 4), (P::A, 1, 2)],
            },
            ViewPreset {
                name: "Two",
                mode: Mode::Floating,
                panels: &[(P::C, 4, 4)],
            },
        ],
    };

    fn defaults() -> Vec<PanelWin<P>> {
        let mut b = LayoutBuilder::new();
        vec![
            b.at(P::A, 0.0, 0.0, 10.0, 10.0),
            b.at(P::B, 0.0, 0.0, 10.0, 10.0),
            b.at(P::C, 0.0, 0.0, 10.0, 10.0),
        ]
    }

    #[test]
    fn preset_orders_visible_panels_and_minimizes_the_rest() {
        let (panels, mode) = SPEC.panels_for("One", &defaults());
        assert_eq!(mode, Mode::Tiling);
        let order: Vec<(P, WinState, u8, u8)> = panels
            .iter()
            .map(|p| (p.kind, p.state, p.tile_w, p.tile_h))
            .collect();
        assert_eq!(order[0], (P::B, WinState::Floating, 3, 4));
        assert_eq!(order[1], (P::A, WinState::Floating, 1, 2));
        assert_eq!((order[2].0, order[2].1), (P::C, WinState::Minimized));
    }

    #[test]
    fn custom_view_falls_back_to_first_preset() {
        let (panels, _) = SPEC.panels_for("mine", &defaults());
        assert_eq!(panels[0].kind, P::B);
    }

    #[test]
    fn sanitize_readds_presets_and_keeps_custom_views() {
        let stored = SavedViews {
            views: vec!["mine".into()],
            active: "mine".into(),
        };
        let r = SPEC.sanitize(stored);
        assert!(r.views.iter().any(|v| v == "One"));
        assert!(r.views.iter().any(|v| v == "Two"));
        assert!(r.views.iter().any(|v| v == "mine"));
        assert_eq!(r.active, "mine");
    }
}
