use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(CustomResource, Serialize, Deserialize, Debug, Clone, JsonSchema)]
#[kube(
    group = "research.nixlab.io",
    version = "v1alpha1",
    kind = "RuntimeProfile",
    plural = "runtimeprofiles",
    singular = "runtimeprofile",
    shortname = "rtp",
    namespaced,
    status = "RuntimeProfileStatus",
    printcolumn = r#"{"name":"Runtime","type":"string","jsonPath":".spec.runtime.type"}"#,
    printcolumn = r#"{"name":"Mode","type":"string","jsonPath":".spec.runtime.mode"}"#,
    printcolumn = r#"{"name":"Ready","type":"boolean","jsonPath":".status.ready"}"#,
    printcolumn = r#"{"name":"Age","type":"date","jsonPath":".metadata.creationTimestamp"}"#
)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeProfileSpec {
    pub runtime: RuntimeSpec,
    pub image: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pull_policy: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub command: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    #[serde(default)]
    pub resources: ResourceProfile,
    #[serde(default)]
    pub storage: StorageProfile,
    #[serde(default)]
    pub scheduling: SchedulingProfile,
    #[serde(default)]
    pub policy: RuntimePolicy,
}

#[derive(Serialize, Deserialize, Debug, Clone, JsonSchema, Default)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeSpec {
    #[serde(rename = "type")]
    pub runtime_type: RuntimeType,
    pub mode: ExecutionMode,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub config: BTreeMap<String, serde_json::Value>,
}

#[derive(Serialize, Deserialize, Debug, Clone, JsonSchema, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub enum RuntimeType {
    #[default]
    Pytorch,
    Mlx,
    Ollama,
    Vllm,
    LlamaCpp,
    Skypilot,
    Modal,
    Custom,
}

#[derive(Serialize, Deserialize, Debug, Clone, JsonSchema, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub enum ExecutionMode {
    #[default]
    BatchJob,
    Service,
    ExternalJob,
}

#[derive(Serialize, Deserialize, Debug, Clone, JsonSchema, Default)]
#[serde(rename_all = "camelCase")]
pub struct ResourceProfile {
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub requests: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub limits: BTreeMap<String, String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct StorageProfile {
    pub workspace_claim_name: String,
    pub workspace_mount_path: String,
}

impl Default for StorageProfile {
    fn default() -> Self {
        Self {
            workspace_claim_name: "athena-workspace".to_string(),
            workspace_mount_path: "/workspace".to_string(),
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, JsonSchema, Default)]
#[serde(rename_all = "camelCase")]
pub struct SchedulingProfile {
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub node_selector: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[schemars(schema_with = "crate::common::json_value_schema")]
    pub tolerations: Vec<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority_class_name: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RuntimePolicy {
    pub allow_image_override: bool,
    pub allow_command_override: bool,
    pub allow_secret_refs: bool,
}

impl Default for RuntimePolicy {
    fn default() -> Self {
        Self {
            allow_image_override: false,
            allow_command_override: false,
            allow_secret_refs: false,
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, JsonSchema, Default)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeProfileStatus {
    pub ready: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}
