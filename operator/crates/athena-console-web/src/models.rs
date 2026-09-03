//! Wire DTOs shared between the axum backend and the Dioxus frontend.
//!
//! The native console used `athena-api` (kube `CustomResource` types) directly.
//! Those cannot compile to wasm (kube pulls hyper/tokio TCP), so the frontend
//! cannot depend on `athena-api`. Instead the server collapses the CR `spec` +
//! `status` into these lean serde structs and serves them as JSON; the frontend
//! deserializes the same structs. This is the "shared models" contract — the
//! serde shape is the boundary, with `athena-api` staying server-side.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// One snapshot of all Athena resources in the cluster — the JSON payload of
/// `GET /api/snapshot`. Mirrors the native `ClusterSnapshot`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ClusterSnapshot {
    pub experiments: Vec<ResourceSummary>,
    pub campaigns: Vec<ResourceSummary>,
    pub templates: Vec<TemplateSummary>,
    pub benchmark_suites: Vec<ResourceSummary>,
    pub benchmark_runs: Vec<ResourceSummary>,
    pub runtime_profiles: Vec<ResourceSummary>,
    /// Scientist-authored paper-dataset curations (ResearchReport).
    #[serde(default)]
    pub reports: Vec<ReportSummary>,
    /// The autonomous loops themselves — phase, curriculum stage, health
    /// conditions, per-template gate evidence. The drill-down's global level
    /// leads with these because the drive is the root of the ownership chain
    /// the navigation mirrors (drive > campaign > experiment/report).
    #[serde(default)]
    pub drives: Vec<DriveSummary>,
}

/// One status condition, flattened for the wire. `type` is a Rust keyword, so
/// the field is `ctype` renamed on the wire.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ConditionDto {
    #[serde(rename = "type")]
    pub ctype: String,
    pub status: String,
    #[serde(default)]
    pub reason: String,
}

/// A ResearchDrive, collapsed for the console's global view.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DriveSummary {
    pub namespace: String,
    pub name: String,
    pub phase: String,
    #[serde(default)]
    pub stage: Option<String>,
    #[serde(default)]
    pub stagnation: u32,
    #[serde(default)]
    pub conditions: Vec<ConditionDto>,
    #[serde(default)]
    pub stages: Vec<StageProgressDto>,
}

/// One curriculum stage's record, with the per-template gate evidence that
/// answers "which line is holding promotion back".
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct StageProgressDto {
    pub name: String,
    #[serde(default)]
    pub promoted_at: Option<String>,
    #[serde(default)]
    pub templates: Vec<TemplateProgressDto>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TemplateProgressDto {
    pub template_ref: String,
    #[serde(default)]
    pub best_objective: Option<f64>,
    #[serde(default)]
    pub succeeded: u32,
    #[serde(default)]
    pub passed: bool,
}

/// A generic Kubernetes resource row (experiment, campaign, suite, run,
/// profile). Mirrors the native `ResourceSummary` minus the per-resource metric
/// panel computation — the Grafana iframe is the metrics surface now.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ResourceSummary {
    pub namespace: String,
    pub name: String,
    /// Lowercase singular CRD kind, e.g. `"experiment"`, `"benchmarkrun"`.
    pub kind: String,
    pub phase: String,
    pub detail: String,
    #[serde(default)]
    pub workspace_path: Option<String>,
    #[serde(default)]
    pub logs_link: Option<String>,
    #[serde(default)]
    pub metrics_link: Option<String>,
    /// Run-window start as epoch-millis string (for scoping the Grafana embed).
    #[serde(default)]
    pub started_at: Option<String>,
    /// Run-window end as epoch-millis string; None = still running (use "now").
    #[serde(default)]
    pub ended_at: Option<String>,
    /// For experiments: the parent campaign (`spec.campaignRef`), so the report
    /// curator can filter experiments by campaign. None for other kinds.
    #[serde(default)]
    pub campaign: Option<String>,
    /// Trained behavior (stance/locomotion/forage/arena) — experiments declare
    /// it in parameters; campaigns get stage context from the drive instead.
    #[serde(default)]
    pub mode: Option<String>,
    /// Experiment hypothesis (truncated server-side); the drill-down's
    /// experiment view shows it, because a run without its question is noise.
    #[serde(default)]
    pub hypothesis: Option<String>,
    /// Status conditions (campaigns: ExperimentsHealthy et al).
    #[serde(default)]
    pub conditions: Vec<ConditionDto>,
}

impl ResourceSummary {
    /// `k8s://<ns>/<kind>/<name>.yaml` — the manifest identifier shown in the
    /// IDE panel header, matching the native console's `manifest_path()`.
    pub fn manifest_path(&self) -> String {
        format!("k8s://{}/{}/{}.yaml", self.namespace, self.kind, self.name)
    }
}

/// An ExperimentTemplate row. Mirrors the native `TemplateSummary`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct TemplateSummary {
    pub namespace: String,
    pub name: String,
    pub objective: String,
    pub detail: String,
}

/// A ResearchReport row for the reports list.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ReportSummary {
    pub namespace: String,
    pub name: String,
    pub campaign_ref: String,
    pub title: String,
    pub phase: String,
    #[serde(default)]
    pub excluded_count: usize,
    /// Full narrative sections — the report IS the research record, so the
    /// console renders it whole rather than linking away from it.
    #[serde(default)]
    pub sections: BTreeMap<String, String>,
    #[serde(default)]
    pub seeded_hypotheses: Vec<String>,
}

/// Payload for creating/updating a ResearchReport and for previewing its dossier.
/// Mirrors `athena_api::research_report::ResearchReportSpec` plus the object's
/// namespace/name so a single struct drives both save and preview.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ReportSpecDto {
    pub namespace: String,
    pub name: String,
    pub campaign_ref: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub included_experiments: Vec<String>,
    #[serde(default)]
    pub excluded_experiments: Vec<String>,
    #[serde(default)]
    pub sections: BTreeMap<String, String>,
    #[serde(default)]
    pub seeded_hypotheses: Vec<String>,
}

// ---------------------------------------------------------------------------
// Scheduling / inference stack (admin views). Mirrors
// `athena_api::scheduling::*` on the wire (camelCase) — `GET /api/scheduling`.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SchedulingSnapshot {
    pub pools: Vec<GpuPool>,
    pub workloads: Vec<WorkloadRow>,
    pub nodes: Vec<NodePower>,
    pub inference: Vec<InferenceBackend>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct GpuPool {
    pub name: String,
    pub gpu_nominal: i64,
    pub gpu_used: i64,
    pub cpu_nominal: i64,
    pub cpu_used: i64,
    pub pending_workloads: i64,
    pub admitted_workloads: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct WorkloadRow {
    pub name: String,
    pub namespace: String,
    pub queue: String,
    pub priority_class: String,
    pub state: String,
    pub gpus: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct NodePower {
    pub name: String,
    pub powered: bool,
    pub phase: String,
    pub pool: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct InferenceBackend {
    pub campaign: String,
    pub kind: String,
    pub name: String,
    pub serving: bool,
    pub endpoint: String,
}
