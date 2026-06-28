use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

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
