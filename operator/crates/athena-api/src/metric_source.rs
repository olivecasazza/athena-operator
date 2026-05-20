use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::common::Condition;

#[derive(CustomResource, Serialize, Deserialize, Debug, Clone, JsonSchema)]
#[kube(
    group = "research.nixlab.io",
    version = "v1alpha1",
    kind = "MetricSource",
    plural = "metricsources",
    singular = "metricsource",
    shortname = "msrc",
    namespaced,
    status = "MetricSourceStatus",
    printcolumn = r#"{"name":"Type","type":"string","jsonPath":".spec.sourceType"}"#,
    printcolumn = r#"{"name":"Format","type":"string","jsonPath":".spec.format"}"#,
    printcolumn = r#"{"name":"Ready","type":"boolean","jsonPath":".status.ready"}"#,
    printcolumn = r#"{"name":"Age","type":"date","jsonPath":".metadata.creationTimestamp"}"#
)]
#[serde(rename_all = "camelCase")]
pub struct MetricSourceSpec {
    pub source_type: MetricSourceType,
    pub path: String,
    pub format: MetricFormat,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub metrics: Vec<MetricMapping>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timestamp_path: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub failure_rules: Vec<MetricFailureRule>,
}

#[derive(Serialize, Deserialize, Debug, Clone, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum MetricSourceType {
    File,
    StdoutRegex,
    Prometheus,
    Loki,
    HttpJson,
    ArtifactManifest,
}

#[derive(Serialize, Deserialize, Debug, Clone, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum MetricFormat {
    Json,
    Jsonl,
    PrometheusText,
    Regex,
    Junit,
    Custom,
}

#[derive(Serialize, Deserialize, Debug, Clone, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct MetricMapping {
    pub name: String,
    pub path: String,
    #[serde(rename = "type")]
    pub metric_type: MetricValueType,
    #[serde(default)]
    pub required: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub normalize: Option<MetricNormalize>,
}

#[derive(Serialize, Deserialize, Debug, Clone, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum MetricValueType {
    Number,
    Integer,
    String,
    Boolean,
}

#[derive(Serialize, Deserialize, Debug, Clone, JsonSchema, Default)]
#[serde(rename_all = "camelCase")]
pub struct MetricNormalize {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub multiply: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offset: Option<f64>,
}

#[derive(Serialize, Deserialize, Debug, Clone, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct MetricFailureRule {
    pub path: String,
    pub equals: String,
    pub reason: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, JsonSchema, Default)]
#[serde(rename_all = "camelCase")]
pub struct MetricSourceStatus {
    pub ready: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_generation: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_validation_time: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sample_validated: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conditions: Vec<Condition>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub controller_version: Option<String>,
}
