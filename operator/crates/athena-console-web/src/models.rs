//! Wire DTOs shared between the axum backend and the Dioxus frontend.
//!
//! The native console used `athena-api` (kube `CustomResource` types) directly.
//! Those cannot compile to wasm (kube pulls hyper/tokio TCP), so the frontend
//! cannot depend on `athena-api`. Instead the server collapses the CR `spec` +
//! `status` into these lean serde structs and serves them as JSON; the frontend
//! deserializes the same structs. This is the "shared models" contract — the
//! serde shape is the boundary, with `athena-api` staying server-side.

use serde::{Deserialize, Serialize};

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
