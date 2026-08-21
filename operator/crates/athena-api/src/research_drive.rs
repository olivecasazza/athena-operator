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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub current_campaigns: Vec<BranchRef>,
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
