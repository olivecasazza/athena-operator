//! Reconcile a `ResearchDrive` — the PERPETUAL outer loop above campaigns.
//!
//! A `ResearchCampaign` is a bounded search: it stops at `budget.maxExperiments`
//! and never speaks again. The drive is the missing outer loop the human used
//! to play by hand: when an owned campaign finishes, the drive folds its result
//! into drive-level state, then asks an LLM (the proposer) what to run next —
//! reading hypotheses, decisions, objectives and sigma back into the prompt —
//! and creates the next campaign(s) itself. Campaigns form a DAG: the proposer
//! can FORK competing theories in parallel (up to `limits.maxActiveBranches`)
//! or CONSOLIDATE — seed a new campaign from the best experiment of a finished
//! branch (`spec.seedExperimentRef`) so knowledge converges instead of every
//! campaign cold-starting from template defaults.
//!
//! Each pass:
//!   1. List campaigns the drive owns (label + ownerReference).
//!   2. Fold newly-terminal campaigns: extract their honest objective
//!      (incumbentRemeasured preferred — bestObjective is maximization-biased),
//!      sigma-gate it against the drive best WITHIN the same template, update
//!      bestObjective / stagnationCounter / campaignsCompleted.
//!   3. If a structural proposal is AwaitingApproval, check the approval
//!      annotation; until it arrives the drive parks (RequireApproval) or
//!      proceeds (Auto).
//!   4. Stagnation: `stagnationCounter >= spec.stagnation.window` (or the
//!      lifetime campaign cap) → phase NeedsHuman + Ready=False condition +
//!      Event; the loop halts rather than burning budget on a flat landscape.
//!   5. If under the active-branch cap and not gated, call the proposer with
//!      the folded context, validate every action against the spec's bounds,
//!      and create the resulting campaigns (ownerRef + drive label).
//!   6. Write status (bounded proposal ring, phase, conditions) and metrics.
//!
//! Campaigns are created with an ownerReference to the drive (GC with it) and
//! a drive label (so this loop can find them). The drive NEVER writes campaign
//! or experiment status — controllers own status; the drive writes specs only.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use athena_api::common::{Condition, ConditionStatus};
use athena_api::experiment::Experiment;
use athena_api::experiment_template::ExperimentTemplate;
use athena_api::research_campaign::{
    CampaignBudget, ResearchCampaign, ResearchCampaignSpec, StrategySpec,
};
use athena_api::research_drive::{
    BranchRef, CurriculumSpec, CurriculumStatus, DrivePhase, PromotionQuantifier, ProposalDecision,
    ProposalRecord, ResearchDrive, ResearchDriveStatus, StageRecord, StructuralChangePolicy,
    TemplateProgress,
};
use chrono::Utc;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::OwnerReference;
use kube::api::{ListParams, ObjectMeta, Patch, PatchParams, PostParams};
use kube::runtime::controller::Action;
use kube::{Api, Resource, ResourceExt};
use serde_json::{Value, json};
use tracing::{info, warn};

use crate::Context;

const MANAGER: &str = "athena-drive";
/// Label marking campaigns a drive owns (queryable DAG edge).
const DRIVE_LABEL: &str = "athena.nixlab.io/drive";
/// Annotation a human sets to approve a structural proposal: value = proposal id.
const APPROVE_ANNOTATION: &str = "research.nixlab.io/approve-proposal";
/// Terminal campaign phases (mirror campaign_reconciler's phase strings).
const TERMINAL_PHASES: [&str; 2] = ["Completed", "CanaryFailed"];
/// Cap on status.proposals — the ring keeps the newest, drops the oldest.
const MAX_PROPOSAL_RECORDS: usize = 10;
/// How many completed campaigns' details go into the proposer prompt. Older
/// history is compressed to one-line summaries to bound prompt size.
const PROMPT_DETAIL_CAMPAIGNS: usize = 3;

/// Prior reports summarized into the proposer prompt. Small on purpose: this is
/// the loop's working memory, not its archive, and footgun text is verbose.
const PROMPT_MEMORY_REPORTS: usize = 6;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("kube error: {0}")]
    Kube(#[from] kube::Error),
    #[error("proposer call failed: {0}")]
    Proposer(String),
    #[error("proposer returned unparseable output: {0}")]
    ProposerOutput(String),
}

pub fn error_policy(drive: Arc<ResearchDrive>, err: &Error, _ctx: Arc<Context>) -> Action {
    warn!(drive = %drive.name_any(), %err, "drive reconcile error, retrying in 30s");
    Action::requeue(Duration::from_secs(30))
}

#[tracing::instrument(skip(drive, ctx), fields(
    drive.name = %drive.name_any(),
    drive.namespace = %drive.namespace().unwrap_or_default(),
))]
pub async fn reconcile(drive: Arc<ResearchDrive>, ctx: Arc<Context>) -> Result<Action, Error> {
    let name = drive.name_any();
    let ns = drive.namespace().unwrap_or_else(|| "default".to_string());
    let spec = &drive.spec;
    let mut status = drive.status.clone().unwrap_or_default();

    // 1. Campaigns this drive owns. Label selector is authoritative; the
    // ownerReference is for GC.
    let campaigns: Api<ResearchCampaign> = Api::namespaced(ctx.client.clone(), &ns);
    let lp = ListParams::default().labels(&format!("{DRIVE_LABEL}={name}"));
    let owned = campaigns.list(&lp).await?;

    // Partition: active (still generating/running) vs terminal-but-unfolded.
    // A campaign is "folded" once its objective has been merged into drive
    // state; status.currentCampaigns tracks the still-active branches.
    let mut active: Vec<&ResearchCampaign> = Vec::new();
    let mut terminal_unfolded: Vec<&ResearchCampaign> = Vec::new();
    for c in &owned.items {
        let phase = c.status.as_ref().and_then(|s| s.phase.clone());
        let is_terminal = phase
            .as_deref()
            .is_some_and(|p| TERMINAL_PHASES.contains(&p));
        if is_terminal {
            // Fold each campaign EXACTLY once, tracked by an explicit ledger.
            // The previous guard was membership in currentCampaigns, which made
            // once-only depend on a status write surviving; when the empty-list
            // write was dropped by merge-patch semantics the same campaign
            // re-folded on every reconcile and the counters ran away.
            if !status.folded_campaigns.iter().any(|n| n == &c.name_any()) {
                terminal_unfolded.push(c);
            }
        } else {
            active.push(c);
        }
    }

    // 2. Fold terminal campaigns into drive state.
    for c in &terminal_unfolded {
        fold_campaign(&mut status, c);
        status
            .current_campaigns
            .retain(|b| b.campaign != c.name_any());
        status.folded_campaigns.push(c.name_any());
        // Bounded status: keep the ledger to the most recent entries. A drive
        // never revisits a campaign this old, and unbounded status fields are
        // prohibited.
        const FOLDED_LEDGER_CAP: usize = 200;
        if status.folded_campaigns.len() > FOLDED_LEDGER_CAP {
            let excess = status.folded_campaigns.len() - FOLDED_LEDGER_CAP;
            status.folded_campaigns.drain(0..excess);
        }
        status.campaigns_completed = status.campaigns_completed.saturating_add(1);
        // Write the campaign up while folding it. The fold is the moment the
        // campaign's outcome becomes final, and it happens exactly once per
        // campaign (guarded by the folded ledger), so this is the natural
        // authoring point.
        //
        // A failure here must never block the fold: the drive's job is to keep
        // researching, and a missing write-up is a gap in the record, not a
        // reason to stall the loop. It is logged and left for the next pass —
        // authoring is create-if-absent, so a later retry still lands.
        match author_report(&spec.proposer, Some(&name), &ctx, &ns, c).await {
            Ok(true) => {
                crate::metrics::DRIVE_REPORTS_AUTHORED
                    .with_label_values(&[&ns, "created"])
                    .inc();
                info!(drive = %name, campaign = %c.name_any(), "authored research report")
            }
            Ok(false) => {}
            Err(e) => {
                crate::metrics::DRIVE_REPORTS_AUTHORED
                    .with_label_values(&[&ns, "error"])
                    .inc();
                warn!(
                    drive = %name,
                    campaign = %c.name_any(),
                    error = %e,
                    "research report authoring failed; campaign still folded"
                )
            }
        }
        info!(
            drive = %name,
            campaign = %c.name_any(),
            stagnation = status.stagnation_counter,
            "folded terminal campaign into drive state"
        );
    }

    // Refresh active branch list (drop campaigns that vanished, e.g. deleted).
    status.current_campaigns.retain(|b| {
        owned.items.iter().any(|c| c.name_any() == b.campaign)
            && active.iter().any(|c| c.name_any() == b.campaign)
    });

    // 2b. Curriculum promotion. Evaluated from campaign status after folds, so
    // a stage advances on observed results only.
    //
    // The transition is recorded in status.curriculum.stageHistory (entered_at
    // / promoted_at / best_experiment) rather than a Kubernetes Event: this
    // operator has no event recorder on Context and emits none anywhere, and
    // introducing one belongs in its own change rather than riding along here.
    if let Some(cur) = spec.curriculum.as_ref() {
        let now = chrono::Utc::now().to_rfc3339();
        if let Some(next) = evaluate_promotion(cur, &mut status, &owned.items, &now) {
            // A promotion is a CHANGE OF OBJECTIVE. Stagnation measured against
            // the previous stage's metric says nothing about the new one, and
            // carrying it forward parks the drive in needsHuman the moment the
            // wound-down old-stage campaigns fold without "improvement" -- they
            // were re-verifying a solved task, which is exactly what promotion
            // certifies. Observed live: stance -> locomotion promoted, four
            // wind-down folds, Stagnated/needsHuman before locomotion's first
            // result existed.
            status.stagnation_counter = 0;
            info!(drive = %name, stage = %next, "curriculum stage promoted; stagnation reset");
        }

        // Wind down branches left over from a PREVIOUS stage. Promotion only
        // changes what is PROPOSABLE; without this, in-flight campaigns from
        // the passed stage hold every branch slot until their budgets run out,
        // the proposer is never called (no free slot), and the promoted stage
        // sits unstarted while GPUs re-verify a solved task. Observed live:
        // stance promoted at 03:10, four stance campaigns mid-budget held all
        // four slots, lastProposalAt aged 13 hours, zero locomotion campaigns.
        //
        // Pinching spec.budget.maxExperiments to the campaign's current total
        // is the gentlest stop: nothing is deleted, running experiments finish
        // and keep their measurements, and the campaign completes at its next
        // reconcile — which folds it, writes its report, and frees the slot.
        if let Some(stage) = status
            .curriculum
            .as_ref()
            .and_then(|c| c.current_stage.as_deref())
        {
            let in_stage: std::collections::HashSet<&str> = cur
                .stages
                .iter()
                .find(|s| s.name == stage)
                .map(|s| s.template_refs.iter().map(String::as_str).collect())
                .unwrap_or_default();
            for c in &owned.items {
                let cs = c.status.clone().unwrap_or_default();
                let terminal = cs
                    .phase
                    .as_deref()
                    .is_some_and(|p| TERMINAL_PHASES.contains(&p));
                if terminal || in_stage.contains(c.spec.template_ref.as_str()) {
                    continue;
                }
                let total = cs.total_experiments;
                if c.spec.budget.max_experiments <= total {
                    continue; // already winding down
                }
                let api: Api<ResearchCampaign> = Api::namespaced(ctx.client.clone(), &ns);
                let patch = json!({
                    "metadata": { "annotations": { "research.nixlab.io/wound-down":
                        format!("out-of-stage after promotion to {stage}; budget pinched to {total} so the slot frees without discarding running work") } },
                    "spec": { "budget": { "maxExperiments": total } }
                });
                match api
                    .patch(
                        &c.name_any(),
                        &PatchParams::apply(MANAGER),
                        &Patch::Merge(&patch),
                    )
                    .await
                {
                    Ok(_) => {
                        info!(drive = %name, campaign = %c.name_any(), stage = %stage, "wound down out-of-stage campaign")
                    }
                    Err(e) => {
                        warn!(drive = %name, campaign = %c.name_any(), %e, "wind-down patch failed")
                    }
                }
            }
        }
    }

    // 3. Structural approval gate. AwaitingApproval proposals block the loop
    // under RequireApproval; the approve annotation flips them to Approved,
    // which unblocks the NEXT pass (this pass records the flip).
    let mut awaiting: Option<String> = None;
    for p in &mut status.proposals {
        if p.decision == ProposalDecision::AwaitingApproval {
            let approved = drive
                .metadata
                .annotations
                .as_ref()
                .and_then(|a| a.get(APPROVE_ANNOTATION))
                .is_some_and(|v| v == &p.id);
            if approved {
                p.decision = ProposalDecision::Approved;
                info!(drive = %name, proposal = %p.id, "structural proposal approved by annotation");
            } else {
                awaiting = Some(p.id.clone());
            }
        }
    }

    // 4. Phase decision.
    let at_campaign_cap = status.campaigns_completed >= spec.limits.max_campaigns;
    let stagnated = status.stagnation_counter >= spec.stagnation.window;
    let (phase, requeue_secs, condition) = if spec.paused {
        (
            DrivePhase::Paused,
            300,
            Some(cond(
                "Ready",
                ConditionStatus::False,
                "Paused",
                "spec.paused=true",
            )),
        )
    } else if awaiting.is_some()
        && spec.structural_change_policy == StructuralChangePolicy::RequireApproval
    {
        (
            DrivePhase::AwaitingApproval,
            60,
            Some(cond(
                "Ready",
                ConditionStatus::False,
                "AwaitingApproval",
                &format!(
                    "structural proposal {} awaits annotation {}",
                    awaiting.as_deref().unwrap_or(""),
                    APPROVE_ANNOTATION
                ),
            )),
        )
    } else if at_campaign_cap || stagnated {
        let reason = if at_campaign_cap {
            "CampaignCap"
        } else {
            "Stagnated"
        };
        let msg = if at_campaign_cap {
            format!(
                "lifetime campaign cap reached ({}/{})",
                status.campaigns_completed, spec.limits.max_campaigns
            )
        } else {
            format!(
                "no sigma-gated improvement in {} consecutive campaigns (window {})",
                status.stagnation_counter, spec.stagnation.window
            )
        };
        (
            DrivePhase::NeedsHuman,
            300,
            Some(cond("Ready", ConditionStatus::False, reason, &msg)),
        )
    } else if active.len() < spec.limits.max_active_branches as usize {
        // A free branch slot is enough to propose: branches are concurrent by
        // design (the DAG diverges across hardware pools), so the loop must
        // not wait for all active campaigns to fold before filling the next
        // slot. The proposer sees the in-flight campaigns in its context and
        // is told not to duplicate them.
        (DrivePhase::Proposing, 15, None)
    } else {
        (DrivePhase::CampaignRunning, 60, None)
    };

    // 5. Propose + create when a slot is free. The phase gate above normally
    // guarantees a slot (current_campaigns is pruned to active campaigns), but
    // enforce it here too so the proposer is never called at full capacity.
    let mut created_this_pass: Vec<String> = Vec::new();
    let has_free_slot = (status.current_campaigns.len() as u32) < spec.limits.max_active_branches;
    if phase == DrivePhase::Proposing && has_free_slot {
        match propose_and_create(&drive, &ctx, &ns, &name, &owned.items, &status).await {
            Ok((branches, record)) => {
                crate::metrics::DRIVE_PROPOSER_CALLS
                    .with_label_values(&[&ns, &spec.domain, "ok"])
                    .inc();
                created_this_pass = branches.iter().map(|b| b.campaign.clone()).collect();
                status.current_campaigns.extend(branches);
                push_proposal(&mut status, record);
                status.last_proposal_at = Some(Utc::now().to_rfc3339());
            }
            Err(e) => {
                // A proposer failure is NOT terminal: park in Proposing and
                // retry with backoff. Only spec/validation bugs (deterministic)
                // deserve the error surface, and those show up as Rejected
                // proposals instead.
                crate::metrics::DRIVE_PROPOSER_CALLS
                    .with_label_values(&[&ns, &spec.domain, "error"])
                    .inc();
                warn!(drive = %name, %e, "proposer call failed; will retry");
                status.conditions = vec![
                    cond(
                        "Ready",
                        ConditionStatus::False,
                        "ProposerError",
                        &e.to_string(),
                    ),
                    // The loop cannot start new work without the proposer, so
                    // this is not "researching" however healthy the pod looks.
                    cond(
                        "Progressing",
                        ConditionStatus::False,
                        "ProposerUnreachable",
                        &e.to_string(),
                    ),
                ];
                write_status(&ctx, &ns, &name, &drive, status, DrivePhase::Proposing).await?;
                return Ok(Action::requeue(Duration::from_secs(120)));
            }
        }
    }

    // 6. Status + metrics.
    let final_phase = if !created_this_pass.is_empty() {
        DrivePhase::CampaignRunning
    } else {
        phase.clone()
    };
    // The drive's own health, recomputed every pass. A drive that has stopped
    // doing research is the failure this system is most likely to suffer and
    // least likely to notice: it keeps reconciling, its pod is Ready, and
    // nothing is red. These conditions are what make that visible to `kubectl`
    // and the console without a human reading operator logs.
    let mut conditions = vec![match &condition {
        Some(c) => c.clone(),
        None => cond(
            "Ready",
            ConditionStatus::True,
            "LoopActive",
            "perpetual loop running",
        ),
    }];

    // Progressing: is the loop actually researching RIGHT NOW? Parked phases
    // are legitimate states, not errors — but they must not read as healthy,
    // because a drive can sit in one indefinitely while GPUs idle.
    conditions.push(match final_phase {
        DrivePhase::AwaitingApproval => cond(
            "Progressing",
            ConditionStatus::False,
            "AwaitingApproval",
            "parked on a structural proposal; no new campaigns until a human decides",
        ),
        DrivePhase::NeedsHuman => cond(
            "Progressing",
            ConditionStatus::False,
            "NeedsHuman",
            "stagnation window exhausted; the drive stopped proposing on purpose",
        ),
        DrivePhase::Paused => cond(
            "Progressing",
            ConditionStatus::False,
            "Paused",
            "spec.paused is set",
        ),
        _ if status.current_campaigns.is_empty() => cond(
            "Progressing",
            ConditionStatus::False,
            "NoActiveBranches",
            "no campaigns in flight",
        ),
        _ => cond(
            "Progressing",
            ConditionStatus::True,
            "Researching",
            &format!("{} branch(es) in flight", status.current_campaigns.len()),
        ),
    });

    // MemoryHealthy: derived from OBSERVED state rather than a remembered
    // flag — a folded campaign with no ResearchReport means a finding was lost,
    // whatever the reason. Authoring failures are deliberately non-fatal to the
    // fold, so without this check they are invisible.
    let reports: Api<athena_api::research_report::ResearchReport> =
        Api::namespaced(ctx.client.clone(), &ns);
    if let Ok(existing) = reports.list(&ListParams::default()).await {
        let have: std::collections::HashSet<String> = existing
            .items
            .iter()
            .map(|r| r.spec.campaign_ref.clone())
            .collect();
        let missing: Vec<&String> = status
            .folded_campaigns
            .iter()
            .filter(|c| !have.contains(*c))
            .collect();
        conditions.push(if missing.is_empty() {
            cond(
                "MemoryHealthy",
                ConditionStatus::True,
                "AllCampaignsWritten",
                "every folded campaign has a research report",
            )
        } else {
            cond(
                "MemoryHealthy",
                ConditionStatus::False,
                "ReportsMissing",
                &format!(
                    "{} folded campaign(s) have no research report, e.g. {}",
                    missing.len(),
                    missing[0]
                ),
            )
        });
    }

    // Structural proposals the controller CANNOT apply — harness, rigging or
    // sim-design changes. Under `Auto` these are recorded and the loop keeps
    // researching, which is what stops a single unaddressed note from halting
    // the fleet. The risk is the opposite failure, and it is not theoretical:
    // this drive ran 8+ experiments across 4 campaigns that COULD NOT succeed
    // while the real blocker (a destructive action frame) sat recorded and
    // unread. So the backlog is surfaced loudly rather than left to whoever
    // thinks to page through status.proposals.
    let pending_structural: Vec<&ProposalRecord> = status
        .proposals
        .iter()
        .filter(|p| p.decision == ProposalDecision::AwaitingApproval)
        .collect();
    if !pending_structural.is_empty() {
        let newest = pending_structural[pending_structural.len() - 1];
        conditions.push(cond(
            "StructuralProposalPending",
            ConditionStatus::True,
            "AwaitingHuman",
            &format!(
                "{} structural proposal(s) recorded and unaddressed; newest {}: {}",
                pending_structural.len(),
                newest.id,
                newest.summary.chars().take(160).collect::<String>()
            ),
        ));
    }
    status.conditions = conditions;
    crate::metrics::DRIVE_PHASE
        .with_label_values(&[&ns, &spec.domain, &phase_label(&final_phase)])
        .set(1.0);
    crate::metrics::DRIVE_CAMPAIGNS_TOTAL
        .with_label_values(&[&ns, &spec.domain, &phase_label(&final_phase)])
        .set(status.campaigns_completed as f64);

    // Per-stage curriculum series. Emitted for EVERY declared stage each pass,
    // not only the current one, so a promotion reads as one series dropping to
    // 0 while the next rises to 1 — a stale 1 left on the old stage would make
    // the dashboard show two live stages forever.
    if let (Some(cur), Some(cstatus)) = (spec.curriculum.as_ref(), status.curriculum.as_ref()) {
        let current = cstatus.current_stage.clone().unwrap_or_default();
        for stage in &cur.stages {
            crate::metrics::DRIVE_CURRICULUM_STAGE
                .with_label_values(&[&ns, &spec.domain, &stage.name])
                .set(if stage.name == current { 1.0 } else { 0.0 });
            let rec = cstatus.stage_history.iter().find(|r| r.name == stage.name);
            crate::metrics::DRIVE_CURRICULUM_STAGE_EXPERIMENTS
                .with_label_values(&[&ns, &spec.domain, &stage.name])
                .set(rec.map(|r| r.succeeded_experiments).unwrap_or(0) as f64);

            // Per-template rows, for every DECLARED template rather than only
            // those with a status row, so a line that has produced nothing is
            // visibly not-passed instead of missing from the dashboard.
            for t in &stage.template_refs {
                let row =
                    rec.and_then(|r| r.template_progress.iter().find(|p| &p.template_ref == t));
                crate::metrics::DRIVE_CURRICULUM_TEMPLATE_PASSED
                    .with_label_values(&[&ns, &spec.domain, &stage.name, t])
                    .set(if row.is_some_and(|p| p.passed) {
                        1.0
                    } else {
                        0.0
                    });
                // Skipped entirely when unmeasured: see the gauge's doc comment.
                if let Some(obj) = row.and_then(|p| p.best_objective) {
                    crate::metrics::DRIVE_CURRICULUM_TEMPLATE_OBJECTIVE
                        .with_label_values(&[&ns, &spec.domain, &stage.name, t])
                        .set(obj);
                }
            }
        }
    }
    write_status(&ctx, &ns, &name, &drive, status, final_phase).await?;

    Ok(Action::requeue(Duration::from_secs(requeue_secs)))
}

/// Merge one terminal campaign's outcome into drive state.
///
/// The honest score is `incumbentRemeasured` (unbiased fresh-seed re-measure);
/// `bestObjective` is the max over N noisy draws and is only the fallback.
/// Improvement is judged WITHIN the campaign's template only, sigma-gated by
/// the campaign's own measured seed noise — a candidate must beat the drive
/// best by more than one sigma to reset the stagnation counter. Cross-template
/// campaigns never displace the incumbent (incomparable units); the proposer
/// sees both and may synthesize.
fn fold_campaign(status: &mut ResearchDriveStatus, campaign: &ResearchCampaign) {
    let cs = campaign.status.clone().unwrap_or_default();
    let score = cs.incumbent_remeasured.or(cs.best_objective);
    let template = campaign.spec.template_ref.clone();
    let best_exp = cs.best_experiment.clone();

    let improved = match (score, &status.best_objective, &status.best_template_ref) {
        (Some(s), Some(best), Some(bt)) if bt == &template => {
            let sigma = cs.seed_noise_sigma.unwrap_or(0.0).max(0.0);
            s > best + sigma
        }
        // No incumbent yet (first fold) or different template: first fold of a
        // template seeds the comparison; different templates never displace.
        (Some(_), None, _) => true,
        (Some(_), Some(_), None) => true,
        _ => false,
    };

    if improved {
        if let (Some(s), Some(exp)) = (score, best_exp) {
            status.best_objective = Some(s);
            status.best_experiment_ref = Some(exp);
            status.best_template_ref = Some(template);
        }
        status.stagnation_counter = 0;
    } else {
        status.stagnation_counter = status.stagnation_counter.saturating_add(1);
    }
}

/// Templates proposable right now: the current curriculum stage's list
/// intersected with the drive allowlist, or the whole allowlist when no
/// curriculum is configured.
///
/// This is the enforcement point that turns stage ORDER from proposer prose
/// into an invariant: a locomotion template simply is not offerable while
/// stance is current, and `build_campaign` rejects it if proposed anyway.
pub(crate) fn allowed_templates(drive: &ResearchDrive) -> Vec<String> {
    let all = &drive.spec.template_refs;
    let Some(cur) = drive.spec.curriculum.as_ref() else {
        return all.clone();
    };
    let stage_name = drive
        .status
        .as_ref()
        .and_then(|s| s.curriculum.as_ref())
        .and_then(|c| c.current_stage.clone())
        .or_else(|| cur.stages.first().map(|s| s.name.clone()));
    let Some(stage) = cur
        .stages
        .iter()
        .find(|s| Some(&s.name) == stage_name.as_ref())
    else {
        return all.clone();
    };
    all.iter()
        .filter(|t| stage.template_refs.iter().any(|s| s == *t))
        .cloned()
        .collect()
}

/// The experiment that seeds a stage's campaigns: the winner recorded for the
/// stage named by `seedFrom`. None when the stage has no `seedFrom` or the
/// referenced stage has not produced a winner yet.
pub(crate) fn stage_seed(drive: &ResearchDrive, template_ref: &str) -> Option<String> {
    let cur = drive.spec.curriculum.as_ref()?;
    let stage = cur
        .stages
        .iter()
        .find(|s| s.template_refs.iter().any(|t| t == template_ref))?;
    let from = stage.seed_from.as_ref()?;
    drive
        .status
        .as_ref()?
        .curriculum
        .as_ref()?
        .stage_history
        .iter()
        .find(|r| &r.name == from)
        .and_then(|r| r.best_experiment.clone())
}

/// Advance the curriculum when the current stage's promotion criteria are met.
///
/// Evaluated purely from campaign status (never client input), and only for
/// campaigns whose templateRef belongs to the current stage. Returns the new
/// stage name when a promotion happened, so the caller can emit an Event.
pub(crate) fn evaluate_promotion(
    spec_curriculum: &CurriculumSpec,
    status: &mut ResearchDriveStatus,
    owned: &[ResearchCampaign],
    now: &str,
) -> Option<String> {
    if spec_curriculum.stages.is_empty() {
        return None;
    }
    let cs = status
        .curriculum
        .get_or_insert_with(CurriculumStatus::default);
    let current = cs
        .current_stage
        .clone()
        .unwrap_or_else(|| spec_curriculum.stages[0].name.clone());
    cs.current_stage = Some(current.clone());
    if !cs.stage_history.iter().any(|r| r.name == current) {
        cs.stage_history.push(StageRecord {
            name: current.clone(),
            entered_at: Some(now.to_string()),
            ..Default::default()
        });
    }

    let idx = spec_curriculum
        .stages
        .iter()
        .position(|s| s.name == current)?;
    let stage = &spec_curriculum.stages[idx];

    // Per-template evidence, one row per DECLARED template in declared order —
    // including templates with no campaigns yet, which is what lets the `All`
    // quantifier block on a line that has produced nothing. Scores use the same
    // honest fold fold_campaign prefers: the unbiased re-measure when present.
    //
    // This is the only place the fold happens; both the status rows and the
    // promotion gate read it, so the dashboard can never disagree with the
    // decision that was actually made.
    let promo = stage.promotion.as_ref();
    let mut rows: Vec<TemplateProgress> = Vec::with_capacity(stage.template_refs.len());
    for t in &stage.template_refs {
        let mut row = TemplateProgress {
            template_ref: t.clone(),
            ..Default::default()
        };
        for c in owned.iter().filter(|c| &c.spec.template_ref == t) {
            let st = c.status.clone().unwrap_or_default();
            row.succeeded_experiments = row
                .succeeded_experiments
                .saturating_add(st.succeeded_experiments);
            if let (Some(score), Some(exp)) = (
                st.incumbent_remeasured.or(st.best_objective),
                st.best_experiment.clone(),
            ) {
                if row.best_objective.is_none_or(|b| score > b) {
                    row.best_objective = Some(score);
                    row.best_experiment = Some(exp);
                }
            }
        }
        // No promotion block means nothing to satisfy, so `passed` stays false
        // even for a strong line: the stage is a deliberate human gate.
        row.passed = promo.is_some_and(|p| {
            row.best_objective.is_some_and(|b| b >= p.threshold)
                && row.succeeded_experiments >= p.min_experiments
        });
        rows.push(row);
    }

    // Stage-level aggregates stay exactly as before: the console reads them,
    // and `best_experiment` is what `stage_seed` hands to the next stage.
    let succeeded: u32 = rows
        .iter()
        .fold(0u32, |a, r| a.saturating_add(r.succeeded_experiments));
    let best: Option<(f64, String)> = rows
        .iter()
        .filter_map(|r| r.best_objective.zip(r.best_experiment.clone()))
        .fold(None, |acc: Option<(f64, String)>, (score, exp)| match acc {
            Some((b, _)) if b >= score => acc,
            _ => Some((score, exp)),
        });

    if let Some(rec) = cs.stage_history.iter_mut().find(|r| r.name == current) {
        rec.succeeded_experiments = succeeded;
        if let Some((score, exp)) = &best {
            rec.best_objective = Some(*score);
            rec.best_experiment = Some(exp.clone());
        }
        rec.template_progress = rows.clone();
    }

    // No promotion block = deliberate human gate; stay put.
    let promo = promo?;
    match promo.quantifier {
        // Templates are alternative routes to one goal: the best result decides.
        PromotionQuantifier::Any => {
            let (score, _) = best.as_ref()?;
            if succeeded < promo.min_experiments || *score < promo.threshold {
                return None;
            }
        }
        // Templates are independent research lines: each must pass on its own
        // evidence. An empty template list is NOT vacuously true — promoting a
        // stage that gates nothing would defeat the point of declaring it.
        PromotionQuantifier::All => {
            if rows.is_empty() || !rows.iter().all(|r| r.passed) {
                return None;
            }
        }
    }
    let next = spec_curriculum.stages.get(idx + 1)?.name.clone();
    if let Some(rec) = cs.stage_history.iter_mut().find(|r| r.name == current) {
        rec.promoted_at = Some(now.to_string());
    }
    cs.current_stage = Some(next.clone());
    cs.stage_history.push(StageRecord {
        name: next.clone(),
        entered_at: Some(now.to_string()),
        ..Default::default()
    });
    Some(next)
}

/// One OpenAI-compatible chat completion against the drive's proposer, returning
/// the assistant text with markdown fences stripped.
///
/// Extracted so the drive's TWO uses share one transport: proposing the next
/// campaign, and writing up a finished one. They must agree on auth, SSE
/// folding and fence handling — a second hand-rolled copy is how the write-up
/// path silently rots while the proposing path is exercised every reconcile.
pub(crate) async fn chat_completion(
    proposer: &athena_api::research_drive::ProposerSpec,
    ctx: &Arc<Context>,
    ns: &str,
    system: &str,
    user: &str,
) -> Result<String, Error> {
    let timeout = Duration::from_secs(proposer.timeout_seconds.unwrap_or(120).max(5) as u64);
    // NOTE: reqwest is built without the `json` feature — the body is
    // serialized manually. rustls-tls IS enabled so the proposer endpoint may
    // be HTTPS (external OpenAI-compatible providers) or plain HTTP
    // (in-cluster mesh-llm / vLLM Service).
    let payload = json!({
        "model": proposer.model,
        "messages": [
            {"role": "system", "content": system},
            {"role": "user", "content": user},
        ],
        "max_tokens": proposer.max_tokens.unwrap_or(4096),
        "temperature": proposer.temperature.unwrap_or(0.7),
        // Explicit non-streaming request; some gateways ignore it (the SSE
        // folding below covers that), but well-behaved ones honor it.
        "stream": false,
    });
    let mut req = reqwest::Client::builder()
        .timeout(timeout)
        .build()
        .map_err(|e| Error::Proposer(e.to_string()))?
        .post(format!(
            "{}/chat/completions",
            proposer.endpoint.trim_end_matches('/')
        ))
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(payload.to_string());
    if let Some(key_ref) = &proposer.api_key_secret_ref {
        let secrets: Api<k8s_openapi::api::core::v1::Secret> =
            Api::namespaced(ctx.client.clone(), ns);
        let secret = secrets.get(&key_ref.name).await?;
        let key = secret
            .data
            .as_ref()
            .and_then(|d| d.get(&key_ref.key))
            .map(|b| String::from_utf8_lossy(&b.0).to_string())
            .ok_or_else(|| {
                Error::Proposer(format!(
                    "secret {}/{} missing key {}",
                    key_ref.name, ns, key_ref.key
                ))
            })?;
        req = req.bearer_auth(key);
    }
    let resp = req
        .send()
        .await
        .map_err(|e| Error::Proposer(e.to_string()))?;
    if !resp.status().is_success() {
        return Err(Error::Proposer(format!("HTTP {}", resp.status())));
    }
    let text = resp
        .text()
        .await
        .map_err(|e| Error::ProposerOutput(e.to_string()))?;
    // Some gateways (OmniRoute combos) answer with an SSE stream even when
    // `stream` is unset. Detect the `data:` framing and fold the chunks into
    // one completion; otherwise parse the body as a single JSON object.
    let body: Value = if text.trim_start().starts_with("data:") {
        let mut content = String::new();
        let mut last: Option<Value> = None;
        for line in text.lines() {
            let line = line.trim();
            let Some(data) = line.strip_prefix("data:") else {
                continue;
            };
            let data = data.trim();
            if data == "[DONE]" || data.is_empty() {
                continue;
            }
            if let Ok(chunk) = serde_json::from_str::<Value>(data) {
                if let Some(delta) = chunk
                    .pointer("/choices/0/delta/content")
                    .and_then(Value::as_str)
                {
                    content.push_str(delta);
                }
                last = Some(chunk);
            }
        }
        // Synthesize the non-streaming shape the rest of the parser expects.
        json!({ "choices": [{ "message": { "content": content } }], "_streamed": last })
    } else {
        serde_json::from_str(&text).map_err(|e| Error::ProposerOutput(e.to_string()))?
    };
    let content = body
        .pointer("/choices/0/message/content")
        .and_then(Value::as_str)
        .ok_or_else(|| Error::ProposerOutput("no choices[0].message.content".into()))?;

    // Tolerate markdown fences around the JSON.
    Ok(content
        .trim()
        .trim_start_matches("```json")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim()
        .to_string())
}

/// Prior findings this drive has already published, newest first, as compact
/// context for the proposer.
///
/// Without this the loop is amnesiac: it re-proposes campaigns down avenues a
/// previous report already recorded as dead ends, and the Footgun sections
/// exist for humans only. Only `title` and `Footguns` are lifted — the whole
/// dossier would swamp the prompt, and footguns are the part that changes what
/// to try next.
async fn recent_findings(ctx: &Arc<Context>, ns: &str, limit: usize) -> Vec<Value> {
    let api: Api<athena_api::research_report::ResearchReport> =
        Api::namespaced(ctx.client.clone(), ns);
    let Ok(list) = api.list(&ListParams::default()).await else {
        // Memory is an enhancement to the prompt, never a precondition for
        // proposing: a read failure must not stall the research loop.
        return Vec::new();
    };
    let mut items = list.items;
    items.sort_by(|a, b| {
        b.metadata
            .creation_timestamp
            .cmp(&a.metadata.creation_timestamp)
    });
    items
        .iter()
        .take(limit)
        .map(|r| {
            json!({
                "campaign": r.spec.campaign_ref,
                "title": r.spec.title,
                "footguns": r.spec.sections.get("Footguns"),
                // The loop's own future-work channel. Report authoring demands
                // testable seededHypotheses; feeding them back is what turns
                // memory into self-direction -- without this the hypotheses
                // were written and never read by anything.
                "seededHypotheses": r.spec.seeded_hypotheses,
            })
        })
        .collect()
}

/// Author a `ResearchReport` for a campaign the drive just folded.
///
/// This is the half of the loop that was missing. `report_reconciler` assembles
/// a dossier from a report's spec, but nothing ever CREATED a report, so the
/// narrative record — hypothesis, analysis, conclusions, footguns — only
/// existed when a human hand-wrote the spec. An autonomous platform whose
/// memory depends on someone remembering to write it down does not have
/// memory.
///
/// Create-if-absent, never update: once the object exists, its spec is
/// scientist-authored curation (see `report_reconciler`) and the controller
/// must not overwrite a human's edits with a fresh generation.
pub(crate) async fn author_report(
    proposer: &athena_api::research_drive::ProposerSpec,
    drive_name: Option<&str>,
    ctx: &Arc<Context>,
    ns: &str,
    campaign: &ResearchCampaign,
) -> Result<bool, Error> {
    let name = dns_name(&campaign.name_any());
    let api: Api<athena_api::research_report::ResearchReport> =
        Api::namespaced(ctx.client.clone(), ns);
    if api.get_opt(&name).await?.is_some() {
        return Ok(false);
    }

    // Every experiment's hypothesis and measured metrics: the raw material a
    // write-up needs. Sourced from controller-written status only — a report
    // must never restate a number the workload claimed about itself.
    //
    // Filtered on spec.campaignRef rather than a label: controller-created
    // experiments carry the campaign label, but hand-authored ones need not,
    // and a write-up that silently omits the human-added arms of a campaign is
    // worse than none.
    let exps: Api<Experiment> = Api::namespaced(ctx.client.clone(), ns);
    let runs: Vec<Value> = exps
        .list(&ListParams::default())
        .await?
        .items
        .iter()
        .filter(|e| e.spec.campaign_ref == campaign.name_any())
        .map(|e| {
            let st = e.status.clone().unwrap_or_default();
            json!({
                "experiment": e.name_any(),
                "hypothesis": e.spec.hypothesis,
                "parameters": e.spec.parameters,
                "phase": st.phase,
                "metrics": st.metrics,
            })
        })
        .collect();
    let cs = campaign.status.clone().unwrap_or_default();
    let context = json!({
        "campaign": campaign.name_any(),
        "templateRef": campaign.spec.template_ref,
        "bestObjective": cs.best_objective,
        "incumbentRemeasured": cs.incumbent_remeasured,
        "bestExperiment": cs.best_experiment,
        "experiments": runs,
        "priorFindings": recent_findings(ctx, ns, PROMPT_MEMORY_REPORTS).await,
    });

    let system = "You are the research scientist for an autonomous RL platform, writing up a \
        FINISHED campaign so a future agent can reuse it. Reply with STRICT JSON only: \
        {\"title\": string, \"sections\": {\"Findings\": string, \"Method\": string, \
        \"Footguns\": string, \"Limitations\": string}, \"seededHypotheses\": [string]}. \
        Rules: cite real numbers inline from the supplied metrics — a claim without a number \
        is not a finding. Record NEGATIVE results and refuted hypotheses explicitly; they are \
        the highest-value content, because they stop the next agent repeating the work. \
        Footguns must name a symptom AND the tell that identifies it, so it is recognizable \
        next time. Limitations must state what this campaign does NOT establish. Invent \
        nothing: if a metric is absent, say so rather than estimating it. seededHypotheses \
        are testable claims, not tasks.";
    let user =
        serde_json::to_string_pretty(&context).map_err(|e| Error::ProposerOutput(e.to_string()))?;
    let cleaned = chat_completion(proposer, ctx, ns, system, &user).await?;
    let out: Value =
        serde_json::from_str(&cleaned).map_err(|e| Error::ProposerOutput(e.to_string()))?;

    let mut sections: std::collections::BTreeMap<String, String> = Default::default();
    if let Some(map) = out.get("sections").and_then(Value::as_object) {
        for (k, v) in map {
            if let Some(s) = v.as_str() {
                sections.insert(k.clone(), s.to_string());
            }
        }
    }
    // A write-up with no sections is not a record; refuse rather than create an
    // empty object that looks like memory and holds none.
    if sections.is_empty() {
        return Err(Error::ProposerOutput(
            "report author returned no sections".into(),
        ));
    }
    let seeded: Vec<String> = out
        .get("seededHypotheses")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();

    let report = athena_api::research_report::ResearchReport {
        metadata: ObjectMeta {
            name: Some(name),
            namespace: Some(ns.to_string()),
            labels: Some(
                [
                    drive_name.map(|d| (DRIVE_LABEL.to_string(), d.to_string())),
                    Some(("athena.nixlab.io/campaign".to_string(), campaign.name_any())),
                ]
                .into_iter()
                .flatten()
                .collect(),
            ),
            // Owned by the campaign it describes: the report cannot assemble
            // without resolving `campaignRef`, so outliving its subject would
            // leave a permanently broken record rather than durable memory.
            owner_references: Some(vec![OwnerReference {
                api_version: "research.nixlab.io/v1alpha1".into(),
                kind: "ResearchCampaign".into(),
                name: campaign.name_any(),
                uid: campaign.uid().unwrap_or_default(),
                controller: Some(false),
                block_owner_deletion: Some(false),
            }]),
            ..Default::default()
        },
        spec: athena_api::research_report::ResearchReportSpec {
            campaign_ref: campaign.name_any(),
            title: out.get("title").and_then(Value::as_str).map(str::to_string),
            sections,
            seeded_hypotheses: seeded,
            // Curation is a human act: the controller composes the whole
            // campaign and leaves pruning, scoping and citations to whoever
            // edits the spec afterwards.
            included_experiments: Vec::new(),
            excluded_experiments: Vec::new(),
            about: None,
            references: Vec::new(),
        },
        status: None,
    };
    api.create(&PostParams::default(), &report).await?;
    Ok(true)
}

/// Call the LLM proposer, validate its actions, create the campaigns.
/// Returns the created branches plus the proposal record for the ring.
async fn propose_and_create(
    drive: &ResearchDrive,
    ctx: &Arc<Context>,
    ns: &str,
    name: &str,
    owned: &[ResearchCampaign],
    status: &ResearchDriveStatus,
) -> Result<(Vec<BranchRef>, ProposalRecord), Error> {
    let spec = &drive.spec;
    let proposal_id = format!(
        "proposal-{}",
        status.proposals.len() + 1 + status.campaigns_completed as usize
    );

    // ---- Build the context the proposer reasons over. ----
    let experiments: Api<Experiment> = Api::namespaced(ctx.client.clone(), ns);
    let mut campaign_summaries: Vec<Value> = Vec::new();
    let detail_from = owned.len().saturating_sub(PROMPT_DETAIL_CAMPAIGNS);
    for (i, c) in owned.iter().enumerate() {
        let cs = c.status.clone().unwrap_or_default();
        let mut summary = json!({
            "campaign": c.name_any(),
            "templateRef": c.spec.template_ref,
            "phase": cs.phase,
            "bestObjective": cs.best_objective,
            "incumbentRemeasured": cs.incumbent_remeasured,
            "seedNoiseSigma": cs.seed_noise_sigma,
            "totalExperiments": cs.total_experiments,
            "succeededExperiments": cs.succeeded_experiments,
            "failedExperiments": cs.failed_experiments,
        });
        // Detail only the most recent campaigns: full experiment-level
        // hypotheses/decisions. Older ones stay one-line (bounded prompt).
        if i >= detail_from {
            let lp = ListParams::default()
                .labels(&format!("athena.nixlab.io/campaign={}", c.name_any()));
            if let Ok(exps) = experiments.list(&lp).await {
                let details: Vec<Value> = exps
                    .items
                    .iter()
                    .map(|e| {
                        json!({
                            "name": e.name_any(),
                            "phase": e.status.as_ref().map(|s| format!("{:?}", s.phase)),
                            "decision": e.status.as_ref().and_then(|s| s.decision.clone()),
                            "hypothesis": e.spec.hypothesis,
                            "parameters": e.spec.parameters,
                        })
                    })
                    .collect();
                summary["experiments"] = json!(details);
            }
        }
        campaign_summaries.push(summary);
    }

    let context = json!({
        "domain": spec.domain,
        "allowedTemplates": allowed_templates(drive),
        "curriculumStage": status
            .curriculum
            .as_ref()
            .and_then(|c| c.current_stage.clone()),
        "driveBest": {
            "objective": status.best_objective,
            "experiment": status.best_experiment_ref,
            "templateRef": status.best_template_ref,
        },
        "stagnationCounter": status.stagnation_counter,
        "campaignsCompleted": status.campaigns_completed,
        "inFlightBranches": status.current_campaigns.iter().map(|b| json!({
            "branch": b.name, "campaign": b.campaign, "templateRef": b.template_ref,
        })).collect::<Vec<_>>(),
        "freeBranchSlots": spec.limits.max_active_branches.saturating_sub(status.current_campaigns.len() as u32),
        "recentProposals": status.proposals.iter().map(|p| json!({
            "id": p.id, "summary": p.summary, "decision": format!("{:?}", p.decision),
        })).collect::<Vec<_>>(),
        "campaigns": campaign_summaries,
        // The loop's own memory. Without this the proposer re-derives dead
        // ends every cycle: past reports name the avenues already refuted and
        // the footguns that made them look promising.
        "priorFindings": recent_findings(ctx, ns, PROMPT_MEMORY_REPORTS).await,
    });

    // ---- Call the proposer (OpenAI-compatible chat completions). ----
    let system = "You are the research proposer for an autonomous RL platform. \
        Given campaign results (hypotheses, decisions, objectives, seed-noise sigma), \
        propose the next experiment campaign(s). Reply with STRICT JSON only: \
        {\"summary\": string, \"actions\": [ ... ]}. Each action is one of: \
        {\"type\":\"fork\",\"branch\":string,\"templateRef\":string,\"strategy\":\"pbt\"|\"heuristic\",\
        \"budget\":{\"maxExperiments\":int,\"maxDuration\":string},\"seedExperimentRef\":string|null,\
        \"hypothesis\":string} — start a new campaign branch; \
        {\"type\":\"consolidate\", ...same fields...} — like fork but the branch MERGES prior \
        branches (set seedExperimentRef to the winning experiment to carry knowledge); \
        {\"type\":\"structural\",\"title\":string,\"rationale\":string} — a harness/rigging/\
        sim-design change the controller cannot apply; it is recorded for human review. \
        Rules: templateRef MUST be one of allowedTemplates. Do NOT duplicate a branch that is \
        already in flight (see inFlightBranches) — propose only for freeBranchSlots. Prefer \
        consolidate when a branch's incumbent clearly won; prefer fork when theories diverge. \
        seedExperimentRef must be an experiment NAME from the context, or null. \
        priorFindings holds this drive's OWN published reports: treat their footguns as \
        established, do not re-propose an avenue a report already refuted, and say which \
        finding you are building on when one applies. Their seededHypotheses are candidate \
        directions -- prefer testing a seeded hypothesis over re-running a solved task at a \
        new difficulty. CAPABILITY GAPS: when the current stage's lines are passing, spend \
        one action per cycle asking what a DEPLOYABLE robot needs that NO stage trains -- \
        righting itself after a fall, recovering from pushes, traversing steps, carrying \
        load, hunting and evading -- and emit a structural action titled 'newBehavior: \
        <name>' with a falsifiable hypothesis, env requirements, and the held-out promotion \
        metric it would gate on. Tuning a solved task is not research; a new behavior is.";
    let user =
        serde_json::to_string_pretty(&context).map_err(|e| Error::ProposerOutput(e.to_string()))?;

    let cleaned = chat_completion(&spec.proposer, ctx, ns, system, &user).await?;
    let proposal: Value =
        serde_json::from_str(&cleaned).map_err(|e| Error::ProposerOutput(e.to_string()))?;

    // ---- Validate + execute actions. ----
    let summary = proposal
        .get("summary")
        .and_then(Value::as_str)
        .unwrap_or("(no summary)")
        .to_string();
    let actions = proposal
        .get("actions")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    let mut branches: Vec<BranchRef> = Vec::new();
    let mut record = ProposalRecord {
        id: proposal_id,
        summary: summary.clone(),
        decision: ProposalDecision::Accepted,
        campaign_names: Vec::new(),
    };
    let campaigns: Api<ResearchCampaign> = Api::namespaced(ctx.client.clone(), ns);
    let templates: Api<ExperimentTemplate> = Api::namespaced(ctx.client.clone(), ns);
    let free_slots = spec
        .limits
        .max_active_branches
        .saturating_sub(status.current_campaigns.len() as u32);

    // Process up to 4 actions per pass (bounds LLM output), but only CAMPAIGN
    // actions consume branch slots — structural proposals are slot-free.
    // A fork/consolidate with no free slot is deferred, never created above
    // maxActiveBranches.
    for action in actions.into_iter().take(4) {
        let action_type = action.get("type").and_then(Value::as_str).unwrap_or("");
        match action_type {
            "structural" => {
                record.decision = match spec.structural_change_policy {
                    StructuralChangePolicy::RequireApproval => ProposalDecision::AwaitingApproval,
                    StructuralChangePolicy::Auto => ProposalDecision::Approved,
                };
                record.summary = format!(
                    "{} [structural: {} — {}]",
                    summary,
                    action.get("title").and_then(Value::as_str).unwrap_or(""),
                    action
                        .get("rationale")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                );
            }
            "fork" | "consolidate" => {
                if branches.len() as u32 >= free_slots {
                    info!(drive = %name, action = action_type,
                        "no free branch slot; deferring proposed campaign to a later pass");
                    continue;
                }
                match build_campaign(
                    drive,
                    name,
                    ns,
                    &action,
                    action_type,
                    owned,
                    &templates,
                    &experiments,
                )
                .await
                {
                    Ok((campaign, branch)) => {
                        let cname = campaign.name_any();
                        match campaigns.create(&PostParams::default(), &campaign).await {
                            Ok(_) => {
                                info!(drive = %name, campaign = %cname, action = action_type,
                                    "drive created campaign");
                                record.campaign_names.push(cname);
                                branches.push(branch);
                            }
                            Err(kube::Error::Api(e)) if e.code == 409 => {}
                            Err(e) => return Err(Error::Kube(e)),
                        }
                    }
                    Err(reason) => {
                        warn!(drive = %name, %reason, "proposed campaign rejected by validation");
                        record.decision = ProposalDecision::Rejected;
                        record.summary = format!("{summary} [rejected: {reason}]");
                    }
                }
            }
            other => {
                warn!(drive = %name, action = %other, "unknown proposer action, skipping");
            }
        }
    }

    if branches.is_empty() && record.decision == ProposalDecision::Accepted {
        // Proposer produced only structural or no actionable campaigns.
        record.decision =
            if record.campaign_names.is_empty() && record.decision == ProposalDecision::Accepted {
                ProposalDecision::Rejected
            } else {
                record.decision
            };
    }
    Ok((branches, record))
}

/// Build (not create) a ResearchCampaign from a validated proposer action.
/// Every spec bound from the drive's limits is enforced HERE — the proposer is
/// untrusted input.
async fn build_campaign(
    drive: &ResearchDrive,
    drive_name: &str,
    ns: &str,
    action: &Value,
    action_type: &str,
    owned: &[ResearchCampaign],
    templates: &Api<ExperimentTemplate>,
    experiments: &Api<Experiment>,
) -> Result<(ResearchCampaign, BranchRef), String> {
    let spec = &drive.spec;
    let template_ref = action
        .get("templateRef")
        .and_then(Value::as_str)
        .ok_or("action missing templateRef")?;
    // Stage-gated: while a curriculum is configured this is the CURRENT
    // stage's templates, so proposing a later stage is rejected here rather
    // than trusted to the proposer's prompt.
    let allowed = allowed_templates(drive);
    if !allowed.iter().any(|t| t == template_ref) {
        return Err(format!(
            "templateRef {template_ref} not proposable now (allowed: {allowed:?})"
        ));
    }
    templates
        .get_opt(template_ref)
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("template {template_ref} does not exist in {ns}"))?;

    let branch_name = action
        .get("branch")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .unwrap_or("branch");
    let campaign_name = dns_name(&format!("{drive_name}-{branch_name}"));

    let strategy = action
        .get("strategy")
        .and_then(Value::as_str)
        .unwrap_or("pbt")
        .to_string();
    if strategy != "pbt" && strategy != "heuristic" {
        return Err(format!("strategy {strategy} must be pbt|heuristic"));
    }

    // Budget: clamp to the drive's per-campaign ceiling, whatever was asked.
    let cap = &spec.limits.campaign_budget;
    let max_experiments = action
        .pointer("/budget/maxExperiments")
        .and_then(Value::as_u64)
        .map(|n| (n as u32).clamp(1, cap.max_experiments))
        .unwrap_or(cap.max_experiments);
    let max_duration = action
        .pointer("/budget/maxDuration")
        .and_then(Value::as_str)
        .unwrap_or(&cap.max_duration)
        .to_string();

    // Seed: fork may reference any prior experiment; consolidate SHOULD.
    // Validate the referenced experiment exists; consolidate with no seed
    // falls back to the drive best, fork with none cold-starts (allowed).
    let seed = match action.get("seedExperimentRef").and_then(Value::as_str) {
        Some(seed) if !seed.is_empty() => {
            if experiments
                .get_opt(seed)
                .await
                .map_err(|e| e.to_string())?
                .is_none()
            {
                return Err(format!("seedExperimentRef {seed} does not exist in {ns}"));
            }
            Some(seed.to_string())
        }
        // A curriculum stage with `seedFrom` always seeds from the named
        // stage's winner: that reference carries WEIGHTS across the stage
        // boundary (seedExperimentRef -> ATHENA_RESUME_FROM), which is the
        // entire reason for training in an order.
        _ => stage_seed(drive, template_ref).or_else(|| {
            if action_type == "consolidate" {
                drive
                    .status
                    .as_ref()
                    .and_then(|s| s.best_experiment_ref.clone())
            } else {
                None
            }
        }),
    };

    let hypothesis = action
        .get("hypothesis")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let forked_from = owned
        .iter()
        .max_by_key(|c| c.meta().creation_timestamp.clone())
        .map(|c| c.name_any());

    let owner = OwnerReference {
        api_version: "research.nixlab.io/v1alpha1".into(),
        kind: "ResearchDrive".into(),
        name: drive_name.to_string(),
        uid: drive.metadata.uid.clone().unwrap_or_default(),
        ..Default::default()
    };
    let campaign = ResearchCampaign {
        metadata: ObjectMeta {
            name: Some(campaign_name.clone()),
            namespace: Some(ns.to_string()),
            labels: Some(BTreeMap::from([
                (DRIVE_LABEL.to_string(), drive_name.to_string()),
                (
                    "athena.nixlab.io/branch".to_string(),
                    branch_name.to_string(),
                ),
            ])),
            annotations: Some(BTreeMap::from([(
                "athena.nixlab.io/hypothesis".to_string(),
                hypothesis,
            )])),
            owner_references: Some(vec![owner]),
            ..Default::default()
        },
        spec: ResearchCampaignSpec {
            template_ref: template_ref.to_string(),
            concurrency: 1,
            budget: CampaignBudget {
                max_experiments,
                max_duration,
            },
            strategy: StrategySpec {
                strategy_type: strategy,
            },
            benchmark_suite_ref: None,
            benchmark_runtime_profile_ref: None,
            population_size: None,
            perturb_factor: None,
            inference_mesh: None,
            inference_cluster: None,
            canary: None,
            seed_experiment_ref: seed,
            // Deliberately None: the DRIVE writes up the campaigns it owns,
            // when it folds them. Setting it here would author twice.
            proposer: None,
        },
        status: None,
    };
    let branch = BranchRef {
        name: branch_name.to_string(),
        campaign: campaign_name,
        template_ref: template_ref.to_string(),
        forked_from,
    };
    Ok((campaign, branch))
}

/// Kubernetes DNS-1123 name: lowercase alnum + '-', max 63 chars, no leading/
/// trailing dash. Branch names come from the LLM, so sanitize hard.
fn dns_name(raw: &str) -> String {
    let mut out: String = raw
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    while out.contains("--") {
        out = out.replace("--", "-");
    }
    let out = out.trim_matches('-');
    out.chars()
        .take(63)
        .collect::<String>()
        .trim_matches('-')
        .to_string()
}

fn push_proposal(status: &mut ResearchDriveStatus, record: ProposalRecord) {
    status.proposals.push(record);
    if status.proposals.len() > MAX_PROPOSAL_RECORDS {
        let overflow = status.proposals.len() - MAX_PROPOSAL_RECORDS;
        status.proposals.drain(0..overflow);
    }
}

fn cond(t: &str, status: ConditionStatus, reason: &str, message: &str) -> Condition {
    Condition {
        condition_type: t.to_string(),
        status,
        reason: Some(reason.to_string()),
        message: Some(message.to_string()),
        last_transition_time: Some(Utc::now().to_rfc3339()),
    }
}

fn phase_label(phase: &DrivePhase) -> String {
    format!("{phase:?}")
}

async fn write_status(
    ctx: &Arc<Context>,
    ns: &str,
    name: &str,
    drive: &ResearchDrive,
    mut status: ResearchDriveStatus,
    phase: DrivePhase,
) -> Result<(), Error> {
    let drives: Api<ResearchDrive> = Api::namespaced(ctx.client.clone(), ns);
    status.phase = Some(phase);
    status.observed_generation = drive.metadata.generation;
    status.controller_version = Some(env!("CARGO_PKG_VERSION").to_string());
    let patch = json!({ "status": status });
    drives
        .patch_status(name, &PatchParams::apply(MANAGER), &Patch::Merge(&patch))
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use athena_api::research_campaign::ResearchCampaignStatus;

    fn campaign_with(
        template: &str,
        remeasured: Option<f64>,
        best: Option<f64>,
        sigma: Option<f64>,
    ) -> ResearchCampaign {
        ResearchCampaign {
            metadata: ObjectMeta {
                name: Some("c".into()),
                namespace: Some("default".into()),
                ..Default::default()
            },
            spec: ResearchCampaignSpec {
                template_ref: template.into(),
                concurrency: 1,
                budget: CampaignBudget::default(),
                strategy: StrategySpec {
                    strategy_type: "pbt".into(),
                },
                benchmark_suite_ref: None,
                benchmark_runtime_profile_ref: None,
                population_size: None,
                perturb_factor: None,
                inference_mesh: None,
                inference_cluster: None,
                canary: None,
                seed_experiment_ref: None,
                proposer: None,
            },
            status: Some(ResearchCampaignStatus {
                best_experiment: Some("c-003".into()),
                best_objective: best,
                incumbent_remeasured: remeasured,
                seed_noise_sigma: sigma,
                ..Default::default()
            }),
        }
    }

    fn drive_spec_with(
        curriculum: CurriculumSpec,
    ) -> athena_api::research_drive::ResearchDriveSpec {
        use athena_api::research_drive::{
            DriveLimits, ProposerSpec, ResearchDriveSpec, SecretKeyRef, StagnationSpec,
        };
        ResearchDriveSpec {
            domain: "test".into(),
            template_refs: vec!["t-stance".into(), "t-loco".into()],
            proposer: ProposerSpec {
                endpoint: "http://localhost".into(),
                model: "m".into(),
                api_key_secret_ref: Some(SecretKeyRef {
                    name: "s".into(),
                    key: "k".into(),
                }),
                max_tokens: Some(128),
                temperature: Some(0.0),
                timeout_seconds: Some(5),
            },
            limits: DriveLimits::default(),
            stagnation: StagnationSpec::default(),
            structural_change_policy: StructuralChangePolicy::default(),
            paused: false,
            curriculum: Some(curriculum),
        }
    }

    fn staged_campaign(
        name: &str,
        template: &str,
        best: Option<f64>,
        succeeded: u32,
    ) -> ResearchCampaign {
        let mut c = campaign_with(template, None, best, None);
        c.metadata.name = Some(name.into());
        if let Some(st) = c.status.as_mut() {
            st.succeeded_experiments = succeeded;
            st.best_experiment = Some(format!("{name}-000"));
        }
        c
    }

    fn curriculum_two_stage() -> CurriculumSpec {
        use athena_api::research_drive::{CurriculumStage, PromotionSpec};
        CurriculumSpec {
            stages: vec![
                CurriculumStage {
                    name: "stance".into(),
                    template_refs: vec!["t-stance".into()],
                    seed_from: None,
                    promotion: Some(PromotionSpec {
                        metric: "eval_upright_frac".into(),
                        threshold: 0.8,
                        min_experiments: 2,
                        // Explicit: this test is the regression guard proving
                        // the default best-of path still behaves as it did.
                        quantifier: PromotionQuantifier::Any,
                    }),
                },
                CurriculumStage {
                    name: "locomotion".into(),
                    template_refs: vec!["t-loco".into()],
                    seed_from: Some("stance".into()),
                    promotion: None,
                },
            ],
        }
    }

    /// Two INDEPENDENT research lines in one stage — the multi-morphology
    /// shape that made best-of gating wrong.
    fn curriculum_two_lines(quantifier: PromotionQuantifier) -> CurriculumSpec {
        use athena_api::research_drive::{CurriculumStage, PromotionSpec};
        CurriculumSpec {
            stages: vec![
                CurriculumStage {
                    name: "stance".into(),
                    template_refs: vec!["t-snake".into(), "t-spot".into()],
                    seed_from: None,
                    promotion: Some(PromotionSpec {
                        metric: "eval_stance_score".into(),
                        threshold: 0.6,
                        min_experiments: 2,
                        quantifier,
                    }),
                },
                CurriculumStage {
                    name: "locomotion".into(),
                    template_refs: vec!["t-loco".into()],
                    seed_from: Some("stance".into()),
                    promotion: None,
                },
            ],
        }
    }

    #[test]
    fn promotion_all_blocks_until_every_template_passes() {
        let cur = curriculum_two_lines(PromotionQuantifier::All);
        let mut status = ResearchDriveStatus::default();

        // Snake standing perfectly cannot carry spot, which falls every
        // episode. This is the live incident: stance promoted on snake's 1.000
        // while spot sat at 0.000.
        let owned = vec![
            staged_campaign("snake", "t-snake", Some(1.0), 4),
            staged_campaign("spot", "t-spot", Some(0.0), 4),
        ];
        assert_eq!(evaluate_promotion(&cur, &mut status, &owned, "t0"), None);

        // Once the lagging line clears the bar on its own evidence, advance.
        let owned = vec![
            staged_campaign("snake", "t-snake", Some(1.0), 4),
            staged_campaign("spot", "t-spot", Some(0.7), 4),
        ];
        assert_eq!(
            evaluate_promotion(&cur, &mut status, &owned, "t1").as_deref(),
            Some("locomotion")
        );
    }

    #[test]
    fn promotion_all_blocks_template_with_no_campaigns() {
        let cur = curriculum_two_lines(PromotionQuantifier::All);
        let mut status = ResearchDriveStatus::default();
        // A line that has produced NOTHING is not a pass; absence of evidence
        // must not read as evidence.
        let owned = vec![staged_campaign("snake", "t-snake", Some(1.0), 4)];
        assert_eq!(evaluate_promotion(&cur, &mut status, &owned, "t0"), None);
    }

    #[test]
    fn promotion_all_counts_min_experiments_per_template() {
        let cur = curriculum_two_lines(PromotionQuantifier::All);
        let mut status = ResearchDriveStatus::default();
        // Both lines above threshold, but spot has one run. Stage-wide the
        // count is 5 and would pass; per template it must not.
        let owned = vec![
            staged_campaign("snake", "t-snake", Some(1.0), 4),
            staged_campaign("spot", "t-spot", Some(0.9), 1),
        ];
        assert_eq!(evaluate_promotion(&cur, &mut status, &owned, "t0"), None);
    }

    #[test]
    fn promotion_any_preserves_best_of_behavior() {
        let cur = curriculum_two_lines(PromotionQuantifier::Any);
        let mut status = ResearchDriveStatus::default();
        // Same lopsided evidence that `All` rejects: `Any` must still promote,
        // because templates there are alternative routes to one goal.
        let owned = vec![
            staged_campaign("snake", "t-snake", Some(1.0), 4),
            staged_campaign("spot", "t-spot", Some(0.0), 4),
        ];
        assert_eq!(
            evaluate_promotion(&cur, &mut status, &owned, "t0").as_deref(),
            Some("locomotion")
        );
    }

    #[test]
    fn template_progress_is_populated_for_lagging_lines() {
        let cur = curriculum_two_lines(PromotionQuantifier::All);
        let mut status = ResearchDriveStatus::default();
        let owned = vec![staged_campaign("snake", "t-snake", Some(1.0), 4)];
        evaluate_promotion(&cur, &mut status, &owned, "t0");

        let cs = status.curriculum.clone().unwrap();
        let stance = cs
            .stage_history
            .iter()
            .find(|r| r.name == "stance")
            .unwrap();
        // One row per DECLARED template, in declared order, so "which line is
        // blocking promotion" is answerable from status alone.
        assert_eq!(stance.template_progress.len(), 2);
        assert_eq!(stance.template_progress[0].template_ref, "t-snake");
        assert!(stance.template_progress[0].passed);
        assert_eq!(stance.template_progress[1].template_ref, "t-spot");
        assert!(!stance.template_progress[1].passed);
        assert_eq!(stance.template_progress[1].best_objective, None);
        assert_eq!(stance.template_progress[1].succeeded_experiments, 0);
    }

    #[test]
    fn promotion_requires_threshold_and_min_experiments() {
        let cur = curriculum_two_stage();
        let mut status = ResearchDriveStatus::default();

        // Above threshold but only one succeeded run: a single lucky result
        // must not advance the curriculum.
        let owned = vec![staged_campaign("c1", "t-stance", Some(0.95), 1)];
        assert_eq!(evaluate_promotion(&cur, &mut status, &owned, "t0"), None);
        let cs = status.curriculum.clone().unwrap();
        assert_eq!(cs.current_stage.as_deref(), Some("stance"));

        // Enough runs but below threshold: still no promotion.
        let owned = vec![staged_campaign("c1", "t-stance", Some(0.5), 4)];
        assert_eq!(evaluate_promotion(&cur, &mut status, &owned, "t1"), None);

        // Both satisfied: advance, and record the winner that seeds the next
        // stage.
        let owned = vec![staged_campaign("c1", "t-stance", Some(0.9), 3)];
        assert_eq!(
            evaluate_promotion(&cur, &mut status, &owned, "t2").as_deref(),
            Some("locomotion")
        );
        let cs = status.curriculum.clone().unwrap();
        assert_eq!(cs.current_stage.as_deref(), Some("locomotion"));
        let stance = cs
            .stage_history
            .iter()
            .find(|r| r.name == "stance")
            .unwrap();
        assert_eq!(stance.promoted_at.as_deref(), Some("t2"));
        assert_eq!(stance.best_experiment.as_deref(), Some("c1-000"));
    }

    #[test]
    fn allowed_templates_gate_to_current_stage() {
        let mut drive = ResearchDrive::new(
            "d",
            crate::drive_reconciler::tests::drive_spec_with(curriculum_two_stage()),
        );
        drive.metadata.namespace = Some("default".into());

        // No status yet -> first stage is current.
        assert_eq!(allowed_templates(&drive), vec!["t-stance".to_string()]);

        // After promotion only the later stage's template is proposable, so a
        // proposer cannot skip ahead OR fall back.
        drive.status = Some(ResearchDriveStatus {
            curriculum: Some(CurriculumStatus {
                current_stage: Some("locomotion".into()),
                stage_history: vec![StageRecord {
                    name: "stance".into(),
                    best_experiment: Some("c1-000".into()),
                    ..Default::default()
                }],
            }),
            ..Default::default()
        });
        assert_eq!(allowed_templates(&drive), vec!["t-loco".to_string()]);

        // The later stage seeds from the earlier stage's winner: this is what
        // carries weights across the boundary.
        assert_eq!(stage_seed(&drive, "t-loco").as_deref(), Some("c1-000"));
        assert_eq!(stage_seed(&drive, "t-stance"), None);
    }

    #[test]
    fn no_curriculum_leaves_allowlist_untouched() {
        let mut spec = drive_spec_with(CurriculumSpec::default());
        spec.curriculum = None;
        let drive = ResearchDrive::new("d", spec);
        assert_eq!(
            allowed_templates(&drive),
            vec!["t-stance".to_string(), "t-loco".to_string()]
        );
    }

    #[test]
    fn fold_first_campaign_seeds_drive_best() {
        let mut status = ResearchDriveStatus::default();
        fold_campaign(
            &mut status,
            &campaign_with("t", Some(10.0), Some(12.0), None),
        );
        assert_eq!(status.best_objective, Some(10.0)); // remeasured, not biased best
        assert_eq!(status.best_experiment_ref.as_deref(), Some("c-003"));
        assert_eq!(status.best_template_ref.as_deref(), Some("t"));
        assert_eq!(status.stagnation_counter, 0);
    }

    #[test]
    fn fold_requires_sigma_margin_to_displace() {
        let mut status = ResearchDriveStatus {
            best_objective: Some(10.0),
            best_template_ref: Some("t".into()),
            ..Default::default()
        };
        // Within one sigma (10.5 < 10.0 + 1.0): no improvement, stagnation +1.
        fold_campaign(
            &mut status,
            &campaign_with("t", Some(10.5), None, Some(1.0)),
        );
        assert_eq!(status.best_objective, Some(10.0));
        assert_eq!(status.stagnation_counter, 1);
        // Beyond one sigma (11.5 > 10.0 + 1.0): displaces, counter resets.
        fold_campaign(
            &mut status,
            &campaign_with("t", Some(11.5), None, Some(1.0)),
        );
        assert_eq!(status.best_objective, Some(11.5));
        assert_eq!(status.stagnation_counter, 0);
    }

    #[test]
    fn fold_cross_template_never_displaces() {
        let mut status = ResearchDriveStatus {
            best_objective: Some(10.0),
            best_template_ref: Some("t".into()),
            ..Default::default()
        };
        // A different template with a wildly higher score is incomparable:
        // it must NOT displace the incumbent, and counts as stagnation.
        fold_campaign(
            &mut status,
            &campaign_with("other", Some(999.0), None, None),
        );
        assert_eq!(status.best_objective, Some(10.0));
        assert_eq!(status.best_template_ref.as_deref(), Some("t"));
        assert_eq!(status.stagnation_counter, 1);
    }

    #[test]
    fn proposal_ring_is_bounded() {
        let mut status = ResearchDriveStatus::default();
        for i in 0..15 {
            push_proposal(
                &mut status,
                ProposalRecord {
                    id: format!("proposal-{i}"),
                    ..Default::default()
                },
            );
        }
        assert_eq!(status.proposals.len(), MAX_PROPOSAL_RECORDS);
        assert_eq!(status.proposals[0].id, "proposal-5"); // oldest dropped
        assert_eq!(status.proposals[9].id, "proposal-14");
    }

    #[test]
    fn dns_name_sanitizes_llm_output() {
        assert_eq!(dns_name("Gait Frequency!"), "gait-frequency");
        assert_eq!(dns_name("--weird__name--"), "weird-name");
        let long = dns_name(&"x".repeat(200));
        assert!(long.len() <= 63);
        assert!(!long.ends_with('-'));
    }
}
