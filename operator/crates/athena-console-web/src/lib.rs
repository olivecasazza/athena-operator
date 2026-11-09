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

mod activity;
mod api;
mod auth;
mod auth_flow;
mod curator;
pub mod models;
mod tables;
mod workspace;

use dioxus::events::PointerEvent as DioxusPointerEvent;
use dioxus::prelude::*;
use models::{
    ClusterSnapshot, ConditionDto, ReportDetailDto, ReportSpecDto, ResourceSummary,
    SchedulingSnapshot, TemplateSummary,
};
use panel_kit::grafana::GrafanaDashboard;
use panel_kit::ide::IdePanel;
use panel_kit::widgets::DataColumnSpec;
use panel_kit::widgets::data_table::SortSpec;
use panel_kit::{LayoutBuilder, PanelKind, PanelWin};
use panel_kit_core::Mode;
use panel_kit_core::PanelCommand;
use panel_kit_core::reducer::WorkspaceEvent;
use panel_kit_core::widgets::data_table::SortDir;
use serde::{Deserialize, Serialize};
use tables::{
    ConsoleTable, DataRow as Row, SortKey as Key, campaign_columns, campaign_row,
    experiment_columns, experiment_row, fmt_ms,
};
use workspace::{PanelWorkspace, ViewPreset, ViewsSpec};
use workspace::{
    handle_key, handle_pointer_cancel, handle_pointer_move, handle_pointer_up, handle_wheel,
    mount_viewport_observer, project_workspace, use_panel_workspace, workspace_area_class,
    workspace_contents, workspace_event_handler,
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
    /// Observed run milestones, as a graph or an expandable trace rail.
    RunActivity,
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
            Panel::RunActivity => "Run Activity",
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

/// Register every story's panel; named views decide which are visible.
fn default_layout() -> Vec<PanelWin<Panel>> {
    let mut b = LayoutBuilder::new();
    column(
        &mut b,
        GUTTER,
        960.0,
        4,
        &[
            (Panel::Research, 640.0),
            (Panel::Experiments, 460.0),
            (Panel::ExperimentDetail, 360.0),
            (Panel::RunActivity, 440.0),
            (Panel::ExperimentMetrics, 560.0),
            (Panel::ExperimentManifest, 360.0),
            (Panel::Reports, 320.0),
            (Panel::ReportCurator, 520.0),
            (Panel::Templates, 480.0),
            (Panel::Benchmarks, 320.0),
            (Panel::RuntimeProfiles, 260.0),
        ],
    )
}

/// Named views over the research workspace: which panels are visible, their
/// tile spans (4-column grid, row-major shelves), and mode. Every other panel
/// is minimized to the dock. Spans are relative: tiles stretch to fill the
/// viewport, so shelf heights set proportions, not pixels.
static RESEARCH_VIEWS: ViewsSpec<Panel> = ViewsSpec {
    base: "athena_console_web_v5",
    presets: &[
        ViewPreset {
            name: "Research",
            mode: Mode::Tiling,
            panels: &[
                (Panel::Research, 2, 4),
                (Panel::ExperimentMetrics, 2, 4),
                (Panel::ExperimentDetail, 2, 3),
                (Panel::RunActivity, 2, 3),
            ],
        },
        ViewPreset {
            name: "Experiments",
            mode: Mode::Tiling,
            panels: &[
                (Panel::Experiments, 2, 4),
                (Panel::ExperimentMetrics, 2, 4),
                (Panel::ExperimentDetail, 2, 3),
                (Panel::RunActivity, 2, 3),
                (Panel::ExperimentManifest, 4, 3),
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

/// Story-owned layout and shared achromatic panel-kit tokens.
const APP_CSS: &str = "
:root {
  --gray-0:#101010; --gray-1:#181818; --gray-2:#242424;
  --gray-3:#363636; --gray-4:#525252; --gray-5:#737373;
  --gray-6:#bdbdbd; --gray-7:#dedede; --gray-8:#fafafa;
  --color-background:var(--gray-0); --color-surface:var(--gray-1);
  --color-text:var(--gray-8); --color-text-muted:var(--gray-6);
  --color-border:var(--gray-4); --color-divider:var(--gray-3);
  --color-inverse-background:var(--gray-8); --color-inverse-text:var(--gray-0);
  --color-primary:var(--gray-8); --color-secondary:var(--gray-7);
  --color-tertiary:var(--gray-6); --color-highlight:var(--gray-8);
  --color-success:var(--gray-8); --color-error:var(--gray-8);
  --color-warning:var(--gray-7); --color-failure:var(--gray-8);
  --color-info:var(--gray-7); --color-focus-ring:var(--gray-8);
  --panel-radius:0; --badge-radius:0;
  --hit-min:44px;
}
button, input, select, textarea, a { min-height:44px; border-radius:0; box-sizing:border-box; }
button, input[type=checkbox] { min-width:44px; }
:focus-visible { outline:2px solid var(--color-focus-ring); outline-offset:2px; }
::selection { background:var(--color-inverse-background); color:var(--color-inverse-text); }
input, textarea { caret-color:var(--color-text); }
a { color:var(--color-text); display:inline-flex; align-items:center; padding:0 .3rem; }
.panel-body { scrollbar-color:var(--gray-6) var(--gray-1); }
.pk-dt { font-variant-numeric:tabular-nums; }
.metadata-chip { display:inline-flex; align-items:center; min-height:44px; }
.metadata-chip .badge-main { margin:0; min-height:44px; min-width:44px; }
.pk-dt button { min-height:44px; min-width:44px; }
.pk-tr-legend-means, .pk-tr-faint, .pk-tr-rail-meta { color:var(--color-text-muted); }
.panel-body button.pk-tr-rail-hit { min-height:0; }
.dock { height:52px; flex-basis:52px; }
.activity-modes { display:flex; flex-wrap:wrap; gap:.5rem; margin:.5rem 0; }
.topbar { display:flex; align-items:center; gap:1rem; padding:.5rem .9rem;
  height:auto; min-height:52px; flex:0 0 auto; border-bottom:1px solid var(--color-border); }
.topbar h1 { font-size:1rem; color:var(--color-text); margin:0; flex-shrink:0; }
.topbar .hint { color:var(--color-text-muted); font-size:.72rem; }
.views { display:flex; align-items:center; gap:.3rem; flex:1; min-width:0; flex-wrap:wrap; }
.view-tab { display:inline-flex; }
.view-btn.active-view { background:var(--color-inverse-background); color:var(--color-inverse-text); border-color:var(--color-inverse-background); }
.view-del { padding:0 .3rem; border-left:0; }
.views .view-name { width:8rem; flex:0 0 8rem; }
/* Report Curator. Follows panel-kit's design rules: labels are tracked caps,
   hierarchy is weight/case not colour, accent only for live state. */
.cur { display:flex; flex-direction:column; gap:.45rem; min-height:100%; }
.cur-subject, .cur-reports, .cur-bulk, .cur-add, .cur-footer {
  display:flex; align-items:center; gap:.4rem; flex-wrap:wrap; }
.cur-label { font-size:.68rem; text-transform:uppercase; letter-spacing:.06em;
  color:var(--color-text-muted); min-width:5.5rem; }
.cur-value { color:var(--color-text); font-weight:600; }
.cur-hint { font-size:.68rem; text-transform:uppercase; letter-spacing:.06em; }
.cur-report { max-width:22ch; overflow:hidden; text-overflow:ellipsis; white-space:nowrap; }
.cur-report.active, .cur-tab.active { color:var(--color-text); border-color:var(--color-text); }
.cur-tabs { display:flex; gap:0; border-bottom:1px solid var(--color-border); }
.cur-tab { background:transparent; border:0; border-bottom:2px solid transparent;
  color:var(--color-text-muted); font-family:var(--mono); font-size:.68rem; text-transform:uppercase;
  letter-spacing:.06em; padding:.3rem .6rem; cursor:pointer; margin-bottom:-1px; }
.cur-tab:hover { color:var(--color-text); }
.cur-count { margin-left:.35rem; color:var(--color-text-muted); }
.cur-body { flex:1; min-height:0; display:flex; flex-direction:column; gap:.4rem; }
.cur-section { border:1px solid var(--color-divider); padding:.35rem; display:flex;
  flex-direction:column; gap:.3rem; }
.cur-section-head { display:flex; gap:.4rem; align-items:center; }
.cur-heading { font-weight:600; }
.cur-text { min-height:7rem; resize:vertical; }
.cur-text.tall { min-height:16rem; }
.cur-footer { position:sticky; bottom:0; background:var(--color-surface);
  border-top:1px solid var(--color-border); padding:.35rem 0 .1rem; margin-top:auto; }
.cur-name { width:18ch; flex:0 0 18ch; }
.cur-title { flex:1; min-width:12ch; width:auto; }
.cur-dirty { color:var(--color-warning); font-size:.68rem; text-transform:uppercase; letter-spacing:.06em; }
.btn.primary:not(:disabled) { border-color:var(--color-text); color:var(--color-text); }
.btn:disabled { color:var(--color-text-muted); border-color:var(--color-border); cursor:not-allowed; }
.ok { color:var(--color-success); font-size:.74rem; }
.views { flex-wrap:nowrap; overflow-x:auto; }
.pk-dt td .row-link { font-size:inherit; }
.row-link { background:none; border:none; color:var(--color-text); cursor:pointer;
  font-family:var(--mono); font-size:.74rem; padding:.2rem .3rem; text-align:left; text-decoration:underline; text-underline-offset:3px; }
.row-link:hover { text-decoration:underline; }
.muted { color:var(--color-text-muted); }
.view-head { margin:.1rem 0 .6rem; }
.view-head h2 { font-size:.92rem; color:var(--color-text); margin:0 0 .2rem; }
.view-head p { font-size:.72rem; color:var(--color-text-muted); margin:0; }
.detail-grid { display:grid; grid-template-columns:auto 1fr; gap:.2rem .8rem;
  font-size:.74rem; margin-bottom:.6rem; }
.detail-grid dt { color:var(--color-text-muted); }
.detail-grid dd { color:var(--color-text); margin:0; word-break:break-all; }
.btn { background:var(--color-background); color:var(--color-text); border:1px solid var(--color-divider);
  border-radius:0; padding:.3rem .5rem; font-size:.7rem; cursor:pointer; font-family:var(--mono); }
.admin-toggle { margin-left:auto; }
.btn:hover { border-color:var(--color-text); }
.section-label { font-size:.78rem; color:var(--color-text); margin:.5rem 0 .25rem; }
/* The Grafana embed tracks the panel's height instead of a fixed 340px, so
   resizing/maximizing the panel actually gives the dashboard more room (it is
   ~1400px tall and scrolls internally otherwise). 100% resolves against
   .panel-body's CONTENT box, so its padding is already excluded; the collapse
   floor is panel-kit's own .grafana-panel min-height. */
.embed-block { height:100%; margin:0; }
.ide-block { height:260px; margin:.4rem 0; }
.todo { color:var(--color-warning); font-size:.74rem; }
.err { color:var(--color-error); font-size:.76rem; }
.preview { max-height:300px; overflow:auto; white-space:pre-wrap;
  font-family:var(--mono); font-size:.7rem; background:var(--color-background);
  border:1px solid var(--color-border); padding:.4rem; margin:.4rem 0; }
.rc-select { background:var(--color-background); color:var(--color-text); border:1px solid var(--color-divider);
  font-family:var(--mono); font-size:.74rem; padding:.15rem .3rem; width:100%; }
.rc-input { background:var(--color-background); color:var(--color-text); border:1px solid var(--color-divider);
  font-family:var(--mono); font-size:.74rem; padding:.15rem .3rem; width:100%;
  box-sizing:border-box; }
.rc-textarea { background:var(--color-background); color:var(--color-text); border:1px solid var(--color-divider);
  font-family:var(--mono); font-size:.72rem; padding:.2rem .3rem; width:100%;
  box-sizing:border-box; height:4rem; resize:vertical; }
/* Breadcrumbs lead back to the one campaign browser. */
.crumbs { display:flex; align-items:baseline; gap:.3rem; font-size:.74rem;
  font-family:var(--mono); margin:.1rem 0 .6rem; }
.pk-dt .date { white-space:nowrap; color:var(--color-text-muted); }
.pk-dt td .row-link { max-width:100%; overflow:hidden; text-overflow:ellipsis; white-space:nowrap; display:inline-block; vertical-align:bottom; }
.filmstrip { width:100%; margin:.2rem 0; border:1px solid var(--color-border); }
.crumb-current { color:var(--color-text); }
@media(max-width:700px) {
  .topbar { flex-wrap:wrap; gap:.5rem; }
  .views { flex-basis:100%; flex-wrap:wrap; overflow:visible; }
  .cur-tabs { flex-wrap:wrap; }
  .cur-name, .cur-title { width:100%; flex-basis:100%; }
  .detail-grid { grid-template-columns:1fr; }
}
";

/// One campaign browser; experiment and report rows open their canonical
/// inspector/curator instead of creating another detail surface.
#[derive(Clone, PartialEq)]
enum ResearchNav {
    Global,
    Campaign { namespace: String, name: String },
}

/// App root.
#[component]
pub fn App() -> Element {
    // New story-owned panel catalog: obsolete campaign/leaf panels are not
    // restored from earlier saved layouts.
    let ws = use_panel_workspace(
        "athena_console_web_v5_layout",
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
    let mut snapshot = use_resource(move || async move { fetch_snapshot().await });
    // Scheduling/inference stack snapshot for the admin page.
    let sched = use_resource(move || async move { fetch_scheduling().await });

    // External selection state for the data-less ExperimentDetail panel.
    let selected = use_signal(|| Option::<ResourceSummary>::None);
    // Editable manifest / template documents for the IdePanels.
    let manifest_doc = use_signal(|| "# Select an experiment to load its manifest.\n".to_string());
    let template_doc = use_signal(|| "# Load a template to view its YAML.\n".to_string());

    // Report Curator state (draft, loaded report, tab, save outcome).
    let curator_state = use_signal(curator::CuratorState::default);

    // Research drill-down navigation. `Panel` variants carry no data (PanelKind
    // is Copy), so the current depth — global fleet, one campaign, one
    // experiment, one report — lives out here exactly like `selected` does for
    // ExperimentDetail. The breadcrumb bar is the rendered form of this signal.
    let research_nav = use_signal(|| ResearchNav::Global);

    // Sign-in state. Drives the top-bar control and gates the Curator's save
    // button: reads stay open, so a signed-out visitor browses normally and
    // only hits auth when it tries to write a ResearchReport.
    let auth_state = use_resource(|| async { resolve_auth_phase().await });

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
            Panel::RunActivity => rsx! { activity::RunActivity { snapshot: snap, selected } },
            Panel::Templates => templates_view(snap, template_doc),
            Panel::RuntimeProfiles => runtime_view(snap),
            Panel::Benchmarks => benchmarks_view(snap),
            Panel::ReportCurator => {
                let nav_campaign = match &*research_nav.read() {
                    ResearchNav::Global => None,
                    ResearchNav::Campaign { name, .. } => Some(name.clone()),
                };
                curator::curator_view(
                    snap,
                    curator_state,
                    selected,
                    nav_campaign,
                    EventHandler::new(move |_| snapshot.restart()),
                    read_phase(&auth_state.read()),
                )
            }
            Panel::Reports => reports_view(snap, ws_emit, curator_state),
            Panel::Research => research_view(
                snap,
                research_nav,
                selected,
                manifest_doc,
                ws_emit,
                curator_state,
            ),
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
                    handle_pointer_cancel(&admin_pointer_cancel_workspace, &event)
                },
                onkeydown: move |event| handle_key(&admin_key_workspace, &event),
                header { class: "topbar",
                    h1 { "Athena Console · Admin" }
                    span { class: "hint", "GPU scheduling · Kueue · inference · Hephaestus — read-only" }
                    {auth_bar(&read_phase(&auth_state.read()))}
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
                    handle_pointer_cancel(&pointer_cancel_workspace, &event)
                },
                onkeydown: move |event| handle_key(&key_workspace, &event),
                header { class: "topbar",
                    h1 { "Athena Console" }
                    {view_bar(&ws, view_draft, view_err)}
                    {auth_bar(&read_phase(&auth_state.read()))}
                    button { class: "btn admin-toggle", onclick: move |_| admin.set(true), "Admin" }
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

/// Current sign-in state as a plain value. A pending or errored resource read
/// collapses to `Loading`, so a failed config fetch never claims a session.
fn read_phase(value: &Option<Result<auth_flow::Phase, String>>) -> auth_flow::Phase {
    match value {
        Some(Ok(phase)) => phase.clone(),
        _ => auth_flow::Phase::Loading,
    }
}

/// Resolve the sign-in state on first paint and on every callback return:
/// finish an `?code=…` exchange, or fall back to a stored token.
async fn resolve_auth_phase() -> Result<auth_flow::Phase, String> {
    let config = fetch_auth_config().await?;
    if !config.enabled {
        return Ok(auth_flow::Phase::Disabled);
    }

    if let Some(message) = auth::callback_error() {
        auth::discard_pending();
        auth::clear_callback_query();
        return Ok(failed_phase(&config, message));
    }

    // Capture the callback before scrubbing the URL: `finish_callback` needs
    // both parameters, and a reload must not replay the code.
    let callback = match (auth::callback_code(), auth::callback_state()) {
        (Some(code), state) => Some((code, state)),
        _ => None,
    };
    auth::clear_callback_query();
    if let Some((code, state)) = callback {
        return finish_callback(&config, &code, state.as_deref()).await;
    }
    // A token the backend already refused is worse than none: drop it and
    // offer sign-in rather than retrying it.
    if auth::take_rejected() {
        auth::clear_token();
        return Ok(expired_phase(&config, "your session is no longer valid"));
    }

    if auth::bearer_token(auth::now_seconds()).is_some() {
        return Ok(auth_flow::Phase::SignedIn);
    }
    Ok(signed_out_phase(&config))
}

/// Exchange the callback code, then report the resulting phase.
async fn finish_callback(
    config: &auth::AuthConfig,
    code: &str,
    returned_state: Option<&str>,
) -> Result<auth_flow::Phase, String> {
    if let Err(message) = auth_flow::check_state(returned_state.unwrap_or_default()) {
        return Ok(failed_phase(config, message));
    }
    let Some(verifier) = auth::take_verifier() else {
        return Ok(failed_phase(
            config,
            "PKCE verifier was lost; sign in again".into(),
        ));
    };
    let Some(redirect_uri) = auth::current_url() else {
        return Ok(failed_phase(
            config,
            "cannot determine the redirect URI".into(),
        ));
    };
    let body = auth::token_request_body(code, &verifier, &redirect_uri, &config.client_id);
    match exchange_token(config, &body).await {
        Ok(token) => {
            auth::store_token(&token);
            Ok(auth_flow::Phase::SignedIn)
        }
        Err(message) => Ok(failed_phase(config, message)),
    }
}

async fn exchange_token(config: &auth::AuthConfig, body: &str) -> Result<auth::Token, String> {
    let url = format!(
        "{}/protocol/openid-connect/token",
        config.issuer.trim_end_matches('/')
    );
    let resp = reqwest::Client::new()
        .post(url)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(body.to_string())
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let status = resp.status().as_u16();
    let text = resp.text().await.unwrap_or_default();
    if !(200..300).contains(&status) {
        return Err(auth_flow::token_error(status, &text));
    }
    auth::parse_token(&text, auth::now_seconds())
}

/// A phase offering a fresh sign-in link for the current page.
fn signed_out_phase(config: &auth::AuthConfig) -> auth_flow::Phase {
    let redirect_uri = auth::current_url().unwrap_or_default();
    auth_flow::Phase::SignedOut {
        sign_in: auth_flow::begin(config, &redirect_uri),
    }
}

/// A failed phase that still offers another attempt when one can be built.
fn failed_phase(config: &auth::AuthConfig, message: String) -> auth_flow::Phase {
    let redirect_uri = auth::current_url().unwrap_or_default();
    auth_flow::Phase::Failed {
        message,
        sign_in: auth_flow::begin(config, &redirect_uri),
    }
}

/// `GET /api/auth/config` — public OIDC parameters for the public client.
async fn fetch_auth_config() -> Result<auth::AuthConfig, String> {
    let url = format!("{}/api/auth/config", api_base());
    reqwest::get(&url)
        .await
        .map_err(|e| e.to_string())?
        .json::<auth::AuthConfig>()
        .await
        .map_err(|e| e.to_string())
}

/// A rejected/expired session that still offers another sign-in attempt.
fn expired_phase(config: &auth::AuthConfig, message: &str) -> auth_flow::Phase {
    let redirect_uri = auth::current_url().unwrap_or_default();
    auth_flow::Phase::Expired {
        message: message.to_string(),
        sign_in: auth_flow::begin(config, &redirect_uri),
    }
}

/// Top-bar sign-in control: shows session state, and when a flow can start,
/// navigates to the identity provider. Silent when auth is off or a session is
/// live, so a working console carries no extra chrome.
fn auth_bar(phase: &auth_flow::Phase) -> Element {
    if let Some((label, title)) = phase.notice() {
        return rsx! {
            span { class: "hint", title: "{title}",
                {metadata_chip("Session", label)}
            }
        };
    }
    if let Some(url) = auth_flow::sign_in_url(phase) {
        return rsx! {
            button {
                class: "btn",
                title: "sign in through the identity provider to save reports",
                onclick: move |_| auth::navigate_to(&url),
                "Sign in"
            }
        };
    }
    rsx! {}
}

fn restore_detail_panels(emit: EventHandler<WorkspaceEvent<Panel>>) {
    restore_panel(emit, Panel::ExperimentDetail);
    restore_panel(emit, Panel::ExperimentMetrics);
    restore_panel(emit, Panel::ExperimentManifest);
    restore_panel(emit, Panel::RunActivity);
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
    let sel = selected.read().as_ref().map(ResourceSummary::manifest_path);
    let rows: Vec<Row> = snap
        .experiments
        .into_iter()
        .map(|exp| {
            let pick = exp.clone();
            let is_sel = sel.as_deref() == Some(exp.manifest_path().as_str());
            let name = exp.name.clone();
            experiment_row(
                &exp,
                rsx! { span {
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
        ConsoleTable {
            id: "athena-experiments",
            columns: experiment_columns(true),
            rows,
            sort: SortSpec::new("created", SortDir::Desc),
            storage_key: "athena.table.experiments".to_string(),
            empty: "No experiments found.".to_string(),
            placeholder: "filter experiments…".to_string(),
        }
    }
}

/// The inspector is the only experiment/campaign detail destination.
fn no_selection() -> Element {
    rsx! { p { class: "muted", "Select a campaign in Research or a run in Experiments." } }
}

fn experiment_detail_view(selected: Signal<Option<ResourceSummary>>) -> Element {
    let Some(exp) = selected.read().clone() else {
        return no_selection();
    };
    let manifest_path = exp.manifest_path();
    rsx! {
        div { class: "view-head",
            h2 { "{exp.name}" }
            p { "{exp.namespace} / {exp.kind}" }
        }
        {metadata_chip("Phase", &exp.phase)}
        {cond_badges(&exp.conditions)}
        if let Some(hypothesis) = &exp.hypothesis { p { "{hypothesis}" } }
        dl { class: "detail-grid",
            dt { "Detail" } dd { "{exp.detail}" }
            dt { "Created" } dd { "{fmt_ms(&exp.created_at)} UTC" }
            dt { "Started" } dd { "{fmt_ms(&exp.started_at)} UTC" }
            dt { "Ended" } dd { "{fmt_ms(&exp.ended_at)} UTC" }
            dt { "Workspace" } dd { {exp.workspace_path.as_deref().unwrap_or("Not reported")} }
            dt { "Manifest" } dd { "{manifest_path}" }
        }
        if let Some(value) = &exp.decision { {metadata_chip("Decision", value)} }
        if let Some(value) = &exp.template { {metadata_chip("Template", value)} }
        if let Some(value) = &exp.parent { {metadata_chip("Parent", value)} }
        if let Some(url) = &exp.logs_link {
            a { href: "{url}", target: "_blank", rel: "noopener noreferrer", "Open logs" }
        }
        if let Some(url) = &exp.metrics_link {
            a { href: "{url}", target: "_blank", rel: "noopener noreferrer", "Open metrics" }
        }
        if exp.kind == "experiment" {
            img {
                class: "filmstrip",
                src: "https://spot.casazza.io/api/v1/experiments/{exp.name}/figures/eval_filmstrip.png",
                alt: "Evaluation filmstrip for {exp.name}",
                "onerror": "this.remove()",
            }
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

/// Select a resource from the experiment table or campaign browser, then
/// open its single inspector, metrics, manifest and activity destinations.
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
pub(crate) fn campaign_label(c: &ResourceSummary) -> String {
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
            Row::new(format!("{}/{}", tpl.namespace, tpl.name))
                .text(tpl.name.clone())
                .text(tpl.objective.clone())
                .date(&tpl.created_at)
                .text_class(tpl.detail.clone(), "muted")
                .cell(
                    rsx! { span {
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
                )
        })
        .collect();
    rsx! {
        div { class: "view-head",
            h2 { "Experiment Templates" }
            p { "Load Kubernetes-owned template YAML and inspect objectives + sources." }
        }
        ConsoleTable {
            id: "athena-templates",
            columns: vec![
                DataColumnSpec::new("name", "Template").pinned(),
                DataColumnSpec::new("objective", "Objective"),
                DataColumnSpec::new("created", "Created").number(),
                DataColumnSpec::new("source", "Source"),
                DataColumnSpec::new("load", "").pinned().unsorted(),
            ],
            rows,
            sort: SortSpec::new("name", SortDir::Asc),
            storage_key: "athena.table.templates".to_string(),
            empty: "No templates found.".to_string(),
            placeholder: "filter templates…".to_string(),
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
            Row::new(r.manifest_path())
                .text(r.name.clone())
                .text_class(r.phase.clone(), "phase")
                .date(&r.created_at)
                .text_class(r.detail.clone(), "muted")
        })
        .collect();
    rsx! {
        ConsoleTable {
            id: format!("athena-{}", name_col.to_lowercase()),
            columns: vec![
                DataColumnSpec::new("name", name_col).pinned(),
                DataColumnSpec::new("phase", "Phase").enumerated(),
                DataColumnSpec::new("created", "Created").number(),
                DataColumnSpec::new("detail", detail_col),
            ],
            rows,
            sort: SortSpec::new("created", SortDir::Desc),
            storage_key: format!("athena.table.{}", name_col.to_lowercase()),
        }
    }
}

// ---------------------------------------------------------------------------
// Research drill-down: global fleet -> campaign -> experiment | report.
// ---------------------------------------------------------------------------

/// Metadata uses the shared chip painter. Clicking copies its literal value;
/// no condition polarity or phase is reinterpreted as research success.
fn metadata_chip(field: &str, value: &str) -> Element {
    rsx! { MetadataChip { field: field.to_string(), value: value.to_string() } }
}

/// Defer event-handler allocation to the component runtime: table row models
/// can be constructed independently of a mounted Dioxus application.
#[component]
fn MetadataChip(field: String, value: String, #[props(default = true)] labeled: bool) -> Element {
    use panel_kit::badge::Badge;
    use panel_kit_core::badge::{BadgeKind, BadgeSpec, BadgeTone, BadgeVariant};
    let label = if labeled {
        format!("{field}: {value}")
    } else {
        value.clone()
    };
    let mut spec = BadgeSpec::new(&field, &label, BadgeKind::Generic)
        .with_tone(BadgeTone::Neutral, BadgeVariant::Outline);
    spec.with_x = false;
    spec.with_plus = false;
    let text = serde_json::to_string(&value).expect("string is JSON encodable");
    rsx! {
        span { class: "metadata-chip", title: "{label} — click to copy",
            Badge { spec, on_action: move |_| {
                let script = format!("navigator.clipboard.writeText({text}).catch(e => console.warn('Copy failed', e))");
                document::eval(&script);
            } }
        }
    }
}

fn cond_badges(conds: &[ConditionDto]) -> Element {
    rsx! {
        for c in conds {
            span { title: "{c.reason}", {metadata_chip(&c.ctype, &c.status)} }
        }
    }
}

fn research_view(
    snap: ClusterSnapshot,
    mut nav: Signal<ResearchNav>,
    selected: Signal<Option<ResourceSummary>>,
    manifest_doc: Signal<String>,
    emit: EventHandler<WorkspaceEvent<Panel>>,
    curator_state: Signal<curator::CuratorState>,
) -> Element {
    match nav() {
        ResearchNav::Global => {
            let campaign_rows: Vec<Row> = snap
                .campaigns
                .iter()
                .map(|c| {
                    let pick = c.clone();
                    let shown = campaign_label(c);
                    campaign_row(
                        c,
                        rsx! {
                            button { class: "row-link", title: "{c.namespace}/{c.name}",
                                onclick: move |_| {
                                    nav.set(ResearchNav::Campaign {
                                        namespace: pick.namespace.clone(), name: pick.name.clone(),
                                    });
                                    select_resource(selected, manifest_doc, emit, pick.clone());
                                },
                                "{shown}"
                            }
                        },
                        &shown,
                    )
                })
                .collect();
            rsx! {
                div { class: "view-head",
                    h2 { "Research" }
                    p { "Drive evidence and campaigns. Select a campaign to inspect its runs and metrics." }
                }
                if snap.drives.is_empty() { p { class: "muted", "No research drives found." } }
                for d in &snap.drives {
                    section {
                        h3 { "{d.namespace}/{d.name}" }
                        {metadata_chip("Phase", &d.phase)}
                        if let Some(stage) = &d.stage { {metadata_chip("Stage", stage)} }
                        p { class: "muted", "Created {fmt_ms(&d.created_at)} UTC · stagnation {d.stagnation}" }
                        {cond_badges(&d.conditions)}
                        for st in &d.stages {
                            {
                                let rows: Vec<Row> = st.templates.iter().map(|t| {
                                    Row::new(&t.template_ref)
                                        .text(&t.template_ref)
                                        .num(t.best_objective, t.best_objective.map(|n| format!("{n:.3}")).unwrap_or_else(|| "—".into()))
                                        .num(Some(t.succeeded as f64), t.succeeded.to_string())
                                        .cell(metadata_chip("Gate", if t.passed { "passed" } else { "pending" }), Key::text(if t.passed { "passed" } else { "pending" }))
                                }).collect();
                                rsx! {
                                    h4 { "{st.name}" }
                                    if let Some(at) = &st.promoted_at { p { class: "muted", "Promoted {fmt_rfc3339(at)} UTC" } }
                                    ConsoleTable {
                                        id: format!("athena-stage-{}-{}-{}", d.namespace, d.name, st.name),
                                        columns: vec![
                                            DataColumnSpec::new("template", "Template").pinned(),
                                            DataColumnSpec::new("best", "Best").number(),
                                            DataColumnSpec::new("succeeded", "Succeeded").number(),
                                            DataColumnSpec::new("gate", "Gate").enumerated(),
                                        ],
                                        rows, sort: SortSpec::new("template", SortDir::Asc),
                                        empty: "No template gate evidence reported.".to_string(),
                                    }
                                }
                            }
                        }
                    }
                }
                h3 { "Campaigns" }
                ConsoleTable {
                    id: "athena-research-campaigns", columns: campaign_columns(), rows: campaign_rows,
                    sort: SortSpec::new("created", SortDir::Desc),
                    storage_key: "athena.table.research.campaigns".to_string(),
                    empty: "No campaigns found.".to_string(),
                    placeholder: "filter campaigns…".to_string(),
                }
            }
        }
        ResearchNav::Campaign { namespace, name } => {
            let camp = snap
                .campaigns
                .iter()
                .find(|c| c.name == name && c.namespace == namespace);
            let exp_rows: Vec<Row> = snap.experiments.iter()
                .filter(|e| e.namespace == namespace && e.campaign.as_deref() == Some(name.as_str()))
                .map(|e| {
                    let pick = e.clone();
                    experiment_row(e, rsx! {
                        button { class: "row-link",
                            onclick: move |_| select_resource(selected, manifest_doc, emit, pick.clone()),
                            "{e.name}"
                        }
                    }, false)
                }).collect();
            let report_rows: Vec<Row> = snap.reports.iter()
                .filter(|r| r.namespace == namespace && r.campaign_ref == name)
                .map(|r| {
                    let pick = r.clone();
                    Row::new(format!("{}/{}", r.namespace, r.name)).cell(rsx! {
                        button { class: "row-link",
                            onclick: move |_| {
                                curator::open_report(curator_state, pick.namespace.clone(), pick.name.clone());
                                restore_panel(emit, Panel::ReportCurator);
                            }, "{r.name}"
                        }
                    }, Key::text(&r.name)).text(&r.title).text_class(&r.phase, "phase").date(&r.created_at)
                }).collect();
            rsx! {
                nav { class: "crumbs", aria_label: "Research breadcrumb",
                    button { class: "row-link", onclick: move |_| nav.set(ResearchNav::Global), "All research" }
                    span { aria_hidden: "true", " / " }
                    span { aria_current: "page", "{namespace}/{name}" }
                }
                div { class: "view-head",
                    h2 { {camp.map(campaign_label).unwrap_or_else(|| name.clone())} }
                    p { "Runs open in the inspector; reports open in the Report Curator." }
                }
                h3 { "Experiments" }
                ConsoleTable {
                    id: "athena-research-experiments", columns: experiment_columns(false), rows: exp_rows,
                    sort: SortSpec::new("created", SortDir::Desc),
                    storage_key: "athena.table.research.experiments".to_string(),
                    empty: "No experiments in this campaign.".to_string(),
                }
                h3 { "Reports" }
                ConsoleTable {
                    id: "athena-research-reports",
                    columns: vec![
                        DataColumnSpec::new("name", "Report").pinned(),
                        DataColumnSpec::new("title", "Title"),
                        DataColumnSpec::new("phase", "Phase").enumerated(),
                        DataColumnSpec::new("created", "Created").number(),
                    ],
                    rows: report_rows, sort: SortSpec::new("created", SortDir::Desc),
                    empty: "No reports in this campaign.".to_string(),
                }
            }
        }
    }
}

fn reports_view(
    snap: ClusterSnapshot,
    emit: EventHandler<WorkspaceEvent<Panel>>,
    curator_state: Signal<curator::CuratorState>,
) -> Element {
    let rows: Vec<Row> = snap
        .reports
        .into_iter()
        .map(|r| {
            let r2 = r.clone();
            Row::new(format!("{}/{}", r.namespace, r.name))
                .text(r.name.clone())
                .text(r.campaign_ref.clone())
                .text(r.title.clone())
                .text_class(r.phase.clone(), "phase")
                .date(&r.created_at)
                .cell(
                    rsx! { span { "{r.excluded_count}" } },
                    Key::num(r.excluded_count as f64),
                )
                .cell(
                    rsx! { span {
                        button {
                            class: "btn",
                            onclick: move |_| {
                                curator::open_report(curator_state, r2.namespace.clone(), r2.name.clone());
                                restore_panel(emit, Panel::ReportCurator);
                            },
                            "Open"
                        }
                    } },
                    Key::Missing,
                )
        })
        .collect();
    rsx! {
        div { class: "view-head",
            h2 { "Reports" }
            p { "Published ResearchReport resources. Open one to review or edit it in the Report Curator." }
        }
        ConsoleTable {
            id: "athena-reports",
            columns: vec![
                DataColumnSpec::new("name", "Report").pinned(),
                DataColumnSpec::new("campaign", "Campaign"),
                DataColumnSpec::new("title", "Title"),
                DataColumnSpec::new("phase", "Phase").enumerated(),
                DataColumnSpec::new("created", "Created").number(),
                DataColumnSpec::new("excluded", "Excluded").number(),
                DataColumnSpec::new("load", "").pinned().unsorted(),
            ],
            rows,
            sort: SortSpec::new("created", SortDir::Desc),
            storage_key: "athena.table.reports".to_string(),
            empty: "No reports found.".to_string(),
            placeholder: "filter reports…".to_string(),
        }
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
        ConsoleTable {
            id: "athena-workloads",
            columns: vec![
                DataColumnSpec::new("name", "Workload").pinned(),
                DataColumnSpec::new("ns", "NS"),
                DataColumnSpec::new("queue", "Queue"),
                DataColumnSpec::new("priority", "Priority"),
                DataColumnSpec::new("state", "State").enumerated(),
                DataColumnSpec::new("gpu", "GPU").number(),
            ],
            sort: SortSpec::new("name", SortDir::Asc),
            storage_key: "athena.table.workloads".to_string(),
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
                        .text(prio)
                        .text(w.state.clone())
                        .cell(rsx! { span { "{w.gpus}" } }, Key::num(w.gpus as f64))
                })
                .collect::<Vec<Row>>(),
            empty: "No active workloads.".to_string(),
            placeholder: "filter workloads…".to_string(),
        }
    }
}

fn node_power_view(snap: SchedulingSnapshot) -> Element {
    let rows = snap
        .nodes
        .iter()
        .map(|n| {
            Row::new(&n.name)
                .text(&n.name)
                .cell(
                    metadata_chip("Power", if n.powered { "on" } else { "off" }),
                    Key::text(if n.powered { "on" } else { "off" }),
                )
                .text_class(&n.phase, "phase")
                .text(&n.pool)
        })
        .collect::<Vec<_>>();
    rsx! {
        div { class: "view-head", h2 { "Node Power" } p { "Hephaestus physical GPU node state; read-only." } }
        ConsoleTable {
            id: "athena-nodes",
            columns: vec![
                DataColumnSpec::new("name", "Node").pinned(),
                DataColumnSpec::new("power", "Power").enumerated(),
                DataColumnSpec::new("phase", "Phase").enumerated(),
                DataColumnSpec::new("pool", "Pool").enumerated(),
            ],
            rows, sort: SortSpec::new("name", SortDir::Asc),
            storage_key: "athena.table.nodes".to_string(),
            empty: "No node power state reported.".to_string(),
        }
    }
}

fn inference_view(snap: SchedulingSnapshot) -> Element {
    let rows = snap
        .inference
        .iter()
        .map(|b| {
            Row::new(format!("{}/{}", b.campaign, b.kind))
                .text(&b.campaign)
                .text(&b.kind)
                .cell(
                    metadata_chip("Serving", if b.serving { "serving" } else { "starting" }),
                    Key::text(if b.serving { "serving" } else { "starting" }),
                )
                .text(&b.endpoint)
        })
        .collect::<Vec<_>>();
    rsx! {
        div { class: "view-head", h2 { "Inference Backends" } p { "Per-campaign mesh / vLLM-on-Ray endpoints; read-only." } }
        ConsoleTable {
            id: "athena-inference",
            columns: vec![
                DataColumnSpec::new("campaign", "Campaign").pinned(),
                DataColumnSpec::new("kind", "Kind").enumerated(),
                DataColumnSpec::new("state", "Serving").enumerated(),
                DataColumnSpec::new("endpoint", "Endpoint"),
            ],
            rows, sort: SortSpec::new("campaign", SortDir::Asc),
            storage_key: "athena.table.inference".to_string(),
            empty: "No ephemeral inference backend active. Campaigns appear while serving.".to_string(),
        }
    }
}
// ---------------------------------------------------------------------------
// Data layer (frontend side): reads are plain, writes go through `api` so the
// bearer token is attached and a 401/403 becomes an actionable message.
// ---------------------------------------------------------------------------

/// Browser origin (`https://host:port`) for building absolute request URLs;
/// empty string outside a browser.
pub(crate) fn api_base() -> String {
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
pub(crate) async fn create_report(dto: ReportSpecDto) -> Result<ReportDetailDto, String> {
    let url = format!("{}/api/reports", api_base());
    api::send_json(reqwest::Client::new().post(&url).json(&dto))
        .await
        .map_err(|e| e.to_string())
}

/// `PUT /api/reports/{ns}/{name}` — replace against the loaded resourceVersion.
pub(crate) async fn update_report(dto: ReportSpecDto) -> Result<ReportDetailDto, String> {
    let url = format!("{}/api/reports/{}/{}", api_base(), dto.namespace, dto.name);
    api::send_json(reqwest::Client::new().put(&url).json(&dto))
        .await
        .map_err(|e| e.to_string())
}

/// `POST /api/reports/preview` — compose the dossier Markdown; nothing persisted.
pub(crate) async fn preview_report(dto: ReportSpecDto) -> Result<String, String> {
    let url = format!("{}/api/reports/preview", api_base());
    api::send_text(reqwest::Client::new().post(&url).json(&dto))
        .await
        .map_err(|e| e.to_string())
}

/// `GET /api/reports/{ns}/{name}` — one report's detail. Reads stay open, so
/// this needs no token; it is here so the curator shares the data layer's URL
/// construction with the write paths.
pub(crate) async fn fetch_report_detail(
    namespace: &str,
    name: &str,
) -> Result<ReportDetailDto, String> {
    let url = format!("{}/api/reports/{namespace}/{name}", api_base());
    reqwest::get(&url)
        .await
        .map_err(|e| e.to_string())?
        .json::<ReportDetailDto>()
        .await
        .map_err(|e| e.to_string())
}
