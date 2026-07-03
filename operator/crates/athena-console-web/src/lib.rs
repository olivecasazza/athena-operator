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
use models::{ClusterSnapshot, ReportSpecDto, ReportSummary, ResourceSummary, TemplateSummary};
use std::collections::{BTreeMap, HashSet};
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
    /// Compose a campaign's experiments into a research-paper dataset.
    ReportCurator,
    /// Published ResearchReport list.
    Reports,
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
        b.at(Panel::ReportCurator, 16.0, 1380.0, 640.0, 520.0),
        b.at(Panel::Reports, 16.0, 1916.0, 640.0, 260.0),
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
.btn:hover { border-color:var(--pink); }
.section-label { font-size:.78rem; color:var(--fg); margin:.5rem 0 .25rem; }
.embed-block { height:340px; margin:.4rem 0; }
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

    // Report Curator state.
    let selected_campaign = use_signal(|| Option::<ResourceSummary>::None);
    let report_name = use_signal(|| String::new());
    let report_title = use_signal(|| String::new());
    let excluded: Signal<HashSet<String>> = use_signal(|| HashSet::new());
    let sec_abstract = use_signal(|| String::new());
    let sec_related_work = use_signal(|| String::new());
    let sec_discussion = use_signal(|| String::new());
    let sec_limitations = use_signal(|| String::new());
    let seeds_text = use_signal(|| String::new());
    let preview_doc = use_signal(|| String::new());
    let save_status = use_signal(|| String::new());

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
            Panel::Reports => reports_view(snap, ws, selected_campaign, report_name, report_title),
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
        div { class: "scroll-tbl",
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

fn reports_view(
    snap: ClusterSnapshot,
    ws: panel_kit::Workspace<Panel>,
    mut selected_campaign: Signal<Option<ResourceSummary>>,
    mut report_name: Signal<String>,
    mut report_title: Signal<String>,
) -> Element {
    let rows = snap.reports;
    let campaigns = snap.campaigns;
    rsx! {
        div { class: "view-head",
            h2 { "Reports" }
            p { "Published ResearchReport resources. Click Load to open one in the Report Curator." }
        }
        table { class: "tbl",
            thead {
                tr {
                    th { "Report" }
                    th { "Campaign" }
                    th { "Title" }
                    th { "Phase" }
                    th { "Excluded" }
                    th { "" }
                }
            }
            tbody {
                if rows.is_empty() {
                    tr { td { colspan: "6", class: "muted", "No reports found." } }
                }
                for r in rows {
                    {
                        let r2 = r.clone();
                        let camps = campaigns.clone();
                        rsx! {
                            tr {
                                td {
                                    div { "{r.name}" }
                                    div { class: "muted", "{r.namespace}" }
                                }
                                td { "{r.campaign_ref}" }
                                td { "{r.title}" }
                                td { class: "phase", "{r.phase}" }
                                td { "{r.excluded_count}" }
                                td {
                                    button {
                                        class: "btn",
                                        onclick: move |_| {
                                            if let Some(camp) = camps.iter().find(|c| c.name == r2.campaign_ref) {
                                                selected_campaign.set(Some(camp.clone()));
                                            }
                                            report_name.set(r2.name.clone());
                                            report_title.set(r2.title.clone());
                                            ws.restore(Panel::ReportCurator);
                                        },
                                        "Load"
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
        if t.is_empty() { None } else { Some(t.to_string()) }
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
    let campaigns_for_change = snap.campaigns.clone();
    let campaigns = snap.campaigns;
    let experiments = snap.experiments;

    let sel_campaign = selected_campaign.read().clone();
    let sel_name = sel_campaign
        .as_ref()
        .map(|c| c.name.clone())
        .unwrap_or_default();

    let exp_rows: Vec<ResourceSummary> = experiments
        .into_iter()
        .filter(|e| {
            !sel_name.is_empty() && e.campaign.as_deref() == Some(sel_name.as_str())
        })
        .collect();

    let excluded_set = excluded.read().clone();

    rsx! {
        div { class: "view-head",
            h2 { "Report Curator" }
            p { "Compose a campaign's experiments into a research-paper dataset (ResearchReport)." }
        }

        div { class: "section-label", "Campaign" }
        select {
            class: "rc-select",
            onchange: move |e| {
                let name = e.value();
                if let Some(found) = campaigns_for_change.iter().find(|c| c.name == name) {
                    selected_campaign.set(Some(found.clone()));
                    excluded.set(HashSet::new());
                } else {
                    selected_campaign.set(None);
                }
            },
            option { value: "", "— select a campaign —" }
            for c in &campaigns {
                {
                    let cname = c.name.clone();
                    let cns = c.namespace.clone();
                    let is_sel = cname == sel_name;
                    rsx! {
                        option { value: "{cname}", selected: is_sel, "{cname} ({cns})" }
                    }
                }
            }
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
        table { class: "tbl",
            thead {
                tr {
                    th { "Include" }
                    th { "Experiment" }
                    th { "Phase" }
                    th { "Detail" }
                }
            }
            tbody {
                if sel_name.is_empty() {
                    tr { td { colspan: "4", class: "muted", "Select a campaign." } }
                } else if exp_rows.is_empty() {
                    tr { td { colspan: "4", class: "muted", "No experiments in this campaign." } }
                }
                for exp in exp_rows {
                    {
                        let exp_name = exp.name.clone();
                        let is_included = !excluded_set.contains(&exp_name);
                        rsx! {
                            tr {
                                td {
                                    input {
                                        r#type: "checkbox",
                                        checked: is_included,
                                        onchange: move |_| {
                                            let mut set = excluded.read().clone();
                                            if set.contains(&exp_name) {
                                                set.remove(&exp_name);
                                            } else {
                                                set.insert(exp_name.clone());
                                            }
                                            excluded.set(set);
                                        }
                                    }
                                }
                                td { "{exp.name}" }
                                td { class: "phase", "{exp.phase}" }
                                td { "{exp.detail}" }
                            }
                        }
                    }
                }
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
    resp.json::<ReportSummary>().await.map_err(|e| e.to_string())
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
