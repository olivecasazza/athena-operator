//! Stable panel identity catalogs for the console workspaces.

use std::rc::Rc;

use panel_kit::{PanelKind, PanelWin};
use panel_kit_core::PanelCatalog;

/// Build the persistence/chrome catalog from one workspace's complete defaults.
pub(super) fn panel_catalog<K: PanelKind>(panels: &[PanelWin<K>]) -> Rc<PanelCatalog<K>> {
    Rc::new(
        PanelCatalog::from_panel_kind_layout(panels)
            .expect("Athena panel enums must serialize as stable string IDs"),
    )
}
