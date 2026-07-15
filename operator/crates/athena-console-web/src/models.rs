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
