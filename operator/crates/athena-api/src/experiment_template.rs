use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(CustomResource, Serialize, Deserialize, Debug, Clone, JsonSchema)]
#[kube(
    group = "research.nixlab.io",
    version = "v1alpha1",
    kind = "ExperimentTemplate",
    plural = "experimenttemplates",
    singular = "experimenttemplate",
    shortname = "ext",
    namespaced,
    status = "ExperimentTemplateStatus"
)]
#[serde(rename_all = "camelCase")]
pub struct ExperimentTemplateSpec {
    pub runtime_profile_ref: String,
    pub source: SourceSpec,
    pub objective: ObjectiveSpec,
    #[serde(default)]
    pub metrics: MetricsSpec,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub parameter_schema: BTreeMap<String, ParameterSpec>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub defaults: BTreeMap<String, serde_json::Value>,
}

#[derive(Serialize, Deserialize, Debug, Clone, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SourceSpec {
    pub git: GitSource,
}

#[derive(Serialize, Deserialize, Debug, Clone, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct GitSource {
    pub url: String,
    pub r#ref: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ObjectiveSpec {
    pub metric: String,
    pub goal: ObjectiveGoal,
}

#[derive(Serialize, Deserialize, Debug, Clone, JsonSchema, PartialEq)]
#[serde(rename_all = "camelCase")]
pub enum ObjectiveGoal {
    Minimize,
    Maximize,
}

#[derive(Serialize, Deserialize, Debug, Clone, JsonSchema, Default)]
#[serde(rename_all = "camelCase")]
pub struct MetricsSpec {
    #[serde(default)]
    pub parser: MetricsParser,
}

#[derive(Serialize, Deserialize, Debug, Clone, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct MetricsParser {
    #[serde(rename = "type")]
    pub parser_type: MetricsParserType,
    #[serde(default = "default_metrics_path")]
    pub path: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub patterns: BTreeMap<String, String>,
}

impl Default for MetricsParser {
    fn default() -> Self {
        Self {
            parser_type: MetricsParserType::File,
            path: default_metrics_path(),
            patterns: BTreeMap::new(),
        }
    }
}

fn default_metrics_path() -> String {
    "metrics.json".to_string()
}

#[derive(Serialize, Deserialize, Debug, Clone, JsonSchema, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub enum MetricsParserType {
    #[default]
    File,
    Regex,
}

#[derive(Serialize, Deserialize, Debug, Clone, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ParameterSpec {
    #[serde(rename = "type")]
    pub parameter_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(schema_with = "crate::common::json_value_schema")]
    pub default: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, JsonSchema, Default)]
#[serde(rename_all = "camelCase")]
pub struct ExperimentTemplateStatus {
    pub ready: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}