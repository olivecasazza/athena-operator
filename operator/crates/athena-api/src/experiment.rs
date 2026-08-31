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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkpoint_policy: Option<CheckpointPolicy>,
    /// Structured record of WHY this experiment exists: which node it came
    /// from, by what operation, and what changed.
    ///
    /// Before this field, the entire search decision lived in the free-text
    /// `hypothesis` string, and the parent pointer was smuggled into
    /// `parameters` as an untyped `parentExperimentId` entry that then had to
    /// be filtered back out in two independent places. Worse, the selection
    /// gate detected control runs by PREFIX-MATCHING the hypothesis prose —
    /// editing that string silently emptied the sigma sample and froze the
    /// incumbent forever with no error. Prose is not a type tag.
    ///
    /// Deliberately NOT an ownerReference: an ownerRef to the parent would
    /// cascade-delete an entire subtree when one node is removed. Lifecycle
    /// ownership belongs to the campaign; derivation is a plain reference.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lineage: Option<ExperimentLineage>,
    /// Extra env vars merged into the job container AFTER the RuntimeProfile env,
    /// overriding same-named profile vars. The campaign controller uses this to
    /// inject `LLM_BASE_URL` for an ephemeral inference mesh.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub env: Vec<crate::runtime_profile::EnvVar>,
}

/// How an experiment relates to the node it derived from.
///
/// A closed enum, not prose: enums aggregate and can be selected on, prose
/// cannot. This is what the selection gate keys off to find control runs, and
/// what a provenance export maps to a predicate.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, JsonSchema, PartialEq, Eq)]
pub enum DerivationRelation {
    /// Campaign's first point — template defaults, no parent.
    Baseline,
    /// Cold-start population spread: perturbed from defaults, no parent yet.
    Seed,
    /// Parameters perturbed away from the parent (the ordinary search step).
    Perturb,
    /// Parent's parameters re-run UNCHANGED on a fresh seed, to measure noise
    /// rather than to search. This is the control slot.
    Remeasure,
    /// Same science point re-run because the local search space was exhausted.
    Replicate,
    /// Cheap gate probe of the recipe before the campaign spends budget.
    Canary,
}

/// One parameter's movement between parent and child.
#[derive(Serialize, Deserialize, Debug, Clone, JsonSchema, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ParameterDelta {
    pub param: String,
    /// Value on the parent (absent for a cold start with no parent).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<f64>,
    pub to: f64,
    /// Multiplicative factor applied, when the operation was multiplicative.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub factor: Option<f64>,
}

/// Structured provenance for one experiment.
///
/// PROV-O export note: an Experiment is a RUN, i.e. a `prov:Activity`, so the
/// correct predicate for this edge is `prov:wasInformedBy` (Activity ->
/// Activity). `prov:wasDerivedFrom` would be wrong — its domain and range are
/// both `prov:Entity`. PROV-O has no native slot for "by perturbing X", which
/// is what `perturbations` carries.
#[derive(Serialize, Deserialize, Debug, Clone, JsonSchema, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ExperimentLineage {
    pub relation: DerivationRelation,
    /// Parent experiment name. None for a baseline/seed/canary with no parent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    /// Parent UID. Names are reused when a campaign is recreated; the UID makes
    /// a stale edge detectable rather than silently wrong.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_uid: Option<String>,
    /// Generation index (idx / populationSize). Previously computed only to
    /// hash a seed and then discarded, so the tree had no depth coordinate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generation: Option<u32>,
    /// Strategy that produced this child ("pbt", "heuristic", ...).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strategy: Option<String>,
    /// Exactly what moved. Empty for a Remeasure/Replicate by definition.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub perturbations: Vec<ParameterDelta>,
    /// Dedup re-roll counter that produced this point, for reproducibility.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub salt: Option<u32>,
}

impl ExperimentLineage {
    /// Human-readable rendering, so `hypothesis` stays a DERIVED view of the
    /// structured record rather than the record itself.
    pub fn describe(&self) -> String {
        let deltas = self
            .perturbations
            .iter()
            .map(|d| match (d.from, d.factor) {
                (Some(f), Some(x)) => format!("{} {:.4}->{:.4} (x{:.4})", d.param, f, d.to, x),
                (Some(f), None) => format!("{} {:.4}->{:.4}", d.param, f, d.to),
                (None, Some(x)) => format!("{} x{:.4}", d.param, x),
                (None, None) => format!("{}={:.4}", d.param, d.to),
            })
            .collect::<Vec<_>>()
            .join(", ");
        let strategy = self.strategy.as_deref().unwrap_or("search");
        match (&self.relation, &self.parent) {
            (DerivationRelation::Baseline, _) => "baseline: template defaults".to_string(),
            (DerivationRelation::Canary, _) => {
                "canary gate: cheap probe of the recipe before spending budget".to_string()
            }
            (DerivationRelation::Seed, _) if deltas.is_empty() => {
                format!("{strategy} cold start: no numeric params to spread")
            }
            (DerivationRelation::Seed, _) => {
                format!("{strategy} cold start: seed spread from defaults: {deltas}")
            }
            (DerivationRelation::Remeasure, Some(p)) => format!(
                "control: re-measure incumbent {p} on a fresh seed \
                 (calibrates sigma; not a search point)"
            ),
            (DerivationRelation::Replicate, Some(p)) => {
                format!("replicate of {p}: local search space exhausted")
            }
            (DerivationRelation::Perturb, Some(p)) if deltas.is_empty() => {
                format!("{strategy} exploit from {p} (no numeric params to perturb)")
            }
            (DerivationRelation::Perturb, Some(p)) => {
                format!("{strategy} exploit+explore from {p}: {deltas}")
            }
            (_, None) => format!("{strategy}: no successful parent yet"),
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, JsonSchema, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CheckpointPolicy {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interval_seconds: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retain: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resume_from: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, JsonSchema, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CheckpointRef {
    pub uri: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub objective_value: Option<f64>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment: Option<ExperimentEnvironment>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resources: Option<ExperimentResources>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metrics_detail: Option<ExperimentMetrics>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifacts: Option<ExperimentArtifacts>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost: Option<ExperimentCost>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conditions: Option<Vec<ExperimentCondition>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dashboard: Option<crate::experiment_template::DashboardSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest_checkpoint: Option<CheckpointRef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub checkpoints: Vec<CheckpointRef>,
}

// --- Denormalized status sub-types ---

#[derive(Serialize, Deserialize, Debug, Clone, JsonSchema, Default)]
#[serde(rename_all = "camelCase")]
pub struct ExperimentEnvironment {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skypilot_cluster: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skypilot_job_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub namespace: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pod_names: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_names: Option<Vec<String>>,

    /// Container image the run actually executed, as rendered from the
    /// RuntimeProfile at Job creation.
    ///
    /// Provenance, not decoration: warm-starting a child from a parent trained
    /// under a DIFFERENT image can be silent corruption rather than transfer.
    /// Observed live at the curriculum v4->v5 cutover: v4 mapped joint targets
    /// as `centre + a * half` and v5 as `defaults + a * 0.10 * half`, so the
    /// same weights command entirely different poses. Three runs were about to
    /// resume across that boundary and would have produced plausible,
    /// meaningless numbers. Without this field the controller could not tell
    /// the two apart, so it could not refuse.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, JsonSchema, Default)]
#[serde(rename_all = "camelCase")]
pub struct ExperimentResources {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gpu_requested: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gpu_allocated: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gpu_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu_requested: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_requested: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, JsonSchema, Default)]
#[serde(rename_all = "camelCase")]
pub struct ExperimentMetrics {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub objective_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub objective_goal: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(schema_with = "crate::common::json_value_schema")]
    pub latest: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(schema_with = "crate::common::json_value_schema")]
    pub best: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metrics_path: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub series: Vec<ExperimentMetricSeries>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_updated: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, JsonSchema, Default)]
#[serde(rename_all = "camelCase")]
pub struct ExperimentMetricSeries {
    pub tag: String,
    pub iteration: i64,
    pub objective: String,
    pub goal: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub points: Vec<ExperimentMetricPoint>,
}

#[derive(Serialize, Deserialize, Debug, Clone, JsonSchema, Default)]
#[serde(rename_all = "camelCase")]
pub struct ExperimentMetricPoint {
    pub name: String,
    pub value: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step: Option<i64>,
}

#[derive(Serialize, Deserialize, Debug, Clone, JsonSchema, Default)]
#[serde(rename_all = "camelCase")]
pub struct ExperimentArtifacts {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_uri: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metrics_uri: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkpoints_uri: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reports_uri: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_commit: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub onnx_uri: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub best_checkpoint_uri: Option<String>,
    /// Append-only JSONL research journal the runner writes over the run
    /// (hypothesis, config, per-eval progress, dead ends, final result). Durable
    /// on the shared workspace PVC; the primary narrative source for paper writeups.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub journal_uri: Option<String>,
    /// Write-once provenance/reproducibility manifest (seed, resolved params, git
    /// commit, image, software/hardware environment) the runner emits at startup.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance_uri: Option<String>,
    /// Directory of rendered scientific figures (loss curves etc.) plus the
    /// re-plottable per-step series the runner writes alongside them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub figures_uri: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, JsonSchema, Default)]
#[serde(rename_all = "camelCase")]
pub struct ExperimentCost {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub estimated_usd: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gpu_hours: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_seconds: Option<i64>,
}

#[derive(Serialize, Deserialize, Debug, Clone, JsonSchema, Default)]
#[serde(rename_all = "camelCase")]
pub struct ExperimentCondition {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[serde(rename = "type")]
    pub condition_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_transition_time: Option<String>,
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
