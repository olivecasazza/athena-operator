use schemars::JsonSchema;
use schemars::r#gen::SchemaGenerator;
use schemars::schema::{Schema, SchemaObject};
use serde::{Deserialize, Serialize};

pub fn json_value_schema(_gen: &mut SchemaGenerator) -> Schema {
    let mut schema = SchemaObject::default();
    schema.extensions.insert(
        "x-kubernetes-preserve-unknown-fields".to_string(),
        serde_json::Value::Bool(true),
    );
    schema.into()
}

#[derive(Serialize, Deserialize, Debug, Clone, JsonSchema, Default)]
#[serde(rename_all = "camelCase")]
pub struct LocalObjectReference {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub namespace: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TypedObjectReference {
    pub api_version: String,
    pub kind: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub namespace: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, JsonSchema, Default)]
#[serde(rename_all = "camelCase")]
pub struct Condition {
    #[serde(rename = "type")]
    pub condition_type: String,
    pub status: ConditionStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_transition_time: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, JsonSchema, Default, PartialEq, Eq)]
pub enum ConditionStatus {
    #[default]
    Unknown,
    True,
    False,
}

#[derive(Serialize, Deserialize, Debug, Clone, JsonSchema, Default)]
#[serde(rename_all = "camelCase")]
pub struct Budget {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_wall_clock: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_gpu_hours: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_retries: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_concurrency: Option<u32>,
}

#[derive(Serialize, Deserialize, Debug, Clone, JsonSchema, Default)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactUris {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metrics_uri: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub report_uri: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manifest_uri: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, JsonSchema, Default)]
#[serde(rename_all = "camelCase")]
pub struct MetricAggregate {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mean: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub std: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub count: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ci_low: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ci_high: Option<f64>,
}

#[derive(Serialize, Deserialize, Debug, Clone, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum MetricGoal {
    Maximize,
    Minimize,
}

#[derive(Serialize, Deserialize, Debug, Clone, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum FailureClass {
    UserCode,
    Infrastructure,
    BudgetExceeded,
    IntegrityViolation,
    MetricParseError,
    Preempted,
    Unknown,
}
