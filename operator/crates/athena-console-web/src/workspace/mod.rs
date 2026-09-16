//! Host-owned panel-kit composition for the research and admin workspaces.

mod catalog;
mod events;
mod state;

use dioxus::prelude::*;
use panel_kit_core::frame::{FrameStatus, Placement, ProjectedFrame};
use panel_kit_core::reducer::WorkspaceEvent;
use panel_kit_core::{PanelCatalog, PanelKind};

pub(crate) use events::{
    handle_key, handle_pointer_move, handle_pointer_up, handle_wheel, workspace_event_handler,
};
pub(crate) use state::{
    mount_viewport_observer, project_workspace, use_panel_workspace, workspace_area_class,
};

/// Compose projected panel shells, chrome, bodies, controls, and resize grips.
pub(crate) fn workspace_contents<K, F>(
    frame: &ProjectedFrame<'_, K>,
    catalog: &PanelCatalog<K>,
    emit: EventHandler<WorkspaceEvent<K>>,
    body: F,
) -> Element
where
    K: PanelKind,
    F: Fn(K, bool) -> Element,
{
    if frame.status == FrameStatus::TooSmall {
        return rsx! {
            p { class: "err", "The viewport is too small for the panel workspace." }
        };
    }

    rsx! {
        for panel in frame.panels.iter().copied() {
            if let Some(meta) = catalog.get(panel.key) {
                {
                    let panel_class = format!("panel-{}", meta.slug);
                    let maximized = matches!(panel.placement, Placement::Maximized);
                    let panel_body = body(panel.key, maximized);
                    panel_kit::widgets::panel::panel_shell(panel, Some(&panel_class), rsx! {
                        {panel_kit::widgets::panel::panel_chrome_with_events(
                            panel,
                            meta,
                            emit,
                            Some(panel_kit::widgets::panel::traffic_lights(panel, emit)),
                            None,
                        )}
                        {panel_kit::widgets::panel::panel_body(panel_body)}
                        {panel_kit::widgets::panel::resize_grip(panel, emit)}
                    })
                }
            }
        }
    }
}
