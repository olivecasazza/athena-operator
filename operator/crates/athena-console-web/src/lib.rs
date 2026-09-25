//! Dioxus (web) reimplementation of `athena-console` on the panel-kit
//! workspace shell.
//!
//! The native console was an Iced desktop app that talked to Kubernetes through
//! `kube` directly. That cannot run in a browser, so this port splits into:
//!
//! - a Dioxus **frontend** (this crate's lib + default bin) where every console
//!   view is a panel-kit [`Panel`], and
//! - a tiny axum **backend** (`src/bin/server.rs`, `server` feature) that reuses
//!   `athena-api` + `kube` to serve the [`models`] DTOs as JSON.
//!
//! The headline view, [`Panel::ExperimentDetail`], embeds the learning-metric
//! Grafana dashboard via [`panel_kit::grafana::GrafanaDashboard`] and the
//! manifest editor via [`panel_kit::ide::IdePanel`]. (Its sibling
//! [`panel_kit::grafana::GrafanaPanel`] embeds one `/d-solo/` chart — unusable
//! here until the dashboard's panel ids are pinned in nixlab, since
//! provisioning reassigns them.)

pub mod models;
mod tables;
mod workspace;

use dioxus::events::PointerEvent as DioxusPointerEvent;
use dioxus::prelude::*;
use models::{
    ClusterSnapshot, ConditionDto, ReportSpecDto, ReportSummary, ResourceSummary,
    SchedulingSnapshot, TemplateSummary,
};
use panel_kit::grafana::GrafanaDashboard;
use panel_kit::ide::IdePanel;
use panel_kit::widgets::{DataColumnSpec, DataRow as Row, DataTable, SortKey as Key};
use panel_kit::{LayoutBuilder, PanelKind, PanelWin};
use panel_kit_core::Mode;
use panel_kit_core::PanelCommand;
use panel_kit_core::reducer::WorkspaceEvent;
use panel_kit_core::widgets::data_table::{SortDir, TableQuery};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};
use tables::{RowExt, campaign_columns, campaign_row, experiment_columns, experiment_row, fmt_ms};
use workspace::{PanelWorkspace, ViewPreset, ViewsSpec};
use workspace::{
    handle_key, handle_pointer_move, handle_pointer_up, handle_wheel, mount_viewport_observer,
    project_workspace, use_panel_workspace, workspace_area_class, workspace_contents,
    workspace_event_handler,
};

/// Grafana base URL for the embedded learning-metric dashboards.
const GRAFANA_BASE: &str = "https://grafana.casazza.io";
/// UID of the Auto-RL / training-loss research-runs dashboard.
const GRAFANA_DASHBOARD_UID: &str = "athena-research-runs";

/// The console's views, one panel each.
///
/// `PanelKind` requires `Copy + Eq + Hash + Serialize`, so variants cannot
/// carry data. The selected experiment for [`Panel::ExperimentDetail`] lives in
/// an external `Signal<Option<ResourceSummary>>` (the direct analogue of the
/// native console's `selected_resource` field).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Panel {
    /// Experiment list (the native `View::Experiments`).
    Experiments,
    /// Metadata for the selected experiment (phase, namespace, workspace…).
    ExperimentDetail,
    /// Learning metrics (embedded Grafana dashboard) for the selected experiment.
    ExperimentMetrics,
    /// Manifest IDE for the selected experiment.
    ExperimentManifest,
    /// ResearchCampaign list.
    Campaigns,
    /// ExperimentTemplate list + template-YAML editor.
    Templates,
    /// RuntimeProfile list.
    RuntimeProfiles,
    /// BenchmarkSuite + BenchmarkRun lists (the native `View::Benchmarks`).
    Benchmarks,
    /// Compose a campaign's experiments into a research-paper dataset.
    ReportCurator,
    /// Published ResearchReport list.
    Reports,
    /// Drill-down research surface: global fleet → campaign → experiment or
    /// report. The hierarchy mirrors the CRD ownership chain (ResearchDrive →
    /// ResearchCampaign → Experiment/ResearchReport); the breadcrumb bar is
    /// the navigation record. Like every other panel it carries no data —
    /// the current level lives in `research_nav` (see [`App`]).
    Research,
}

impl PanelKind for Panel {
    fn title(self) -> &'static str {
        match self {
            Panel::Experiments => "Experiments",
            Panel::ExperimentDetail => "Experiment Detail",
            Panel::ExperimentMetrics => "Experiment Metrics",
            Panel::ExperimentManifest => "Experiment Manifest",
            Panel::Campaigns => "Campaigns",
            Panel::Templates => "Templates",
            Panel::RuntimeProfiles => "Runtime Profiles",
            Panel::Benchmarks => "Benchmarks",
            Panel::ReportCurator => "Report Curator",
            Panel::Reports => "Reports",
            Panel::Research => "Research",
        }
    }
}

/// Gap between panels, and from the workspace edge.
const GUTTER: f64 = 16.0;

/// Stack `panels` (kind + height) down a column of width `w` starting at `x`,
/// deriving every `y`. The same authored height becomes an explicit tile-row
/// span; the host preserves that span as a floor, replacing the removed
/// per-panel `tile_min_h` without inventing a second height.
fn column<K: Copy>(
    b: &mut LayoutBuilder,
    x: f64,
    w: f64,
    tile_w: u8,
    panels: &[(K, f64)],
) -> Vec<PanelWin<K>> {
    let mut y = GUTTER;
    panels
        .iter()
        .map(|&(kind, h)| {
            let tile_h = (h / panel_kit::TILE_ROW_PX).ceil() as u8;
            let win = b.at(kind, x, y, w, h).with_tile(tile_w, tile_h);
            y += h + GUTTER;
            win
        })
        .collect()
}

/// Two columns: browse/curate on the left, the selected experiment on the right.
fn default_layout() -> Vec<PanelWin<Panel>> {
    const BROWSE_W: f64 = 640.0;
    const DETAIL_W: f64 = 620.0;
    let mut b = LayoutBuilder::new();
    let mut wins = column(
        &mut b,
        GUTTER,
        BROWSE_W,
        2,
        &[
            (Panel::Experiments, 460.0),
            (Panel::Templates, 320.0),
            (Panel::Campaigns, 260.0),
            (Panel::RuntimeProfiles, 260.0),
            (Panel::ReportCurator, 520.0),
            (Panel::Reports, 260.0),
        ],
    );
    wins.extend(column(
        &mut b,
        GUTTER + BROWSE_W + GUTTER,
        DETAIL_W,
        2,
        &[
            (Panel::ExperimentDetail, 200.0),
            // Tall by default: this hosts a whole Grafana dashboard, not one chart.
            (Panel::ExperimentMetrics, 560.0),
            (Panel::ExperimentManifest, 360.0),
            (Panel::Benchmarks, 320.0),
            // The drill-down surface is tall: the global level stacks drive
            // cards (with per-stage template tables) above the campaign table.
            (Panel::Research, 640.0),
        ],
    ));
    wins
}

/// Named views over the research workspace: which panels are visible, their
/// tile spans (4-column grid, row-major shelves), and mode. Every other panel
/// is minimized to the dock. Spans are relative: tiles stretch to fill the
/// viewport, so shelf heights set proportions, not pixels.
static RESEARCH_VIEWS: ViewsSpec<Panel> = ViewsSpec {
    base: "athena_console_web_v4",
    presets: &[
        ViewPreset {
            name: "Research",
            mode: Mode::Tiling,
            panels: &[
                (Panel::Research, 2, 4),
                (Panel::ExperimentMetrics, 2, 4),
                (Panel::ExperimentDetail, 4, 1),
            ],
        },
        ViewPreset {
            name: "Experiments",
            mode: Mode::Tiling,
            panels: &[
                (Panel::Experiments, 2, 4),
                (Panel::ExperimentMetrics, 2, 4),
                (Panel::ExperimentDetail, 2, 2),
                (Panel::ExperimentManifest, 2, 2),
            ],
        },
        ViewPreset {
            name: "Campaigns",
            mode: Mode::Tiling,
            panels: &[
                (Panel::Campaigns, 2, 4),
                (Panel::ExperimentMetrics, 2, 4),
                (Panel::ExperimentDetail, 4, 1),
            ],
        },
        ViewPreset {
            name: "Reports",
            mode: Mode::Tiling,
            panels: &[(Panel::Reports, 2, 4), (Panel::ReportCurator, 2, 4)],
        },
        ViewPreset {
            name: "Catalog",
            mode: Mode::Tiling,
            panels: &[
                (Panel::Templates, 2, 3),
                (Panel::Benchmarks, 2, 3),
                (Panel::RuntimeProfiles, 4, 2),
            ],
        },
    ],
};

/// Top-bar view switcher: one button per view, reset, save-as, delete custom.
fn view_bar(
    ws: &PanelWorkspace<Panel>,
    mut draft: Signal<String>,
    mut err: Signal<String>,
) -> Element {
    let registry = ws.registry.read().clone();
    let reset_ws = ws.clone();
    let save_ws = ws.clone();
    rsx! {
        nav { class: "views", aria_label: "views",
            for name in registry.views.clone() {
                {
                    let active = name == registry.active;
                    let custom = !RESEARCH_VIEWS.is_preset(&name);
                    let switch_ws = ws.clone();
                    let delete_ws = ws.clone();
                    let target = name.clone();
                    let doomed = name.clone();
                    rsx! {
                        span { class: "view-tab", key: "{name}",
                            button {
                                class: if active { "btn view-btn active-view" } else { "btn view-btn" },
                                aria_pressed: "{active}",
                                onclick: move |_| switch_ws.switch_view(&target),
                                "{name}"
                            }
                            if custom {
                                button {
                                    class: "btn view-del",
                                    title: "delete view {name}",
                                    onclick: move |_| delete_ws.delete_view(&doomed),
                                    "\u{d7}"
                                }
                            }
                        }
                    }
                }
            }
            button {
                class: "btn",
                title: "restore this view's panels and layout",
                onclick: move |_| reset_ws.reset_view(),
                "Reset"
            }
            input {
                class: "rc-input view-name",
                placeholder: "new view…",
                value: "{draft}",
                oninput: move |e| draft.set(e.value()),
            }
            button {
                class: "btn",
                disabled: draft.read().trim().is_empty(),
                onclick: move |_| {
                    let name = draft.read().trim().to_string();
                    match save_ws.save_view_as(&name) {
                        Ok(()) => {
                            draft.set(String::new());
                            err.set(String::new());
                        }
                        Err(e) => err.set(e),
                    }
                },
                "Save as"
            }
            if !err.read().is_empty() {
                span { class: "err", "{err}" }
            }
        }
    }
}

/// Admin-only page: the GPU-scheduling / inference stack. Rendered on a SEPARATE
/// panel-kit workspace (its own layout), opened via the topbar "Admin" toggle.
/// Observability only — cluster config stays GitOps (nixlab/Flux).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AdminPanel {
    GpuPools,
    NodePower,
    Inference,
}

impl PanelKind for AdminPanel {
    fn title(self) -> &'static str {
        match self {
            AdminPanel::GpuPools => "GPU Pools & Queues",
            AdminPanel::NodePower => "Node Power · Hephaestus",
            AdminPanel::Inference => "Inference Backends",
        }
    }
}

fn admin_layout() -> Vec<PanelWin<AdminPanel>> {
    const POOLS_W: f64 = 900.0;
    const SIDE_W: f64 = 440.0;
    let mut b = LayoutBuilder::new();
    let mut wins = column(&mut b, GUTTER, POOLS_W, 3, &[(AdminPanel::GpuPools, 560.0)]);
    wins.extend(column(
        &mut b,
        GUTTER + POOLS_W + GUTTER,
        SIDE_W,
        1,
        &[
            (AdminPanel::NodePower, 260.0),
            (AdminPanel::Inference, 284.0),
        ],
    ));
    wins
}

/// App-specific theming layered after [`panel_kit::CSS`]: a high-contrast
/// pink/blue palette echoing the native console, plus table + detail styles.
const APP_CSS: &str = "
:root { --accent: #f472b6; --pink: #f472b6; --blue: #93c5fd; }
.topbar { display:flex; align-items:baseline; gap:1rem; padding:.5rem .9rem;
  border-bottom:1px solid var(--line); }
.topbar h1 { font-size:1rem; color:var(--pink); margin:0; }
.topbar .hint { color:var(--dim); font-size:.72rem; }
.views { display:flex; align-items:center; gap:.3rem; flex:1; min-width:0; flex-wrap:wrap; }
.view-tab { display:inline-flex; }
.view-btn.active-view { background:var(--inv-bg); color:var(--inv-fg); border-color:var(--inv-bg); }
.view-del { padding:0 .3rem; border-left:0; }
.view-name { width:8rem; }
.tbl { width:100%; border-collapse:collapse; font-size:.74rem; }
.tbl th { text-align:left; color:var(--dim); font-weight:normal;
  border-bottom:1px solid var(--line2); padding:.25rem .4rem; }
.tbl td { padding:.25rem .4rem; border-bottom:1px solid var(--line);
  color:var(--fg); vertical-align:top; }
.tbl .phase { color:var(--blue); }
.row-link { background:none; border:none; color:var(--pink); cursor:pointer;
  font-family:var(--mono); font-size:.74rem; padding:0; text-align:left; }
.row-link:hover { text-decoration:underline; }
.muted { color:var(--dim); }
.view-head { margin:.1rem 0 .6rem; }
.scroll-tbl { max-height:340px; overflow-y:auto; }
.scroll-tbl .tbl thead th { position:sticky; top:0; background:var(--bg); z-index:1; }
.view-head h2 { font-size:.92rem; color:var(--fg); margin:0 0 .2rem; }
.view-head p { font-size:.72rem; color:var(--dim); margin:0; }
.detail-grid { display:grid; grid-template-columns:auto 1fr; gap:.2rem .8rem;
  font-size:.74rem; margin-bottom:.6rem; }
.detail-grid dt { color:var(--dim); }
.detail-grid dd { color:var(--fg); margin:0; word-break:break-all; }
.btn { background:var(--bg); color:var(--fg); border:1px solid var(--line2);
  border-radius:3px; padding:.1rem .45rem; font-size:.7rem; cursor:pointer;
  font-family:var(--mono); }
.admin-toggle { margin-left:auto; color:var(--pink); border-color:var(--pink); }
.btn:hover { border-color:var(--pink); }
.section-label { font-size:.78rem; color:var(--fg); margin:.5rem 0 .25rem; }
/* The Grafana embed tracks the panel's height instead of a fixed 340px, so
   resizing/maximizing the panel actually gives the dashboard more room (it is
   ~1400px tall and scrolls internally otherwise). 100% resolves against
   .panel-body's CONTENT box, so its padding is already excluded; the collapse
   floor is panel-kit's own .grafana-panel min-height. */
.embed-block { height:100%; margin:0; }
.ide-block { height:260px; margin:.4rem 0; }
.todo { color:var(--yellow); font-size:.74rem; }
.err { color:var(--red); font-size:.76rem; }
.preview { max-height:300px; overflow:auto; white-space:pre-wrap;
  font-family:var(--mono); font-size:.7rem; background:var(--bg);
  border:1px solid var(--line); padding:.4rem; margin:.4rem 0; }
.rc-select { background:var(--bg); color:var(--fg); border:1px solid var(--line2);
  font-family:var(--mono); font-size:.74rem; padding:.15rem .3rem; width:100%; }
.rc-input { background:var(--bg); color:var(--fg); border:1px solid var(--line2);
  font-family:var(--mono); font-size:.74rem; padding:.15rem .3rem; width:100%;
  box-sizing:border-box; }
.rc-textarea { background:var(--bg); color:var(--fg); border:1px solid var(--line2);
  font-family:var(--mono); font-size:.72rem; padding:.2rem .3rem; width:100%;
  box-sizing:border-box; height:4rem; resize:vertical; }
/* Research drill-down: breadcrumbs are the navigation record; chips mark
   leaf kinds (experiment vs report); condition badges colour by status. */
.crumbs { display:flex; align-items:baseline; gap:.3rem; font-size:.74rem;
  font-family:var(--mono); margin:.1rem 0 .6rem; }
.crumbs .sep { color:var(--dim); }
.chip { display:inline-block; border:1px solid var(--blue); color:var(--blue);
  border-radius:3px; padding:0 .25rem; font-size:.66rem; margin-left:.3rem; }
.cond { display:inline-block; border-radius:3px; padding:0 .25rem;
  font-size:.66rem; margin:0 .15rem .15rem 0; border:1px solid var(--line2);
  color:var(--dim); }
.cond.True, .cond.ok, .cond.ready { border-color:var(--green); color:var(--green); }
.cond.warn { border-color:var(--yellow); color:var(--yellow); }
.cond.bad, .cond.err { border-color:var(--red); color:var(--red); }
.pk-dt td.date { white-space:nowrap; color:var(--dim); }
.pk-dt td .row-link { max-width:100%; overflow:hidden; text-overflow:ellipsis; white-space:nowrap; display:inline-block; vertical-align:bottom; }
.filmstrip { width:100%; margin:.2rem 0; border:1px solid var(--line); }
.crumb-current { color:var(--fg); }
";

/// Research drill-down depth. The hierarchy mirrors CRD ownership
/// (ResearchDrive -> ResearchCampaign -> Experiment / ResearchReport); one
/// level is visible at a time and the breadcrumb bar renders the path back.
#[derive(Clone, PartialEq)]
enum ResearchNav {
    Global,
    Campaign(String),
    Experiment {
        campaign: String,
        exp: ResourceSummary,
    },
    Report {
        campaign: String,
        report: ReportSummary,
    },
}

/// App root.
#[component]
pub fn App() -> Element {
    // Preserve the existing versioned localStorage keys. The core V1/V2 reader
    // accepts the controller-era records; authored tile spans are reapplied as
    // floors so old saved layouts cannot collapse iframe-backed panels.
    let ws = use_panel_workspace(
        "athena_console_web_v3",
        default_layout,
        Some(&RESEARCH_VIEWS),
    );
    let admin_ws = use_panel_workspace("athena_console_web_admin_v2", admin_layout, None);
    let view_draft = use_signal(String::new);
    let view_err = use_signal(String::new);
    mount_viewport_observer(&ws);
    mount_viewport_observer(&admin_ws);
    let ws_emit = workspace_event_handler(&ws);
    let admin_emit = workspace_event_handler(&admin_ws);

    // Admin page toggle — a separate panel set (GPU/Kueue/inference). Researchers
    // stay on the default page; access itself is gated at the ingress (Zero Trust).
    let mut admin = use_signal(|| false);

    // Snapshot fetched once from the backend; views read it reactively.
    let snapshot = use_resource(move || async move { fetch_snapshot().await });
    // Scheduling/inference stack snapshot for the admin page.
    let sched = use_resource(move || async move { fetch_scheduling().await });

    // External selection state for the data-less ExperimentDetail panel.
    let selected = use_signal(|| Option::<ResourceSummary>::None);
    // Editable manifest / template documents for the IdePanels.
    let manifest_doc = use_signal(|| "# Select an experiment to load its manifest.\n".to_string());
    let template_doc = use_signal(|| "# Load a template to view its YAML.\n".to_string());

    // Report Curator state.
    let selected_campaign = use_signal(|| Option::<ResourceSummary>::None);
    let report_name = use_signal(String::new);
    let report_title = use_signal(String::new);
    let excluded: Signal<HashSet<String>> = use_signal(HashSet::new);
    let sec_abstract = use_signal(String::new);
    let sec_related_work = use_signal(String::new);
    let sec_discussion = use_signal(String::new);
    let sec_limitations = use_signal(String::new);
    let seeds_text = use_signal(String::new);
    let preview_doc = use_signal(String::new);
    let save_status = use_signal(String::new);

    // Research drill-down navigation. `Panel` variants carry no data (PanelKind
    // is Copy), so the current depth — global fleet, one campaign, one
    // experiment, one report — lives out here exactly like `selected` does for
    // ExperimentDetail. The breadcrumb bar is the rendered form of this signal.
    let research_nav = use_signal(|| ResearchNav::Global);

    let body = move |kind: Panel, _maximized: bool| -> Element {
        let snap = snapshot.read();
        let snap = match &*snap {
            None => {
                return rsx! { p { class: "muted", "Loading Athena resources…" } };
            }
            Some(Err(e)) => {
                return rsx! { p { class: "err", "Failed to load snapshot: {e}" } };
            }
            Some(Ok(snap)) => snap.clone(),
        };

        match kind {
            Panel::Experiments => experiments_view(snap, ws_emit, selected, manifest_doc),
            Panel::ExperimentDetail => experiment_detail_view(selected),
            Panel::ExperimentMetrics => experiment_metrics_view(selected),
            Panel::ExperimentManifest => experiment_manifest_view(selected, manifest_doc),
            Panel::Campaigns => campaigns_view(snap, ws_emit, selected, manifest_doc),
            Panel::Templates => templates_view(snap, template_doc),
            Panel::RuntimeProfiles => runtime_view(snap),
            Panel::Benchmarks => benchmarks_view(snap),
            Panel::ReportCurator => report_curator_view(
                snap,
                selected_campaign,
                report_name,
                report_title,
                excluded,
                sec_abstract,
                sec_related_work,
                sec_discussion,
                sec_limitations,
                seeds_text,
                preview_doc,
                save_status,
            ),
            Panel::Reports => {
                reports_view(snap, ws_emit, selected_campaign, report_name, report_title)
            }
            Panel::Research => research_view(snap, research_nav, selected, manifest_doc, ws_emit),
        }
    };

    let admin_body = move |kind: AdminPanel, _maximized: bool| -> Element {
        let s = sched.read();
        let snap = match &*s {
            None => return rsx! { p { class: "muted", "Loading scheduling state…" } },
            Some(Err(e)) => {
                return rsx! { p { class: "err", "Failed to load scheduling: {e}" } };
            }
            Some(Ok(snap)) => snap.clone(),
        };
        match kind {
            AdminPanel::GpuPools => gpu_pools_view(snap),
            AdminPanel::NodePower => node_power_view(snap),
            AdminPanel::Inference => inference_view(snap),
        }
    };

    let workspace_snapshot = ws.snapshot.read();
    let mut workspace_scratch = ws.scratch.borrow_mut();
    let workspace_frame = project_workspace(&workspace_snapshot, &mut workspace_scratch);
    let workspace_root_class = panel_kit::widgets::root::root_class(&workspace_frame);
    let workspace_class = workspace_area_class(&workspace_frame);
    let workspace_style = workspace_frame
        .tile_grid
        .map(panel_kit::widgets::root::tile_grid_style)
        .unwrap_or_default();

    let admin_snapshot = admin_ws.snapshot.read();
    let mut admin_scratch = admin_ws.scratch.borrow_mut();
    let admin_frame = project_workspace(&admin_snapshot, &mut admin_scratch);
    let admin_root_class = panel_kit::widgets::root::root_class(&admin_frame);
    let admin_class = workspace_area_class(&admin_frame);
    let admin_style = admin_frame
        .tile_grid
        .map(panel_kit::widgets::root::tile_grid_style)
        .unwrap_or_default();

    let pointer_move_workspace = ws.clone();
    let pointer_up_workspace = ws.clone();
    let pointer_cancel_workspace = ws.clone();
    let key_workspace = ws.clone();
    let wheel_workspace = ws.clone();
    let admin_pointer_move_workspace = admin_ws.clone();
    let admin_pointer_up_workspace = admin_ws.clone();
    let admin_pointer_cancel_workspace = admin_ws.clone();
    let admin_key_workspace = admin_ws.clone();
    let admin_wheel_workspace = admin_ws.clone();

    rsx! {
        style { {panel_kit::CSS} }
        style { {APP_CSS} }
        if admin() {
            div {
                class: "{admin_root_class}",
                tabindex: "0",
                onpointermove: move |event: DioxusPointerEvent| {
                    handle_pointer_move(&admin_pointer_move_workspace, &event)
                },
                onpointerup: move |event: DioxusPointerEvent| {
                    handle_pointer_up(&admin_pointer_up_workspace, &event)
                },
                onpointercancel: move |event: DioxusPointerEvent| {
                    handle_pointer_up(&admin_pointer_cancel_workspace, &event)
                },
                onkeydown: move |event| handle_key(&admin_key_workspace, &event),
                header { class: "topbar",
                    h1 { "Athena Console · Admin" }
                    span { class: "hint", "GPU scheduling · Kueue · inference · Hephaestus — read-only" }
                    button { class: "btn admin-toggle", onclick: move |_| admin.set(false), "← Research" }
                }
                div {
                    class: "{admin_class}",
                    style: "{admin_style}",
                    onwheel: move |event| handle_wheel(&admin_wheel_workspace, &event),
                    {workspace_contents(&admin_frame, &admin_ws.catalog, admin_emit, admin_body)}
                }
                {panel_kit::widgets::dock::dock(
                    admin_frame.dock,
                    &admin_ws.catalog,
                    admin_emit,
                    None,
                )}
            }
        } else {
            div {
                class: "{workspace_root_class}",
                tabindex: "0",
                onpointermove: move |event: DioxusPointerEvent| {
                    handle_pointer_move(&pointer_move_workspace, &event)
                },
                onpointerup: move |event: DioxusPointerEvent| {
                    handle_pointer_up(&pointer_up_workspace, &event)
                },
                onpointercancel: move |event: DioxusPointerEvent| {
                    handle_pointer_up(&pointer_cancel_workspace, &event)
                },
                onkeydown: move |event| handle_key(&key_workspace, &event),
                header { class: "topbar",
                    h1 { "Athena Console" }
                    {view_bar(&ws, view_draft, view_err)}
                    button { class: "btn admin-toggle", onclick: move |_| admin.set(true), "⚙ Admin" }
                }
                div {
                    class: "{workspace_class}",
                    style: "{workspace_style}",
                    onwheel: move |event| handle_wheel(&wheel_workspace, &event),
                    {workspace_contents(&workspace_frame, &ws.catalog, ws_emit, body)}
                }
                {panel_kit::widgets::dock::dock(
                    workspace_frame.dock,
                    &ws.catalog,
                    ws_emit,
                    None,
                )}
            }
        }
    }
}

fn restore_panel(emit: EventHandler<WorkspaceEvent<Panel>>, panel: Panel) {
    emit.call(WorkspaceEvent::Command {
        target: Some(panel),
        command: PanelCommand::Restore,
    });
}

fn restore_detail_panels(emit: EventHandler<WorkspaceEvent<Panel>>) {
    restore_panel(emit, Panel::ExperimentDetail);
    restore_panel(emit, Panel::ExperimentMetrics);
    restore_panel(emit, Panel::ExperimentManifest);
}

// ---------------------------------------------------------------------------
// Views
// ---------------------------------------------------------------------------

fn experiments_view(
    snap: ClusterSnapshot,
    emit: EventHandler<WorkspaceEvent<Panel>>,
    selected: Signal<Option<ResourceSummary>>,
    manifest_doc: Signal<String>,
) -> Element {
    let sel = selected.read().as_ref().map(|s| s.name.clone());
    let rows: Vec<Row> = snap
        .experiments
        .into_iter()
        .map(|exp| {
            let pick = exp.clone();
            let is_sel = sel.as_deref() == Some(exp.name.as_str());
            let name = exp.name.clone();
            experiment_row(
                &exp,
                rsx! { td {
                    button {
                        class: "row-link",
                        onclick: move |_| select_resource(selected, manifest_doc, emit, pick.clone()),
                        "{name}"
                    }
                } },
                true,
            )
            .selected(is_sel)
        })
        .collect();
    rsx! {
        div { class: "view-head",
            h2 { "Experiments" }
            p { "Kubernetes-native experiment resources, phases, and workspace refs." }
        }
        DataTable {
            columns: experiment_columns(true),
            rows,
            initial: TableQuery::sorted("created", SortDir::Desc),
            storage_key: Some("athena.table.experiments".to_string()),
            empty: "No experiments found.",
            placeholder: "filter experiments…",
        }
    }
}

/// Placeholder shown by the per-experiment panels when nothing is selected.
fn no_selection() -> Element {
    rsx! {
        p { class: "muted",
            "Select an experiment from the Experiments panel."
        }
    }
}

/// Metadata grid for the selected experiment.
fn experiment_detail_view(selected: Signal<Option<ResourceSummary>>) -> Element {
    let Some(exp) = selected.read().clone() else {
        return no_selection();
    };
    let workspace = exp
        .workspace_path
        .clone()
        .unwrap_or_else(|| "Not reported".to_string());
    let manifest_path = exp.manifest_path();

    rsx! {
        div { class: "view-head",
            h2 { "{exp.name}" }
            p { "{exp.namespace} / {exp.kind} · phase {exp.phase}" }
        }
        dl { class: "detail-grid",
            dt { "Phase" } dd { "{exp.phase}" }
            dt { "Namespace" } dd { "{exp.namespace}" }
            dt { "Detail" } dd { "{exp.detail}" }
            dt { "Workspace" } dd { "{workspace}" }
            dt { "Manifest" } dd { "{manifest_path}" }
        }
    }
}

/// Buffer (ms) added around a run window so the embed shows a little before/after.
const RANGE_BUFFER_MS: i64 = 300_000; // 5 min

/// Grafana `from`/`to` (epoch-ms strings, or relative) scoped to a resource's run
/// window with a buffer. Unknown start ⇒ default 6h window; still-running ⇒ "now".
fn time_range(sel: &ResourceSummary) -> (String, String) {
    let from = sel
        .started_at
        .as_ref()
        .and_then(|s| s.parse::<i64>().ok())
        .map(|ms| (ms - RANGE_BUFFER_MS).to_string())
        .unwrap_or_else(|| "now-6h".to_string());
    let to = sel
        .ended_at
        .as_ref()
        .and_then(|s| s.parse::<i64>().ok())
        .map(|ms| (ms + RANGE_BUFFER_MS).to_string())
        .unwrap_or_else(|| "now".to_string());
    (from, to)
}

/// Set the selection, surface the three detail panels, and load the manifest —
/// shared by the Experiments and Campaigns tables (both are clickable).
fn select_resource(
    mut selected: Signal<Option<ResourceSummary>>,
    mut manifest_doc: Signal<String>,
    emit: EventHandler<WorkspaceEvent<Panel>>,
    r: ResourceSummary,
) {
    selected.set(Some(r.clone()));
    restore_detail_panels(emit);
    spawn(async move {
        match fetch_manifest(&r.namespace, &r.kind, &r.name).await {
            Ok(yaml) => manifest_doc.set(yaml),
            Err(err) => manifest_doc.set(format!("# failed to load manifest: {err}\n")),
        }
    });
}

/// Embedded learning-metrics dashboard, scoped to the selected experiment OR
/// campaign and time-ranged to its run window. A campaign selection scopes by the
/// `campaign` dashboard var (all its experiments); an experiment by `experiment`.
fn experiment_metrics_view(selected: Signal<Option<ResourceSummary>>) -> Element {
    let Some(sel) = selected.read().clone() else {
        return no_selection();
    };
    let var_key = if sel.kind == "researchcampaign" {
        "campaign"
    } else {
        "experiment"
    };
    let vars = vec![(var_key.to_string(), sel.name.clone())];
    let (from, to) = time_range(&sel);

    rsx! {
        div { class: "embed-block",
            GrafanaDashboard {
                base_url: GRAFANA_BASE,
                dashboard_uid: GRAFANA_DASHBOARD_UID,
                vars,
                from,
                to,
                theme: "dark",
                title: "Athena research runs",
            }
        }
    }
}

/// Manifest IDE for the selected experiment.
fn experiment_manifest_view(
    selected: Signal<Option<ResourceSummary>>,
    mut manifest_doc: Signal<String>,
) -> Element {
    let Some(exp) = selected.read().clone() else {
        return no_selection();
    };
    let manifest_path = exp.manifest_path();

    rsx! {
        div { class: "ide-block",
            IdePanel {
                value: manifest_doc(),
                language: "yaml",
                title: manifest_path,
                on_change: move |next: String| manifest_doc.set(next),
            }
        }
    }
}

fn campaigns_view(
    snap: ClusterSnapshot,
    emit: EventHandler<WorkspaceEvent<Panel>>,
    selected: Signal<Option<ResourceSummary>>,
    manifest_doc: Signal<String>,
) -> Element {
    let sel = selected.read().as_ref().map(|s| s.name.clone());
    let rows: Vec<Row> = snap
        .campaigns
        .into_iter()
        .map(|c| {
            let pick = c.clone();
            let shown = campaign_label(&c);
            let is_sel = sel.as_deref() == Some(c.name.as_str());
            let full = c.name.clone();
            let label = shown.clone();
            campaign_row(
                &c,
                rsx! { td { title: "{full}",
                    button {
                        class: "row-link",
                        onclick: move |_| select_resource(selected, manifest_doc, emit, pick.clone()),
                        "{label}"
                    }
                } },
                &shown,
            )
            .selected(is_sel)
        })
        .collect();
    rsx! {
        div { class: "view-head",
            h2 { "Campaigns" }
            p { "Click a campaign to load its metrics (all member experiments, over the campaign window) into the Metrics panel." }
        }
        DataTable {
            columns: campaign_columns(),
            rows,
            initial: TableQuery::sorted("created", SortDir::Desc),
            storage_key: Some("athena.table.campaigns".to_string()),
            empty: "No campaigns found.",
            placeholder: "filter campaigns…",
        }
    }
}

/// RFC 3339 timestamp (status fields) → `YYYY-MM-DD HH:MM`, matching [`fmt_ms`].
fn fmt_rfc3339(at: &str) -> String {
    at.get(..16)
        .map(|s| s.replace('T', " "))
        .unwrap_or_else(|| at.to_string())
}

/// Campaign name as shown in lists: legacy drive-spawned campaigns repeat the
/// drive name as a prefix (sometimes twice). The CR name is immutable history,
/// so the viewer strips the echo for display and keeps the full name in the
/// tooltip and search haystack.
fn campaign_label(c: &ResourceSummary) -> String {
    let Some(drive) = c.drive.as_deref() else {
        return c.name.clone();
    };
    let prefix = format!("{drive}-");
    let mut shown = c.name.as_str();
    while let Some(rest) = shown.strip_prefix(prefix.as_str()) {
        if rest.is_empty() {
            break;
        }
        shown = rest;
    }
    shown.to_string()
}

fn templates_view(snap: ClusterSnapshot, mut template_doc: Signal<String>) -> Element {
    let rows: Vec<Row> = snap
        .templates
        .into_iter()
        .map(|tpl: TemplateSummary| {
            let t = tpl.clone();
            Row::new(tpl.name.clone())
                .text(tpl.name.clone())
                .text(tpl.objective.clone())
                .date(&tpl.created_at)
                .text_class(tpl.detail.clone(), "muted")
                .cell(
                    rsx! { td {
                        button {
                            class: "btn",
                            onclick: move |_| {
                                let t = t.clone();
                                spawn(async move {
                                    match fetch_template_yaml(&t.namespace, &t.name).await {
                                        Ok(yaml) => template_doc.set(yaml),
                                        Err(err) => template_doc.set(format!("# failed to load template: {err}\n")),
                                    }
                                });
                            },
                            "Load YAML"
                        }
                    } },
                    Key::Missing,
                    "",
                )
        })
        .collect();
    rsx! {
        div { class: "view-head",
            h2 { "Experiment Templates" }
            p { "Load Kubernetes-owned template YAML and inspect objectives + sources." }
        }
        DataTable {
            columns: vec![
                DataColumnSpec::new("name", "Template").pinned(),
                DataColumnSpec::new("objective", "Objective"),
                DataColumnSpec::new("created", "Created"),
                DataColumnSpec::new("source", "Source"),
                DataColumnSpec::new("load", "").pinned().unsorted(),
            ],
            rows,
            initial: TableQuery::sorted("name", SortDir::Asc),
            storage_key: Some("athena.table.templates".to_string()),
            empty: "No templates found.",
            placeholder: "filter templates…",
        }
        div { class: "section-label", "Template YAML" }
        div { class: "ide-block",
            IdePanel {
                value: template_doc(),
                language: "yaml",
                title: "experimenttemplate.yaml",
                on_change: move |next: String| template_doc.set(next),
            }
        }
    }
}

fn runtime_view(snap: ClusterSnapshot) -> Element {
    rsx! {
        div { class: "view-head",
            h2 { "Runtime Profiles" }
            p { "RuntimeProfile resources: execution mode, images, workspace storage." }
        }
        {resource_table(snap.runtime_profiles, "RuntimeProfile", "Runtime")}
    }
}

fn benchmarks_view(snap: ClusterSnapshot) -> Element {
    rsx! {
        div { class: "view-head",
            h2 { "Benchmarks" }
            p { "BenchmarkSuite and BenchmarkRun resources read from Kubernetes status." }
        }
        div { class: "section-label", "Suites" }
        {resource_table(snap.benchmark_suites, "BenchmarkSuite", "Tasks")}
        div { class: "section-label", "Runs" }
        {resource_table(snap.benchmark_runs, "BenchmarkRun", "Suite")}
    }
}

/// A generic three-column table (name+namespace, phase, detail) for the views
/// that don't need row interactions.
fn resource_table(
    rows: Vec<ResourceSummary>,
    name_col: &'static str,
    detail_col: &'static str,
) -> Element {
    let rows: Vec<Row> = rows
        .into_iter()
        .map(|r| {
            Row::new(r.name.clone())
                .text(r.name.clone())
                .text_class(r.phase.clone(), "phase")
                .date(&r.created_at)
                .text_class(r.detail.clone(), "muted")
        })
        .collect();
    rsx! {
        DataTable {
            columns: vec![
                DataColumnSpec::new("name", name_col).pinned(),
                DataColumnSpec::new("phase", "Phase"),
                DataColumnSpec::new("created", "Created"),
                DataColumnSpec::new("detail", detail_col),
            ],
            rows,
            initial: TableQuery::sorted("created", SortDir::Desc),
            storage_key: Some(format!("athena.table.{}", name_col.to_lowercase())),
        }
    }
}

// ---------------------------------------------------------------------------
// Research drill-down: global fleet -> campaign -> experiment | report.
// ---------------------------------------------------------------------------

/// Condition badges. Class picks the colour: True reads green, anything else
/// red; the reason rides in the title tooltip.
fn cond_badges(conds: &[ConditionDto]) -> Element {
    let rows: Vec<(String, String, String)> = conds
        .iter()
        .map(|c| {
            let class = if c.status == "True" {
                "cond ok"
            } else {
                "cond bad"
            };
            (
                class.to_string(),
                format!("{} {}", c.ctype, c.status),
                c.reason.clone(),
            )
        })
        .collect();
    rsx! {
        for (class, label, reason) in rows {
            span { class: "{class}", title: "{reason}", "{label}" }
        }
    }
}

fn research_view(
    snap: ClusterSnapshot,
    mut nav: Signal<ResearchNav>,
    mut selected: Signal<Option<ResourceSummary>>,
    mut manifest_doc: Signal<String>,
    emit: EventHandler<WorkspaceEvent<Panel>>,
) -> Element {
    let level = nav.read().clone();

    // Breadcrumbs: every ancestor is a button back to that depth.
    let campaign_crumb = match &level {
        ResearchNav::Global => None,
        ResearchNav::Campaign(c) => Some(c.clone()),
        ResearchNav::Experiment { campaign, .. } | ResearchNav::Report { campaign, .. } => {
            Some(campaign.clone())
        }
    };
    let leaf_crumb = match &level {
        ResearchNav::Experiment { exp, .. } => Some(exp.name.clone()),
        ResearchNav::Report { report, .. } => Some(report.name.clone()),
        _ => None,
    };
    let crumbs = {
        let campaign_for_leafless = campaign_crumb.clone();
        rsx! {
            div { class: "crumbs",
                button { class: "row-link", onclick: move |_| nav.set(ResearchNav::Global), "research" }
                if let Some(c) = campaign_crumb.clone() {
                    span { class: "sep", "\u{203a}" }
                    if leaf_crumb.is_some() {
                        button {
                            class: "row-link",
                            onclick: move |_| {
                                if let Some(c) = campaign_for_leafless.clone() {
                                    nav.set(ResearchNav::Campaign(c));
                                }
                            },
                            "{c}"
                        }
                    } else {
                        span { class: "crumb-current", "{c}" }
                    }
                }
                if let Some(l) = leaf_crumb.clone() {
                    span { class: "sep", "\u{203a}" }
                    span { class: "crumb-current", "{l}" }
                }
            }
        }
    };

    let body = match level {
        ResearchNav::Global => {
            let drives = snap.drives.clone();
            let campaign_rows: Vec<Row> = snap
                .campaigns
                .iter()
                .map(|c| {
                    let name = c.name.clone();
                    let shown = campaign_label(c);
                    let full = c.name.clone();
                    let label = shown.clone();
                    campaign_row(
                        c,
                        rsx! { td { title: "{full}",
                            button {
                                class: "row-link",
                                onclick: move |_| nav.set(ResearchNav::Campaign(name.clone())),
                                "{label}"
                            }
                        } },
                        &shown,
                    )
                })
                .collect();
            rsx! {
                div { class: "view-head",
                    h2 { "Research" }
                    p { "The autonomous loops, their curriculum evidence, and every campaign. Click through: campaign \u{203a} experiment or report." }
                }
                for d in drives {
                    div { class: "detail-card",
                        h3 { "{d.name}" }
                        p {
                            span { class: "phase", "{d.phase}" }
                            span { class: "muted", " created {fmt_ms(&d.created_at)}" }
                            if let Some(stage) = d.stage.clone() {
                                span { class: "chip", "stage: {stage}" }
                            }
                            span { class: "muted", " stagnation {d.stagnation}" }
                        }
                        p { {cond_badges(&d.conditions)} }
                        for st in d.stages.clone() {
                            div {
                                p { class: "muted",
                                    "{st.name}"
                                    if let Some(at) = st.promoted_at.clone() {
                                        " \u{2014} promoted {fmt_rfc3339(&at)}"
                                    }
                                }
                                table { class: "tbl",
                                    thead { tr { th { "Template" } th { "Best" } th { "Succeeded" } th { "Gate" } } }
                                    tbody {
                                        for t in st.templates.clone() {
                                            tr {
                                                td { "{t.template_ref}" }
                                                td { {t.best_objective.map(|b| format!("{b:.3}")).unwrap_or_else(|| "\u{2014}".into())} }
                                                td { "{t.succeeded}" }
                                                td {
                                                    if t.passed {
                                                        span { class: "cond ok", "passed" }
                                                    } else {
                                                        span { class: "cond", "pending" }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                h3 { "Campaigns" }
                DataTable {
                    columns: campaign_columns(),
                    rows: campaign_rows,
                    initial: TableQuery::sorted("created", SortDir::Desc),
                    storage_key: Some("athena.table.research.campaigns".to_string()),
                    empty: "No campaigns found.",
                    placeholder: "filter campaigns…",
                }
            }
        }
        ResearchNav::Campaign(campaign) => {
            let camp = snap.campaigns.iter().find(|c| c.name == campaign).cloned();
            let exps: Vec<ResourceSummary> = snap
                .experiments
                .iter()
                .filter(|e| e.campaign.as_deref() == Some(campaign.as_str()))
                .cloned()
                .collect();
            let reports: Vec<ReportSummary> = snap
                .reports
                .iter()
                .filter(|r| r.campaign_ref == campaign)
                .cloned()
                .collect();
            let exp_rows: Vec<Row> = exps
                .into_iter()
                .map(|e| {
                    let camp = campaign.clone();
                    let pick = e.clone();
                    let name = e.name.clone();
                    experiment_row(
                        &e,
                        rsx! { td {
                            button {
                                class: "row-link",
                                onclick: move |_| nav.set(ResearchNav::Experiment {
                                    campaign: camp.clone(),
                                    exp: pick.clone(),
                                }),
                                "{name}"
                            }
                        } },
                        false,
                    )
                })
                .collect();
            let report_rows: Vec<Row> = reports
                .into_iter()
                .map(|r| {
                    let camp = campaign.clone();
                    let pick = r.clone();
                    Row::new(r.name.clone())
                        .cell(
                            rsx! { td {
                                button {
                                    class: "row-link",
                                    onclick: move |_| nav.set(ResearchNav::Report {
                                        campaign: camp.clone(),
                                        report: pick.clone(),
                                    }),
                                    "{r.name}"
                                }
                            } },
                            Key::text(r.name.clone()),
                            &r.name,
                        )
                        .text(r.title.clone())
                        .text_class(r.phase.clone(), "phase")
                        .date(&r.created_at)
                })
                .collect();
            rsx! {
                div { class: "view-head",
                    h2 { title: "{campaign}", {camp.as_ref().map(campaign_label).unwrap_or_else(|| campaign.clone())} }
                    if let Some(c) = camp.clone() {
                        p {
                            span { class: "phase", "{c.phase}" }
                            span { class: "muted", " created {fmt_ms(&c.created_at)}" }
                            if let Some(t) = c.template.clone() { span { class: "chip", "template: {t}" } }
                            if let Some(st) = c.strategy.clone() { span { class: "chip", "strategy: {st}" } }
                            span { class: "muted", " {c.detail}" }
                        }
                        p { {cond_badges(&c.conditions)} }
                    }
                }
                h3 { "Experiments" }
                DataTable {
                    columns: experiment_columns(false),
                    rows: exp_rows,
                    initial: TableQuery::sorted("created", SortDir::Desc),
                    storage_key: Some("athena.table.research.experiments".to_string()),
                    empty: "No experiments in this campaign.",
                    placeholder: "filter experiments…",
                }
                if !report_rows.is_empty() {
                    h3 { "Reports" }
                    DataTable {
                        columns: vec![
                            DataColumnSpec::new("name", "Report").pinned(),
                            DataColumnSpec::new("title", "Title"),
                            DataColumnSpec::new("phase", "Phase"),
                            DataColumnSpec::new("created", "Created"),
                        ],
                        rows: report_rows,
                        initial: TableQuery::sorted("created", SortDir::Desc),
                    }
                }
            }
        }
        ResearchNav::Experiment { exp, .. } => {
            // Filmstrip via the public Panathenaia BFF. Only robot eval runs
            // produce one; non-robot experiments (audits, probes) 404, so the
            // image removes itself instead of rendering a broken icon.
            let film = format!(
                "https://spot.casazza.io/api/v1/experiments/{}/figures/eval_filmstrip.png",
                exp.name
            );
            let exp_open = exp.clone();
            rsx! {
                div { class: "view-head",
                    h2 { "{exp.name}" }
                    p { span { class: "phase", "{exp.phase}" }
                        if let Some(d) = exp.decision.clone() { span { class: "chip", "{d}" } }
                        if let Some(t) = exp.template.clone() { span { class: "chip", "template: {t}" } }
                        if let Some(p) = exp.parent.clone() { span { class: "chip", "parent: {p}" } }
                        span { class: "muted", " created {fmt_ms(&exp.created_at)} \u{b7} started {fmt_ms(&exp.started_at)} \u{b7} ended {fmt_ms(&exp.ended_at)}" }
                    }
                }
                if let Some(h) = exp.hypothesis.clone() {
                    p { "{h}" }
                }
                img {
                    class: "filmstrip",
                    src: "{film}",
                    alt: "",
                    "onerror": "this.remove()",
                }
                p {
                    button {
                        class: "btn",
                        onclick: move |_| {
                            let e = exp_open.clone();
                            selected.set(Some(e.clone()));
                            restore_detail_panels(emit);
                            spawn(async move {
                                match fetch_manifest(&e.namespace, &e.kind, &e.name).await {
                                    Ok(yaml) => manifest_doc.set(yaml),
                                    Err(err) => manifest_doc.set(format!("# failed to load manifest: {err}\n")),
                                }
                            });
                        },
                        "Open detail panels"
                    }
                }
            }
        }
        ResearchNav::Report { report, .. } => {
            let sections: Vec<(String, String)> = report.sections.clone().into_iter().collect();
            rsx! {
                div { class: "view-head",
                    h2 { {if report.title.is_empty() { report.name.clone() } else { report.title.clone() }} }
                    p { span { class: "phase", "{report.phase}" }
                        span { class: "muted", " {report.name}" } }
                }
                for (heading, text) in sections {
                    h3 { "{heading}" }
                    p { "{text}" }
                }
                if !report.seeded_hypotheses.is_empty() {
                    h3 { "Seeded hypotheses" }
                    ul {
                        for h in report.seeded_hypotheses.clone() {
                            li { "{h}" }
                        }
                    }
                }
            }
        }
    };

    rsx! {
        {crumbs}
        {body}
    }
}

fn reports_view(
    snap: ClusterSnapshot,
    emit: EventHandler<WorkspaceEvent<Panel>>,
    mut selected_campaign: Signal<Option<ResourceSummary>>,
    mut report_name: Signal<String>,
    mut report_title: Signal<String>,
) -> Element {
    let campaigns = snap.campaigns;
    let rows: Vec<Row> = snap
        .reports
        .into_iter()
        .map(|r| {
            let r2 = r.clone();
            let camps = campaigns.clone();
            Row::new(r.name.clone())
                .text(r.name.clone())
                .text(r.campaign_ref.clone())
                .text(r.title.clone())
                .text_class(r.phase.clone(), "phase")
                .date(&r.created_at)
                .cell(
                    rsx! { td { "{r.excluded_count}" } },
                    Key::num(r.excluded_count as f64),
                    "",
                )
                .cell(
                    rsx! { td {
                        button {
                            class: "btn",
                            onclick: move |_| {
                                if let Some(camp) = camps.iter().find(|c| c.name == r2.campaign_ref) {
                                    selected_campaign.set(Some(camp.clone()));
                                }
                                report_name.set(r2.name.clone());
                                report_title.set(r2.title.clone());
                                restore_panel(emit, Panel::ReportCurator);
                            },
                            "Load"
                        }
                    } },
                    Key::Missing,
                    "",
                )
        })
        .collect();
    rsx! {
        div { class: "view-head",
            h2 { "Reports" }
            p { "Published ResearchReport resources. Click Load to open one in the Report Curator." }
        }
        DataTable {
            columns: vec![
                DataColumnSpec::new("name", "Report").pinned(),
                DataColumnSpec::new("campaign", "Campaign"),
                DataColumnSpec::new("title", "Title"),
                DataColumnSpec::new("phase", "Phase"),
                DataColumnSpec::new("created", "Created"),
                DataColumnSpec::new("excluded", "Excluded"),
                DataColumnSpec::new("load", "").pinned().unsorted(),
            ],
            rows,
            initial: TableQuery::sorted("created", SortDir::Desc),
            storage_key: Some("athena.table.reports".to_string()),
            empty: "No reports found.",
            placeholder: "filter reports…",
        }
    }
}

/// Searchable campaign picker, newest first, grouped by owning drive, each
/// label carrying its creation date. A component (not a plain view fn) because
/// the dropdown's popup state is a hook.
#[component]
fn CampaignPicker(
    campaigns: Vec<ResourceSummary>,
    selected: String,
    on_pick: EventHandler<Option<ResourceSummary>>,
) -> Element {
    use panel_kit::widgets::{Dropdown, DropdownAction, DropdownItem, DropdownState};
    let state = use_signal(DropdownState::default);
    let mut sorted = campaigns.clone();
    sorted.sort_by_key(|c| {
        std::cmp::Reverse(
            c.created_at
                .as_deref()
                .and_then(|s| s.parse::<i64>().ok())
                .unwrap_or(i64::MIN),
        )
    });
    let items: Vec<DropdownItem> = sorted
        .iter()
        .map(|c| DropdownItem {
            value: c.name.clone(),
            label: format!("{} \u{b7} {}", campaign_label(c), fmt_ms(&c.created_at)),
            group: c.drive.clone().unwrap_or_else(|| "standalone".to_string()),
        })
        .collect();
    rsx! {
        Dropdown {
            items,
            state,
            selected,
            placeholder: "select a campaign…".to_string(),
            searchable: true,
            on_action: move |action: DropdownAction| {
                if let DropdownAction::Select { value } = action {
                    on_pick.call(campaigns.iter().find(|c| c.name == value).cloned());
                }
            },
        }
    }
}

/// Build a [`ReportSpecDto`] from curator form state. Shared by preview and save.
fn build_report_spec(
    campaign: &ResourceSummary,
    name: &str,
    title: &str,
    excluded: &HashSet<String>,
    sec_abstract: &str,
    sec_related_work: &str,
    sec_discussion: &str,
    sec_limitations: &str,
    seeds_text: &str,
) -> ReportSpecDto {
    let mut sections = BTreeMap::new();
    let pairs = [
        ("Abstract", sec_abstract),
        ("Related Work", sec_related_work),
        ("Discussion", sec_discussion),
        ("Limitations", sec_limitations),
    ];
    for (key, val) in pairs {
        let v = val.trim();
        if !v.is_empty() {
            sections.insert(key.to_string(), v.to_string());
        }
    }
    let seeded_hypotheses = seeds_text
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect();
    let title_opt = {
        let t = title.trim();
        if t.is_empty() {
            None
        } else {
            Some(t.to_string())
        }
    };
    ReportSpecDto {
        namespace: campaign.namespace.clone(),
        name: name.trim().to_string(),
        campaign_ref: campaign.name.clone(),
        title: title_opt,
        included_experiments: vec![],
        excluded_experiments: excluded.iter().cloned().collect(),
        sections,
        seeded_hypotheses,
    }
}

#[allow(clippy::too_many_arguments)]
fn report_curator_view(
    snap: ClusterSnapshot,
    mut selected_campaign: Signal<Option<ResourceSummary>>,
    mut report_name: Signal<String>,
    mut report_title: Signal<String>,
    mut excluded: Signal<HashSet<String>>,
    mut sec_abstract: Signal<String>,
    mut sec_related_work: Signal<String>,
    mut sec_discussion: Signal<String>,
    mut sec_limitations: Signal<String>,
    mut seeds_text: Signal<String>,
    mut preview_doc: Signal<String>,
    mut save_status: Signal<String>,
) -> Element {
    // Two separate Vec copies: one for the handler closure, one for iteration.
    let campaigns_for_change = snap.campaigns;
    let experiments = snap.experiments;

    let sel_campaign = selected_campaign.read().clone();
    let sel_name = sel_campaign
        .as_ref()
        .map(|c| c.name.clone())
        .unwrap_or_default();

    let exp_rows: Vec<ResourceSummary> = experiments
        .into_iter()
        .filter(|e| !sel_name.is_empty() && e.campaign.as_deref() == Some(sel_name.as_str()))
        .collect();

    let excluded_set = excluded.read().clone();
    let exp_table: Vec<Row> = exp_rows
        .into_iter()
        .map(|exp| {
            let exp_name = exp.name.clone();
            let is_included = !excluded_set.contains(&exp_name);
            Row::new(exp.name.clone())
                .cell(
                    rsx! { td {
                        input {
                            r#type: "checkbox",
                            checked: is_included,
                            onchange: move |_| {
                                let mut set = excluded.read().clone();
                                if !set.remove(&exp_name) {
                                    set.insert(exp_name.clone());
                                }
                                excluded.set(set);
                            }
                        }
                    } },
                    Key::Missing,
                    "",
                )
                .text(exp.name.clone())
                .text_class(exp.phase.clone(), "phase")
                .opt_text(exp.decision.clone())
                .date(&exp.created_at)
                .text_class(exp.detail.clone(), "muted")
        })
        .collect();

    rsx! {
        div { class: "view-head",
            h2 { "Report Curator" }
            p { "Compose a campaign's experiments into a research-paper dataset (ResearchReport)." }
        }

        div { class: "section-label", "Campaign" }
        CampaignPicker {
            campaigns: campaigns_for_change,
            selected: sel_name.clone(),
            on_pick: move |found: Option<ResourceSummary>| {
                selected_campaign.set(found);
                excluded.set(HashSet::new());
            },
        }

        div { class: "section-label", "Report Name" }
        input {
            class: "rc-input",
            r#type: "text",
            value: "{report_name()}",
            placeholder: "my-report-2025",
            oninput: move |e| report_name.set(e.value()),
        }
        div { class: "section-label", "Title (optional)" }
        input {
            class: "rc-input",
            r#type: "text",
            value: "{report_title()}",
            placeholder: "Human-readable paper title",
            oninput: move |e| report_title.set(e.value()),
        }

        div { class: "section-label", "Experiments" }
        if sel_name.is_empty() {
            p { class: "muted", "Select a campaign." }
        } else {
            DataTable {
                columns: vec![
                    DataColumnSpec::new("include", "Include").pinned().unsorted(),
                    DataColumnSpec::new("name", "Experiment").pinned(),
                    DataColumnSpec::new("phase", "Phase"),
                    DataColumnSpec::new("decision", "Decision"),
                    DataColumnSpec::new("created", "Created"),
                    DataColumnSpec::new("detail", "Detail"),
                ],
                rows: exp_table,
                initial: TableQuery::sorted("created", SortDir::Desc),
                storage_key: Some("athena.table.curator".to_string()),
                empty: "No experiments in this campaign.",
                placeholder: "filter experiments…",
            }
        }

        div { class: "section-label", "Abstract" }
        textarea {
            class: "rc-textarea",
            value: "{sec_abstract()}",
            oninput: move |e| sec_abstract.set(e.value()),
        }
        div { class: "section-label", "Related Work" }
        textarea {
            class: "rc-textarea",
            value: "{sec_related_work()}",
            oninput: move |e| sec_related_work.set(e.value()),
        }
        div { class: "section-label", "Discussion" }
        textarea {
            class: "rc-textarea",
            value: "{sec_discussion()}",
            oninput: move |e| sec_discussion.set(e.value()),
        }
        div { class: "section-label", "Limitations" }
        textarea {
            class: "rc-textarea",
            value: "{sec_limitations()}",
            oninput: move |e| sec_limitations.set(e.value()),
        }
        div { class: "section-label", "Seeded Hypotheses (one per line)" }
        textarea {
            class: "rc-textarea",
            value: "{seeds_text()}",
            oninput: move |e| seeds_text.set(e.value()),
        }

        div { style: "display:flex;gap:.5rem;margin:.5rem 0;",
            button {
                class: "btn",
                onclick: move |_| {
                    let sel = selected_campaign.read().clone();
                    let rn = report_name.read().clone();
                    let rt = report_title.read().clone();
                    let ex = excluded.read().clone();
                    let sa = sec_abstract.read().clone();
                    let srw = sec_related_work.read().clone();
                    let sd = sec_discussion.read().clone();
                    let sl = sec_limitations.read().clone();
                    let st = seeds_text.read().clone();
                    let camp = match sel {
                        None => {
                            preview_doc.set("Select a campaign first.".to_string());
                            return;
                        }
                        Some(c) => c,
                    };
                    if rn.trim().is_empty() {
                        preview_doc.set("Enter a report name.".to_string());
                        return;
                    }
                    let dto = build_report_spec(&camp, &rn, &rt, &ex, &sa, &srw, &sd, &sl, &st);
                    spawn(async move {
                        match preview_report(dto).await {
                            Ok(md) => preview_doc.set(md),
                            Err(e) => preview_doc.set(format!("Preview error: {e}")),
                        }
                    });
                },
                "Preview Dossier"
            }
            button {
                class: "btn",
                onclick: move |_| {
                    let sel = selected_campaign.read().clone();
                    let rn = report_name.read().clone();
                    let rt = report_title.read().clone();
                    let ex = excluded.read().clone();
                    let sa = sec_abstract.read().clone();
                    let srw = sec_related_work.read().clone();
                    let sd = sec_discussion.read().clone();
                    let sl = sec_limitations.read().clone();
                    let st = seeds_text.read().clone();
                    let camp = match sel {
                        None => {
                            save_status.set("Select a campaign first.".to_string());
                            return;
                        }
                        Some(c) => c,
                    };
                    if rn.trim().is_empty() {
                        save_status.set("Enter a report name.".to_string());
                        return;
                    }
                    let dto = build_report_spec(&camp, &rn, &rt, &ex, &sa, &srw, &sd, &sl, &st);
                    spawn(async move {
                        match save_report(dto).await {
                            Ok(s) => save_status.set(format!("Saved: {} ({})", s.name, s.phase)),
                            Err(e) => save_status.set(format!("Save error: {e}")),
                        }
                    });
                },
                "Save Report"
            }
        }

        pre { class: "preview", "{preview_doc}" }
        p { class: "err", "{save_status}" }
    }
}

// ---------------------------------------------------------------------------
// Admin views (GPU-scheduling / inference stack) — read from SchedulingSnapshot.
// ---------------------------------------------------------------------------

fn gpu_pools_view(snap: SchedulingSnapshot) -> Element {
    rsx! {
        div { class: "view-head",
            h2 { "GPU Pools & Queues" }
            p { "Kueue admission — quota vs live usage, and the workload queue (preemption in play)." }
        }
        for p in snap.pools.iter() {
            dl { class: "detail-grid",
                dt { "Pool" }
                dd { "{p.name}" }
                dt { "GPU" }
                dd { "{p.gpu_used} / {p.gpu_nominal} used" }
                dt { "CPU" }
                dd { "{p.cpu_used} / {p.cpu_nominal}" }
                dt { "Queue" }
                dd { "{p.admitted_workloads} admitted · {p.pending_workloads} pending" }
            }
        }
        DataTable {
            columns: vec![
                DataColumnSpec::new("name", "Workload").pinned(),
                DataColumnSpec::new("ns", "NS"),
                DataColumnSpec::new("queue", "Queue"),
                DataColumnSpec::new("priority", "Priority"),
                DataColumnSpec::new("state", "State"),
                DataColumnSpec::new("gpu", "GPU"),
            ],
            storage_key: Some("athena.table.workloads".to_string()),
            rows: snap
                .workloads
                .iter()
                .filter(|w| w.state != "Finished")
                .map(|w| {
                    let prio = if w.priority_class.is_empty() { "default" } else { w.priority_class.as_str() };
                    Row::new(format!("{}/{}", w.namespace, w.name))
                        .text(w.name.clone())
                        .text(w.namespace.clone())
                        .text(w.queue.clone())
                        .text_class(prio, "phase")
                        .text(w.state.clone())
                        .cell(rsx! { td { "{w.gpus}" } }, Key::num(w.gpus as f64), "")
                })
                .collect::<Vec<Row>>(),
            empty: "No active workloads.",
            placeholder: "filter workloads…",
        }
    }
}

fn node_power_view(snap: SchedulingSnapshot) -> Element {
    rsx! {
        div { class: "view-head",
            h2 { "Node Power" }
            p { "Hephaestus scale-from-zero — physical GPU nodes powered on/off." }
        }
        table { class: "tbl",
            thead {
                tr {
                    th { "Node" }
                    th { "Power" }
                    th { "Phase" }
                    th { "Pool" }
                }
            }
            tbody {
                for n in snap.nodes.iter() {
                    tr {
                        td { "{n.name}" }
                        td { class: "phase", {if n.powered { "● ON" } else { "○ off" }} }
                        td { "{n.phase}" }
                        td { "{n.pool}" }
                    }
                }
            }
        }
    }
}

fn inference_view(snap: SchedulingSnapshot) -> Element {
    if snap.inference.is_empty() {
        return rsx! {
            div { class: "view-head", h2 { "Inference Backends" } }
            p { class: "muted",
                "No ephemeral inference backend active. Campaigns with inferenceMesh / inferenceCluster appear here while serving."
            }
        };
    }
    rsx! {
        div { class: "view-head",
            h2 { "Inference Backends" }
            p { "Ephemeral per-campaign endpoints (mesh-llm / vLLM-on-Ray)." }
        }
        table { class: "tbl",
            thead {
                tr {
                    th { "Campaign" }
                    th { "Kind" }
                    th { "Serving" }
                    th { "Endpoint" }
                }
            }
            tbody {
                for b in snap.inference.iter() {
                    tr {
                        td { "{b.campaign}" }
                        td { "{b.kind}" }
                        td { class: "phase", {if b.serving { "● serving" } else { "… starting" }} }
                        td { "{b.endpoint}" }
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Data layer (frontend side): plain reqwest fetches to the axum backend.
// ---------------------------------------------------------------------------

/// Browser origin (`https://host:port`) for building absolute request URLs;
/// empty string outside a browser.
fn api_base() -> String {
    web_sys::window()
        .and_then(|w| w.location().origin().ok())
        .unwrap_or_default()
}

/// `GET /api/snapshot` → the full [`ClusterSnapshot`].
async fn fetch_snapshot() -> Result<ClusterSnapshot, String> {
    let url = format!("{}/api/snapshot", api_base());
    reqwest::get(&url)
        .await
        .map_err(|e| e.to_string())?
        .json::<ClusterSnapshot>()
        .await
        .map_err(|e| e.to_string())
}

/// `GET /api/scheduling` → the [`SchedulingSnapshot`] for the admin page.
async fn fetch_scheduling() -> Result<SchedulingSnapshot, String> {
    let url = format!("{}/api/scheduling", api_base());
    reqwest::get(&url)
        .await
        .map_err(|e| e.to_string())?
        .json::<SchedulingSnapshot>()
        .await
        .map_err(|e| e.to_string())
}

/// `GET /api/manifest/{ns}/{kind}/{name}` → resource manifest YAML.
async fn fetch_manifest(namespace: &str, kind: &str, name: &str) -> Result<String, String> {
    let url = format!("{}/api/manifest/{namespace}/{kind}/{name}", api_base());
    reqwest::get(&url)
        .await
        .map_err(|e| e.to_string())?
        .text()
        .await
        .map_err(|e| e.to_string())
}

/// `GET /api/template/{ns}/{name}` → ExperimentTemplate YAML.
async fn fetch_template_yaml(namespace: &str, name: &str) -> Result<String, String> {
    let url = format!("{}/api/template/{namespace}/{name}", api_base());
    reqwest::get(&url)
        .await
        .map_err(|e| e.to_string())?
        .text()
        .await
        .map_err(|e| e.to_string())
}

/// `POST /api/reports` — persist a ResearchReport spec, returns the summary row.
async fn save_report(dto: ReportSpecDto) -> Result<ReportSummary, String> {
    let url = format!("{}/api/reports", api_base());
    let resp = reqwest::Client::new()
        .post(&url)
        .json(&dto)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(resp.text().await.unwrap_or_else(|e| e.to_string()));
    }
    resp.json::<ReportSummary>()
        .await
        .map_err(|e| e.to_string())
}

/// `POST /api/reports/preview` — compose the dossier Markdown; nothing persisted.
async fn preview_report(dto: ReportSpecDto) -> Result<String, String> {
    let url = format!("{}/api/reports/preview", api_base());
    let resp = reqwest::Client::new()
        .post(&url)
        .json(&dto)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(resp.text().await.unwrap_or_else(|e| e.to_string()));
    }
    resp.text().await.map_err(|e| e.to_string())
}
