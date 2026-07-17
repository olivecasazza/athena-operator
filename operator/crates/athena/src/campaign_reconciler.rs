//! Reconcile a `ResearchCampaign` — the autonomous Auto-RL loop.
//!
//! Each pass:
//!   1. Resolve the campaign's `ExperimentTemplate` (objective + parameter space).
//!   2. List the campaign's `Experiment`s, partition by phase.
//!   3. Evaluate succeeded experiments against the objective, pick the best, and
//!      stamp each one's `status.decision` (Keep on the best, Discard otherwise).
//!      The experiment reconciler owns phase/metrics; the campaign owns decision.
//!   3d. Canary gate (spec.canary): before any budgeted experiment exists, the
//!       campaign creates exactly ONE cheap probe (`<campaign>-canary`) and
//!       holds ALL further generation until it Succeeds and (when a benchmark
//!       suite gates it) its BenchmarkRun verdict is Keep. Failed/Discarded
//!       canary → status.phase = CanaryFailed and nothing more is generated.
//!   4. If under `budget.maxExperiments` and below the concurrency target,
//!      generate the next experiment(s) via the strategy:
//!        - "heuristic": hill-climb from the best (baseline from template
//!          defaults, then perturb one numeric parameter per child).
//!        - "pbt": population-based training. Each child warm-starts weights
//!          from the best succeeded experiment's `status.latestCheckpoint`
//!          (`spec.checkpointPolicy.resumeFrom`), inherits the best's
//!          hyperparameters, and explores by perturbing every numeric param by
//!          `spec.perturbFactor` (up or 1/factor down). Concurrency target is
//!          `spec.populationSize` when set, else `spec.concurrency`.
//!   5. Update campaign status (counts, bestExperiment, bestObjective, phase).
//!
//! Experiments are created with an ownerReference to the campaign (so they are
//! garbage-collected with it) and a campaign label (so this loop can find them).

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use athena_api::benchmark_run::{
    BenchmarkRun, BenchmarkRunOutput, BenchmarkRunSpec, PromotionPolicy,
};
use athena_api::common::{LocalObjectReference, TypedObjectReference};
use athena_api::defaults::deep_merge;
use athena_api::experiment::{
    CheckpointPolicy, Experiment, ExperimentDecision, ExperimentPhase, ExperimentSpec,
};
use athena_api::experiment_template::{ExperimentTemplate, ObjectiveGoal, ObjectiveSpec};
use athena_api::research_campaign::{
    CanarySpec, InferenceMeshSpec, ResearchCampaign, ResearchCampaignSpec, VllmClusterSpec,
};
use athena_api::runtime_profile::EnvVar;
use k8s_openapi::api::apps::v1::{Deployment, DeploymentSpec};
use k8s_openapi::api::core::v1::{
    Container, ContainerPort, HTTPGetAction, PodSpec, PodTemplateSpec, Probe, ResourceRequirements,
    Service, ServicePort, ServiceSpec, Toleration,
};
use k8s_openapi::apimachinery::pkg::api::resource::Quantity;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::{LabelSelector, OwnerReference};
use k8s_openapi::apimachinery::pkg::util::intstr::IntOrString;
use kube::ResourceExt;
use kube::api::{
    Api, ApiResource, DeleteParams, DynamicObject, ListParams, ObjectMeta, Patch, PatchParams,
    PostParams,
};
use kube::core::GroupVersionKind;
use kube::runtime::controller::Action;
use serde_json::{Value, json};
use tracing::{info, warn};

use crate::Context;

const MANAGER: &str = "athena-campaign";
const CAMPAIGN_LABEL: &str = "athena.nixlab.io/campaign";
/// Label marking a campaign's canary gate experiment.
const CANARY_LABEL: &str = "athena.nixlab.io/canary";
/// Multiplicative step for the hill-climb perturbation of a numeric parameter.
const STEP: f64 = 0.5;
/// Default PBT perturbation factor: explore at 1.2x up / ~0.83x (1/1.2) down.
const PBT_FACTOR: f64 = 1.2;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("kube error: {0}")]
    Kube(#[from] kube::Error),
    #[error("campaign {0} references missing ExperimentTemplate {1}")]
    MissingTemplate(String, String),
}

pub fn error_policy(campaign: Arc<ResearchCampaign>, err: &Error, _ctx: Arc<Context>) -> Action {
    warn!(campaign = %campaign.name_any(), %err, "campaign reconcile error, retrying in 30s");
    Action::requeue(Duration::from_secs(30))
}

#[tracing::instrument(skip(campaign, ctx), fields(
    campaign.name = %campaign.name_any(),
    campaign.namespace = %campaign.namespace().unwrap_or_default(),
))]
pub async fn reconcile(
    campaign: Arc<ResearchCampaign>,
    ctx: Arc<Context>,
) -> Result<Action, Error> {
    let name = campaign.name_any();
    let ns = campaign
        .namespace()
        .unwrap_or_else(|| "default".to_string());

    // 1. Resolve the template for objective + parameter space.
    let templates: Api<ExperimentTemplate> = Api::namespaced(ctx.client.clone(), &ns);
    let template = templates
        .get_opt(&campaign.spec.template_ref)
        .await?
        .ok_or_else(|| Error::MissingTemplate(name.clone(), campaign.spec.template_ref.clone()))?;
    let objective = &template.spec.objective;

    // 2. List this campaign's experiments and partition by phase.
    let experiments: Api<Experiment> = Api::namespaced(ctx.client.clone(), &ns);
    let lp = ListParams::default().labels(&format!("{CAMPAIGN_LABEL}={name}"));
    let exps = experiments.list(&lp).await?;

    let mut running = 0u32;
    let mut succeeded = 0u32;
    let mut failed = 0u32;
    let mut completed: Vec<&Experiment> = Vec::new();
    for e in &exps.items {
        match e.status.as_ref().map(|s| &s.phase) {
            Some(ExperimentPhase::Succeeded) => {
                succeeded += 1;
                completed.push(e);
            }
            Some(ExperimentPhase::Failed) | Some(ExperimentPhase::Error) => failed += 1,
            _ => running += 1,
        }
    }
    let total = exps.items.len() as u32;

    // 2b. Canary gate. The canary is the campaign's single pre-budget probe
    // experiment; while it hasn't passed, NOTHING else is generated. Everything
    // here is a no-op for campaigns without spec.canary (gate = Unblock).
    let canary_name = format!("{name}-canary");
    // Only resolved when spec.canary is set, so a hand-made "<name>-canary"
    // experiment can't change the behavior of a canary-less campaign.
    let canary_exp = campaign
        .spec
        .canary
        .as_ref()
        .and_then(|_| exps.items.iter().find(|e| e.name_any() == canary_name));
    // An existing canary with no status yet is Pending, not missing — mapping it
    // to None would make the gate ask for a second create.
    let canary_phase: Option<ExperimentPhase> = canary_exp.map(|e| {
        e.status
            .as_ref()
            .map(|s| s.phase.clone())
            .unwrap_or_default()
    });
    let canary_decision: Option<ExperimentDecision> = canary_exp
        .and_then(|e| e.status.as_ref())
        .and_then(|s| s.decision.clone());
    let gate = canary_gate(
        &campaign.spec,
        canary_phase.as_ref(),
        canary_decision.as_ref(),
    );
    let canary_dead = gate == CanaryGateAction::CanaryFailed;

    // 3. Evaluate: pick best by objective.
    //
    // The canary is deliberately NOT excluded here: it counts like any other
    // succeeded experiment for bestExperiment/bestObjective. Its objective was
    // produced under the same template/metric, so it is comparable in kind —
    // just cheaper, so a real experiment should overtake it quickly. Excluding
    // it would also leave hill-climb/PBT with no seed right after the gate opens.
    let best = pick_best(&completed, objective);

    // 3b. Decision. When a benchmark suite is configured, the campaign does NOT
    // stamp Keep/Discard from the raw training objective — instead it ensures a
    // BenchmarkRun per succeeded experiment and lets the benchmark's gate results
    // drive `status.decision` (via promotionPolicy.updateExperimentStatus). Else,
    // keep the objective-based decision.
    // The canary flows through the same BenchmarkRun machinery but may gate on
    // its own suite (spec.canary.benchmarkSuiteRef falls back to the campaign's),
    // so it is ensured separately below and skipped in both branches here.
    let is_canary = |e: &Experiment| canary_exp.is_some() && e.name_any() == canary_name;
    if let Some(suite) = campaign.spec.benchmark_suite_ref.as_deref() {
        for e in &completed {
            if is_canary(e) {
                continue;
            }
            ensure_benchmark_run(
                &ctx,
                &ns,
                &campaign,
                e,
                suite,
                campaign.spec.benchmark_runtime_profile_ref.as_deref(),
            )
            .await?;
        }
    } else {
        for e in &completed {
            if is_canary(e) {
                // No campaign-wide suite: the canary's decision belongs to its
                // own gate suite (if any); don't stamp an objective decision
                // that could race the benchmark's verdict.
                continue;
            }
            let en = e.name_any();
            let want = if Some(en.as_str()) == best.as_ref().map(|b| b.0.as_str()) {
                ExperimentDecision::Keep
            } else {
                ExperimentDecision::Discard
            };
            if e.status.as_ref().and_then(|s| s.decision.clone()).as_ref() != Some(&want) {
                let patch = json!({ "status": { "decision": want } });
                experiments
                    .patch_status(&en, &PatchParams::apply(MANAGER), &Patch::Merge(&patch))
                    .await?;
            }
        }
    }

    // 3b-canary. Once the canary Succeeds and a suite gates it, run it through
    // the SAME BenchmarkRun machinery as any other experiment; the gate then
    // reads the Keep/Discard verdict the benchmark writes back onto the canary
    // (promotionPolicy.updateExperimentStatus).
    let canary_gate_suite = campaign.spec.canary.as_ref().and_then(|c| {
        c.benchmark_suite_ref
            .as_deref()
            .or(campaign.spec.benchmark_suite_ref.as_deref())
    });
    if let (Some(suite), Some(c), Some(ExperimentPhase::Succeeded)) =
        (canary_gate_suite, canary_exp, canary_phase.as_ref())
    {
        ensure_benchmark_run(
            &ctx,
            &ns,
            &campaign,
            c,
            suite,
            campaign.spec.benchmark_runtime_profile_ref.as_deref(),
        )
        .await?;
    }

    // 3c. Ephemeral inference mesh (mesh-llm): bring it up while the campaign is
    // active, gate experiment generation on its readiness, and tear it down at
    // terminal phase (NOT object deletion — completed campaigns linger for
    // decision evaluation, so ownerReference-only GC would outlive the run).
    //
    // The canary is a pre-budget gate probe: it counts in status.totalExperiments
    // but does NOT consume budget.maxExperiments (otherwise `maxExperiments: 1`
    // plus a canary could never run a single real experiment). Identical to
    // `total` when no canary exists.
    let budgeted_total = total.saturating_sub(u32::from(canary_exp.is_some()));
    let at_budget = budgeted_total >= campaign.spec.budget.max_experiments;
    // "The run ended" = budget reached AND all experiments terminal (running == 0).
    // at_budget alone only means "done generating": the final `concurrency`
    // experiments are still Running when total hits budget and must keep their mesh
    // endpoint until they finish. Keep the mesh up (ensure self-heals) through that
    // drain window; tear down only once nothing is left running.
    // A dead canary also ends the run: nothing further will ever be generated,
    // so the mesh must not idle forever behind a CanaryFailed campaign.
    let all_done = (at_budget || canary_dead) && running == 0;
    // Two backends: multi-node vLLM cluster (RayJob) wins if set, else single-node
    // mesh-llm (Deployment). Same lifecycle: ensure while active, tear down when
    // all experiments are terminal.
    let mesh_ready = if let Some(cluster) = &campaign.spec.inference_cluster {
        if all_done {
            teardown_vllm_cluster(&ctx, &ns, &name).await?;
            true
        } else {
            ensure_vllm_cluster(&ctx, &ns, &campaign, &name, cluster).await?
        }
    } else {
        match &campaign.spec.inference_mesh {
            Some(mesh) if !all_done => ensure_mesh(&ctx, &ns, &campaign, &name, mesh).await?,
            Some(_) => {
                teardown_mesh(&ctx, &ns, &name).await?;
                true
            }
            None => true,
        }
    };

    // 4. Generate next experiments within budget + concurrency.
    // PBT runs a population at once: its concurrency target is populationSize
    // when set, falling back to spec.concurrency. Heuristic uses concurrency.
    let is_pbt = campaign.spec.strategy.strategy_type == "pbt";
    let concurrency = if is_pbt {
        campaign
            .spec
            .population_size
            .unwrap_or(campaign.spec.concurrency)
            .max(1)
    } else {
        campaign.spec.concurrency.max(1)
    };
    let mut created = 0u32;
    let mut canary_created = false;
    // When an inference mesh is configured, hold experiment creation until it is
    // Ready so the first prover Jobs don't launch against a dead LLM_BASE_URL.
    // The canary gate wraps around that: CreateCanary generates exactly the
    // canary, Hold/CanaryFailed generate nothing, Unblock is today's behavior
    // (and the only reachable arm when spec.canary is unset).
    match gate {
        CanaryGateAction::CreateCanary if mesh_ready => {
            if let Some(canary_spec) = &campaign.spec.canary {
                let exp = build_canary_experiment(&campaign, &name, &ns, &template, canary_spec);
                match experiments.create(&PostParams::default(), &exp).await {
                    Ok(_) => {
                        canary_created = true;
                        info!(campaign = %name, canary = %canary_name,
                            "created canary gate experiment; holding generation until it passes");
                    }
                    Err(kube::Error::Api(e)) if e.code == 409 => {}
                    Err(e) => {
                        warn!(%e, canary = %canary_name, "failed to create canary experiment")
                    }
                }
            }
        }
        // Canary waiting on the mesh, in flight, or awaiting its benchmark
        // verdict — or dead: generate nothing.
        CanaryGateAction::CreateCanary
        | CanaryGateAction::Hold
        | CanaryGateAction::CanaryFailed => {}
        CanaryGateAction::Unblock if !at_budget && mesh_ready => {
            let want = concurrency.saturating_sub(running);
            let budget_left = campaign.spec.budget.max_experiments - budgeted_total;
            // The best succeeded experiment is the seed for both strategies: its
            // params drive perturbation and (for PBT) its latest checkpoint warm-
            // starts the children's weights.
            let best_exp: Option<&Experiment> = best
                .as_ref()
                .and_then(|(bn, _)| completed.iter().find(|e| &e.name_any() == bn).copied());
            // A canary seed's SCIENCE params are a valid hill-climb/PBT start,
            // but its cheapness must not leak into budgeted children: keys the
            // canary explicitly overrode (spec.canary.parameters, e.g.
            // total_timesteps: 2M) are stripped so they revert to template
            // defaults. Caught live: spot-recover-v70-001 ran at the canary's
            // 2M budget instead of the template's 15M.
            let seed_params: Option<BTreeMap<String, Value>> = best_exp.map(|e| {
                canary_seed_params(
                    &e.spec.parameters,
                    campaign.spec.canary.as_ref().map(|c| &c.parameters),
                )
            });
            let best_ctx = best
                .as_ref()
                .zip(seed_params.as_ref())
                .map(|((bn, _), p)| (bn.as_str(), p));
            // PBT explore factor (guard against non-positive overrides).
            let perturb_factor = match campaign.spec.perturb_factor {
                Some(f) if f > 0.0 => f,
                _ => PBT_FACTOR,
            };
            // Already-tried science points (all children, any phase) for duplicate
            // detection; grows with the candidates created this pass.
            let mut seen: std::collections::HashSet<String> = exps
                .items
                .iter()
                .map(|e| science_key(&e.spec.parameters))
                .collect();
            for i in 0..want.min(budget_left) {
                let idx = total + i;
                let generate = |salt: u32| {
                    if is_pbt {
                        let (params, hypothesis) =
                            pbt_experiment(&template, best_ctx, perturb_factor, idx, salt);
                        (params, hypothesis, pbt_checkpoint_policy(best_exp))
                    } else {
                        let (params, hypothesis) = next_experiment(&template, best_ctx, idx, salt);
                        (params, hypothesis, None)
                    }
                };
                // Dedup only applies when there is a best to perturb from — baselines
                // have nothing to vary. Bounded re-rolls; if the local lattice is
                // exhausted, accept the duplicate as a labeled replicate (liveness).
                let mut chosen = generate(0);
                if best_ctx.is_some() {
                    let mut deduped = false;
                    for salt in 0..=MAX_REROLLS {
                        let candidate = generate(salt);
                        if !seen.contains(&science_key(&candidate.0)) {
                            chosen = candidate;
                            deduped = true;
                            break;
                        }
                    }
                    if !deduped {
                        chosen.1 =
                            format!("{} [replicate: local search space exhausted]", chosen.1);
                    }
                }
                seen.insert(science_key(&chosen.0));
                let (params, hypothesis, checkpoint_policy) = chosen;
                let exp = build_experiment(
                    &campaign,
                    &name,
                    &ns,
                    idx,
                    params,
                    hypothesis,
                    checkpoint_policy,
                );
                match experiments.create(&PostParams::default(), &exp).await {
                    Ok(_) => created += 1,
                    Err(e) => warn!(%e, idx, "failed to create experiment"),
                }
            }
            if created > 0 {
                info!(campaign = %name, created, total = total + created, "generated experiments");
            }
        }
        // Unblocked but at budget / mesh not ready: nothing to generate this pass.
        CanaryGateAction::Unblock => {}
    }

    // 5. Update campaign status. A dead canary parks the campaign in
    // CanaryFailed (nothing is deleted; the canary experiment and its
    // BenchmarkRun stay around as the record of WHY the recipe was vetoed).
    let new = created + u32::from(canary_created);
    let phase = if canary_dead {
        "CanaryFailed"
    } else if at_budget {
        "Completed"
    } else {
        "Running"
    };
    let mut status = json!({ "status": {
        "runningExperiments": running + new,
        "succeededExperiments": succeeded,
        "failedExperiments": failed,
        "totalExperiments": total + new,
        "bestExperiment": best.as_ref().map(|b| b.0.clone()),
        "bestObjective": best.as_ref().map(|b| b.1),
        "phase": phase,
        "observedGeneration": campaign.metadata.generation,
        "controllerVersion": env!("CARGO_PKG_VERSION"),
    }});
    // Canary status is only ever written for canary campaigns, so existing CRs
    // are untouched (merge-patch: keys we don't send are left alone).
    if campaign.spec.canary.is_some() && (canary_exp.is_some() || canary_created) {
        status["status"]["canaryExperiment"] = json!(canary_name);
        status["status"]["canaryState"] = json!(canary_state(gate, canary_phase.as_ref()));
    }
    let campaigns: Api<ResearchCampaign> = Api::namespaced(ctx.client.clone(), &ns);
    campaigns
        .patch_status(&name, &PatchParams::apply(MANAGER), &Patch::Merge(&status))
        .await?;

    // Poll faster while the loop is active so it advances promptly between runs.
    // A CanaryFailed campaign is as terminal as a completed one.
    Ok(Action::requeue(Duration::from_secs(
        if at_budget || canary_dead { 300 } else { 15 },
    )))
}

/// Read an experiment's objective value from its status metrics.
fn objective_value(exp: &Experiment, metric: &str) -> Option<f64> {
    let v = exp.status.as_ref()?.metrics.get(metric)?;
    value_as_f64(v)
}

fn value_as_f64(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.parse().ok(),
        _ => None,
    }
}

/// Best (name, objective value) among succeeded experiments per the goal.
fn pick_best(completed: &[&Experiment], objective: &ObjectiveSpec) -> Option<(String, f64)> {
    completed
        .iter()
        .filter_map(|e| objective_value(e, &objective.metric).map(|v| (e.name_any(), v)))
        .reduce(|a, b| {
            let better = match objective.goal {
                ObjectiveGoal::Minimize => b.1 < a.1,
                ObjectiveGoal::Maximize => b.1 > a.1,
            };
            if better { b } else { a }
        })
}

/// Numeric parameters from a base map, sorted by key for deterministic cycling.
fn numeric_params(base: &BTreeMap<String, Value>) -> Vec<String> {
    base.iter()
        .filter(|(_, v)| value_as_f64(v).is_some())
        .map(|(k, _)| k.clone())
        .collect()
}

/// Bookkeeping keys stamped on every child; excluded from duplicate detection.
const BOOKKEEPING: [&str; 2] = ["experimentIteration", "parentExperimentId"];

/// Canonical string of the science parameters (bookkeeping stripped) for
/// duplicate-point detection across a campaign's children.
fn science_key(params: &BTreeMap<String, Value>) -> String {
    params
        .iter()
        .filter(|(k, _)| !BOOKKEEPING.contains(&k.as_str()))
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join(",")
}

/// Bounded re-rolls when a generated candidate duplicates an already-tried
/// point. Each salt varies the perturbation pattern; if the local lattice is
/// exhausted we accept the duplicate as a deliberate replicate (liveness).
const MAX_REROLLS: u32 = 8;

/// Build the parameter set + hypothesis for the next experiment.
///
/// idx 0 (or no best yet) → baseline from template defaults. Otherwise hill-climb
/// from the best experiment's parameters by perturbing one numeric parameter
/// (coordinate-wise, alternating direction each lap). Always stamps the loop
/// bookkeeping params (iteration/parent/tag) the runners read from the spec.
fn next_experiment(
    template: &ExperimentTemplate,
    best: Option<(&str, &BTreeMap<String, Value>)>,
    idx: u32,
    salt: u32,
) -> (BTreeMap<String, Value>, String) {
    // Base: template defaults + parameter_schema defaults. For non-baseline
    // experiments, overlay the best experiment's parameters so the hill-climb
    // builds on the best-so-far rather than always perturbing the default.
    let mut params: BTreeMap<String, Value> = template.spec.defaults.clone();
    for (k, spec) in &template.spec.parameter_schema {
        if let Some(d) = &spec.default {
            params.entry(k.clone()).or_insert_with(|| d.clone());
        }
    }
    if idx > 0 {
        if let Some((_, best_params)) = best {
            for (k, v) in best_params.iter() {
                if k != "experimentIteration" && k != "parentExperimentId" {
                    params.insert(k.clone(), v.clone());
                }
            }
        }
    }

    let hypothesis = match best {
        // First experiment, or nothing to climb from: baseline.
        _ if idx == 0 => "baseline: template defaults".to_string(),
        None => "baseline: no successful parent yet".to_string(),
        Some((best_name, _)) => {
            let keys = numeric_params(&params);
            if keys.is_empty() {
                format!("re-run from {best_name} (no numeric parameters to perturb)")
            } else {
                // Coordinate descent: pick a parameter, alternate direction per
                // lap. The dedup salt advances the (param, direction) cycle so a
                // re-roll lands on an untried point.
                let pick = idx as usize - 1 + salt as usize;
                let lap = pick / keys.len();
                let key = &keys[pick % keys.len()];
                let factor = if lap % 2 == 0 { 1.0 + STEP } else { 1.0 - STEP };
                if let Some(cur) = params.get(key).and_then(value_as_f64) {
                    let next = cur * factor;
                    params.insert(
                        key.clone(),
                        serde_json::Number::from_f64(next)
                            .map(Value::Number)
                            .unwrap_or(Value::Null),
                    );
                    format!("perturb {key} {cur:.4}->{next:.4} (x{factor}) from {best_name}")
                } else {
                    format!("re-run from {best_name}")
                }
            }
        }
    };

    params.insert("experimentIteration".into(), json!(idx));
    params.insert(
        "parentExperimentId".into(),
        best.map(|(n, _)| json!(n)).unwrap_or(Value::Null),
    );
    (params, hypothesis)
}

/// Build a PBT child's parameter set + hypothesis.
///
/// Population-based training: every child *exploits* the best (inherits its
/// hyperparameters) and then *explores* by perturbing each numeric parameter by
/// `perturb_factor` (up) or `1/perturb_factor` (down). Weights are warm-started
/// separately via [`pbt_checkpoint_policy`]. Direction alternates per parameter
/// and per child so concurrently-spawned children diverge. With no successful
/// parent yet this is a cold start from the template defaults.
fn pbt_experiment(
    template: &ExperimentTemplate,
    best: Option<(&str, &BTreeMap<String, Value>)>,
    perturb_factor: f64,
    idx: u32,
    salt: u32,
) -> (BTreeMap<String, Value>, String) {
    // Base: template defaults + parameter_schema defaults (the param space).
    let mut params: BTreeMap<String, Value> = template.spec.defaults.clone();
    for (k, spec) in &template.spec.parameter_schema {
        if let Some(d) = &spec.default {
            params.entry(k.clone()).or_insert_with(|| d.clone());
        }
    }

    let hypothesis = match best {
        None => "pbt cold start: no successful parent yet".to_string(),
        Some((best_name, best_params)) => {
            // Exploit: inherit the best's hyperparameters.
            for (k, v) in best_params.iter() {
                if k != "experimentIteration" && k != "parentExperimentId" {
                    params.insert(k.clone(), v.clone());
                }
            }
            // Explore: perturb every numeric param up or 1/factor down.
            let down = 1.0 / perturb_factor;
            let keys = numeric_params(&params);
            let mut perturbed = Vec::new();
            for (j, key) in keys.iter().enumerate() {
                if let Some(cur) = params.get(key).and_then(value_as_f64) {
                    // Base parity alternates per child; the dedup salt is a
                    // bitmask flipping individual params' directions, giving
                    // 2^n distinct patterns instead of two.
                    let up = ((idx as usize + j) % 2 == 0) ^ ((salt >> j) & 1 == 1);
                    let factor = if up { perturb_factor } else { down };
                    let next = cur * factor;
                    params.insert(
                        key.clone(),
                        serde_json::Number::from_f64(next)
                            .map(Value::Number)
                            .unwrap_or(Value::Null),
                    );
                    perturbed.push(format!("{key} x{factor:.4}"));
                }
            }
            if perturbed.is_empty() {
                format!("pbt exploit from {best_name} (no numeric params to perturb)")
            } else {
                format!(
                    "pbt exploit+explore from {best_name}: {}",
                    perturbed.join(", ")
                )
            }
        }
    };

    params.insert("experimentIteration".into(), json!(idx));
    params.insert(
        "parentExperimentId".into(),
        best.map(|(n, _)| json!(n)).unwrap_or(Value::Null),
    );
    (params, hypothesis)
}

/// Warm-start policy for a PBT child: resume weights from the best succeeded
/// experiment's latest checkpoint. None when there is no best, no checkpoint
/// yet, or no usable URI — i.e. a cold start.
fn pbt_checkpoint_policy(best_exp: Option<&Experiment>) -> Option<CheckpointPolicy> {
    let uri = best_exp?
        .status
        .as_ref()?
        .latest_checkpoint
        .as_ref()?
        .uri
        .clone();
    Some(CheckpointPolicy {
        resume_from: Some(uri),
        ..Default::default()
    })
}

/// What the campaign loop must do about its canary gate this pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CanaryGateAction {
    /// spec.canary is set and the canary experiment doesn't exist yet: create
    /// exactly it, and generate nothing else.
    CreateCanary,
    /// Canary exists but hasn't passed (non-terminal, or Succeeded and still
    /// awaiting its benchmark verdict / flagged NeedsReview): hold generation.
    Hold,
    /// No canary configured, or the canary passed: normal generation.
    Unblock,
    /// Canary Failed/Errored, or its benchmark gates rejected it (Discard):
    /// the recipe is vetoed; generate nothing further, ever.
    CanaryFailed,
}

/// Pure canary gating decision.
///
/// `canary_phase` is None when the canary experiment does not exist yet (an
/// existing experiment with no status is Pending, not missing). The benchmark
/// verdict arrives as the canary experiment's `status.decision`, written back
/// by its BenchmarkRun (promotionPolicy.updateExperimentStatus).
///
/// The gate is Succeeded-only when no suite applies; with a suite (the canary's
/// own `benchmarkSuiteRef`, falling back to the campaign's), Succeeded is
/// necessary but the Keep verdict is what opens the gate.
fn canary_gate(
    spec: &ResearchCampaignSpec,
    canary_phase: Option<&ExperimentPhase>,
    canary_decision: Option<&ExperimentDecision>,
) -> CanaryGateAction {
    // No canary configured: the gate does not exist. Everything downstream is
    // guarded on this, keeping canary-less campaigns byte-identical to before.
    let Some(canary) = &spec.canary else {
        return CanaryGateAction::Unblock;
    };
    let Some(phase) = canary_phase else {
        return CanaryGateAction::CreateCanary;
    };
    match phase {
        ExperimentPhase::Failed | ExperimentPhase::Error => CanaryGateAction::CanaryFailed,
        ExperimentPhase::Succeeded => {
            let gated = canary
                .benchmark_suite_ref
                .as_deref()
                .or(spec.benchmark_suite_ref.as_deref())
                .is_some();
            if !gated {
                return CanaryGateAction::Unblock;
            }
            match canary_decision {
                Some(ExperimentDecision::Keep) => CanaryGateAction::Unblock,
                Some(ExperimentDecision::Discard) => CanaryGateAction::CanaryFailed,
                // No verdict yet, or NeedsReview: the benchmark hasn't ruled
                // (or a human must) — keep holding rather than guessing.
                _ => CanaryGateAction::Hold,
            }
        }
        _ => CanaryGateAction::Hold,
    }
}

/// status.canaryState string for the current gate outcome.
fn canary_state(gate: CanaryGateAction, canary_phase: Option<&ExperimentPhase>) -> &'static str {
    match gate {
        CanaryGateAction::CreateCanary => "pending",
        CanaryGateAction::Hold => match canary_phase {
            // Succeeded-but-awaiting-verdict reads as "running": the gate work
            // (the BenchmarkRun) is still in flight.
            Some(ExperimentPhase::Pending) | None => "pending",
            _ => "running",
        },
        CanaryGateAction::Unblock => "passed",
        CanaryGateAction::CanaryFailed => "failed",
    }
}

/// Seed params for generation: strip every key spec.canary.parameters
/// overrides, from ANY seed, so budgeted children always fall back to
/// template defaults for those keys. Canary overrides are canary-only by
/// definition — and the leak is generational: a child that inherited the
/// canary's cheap total_timesteps would pass it to ITS children even when it
/// (not the canary) becomes the best seed, so a canary-seed-only strip is not
/// enough.
fn canary_seed_params(
    params: &BTreeMap<String, Value>,
    canary_overrides: Option<&Value>,
) -> BTreeMap<String, Value> {
    let strip: Option<&serde_json::Map<String, Value>> =
        canary_overrides.and_then(Value::as_object);
    params
        .iter()
        .filter(|(k, _)| strip.is_none_or(|m| !m.contains_key(*k)))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

/// Canary parameters: template defaults (spec.defaults + parameterSchema
/// defaults) with the canary overrides deep-merged on top — canary wins at any
/// depth, arrays replace wholesale, and an explicit null falls back to the
/// default (same semantics as RuntimeProfile defaulting). A non-object,
/// non-null override can't describe a parameter map and is ignored rather than
/// wiping the defaults.
fn merge_canary_parameters(
    base: BTreeMap<String, Value>,
    overrides: &Value,
) -> BTreeMap<String, Value> {
    if !overrides.is_object() && !overrides.is_null() {
        return base;
    }
    match deep_merge(Value::Object(base.into_iter().collect()), overrides.clone()) {
        Value::Object(m) => m.into_iter().collect(),
        // object/null merged over an object is always an object.
        _ => unreachable!("deep_merge of object base with object/null overrides"),
    }
}

/// Build the campaign's single canary gate experiment: `<campaign>-canary`,
/// labeled `athena.nixlab.io/canary=true` (plus the usual campaign label),
/// parameters = template defaults ⊕ spec.canary.parameters.
fn build_canary_experiment(
    campaign: &ResearchCampaign,
    campaign_name: &str,
    ns: &str,
    template: &ExperimentTemplate,
    canary: &CanarySpec,
) -> Experiment {
    // Same base the generation strategies start from.
    let mut base: BTreeMap<String, Value> = template.spec.defaults.clone();
    for (k, spec) in &template.spec.parameter_schema {
        if let Some(d) = &spec.default {
            base.entry(k.clone()).or_insert_with(|| d.clone());
        }
    }
    let mut params = merge_canary_parameters(base, &canary.parameters);
    // Bookkeeping the runners read from the spec: the canary is iteration 0
    // with no parent (budgeted experiments start at idx = total, i.e. 1).
    params.insert("experimentIteration".into(), json!(0));
    params.insert("parentExperimentId".into(), Value::Null);

    let owner = OwnerReference {
        api_version: "research.nixlab.io/v1alpha1".to_string(),
        kind: "ResearchCampaign".to_string(),
        name: campaign_name.to_string(),
        uid: campaign.uid().unwrap_or_default(),
        controller: Some(true),
        block_owner_deletion: Some(true),
    };
    Experiment {
        metadata: ObjectMeta {
            name: Some(format!("{campaign_name}-canary")),
            namespace: Some(ns.to_string()),
            labels: Some(BTreeMap::from([
                (CAMPAIGN_LABEL.to_string(), campaign_name.to_string()),
                (CANARY_LABEL.to_string(), "true".to_string()),
            ])),
            owner_references: Some(vec![owner]),
            ..Default::default()
        },
        spec: ExperimentSpec {
            campaign_ref: campaign_name.to_string(),
            hypothesis: "canary gate: cheap probe of the recipe before spending budget".to_string(),
            parameters: params,
            patch: None,
            checkpoint_policy: None,
            env: mesh_env(campaign, campaign_name, ns),
        },
        status: None,
    }
}

fn build_experiment(
    campaign: &ResearchCampaign,
    campaign_name: &str,
    ns: &str,
    idx: u32,
    parameters: BTreeMap<String, Value>,
    hypothesis: String,
    checkpoint_policy: Option<CheckpointPolicy>,
) -> Experiment {
    let exp_name = format!("{campaign_name}-{idx:03}");
    let owner = OwnerReference {
        api_version: "research.nixlab.io/v1alpha1".to_string(),
        kind: "ResearchCampaign".to_string(),
        name: campaign_name.to_string(),
        uid: campaign.uid().unwrap_or_default(),
        controller: Some(true),
        block_owner_deletion: Some(true),
    };
    Experiment {
        metadata: ObjectMeta {
            name: Some(exp_name),
            namespace: Some(ns.to_string()),
            labels: Some(BTreeMap::from([(
                CAMPAIGN_LABEL.to_string(),
                campaign_name.to_string(),
            )])),
            owner_references: Some(vec![owner]),
            ..Default::default()
        },
        spec: ExperimentSpec {
            campaign_ref: campaign_name.to_string(),
            hypothesis,
            parameters,
            patch: None,
            checkpoint_policy,
            env: mesh_env(campaign, campaign_name, ns),
        },
        status: None,
    }
}

/// When the campaign runs an ephemeral inference mesh, point experiment jobs at
/// its Service via `LLM_BASE_URL` (prove.py reads this env directly). Empty
/// otherwise, so non-mesh campaigns are unchanged.
fn mesh_env(campaign: &ResearchCampaign, campaign_name: &str, ns: &str) -> Vec<EnvVar> {
    // Multi-node vLLM cluster wins over single-node mesh (mirrors ensure order).
    let url = if let Some(cluster) = &campaign.spec.inference_cluster {
        Some(format!(
            "http://vllm-{campaign_name}.{ns}.svc.cluster.local:{}/v1",
            cluster.port
        ))
    } else {
        campaign.spec.inference_mesh.as_ref().map(|mesh| {
            format!(
                "http://mesh-llm-{campaign_name}.{ns}.svc.cluster.local:{}/v1",
                mesh.port
            )
        })
    };
    match url {
        Some(u) => vec![EnvVar {
            name: "LLM_BASE_URL".to_string(),
            value: Some(u),
        }],
        None => Vec::new(),
    }
}

/// Ensure the campaign's ephemeral mesh-llm Deployment + Service exist, returning
/// whether the Deployment reports at least one available (Ready) replica.
/// Idempotent: creates on first sight, otherwise just reads readiness. Both
/// objects are owned by the campaign (crash-safety GC backstop); terminal-phase
/// `teardown_mesh` is the primary teardown path.
async fn ensure_mesh(
    ctx: &Arc<Context>,
    ns: &str,
    campaign: &ResearchCampaign,
    campaign_name: &str,
    mesh: &InferenceMeshSpec,
) -> Result<bool, Error> {
    let name = format!("mesh-llm-{campaign_name}");
    let deployments: Api<Deployment> = Api::namespaced(ctx.client.clone(), ns);
    let services: Api<Service> = Api::namespaced(ctx.client.clone(), ns);

    let owner = OwnerReference {
        api_version: "research.nixlab.io/v1alpha1".to_string(),
        kind: "ResearchCampaign".to_string(),
        name: campaign_name.to_string(),
        uid: campaign.uid().unwrap_or_default(),
        controller: Some(true),
        block_owner_deletion: Some(true),
    };
    let labels = BTreeMap::from([
        ("app".to_string(), "mesh-llm".to_string()),
        (CAMPAIGN_LABEL.to_string(), campaign_name.to_string()),
    ]);

    if services.get_opt(&name).await?.is_none() {
        let svc = Service {
            metadata: ObjectMeta {
                name: Some(name.clone()),
                namespace: Some(ns.to_string()),
                labels: Some(labels.clone()),
                owner_references: Some(vec![owner.clone()]),
                ..Default::default()
            },
            spec: Some(ServiceSpec {
                selector: Some(labels.clone()),
                ports: Some(vec![ServicePort {
                    name: Some("http".to_string()),
                    port: mesh.port as i32,
                    target_port: Some(IntOrString::Int(mesh.port as i32)),
                    ..Default::default()
                }]),
                ..Default::default()
            }),
            ..Default::default()
        };
        match services.create(&PostParams::default(), &svc).await {
            Ok(_) => info!(campaign = %campaign_name, %name, "created inference mesh Service"),
            Err(kube::Error::Api(e)) if e.code == 409 => {}
            Err(e) => return Err(Error::Kube(e)),
        }
    }

    match deployments.get_opt(&name).await? {
        None => {
            let dep = build_mesh_deployment(&name, ns, mesh, &labels, owner);
            match deployments.create(&PostParams::default(), &dep).await {
                Ok(_) => {
                    info!(campaign = %campaign_name, %name, "created inference mesh Deployment")
                }
                Err(kube::Error::Api(e)) if e.code == 409 => {}
                Err(e) => return Err(Error::Kube(e)),
            }
            Ok(false)
        }
        Some(dep) => Ok(dep
            .status
            .as_ref()
            .and_then(|s| s.available_replicas)
            .unwrap_or(0)
            >= 1),
    }
}

/// Build the mesh-llm serving Deployment: `mesh-llm serve --model <m> --listen-all
/// --headless --port <p>` with the campaign-specified node placement, tolerations,
/// GPU resources, and runtimeClassName.
fn build_mesh_deployment(
    name: &str,
    ns: &str,
    mesh: &InferenceMeshSpec,
    labels: &BTreeMap<String, String>,
    owner: OwnerReference,
) -> Deployment {
    let mut args = vec![
        "serve".to_string(),
        "--model".to_string(),
        mesh.model.clone(),
        "--listen-all".to_string(),
        "--headless".to_string(),
        "--port".to_string(),
        mesh.port.to_string(),
    ];
    args.extend(mesh.extra_args.iter().cloned());

    let resources = if mesh.gpu_resources.is_empty() {
        None
    } else {
        let q: BTreeMap<String, Quantity> = mesh
            .gpu_resources
            .iter()
            .map(|(k, v)| (k.clone(), Quantity(v.clone())))
            .collect();
        Some(ResourceRequirements {
            limits: Some(q.clone()),
            requests: Some(q),
            ..Default::default()
        })
    };

    let tolerations: Option<Vec<Toleration>> = if mesh.tolerations.is_empty() {
        None
    } else {
        Some(
            mesh.tolerations
                .iter()
                .filter_map(|v| serde_json::from_value(v.clone()).ok())
                .collect(),
        )
    };

    let node_selector = if mesh.node_selector.is_empty() {
        None
    } else {
        Some(mesh.node_selector.clone())
    };

    let container = Container {
        name: "mesh-llm".to_string(),
        image: Some(mesh.image.clone()),
        args: Some(args),
        ports: Some(vec![ContainerPort {
            container_port: mesh.port as i32,
            name: Some("http".to_string()),
            ..Default::default()
        }]),
        resources,
        // Readiness must reflect *serving* readiness, not container start: /v1/models
        // only 200s once the model is loaded. Without this the campaign's readiness
        // gate would release experiments against a mesh that isn't serving yet.
        // failureThreshold * periodSeconds (60 * 15s = 15m) covers model download.
        readiness_probe: Some(Probe {
            http_get: Some(HTTPGetAction {
                path: Some("/v1/models".to_string()),
                port: IntOrString::Int(mesh.port as i32),
                ..Default::default()
            }),
            initial_delay_seconds: Some(20),
            period_seconds: Some(15),
            timeout_seconds: Some(5),
            failure_threshold: Some(60),
            ..Default::default()
        }),
        ..Default::default()
    };

    Deployment {
        metadata: ObjectMeta {
            name: Some(name.to_string()),
            namespace: Some(ns.to_string()),
            labels: Some(labels.clone()),
            owner_references: Some(vec![owner]),
            ..Default::default()
        },
        spec: Some(DeploymentSpec {
            replicas: Some(1),
            selector: LabelSelector {
                match_labels: Some(labels.clone()),
                ..Default::default()
            },
            template: PodTemplateSpec {
                metadata: Some(ObjectMeta {
                    labels: Some(labels.clone()),
                    ..Default::default()
                }),
                spec: Some(PodSpec {
                    containers: vec![container],
                    node_selector,
                    tolerations,
                    runtime_class_name: mesh.runtime_class_name.clone(),
                    ..Default::default()
                }),
            },
            ..Default::default()
        }),
        ..Default::default()
    }
}

/// Delete the campaign's mesh Deployment + Service at terminal phase. Idempotent:
/// only deletes what still exists, so repeated terminal reconciles are no-ops.
async fn teardown_mesh(ctx: &Arc<Context>, ns: &str, campaign_name: &str) -> Result<(), Error> {
    let name = format!("mesh-llm-{campaign_name}");
    let deployments: Api<Deployment> = Api::namespaced(ctx.client.clone(), ns);
    let services: Api<Service> = Api::namespaced(ctx.client.clone(), ns);
    if deployments.get_opt(&name).await?.is_some() {
        deployments.delete(&name, &DeleteParams::default()).await?;
        info!(campaign = %campaign_name, %name, "tore down inference mesh Deployment");
    }
    if services.get_opt(&name).await?.is_some() {
        services.delete(&name, &DeleteParams::default()).await?;
    }
    Ok(())
}

/// Dynamic Api for the ray.io/v1 RayJob CRD (no k8s_openapi type).
fn rayjob_api(ctx: &Arc<Context>, ns: &str) -> Api<DynamicObject> {
    let ar = ApiResource::from_gvk(&GroupVersionKind::gvk("ray.io", "v1", "RayJob"));
    Api::namespaced_with(ctx.client.clone(), ns, &ar)
}

/// Ensure the campaign's ephemeral multi-node vLLM cluster (RayJob + stable head
/// Service) exists, returning whether it is Ready (serving). Readiness = the head
/// Service has ready endpoints; the head container's readinessProbe hits vLLM's
/// /health, so an endpoint appears only once vLLM is actually serving (not merely
/// when Ray started). Both objects owned by the campaign (GC backstop);
/// terminal-phase teardown is the primary path.
async fn ensure_vllm_cluster(
    ctx: &Arc<Context>,
    ns: &str,
    campaign: &ResearchCampaign,
    campaign_name: &str,
    cluster: &VllmClusterSpec,
) -> Result<bool, Error> {
    let name = format!("vllm-{campaign_name}");
    let owner = OwnerReference {
        api_version: "research.nixlab.io/v1alpha1".to_string(),
        kind: "ResearchCampaign".to_string(),
        name: campaign_name.to_string(),
        uid: campaign.uid().unwrap_or_default(),
        controller: Some(true),
        block_owner_deletion: Some(true),
    };

    // Stable head Service (selects the head pod by campaign label + ray head role).
    let services: Api<Service> = Api::namespaced(ctx.client.clone(), ns);
    if services.get_opt(&name).await?.is_none() {
        let selector = BTreeMap::from([
            (CAMPAIGN_LABEL.to_string(), campaign_name.to_string()),
            ("ray.io/node-type".to_string(), "head".to_string()),
        ]);
        let svc = Service {
            metadata: ObjectMeta {
                name: Some(name.clone()),
                namespace: Some(ns.to_string()),
                owner_references: Some(vec![owner.clone()]),
                ..Default::default()
            },
            spec: Some(ServiceSpec {
                selector: Some(selector),
                ports: Some(vec![ServicePort {
                    name: Some("http".to_string()),
                    port: cluster.port as i32,
                    target_port: Some(IntOrString::Int(cluster.port as i32)),
                    ..Default::default()
                }]),
                ..Default::default()
            }),
            ..Default::default()
        };
        match services.create(&PostParams::default(), &svc).await {
            Ok(_) => info!(campaign = %campaign_name, %name, "created vLLM head Service"),
            Err(kube::Error::Api(e)) if e.code == 409 => {}
            Err(e) => return Err(Error::Kube(e)),
        }
    }

    // RayJob (dynamic).
    let rayjobs = rayjob_api(ctx, ns);
    match rayjobs.get_opt(&name).await? {
        None => {
            let rj = build_vllm_rayjob(&name, ns, campaign_name, cluster, owner);
            match rayjobs.create(&PostParams::default(), &rj).await {
                Ok(_) => info!(campaign = %campaign_name, %name, "created vLLM RayJob"),
                Err(kube::Error::Api(e)) if e.code == 409 => {}
                Err(e) => return Err(Error::Kube(e)),
            }
            return Ok(false);
        }
        Some(rj) => {
            // Surface a Failed RayJob — it will NOT self-heal (get_opt sees it, so
            // ensure never recreates), leaving the campaign hung at the readiness
            // gate. Visible here; recreate-on-Failed is a future hardening.
            let dep = rj
                .data
                .get("status")
                .and_then(|s| s.get("jobDeploymentStatus"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if dep == "Failed" {
                warn!(campaign = %campaign_name, %name,
                    "vLLM RayJob is FAILED — campaign is gated with no experiments; \
                     delete it to retry or inspect `kubectl logs`");
            }
        }
    }

    // Serving-readiness: poll vLLM's /health directly (the head has NO readinessProbe
    // — that would deadlock KubeRay provisioning). /health 200s only once vLLM is up.
    let health = format!(
        "http://{name}.{ns}.svc.cluster.local:{}/health",
        cluster.port
    );
    let ready = match reqwest::Client::builder()
        .timeout(Duration::from_secs(3))
        .build()
    {
        Ok(c) => c
            .get(&health)
            .send()
            .await
            .map(|r| r.status().is_success())
            .unwrap_or(false),
        Err(_) => false,
    };
    Ok(ready)
}

/// Build the vLLM-on-Ray RayJob: GPU head (driver + rank 0) plus
/// pipelineParallelSize-1 GPU workers, PP across all. Verified config on Turing:
/// fp16 + enforce-eager + XFORMERS; head readinessProbe on vLLM /health.
fn build_vllm_rayjob(
    name: &str,
    ns: &str,
    campaign_name: &str,
    cluster: &VllmClusterSpec,
    owner: OwnerReference,
) -> DynamicObject {
    let workers = cluster.pipeline_parallel_size.saturating_sub(1);
    let mut entrypoint = format!(
        "vllm serve {} --pipeline-parallel-size {} --distributed-executor-backend ray \
         --dtype {} --enforce-eager --gpu-memory-utilization 0.9 --max-model-len {} \
         --host 0.0.0.0 --port {} --trust-remote-code",
        cluster.model,
        cluster.pipeline_parallel_size,
        cluster.dtype,
        cluster.max_model_len,
        cluster.port
    );
    if !cluster.extra_args.is_empty() {
        entrypoint.push(' ');
        entrypoint.push_str(&cluster.extra_args.join(" "));
    }

    let tolerations = if cluster.tolerations.is_empty() {
        json!([{ "key": "nvidia.com/gpu", "operator": "Exists", "effect": "NoSchedule" }])
    } else {
        Value::Array(cluster.tolerations.clone())
    };
    let node_selector = json!({ "nvidia.com/gpu.product": cluster.gpu_product });
    let env = json!([
        { "name": "VLLM_ATTENTION_BACKEND", "value": "XFORMERS" },
        { "name": "HF_HOME", "value": "/data/hf" },
    ]);
    let gpu_resources = json!({
        "requests": { "cpu": "8", "memory": "24Gi", "nvidia.com/gpu": "1" },
        "limits": { "memory": "40Gi", "nvidia.com/gpu": "1" },
    });
    let volumes = json!([{ "name": "hf", "emptyDir": { "sizeLimit": "40Gi" } }]);
    let volume_mounts = json!([{ "name": "hf", "mountPath": "/data/hf" }]);

    let body = json!({
        "apiVersion": "ray.io/v1",
        "kind": "RayJob",
        "metadata": {
            "name": name,
            "namespace": ns,
            "labels": {
                "kueue.x-k8s.io/queue-name": cluster.queue_name,
                "kueue.x-k8s.io/priority-class": cluster.priority_class,
            },
            "ownerReferences": [owner],
        },
        "spec": {
            "shutdownAfterJobFinishes": true,
            "entrypoint": entrypoint,
            "rayClusterSpec": {
                "rayVersion": cluster.ray_version,
                "headGroupSpec": {
                    "rayStartParams": { "dashboard-host": "0.0.0.0" },
                    "template": {
                        "metadata": { "labels": { CAMPAIGN_LABEL: campaign_name } },
                        "spec": {
                            "runtimeClassName": cluster.runtime_class_name,
                            "nodeSelector": node_selector,
                            "tolerations": tolerations,
                            // NO readinessProbe on the head: HeadPodReady gates
                            // KubeRay provisioning, which gates entrypoint submission,
                            // which starts vLLM — a /health probe here deadlocks
                            // (head never Ready → never provisioned → vLLM never runs).
                            // Serving-readiness is polled by the reconciler over HTTP.
                            "containers": [{
                                "name": "ray-head",
                                "image": cluster.image,
                                "env": env,
                                "resources": gpu_resources,
                                "volumeMounts": volume_mounts,
                            }],
                            "volumes": volumes,
                        },
                    },
                },
                "workerGroupSpecs": [{
                    "groupName": "gpu",
                    "replicas": workers,
                    "minReplicas": workers,
                    "maxReplicas": workers,
                    "numOfHosts": 1,
                    "rayStartParams": {},
                    "template": {
                        "spec": {
                            "runtimeClassName": cluster.runtime_class_name,
                            "nodeSelector": node_selector,
                            "tolerations": tolerations,
                            "containers": [{
                                "name": "ray-worker",
                                "image": cluster.image,
                                "env": env,
                                "resources": gpu_resources,
                                "volumeMounts": volume_mounts,
                            }],
                            "volumes": volumes,
                        },
                    },
                }],
            },
        },
    });
    serde_json::from_value(body).expect("vLLM RayJob JSON is a valid DynamicObject")
}

/// Delete the campaign's vLLM RayJob + head Service at terminal phase. Idempotent.
async fn teardown_vllm_cluster(
    ctx: &Arc<Context>,
    ns: &str,
    campaign_name: &str,
) -> Result<(), Error> {
    let name = format!("vllm-{campaign_name}");
    let rayjobs = rayjob_api(ctx, ns);
    let services: Api<Service> = Api::namespaced(ctx.client.clone(), ns);
    if rayjobs.get_opt(&name).await?.is_some() {
        rayjobs.delete(&name, &DeleteParams::default()).await?;
        info!(campaign = %campaign_name, %name, "tore down vLLM RayJob");
    }
    if services.get_opt(&name).await?.is_some() {
        services.delete(&name, &DeleteParams::default()).await?;
    }
    Ok(())
}

/// Create a BenchmarkRun for a succeeded experiment if one doesn't exist yet.
/// targetRef points at the Experiment; `updateExperimentStatus` lets the
/// benchmark write the gate-based Keep/Discard verdict back onto the experiment,
/// which the campaign loop then reads as the decision.
async fn ensure_benchmark_run(
    ctx: &Arc<Context>,
    ns: &str,
    campaign: &ResearchCampaign,
    exp: &Experiment,
    suite: &str,
    profile: Option<&str>,
) -> Result<(), Error> {
    let exp_name = exp.name_any();
    let run_name = format!("bench-{exp_name}");
    let runs: Api<BenchmarkRun> = Api::namespaced(ctx.client.clone(), ns);
    if runs.get_opt(&run_name).await?.is_some() {
        return Ok(());
    }

    let owner = OwnerReference {
        api_version: "research.nixlab.io/v1alpha1".to_string(),
        kind: "ResearchCampaign".to_string(),
        name: campaign.name_any(),
        uid: campaign.uid().unwrap_or_default(),
        controller: Some(true),
        block_owner_deletion: Some(true),
    };

    let run = BenchmarkRun {
        metadata: ObjectMeta {
            name: Some(run_name),
            namespace: Some(ns.to_string()),
            labels: Some(BTreeMap::from([(
                CAMPAIGN_LABEL.to_string(),
                campaign.name_any(),
            )])),
            owner_references: Some(vec![owner]),
            ..Default::default()
        },
        spec: BenchmarkRunSpec {
            suite_ref: LocalObjectReference {
                name: suite.to_string(),
                namespace: None,
            },
            target_ref: TypedObjectReference {
                api_version: "research.nixlab.io/v1alpha1".to_string(),
                kind: "Experiment".to_string(),
                name: exp_name.clone(),
                namespace: Some(ns.to_string()),
            },
            mode: Default::default(),
            suspend: false,
            task_selector: None,
            runtime_profile_ref: profile.map(|p| LocalObjectReference {
                name: p.to_string(),
                namespace: None,
            }),
            budget: Default::default(),
            seed_matrix: None,
            output: exp
                .status
                .as_ref()
                .and_then(|s| s.workspace_path.clone())
                .map(|wp| BenchmarkRunOutput {
                    workspace_path: Some(wp),
                }),
            promotion_policy: PromotionPolicy {
                update_experiment_status: true,
                block_on_holdout_failure: false,
            },
            cleanup_policy: Default::default(),
            max_parallel_tasks: None,
        },
        status: None,
    };

    match runs.create(&PostParams::default(), &run).await {
        Ok(_) => {
            info!(campaign = %campaign.name_any(), experiment = %exp_name, "created BenchmarkRun")
        }
        Err(e) => warn!(%e, experiment = %exp_name, "failed to create BenchmarkRun"),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use athena_api::experiment::{CheckpointRef, ExperimentStatus};
    use athena_api::experiment_template::{
        ExperimentTemplateSpec, GitSource, MetricsSpec, ParameterSpec, SourceSpec,
    };
    use athena_api::research_campaign::ResearchCampaignSpec;

    fn exp_with(name: &str, phase: ExperimentPhase, metric: &str, value: f64) -> Experiment {
        let mut metrics = BTreeMap::new();
        metrics.insert(metric.to_string(), json!(value));
        Experiment {
            metadata: ObjectMeta {
                name: Some(name.to_string()),
                ..Default::default()
            },
            spec: ExperimentSpec {
                campaign_ref: "c".into(),
                hypothesis: String::new(),
                parameters: BTreeMap::new(),
                patch: None,
                checkpoint_policy: None,
                env: vec![],
            },
            status: Some(ExperimentStatus {
                phase,
                metrics,
                ..Default::default()
            }),
        }
    }

    fn obj(metric: &str, goal: ObjectiveGoal) -> ObjectiveSpec {
        ObjectiveSpec {
            metric: metric.to_string(),
            goal,
        }
    }

    #[test]
    fn pick_best_minimize_and_maximize() {
        let a = exp_with("a", ExperimentPhase::Succeeded, "loss", 2.0);
        let b = exp_with("b", ExperimentPhase::Succeeded, "loss", 0.5);
        let c = exp_with("c", ExperimentPhase::Succeeded, "loss", 1.0);
        let v: Vec<&Experiment> = vec![&a, &b, &c];
        assert_eq!(
            pick_best(&v, &obj("loss", ObjectiveGoal::Minimize)),
            Some(("b".into(), 0.5))
        );
        assert_eq!(
            pick_best(&v, &obj("loss", ObjectiveGoal::Maximize)),
            Some(("a".into(), 2.0))
        );
    }

    // Dedup machinery: the salt must move a candidate to a DIFFERENT science
    // point (both strategies), and science_key must ignore bookkeeping params —
    // otherwise every child would look unique and dedup would never fire.
    #[test]
    fn salt_varies_candidates_and_science_key_ignores_bookkeeping() {
        let t = template_with_default_lr(0.1);
        let best = BTreeMap::from([("lr".to_string(), json!(0.1))]);

        let (p0, _) = next_experiment(&t, Some(("c-000", &best)), 1, 0);
        let (p1, _) = next_experiment(&t, Some(("c-000", &best)), 1, 1);
        assert_ne!(
            science_key(&p0),
            science_key(&p1),
            "hill-climb salt must vary the point"
        );

        let (q0, _) = pbt_experiment(&t, Some(("c-000", &best)), 1.2, 2, 0);
        let (q1, _) = pbt_experiment(&t, Some(("c-000", &best)), 1.2, 2, 1);
        assert_ne!(
            science_key(&q0),
            science_key(&q1),
            "pbt salt bitmask must vary the point"
        );

        // Same science params, different bookkeeping → same key.
        let mut a = BTreeMap::from([("lr".to_string(), json!(0.1))]);
        let mut b = a.clone();
        a.insert("experimentIteration".into(), json!(3));
        b.insert("experimentIteration".into(), json!(7));
        a.insert("parentExperimentId".into(), json!("x"));
        b.insert("parentExperimentId".into(), Value::Null);
        assert_eq!(science_key(&a), science_key(&b));
    }

    #[test]
    fn pick_best_none_when_no_metric() {
        let a = exp_with("a", ExperimentPhase::Succeeded, "other", 2.0);
        assert_eq!(
            pick_best(&[&a], &obj("loss", ObjectiveGoal::Minimize)),
            None
        );
    }

    fn template_with_default_lr(lr: f64) -> ExperimentTemplate {
        ExperimentTemplate {
            metadata: ObjectMeta::default(),
            spec: ExperimentTemplateSpec {
                runtime_profile_ref: "rp".into(),
                source: SourceSpec {
                    git: GitSource {
                        url: "u".into(),
                        r#ref: "main".into(),
                        commit: None,
                    },
                },
                objective: obj("loss", ObjectiveGoal::Minimize),
                metrics: MetricsSpec::default(),
                parameter_schema: BTreeMap::from([(
                    "lr".to_string(),
                    ParameterSpec {
                        parameter_type: "number".into(),
                        default: Some(json!(lr)),
                        description: None,
                    },
                )]),
                defaults: BTreeMap::from([("lr".to_string(), json!(lr))]),
                dashboard: None,
                research_objective: None,
            },
            status: None,
        }
    }

    #[test]
    fn baseline_uses_defaults_and_stamps_bookkeeping() {
        let t = template_with_default_lr(0.1);
        let (params, hyp) = next_experiment(&t, None, 0, 0);
        assert_eq!(params.get("lr"), Some(&json!(0.1)));
        assert_eq!(params.get("experimentIteration"), Some(&json!(0)));
        assert_eq!(params.get("parentExperimentId"), Some(&Value::Null));
        assert!(hyp.contains("baseline"));
    }

    #[test]
    fn hillclimb_perturbs_from_best_params_not_defaults() {
        // Default lr is 0.1, but the best experiment has lr=0.5 — the climb must
        // perturb 0.5 (the best), not 0.1 (the default). This is the bug the live
        // validation caught.
        let t = template_with_default_lr(0.1);
        let best: BTreeMap<String, Value> = BTreeMap::from([("lr".to_string(), json!(0.5))]);
        let lr = |p: &BTreeMap<String, Value>| value_as_f64(p.get("lr").unwrap()).unwrap();
        // idx 1, lap 0 -> factor 1.5 -> 0.5*1.5 = 0.75
        let (params, hyp) = next_experiment(&t, Some(("c-002", &best)), 1, 0);
        assert!((lr(&params) - 0.75).abs() < 1e-9, "{:?}", params.get("lr"));
        assert_eq!(params.get("parentExperimentId"), Some(&json!("c-002")));
        assert!(hyp.contains("perturb lr"), "{hyp}");
        // idx 2, lap 1 -> factor 0.5 -> 0.5*0.5 = 0.25
        let (params2, _) = next_experiment(&t, Some(("c-002", &best)), 2, 0);
        assert!(
            (lr(&params2) - 0.25).abs() < 1e-9,
            "{:?}",
            params2.get("lr")
        );
    }

    fn campaign_pbt(population: Option<u32>, perturb: Option<f64>) -> ResearchCampaign {
        ResearchCampaign {
            metadata: ObjectMeta {
                name: Some("c".into()),
                namespace: Some("default".into()),
                ..Default::default()
            },
            spec: ResearchCampaignSpec {
                template_ref: "t".into(),
                concurrency: 1,
                budget: Default::default(),
                strategy: athena_api::research_campaign::StrategySpec {
                    strategy_type: "pbt".into(),
                },
                benchmark_suite_ref: None,
                benchmark_runtime_profile_ref: None,
                population_size: population,
                perturb_factor: perturb,
                inference_mesh: None,
                inference_cluster: None,
                canary: None,
            },
            status: None,
        }
    }

    #[test]
    fn pbt_perturbs_every_numeric_param_from_best_not_defaults() {
        // Template default lr is 0.1; the best ran lr=0.5. PBT must explore from
        // 0.5 (exploit the best), never the 0.1 default.
        let t = template_with_default_lr(0.1);
        let best: BTreeMap<String, Value> = BTreeMap::from([("lr".to_string(), json!(0.5))]);
        let lr = |p: &BTreeMap<String, Value>| value_as_f64(p.get("lr").unwrap()).unwrap();
        // One param (j=0). idx 2 -> (2+0)%2==0 -> up by 1.2 -> 0.5*1.2 = 0.6.
        let (params, hyp) = pbt_experiment(&t, Some(("c-002", &best)), 1.2, 2, 0);
        assert!((lr(&params) - 0.6).abs() < 1e-9, "{:?}", params.get("lr"));
        assert!((lr(&params) - 0.1).abs() > 1e-6, "must not use the default");
        assert_eq!(params.get("parentExperimentId"), Some(&json!("c-002")));
        assert!(hyp.contains("pbt"), "{hyp}");
        // idx 1 -> (1+0)%2==1 -> down by 1/1.2 -> 0.5/1.2.
        let (params2, _) = pbt_experiment(&t, Some(("c-002", &best)), 1.2, 1, 0);
        assert!(
            (lr(&params2) - 0.5 / 1.2).abs() < 1e-9,
            "{:?}",
            params2.get("lr")
        );
    }

    #[test]
    fn pbt_cold_start_when_no_best() {
        let t = template_with_default_lr(0.1);
        let (params, hyp) = pbt_experiment(&t, None, 1.2, 0, 0);
        assert_eq!(params.get("lr"), Some(&json!(0.1)));
        assert_eq!(params.get("parentExperimentId"), Some(&Value::Null));
        assert!(hyp.contains("cold start"), "{hyp}");
        assert!(pbt_checkpoint_policy(None).is_none());
    }

    #[test]
    fn pbt_checkpoint_policy_resumes_from_best_latest_checkpoint() {
        let mut best = exp_with("best", ExperimentPhase::Succeeded, "loss", 0.1);
        best.status.as_mut().unwrap().latest_checkpoint = Some(CheckpointRef {
            uri: "s3://ckpt/best/step-100".into(),
            step: Some(100),
            ..Default::default()
        });
        let cp = pbt_checkpoint_policy(Some(&best)).expect("best has a checkpoint");
        assert_eq!(cp.resume_from.as_deref(), Some("s3://ckpt/best/step-100"));
        // No checkpoint yet -> cold start (None).
        let no_ckpt = exp_with("warm", ExperimentPhase::Succeeded, "loss", 0.2);
        assert!(pbt_checkpoint_policy(Some(&no_ckpt)).is_none());
    }

    #[test]
    fn pbt_child_experiment_warm_starts_and_perturbs() {
        // End-to-end through build_experiment: the produced child Experiment must
        // carry resumeFrom from the best's latestCheckpoint AND perturbed params.
        let t = template_with_default_lr(0.1);
        let mut best_exp = exp_with("c-000", ExperimentPhase::Succeeded, "loss", 0.1);
        best_exp.spec.parameters = BTreeMap::from([("lr".to_string(), json!(0.5))]);
        best_exp.status.as_mut().unwrap().latest_checkpoint = Some(CheckpointRef {
            uri: "s3://ckpt/c-000/step-42".into(),
            ..Default::default()
        });
        let campaign = campaign_pbt(None, Some(1.2));

        let (params, hypothesis) =
            pbt_experiment(&t, Some(("c-000", &best_exp.spec.parameters)), 1.2, 2, 0);
        let cp = pbt_checkpoint_policy(Some(&best_exp));
        let child = build_experiment(&campaign, "c", "default", 2, params, hypothesis, cp);

        assert_eq!(
            child
                .spec
                .checkpoint_policy
                .as_ref()
                .and_then(|p| p.resume_from.as_deref()),
            Some("s3://ckpt/c-000/step-42"),
            "child must warm-start from the best's latest checkpoint"
        );
        let lr = value_as_f64(child.spec.parameters.get("lr").unwrap()).unwrap();
        assert!((lr - 0.6).abs() < 1e-9, "perturbed from best 0.5, got {lr}");
        assert!((lr - 0.1).abs() > 1e-6, "must not be the template default");
    }

    #[test]
    fn heuristic_child_has_no_checkpoint_policy() {
        // Requirement 4: the heuristic path is unchanged — children cold-start.
        let t = template_with_default_lr(0.1);
        let campaign = ResearchCampaign {
            metadata: ObjectMeta {
                name: Some("c".into()),
                ..Default::default()
            },
            spec: ResearchCampaignSpec {
                template_ref: "t".into(),
                concurrency: 1,
                budget: Default::default(),
                strategy: Default::default(),
                benchmark_suite_ref: None,
                benchmark_runtime_profile_ref: None,
                population_size: None,
                perturb_factor: None,
                inference_mesh: None,
                inference_cluster: None,
                canary: None,
            },
            status: None,
        };
        let (params, hypothesis) = next_experiment(&t, None, 0, 0);
        let child = build_experiment(&campaign, "c", "default", 0, params, hypothesis, None);
        assert!(child.spec.checkpoint_policy.is_none());
    }

    // ---- Canary gate ----

    /// Campaign spec with an optional canary and optional campaign-level suite.
    fn campaign_spec_with_canary(
        canary: Option<CanarySpec>,
        campaign_suite: Option<&str>,
    ) -> ResearchCampaignSpec {
        ResearchCampaignSpec {
            template_ref: "t".into(),
            concurrency: 1,
            budget: Default::default(),
            strategy: Default::default(),
            benchmark_suite_ref: campaign_suite.map(str::to_string),
            benchmark_runtime_profile_ref: None,
            population_size: None,
            perturb_factor: None,
            inference_mesh: None,
            inference_cluster: None,
            canary,
        }
    }

    fn canary_spec(suite: Option<&str>) -> CanarySpec {
        CanarySpec {
            parameters: json!({ "total_timesteps": 2_000_000 }),
            benchmark_suite_ref: suite.map(str::to_string),
            max_duration: None,
        }
    }

    #[test]
    fn gate_without_canary_is_always_unblock() {
        // Requirement 5: campaigns without spec.canary behave exactly as today,
        // whatever junk the other inputs carry.
        let spec = campaign_spec_with_canary(None, Some("suite"));
        for phase in [
            None,
            Some(ExperimentPhase::Pending),
            Some(ExperimentPhase::Running),
            Some(ExperimentPhase::Succeeded),
            Some(ExperimentPhase::Failed),
        ] {
            for decision in [None, Some(ExperimentDecision::Discard)] {
                assert_eq!(
                    canary_gate(&spec, phase.as_ref(), decision.as_ref()),
                    CanaryGateAction::Unblock
                );
            }
        }
    }

    #[test]
    fn gate_creates_canary_once_then_holds_while_nonterminal() {
        let spec = campaign_spec_with_canary(Some(canary_spec(None)), None);
        // Not created yet.
        assert_eq!(
            canary_gate(&spec, None, None),
            CanaryGateAction::CreateCanary
        );
        // Created (even before it has any status → Pending) through Running: hold.
        for phase in [
            ExperimentPhase::Pending,
            ExperimentPhase::Preparing,
            ExperimentPhase::Running,
        ] {
            assert_eq!(
                canary_gate(&spec, Some(&phase), None),
                CanaryGateAction::Hold,
                "{phase:?}"
            );
        }
    }

    #[test]
    fn gate_ungated_canary_passes_on_success_alone() {
        // No suite on the canary or the campaign: Succeeded IS the gate.
        let spec = campaign_spec_with_canary(Some(canary_spec(None)), None);
        assert_eq!(
            canary_gate(&spec, Some(&ExperimentPhase::Succeeded), None),
            CanaryGateAction::Unblock
        );
    }

    #[test]
    fn gate_failed_canary_is_terminal() {
        let spec = campaign_spec_with_canary(Some(canary_spec(None)), None);
        for phase in [ExperimentPhase::Failed, ExperimentPhase::Error] {
            assert_eq!(
                canary_gate(&spec, Some(&phase), None),
                CanaryGateAction::CanaryFailed,
                "{phase:?}"
            );
        }
    }

    #[test]
    fn gate_with_suite_waits_for_benchmark_verdict() {
        // Suite from the campaign (canary falls back to it): Succeeded alone is
        // NOT enough — the gate opens on Keep, closes forever on Discard, and
        // holds on no-verdict-yet or NeedsReview.
        let spec = campaign_spec_with_canary(Some(canary_spec(None)), Some("gates"));
        let succeeded = ExperimentPhase::Succeeded;
        assert_eq!(
            canary_gate(&spec, Some(&succeeded), None),
            CanaryGateAction::Hold
        );
        assert_eq!(
            canary_gate(
                &spec,
                Some(&succeeded),
                Some(&ExperimentDecision::NeedsReview)
            ),
            CanaryGateAction::Hold
        );
        assert_eq!(
            canary_gate(&spec, Some(&succeeded), Some(&ExperimentDecision::Keep)),
            CanaryGateAction::Unblock
        );
        assert_eq!(
            canary_gate(&spec, Some(&succeeded), Some(&ExperimentDecision::Discard)),
            CanaryGateAction::CanaryFailed
        );
    }

    #[test]
    fn gate_canary_suite_override_gates_without_campaign_suite() {
        // The canary can carry its own (cheaper) suite even when the campaign
        // has none — the gate must still wait for the verdict.
        let spec = campaign_spec_with_canary(Some(canary_spec(Some("canary-gates"))), None);
        let succeeded = ExperimentPhase::Succeeded;
        assert_eq!(
            canary_gate(&spec, Some(&succeeded), None),
            CanaryGateAction::Hold
        );
        assert_eq!(
            canary_gate(&spec, Some(&succeeded), Some(&ExperimentDecision::Keep)),
            CanaryGateAction::Unblock
        );
    }

    #[test]
    fn canary_state_strings_cover_the_lifecycle() {
        use CanaryGateAction::*;
        assert_eq!(canary_state(CreateCanary, None), "pending");
        assert_eq!(
            canary_state(Hold, Some(&ExperimentPhase::Pending)),
            "pending"
        );
        assert_eq!(
            canary_state(Hold, Some(&ExperimentPhase::Running)),
            "running"
        );
        // Succeeded but the BenchmarkRun hasn't ruled: gate work still running.
        assert_eq!(
            canary_state(Hold, Some(&ExperimentPhase::Succeeded)),
            "running"
        );
        assert_eq!(
            canary_state(Unblock, Some(&ExperimentPhase::Succeeded)),
            "passed"
        );
        assert_eq!(
            canary_state(CanaryFailed, Some(&ExperimentPhase::Failed)),
            "failed"
        );
    }

    #[test]
    fn canary_seed_strips_only_canary_overrides() {
        let params = BTreeMap::from([
            ("alive".to_string(), json!(12.0)),
            ("total_timesteps".to_string(), json!(2_000_000)),
        ]);
        let overrides = json!({ "total_timesteps": 2_000_000 });
        // Any seed with a canary configured: the override key is stripped
        // (falls back to template defaults downstream), science params
        // survive. Unconditional because the leak is generational — a child
        // that inherited the canary's cheap budget would re-leak it as the
        // next seed.
        let seeded = canary_seed_params(&params, Some(&overrides));
        assert!(!seeded.contains_key("total_timesteps"));
        assert_eq!(seeded.get("alive"), Some(&json!(12.0)));
        // No canary configured: identity.
        let seeded = canary_seed_params(&params, None);
        assert_eq!(seeded, params);
    }

    #[test]
    fn canary_seeded_child_reverts_to_template_budget() {
        // End-to-end through next_experiment: template default 15M, canary ran
        // at 2M, the budgeted child must climb from the canary's science but
        // train at 15M.
        let mut t = template_with_default_lr(0.1);
        t.spec
            .defaults
            .insert("total_timesteps".to_string(), json!(15_000_000));
        let canary_params = BTreeMap::from([
            ("lr".to_string(), json!(0.2)),
            ("total_timesteps".to_string(), json!(2_000_000)),
        ]);
        let seeded = canary_seed_params(
            &canary_params,
            Some(&json!({ "total_timesteps": 2_000_000 })),
        );
        let (params, _) = next_experiment(&t, Some(("c-canary", &seeded)), 1, 0);
        assert_eq!(params.get("total_timesteps"), Some(&json!(15_000_000)));
        assert_eq!(params.get("parentExperimentId"), Some(&json!("c-canary")));
    }

    #[test]
    fn canary_parameters_deep_merge_over_defaults() {
        let base = BTreeMap::from([
            ("lr".to_string(), json!(0.001)),
            ("total_timesteps".to_string(), json!(25_000_000)),
            ("opt".to_string(), json!({ "beta1": 0.9, "beta2": 0.999 })),
        ]);
        // Canary wins; untouched defaults survive, nested siblings survive.
        let merged = merge_canary_parameters(
            base.clone(),
            &json!({ "total_timesteps": 2_000_000, "opt": { "beta1": 0.5 } }),
        );
        assert_eq!(merged.get("total_timesteps"), Some(&json!(2_000_000)));
        assert_eq!(merged.get("lr"), Some(&json!(0.001)));
        assert_eq!(
            merged.get("opt"),
            Some(&json!({ "beta1": 0.5, "beta2": 0.999 }))
        );
        // Null / non-object overrides leave the defaults alone.
        assert_eq!(merge_canary_parameters(base.clone(), &Value::Null), base);
        assert_eq!(merge_canary_parameters(base.clone(), &json!(42)), base);
    }

    #[test]
    fn canary_experiment_is_named_labeled_and_merged() {
        let t = template_with_default_lr(0.1);
        let spec = campaign_spec_with_canary(Some(canary_spec(None)), None);
        let campaign = ResearchCampaign {
            metadata: ObjectMeta {
                name: Some("c".into()),
                namespace: Some("default".into()),
                ..Default::default()
            },
            spec,
            status: None,
        };
        let canary = campaign.spec.canary.clone().unwrap();
        let exp = build_canary_experiment(&campaign, "c", "default", &t, &canary);

        assert_eq!(exp.metadata.name.as_deref(), Some("c-canary"));
        let labels = exp.metadata.labels.as_ref().unwrap();
        assert_eq!(labels.get(CANARY_LABEL).map(String::as_str), Some("true"));
        assert_eq!(labels.get(CAMPAIGN_LABEL).map(String::as_str), Some("c"));
        // Template default survives, canary override lands, bookkeeping stamped.
        assert_eq!(exp.spec.parameters.get("lr"), Some(&json!(0.1)));
        assert_eq!(
            exp.spec.parameters.get("total_timesteps"),
            Some(&json!(2_000_000))
        );
        assert_eq!(
            exp.spec.parameters.get("experimentIteration"),
            Some(&json!(0))
        );
        assert_eq!(
            exp.spec.parameters.get("parentExperimentId"),
            Some(&Value::Null)
        );
        assert!(exp.spec.hypothesis.contains("canary gate"));
    }
}
