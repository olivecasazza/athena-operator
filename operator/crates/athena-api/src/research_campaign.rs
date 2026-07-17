use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(CustomResource, Serialize, Deserialize, Debug, Clone, JsonSchema)]
#[kube(
    group = "research.nixlab.io",
    version = "v1alpha1",
    kind = "ResearchCampaign",
    plural = "researchcampaigns",
    singular = "researchcampaign",
    shortname = "rcp",
    namespaced,
    status = "ResearchCampaignStatus",
    printcolumn = r#"{"name":"Template","type":"string","jsonPath":".spec.templateRef"}"#,
    printcolumn = r#"{"name":"Running","type":"integer","jsonPath":".status.runningExperiments"}"#,
    printcolumn = r#"{"name":"Succeeded","type":"integer","jsonPath":".status.succeededExperiments"}"#,
    printcolumn = r#"{"name":"Age","type":"date","jsonPath":".metadata.creationTimestamp"}"#
)]
#[serde(rename_all = "camelCase")]
pub struct ResearchCampaignSpec {
    pub template_ref: String,
    #[serde(default)]
    pub concurrency: u32,
    #[serde(default)]
    pub budget: CampaignBudget,
    #[serde(default)]
    pub strategy: StrategySpec,

    /// Optional BenchmarkSuite to evaluate each succeeded experiment against.
    /// When set, the campaign creates a BenchmarkRun per succeeded experiment
    /// (targetRef = the Experiment) and DEFERS the Keep/Discard decision to the
    /// benchmark's gate results (via promotionPolicy.updateExperimentStatus)
    /// instead of stamping it from the raw training objective.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub benchmark_suite_ref: Option<String>,

    /// RuntimeProfile (BatchJob mode) the BenchmarkRun executes in. Required for
    /// benchmarking to actually run — an Experiment targetRef is not itself a
    /// RuntimeProfile, so the run can't fall back to it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub benchmark_runtime_profile_ref: Option<String>,

    /// Population size for population-based training (PBT) strategies.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub population_size: Option<u32>,

    /// Perturbation factor applied to hyperparameters during PBT exploit/explore.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub perturb_factor: Option<f64>,

    /// Optional ephemeral inference mesh (mesh-llm) brought up for the campaign's
    /// active lifetime and torn down when the campaign reaches terminal phase.
    /// While set, every experiment the campaign creates gets `LLM_BASE_URL`
    /// injected to point at the mesh Service, and experiment generation is gated
    /// on the mesh becoming Ready.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inference_mesh: Option<InferenceMeshSpec>,

    /// Optional ephemeral MULTI-NODE inference cluster (vLLM tensor/pipeline
    /// parallel over Ray) for models too big for one GPU. The operator creates a
    /// Kueue-admitted RayJob (mesh-high priority → reclaims GPUs from spot) plus a
    /// stable head Service; experiments get `LLM_BASE_URL` injected and generation
    /// is gated on it serving. Torn down at terminal phase. Mutually exclusive with
    /// inferenceMesh (single-node); if both set, inferenceCluster wins.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inference_cluster: Option<VllmClusterSpec>,

    /// Optional canary stage: one cheap gate experiment generated before any
    /// budgeted experiments. Generation of further experiments is held until the
    /// canary succeeds AND (if a benchmark suite gates it) its gates pass.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub canary: Option<CanarySpec>,
}

/// One cheap gate experiment run before the campaign spends real budget.
///
/// Rationale: full-length training runs (e.g. 25M-step, ~280 GPU-h sweeps) have
/// repeatedly been burned on recipes a 2M-step canary + behavioral probes would
/// have vetoed in ~40 minutes. With a canary configured the campaign generates
/// exactly one experiment (`<campaign>-canary`) and holds everything else until
/// it Succeeds and its benchmark gates pass; a Failed canary (or a gate Discard)
/// parks the campaign in phase `CanaryFailed` without generating anything
/// further. Hard rule for cloud bursts: nothing reaches `sky launch` without a
/// green canary.
#[derive(Serialize, Deserialize, Debug, Clone, JsonSchema, Default)]
#[serde(rename_all = "camelCase")]
pub struct CanarySpec {
    /// Parameter overrides deep-merged over the template's default parameters
    /// (canary wins at any depth; arrays replace wholesale) for the canary
    /// experiment only. Typical use: {"total_timesteps": 2000000}.
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    #[schemars(schema_with = "crate::common::json_value_schema")]
    pub parameters: serde_json::Value,

    /// Gate suite for the canary; falls back to the campaign's
    /// benchmarkSuiteRef. If neither is set, the gate is simply "the canary
    /// experiment Succeeded".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub benchmark_suite_ref: Option<String>,

    /// Optional wall-clock bound for the canary. Recorded for runners/humans
    /// but NOT enforced by the controller yet — budget.maxDuration isn't
    /// enforced either, and the canary mirrors that mechanism rather than
    /// inventing a new one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_duration: Option<String>,
}

/// Ephemeral multi-node vLLM-on-Ray inference cluster. Serves one model split
/// across `pipelineParallelSize` GPUs (head = driver + rank 0, plus N-1 workers)
/// via a RayJob. Verified config: vLLM 0.6.6 / ray 2.40.0 on Quadro-RTX-4000
/// (Turing) needs fp16 + XFORMERS + enforce-eager; head MUST carry a GPU (driver
/// does CUDA device inference); max-model-len bounded by 8GB KV-cache headroom.
#[derive(Serialize, Deserialize, Debug, Clone, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct VllmClusterSpec {
    /// vLLM OpenAI image (must bundle ray == rayVersion). e.g. vllm/vllm-openai:v0.6.6.post1
    pub image: String,
    /// HF model ref (safetensors), e.g. deepseek-ai/DeepSeek-Prover-V2-7B.
    pub model: String,
    /// Total GPUs to split the model across (= pipeline-parallel-size). 1 head + (n-1) workers.
    pub pipeline_parallel_size: u32,
    /// OpenAI API port vLLM serves on.
    #[serde(default = "default_vllm_port")]
    pub port: u16,
    /// Ray version — MUST match the ray bundled in `image` or GCS registration fails.
    #[serde(default = "default_ray_version")]
    pub ray_version: String,
    /// Max sequence length; bounded by per-GPU KV-cache headroom after weights.
    #[serde(default = "default_max_model_len")]
    pub max_model_len: u32,
    /// vLLM dtype. Turing has no efficient bf16 → float16.
    #[serde(default = "default_dtype")]
    pub dtype: String,
    /// nodeSelector `nvidia.com/gpu.product` value (the GPU pool to land on).
    #[serde(default = "default_gpu_product")]
    pub gpu_product: String,
    /// Kueue LocalQueue name (maps to the GPU ClusterQueue).
    #[serde(default = "default_cluster_queue_name")]
    pub queue_name: String,
    /// Kueue WorkloadPriorityClass — `mesh-high` preempts default-priority spot.
    #[serde(default = "default_priority_class")]
    pub priority_class: String,
    /// Pod tolerations (raw JSON). Defaults to the nvidia GPU taint if empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[schemars(schema_with = "crate::common::json_value_schema")]
    pub tolerations: Vec<serde_json::Value>,
    /// runtimeClassName (nvidia on k3s GPU nodes).
    #[serde(default = "default_nvidia_runtime_class")]
    pub runtime_class_name: String,
    /// Extra args appended to `vllm serve`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extra_args: Vec<String>,
}

fn default_vllm_port() -> u16 {
    8000
}
fn default_ray_version() -> String {
    "2.40.0".to_string()
}
fn default_max_model_len() -> u32 {
    4096
}
fn default_dtype() -> String {
    "float16".to_string()
}
fn default_gpu_product() -> String {
    "Quadro-RTX-4000".to_string()
}
fn default_cluster_queue_name() -> String {
    "athena-gpu".to_string()
}
fn default_priority_class() -> String {
    "mesh-high".to_string()
}
fn default_nvidia_runtime_class() -> String {
    "nvidia".to_string()
}

/// Ephemeral OpenAI-compatible inference endpoint (mesh-llm) the operator brings
/// up for the duration of a campaign. Runs as a companion Deployment+Service the
/// campaign owns; keep it OUT of the Kueue-managed GPU pool (e.g. pin to traitor)
/// so it can't over-subscribe quota Kueue arbitrates for experiment Jobs.
#[derive(Serialize, Deserialize, Debug, Clone, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct InferenceMeshSpec {
    /// Container image serving the mesh-llm OpenAI endpoint.
    pub image: String,
    /// Model to serve (mesh-llm catalog name, HF ref, or in-image path).
    pub model: String,
    /// API port. mesh-llm defaults to 9337.
    #[serde(default = "default_mesh_port")]
    pub port: u16,
    /// nodeSelector for the mesh pod (e.g. pin to a specific host/pool).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub node_selector: BTreeMap<String, String>,
    /// Pod tolerations (raw JSON, mirrors SchedulingProfile.tolerations).
    /// json_value_schema is REQUIRED: schemars emits `items: {}` for
    /// serde_json::Value and the apiserver rejects untyped array items
    /// (caught live: helm CRD replace failed on this exact field).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[schemars(schema_with = "crate::common::json_value_schema")]
    pub tolerations: Vec<serde_json::Value>,
    /// GPU resource requests/limits, e.g. {"amd.com/gpu": "1"} or
    /// {"nvidia.com/gpu": "2"}. Empty = CPU-only.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub gpu_resources: BTreeMap<String, String>,
    /// runtimeClassName (e.g. "nvidia"). Required for NVIDIA GPU pods on k3s.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_class_name: Option<String>,
    /// Extra args appended to `mesh-llm serve`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extra_args: Vec<String>,
}

fn default_mesh_port() -> u16 {
    9337
}

#[derive(Serialize, Deserialize, Debug, Clone, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CampaignBudget {
    pub max_experiments: u32,
    pub max_duration: String,
}

impl Default for CampaignBudget {
    fn default() -> Self {
        Self {
            max_experiments: 300,
            max_duration: "24h".to_string(),
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct StrategySpec {
    #[serde(rename = "type")]
    pub strategy_type: String,
}

impl Default for StrategySpec {
    fn default() -> Self {
        Self {
            strategy_type: "heuristic".to_string(),
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, JsonSchema, Default)]
#[serde(rename_all = "camelCase")]
pub struct ResearchCampaignStatus {
    pub running_experiments: u32,
    pub succeeded_experiments: u32,
    pub failed_experiments: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub best_experiment: Option<String>,
    /// Total experiments the campaign has generated so far.
    #[serde(default)]
    pub total_experiments: u32,
    /// Best objective value observed across succeeded experiments (per the
    /// template objective goal). Lets you watch the loop improve over iterations.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub best_objective: Option<f64>,
    /// Loop phase: Running while generating/evaluating, Completed at budget.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_generation: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub controller_version: Option<String>,
    /// Name of the canary gate Experiment, once created (spec.canary only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub canary_experiment: Option<String>,
    /// Canary gate state: "pending" | "running" | "passed" | "failed".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub canary_state: Option<String>,
}
