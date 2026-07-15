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
    #[serde(default)]
    pub metrics_endpoint: MetricsEndpoint,
    /// Image pull secrets for private registries.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub image_pull_secrets: Vec<crate::common::LocalObjectReference>,
    /// Plain name/value environment variables injected into the job container.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub env: Vec<EnvVar>,
    /// Secrets mounted as files (e.g. a GCS service-account key).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub secret_mounts: Vec<SecretMount>,
    /// Optional SkyPilot cloud-burst layer. Absent = on-prem Kubernetes Job
    /// (today's behavior). Present + enabled = the operator builds a small
    /// launcher Job that renders a sky task from the experiment (same image /
    /// command / env the k8s path would use) and runs `sky launch` — the chain
    /// is sky -> kueue -> ray -> experiment: the LAUNCHER Job (not the cloud
    /// node) is what Kueue admits, so cloud bursts still flow through quota
    /// accounting. Quota then represents "concurrent experiments", not local
    /// GPUs — intended.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sky: Option<SkySpec>,
}

/// SkyPilot cloud-burst settings for a RuntimeProfile.
///
/// TTLs are belt-and-braces: `idleMinutesToAutostop` handles idleness,
/// `ttlMinutes` is a HARD teardown enforced by the launcher (`timeout` around
/// `sky launch --down` followed by an unconditional `sky down`) — a burst
/// never leaves a zombie cloud node.
#[derive(Serialize, Deserialize, Debug, Clone, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SkySpec {
    /// Burst this profile's experiments to the cloud (default true when the
    /// block is present; the block being absent is the on-prem switch).
    #[serde(default = "default_sky_enabled")]
    pub enabled: bool,
    /// SkyPilot accelerator string, e.g. `A10:8`. Unset = CPU-only task.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accelerators: Option<String>,
    /// Optional cloud pin (e.g. `gcp`). Unset = SkyPilot's ordered/cheapest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cloud: Option<String>,
    /// Use spot/preemptible instances.
    #[serde(default = "default_sky_use_spot")]
    pub use_spot: bool,
    /// Hard autostop/teardown in minutes — the launcher tears the cluster down
    /// unconditionally once this elapses, even if the task hangs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ttl_minutes: Option<u32>,
    /// `sky launch --idle-minutes-to-autostop` window.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idle_minutes_to_autostop: Option<u32>,
    /// Secret (in the Job's namespace) whose keys are injected as env vars into
    /// the launcher container for sky CLI cloud credentials.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env_secret_ref: Option<String>,
}

impl Default for SkySpec {
    fn default() -> Self {
        Self {
            enabled: default_sky_enabled(),
            accelerators: None,
            cloud: None,
            use_spot: default_sky_use_spot(),
            ttl_minutes: None,
            idle_minutes_to_autostop: None,
            env_secret_ref: None,
        }
    }
}

fn default_sky_enabled() -> bool {
    true
}

fn default_sky_use_spot() -> bool {
    true
}

#[derive(Serialize, Deserialize, Debug, Clone, JsonSchema, Default)]
#[serde(rename_all = "camelCase")]
pub struct EnvVar {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, JsonSchema, Default)]
#[serde(rename_all = "camelCase")]
pub struct SecretMount {
    pub secret_name: String,
    pub mount_path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sub_path: Option<String>,
}

/// Live metrics integration for experiment Jobs.
///
/// The operator injects the scrape `port`/`path` and (optionally) a Prometheus
/// query URL into each experiment pod, so training code can transparently
/// *store* metrics (expose them for scrape) and *retrieve* them for live
/// display (query Prometheus) during a run. A chart-rendered `PodMonitor`
/// selects pods labelled `app.kubernetes.io/name=athena-experiment` and scrapes
/// `port`; the cluster Prometheus discovers it (empty podMonitorSelector).
///
/// `metrics.json` in the workspace remains the authoritative final summary;
/// Prometheus is additive live data, not a replacement. Retrieval that feeds a
/// control-flow decision must read authoritative CR status / artifacts instead.
#[derive(Serialize, Deserialize, Debug, Clone, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct MetricsEndpoint {
    /// Expose live metrics for scraping (default true).
    #[serde(default = "default_metrics_enabled")]
    pub enabled: bool,
    /// Port the experiment exposes Prometheus-text metrics on.
    #[serde(default = "default_metrics_port")]
    pub port: i32,
    /// HTTP path the metrics are served at.
    #[serde(default = "default_metrics_path")]
    pub path: String,
    /// Prometheus HTTP query base URL injected for in-job display retrieval
    /// (e.g. `http://prometheus-operated.monitoring.svc:9090`). When unset the
    /// retrieve-for-display path is disabled in the job.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prometheus_query_url: Option<String>,
}

impl Default for MetricsEndpoint {
    fn default() -> Self {
        Self {
            enabled: default_metrics_enabled(),
            port: default_metrics_port(),
            path: default_metrics_path(),
            prometheus_query_url: None,
        }
    }
}

fn default_metrics_enabled() -> bool {
    true
}

fn default_metrics_port() -> i32 {
    9108
}

fn default_metrics_path() -> String {
    "/metrics".to_string()
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
    #[serde(default = "default_create_workspace_claim")]
    pub create_workspace_claim: bool,
    #[serde(
        default = "default_workspace_access_modes",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub workspace_access_modes: Vec<String>,
    #[serde(default = "default_workspace_size")]
    pub workspace_size: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_storage_class_name: Option<String>,
}

impl Default for StorageProfile {
    fn default() -> Self {
        Self {
            workspace_claim_name: "athena-workspace".to_string(),
            workspace_mount_path: "/workspace".to_string(),
            create_workspace_claim: true,
            workspace_access_modes: default_workspace_access_modes(),
            workspace_size: default_workspace_size(),
            workspace_storage_class_name: None,
        }
    }
}

fn default_create_workspace_claim() -> bool {
    true
}

fn default_workspace_access_modes() -> Vec<String> {
    vec!["ReadWriteOnce".to_string()]
}

fn default_workspace_size() -> String {
    "20Gi".to_string()
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
    /// Container runtime class for the experiment pods (e.g. `nvidia` on k3s
    /// nodes where the NVIDIA runtime is not containerd's default — without it
    /// the pod gets the GPU resource but no driver).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_class_name: Option<String>,
    /// Kueue LocalQueue (in the Job's namespace) to submit this experiment's
    /// Job to. When set, the operator stamps the `kueue.x-k8s.io/queue-name`
    /// label and creates the Job suspended, so Kueue admits it against the
    /// queue's quota. Unset → Jobs schedule directly, no Kueue involvement.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue_name: Option<String>,
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
