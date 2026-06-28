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
//! Grafana dashboard via [`panel_kit::GrafanaPanel`] and the manifest editor via
//! [`panel_kit::IdePanel`].

pub mod models;

use dioxus::prelude::*;
use models::{ClusterSnapshot, ResourceSummary, TemplateSummary};
use panel_kit::{GrafanaPanel, IdePanel, LayoutBuilder, PanelKind, PanelWin, use_workspace};
use serde::{Deserialize, Serialize};

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
        }
    }
}

fn default_layout() -> Vec<PanelWin<Panel>> {
    let mut b = LayoutBuilder::new();
    vec![
        b.at(Panel::Experiments, 16.0, 16.0, 640.0, 460.0),
        b.at(Panel::ExperimentDetail, 672.0, 16.0, 620.0, 200.0),
        b.at(Panel::ExperimentMetrics, 672.0, 232.0, 620.0, 360.0),
        b.at(Panel::ExperimentManifest, 672.0, 608.0, 620.0, 360.0),
        b.at(Panel::Templates, 16.0, 492.0, 640.0, 320.0),
        b.at(Panel::Campaigns, 16.0, 828.0, 640.0, 260.0),
        b.at(Panel::Benchmarks, 672.0, 984.0, 620.0, 320.0),
        b.at(Panel::RuntimeProfiles, 16.0, 1104.0, 640.0, 260.0),
    ]
}

/// App-specific theming layered after [`panel_kit::CSS`]: a high-contrast
/// pink/blue palette echoing the native console, plus table + detail styles.
const APP_CSS: &str = "
:root { --accent: #f472b6; --pink: #f472b6; --blue: #93c5fd; }
.topbar { display:flex; align-items:baseline; gap:1rem; padding:.5rem .9rem;
  border-bottom:1px solid var(--line); }
.topbar h1 { font-size:1rem; color:var(--pink); margin:0; }
.topbar .hint { color:var(--dim); font-size:.72rem; }
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
.view-head h2 { font-size:.92rem; color:var(--fg); margin:0 0 .2rem; }
.view-head p { font-size:.72rem; color:var(--dim); margin:0; }
.detail-grid { display:grid; grid-template-columns:auto 1fr; gap:.2rem .8rem;
  font-size:.74rem; margin-bottom:.6rem; }
.detail-grid dt { color:var(--dim); }
.detail-grid dd { color:var(--fg); margin:0; word-break:break-all; }
.btn { background:var(--bg); color:var(--fg); border:1px solid var(--line2);
  border-radius:3px; padding:.1rem .45rem; font-size:.7rem; cursor:pointer;
  font-family:var(--mono); }
.btn:hover { border-color:var(--pink); }
.section-label { font-size:.78rem; color:var(--fg); margin:.5rem 0 .25rem; }
.embed-block { height:340px; margin:.4rem 0; }
.ide-block { height:260px; margin:.4rem 0; }
.todo { color:var(--yellow); font-size:.74rem; }
.err { color:var(--red); font-size:.76rem; }
";

/// App root.
#[component]
pub fn App() -> Element {
    let ws = use_workspace("athena_console_web", default_layout);

    // Snapshot fetched once from the backend; views read it reactively.
    let snapshot = use_resource(move || async move { fetch_snapshot().await });

    // External selection state for the data-less ExperimentDetail panel.
    let selected = use_signal(|| Option::<ResourceSummary>::None);
    // Editable manifest / template documents for the IdePanels.
    let manifest_doc = use_signal(|| "# Select an experiment to load its manifest.\n".to_string());
    let template_doc = use_signal(|| "# Load a template to view its YAML.\n".to_string());

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
            Panel::Experiments => experiments_view(snap, ws, selected, manifest_doc),
            Panel::ExperimentDetail => experiment_detail_view(selected),
            Panel::ExperimentMetrics => experiment_metrics_view(selected),
            Panel::ExperimentManifest => experiment_manifest_view(selected, manifest_doc),
            Panel::Campaigns => campaigns_view(snap, ws, selected, manifest_doc),
            Panel::Templates => templates_view(snap, template_doc),
            Panel::RuntimeProfiles => runtime_view(snap),
            Panel::Benchmarks => benchmarks_view(snap),
        }
    };

    rsx! {
        style { {panel_kit::CSS} }
        style { {APP_CSS} }
        div {
            class: ws.root_class(),
            onmousemove: move |e| ws.handle_mouse_move(&e),
            onmouseup: move |_| ws.handle_mouse_up(),
            header { class: "topbar",
                h1 { "Athena Console" }
                span { class: "hint", "Kubernetes research operator dashboard · drag, resize, tile panels" }
            }
            {ws.render(body)}
            {ws.dock()}
        }
    }
}

// ---------------------------------------------------------------------------
// Views
// ---------------------------------------------------------------------------

fn experiments_view(
    snap: ClusterSnapshot,
    ws: panel_kit::Workspace<Panel>,
    mut selected: Signal<Option<ResourceSummary>>,
    mut manifest_doc: Signal<String>,
) -> Element {
    let rows = snap.experiments;
    rsx! {
        div { class: "view-head",
            h2 { "Experiments" }
            p { "Kubernetes-native experiment resources, phases, and workspace refs." }
        }
        table { class: "tbl",
            thead {
                tr {
                    th { "Experiment" }
                    th { "Phase" }
                    th { "Workspace" }
                }
            }
            tbody {
                if rows.is_empty() {
                    tr { td { colspan: "3", class: "muted", "No experiments found." } }
                }
                for exp in rows {
                    {
                        let exp_select = exp.clone();
                        rsx! {
                            tr {
                                td {
                                    button {
                                        class: "row-link",
                                        onclick: move |_| {
                                            let e = exp_select.clone();
                                            selected.set(Some(e.clone()));
                                            // Surface all three per-experiment panels.
                                            ws.restore(Panel::ExperimentDetail);
                                            ws.restore(Panel::ExperimentMetrics);
                                            ws.restore(Panel::ExperimentManifest);
                                            // Load the manifest YAML into the IDE panel.
                                            spawn(async move {
                                                match fetch_manifest(&e.namespace, &e.kind, &e.name).await {
                                                    Ok(yaml) => manifest_doc.set(yaml),
                                                    Err(err) => manifest_doc.set(format!("# failed to load manifest: {err}\n")),
                                                }
                                            });
                                        },
                                        "{exp.name}"
                                    }
                                    div { class: "muted", "{exp.namespace}" }
                                }
                                td { class: "phase", "{exp.phase}" }
                                td {
                                    div { "{exp.workspace_path.clone().unwrap_or_else(|| \"Not reported\".to_string())}" }
                                    div { class: "muted", "{exp.detail}" }
                                }
                            }
                        }
                    }
                }
            }
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
    ws: panel_kit::Workspace<Panel>,
    r: ResourceSummary,
) {
    selected.set(Some(r.clone()));
    ws.restore(Panel::ExperimentDetail);
    ws.restore(Panel::ExperimentMetrics);
    ws.restore(Panel::ExperimentManifest);
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
            GrafanaPanel {
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
    ws: panel_kit::Workspace<Panel>,
    selected: Signal<Option<ResourceSummary>>,
    manifest_doc: Signal<String>,
) -> Element {
    let rows = snap.campaigns;
    rsx! {
        div { class: "view-head",
            h2 { "Campaigns" }
            p { "Click a campaign to load its metrics (all member experiments, over the campaign window) into the Metrics panel." }
        }
        table { class: "tbl",
            thead {
                tr { th { "Campaign" } th { "Phase" } th { "Progress" } }
            }
            tbody {
                if rows.is_empty() {
                    tr { td { colspan: "3", class: "muted", "No campaigns found." } }
                }
                for c in rows {
                    {
                        let cs = c.clone();
                        rsx! {
                            tr {
                                td {
                                    button {
                                        class: "row-link",
                                        onclick: move |_| select_resource(selected, manifest_doc, ws, cs.clone()),
                                        "{c.name}"
                                    }
                                    div { class: "muted", "{c.namespace}" }
                                }
                                td { class: "phase", "{c.phase}" }
                                td { div { class: "muted", "{c.detail}" } }
                            }
                        }
                    }
                }
            }
        }
    }
}

fn templates_view(snap: ClusterSnapshot, mut template_doc: Signal<String>) -> Element {
    let rows: Vec<TemplateSummary> = snap.templates;
    rsx! {
        div { class: "view-head",
            h2 { "Experiment Templates" }
            p { "Load Kubernetes-owned template YAML and inspect objectives + sources." }
        }
        table { class: "tbl",
            thead {
                tr { th { "Template" } th { "Objective" } th { "Source" } th { "" } }
            }
            tbody {
                if rows.is_empty() {
                    tr { td { colspan: "4", class: "muted", "No templates found." } }
                }
                for tpl in rows {
                    {
                        let tpl_load = tpl.clone();
                        rsx! {
                            tr {
                                td {
                                    div { "{tpl.name}" }
                                    div { class: "muted", "{tpl.namespace}" }
                                }
                                td { class: "phase", "{tpl.objective}" }
                                td { "{tpl.detail}" }
                                td {
                                    button {
                                        class: "btn",
                                        onclick: move |_| {
                                            let t = tpl_load.clone();
                                            spawn(async move {
                                                match fetch_template_yaml(&t.namespace, &t.name).await {
                                                    Ok(yaml) => template_doc.set(yaml),
                                                    Err(err) => template_doc.set(format!("# failed to load template: {err}\n")),
                                                }
                                            });
                                        },
                                        "Load YAML"
                                    }
                                }
                            }
                        }
                    }
                }
            }
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
fn resource_table(rows: Vec<ResourceSummary>, name_col: &str, detail_col: &str) -> Element {
    rsx! {
        table { class: "tbl",
            thead {
                tr { th { "{name_col}" } th { "Phase" } th { "{detail_col}" } }
            }
            tbody {
                if rows.is_empty() {
                    tr { td { colspan: "3", class: "muted", "Nothing found." } }
                }
                for r in rows {
                    tr {
                        td {
                            div { "{r.name}" }
                            div { class: "muted", "{r.namespace}" }
                        }
                        td { class: "phase", "{r.phase}" }
                        td { "{r.detail}" }
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
