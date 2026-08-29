use crate::common::Condition;
use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(CustomResource, Serialize, Deserialize, Debug, Clone, JsonSchema)]
#[kube(
    group = "research.nixlab.io",
    version = "v1alpha1",
    kind = "ResearchDrive",
    plural = "researchdrives",
    singular = "researchdrive",
    shortname = "rsd",
    namespaced,
    status = "ResearchDriveStatus",
    printcolumn = r#"{"name":"Phase","type":"string","jsonPath":".status.phase"}"#,
    printcolumn = r#"{"name":"Campaigns","type":"integer","jsonPath":".status.campaignsCompleted"}"#,
    printcolumn = r#"{"name":"Age","type":"date","jsonPath":".metadata.creationTimestamp"}"#
)]
#[serde(rename_all = "camelCase")]
pub struct ResearchDriveSpec {
    /// Research domain label (e.g. "spot-locomotion"). Low-cardinality; used in
    /// metrics and proposer context.
    pub domain: String,

    /// ExperimentTemplates the drive's proposer may launch campaigns against.
    /// The controller REJECTS any proposed campaign whose templateRef is not in
    /// this list. Minimum one entry.
    pub template_refs: Vec<String>,

    /// LLM proposer configuration. The controller calls this OpenAI-compatible
    /// endpoint when a campaign the drive owns reaches a terminal phase, asking
    /// for the next campaign(s) — the perpetual loop's hypothesis generator.
    pub proposer: ProposerSpec,

    /// Hard bounds the controller enforces on the drive regardless of what the
    /// proposer asks for.
    #[serde(default)]
    pub limits: DriveLimits,

    /// Stagnation policy: when the last `window` completed campaigns produced no
    /// sigma-gated improvement over the drive's best, the drive parks in phase
    /// NeedsHuman instead of proposing further.
    #[serde(default)]
    pub stagnation: StagnationSpec,

    /// Gate for proposals that are NOT plain campaigns (harness, rigging, or
    /// sim-design changes). RequireApproval (default): the drive records the
    /// proposal in status.proposals and waits for the
    /// `research.nixlab.io/approve-proposal: "<id>"` annotation. Auto: structural
    /// proposals are still only RECORDED (the controller cannot apply code
    /// changes), but the drive keeps proposing/running campaigns without pausing.
    #[serde(default)]
    pub structural_change_policy: StructuralChangePolicy,

    /// Manual brake: while true the controller proposes and creates nothing.
    #[serde(default)]
    pub paused: bool,

    /// Ordered training curriculum. When set, the drive may only launch
    /// campaigns against the CURRENT stage's templates (intersected with
    /// `templateRefs`), and advances a stage only when that stage's promotion
    /// criteria are met from campaign status.
    ///
    /// Absent (the default) preserves the previous behaviour exactly: every
    /// template in `templateRefs` is proposable at any time. Without this the
    /// stage ORDER lives in proposer prose, which makes "stance before
    /// locomotion" a suggestion an LLM may ignore rather than an invariant the
    /// controller enforces.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub curriculum: Option<CurriculumSpec>,
}

/// Ordered stages a morphology must pass through, e.g.
/// stance -> locomotion -> forage -> arena.
#[derive(Serialize, Deserialize, Debug, Clone, JsonSchema, Default)]
#[serde(rename_all = "camelCase")]
pub struct CurriculumSpec {
    /// Stages in order. The first is entered on creation. Bounded by the CRD
    /// schema at 16 to keep status.stageHistory bounded too.
    #[serde(default)]
    pub stages: Vec<CurriculumStage>,
}

#[derive(Serialize, Deserialize, Debug, Clone, JsonSchema, Default)]
#[serde(rename_all = "camelCase")]
pub struct CurriculumStage {
    /// Stable low-cardinality stage name (used in metrics labels and status).
    pub name: String,

    /// Templates proposable while this stage is current. Intersected with the
    /// drive's `templateRefs`, so the allowlist remains the outer bound.
    #[serde(default)]
    pub template_refs: Vec<String>,

    /// Name of an EARLIER stage whose winning experiment seeds campaigns in
    /// this stage (sets `spec.seedExperimentRef`, which carries both parameters
    /// and the checkpoint the runner warm-starts from). This is what makes the
    /// ordering pay for itself instead of each stage cold-starting.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed_from: Option<String>,

    /// Criteria for leaving this stage. Absent means the stage never
    /// auto-promotes (a deliberate human gate).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub promotion: Option<PromotionSpec>,
}

/// When a stage is considered passed. Evaluated from campaign status only.
#[derive(Serialize, Deserialize, Debug, Clone, JsonSchema, Default)]
#[serde(rename_all = "camelCase")]
pub struct PromotionSpec {
    /// Metric to gate on. MUST be a held-out metric: gating on a metric the
    /// reward optimizes promotes reward hacking.
    pub metric: String,

    /// Objective value at or above which the stage counts as passed.
    pub threshold: f64,

    /// Minimum succeeded experiments before promotion may fire, so a single
    /// lucky run cannot advance the curriculum. Counted stage-wide under
    /// `Any`, PER TEMPLATE under `All` — a stage-wide count is meaningless
    /// under `All`, since 8 runs concentrated on one template would satisfy it
    /// while the other templates hold no evidence at all.
    #[serde(default = "default_min_experiments")]
    pub min_experiments: u32,

    /// How many of the stage's templates must clear `threshold`.
    ///
    /// Defaults to `Any` because that is the pre-existing behavior and this
    /// field is additive; a multi-line curriculum almost always wants `All`.
    ///
    /// `Any` promotes on the single best result anywhere in the stage. That is
    /// correct when the templates are alternative routes to ONE goal (pick the
    /// winner and move on), and wrong when each template is its own research
    /// line. Observed live: the multi-robot curriculum promoted stance ->
    /// locomotion on snake's eval_stance_score of 1.000 while spot and
    /// humanoid sat at 0.000 with eval_fall_rate 1.00, so two morphologies
    /// advanced to locomotion having never learned to stand — and the next
    /// stage would warm-start them from checkpoints of a falling robot.
    /// `minExperiments: 8` did not prevent it, because 8 runs stage-wide were
    /// satisfied by the lines that were already succeeding.
    #[serde(default)]
    pub quantifier: PromotionQuantifier,
}

/// How many of a stage's templates must clear the promotion threshold.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, JsonSchema, Default)]
pub enum PromotionQuantifier {
    /// The best result in the stage decides. Templates are alternative routes
    /// to one goal.
    #[default]
    Any,
    /// EVERY template in the stage must independently clear `threshold` with at
    /// least `minExperiments` succeeded runs. Templates are parallel research
    /// lines that must each pass on their own evidence.
    All,
}

fn default_min_experiments() -> u32 {
    3
}

/// Observed curriculum progress. Controller-owned.
#[derive(Serialize, Deserialize, Debug, Clone, JsonSchema, Default)]
#[serde(rename_all = "camelCase")]
pub struct CurriculumStatus {
    /// Name of the stage currently proposable. Empty until first reconcile.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_stage: Option<String>,

    /// One record per stage entered, oldest first. Bounded by spec.stages.
    #[serde(default)]
    pub stage_history: Vec<StageRecord>,
}

#[derive(Serialize, Deserialize, Debug, Clone, JsonSchema, Default)]
#[serde(rename_all = "camelCase")]
pub struct StageRecord {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entered_at: Option<String>,
    /// Set when the stage's promotion criteria were met.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub promoted_at: Option<String>,
    /// Experiment that satisfied promotion; seeds the next stage.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub best_experiment: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub best_objective: Option<f64>,
    /// Succeeded experiments observed in this stage.
    #[serde(default)]
    pub succeeded_experiments: u32,
    /// Per-template evidence within this stage, one entry per
    /// `stage.templateRefs`, ordered as declared.
    ///
    /// Without this, a stage that fails to promote gives no answer to "which
    /// line is holding it back" — the stage-level `bestObjective` shows only
    /// the leader. Bounded by the stage's template count.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub template_progress: Vec<TemplateProgress>,
}

/// One template's evidence inside a curriculum stage. Controller-owned.
#[derive(Serialize, Deserialize, Debug, Clone, JsonSchema, Default)]
#[serde(rename_all = "camelCase")]
pub struct TemplateProgress {
    /// The `ExperimentTemplate` this row summarizes.
    pub template_ref: String,
    /// Best honest score for this template: the unbiased re-measure when the
    /// campaign has one, else its best objective. Same fold the drive-level
    /// best uses, so the two are comparable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub best_objective: Option<f64>,
    /// Experiment holding `bestObjective`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub best_experiment: Option<String>,
    /// Succeeded experiments observed for this template in this stage.
    #[serde(default)]
    pub succeeded_experiments: u32,
    /// Whether this template independently satisfies the stage's promotion
    /// criteria. Under `All`, promotion fires only when every row is true.
    #[serde(default)]
    pub passed: bool,
}

/// OpenAI-compatible LLM endpoint the controller consults for hypotheses.
#[derive(Serialize, Deserialize, Debug, Clone, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProposerSpec {
    /// Base URL of an OpenAI-compatible API (e.g. an in-cluster mesh-llm/vLLM
    /// Service or an external provider). The controller POSTs to
    /// `{endpoint}/chat/completions`.
    pub endpoint: String,
    /// Model identifier passed in the request body.
    pub model: String,
    /// Optional Secret holding the API key. The key's value is sent as a Bearer
    /// token. Omit for unauthenticated in-cluster endpoints.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key_secret_ref: Option<SecretKeyRef>,
    /// Max completion tokens per proposal call.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    /// Sampling temperature.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    /// HTTP timeout for one proposal call.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_seconds: Option<u32>,
}

/// Reference to one key inside a same-namespace Secret.
#[derive(Serialize, Deserialize, Debug, Clone, JsonSchema, Default)]
#[serde(rename_all = "camelCase")]
pub struct SecretKeyRef {
    pub name: String,
    pub key: String,
}

/// Hard bounds enforced by the controller, independent of proposer output.
#[derive(Serialize, Deserialize, Debug, Clone, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DriveLimits {
    /// Maximum simultaneously-running campaigns the drive may own. Proposals
    /// beyond this are queued by deferral: the controller proposes again when a
    /// slot frees.
    #[serde(default = "default_max_active_branches")]
    pub max_active_branches: u32,
    /// Lifetime campaign cap for the drive. Reaching it parks the drive in
    /// phase NeedsHuman (budget exhausted), not Failed.
    #[serde(default = "default_max_campaigns")]
    pub max_campaigns: u32,
    /// Per-campaign budget clamp: any proposed campaign's budget is reduced to
    /// at most these values before creation.
    #[serde(default)]
    pub campaign_budget: CampaignBudgetLimit,
}

impl Default for DriveLimits {
    fn default() -> Self {
        Self {
            max_active_branches: default_max_active_branches(),
            max_campaigns: default_max_campaigns(),
            campaign_budget: CampaignBudgetLimit::default(),
        }
    }
}

fn default_max_active_branches() -> u32 {
    2
}
fn default_max_campaigns() -> u32 {
    100
}

/// Per-campaign budget ceiling applied to every proposed campaign.
#[derive(Serialize, Deserialize, Debug, Clone, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CampaignBudgetLimit {
    pub max_experiments: u32,
    pub max_duration: String,
}

impl Default for CampaignBudgetLimit {
    fn default() -> Self {
        Self {
            max_experiments: 12,
            max_duration: "48h".to_string(),
        }
    }
}

/// Stagnation policy for the perpetual loop.
#[derive(Serialize, Deserialize, Debug, Clone, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct StagnationSpec {
    /// Number of consecutive completed campaigns without a sigma-gated
    /// improvement over the drive's bestObjective before the drive parks in
    /// NeedsHuman.
    #[serde(default = "default_stagnation_window")]
    pub window: u32,
}

impl Default for StagnationSpec {
    fn default() -> Self {
        Self {
            window: default_stagnation_window(),
        }
    }
}

fn default_stagnation_window() -> u32 {
    3
}

/// Gate policy for structural (harness/rigging/sim) proposals.
#[derive(Serialize, Deserialize, Debug, Clone, JsonSchema, Default, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum StructuralChangePolicy {
    /// Structural proposals pause the drive in AwaitingApproval until a human
    /// sets the approval annotation.
    #[default]
    RequireApproval,
    /// Structural proposals are recorded but never pause the loop.
    Auto,
}

/// One campaign the drive currently owns (a live branch of the research DAG).
#[derive(Serialize, Deserialize, Debug, Clone, JsonSchema, Default)]
#[serde(rename_all = "camelCase")]
pub struct BranchRef {
    /// Branch label (proposer-chosen theory name, e.g. "gait-frequency").
    pub name: String,
    /// Name of the ResearchCampaign running this branch.
    pub campaign: String,
    /// ExperimentTemplate the campaign runs against.
    pub template_ref: String,
    /// Campaign this branch forked from (None for root branches).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub forked_from: Option<String>,
}

/// Lifecycle phase of the perpetual loop. Bounded enum per AGENTS.md CRD rules.
#[derive(Serialize, Deserialize, Debug, Clone, JsonSchema, Default, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum DrivePhase {
    /// No campaign running; waiting to propose (or just created).
    #[default]
    Idle,
    /// At least one owned campaign is active.
    CampaignRunning,
    /// A proposer call is in flight for the next campaign(s).
    Proposing,
    /// A structural proposal awaits the approval annotation.
    AwaitingApproval,
    /// Stagnation window or lifetime campaign cap reached; human input needed.
    NeedsHuman,
    /// spec.paused=true; controller proposes and creates nothing.
    Paused,
    /// Terminal controller error (e.g. proposer permanently unreachable).
    Failed,
}

/// Decision state of one recorded proposal. Bounded enum per AGENTS.md CRD rules.
#[derive(Serialize, Deserialize, Debug, Clone, JsonSchema, Default, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum ProposalDecision {
    /// Campaign(s) created from this proposal.
    #[default]
    Accepted,
    /// Proposal failed validation (template not allowed, budget over clamp,
    /// unknown seed experiment) and was discarded.
    Rejected,
    /// Structural proposal waiting for the approval annotation.
    AwaitingApproval,
    /// Human approved a structural proposal via annotation.
    Approved,
    /// Human declined a structural proposal.
    Declined,
}

/// One proposer decision recorded in the drive's bounded history (cap 10;
/// the controller drops the oldest entry when full).
#[derive(Serialize, Deserialize, Debug, Clone, JsonSchema, Default)]
#[serde(rename_all = "camelCase")]
pub struct ProposalRecord {
    /// Stable id assigned by the controller (proposal-<n>). Humans approve
    /// structural proposals via annotation `research.nixlab.io/approve-proposal: "<id>"`.
    pub id: String,
    /// Proposer's one-paragraph hypothesis/rationale.
    pub summary: String,
    /// Decision state of this proposal.
    #[serde(default)]
    pub decision: ProposalDecision,
    /// Campaign names created from this proposal (empty for structural or
    /// rejected proposals).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub campaign_names: Vec<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, JsonSchema, Default)]
#[serde(rename_all = "camelCase")]
pub struct ResearchDriveStatus {
    /// Loop lifecycle phase.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase: Option<DrivePhase>,
    /// Campaigns the drive currently owns (live branches).
    ///
    /// Deliberately NOT `skip_serializing_if = "Vec::is_empty"`. The status is
    /// written with a MERGE patch, so an omitted field leaves the server's old
    /// value intact: skipping the empty vec meant the last branch could never
    /// be cleared, the folded campaign stayed "listed", and it was re-folded on
    /// every reconcile — inflating campaignsCompleted and stagnationCounter
    /// without bound (observed at 5.1M against a stagnation window of 3, which
    /// pins the drive permanently stagnant).
    #[serde(default)]
    pub current_campaigns: Vec<BranchRef>,
    /// Campaigns already folded into drive state, newest last (bounded ring).
    ///
    /// Belt-and-braces idempotency for the fold: membership in
    /// `currentCampaigns` alone made "fold exactly once" depend on a status
    /// write surviving, and when that write silently dropped the empty list the
    /// same campaign folded forever. An explicit ledger makes double-folding
    /// structurally impossible regardless of patch semantics.
    #[serde(default)]
    pub folded_campaigns: Vec<String>,
    /// Observed curriculum progress; present only when spec.curriculum is set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub curriculum: Option<CurriculumStatus>,
    /// Lifetime campaigns completed under this drive.
    #[serde(default)]
    pub campaigns_completed: u32,
    /// Best sigma-gated objective observed across ALL campaigns of this drive.
    /// Unlike campaign-level bestObjective this only moves on re-measured
    /// (unbiased) improvements, so it is honest progress evidence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub best_objective: Option<f64>,
    /// <experiment-name> holding the drive-level best objective.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub best_experiment_ref: Option<String>,
    /// Template that produced the current drive-level best. Sigma-gated
    /// comparison only happens WITHIN a template — objectives from different
    /// templates are not comparable, so a campaign fold against a different
    /// template is recorded but never displaces the incumbent (cross-template
    /// synthesis is the proposer's job, not a raw number comparison).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub best_template_ref: Option<String>,
    /// Consecutive completed campaigns without a sigma-gated improvement.
    /// Reset to 0 on any accepted improvement.
    #[serde(default)]
    pub stagnation_counter: u32,
    /// Bounded ring of proposer decisions (newest last, cap 10).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub proposals: Vec<ProposalRecord>,
    /// RFC 3339 timestamp of the last successful proposer call.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_proposal_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_generation: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub controller_version: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conditions: Vec<Condition>,
}
