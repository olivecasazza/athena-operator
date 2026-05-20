use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::common::{Budget, Condition, LocalObjectReference, MetricGoal};

#[derive(CustomResource, Serialize, Deserialize, Debug, Clone, JsonSchema)]
#[kube(
    group = "research.nixlab.io",
    version = "v1alpha1",
    kind = "BenchmarkSuite",
    plural = "benchmarksuites",
    singular = "benchmarksuite",
    shortname = "bsuite",
    namespaced,
    status = "BenchmarkSuiteStatus",
    printcolumn = r#"{"name":"Taxonomy","type":"string","jsonPath":".spec.taxonomy"}"#,
    printcolumn = r#"{"name":"Ready","type":"boolean","jsonPath":".status.ready"}"#,
    printcolumn = r#"{"name":"Tasks","type":"integer","jsonPath":".status.taskCount"}"#,
    printcolumn = r#"{"name":"Age","type":"date","jsonPath":".metadata.creationTimestamp"}"#
)]
#[serde(rename_all = "camelCase")]
pub struct BenchmarkSuiteSpec {
    pub taxonomy: BenchmarkTaxonomy,
    pub suite_version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suite_hash: Option<String>,
    #[serde(default)]
    pub target_ref_policy: TargetRefPolicy,
    pub tasks: Vec<BenchmarkTask>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub metric_sources: Vec<NamedMetricSourceRef>,
    #[serde(default)]
    pub integrity: BenchmarkIntegrityPolicy,
    #[serde(default)]
    pub reporting: BenchmarkReporting,
}

#[derive(Serialize, Deserialize, Debug, Clone, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum BenchmarkTaxonomy {
    RlTraining,
    LlmCapability,
    ResearchLoop,
    RuntimeHealth,
}

#[derive(Serialize, Deserialize, Debug, Clone, JsonSchema, Default)]
#[serde(rename_all = "camelCase")]
pub struct TargetRefPolicy {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_kinds: Vec<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BenchmarkTask {
    pub name: String,
    pub integration: BenchmarkIntegration,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub labels: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dataset_ref: Option<DatasetRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evaluator: Option<EvaluatorSpec>,
    pub metrics: TaskMetricsSpec,
    pub budget: Budget,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seeds: Option<SeedSpec>,
    #[serde(default)]
    pub execution: TaskExecutionSpec,
}

#[derive(Serialize, Deserialize, Debug, Clone, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum BenchmarkIntegration {
    ToyRl,
    CustomCommand,
    LmEvaluationHarness,
    LiveCodeBench,
    SweBenchHook,
}

#[derive(Serialize, Deserialize, Debug, Clone, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DatasetRef {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_type: Option<DatasetSourceType>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub split: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum DatasetSourceType {
    HfDataset,
    Seaweedfs,
    S3,
    Pvc,
    SecretHoldout,
}

#[derive(Serialize, Deserialize, Debug, Clone, JsonSchema, Default)]
#[serde(rename_all = "camelCase")]
pub struct EvaluatorSpec {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub command: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args_template: Vec<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TaskMetricsSpec {
    pub primary: String,
    pub goal: MetricGoal,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub required: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub optional: Vec<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, JsonSchema, Default)]
#[serde(rename_all = "camelCase")]
pub struct SeedSpec {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub values: Vec<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aggregation: Option<SeedAggregation>,
}

#[derive(Serialize, Deserialize, Debug, Clone, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum SeedAggregation {
    MeanStd,
    MedianIqr,
}

#[derive(Serialize, Deserialize, Debug, Clone, JsonSchema, Default)]
#[serde(rename_all = "camelCase")]
pub struct TaskExecutionSpec {
    #[serde(default)]
    pub grouped: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct NamedMetricSourceRef {
    pub name: String,
    pub metric_source_ref: LocalObjectReference,
}

#[derive(Serialize, Deserialize, Debug, Clone, JsonSchema, Default)]
#[serde(rename_all = "camelCase")]
pub struct BenchmarkIntegrityPolicy {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub holdout_policy: Option<HoldoutPolicy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub leakage_policy: Option<LeakagePolicy>,
    #[serde(default)]
    pub immutable_inputs_required: bool,
    #[serde(default)]
    pub require_image_digest: bool,
    #[serde(default)]
    pub require_git_commit: bool,
    #[serde(default)]
    pub require_dataset_hash: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum HoldoutPolicy {
    PublicOnly,
    PrivateHoldout,
    Mixed,
}

#[derive(Serialize, Deserialize, Debug, Clone, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum LeakagePolicy {
    Warn,
    BlockOnMatch,
}

#[derive(Serialize, Deserialize, Debug, Clone, JsonSchema, Default)]
#[serde(rename_all = "camelCase")]
pub struct BenchmarkReporting {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub compare_against: Vec<LocalObjectReference>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub gates: Vec<BenchmarkGate>,
}

#[derive(Serialize, Deserialize, Debug, Clone, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BenchmarkGate {
    pub metric: String,
    pub operator: GateOperator,
    pub value: f64,
}

#[derive(Serialize, Deserialize, Debug, Clone, JsonSchema, PartialEq, Eq)]
pub enum GateOperator {
    #[serde(rename = ">=")]
    GreaterThanOrEqual,
    #[serde(rename = ">")]
    GreaterThan,
    #[serde(rename = "<=")]
    LessThanOrEqual,
    #[serde(rename = "<")]
    LessThan,
}

#[derive(Serialize, Deserialize, Debug, Clone, JsonSchema, Default)]
#[serde(rename_all = "camelCase")]
pub struct BenchmarkSuiteStatus {
    pub ready: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_generation: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_suite_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_count: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub public_task_count: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub holdout_task_count: Option<u32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conditions: Vec<Condition>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_validation_time: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub controller_version: Option<String>,
}
