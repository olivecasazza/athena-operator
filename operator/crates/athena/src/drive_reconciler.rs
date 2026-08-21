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
    BranchRef, DrivePhase, ProposalDecision, ProposalRecord, ResearchDrive, ResearchDriveStatus,
    StructuralChangePolicy,
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
            // Fold only campaigns not already folded: membership in
            // currentCampaigns marks "known active"; a terminal campaign still
            // listed there hasn't been folded yet.
            let listed = status
                .current_campaigns
                .iter()
                .any(|b| b.campaign == c.name_any());
            if listed {
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
        status.campaigns_completed = status.campaigns_completed.saturating_add(1);
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
    } else if !active.is_empty() {
        (DrivePhase::CampaignRunning, 60, None)
    } else {
        // Slots free and nothing running: propose.
        (DrivePhase::Proposing, 15, None)
    };

    // 5. Propose + create when a slot is free. The phase gate above normally
    // guarantees a slot (current_campaigns is pruned to active campaigns), but
    // enforce it here too so the proposer is never called at full capacity.
    let mut created_this_pass: Vec<String> = Vec::new();
    let has_free_slot = (status.current_campaigns.len() as u32) < spec.limits.max_active_branches;
    if phase == DrivePhase::Proposing && has_free_slot {
        match propose_and_create(&drive, &ctx, &ns, &name, &owned.items, &status).await {
            Ok((branches, record)) => {
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
                warn!(drive = %name, %e, "proposer call failed; will retry");
                status.conditions = vec![cond(
                    "Ready",
                    ConditionStatus::False,
                    "ProposerError",
                    &e.to_string(),
                )];
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
    let conditions = match condition {
        Some(c) => vec![c],
        None => vec![cond(
            "Ready",
            ConditionStatus::True,
            "LoopActive",
            "perpetual loop running",
        )],
    };
    status.conditions = conditions;
    crate::metrics::DRIVE_CAMPAIGNS_TOTAL
        .with_label_values(&[&ns, &spec.domain, &phase_label(&final_phase)])
        .set(status.campaigns_completed as f64);
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
        "allowedTemplates": spec.template_refs,
        "driveBest": {
            "objective": status.best_objective,
            "experiment": status.best_experiment_ref,
            "templateRef": status.best_template_ref,
        },
        "stagnationCounter": status.stagnation_counter,
        "campaignsCompleted": status.campaigns_completed,
        "recentProposals": status.proposals.iter().map(|p| json!({
            "id": p.id, "summary": p.summary, "decision": format!("{:?}", p.decision),
        })).collect::<Vec<_>>(),
        "campaigns": campaign_summaries,
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
        Rules: templateRef MUST be one of allowedTemplates. Propose 1-2 campaigns max. \
        Prefer consolidate when a branch's incumbent clearly won; prefer fork when theories \
        diverge. seedExperimentRef must be an experiment NAME from the context, or null.";
    let user =
        serde_json::to_string_pretty(&context).map_err(|e| Error::ProposerOutput(e.to_string()))?;

    let timeout = Duration::from_secs(spec.proposer.timeout_seconds.unwrap_or(120).max(5) as u64);
    // NOTE: reqwest is built without the `json` feature — the body is
    // serialized manually. rustls-tls IS enabled so the proposer endpoint may
    // be HTTPS (external OpenAI-compatible providers) or plain HTTP
    // (in-cluster mesh-llm / vLLM Service).
    let payload = json!({
        "model": spec.proposer.model,
        "messages": [
            {"role": "system", "content": system},
            {"role": "user", "content": user},
        ],
        "max_tokens": spec.proposer.max_tokens.unwrap_or(4096),
        "temperature": spec.proposer.temperature.unwrap_or(0.7),
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
            spec.proposer.endpoint.trim_end_matches('/')
        ))
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(payload.to_string());
    if let Some(key_ref) = &spec.proposer.api_key_secret_ref {
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
    let resp = req.send().await.map_err(|e| Error::Proposer(e.to_string()))?;
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
                if let Some(delta) =
                    chunk.pointer("/choices/0/delta/content").and_then(Value::as_str)
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
    let cleaned = content
        .trim()
        .trim_start_matches("```json")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim();
    let proposal: Value =
        serde_json::from_str(cleaned).map_err(|e| Error::ProposerOutput(e.to_string()))?;

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
    if !spec.template_refs.iter().any(|t| t == template_ref) {
        return Err(format!(
            "templateRef {template_ref} not in drive templateRefs"
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
        _ if action_type == "consolidate" => drive
            .status
            .as_ref()
            .and_then(|s| s.best_experiment_ref.clone()),
        _ => None,
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
