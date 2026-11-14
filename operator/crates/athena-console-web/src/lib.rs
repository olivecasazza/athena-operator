//! Dioxus (web) reimplementation of `athena-console` on the panel-kit
//! workspace shell.
//!
//! The native console was an Iced desktop app that talked to Kubernetes through
//! `kube` directly. That cannot run in a browser, so this port splits into:
//!
//! - a Dioxus **frontend** (this crate's lib + default bin) where every console
//!   view is a panel-kit [`Panel`], and
//! - a tiny axum **backend** (`src/bin/console-server/main.rs`, `server`
//!   feature) that projects Kubernetes resources into [`models`] DTOs.
//!
//! Review is the default human workflow: compare a candidate with a real
//! retained reference, inspect evidence and record advisory research feedback.
//! Existing research, metric, manifest, report and operator tools remain
//! supporting destinations; the console never owns execution status.

pub mod access;
mod activity;
mod api;
mod auth;
mod auth_flow;
mod curator;
pub mod models;
mod review;
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
use panel_kit::widgets::data_table::{Control, SortSpec};
use panel_kit::{LayoutBuilder, PanelKind, PanelWin};
use panel_kit_core::Mode;
use panel_kit_core::PanelCommand;
use panel_kit_core::reducer::WorkspaceEvent;
use panel_kit_core::widgets::data_table::SortDir;
use serde::{Deserialize, Serialize};
use std::{cell::RefCell, rc::Rc};
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
    /// Browse evidence without inventing a required-review queue.
    ReviewResults,
    /// Candidate/reference evidence and advisory ResearchReport feedback.
    ReviewEvidence,
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
            Panel::ReviewResults => "Results",
            Panel::ReviewEvidence => "Evidence review",
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
            (Panel::ReviewResults, 640.0),
            (Panel::ReviewEvidence, 640.0),
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
            name: "Review",
            mode: Mode::Tiling,
            panels: &[
                (Panel::ReviewEvidence, 3, 5),
                (Panel::ReviewResults, 1, 5),
            ],
        },
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
            details { class: "workspace-tools",
                summary { "Layout" }
                div { class: "workspace-tools-body",
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

/// Preserve Athena's pink/blue identity through the shared theme roles.
/// Component geometry and pointer-dependent hit targets remain panel-kit's.
const APP_CSS: &str = "
:root {
  --color-primary:#93c5fd; --color-secondary:#f472b6;
  --color-text-muted:#bdbdbd; --panel-radius:0; --badge-radius:0;
}
.ws-root :focus-visible { outline:2px solid var(--color-focus-ring); outline-offset:2px; }
::selection { background:var(--color-inverse-background); color:var(--color-inverse-text); }
input, textarea { caret-color:var(--color-text); }
a { color:var(--color-secondary); }
.panel-body { scrollbar-color:var(--color-text-muted) var(--color-surface); }
.pk-dt { font-variant-numeric:tabular-nums; }
.metadata-chip { display:inline-flex; align-items:center; max-width:100%; vertical-align:middle; }
.pk-tr-legend-means, .pk-tr-faint, .pk-tr-rail-meta { color:var(--color-text-muted); }
.activity-modes { display:flex; flex-wrap:wrap; gap:.5rem; margin:.5rem 0; }
.topbar { display:flex; align-items:center; gap:1rem; padding:.4rem .75rem;
  height:auto; min-height:36px; flex:0 0 auto; border-bottom:1px solid var(--color-divider); }
.topbar h1 { font-size:1rem; color:var(--color-secondary); margin:0; flex-shrink:0; }
.topbar .hint { color:var(--color-text-muted); font-size:.72rem; }
.access-shell { min-height:100dvh; background:var(--color-background); color:var(--color-text); }
.access-shell .topbar { flex-wrap:wrap; }
.access-wall { max-width:70ch; padding:clamp(1.5rem,5vw,4rem); margin-inline:auto; }
.access-wall h2 { font-size:clamp(1.35rem,3vw,2rem); text-wrap:balance; margin:1rem 0; }
.access-wall p { line-height:1.6; overflow-wrap:anywhere; }
.access-wall .btn { margin-top:1rem; }
.access-shell :focus-visible { outline:2px solid var(--color-focus-ring); outline-offset:2px; }
.metrics-access { flex:0 0 auto; line-height:1.5; margin:.5rem .75rem; font-size:.8rem; }
.views { display:flex; align-items:center; gap:.3rem; flex:1; min-width:0; flex-wrap:wrap; }
.view-tab { display:inline-flex; }
.view-btn.active-view { background:var(--color-background); color:var(--color-secondary); border-color:var(--color-secondary); }
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
.cur-report.active, .cur-tab.active { color:var(--color-secondary); border-color:var(--color-secondary); }
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
.cur-save-feedback { flex:1 0 100%; min-width:0; }
.cur-failure { color:var(--color-text); padding:.6rem; border:1px solid var(--color-border);
  background:var(--color-background); min-width:0; }
.cur-failure p { margin:0 0 .4rem; max-width:72ch; line-height:1.5; overflow-wrap:anywhere; }
.cur-failure .cur-failure-title { font-weight:600; }
.cur-failure details { margin-top:.5rem; }
.cur-failure summary { cursor:pointer; min-height:var(--hit-min); display:list-item;
  align-content:center; color:var(--color-text-muted); }
.cur-failure pre { max-height:12rem; overflow:auto; white-space:pre-wrap;
  overflow-wrap:anywhere; margin:.4rem 0 0; font:inherit; line-height:1.5; }
.cur-failure .btn { margin:.1rem 0 .25rem; }
.cur-navigation { color:var(--color-text); padding:.6rem; border:1px solid var(--color-border);
  background:var(--color-background); min-width:0; }
.cur-navigation-copy { display:flex; flex-direction:column; align-items:flex-start; gap:.35rem; }
.cur-navigation p { margin:0; max-width:72ch; line-height:1.5; overflow-wrap:anywhere; }
.cur-navigation code { max-width:100%; white-space:normal; overflow-wrap:anywhere; }
.cur-navigation-actions { display:flex; flex-wrap:wrap; gap:.4rem; margin-top:.6rem; }
.cur-navigation-actions .btn { min-height:44px; min-width:44px; max-width:100%; white-space:normal; text-align:left; }
.btn.primary:not(:disabled) { border-color:var(--color-text); color:var(--color-text); }
.btn:disabled { color:var(--color-text-muted); border-color:var(--color-border); cursor:not-allowed; }
.ok { color:var(--color-success); font-size:.74rem; }
.pk-dt td .row-link { font-size:inherit; }
.row-link { background:none; border:none; color:var(--color-secondary); cursor:pointer;
  font-family:var(--mono); font-size:inherit; min-height:var(--hit-min); padding:.1rem .2rem; text-align:left; text-decoration:underline; text-underline-offset:3px; }
.row-link:hover { text-decoration:underline; }
.muted { color:var(--color-text-muted); }
.view-head { margin:.1rem 0 .6rem; }
.view-head h2 { font-size:1rem; color:var(--color-text); margin:0 0 .25rem; }
.view-head p { font-size:var(--body-size); color:var(--color-text-muted); margin:0; line-height:1.5; }
.detail-grid { display:grid; grid-template-columns:max-content minmax(0,1fr); gap:.3rem .8rem;
  font-size:var(--body-size); margin-bottom:.6rem; }
.detail-grid dt { color:var(--color-text-muted); }
.detail-grid dd { color:var(--color-text); margin:0; min-width:0; overflow-wrap:anywhere; }
.btn { background:var(--color-background); color:var(--color-text); border:1px solid var(--color-border);
  border-radius:0; min-height:var(--hit-min); padding:.1rem .45rem; font-size:var(--label-size); cursor:pointer; font-family:var(--mono); }
.admin-toggle { margin-left:auto; }
.btn:hover { border-color:var(--color-secondary); }
.section-label { font-size:var(--body-size); color:var(--color-text); margin:.5rem 0 .25rem; }
/* The Grafana embed tracks the panel's height instead of a fixed 340px, so
   resizing/maximizing the panel actually gives the dashboard more room (it is
   ~1400px tall and scrolls internally otherwise). 100% resolves against
   .panel-body's CONTENT box, so its padding is already excluded; the collapse
   floor is panel-kit's own .grafana-panel min-height. */
.embed-block { height:100%; margin:0; }
.grafana-open { opacity:1; color:var(--color-secondary); }
/* A table's 100% height cannot be added after its panel heading. Let the host
   allocate the remainder; multi-table research/curation views stay in flow. */
.panel-experiments .panel-body, .panel-reports .panel-body,
.panel-runtime-profiles .panel-body, .panel-node-power---hephaestus .panel-body,
.panel-inference-backends .panel-body { display:flex; flex-direction:column; overflow:hidden; }
.panel-body > .view-head, .panel-body > .detail-grid { flex:0 0 auto; }
.panel-body > .pk-dt { flex:1; min-height:0; height:auto; }
.panel-body > .pk-dt > .pk-dt-scroll { flex:1; min-height:0; max-height:none; }
.cur .pk-dt { height:auto; }
.panel-benchmarks .panel-body { display:grid; grid-template-rows:auto auto minmax(110px,1fr) auto minmax(110px,1fr); overflow:auto; }
.panel-gpu-pools---queues .panel-body { display:flex; flex-direction:column; overflow:auto; }
.panel-gpu-pools---queues .pk-dt { flex:1 0 140px; min-height:140px; }
.panel-run-activity .panel-body { display:flex; flex-direction:column; overflow:auto; }
.panel-run-activity .activity-modes { flex:0 0 auto; }
.panel-run-activity .pk-tr { flex:1; min-height:0; height:auto; }
.panel-experiment-manifest .panel-body { display:flex; flex-direction:column; overflow:hidden; }
.panel-templates .panel-body { display:grid; grid-template-rows:auto minmax(100px,2fr) auto minmax(140px,3fr); overflow:auto; }
.panel-templates .pk-dt { min-width:0; min-height:0; }
.panel-templates .pk-dt-scroll { flex:1; min-height:0; max-height:none; }
.ide-block { display:flex; flex-direction:column; flex:1; min-width:0; min-height:0; }
.ide-block > .ide-panel { flex:1; min-height:0; }
.ide-block .ide-textarea { min-height:0; }
.todo { color:var(--color-warning); font-size:.74rem; }
.err { color:var(--color-error); font-size:.76rem; }
.preview { max-height:300px; overflow:auto; white-space:pre-wrap;
  font-family:var(--mono); font-size:.7rem; background:var(--color-background);
  border:1px solid var(--color-border); padding:.4rem; margin:.4rem 0; }
.rc-select { background:var(--color-background); color:var(--color-text); border:1px solid var(--color-divider);
  font-family:var(--mono); font-size:var(--body-size); min-height:var(--hit-min); padding:.15rem .3rem; width:100%; }
.rc-input { background:var(--color-background); color:var(--color-text); border:1px solid var(--color-divider);
  font-family:var(--mono); font-size:var(--body-size); min-height:var(--hit-min); padding:.15rem .3rem; width:100%;
  box-sizing:border-box; }
.rc-textarea { background:var(--color-background); color:var(--color-text); border:1px solid var(--color-divider);
  font-family:var(--mono); font-size:var(--body-size); padding:.2rem .3rem; width:100%;
  box-sizing:border-box; height:4rem; resize:vertical; }
/* Breadcrumbs lead back to the one campaign browser. */
.crumbs { display:flex; align-items:baseline; gap:.3rem; font-size:.74rem;
  font-family:var(--mono); margin:.1rem 0 .6rem; }
.pk-dt .date { white-space:nowrap; color:var(--color-text-muted); }
.pk-dt td .row-link { max-width:100%; white-space:normal; overflow-wrap:anywhere; display:inline-block; vertical-align:middle; }
.crumb-current { color:var(--color-text); }
.workspace-tools { position:relative; margin-left:auto; }
.review-figure video { display:block; width:100%; min-height:8rem; max-height:28rem; background:var(--color-background); }
.workspace-tools summary { cursor:pointer; min-height:var(--hit-min); padding:.1rem .45rem; border:1px solid var(--color-border); }
.workspace-tools-body { position:absolute; right:0; top:100%; z-index:100; display:flex; flex-wrap:wrap; gap:.4rem; padding:.6rem; width:min(28rem,85vw); background:var(--color-surface); border:1px solid var(--color-border); }
.panel-results .panel-body { display:flex; flex-direction:column; overflow:hidden; }
.panel-evidence-review .panel-body { overflow:auto; }
.review-workflow { min-width:0; padding:.35rem; display:flex; flex-direction:column; gap:.85rem; font-size:var(--body-size); }
.review-heading { display:flex; align-items:start; justify-content:space-between; gap:1rem; flex-wrap:wrap; }
.review-heading h2 { font-size:1.15rem; margin:0 0 .35rem; }
.review-heading p, .review-feedback p { margin:.3rem 0; line-height:1.5; }
.review-actions, .review-progress, .review-observations, .review-replay-actions { display:flex; align-items:center; flex-wrap:wrap; gap:.45rem; }
.review-progress { padding:.5rem 0; border-top:1px solid var(--color-divider); border-bottom:1px solid var(--color-divider); }
.review-reference-label { display:flex; flex-direction:column; gap:.35rem; min-width:0; }
.review-reference-label select { max-width:100%; min-width:0; }
.review-scale { display:flex; align-items:center; flex-wrap:wrap; gap:.5rem; }
.review-scale select { width:auto; max-width:100%; }
.review-row-context { display:block; color:var(--color-text-muted); font-size:.75rem; overflow-wrap:anywhere; line-height:1.5; }
.review-pair { display:grid; grid-template-columns:minmax(0,1fr) minmax(0,1fr); gap:1rem; align-items:start; }
.review-evidence { min-width:0; display:flex; flex-direction:column; gap:.6rem; }
.review-evidence h3 { margin:0; font-size:1rem; }
.review-run-name { margin:0; font-weight:600; overflow-wrap:anywhere; line-height:1.5; }
.review-notice, .review-artifact-missing { padding:.6rem; border:1px solid var(--color-border); margin:0; line-height:1.5; overflow-wrap:anywhere; }
.review-artifact-missing { min-height:10rem; display:flex; flex-direction:column; justify-content:center; gap:.6rem; }
.review-figure { margin:0; min-width:0; }
.review-figure img { display:block; width:100%; height:auto; background:var(--color-background); }
.review-media-frame { overflow:auto; max-height:28rem; min-width:0; }
.review-artifact-loading { min-height:8rem; padding:.6rem; }
.review-figure figcaption { font-size:.75rem; color:var(--color-text-muted); line-height:1.5; margin-top:.3rem; }
.review-policy { display:block; width:100%; height:28rem; border:1px solid var(--color-border); }
.review-hypothesis { margin:0; line-height:1.5; overflow-wrap:anywhere; }
.review-facts { margin:0; }
.review-checks { min-width:0; }
.review-checks summary { cursor:pointer; min-height:var(--hit-min); padding:.3rem 0; }
.review-checks .pk-dt { height:auto; max-height:16rem; }
.review-checks .pk-dt-scroll { max-height:12rem; }
.review-feedback { border-top:1px solid var(--color-divider); padding-top:.8rem; display:flex; flex-direction:column; gap:.5rem; min-width:0; }
.review-feedback h3 { margin:0; font-size:1rem; }
.review-feedback .rc-textarea { min-height:7rem; font-family:inherit; }
.review-feedback .cur-footer { position:static; }
.review-feedback details p { white-space:pre-wrap; overflow-wrap:anywhere; }
@media(max-width:700px) {
  .topbar { flex-wrap:wrap; gap:.5rem; }
  .topbar { position:relative; }
  .topbar h1 { min-height:var(--hit-min); display:flex; align-items:center; }
  .workspace-tools { position:absolute; top:.4rem; right:.75rem; margin:0; }
  .workspace-tools-body { position:fixed; top:3.6rem; left:.75rem; right:.75rem; width:auto; }
  .views { flex-basis:100%; flex-wrap:wrap; overflow:visible; }
  .cur-tabs { flex-wrap:wrap; }
  .cur-name, .cur-title { width:100%; flex-basis:100%; }
  .detail-grid { grid-template-columns:1fr; }
  .review-pair { grid-template-columns:minmax(0,1fr); }
  .review-workflow { padding:.6rem; }
  .review-policy { height:24rem; }
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

    let mut admin = use_signal(|| false);
    let mut auth_state = use_resource(|| async {
        let _revision = auth::session_revision();
        resolve_auth_phase().await
    });
    // A verified session precedes business data; protected reads never fall
    // back to anonymous traffic while access is being checked.
    let mut snapshot = use_resource(move || async move {
        if !read_phase(&auth_state.read()).can_read() {
            return Err("sign in to load research resources".into());
        }
        fetch_snapshot().await
    });
    let sched = use_resource(move || async move {
        if !read_phase(&auth_state.read()).can_read_infrastructure() {
            return Err("infrastructure access is not assigned to this session".into());
        }
        fetch_scheduling().await
    });

    let pending = use_hook(|| Rc::new(RefCell::new(review::restore_pending("athena.review.pending.v1"))));
    // External selection state for the data-less ExperimentDetail panel.
    let selected = use_signal({
        let pending = pending.clone();
        move || pending.borrow_mut().selected.take()
    });
    // Editable manifest / template documents for the IdePanels.
    let manifest_doc = use_signal(|| "# Select an experiment to load its manifest.\n".to_string());
    let template_doc = use_signal(|| "# Load a template to view its YAML.\n".to_string());

    // Report Curator state (draft, loaded report, tab, save outcome).
    let curator_state = use_signal(|| {
        let pending = review::restore_pending("athena.curator.pending.v1");
        curator::CuratorState {
            loaded: pending.loaded,
            pinned_campaign: pending.pinned_campaign,
            base: pending.base,
            draft: pending.draft,
            ..Default::default()
        }
    });
    let review_feedback = use_signal({
        let pending = pending.clone();
        move || {
            let mut pending = pending.borrow_mut();
            curator::CuratorState {
                loaded: pending.loaded.take(),
                pinned_campaign: pending.pinned_campaign.take(),
                base: std::mem::take(&mut pending.base),
                draft: std::mem::take(&mut pending.draft),
                ..Default::default()
            }
        }
    });
    let review_feedback_subject = use_signal({
        let pending = pending.clone();
        move || pending.borrow_mut().subject.take()
    });
    let review_reference = use_signal(move || pending.borrow_mut().reference.take());
    use_effect(move || review::preserve_pending(
        "athena.review.pending.v1",
        &review_feedback.read(),
        selected.read().as_ref(),
        review_reference.read().as_ref(),
        review_feedback_subject.read().as_deref(),
        snapshot.read().as_ref().and_then(|value| value.as_ref().ok()),
    ));
    use_effect(move || review::preserve_pending(
        "athena.curator.pending.v1", &curator_state.read(), None, None, None, None,
    ));
    review::use_pending_flush(move || {
        let snapshot = snapshot.peek();
        review::preserve_pending(
            "athena.review.pending.v1", &review_feedback.peek(),
            selected.peek().as_ref(), review_reference.peek().as_ref(),
            review_feedback_subject.peek().as_deref(),
            snapshot.as_ref().and_then(|value| value.as_ref().ok()),
        );
        review::preserve_pending(
            "athena.curator.pending.v1", &curator_state.peek(), None, None, None, None,
        );
    });
    // Pin the initial presentation selection once. A later refresh must not
    // silently move the comparison to a newly-created run.
    use_effect(move || {
        if let Some(Ok(snap)) = &*snapshot.read() {
            let next = {
                let current = selected.peek();
                match current.as_ref() {
                    None => review::candidate(snap, None).cloned(),
                    Some(current) if current.kind == "experiment" => review::candidate(snap, Some(current)).cloned(),
                    _ => None,
                }
            };
            if let Some(run) = next {
                load_resource(selected, manifest_doc, run);
            }
        }
    });

    // Research drill-down navigation. `Panel` variants carry no data (PanelKind
    // is Copy), so the current depth — global fleet, one campaign, one
    // experiment, one report — lives out here exactly like `selected` does for
    // ExperimentDetail. The breadcrumb bar is the rendered form of this signal.
    let research_nav = use_signal(|| ResearchNav::Global);


    let review_ws = ws.clone();
    let body = move |kind: Panel, _maximized: bool| -> Element {
        let snap = snapshot.read();
        let snap = match &*snap {
            None => {
                return rsx! { p { class: "muted", "Loading Athena resources…" } };
            }
            Some(Err(e)) => {
                return rsx! {
                    div { role: "alert",
                        p { class: "err", "Failed to load resources: {e}" }
                        button { class: "btn", onclick: move |_| snapshot.restart(), "Retry resources" }
                    }
                };
            }
            Some(Ok(snap)) => snap.clone(),
        };

        match kind {
            Panel::ReviewResults => review::results_view(snap, selected),
            Panel::ReviewEvidence => {
                let inspect_ws = review_ws.clone();
                let campaign_ws = review_ws.clone();
                rsx! {
                    review::ReviewEvidence {
                        snap,
                        selected,
                        reference: review_reference,
                        feedback: review_feedback,
                        feedback_subject: review_feedback_subject,
                        auth: read_phase(&auth_state.read()),
                        on_saved: move |_| snapshot.restart(),
                        on_inspect: move |run| {
                            inspect_ws.switch_view("Experiments");
                            select_resource(selected, manifest_doc, ws_emit, run);
                        },
                        on_campaign: move |(namespace, name)| {
                            let mut nav = research_nav;
                            nav.set(ResearchNav::Campaign { namespace, name });
                            campaign_ws.switch_view("Research");
                        },
                    }
                }
            }
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
    let phase = read_phase(&auth_state.read());

    rsx! {
        style { {panel_kit::CSS} }
        style { {APP_CSS} }
        if !phase.can_read() {
            div { class: "access-shell",
                header { class: "topbar",
                    h1 { "Athena Console" }
                    {auth_bar(&phase)}
                }
                main { class: "access-wall", role: "status",
                    h2 {
                        match &phase {
                            auth_flow::Phase::Loading => "Checking access…",
                            auth_flow::Phase::Denied { .. } => "Athena access is not assigned",
                            auth_flow::Phase::Expired { .. } => "Sign in again to continue",
                            auth_flow::Phase::Failed { .. } => "Sign-in could not finish",
                            _ => "Sign in to your research workspace",
                        }
                    }
                    match &phase {
                        auth_flow::Phase::Denied { message, .. }
                        | auth_flow::Phase::Expired { message, .. }
                        | auth_flow::Phase::Failed { message, .. } => rsx! { p { "{message}" } },
                        _ => rsx! { p { "Research resources require an authorized Athena account." } },
                    }
                    p { class: "muted", "Signing in does not submit feedback or change research execution." }
                    if !matches!(phase, auth_flow::Phase::Loading) {
                        button { class: "btn", onclick: move |_| auth_state.restart(), "Check session" }
                    }
                }
            }
        } else if admin() && phase.can_read_infrastructure() {
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
                    h1 { "Athena Console · Infrastructure" }
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
                    button { class: "btn", title: "Recheck access and reload controller-owned snapshots", onclick: move |_| auth_state.restart(), "Refresh" }
                    {auth_bar(&phase)}
                    if phase.can_read_infrastructure() {
                        button { class: "btn admin-toggle", onclick: move |_| admin.set(true), "Infrastructure" }
                    }
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

    match auth_flow::stored_session(auth::stored_token().as_ref(), auth::now_seconds()) {
        auth_flow::StoredSession::Verify => verified_session_phase(&config).await,
        auth_flow::StoredSession::Expired => {
            auth::clear_token();
            Ok(expired_phase(&config, "your access token expired; sign in again to continue"))
        }
        auth_flow::StoredSession::Missing => Ok(signed_out_phase(&config)),
    }
}

/// Exchange the callback code, then report the resulting phase.
async fn finish_callback(
    config: &auth::AuthConfig,
    code: &str,
    returned_state: Option<&str>,
) -> Result<auth_flow::Phase, String> {
    let login = match auth_flow::check_state(returned_state.unwrap_or_default()) {
        Ok(login) => login,
        Err(message) => return Ok(failed_phase(config, message)),
    };
    let body = auth::token_request_body(code, &login.verifier, &login.redirect_uri, &config.client_id);
    match exchange_token(config, &body).await {
        Ok(token) => {
            auth::store_token(&token);
            verified_session_phase(config).await
        }
        Err(message) => Ok(failed_phase(config, message)),
    }
}

async fn verified_session_phase(config: &auth::AuthConfig) -> Result<auth_flow::Phase, String> {
    let url = format!("{}/api/auth/session", api_base());
    match api::send_json(reqwest::Client::new().get(url)).await {
        Ok(session) => Ok(auth_flow::Phase::SignedIn { session }),
        Err(api::ApiError::Unauthorized { message, .. }) => {
            auth::clear_token();
            Ok(expired_phase(config, &message))
        }
        Err(api::ApiError::Forbidden(message)) => Ok(auth_flow::Phase::Denied {
            message,
            sign_in: auth_flow::begin(config, &auth::current_url().unwrap_or_default()),
        }),
        Err(error) => Err(error.to_string()),
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

/// Session state and recovery remain visible without taking over the workspace.
fn auth_bar(phase: &auth_flow::Phase) -> Element {
    let notice = phase.notice();
    let sign_in = auth_flow::sign_in_url(phase);
    rsx! {
        if let auth_flow::Phase::SignedIn { session } = phase {
            span { class: "hint", title: "{session.profile} · {session.namespaces.join(\", \")}",
                {metadata_chip("Access", if session.permissions.write_reports { "reports enabled" } else { "read only" })}
            }
        }
        if let Some((label, title)) = notice {
            span { class: "hint", title: "{title}", {metadata_chip("Session", label)} }
        }
        if let Some(url) = sign_in {
            button {
                class: "btn",
                title: "Sign in through the identity provider; no research action is submitted",
                onclick: move |_| auth::navigate_to(&url),
                "Sign in"
            }
        }
    }
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
        if let Some(url) = &exp.figures_url {
            a { href: "{url}", target: "_blank", rel: "noopener noreferrer", "Open evaluation artifact" }
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
    selected: Signal<Option<ResourceSummary>>,
    manifest_doc: Signal<String>,
    emit: EventHandler<WorkspaceEvent<Panel>>,
    r: ResourceSummary,
) {
    restore_detail_panels(emit);
    load_resource(selected, manifest_doc, r);
}

fn load_resource(
    mut selected: Signal<Option<ResourceSummary>>,
    mut manifest_doc: Signal<String>,
    r: ResourceSummary,
) {
    selected.set(Some(r.clone()));
    manifest_doc.set(format!("# Loading {}/{}…\n", r.namespace, r.name));
    spawn(async move {
        let result = fetch_manifest(&r.namespace, &r.kind, &r.name).await;
        if selected.peek().as_ref().is_some_and(|current| {
            current.namespace == r.namespace && current.name == r.name && current.uid == r.uid
        }) {
            manifest_doc.set(match result {
                Ok(yaml) => yaml,
                Err(err) => format!("# failed to load manifest: {err}\n"),
            });
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
            p { class: "metrics-access",
                "Metrics use your separate Grafana session. "
                a { href: "{GRAFANA_BASE}/login", target: "_blank", rel: "noopener noreferrer", "Sign in to Grafana" }
                " if the embedded dashboard asks for access, then reopen this panel. The console does not pass its token to Grafana."
            }
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
            toolbar: vec![Control::Quick, Control::Columns, Control::Tally],
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
            toolbar: vec![Control::Quick, Control::Columns, Control::Tally],
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
                                        toolbar: vec![Control::Quick, Control::Tally],
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
    api::send_json(reqwest::Client::new().get(url)).await.map_err(|e| e.to_string())
}

/// `GET /api/scheduling` → the [`SchedulingSnapshot`] for the admin page.
async fn fetch_scheduling() -> Result<SchedulingSnapshot, String> {
    let url = format!("{}/api/scheduling", api_base());
    api::send_json(reqwest::Client::new().get(url)).await.map_err(|e| e.to_string())
}

/// `GET /api/manifest/{ns}/{kind}/{name}` → resource manifest YAML.
async fn fetch_manifest(namespace: &str, kind: &str, name: &str) -> Result<String, String> {
    let url = format!("{}/api/manifest/{namespace}/{kind}/{name}", api_base());
    api::send_text(reqwest::Client::new().get(url)).await.map_err(|e| e.to_string())
}

/// `GET /api/template/{ns}/{name}` → ExperimentTemplate YAML.
async fn fetch_template_yaml(namespace: &str, name: &str) -> Result<String, String> {
    let url = format!("{}/api/template/{namespace}/{name}", api_base());
    api::send_text(reqwest::Client::new().get(url)).await.map_err(|e| e.to_string())
}

/// `POST /api/reports` — persist a ResearchReport spec, returns the summary row.
pub(crate) async fn create_report(dto: ReportSpecDto) -> Result<ReportDetailDto, api::ApiError> {
    let url = format!("{}/api/reports", api_base());
    api::send_json(reqwest::Client::new().post(&url).json(&dto)).await
}

/// `PUT /api/reports/{ns}/{name}` — replace against the loaded resourceVersion.
pub(crate) async fn update_report(dto: ReportSpecDto) -> Result<ReportDetailDto, api::ApiError> {
    let url = format!("{}/api/reports/{}/{}", api_base(), dto.namespace, dto.name);
    api::send_json(reqwest::Client::new().put(&url).json(&dto)).await
}

/// `POST /api/reports/preview` — compose the dossier Markdown; nothing persisted.
pub(crate) async fn preview_report(dto: ReportSpecDto) -> Result<String, String> {
    let url = format!("{}/api/reports/preview", api_base());
    api::send_text(reqwest::Client::new().post(&url).json(&dto))
        .await
        .map_err(|e| e.to_string())
}

/// `GET /api/reports/{ns}/{name}` — authenticated editable spec and observed status.
pub(crate) async fn fetch_report_detail(
    namespace: &str,
    name: &str,
) -> Result<ReportDetailDto, String> {
    let url = format!("{}/api/reports/{namespace}/{name}", api_base());
    api::send_json(reqwest::Client::new().get(url)).await.map_err(|e| e.to_string())
}
