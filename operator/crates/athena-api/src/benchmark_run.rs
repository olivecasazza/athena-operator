use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::common::{
    ArtifactUris, Budget, Condition, LocalObjectReference, MetricAggregate, TypedObjectReference,
};
use crate::experiment::ExperimentMetricSeries;

#[derive(CustomResource, Serialize, Deserialize, Debug, Clone, JsonSchema)]
#[kube(
    group = "research.nixlab.io",
    version = "v1alpha1",
    kind = "BenchmarkRun",
    plural = "benchmarkruns",
    singular = "benchmarkrun",
    shortname = "brun",
    namespaced,
    status = "BenchmarkRunStatus",
    printcolumn = r#"{"name":"Suite","type":"string","jsonPath":".spec.suiteRef.name"}"#,
    printcolumn = r#"{"name":"Phase","type":"string","jsonPath":".status.phase"}"#,
    printcolumn = r#"{"name":"Age","type":"date","jsonPath":".metadata.creationTimestamp"}"#
)]
#[serde(rename_all = "camelCase")]
pub struct BenchmarkRunSpec {
    pub suite_ref: LocalObjectReference,
    pub target_ref: TypedObjectReference,
    #[serde(default)]
    pub mode: BenchmarkRunMode,
    #[serde(default)]
    pub suspend: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_selector: Option<TaskSelector>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_profile_ref: Option<LocalObjectReference>,
    #[serde(default)]
    pub budget: Budget,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed_matrix: Option<SeedMatrix>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<BenchmarkRunOutput>,
    #[serde(default)]
    pub promotion_policy: PromotionPolicy,
    #[serde(default)]
    pub cleanup_policy: CleanupPolicy,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_parallel_tasks: Option<u32>,
}

#[derive(Serialize, Deserialize, Debug, Clone, JsonSchema, Default, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum BenchmarkRunMode {
    #[default]
    Full,
    Subset,
    Smoke,
    HoldoutOnly,
    Replay,
}

#[derive(Serialize, Deserialize, Debug, Clone, JsonSchema, Default)]
#[serde(rename_all = "camelCase")]
pub struct TaskSelector {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub names: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub labels: BTreeMap<String, String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, JsonSchema, Default)]
#[serde(rename_all = "camelCase")]
pub struct SeedMatrix {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub seeds: Vec<i64>,
    #[serde(default)]
    pub deterministic: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone, JsonSchema, Default)]
#[serde(rename_all = "camelCase")]
pub struct BenchmarkRunOutput {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_path: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, JsonSchema, Default)]
#[serde(rename_all = "camelCase")]
pub struct PromotionPolicy {
    #[serde(default)]
    pub update_experiment_status: bool,
    #[serde(default)]
    pub block_on_holdout_failure: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone, JsonSchema, Default)]
#[serde(rename_all = "camelCase")]
pub struct CleanupPolicy {
    #[serde(default)]
    pub ttl_seconds_after_finished: Option<i32>,
    #[serde(default)]
    pub retain_failed_jobs: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone, JsonSchema, Default)]
#[serde(rename_all = "camelCase")]
pub struct BenchmarkRunStatus {
    #[serde(default)]
    pub phase: BenchmarkRunPhase,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_generation: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_time: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion_time: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_suite_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reproducibility_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub job_names: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub task_results: Vec<TaskResultSummary>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub aggregate_metrics: BTreeMap<String, MetricAggregate>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub metric_series: Vec<ExperimentMetricSeries>,
    #[serde(default)]
    pub cost: BenchmarkRunCost,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub gates: Vec<GateResult>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logs_link: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metrics_link: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub report_uri: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conditions: Vec<Condition>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub controller_version: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, JsonSchema, Default, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum BenchmarkRunPhase {
    #[default]
    Pending,
    Preparing,
    Running,
    Succeeded,
    Failed,
    Error,
    Cancelled,
    Skipped,
}

#[derive(Serialize, Deserialize, Debug, Clone, JsonSchema, Default)]
#[serde(rename_all = "camelCase")]
pub struct TaskResultSummary {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed: Option<i64>,
    #[serde(default)]
    pub phase: BenchmarkRunPhase,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metrics: BTreeMap<String, f64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub metric_series: Vec<ExperimentMetricSeries>,
    #[serde(default)]
    pub artifacts: ArtifactUris,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gpu_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt: Option<u32>,
}

#[derive(Serialize, Deserialize, Debug, Clone, JsonSchema, Default)]
#[serde(rename_all = "camelCase")]
pub struct BenchmarkRunCost {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gpu_hours: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wall_clock_seconds: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_tokens: Option<i64>,
}

#[derive(Serialize, Deserialize, Debug, Clone, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct GateResult {
    pub metric: String,
    pub passed: bool,
    pub threshold: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actual: Option<f64>,
}
