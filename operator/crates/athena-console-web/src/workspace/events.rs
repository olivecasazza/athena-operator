//! Browser input translation and reducer/persistence handoff.

use dioxus::events::{KeyboardEvent, PointerEvent as DioxusPointerEvent, WheelEvent};
use dioxus::prelude::*;
use panel_kit::input::{
    clear_selection, keyboard_event, pointer_event, release_pointer, wheel_event,
};
use panel_kit_core::persist::apply_save_decision;
use panel_kit_core::reducer::{reduce, HitTarget, ReduceContext, WorkspaceEvent};
use panel_kit_core::{
    Clamp, CommandStep, FocusContext, PanelKind, PointerButton, PointerEventKind, SnapPolicy,
    TileMetrics,
};

use super::state::{enforce_tile_minimums, log_layout_error, PanelWorkspace};

/// Build an event handler that reduces into one workspace and applies persistence.
pub(crate) fn workspace_event_handler<K: PanelKind>(
    workspace: &PanelWorkspace<K>,
) -> EventHandler<WorkspaceEvent<K>> {
    let workspace = workspace.clone();
    EventHandler::new(move |event| {
        reduce_and_persist_workspace_event(&workspace, event);
    })
}

/// Reduce root keyboard input after focused editors receive first refusal.
pub(crate) fn handle_key<K: PanelKind>(workspace: &PanelWorkspace<K>, event: &KeyboardEvent) {
    let focus = if panel_kit::input::is_editing() {
        FocusContext::TextInput
    } else if let Some(key) = workspace.snapshot.read().focused {
        FocusContext::Panel(key)
    } else {
        FocusContext::Workspace
    };

    let Some(workspace_event) = keyboard_event(event, focus) else {
        return;
    };
    if reduce_and_persist_workspace_event(workspace, workspace_event) {
        event.prevent_default();
    }
}

/// Reduce in-flight workspace pointer motion.
pub(crate) fn handle_pointer_move<K: PanelKind>(
    workspace: &PanelWorkspace<K>,
    event: &DioxusPointerEvent,
) {
    let kind = if workspace.snapshot.read().drag.is_some() {
        PointerEventKind::Drag(PointerButton::Primary)
    } else {
        PointerEventKind::Moved
    };
    reduce_and_persist_workspace_event(workspace, pointer_event(HitTarget::Workspace, event, kind));
}

/// Settle a pointer gesture and persist the settled layout.
pub(crate) fn handle_pointer_up<K: PanelKind>(
    workspace: &PanelWorkspace<K>,
    event: &DioxusPointerEvent,
) {
    release_pointer(event);
    let changed = reduce_and_persist_workspace_event(
        workspace,
        pointer_event(
            HitTarget::Workspace,
            event,
            PointerEventKind::Up(PointerButton::Primary),
        ),
    );
    if changed {
        clear_selection();
    }
}

/// Reduce wheel input after native panel-body scrolling receives first refusal.
pub(crate) fn handle_wheel<K: PanelKind>(workspace: &PanelWorkspace<K>, event: &WheelEvent) {
    if reduce_and_persist_workspace_event(workspace, wheel_event(event)) {
        event.prevent_default();
    }
}

/// Apply one host-issued event to the reducer and explicit save policy.
pub(super) fn reduce_and_persist_workspace_event<K: PanelKind>(
    workspace: &PanelWorkspace<K>,
    event: WorkspaceEvent<K>,
) -> bool {
    let mut snapshot_signal = workspace.snapshot;
    let mut snapshot = snapshot_signal.write();
    let context = reduce_context(panel_kit::surface::surface_profile(snapshot.viewport.width));
    let reduction = reduce(&mut snapshot, event, context);
    let decision = workspace.save_policy.decide(&reduction);
    let changed = reduction.changed;

    enforce_tile_minimums(&mut snapshot, &workspace.tile_min_rows);

    if let Err(error) =
        apply_save_decision(decision, &*workspace.store, &snapshot, &workspace.catalog)
    {
        log_layout_error("save layout", workspace.storage_key, &error);
    }

    changed
}

fn reduce_context(surface: panel_kit_core::SurfaceProfile) -> ReduceContext<'static> {
    ReduceContext {
        surface,
        clamp: &Clamp::WEB,
        command_step: CommandStep::WEB,
        tile: &TileMetrics::WEB,
        snap: SnapPolicy::default(),
    }
}
