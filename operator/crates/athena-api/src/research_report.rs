use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// A scientist's curation of a ResearchCampaign's results into a research-paper
/// dataset. The **spec** is human/console-authored desired state (which
/// experiments are included, narrative overrides, seeded hypotheses); the
/// **status** is controller-owned (assembled dataset location, completeness).
///
/// The composed dossier is regenerable from spec at any time via
/// `athena dossier --report <name>` or the console BFF, so v1 needs no
/// reconciler — the durable product state is the curation spec.
#[derive(CustomResource, Serialize, Deserialize, Debug, Clone, JsonSchema)]
#[kube(
    group = "research.nixlab.io",
    version = "v1alpha1",
    kind = "ResearchReport",
    plural = "researchreports",
    singular = "researchreport",
    shortname = "rrp",
    namespaced,
    status = "ResearchReportStatus",
    printcolumn = r#"{"name":"Campaign","type":"string","jsonPath":".spec.campaignRef"}"#,
    printcolumn = r#"{"name":"Phase","type":"string","jsonPath":".status.phase"}"#,
    printcolumn = r#"{"name":"Age","type":"date","jsonPath":".metadata.creationTimestamp"}"#
)]
#[serde(rename_all = "camelCase")]
pub struct ResearchReportSpec {
    /// The ResearchCampaign whose experiments this report composes.
    pub campaign_ref: String,
    /// Paper title. Falls back to a generated title when unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Experiments to include by name. Empty = every experiment in the campaign
    /// (minus `excludedExperiments`); the curator UI lists them all and the
    /// scientist prunes failed/unwanted runs via `excludedExperiments`. No phase
    /// filter is applied — inclusion is an explicit curation decision.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub included_experiments: Vec<String>,
    /// Experiments to prune from the dataset by name (applied after inclusion).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub excluded_experiments: Vec<String>,
    /// Scientist-authored narrative, keyed by section name (e.g. `relatedWork`,
    /// `limitations`, `discussion`). Rendered as extra sections in the dossier.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub sections: BTreeMap<String, String>,
    /// Hypotheses the scientist wants recorded for follow-up — surfaced as a
    /// "Seeded Hypotheses / Future Work" section. (This does NOT launch
    /// experiments; it is report content.)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub seeded_hypotheses: Vec<String>,
    /// Structured external citations. Narrative sections reference them inline
    /// as `[@key]`; the dossier renders a References section in both formats,
    /// and the reconciler surfaces cited-but-undefined / defined-but-uncited
    /// keys as conditions. Verification of the claims themselves is a separate
    /// adversarial audit workload, never self-asserted here.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub references: Vec<Reference>,
}

#[derive(Serialize, Deserialize, Debug, Clone, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Reference {
    /// Citation key used inline as `[@key]` in section text.
    pub key: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub doi: Option<String>,
    /// The specific claim in this report the source is cited to support —
    /// the unit the adversarial audit verifies.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, JsonSchema, Default)]
#[serde(rename_all = "camelCase")]
pub struct ResearchReportStatus {
    /// Lifecycle phase, e.g. `Draft` / `Assembled`. Controller-owned (phase 2).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase: Option<String>,
    /// Location of the assembled dataset bundle once a reconciler persists it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dataset_uri: Option<String>,
    /// Number of experiments in the composed dataset after curation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub included_count: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_assembled_time: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_generation: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub controller_version: Option<String>,
    /// Bounded conditions (reuses the shared condition shape), e.g.
    /// `MissingProvenance`, `CampaignResolved`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conditions: Option<Vec<crate::experiment::ExperimentCondition>>,
}
