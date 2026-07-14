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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
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
}
