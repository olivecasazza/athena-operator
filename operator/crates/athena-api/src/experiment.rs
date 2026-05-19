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
