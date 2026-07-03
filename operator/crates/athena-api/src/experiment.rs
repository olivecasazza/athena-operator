use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(CustomResource, Serialize, Deserialize, Debug, Clone, JsonSchema)]
#[kube(
    group = "research.nixlab.io",
    version = "v1alpha1",
    kind = "Experiment",
    plural = "experiments",
    singular = "experiment",
    shortname = "exp",
    namespaced,
    status = "ExperimentStatus",
    printcolumn = r#"{"name":"Phase","type":"string","jsonPath":".status.phase"}"#,
    printcolumn = r#"{"name":"Decision","type":"string","jsonPath":".status.decision"}"#,
    printcolumn = r#"{"name":"Age","type":"date","jsonPath":".metadata.creationTimestamp"}"#
)]
#[serde(rename_all = "camelCase")]
pub struct ExperimentSpec {
    pub campaign_ref: String,
    pub hypothesis: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub parameters: BTreeMap<String, serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub patch: Option<PatchSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkpoint_policy: Option<CheckpointPolicy>,
}

#[derive(Serialize, Deserialize, Debug, Clone, JsonSchema, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CheckpointPolicy {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interval_seconds: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retain: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resume_from: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, JsonSchema, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CheckpointRef {
    pub uri: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub objective_value: Option<f64>,
}

#[derive(Serialize, Deserialize, Debug, Clone, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PatchSpec {
    #[serde(rename = "type")]
    pub patch_type: PatchType,
    pub value: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, JsonSchema, PartialEq)]
#[serde(rename_all = "camelCase")]
pub enum PatchType {
    GitPatch,
    None,
}

#[derive(Serialize, Deserialize, Debug, Clone, JsonSchema, Default)]
#[serde(rename_all = "camelCase")]
pub struct ExperimentStatus {
    pub phase: ExperimentPhase,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logs_link: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metrics_link: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metrics: BTreeMap<String, serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision: Option<ExperimentDecision>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment: Option<ExperimentEnvironment>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resources: Option<ExperimentResources>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metrics_detail: Option<ExperimentMetrics>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifacts: Option<ExperimentArtifacts>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost: Option<ExperimentCost>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conditions: Option<Vec<ExperimentCondition>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dashboard: Option<crate::experiment_template::DashboardSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest_checkpoint: Option<CheckpointRef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub checkpoints: Vec<CheckpointRef>,
}

// --- Denormalized status sub-types ---

#[derive(Serialize, Deserialize, Debug, Clone, JsonSchema, Default)]
#[serde(rename_all = "camelCase")]
pub struct ExperimentEnvironment {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skypilot_cluster: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skypilot_job_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub namespace: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pod_names: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_names: Option<Vec<String>>,
}

#[derive(Serialize, Deserialize, Debug, Clone, JsonSchema, Default)]
#[serde(rename_all = "camelCase")]
pub struct ExperimentResources {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gpu_requested: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gpu_allocated: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gpu_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu_requested: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_requested: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, JsonSchema, Default)]
#[serde(rename_all = "camelCase")]
pub struct ExperimentMetrics {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub objective_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub objective_goal: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(schema_with = "crate::common::json_value_schema")]
    pub latest: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(schema_with = "crate::common::json_value_schema")]
    pub best: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metrics_path: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub series: Vec<ExperimentMetricSeries>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_updated: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, JsonSchema, Default)]
#[serde(rename_all = "camelCase")]
pub struct ExperimentMetricSeries {
    pub tag: String,
    pub iteration: i64,
    pub objective: String,
    pub goal: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub points: Vec<ExperimentMetricPoint>,
}

#[derive(Serialize, Deserialize, Debug, Clone, JsonSchema, Default)]
#[serde(rename_all = "camelCase")]
pub struct ExperimentMetricPoint {
    pub name: String,
    pub value: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step: Option<i64>,
}

#[derive(Serialize, Deserialize, Debug, Clone, JsonSchema, Default)]
#[serde(rename_all = "camelCase")]
pub struct ExperimentArtifacts {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_uri: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metrics_uri: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkpoints_uri: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reports_uri: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_commit: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub onnx_uri: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub best_checkpoint_uri: Option<String>,
    /// Append-only JSONL research journal the runner writes over the run
    /// (hypothesis, config, per-eval progress, dead ends, final result). Durable
    /// on the shared workspace PVC; the primary narrative source for paper writeups.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub journal_uri: Option<String>,
    /// Write-once provenance/reproducibility manifest (seed, resolved params, git
    /// commit, image, software/hardware environment) the runner emits at startup.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance_uri: Option<String>,
    /// Directory of rendered scientific figures (loss curves etc.) plus the
    /// re-plottable per-step series the runner writes alongside them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub figures_uri: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, JsonSchema, Default)]
#[serde(rename_all = "camelCase")]
pub struct ExperimentCost {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub estimated_usd: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gpu_hours: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_seconds: Option<i64>,
}

#[derive(Serialize, Deserialize, Debug, Clone, JsonSchema, Default)]
#[serde(rename_all = "camelCase")]
pub struct ExperimentCondition {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[serde(rename = "type")]
    pub condition_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_transition_time: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, JsonSchema, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub enum ExperimentPhase {
    #[default]
    Pending,
    Preparing,
    Running,
    Succeeded,
    Failed,
    Error,
}

#[derive(Serialize, Deserialize, Debug, Clone, JsonSchema, PartialEq)]
#[serde(rename_all = "camelCase")]
pub enum ExperimentDecision {
    Keep,
    Discard,
    NeedsReview,
}
