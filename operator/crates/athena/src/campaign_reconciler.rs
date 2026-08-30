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
    CheckpointPolicy, DerivationRelation, Experiment, ExperimentDecision, ExperimentLineage,
    ExperimentPhase, ExperimentSpec, ParameterDelta,
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
/// Derivation edge, mirrored from `spec.lineage.parent` so the search tree is
/// queryable. Kubernetes selects on labels only, never annotations.
const PARENT_LABEL: &str = "research.nixlab.io/parent";
/// Generation depth, mirrored from `spec.lineage.generation`.
const GENERATION_LABEL: &str = "research.nixlab.io/generation";
/// Node role, mirrored from `spec.lineage.relation`.
const ROLE_LABEL: &str = "research.nixlab.io/role";
/// Prefix stamped on a control-slot experiment's hypothesis. Control runs are
/// re-measurements of the incumbent, not search points, so they must be
/// identifiable when computing the seed-noise estimate.
const CONTROL_HYPOTHESIS_PREFIX: &str = "control: re-measure incumbent";

/// True for an experiment generated as a control-slot re-measurement.
///
/// Prefers the typed lineage relation. The prose fallback exists only for
/// experiments created before `spec.lineage` existed — without it, upgrading
/// the operator mid-campaign would silently discard the accumulated sigma
/// sample and freeze the incumbent.
fn is_control_experiment(e: &Experiment) -> bool {
    match &e.spec.lineage {
        Some(l) => l.relation == DerivationRelation::Remeasure,
        None => e.spec.hypothesis.starts_with(CONTROL_HYPOTHESIS_PREFIX),
    }
}

/// True for a founding-cohort experiment: baseline, seed spread, or canary —
/// anything with no parent experiment. Prefers the typed lineage; falls back
/// to the bookkeeping parameter for pre-lineage experiments.
fn is_cold_start(e: &Experiment) -> bool {
    match &e.spec.lineage {
        Some(l) => l.parent.is_none(),
        None => e
            .spec
            .parameters
            .get("parentExperimentId")
            .is_none_or(Value::is_null),
    }
}

/// True when an experiment is in a terminal phase.
fn is_terminal(e: &Experiment) -> bool {
    matches!(
        e.status.as_ref().map(|s| &s.phase),
        Some(ExperimentPhase::Succeeded | ExperimentPhase::Failed | ExperimentPhase::Error)
    )
}

/// A warm-started child reporting its parent's objective BIT-IDENTICALLY did
/// not train: it resumed the parent's final checkpoint and re-reported the
/// parent's result. Measured live in v70: five children of -001 finished in
/// ~35s (parents took ~80min), every one returning 3849.359637757268.
///
/// Echoes are not measurements. In the argmax they add a duplicate point that
/// can masquerade as a search result; in the control sample they drive sigma
/// toward 0 and turn the selection gate reckless. The seed-rotation design
/// makes a legit bit-identical re-run impossible across generations (each
/// generation draws a fresh seed), so exact f64 equality with the parent is
/// sufficient evidence.
fn is_echo(e: &Experiment, by_name: &BTreeMap<String, &Experiment>, metric: &str) -> bool {
    // Only warm-started children can echo.
    if e.spec
        .checkpoint_policy
        .as_ref()
        .and_then(|p| p.resume_from.as_ref())
        .is_none()
    {
        return false;
    }
    let parent_name = e
        .spec
        .lineage
        .as_ref()
        .and_then(|l| l.parent.clone())
        .or_else(|| {
            e.spec
                .parameters
                .get("parentExperimentId")
                .and_then(Value::as_str)
                .map(str::to_string)
        });
    let Some(parent_name) = parent_name else {
        return false;
    };
    let Some(parent) = by_name.get(&parent_name) else {
        return false;
    };
    match (objective_value(e, metric), objective_value(parent, metric)) {
        (Some(child), Some(par)) => child == par,
        _ => false,
    }
}
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
    // ECHO FILTER. A warm-started child that reports its parent's objective
    // bit-identically never trained — it resumed a completed checkpoint and
    // re-reported the parent's result. Measured live in v70: five children of
    // -001 finished in ~35s each (their parent took ~80min) and every one
    // returned 3849.359637757268, the parent's exact score; perturbations
    // never entered a training loop because there was none. Echoes are not
    // measurements: in the argmax they are duplicate points posing as search
    // results, and in the control sample they collapse sigma toward 0, which
    // makes the selection gate reckless. They are excluded from BOTH pools.
    // (They still count in succeeded/failed tallies — the Job did run.)
    let by_name: BTreeMap<String, &Experiment> =
        exps.items.iter().map(|e| (e.name_any(), e)).collect();
    let echo_names: std::collections::HashSet<String> = completed
        .iter()
        .filter(|e| is_echo(e, &by_name, &objective.metric))
        .map(|e| e.name_any())
        .collect();
    if !echo_names.is_empty() {
        warn!(
            campaign = %name,
            echoes = ?echo_names,
            "excluding echo experiments from selection: objective bit-identical to warm-start parent (no training occurred)"
        );
    }
    let completed: Vec<&Experiment> = completed
        .into_iter()
        .filter(|e| !echo_names.contains(&e.name_any()))
        .collect();

    // The canary is deliberately NOT excluded here: it counts like any other
    // succeeded experiment for bestExperiment/bestObjective. Its objective was
    // produced under the same template/metric, so it is comparable in kind —
    // just cheaper, so a real experiment should overtake it quickly. Excluding
    // it would also leave hill-climb/PBT with no seed right after the gate opens.
    let best_observed = pick_best(&completed, objective);

    // Control-slot runs: experiments whose hypothesis marks them as an
    // incumbent re-measurement rather than a search point. Repeated scores of
    // the SAME config are the only unconfounded seed-noise sample available.
    // Control runs are identified by the TYPED lineage relation. This used to
    // prefix-match the free-text hypothesis, which made the entire selection
    // gate depend on a prose string: editing it emptied the sigma sample,
    // sigma_upper_bound returned None, beats_incumbent returned false
    // unconditionally, and the incumbent froze forever with no error anywhere.
    // Falls back to the legacy prefix so experiments created before this field
    // existed still contribute their sigma sample.
    let control_scores: Vec<f64> = completed
        .iter()
        .filter(|e| is_control_experiment(e))
        .filter_map(|e| objective_value(e, &objective.metric))
        .collect();
    // Reported for observability; the GATE uses the upper confidence limit so a
    // poorly-determined sigma cannot make the threshold falsely tight.
    let sigma = seed_noise_sigma(&control_scores);
    let sigma_gate = sigma_upper_bound(&control_scores);
    let incumbent_remeasured = control_scores.last().copied();

    // The incumbent only changes when the challenger clears the measured noise
    // floor. Below that, `best_observed` is indistinguishable from a lucky seed,
    // and adopting it would warm-start the entire next generation from noise.
    // The previously-adopted parent, carried in status across reconciles. This
    // is what "holding the incumbent" means: the loop keeps warm-starting from
    // the same config until something genuinely clears the noise floor.
    let prior_incumbent: Option<(String, f64)> = campaign
        .status
        .as_ref()
        .and_then(|s| s.best_experiment.clone())
        .and_then(|n| {
            completed
                .iter()
                .find(|e| e.name_any() == n)
                .and_then(|e| objective_value(e, &objective.metric))
                .map(|v| (n, v))
        });

    // COLD-START PEERS ADVANCE FREELY. The sigma gate exists to stop
    // warm-started challengers from displacing the incumbent on an
    // uncalibrated comparison. It must NOT gate the founding cohort: cold
    // starts are independent seeds, and the first-generation champion is the
    // argmax of ALL of them, whenever they finish. Measured live in v85:
    // 003 finished first (09:37) walking BACKWARD (-0.134 m/s) and was
    // adopted; 001 finished 3 minutes later walking FORWARD (+0.026 m/s, the
    // campaign argmax) and the gate — correctly refusing an uncalibrated
    // displacement — locked the backward-walker in and discarded the best
    // run of the campaign. Order of completion is scheduling noise; argmax of
    // completed cold starts is order-independent.
    let best_observed_is_cold = best_observed
        .as_ref()
        .and_then(|(bn, _)| by_name.get(bn))
        .is_some_and(|e| is_cold_start(e));
    let prior_incumbent_is_cold = prior_incumbent
        .as_ref()
        .and_then(|(n, _)| by_name.get(n))
        .is_some_and(|e| is_cold_start(e));
    let founding_peers = best_observed_is_cold && prior_incumbent_is_cold;

    let best = match (&best_observed, &prior_incumbent) {
        (Some((bn, bv)), Some((inc_name, inc_v))) if bn != inc_name => {
            if founding_peers {
                info!(
                    challenger = %bn, challenger_score = bv, incumbent = %inc_name,
                    "founding cohort peer finished with a better score; champion advances (sigma gate applies to warm-started challengers only)"
                );
                best_observed.clone()
            } else {
                // Prefer the control slot's fresh re-measurement of the incumbent
                // over its original (max-selected, upward-biased) score.
                let baseline = incumbent_remeasured.unwrap_or(*inc_v);
                if beats_incumbent(*bv, baseline, sigma_gate, &objective.goal) {
                    info!(
                        challenger = %bn, challenger_score = bv, incumbent = %inc_name,
                        incumbent_score = baseline, sigma = ?sigma,
                        "incumbent displaced: challenger cleared the seed-noise floor"
                    );
                    best_observed.clone()
                } else {
                    info!(
                        challenger = %bn, challenger_score = bv, incumbent = %inc_name,
                        incumbent_score = baseline, sigma = ?sigma, sigma_gate = ?sigma_gate,
                        "holding incumbent: challenger is within seed noise, so its lead \
                         is not distinguishable from a lucky seed"
                    );
                    prior_incumbent.clone()
                }
            }
        }
        // No prior incumbent (first generation) — adopt the observed best so the
        // loop has something to warm-start from.
        _ => best_observed.clone(),
    };

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
        // Echoes are excluded from `completed` above and so escape the
        // Keep/Discard loop entirely, leaving them decisionless forever.
        // Stamp them Discard explicitly: a warm-started child that
        // re-reported its parent's metric is not a candidate for anything.
        for e in &exps.items {
            if !echo_names.contains(&e.name_any()) {
                continue;
            }
            if e.status.as_ref().and_then(|s| s.decision.clone()).is_none() {
                let patch = json!({ "status": { "decision": ExperimentDecision::Discard } });
                experiments
                    .patch_status(
                        &e.name_any(),
                        &PatchParams::apply(MANAGER),
                        &Patch::Merge(&patch),
                    )
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
            // Cross-campaign seed (spec.seedExperimentRef): while the campaign
            // has no in-campaign best yet, seed the search from the referenced
            // experiment's parameters and (for PBT) its checkpoint instead of
            // cold-starting from template defaults. This is how a ResearchDrive
            // carries a prior branch's incumbent into a consolidation campaign
            // (the DAG's converge edge). A missing/unreadable seed is non-fatal:
            // warn and cold-start, never wedge the campaign.
            let external_seed: Option<Experiment> = if best_exp.is_none() {
                match &campaign.spec.seed_experiment_ref {
                    Some(seed_name) => match experiments.get_opt(seed_name).await {
                        Ok(Some(e)) => Some(e),
                        Ok(None) => {
                            warn!(campaign = %name, seed = %seed_name,
                                "seedExperimentRef experiment not found; cold-starting from template defaults");
                            None
                        }
                        Err(e) => {
                            warn!(campaign = %name, seed = %seed_name, %e,
                                "failed to fetch seedExperimentRef experiment; cold-starting");
                            None
                        }
                    },
                    None => None,
                }
            } else {
                None
            };
            let external_seed_name = external_seed.as_ref().map(|e| e.name_any());
            // The effective seed experiment: an in-campaign best always wins;
            // the external seed applies only until one exists.
            let seed_exp: Option<&Experiment> = best_exp.or(external_seed.as_ref());
            // A canary seed's SCIENCE params are a valid hill-climb/PBT start,
            // but its cheapness must not leak into budgeted children: keys the
            // canary explicitly overrode (spec.canary.parameters, e.g.
            // total_timesteps: 2M) are stripped so they revert to template
            // defaults. Caught live: spot-recover-v70-001 ran at the canary's
            // 2M budget instead of the template's 15M.
            let seed_params: Option<BTreeMap<String, Value>> = seed_exp.map(|e| {
                canary_seed_params(
                    &e.spec.parameters,
                    campaign.spec.canary.as_ref().map(|c| &c.parameters),
                )
            });
            // The seed's identity for lineage/parent bookkeeping: the in-campaign
            // best's name, or the external seed experiment's name when seeding
            // cross-campaign.
            let seed_name: Option<String> = best
                .as_ref()
                .map(|(bn, _)| bn.clone())
                .or(external_seed_name);
            let best_ctx = seed_name.as_deref().zip(seed_params.as_ref());
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
            // CONTROL SLOT. One slot per generation re-runs the incumbent's
            // EXACT config on a fresh seed instead of exploring. It costs a
            // third of a population-3 budget and pays for itself three ways:
            // it supplies the live sigma that calibrates `beats_incumbent`
            // (without it every threshold is uncalibrated), it re-measures the
            // incumbent unbiasedly so progress can be reported from something
            // that is allowed to go DOWN, and the gap between it and
            // `best_objective` is the maximization bias made visible.
            //
            // 2 candidates + 1 control beats 3 candidates: maximization bias
            // grows with the number of arms, so the third arm actively costs
            // selection honesty on top of buying no measurement.
            // ONE control per GENERATION, keyed off the absolute index, not the
            // position in this reconcile batch.
            //
            // The previous `Some(total)` marked slot 0 of EVERY batch. Once the
            // population saturates, slots free one at a time, so want==1 and
            // that single slot was always the control -- spot-walk-pbt-v72
            // generated 9 controls and ZERO candidates after generation 1, and
            // the search did nothing while looking healthy.
            //
            // idx % populationSize == 0 is stateless and reconcile-count
            // independent: exactly one control per generation of `pop`.
            let control_pop = campaign.spec.population_size.filter(|p| *p > 0);

            for i in 0..want.min(budget_left) {
                let idx = total + i;
                let is_control_slot =
                    is_pbt && best_ctx.is_some() && control_pop.is_some_and(|pop| idx % pop == 0);
                if is_control_slot {
                    // Replicate of the incumbent: same params, no perturbation,
                    // and deliberately exempt from the science_key dedup below —
                    // a duplicate point is the entire purpose of a control.
                    let (bn, bp) = best_ctx.expect("control slot requires an incumbent");
                    let mut params = bp.clone();
                    params.insert("experimentIteration".into(), json!(idx));
                    params.insert("parentExperimentId".into(), json!(bn));
                    let lineage = ExperimentLineage {
                        relation: DerivationRelation::Remeasure,
                        parent: Some(bn.to_string()),
                        parent_uid: seed_exp.and_then(|e| e.uid()),
                        generation: control_pop.map(|pop| idx / pop),
                        strategy: Some(campaign.spec.strategy.strategy_type.clone()),
                        perturbations: Vec::new(),
                        salt: None,
                    };
                    let exp = build_experiment(
                        &campaign,
                        &name,
                        &ns,
                        idx,
                        params,
                        lineage.describe(),
                        warm_start_policy(seed_exp),
                        Some(lineage),
                    );
                    match experiments.create(&PostParams::default(), &exp).await {
                        Ok(_) => created += 1,
                        Err(e) => warn!(%e, "failed to create control experiment"),
                    }
                    continue;
                }
                let generate = |salt: u32| {
                    if is_pbt {
                        let (params, hypothesis) =
                            pbt_experiment(&template, best_ctx, perturb_factor, idx, salt);
                        (params, hypothesis, warm_start_policy(seed_exp))
                    } else {
                        let (params, hypothesis) = next_experiment(&template, best_ctx, idx, salt);
                        // Heuristic children inherit WEIGHTS as well as parameters.
                        // They already record `parentExperimentId` and perturb from
                        // the best run's parameters, but this branch used to return
                        // None, so the parentage was bookkeeping only and every
                        // generation silently cold-started. Measured on
                        // multi-robot-curriculum-drive-spot-stance-cpu-longtrain:
                        // three experiments, each 2,002,944 steps, all reporting
                        // warm_started 0 while carrying a parentExperimentId — 4M
                        // steps of "curriculum" that were independent random
                        // restarts. Control slots `continue` above and never reach
                        // here, so cold baselines stay cold and the sigma
                        // comparison is unaffected.
                        (params, hypothesis, warm_start_policy(seed_exp))
                    }
                };
                // Dedup only applies when there is a best to perturb from — baselines
                // have nothing to vary. Bounded re-rolls; if the local lattice is
                // exhausted, accept the duplicate as a labeled replicate (liveness).
                let mut chosen = generate(0);
                let mut chosen_salt = 0u32;
                let mut replicated = false;
                if best_ctx.is_some() {
                    let mut deduped = false;
                    for salt in 0..=MAX_REROLLS {
                        let candidate = generate(salt);
                        if !seen.contains(&science_key(&candidate.0)) {
                            chosen = candidate;
                            chosen_salt = salt;
                            deduped = true;
                            break;
                        }
                    }
                    if !deduped {
                        replicated = true;
                        chosen.1 =
                            format!("{} [replicate: local search space exhausted]", chosen.1);
                    }
                }
                seen.insert(science_key(&chosen.0));
                let (params, hypothesis, checkpoint_policy) = chosen;
                // Reconstruct the structured record from the parent/child
                // parameter diff, so lineage is exact rather than parsed back
                // out of the prose we just formatted.
                let relation = if replicated {
                    DerivationRelation::Replicate
                } else if best_ctx.is_none() {
                    if idx == 0 {
                        DerivationRelation::Baseline
                    } else {
                        DerivationRelation::Seed
                    }
                } else {
                    DerivationRelation::Perturb
                };
                let lineage = ExperimentLineage {
                    relation,
                    parent: best_ctx.map(|(bn, _)| bn.to_string()),
                    parent_uid: seed_exp.and_then(|e| e.uid()),
                    generation: control_pop.map(|pop| idx / pop),
                    strategy: Some(campaign.spec.strategy.strategy_type.clone()),
                    perturbations: parameter_deltas(best_ctx.map(|(_, bp)| bp), &params),
                    salt: Some(chosen_salt),
                };
                let exp = build_experiment(
                    &campaign,
                    &name,
                    &ns,
                    idx,
                    params,
                    hypothesis,
                    checkpoint_policy,
                    Some(lineage),
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
        // NOTE: max over N noisy draws — monotone by construction, NOT progress.
        // `incumbentRemeasured` is the honest signal; it is allowed to go down.
        "bestObjective": best_observed.as_ref().map(|b| b.1),
        "incumbentRemeasured": incumbent_remeasured,
        "seedNoiseSigma": sigma,
        "controlRuns": control_scores.len() as u32,
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

    // Author this campaign's write-up once it is terminal. Drive-owned
    // campaigns are written up by the drive when it folds them; this covers
    // STANDALONE campaigns, which otherwise terminate leaving nothing but raw
    // metrics and whatever a human remembered to type.
    //
    // Keyed on terminal phase plus create-if-absent rather than on a
    // transition edge, so a campaign that went terminal before this controller
    // existed still gets written up on its next reconcile, and a retry after a
    // failed generation still lands.
    if let Some(proposer) = campaign.spec.proposer.as_ref() {
        if at_budget || canary_dead {
            // Re-read: the copy in hand predates the status patch above, and the
            // write-up must see the terminal numbers it just published.
            let fresh = campaigns
                .get(&name)
                .await
                .unwrap_or_else(|_| (*campaign).clone());
            match crate::drive_reconciler::author_report(proposer, None, &ctx, &ns, &fresh).await {
                Ok(true) => info!(campaign = %name, "authored research report"),
                Ok(false) => {}
                // Never fail the reconcile over a write-up: the campaign's
                // result is already durable in status, and the next pass retries.
                Err(e) => warn!(campaign = %name, error = %e, "report authoring failed"),
            }
        }
    }

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
///
/// Raw argmax. This is a BIASED estimator of the best config: E[max of N noisy
/// draws] >= max of the true means, and the gap grows with N, so on its own it
/// both overstates the winner and can pick a worse config. With this stack's
/// measured seed noise (sd ~740 on mean ~4026 from three byte-identical runs) a
/// 3% true effect is selected correctly only Phi(0.115) ~ 55% of the time — a
/// coin flip. Callers must gate it through [`select_parent`], which requires a
/// margin over the incumbent before acting on it.
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

/// Sample standard deviation of the control slot's repeated runs of the SAME
/// config — the live seed-noise estimate that calibrates selection.
///
/// None below two samples (sd is undefined at n=1). Uses the n-1 denominator:
/// at these sample sizes the population form is biased low, and a low sigma
/// makes the selection gate too eager, which is the failure direction we care
/// about.
fn seed_noise_sigma(control_scores: &[f64]) -> Option<f64> {
    if control_scores.len() < 2 {
        return None;
    }
    let n = control_scores.len() as f64;
    let mean = control_scores.iter().sum::<f64>() / n;
    let var = control_scores
        .iter()
        .map(|v| (v - mean).powi(2))
        .sum::<f64>()
        / (n - 1.0);
    let sd = var.sqrt();
    sd.is_finite().then_some(sd)
}

/// Minimum control runs before the selection gate may fire at all.
///
/// At n=2 the sd has 1 degree of freedom and its 95% chi-square interval spans
/// more than an order of magnitude — a threshold built on it is meaningless.
/// Below this the gate holds the incumbent rather than acting on a number it
/// cannot trust.
const MIN_CONTROL_RUNS: usize = 3;

/// One-sided 80% upper confidence limit on sigma: `s * sqrt(df / chi2_{0.20,df})`.
///
/// A sample sd from a handful of runs is biased LOW and wildly scattered (the
/// measured s=740 from n=3 carries a 95% interval of roughly [385, 4651]).
/// Using the point estimate as a selection threshold is therefore
/// over-confident exactly when the estimate is worst. Taking the upper limit
/// makes the gate strict while sigma is poorly determined and lets it relax
/// naturally as control runs accumulate — fail closed under uncertainty, the
/// same principle as requiring a sigma at all.
///
/// Factors are chi-square quantiles for df = n-1 (exact for df<=5; the tail
/// uses Wilson-Hilferty, which agrees to <1% there). df=1 is deliberately
/// absent: MIN_CONTROL_RUNS makes it unreachable, and W-H is poor at df=1.
fn sigma_upper_bound(control_scores: &[f64]) -> Option<f64> {
    if control_scores.len() < MIN_CONTROL_RUNS {
        return None;
    }
    let s = seed_noise_sigma(control_scores)?;
    const FACTORS: [f64; 8] = [
        2.1169, // n=3
        1.7276, // n=4
        1.5576, // n=5
        1.4610, // n=6
        1.3949, // n=7
        1.3509, // n=8
        1.3178, // n=9
        1.2918, // n=10
    ];
    let idx = control_scores.len() - MIN_CONTROL_RUNS;
    // Beyond the table the factor keeps shrinking toward 1; holding the last
    // tabulated value is conservative (slightly wider than truth), which is the
    // safe direction for a gate.
    let factor = FACTORS.get(idx).copied().unwrap_or(1.2918);
    Some(s * factor)
}

/// Whether a challenger should displace the incumbent parent.
///
/// The no-change default: keep the incumbent unless the challenger beats it by
/// more than one sigma of measured seed noise. Without this the loop hill-climbs
/// on whichever seed got lucky and then warm-starts the whole next generation
/// from it.
///
/// With the current effect size (epsilon ~0.2) this will rarely fire. That is
/// the honest outcome, not a bug — it means the perturbation is too small to
/// resolve, and the fix is a wider perturbation (required seeds scale ~1/eps^2),
/// not a looser threshold.
///
/// Until the control slot has produced two runs there is no sigma, and we hold
/// the incumbent rather than guess a threshold: acting on an uncalibrated
/// comparison is exactly the behaviour being removed.
fn beats_incumbent(
    challenger: f64,
    incumbent: f64,
    sigma: Option<f64>,
    goal: &ObjectiveGoal,
) -> bool {
    let Some(sigma) = sigma else { return false };
    match goal {
        ObjectiveGoal::Maximize => challenger > incumbent + sigma,
        ObjectiveGoal::Minimize => challenger < incumbent - sigma,
    }
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
/// Exactly which numeric parameters differ between parent and child.
///
/// Derived from the actual parameter maps rather than from the perturbation
/// code, so it stays correct no matter which strategy produced the child —
/// including the canary-override reversion, which changes params in a way the
/// perturbation loop never sees.
fn parameter_deltas(
    parent: Option<&BTreeMap<String, Value>>,
    child: &BTreeMap<String, Value>,
) -> Vec<ParameterDelta> {
    let mut out = Vec::new();
    for (k, v) in child {
        if BOOKKEEPING.contains(&k.as_str()) {
            continue;
        }
        let Some(to) = value_as_f64(v) else { continue };
        let from = parent.and_then(|p| p.get(k)).and_then(value_as_f64);
        if from.is_some_and(|f| (f - to).abs() < f64::EPSILON) {
            continue; // unchanged
        }
        // Only meaningful for a multiplicative step from a non-zero base.
        let factor = from.filter(|f| f.abs() > f64::EPSILON).map(|f| to / f);
        out.push(ParameterDelta {
            param: k.clone(),
            from,
            to,
            factor,
        });
    }
    out
}

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
        // Cold start: there is no parent to exploit, but returning the template
        // defaults unchanged makes the whole first generation N identical runs
        // — populationSize GPUs spending a full generation to sample one point.
        // Real PBT seeds a SPREAD. Child 0 stays the exact baseline (so the
        // campaign always has an unperturbed reference to compare against);
        // children 1..N fan out from it, each stepping a different subset of
        // params, reusing the same perturb_factor as the explore step.
        None if idx == 0 => "pbt cold start: baseline (template defaults)".to_string(),
        None => {
            let down = 1.0 / perturb_factor;
            let keys = numeric_params(&params);
            let mut spread = Vec::new();
            for (j, key) in keys.iter().enumerate() {
                // Vary which params move and in which direction per child, so
                // children 1..N are distinct from the baseline AND each other.
                if (idx as usize + j) % 2 == 0 {
                    continue;
                }
                if let Some(cur) = params.get(key).and_then(value_as_f64) {
                    let up = ((idx as usize / 2 + j) % 2 == 0) ^ ((salt >> j) & 1 == 1);
                    let factor = if up { perturb_factor } else { down };
                    let next = cur * factor;
                    params.insert(
                        key.clone(),
                        serde_json::Number::from_f64(next)
                            .map(Value::Number)
                            .unwrap_or(Value::Null),
                    );
                    spread.push(format!("{key} x{factor:.4}"));
                }
            }
            if spread.is_empty() {
                "pbt cold start: no numeric params to spread".to_string()
            } else {
                format!(
                    "pbt cold start: seed spread from defaults: {}",
                    spread.join(", ")
                )
            }
        }
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

/// Warm-start policy for a generated child: resume weights from the best
/// succeeded experiment's latest checkpoint. None when there is no best, no
/// checkpoint yet, or no usable URI — i.e. a genuine cold start.
///
/// Used by BOTH strategies. It was PBT-only, which made heuristic campaigns
/// record `parentExperimentId` while transferring nothing, so a "generation"
/// was a random restart wearing a lineage label.
fn warm_start_policy(best_exp: Option<&Experiment>) -> Option<CheckpointPolicy> {
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
            // The canary is a gate probe, not a search point: generation 0, no
            // parent, no perturbation to record.
            lineage: Some(ExperimentLineage {
                relation: DerivationRelation::Canary,
                parent: None,
                parent_uid: None,
                generation: Some(0),
                strategy: Some(campaign.spec.strategy.strategy_type.clone()),
                perturbations: Vec::new(),
                salt: None,
            }),
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
    lineage: Option<ExperimentLineage>,
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
            labels: Some({
                // Labels, not annotations: only labels are selectable, so this
                // is what makes the tree queryable with
                //   kubectl get exp -l research.nixlab.io/parent=<name>
                //   kubectl get exp -l research.nixlab.io/role=Remeasure
                let mut l =
                    BTreeMap::from([(CAMPAIGN_LABEL.to_string(), campaign_name.to_string())]);
                if let Some(lin) = &lineage {
                    l.insert(ROLE_LABEL.to_string(), format!("{:?}", lin.relation));
                    if let Some(g) = lin.generation {
                        l.insert(GENERATION_LABEL.to_string(), g.to_string());
                    }
                    // Label values are capped at 63 chars; a longer parent name
                    // is dropped from the label but kept in spec.lineage.
                    if let Some(pn) = lin.parent.as_deref().filter(|p| p.len() <= 63) {
                        l.insert(PARENT_LABEL.to_string(), pn.to_string());
                    }
                }
                l
            }),
            owner_references: Some(vec![owner]),
            ..Default::default()
        },
        spec: ExperimentSpec {
            campaign_ref: campaign_name.to_string(),
            hypothesis,
            parameters,
            patch: None,
            checkpoint_policy,
            lineage,
            env: {
                let mut env = mesh_env(campaign, campaign_name, ns);
                env.extend(generation_seed_env(campaign, idx));
                env
            },
        },
        status: None,
    }
}

/// Shared per-generation seed, so every experiment in a generation trains on
/// the SAME random stream (common random numbers).
///
/// MEASURED 2026-07-26: with a fixed seed this trainer is BIT-reproducible.
/// Three probes -- 5 workers/25 iters, 5 workers/120 iters, and 8 workers
/// deliberately oversubscribed onto 7 CPU to force env_runner stragglers --
/// each returned identical rewards to 16 significant figures across separate
/// pods. The seed term is not merely small; for a fixed seed it is exactly 0.
///
/// That makes candidate-minus-control a PAIRED difference: run the control
/// slot (incumbent) and the candidates at the same seed and the seed term
/// CANCELS, instead of being averaged down by sqrt(k) as replication would.
/// Strictly stronger than replication, at zero GPU cost.
///
/// The seed ROTATES per generation. A seed is not a tunable hyperparameter --
/// pinning one for a whole campaign would let the search overfit to it and the
/// winner would not survive a different seed.
///
/// Empty when populationSize is unset, so non-PBT campaigns keep their
/// previous behaviour and this is purely additive.
fn generation_seed_env(campaign: &ResearchCampaign, idx: u32) -> Vec<EnvVar> {
    let Some(pop) = campaign.spec.population_size.filter(|p| *p > 0) else {
        return Vec::new();
    };
    let generation = idx / pop;
    // Hash the campaign UID (not its name, so a recreated campaign of the same
    // name draws fresh seeds) with the generation index. Deterministic: no RNG,
    // so the same experiment always resolves to the same seed across reconciles
    // and operator restarts.
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    campaign.uid().unwrap_or_default().hash(&mut h);
    generation.hash(&mut h);
    // Must fit a 32-bit range: numpy/torch seeds are bounded.
    let seed = (h.finish() % (i32::MAX as u64)) as u32;
    vec![EnvVar {
        name: "ATHENA_SEED".to_string(),
        value: Some(seed.to_string()),
        ..Default::default()
    }]
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
///
/// GPU meshes are Kueue-managed: the POD TEMPLATE (not just the Deployment)
/// carries `kueue.x-k8s.io/queue-name` + `kueue.x-k8s.io/priority-class` because
/// Kueue's pod integration reads pod labels and gates the pod with a scheduling
/// gate at admission — Deployments have no suspend field, so labels are the whole
/// mechanism. CPU-only meshes (empty gpuResources) stay unlabeled and schedule
/// directly.
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

    // Kueue labels go on the pod template only; the selector/Service keep the
    // base labels (selectors are immutable and Kueue doesn't read them).
    let mut pod_labels = labels.clone();
    if !mesh.gpu_resources.is_empty() && !mesh.queue_name.is_empty() {
        pod_labels.insert(
            "kueue.x-k8s.io/queue-name".to_string(),
            mesh.queue_name.clone(),
        );
        pod_labels.insert(
            "kueue.x-k8s.io/priority-class".to_string(),
            mesh.priority_class.clone(),
        );
    }

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
                    labels: Some(pod_labels),
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
                lineage: None,
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
                seed_experiment_ref: None,
                proposer: None,
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
    fn sigma_needs_two_samples_and_uses_the_n_minus_1_denominator() {
        assert_eq!(seed_noise_sigma(&[]), None);
        assert_eq!(seed_noise_sigma(&[4000.0]), None, "sd undefined at n=1");
        // The real measurement from three byte-identical spot runs.
        let s = seed_noise_sigma(&[4838.16, 3849.36, 3390.02]).unwrap();
        assert!((s - 740.0).abs() < 15.0, "expected sample sd ~740, got {s}");
        // Population form would give ~604; the n-1 form must be LARGER, because a
        // low sigma makes the selection gate too eager.
        assert!(s > 604.0, "n-1 denominator must not understate the noise");
    }

    #[test]
    fn sigma_gate_is_the_upper_confidence_limit_not_the_point_estimate() {
        // n=2: below MIN_CONTROL_RUNS -> no gate at all. A sd on 1 dof has a
        // 95% interval spanning an order of magnitude; acting on it is worse
        // than not acting.
        assert_eq!(sigma_upper_bound(&[4000.0, 4500.0]), None);

        // The real n=3 measurement. Point estimate ~740; the gate must use the
        // inflated bound so a badly-determined sigma cannot make it too eager.
        let scores = [4838.16, 3849.36, 3390.02];
        let point = seed_noise_sigma(&scores).unwrap();
        let gate = sigma_upper_bound(&scores).unwrap();
        assert!(
            gate > point,
            "gate {gate} must exceed point estimate {point}"
        );
        assert!(
            (gate / point - 2.1169).abs() < 1e-3,
            "n=3 factor should be the chi-square 80% one-sided limit"
        );

        // More control runs -> the bound tightens toward the point estimate.
        let many = [4838.16, 3849.36, 3390.02, 4100.0, 4300.0, 3950.0];
        let ratio = sigma_upper_bound(&many).unwrap() / seed_noise_sigma(&many).unwrap();
        assert!(ratio < 1.5, "factor must shrink as n grows, got {ratio}");
    }

    #[test]
    fn a_challenger_inside_the_noise_floor_does_not_displace_the_incumbent() {
        let sigma = Some(740.0);
        // +400 on ~4000 is a 10% lead and still pure noise at this sigma.
        assert!(!beats_incumbent(
            4400.0,
            4000.0,
            sigma,
            &ObjectiveGoal::Maximize
        ));
        // Clearing one sigma does displace.
        assert!(beats_incumbent(
            4800.0,
            4000.0,
            sigma,
            &ObjectiveGoal::Maximize
        ));
        // Minimize direction is mirrored.
        assert!(beats_incumbent(
            3200.0,
            4000.0,
            sigma,
            &ObjectiveGoal::Minimize
        ));
        assert!(!beats_incumbent(
            3600.0,
            4000.0,
            sigma,
            &ObjectiveGoal::Minimize
        ));
    }

    #[test]
    fn without_a_calibrated_sigma_the_incumbent_is_never_displaced() {
        // No control runs yet -> no sigma -> hold. Acting on an uncalibrated
        // comparison is exactly the behaviour this rule removes, so an absent
        // sigma must fail CLOSED, not fall back to argmax.
        assert!(!beats_incumbent(
            9999.0,
            1.0,
            None,
            &ObjectiveGoal::Maximize
        ));
        assert!(!beats_incumbent(
            0.0,
            9999.0,
            None,
            &ObjectiveGoal::Minimize
        ));
    }

    #[test]
    fn parameter_deltas_capture_what_actually_moved() {
        let parent = BTreeMap::from([
            ("gait".to_string(), json!(2.0)),
            ("jerk".to_string(), json!(-0.6)),
            ("unchanged".to_string(), json!(1.0)),
            ("experimentIteration".to_string(), json!(3)),
        ]);
        let child = BTreeMap::from([
            ("gait".to_string(), json!(4.0)),
            ("jerk".to_string(), json!(-0.6)),
            ("unchanged".to_string(), json!(1.0)),
            ("experimentIteration".to_string(), json!(4)),
        ]);
        let d = parameter_deltas(Some(&parent), &child);
        assert_eq!(d.len(), 1, "only the moved param is recorded: {d:?}");
        assert_eq!(d[0].param, "gait");
        assert_eq!(d[0].from, Some(2.0));
        assert_eq!(d[0].to, 4.0);
        assert_eq!(d[0].factor, Some(2.0));
        // Bookkeeping must never surface as a science delta, even though it
        // changed — that conflation is what the typed lineage exists to end.
        assert!(!d.iter().any(|x| x.param == "experimentIteration"));
    }

    #[test]
    fn hypothesis_is_a_derived_view_of_the_structured_record() {
        // The prose must be reconstructible FROM the record, so the record is
        // the source of truth and the string is only a rendering.
        let l = ExperimentLineage {
            relation: DerivationRelation::Remeasure,
            parent: Some("camp-003".into()),
            parent_uid: None,
            generation: Some(1),
            strategy: Some("pbt".into()),
            perturbations: Vec::new(),
            salt: None,
        };
        // Byte-identical to the legacy string, so the prose fallback in
        // is_control_experiment still matches objects written by this version.
        assert!(
            l.describe().starts_with(CONTROL_HYPOTHESIS_PREFIX),
            "{}",
            l.describe()
        );

        let p = ExperimentLineage {
            relation: DerivationRelation::Perturb,
            parent: Some("camp-001".into()),
            parent_uid: None,
            generation: Some(2),
            strategy: Some("pbt".into()),
            perturbations: vec![ParameterDelta {
                param: "gait".into(),
                from: Some(2.0),
                to: 4.0,
                factor: Some(2.0),
            }],
            salt: Some(0),
        };
        let d = p.describe();
        assert!(d.contains("camp-001") && d.contains("gait"), "{d}");
    }

    #[test]
    fn control_runs_are_identified_by_typed_relation_not_prose() {
        let mut e = exp_with("c", ExperimentPhase::Succeeded, "r", 1.0);
        // Typed relation wins.
        e.spec.lineage = Some(ExperimentLineage {
            relation: DerivationRelation::Remeasure,
            parent: Some("p".into()),
            parent_uid: None,
            generation: Some(1),
            strategy: Some("pbt".into()),
            perturbations: Vec::new(),
            salt: None,
        });
        e.spec.hypothesis = "totally unrelated prose".into();
        assert!(is_control_experiment(&e), "relation must decide, not prose");

        e.spec.lineage.as_mut().unwrap().relation = DerivationRelation::Perturb;
        assert!(!is_control_experiment(&e));

        // Legacy objects (no lineage) still contribute their sigma sample via
        // the prose fallback — without this, upgrading mid-campaign would empty
        // the control set and freeze the incumbent.
        e.spec.lineage = None;
        e.spec.hypothesis = "control: re-measure incumbent foo-000 on a fresh seed".into();
        assert!(
            is_control_experiment(&e),
            "legacy prose fallback must survive"
        );
        e.spec.hypothesis = "pbt exploit+explore from foo-000: gait x2.0".into();
        assert!(!is_control_experiment(&e));
    }

    #[test]
    fn one_shared_seed_per_generation_rotating_across_generations() {
        let mut c = campaign_pbt(Some(3), None);
        c.metadata.uid = Some("uid-abc".into());
        let seed_of = |c: &ResearchCampaign, idx: u32| {
            generation_seed_env(c, idx)
                .into_iter()
                .find(|e| e.name == "ATHENA_SEED")
                .and_then(|e| e.value)
        };
        // Generation 0 = idx 0,1,2 -> one shared seed (paired comparison).
        let g0 = seed_of(&c, 0);
        assert!(g0.is_some());
        assert_eq!(seed_of(&c, 1), g0);
        assert_eq!(seed_of(&c, 2), g0, "a generation must share ONE seed");
        // Generation 1 = idx 3.. -> a DIFFERENT seed, so the search cannot
        // overfit to one seed.
        let g1 = seed_of(&c, 3);
        assert_ne!(g1, g0, "seed must rotate per generation");
        assert_eq!(seed_of(&c, 4), g1);
        // Deterministic across calls (no RNG).
        assert_eq!(seed_of(&c, 0), g0);
        // Fits a 32-bit seed range.
        let v: u64 = g0.clone().unwrap().parse().unwrap();
        assert!(v < i32::MAX as u64);
        // A recreated campaign (new UID) draws fresh seeds.
        let mut c2 = c.clone();
        c2.metadata.uid = Some("uid-xyz".into());
        assert_ne!(seed_of(&c2, 0), g0);
    }

    #[test]
    fn exactly_one_control_slot_per_generation_regardless_of_batch_shape() {
        // The v72 bug: control was slot 0 of every reconcile BATCH. Once the
        // population saturates, slots free one at a time (want==1), so every
        // new experiment became a control -- 9 controls, 0 candidates, and the
        // search silently did nothing. Keying off the absolute index makes the
        // decision independent of how work is batched.
        let pop = 3u32;
        let is_control = |idx: u32| idx % pop == 0;
        let controls: Vec<u32> = (0..12).filter(|i| is_control(*i)).collect();
        assert_eq!(controls, vec![0, 3, 6, 9], "one per generation of 3");
        // 12 experiments -> 4 generations -> 4 controls and 8 candidates.
        assert_eq!(controls.len(), 4);
        assert_eq!(12 - controls.len(), 8, "the rest must be search points");
    }

    #[test]
    fn non_pbt_campaigns_get_no_pinned_seed() {
        // populationSize unset -> additive change, previous behaviour intact.
        let c = campaign_pbt(None, None);
        assert!(generation_seed_env(&c, 0).is_empty());
    }

    #[test]
    fn pbt_cold_start_when_no_best() {
        let t = template_with_default_lr(0.1);
        let (params, hyp) = pbt_experiment(&t, None, 1.2, 0, 0);
        // Child 0 is the untouched baseline reference.
        assert_eq!(params.get("lr"), Some(&json!(0.1)));
        assert_eq!(params.get("parentExperimentId"), Some(&Value::Null));
        assert!(hyp.contains("cold start"), "{hyp}");
        assert!(warm_start_policy(None).is_none());
    }

    #[test]
    fn pbt_cold_start_spreads_the_population_instead_of_cloning_defaults() {
        // The whole point of populationSize is to sample >1 point per
        // generation. Before this, every cold-start child came back with the
        // template defaults, so generation 1 burned N GPUs on one config.
        let t = template_with_default_lr(0.1);
        let seeds: Vec<_> = (0..3)
            .map(|i| pbt_experiment(&t, None, 1.2, i, 0).0)
            .collect();
        assert_eq!(seeds[0].get("lr"), Some(&json!(0.1)), "child 0 is baseline");
        let distinct: std::collections::BTreeSet<String> =
            seeds.iter().map(|p| format!("{:?}", p.get("lr"))).collect();
        assert!(
            distinct.len() > 1,
            "cold-start population must not be N clones, got {distinct:?}"
        );
    }

    #[test]
    fn pbt_checkpoint_policy_resumes_from_best_latest_checkpoint() {
        let mut best = exp_with("best", ExperimentPhase::Succeeded, "loss", 0.1);
        best.status.as_mut().unwrap().latest_checkpoint = Some(CheckpointRef {
            uri: "s3://ckpt/best/step-100".into(),
            step: Some(100),
            ..Default::default()
        });
        let cp = warm_start_policy(Some(&best)).expect("best has a checkpoint");
        assert_eq!(cp.resume_from.as_deref(), Some("s3://ckpt/best/step-100"));
        // No checkpoint yet -> cold start (None).
        let no_ckpt = exp_with("warm", ExperimentPhase::Succeeded, "loss", 0.2);
        assert!(warm_start_policy(Some(&no_ckpt)).is_none());
    }

    #[test]
    fn heuristic_child_warm_starts_from_parent_not_only_pbt() {
        // Regression: the heuristic branch recorded parentExperimentId but passed
        // None as the checkpoint policy, so every generation cold-started while
        // claiming a parent. Observed live as three 2,002,944-step experiments
        // reporting warm_started 0 with a parentExperimentId set.
        let t = template_with_default_lr(0.1);
        let mut best_exp = exp_with("c-000", ExperimentPhase::Succeeded, "loss", 0.1);
        best_exp.spec.parameters = BTreeMap::from([("lr".to_string(), json!(0.5))]);
        best_exp.status.as_mut().unwrap().latest_checkpoint = Some(CheckpointRef {
            uri: "/workspace/runs/c/c-000/checkpoints".into(),
            ..Default::default()
        });
        let campaign = campaign_pbt(None, None);

        let (params, hypothesis) =
            next_experiment(&t, Some(("c-000", &best_exp.spec.parameters)), 1, 0);
        // Parentage must be recorded AND weights carried; recording one without
        // the other is what made the lineage a lie.
        assert_eq!(
            params.get("parentExperimentId").and_then(Value::as_str),
            Some("c-000")
        );
        let cp = warm_start_policy(Some(&best_exp));
        let child = build_experiment(&campaign, "c", "default", 1, params, hypothesis, cp, None);
        assert_eq!(
            child
                .spec
                .checkpoint_policy
                .as_ref()
                .and_then(|p| p.resume_from.as_deref()),
            Some("/workspace/runs/c/c-000/checkpoints"),
            "heuristic child must inherit the parent's weights"
        );
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
        let cp = warm_start_policy(Some(&best_exp));
        let child = build_experiment(&campaign, "c", "default", 2, params, hypothesis, cp, None);

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
    fn child_with_no_parent_cold_starts() {
        // A child with no best to inherit from has nothing to resume, so it must
        // cold-start. Previously named `heuristic_child_has_no_checkpoint_policy`
        // and documented as "the heuristic path is unchanged — children
        // cold-start", which asserted an intent that turned out to be a bug:
        // heuristic children WITH a parent were also cold-starting. This test
        // never exercised that case (it passes None for both parent and policy),
        // so it kept passing while the real invariant was violated. See
        // `heuristic_child_warm_starts_from_parent_not_only_pbt`.
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
                seed_experiment_ref: None,
                proposer: None,
            },
            status: None,
        };
        let (params, hypothesis) = next_experiment(&t, None, 0, 0);
        let child = build_experiment(&campaign, "c", "default", 0, params, hypothesis, None, None);
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
            seed_experiment_ref: None,
            proposer: None,
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

    // Kueue's pod integration reads POD labels: a GPU mesh must carry the
    // queue-name/priority-class pair on the pod template (default athena-gpu /
    // mesh-high), while a CPU-only mesh must stay out of Kueue entirely.
    #[test]
    fn mesh_pod_template_kueue_labels_gpu_only() {
        let mesh = |gpu: serde_json::Value| -> InferenceMeshSpec {
            serde_json::from_value(json!({
                "image": "ghcr.io/x/mesh-llm:1",
                "model": "m",
                "gpuResources": gpu,
            }))
            .unwrap()
        };
        let labels = BTreeMap::from([("app".to_string(), "mesh-llm".to_string())]);
        let owner = OwnerReference::default;

        let gpu_dep = build_mesh_deployment(
            "mesh-llm-c",
            "default",
            &mesh(json!({"nvidia.com/gpu": "1"})),
            &labels,
            owner(),
        );
        let pod_labels = |d: &Deployment| {
            d.spec
                .as_ref()
                .unwrap()
                .template
                .metadata
                .as_ref()
                .unwrap()
                .labels
                .clone()
                .unwrap()
        };
        let gpu_labels = pod_labels(&gpu_dep);
        assert_eq!(
            gpu_labels
                .get("kueue.x-k8s.io/queue-name")
                .map(String::as_str),
            Some("athena-gpu")
        );
        assert_eq!(
            gpu_labels
                .get("kueue.x-k8s.io/priority-class")
                .map(String::as_str),
            Some("mesh-high")
        );
        // Selector must NOT pick up the kueue labels (selectors are immutable).
        assert_eq!(
            gpu_dep.spec.as_ref().unwrap().selector.match_labels,
            Some(labels.clone())
        );

        let cpu_dep =
            build_mesh_deployment("mesh-llm-c", "default", &mesh(json!({})), &labels, owner());
        let cpu_labels = pod_labels(&cpu_dep);
        assert!(!cpu_labels.contains_key("kueue.x-k8s.io/queue-name"));
        assert!(!cpu_labels.contains_key("kueue.x-k8s.io/priority-class"));
    }

    // ---- Echo filter (v70 bug) ----

    use athena_api::experiment::{CheckpointPolicy, ExperimentLineage};

    /// A warm-started child with a typed lineage parent.
    fn warm_child(name: &str, parent: &str, value: f64) -> Experiment {
        let mut e = exp_with(name, ExperimentPhase::Succeeded, "reward_mean", value);
        e.spec.checkpoint_policy = Some(CheckpointPolicy {
            interval_seconds: None,
            resume_from: Some(format!("/workspace/runs/c/{parent}/checkpoints/x")),
            retain: None,
        });
        e.spec.lineage = Some(ExperimentLineage {
            relation: DerivationRelation::Perturb,
            parent: Some(parent.to_string()),
            parent_uid: None,
            generation: None,
            strategy: None,
            perturbations: Vec::new(),
            salt: None,
        });
        e
    }

    #[test]
    fn echo_detected_only_for_warm_started_bit_identical_children() {
        let parent = exp_with("c-000", ExperimentPhase::Succeeded, "reward_mean", 100.0);
        let echo = warm_child("c-001", "c-000", 100.0);
        let real = warm_child("c-002", "c-000", 100.0 + 1e-12);
        let cold = exp_with("c-003", ExperimentPhase::Succeeded, "reward_mean", 100.0);
        let by_name: BTreeMap<String, &Experiment> = [&parent, &echo, &real, &cold]
            .into_iter()
            .map(|e| (e.name_any(), e))
            .collect();

        assert!(
            is_echo(&echo, &by_name, "reward_mean"),
            "bit-identical warm-started child is an echo"
        );
        assert!(
            !is_echo(&real, &by_name, "reward_mean"),
            "any float difference means training happened"
        );
        assert!(
            !is_echo(&cold, &by_name, "reward_mean"),
            "cold start without resume is not an echo"
        );
        assert!(
            !is_echo(&parent, &by_name, "reward_mean"),
            "parent itself is not an echo"
        );
    }

    #[test]
    fn echo_requires_parent_in_campaign() {
        let echo = warm_child("c-001", "deleted-parent", 100.0);
        let by_name: BTreeMap<String, &Experiment> =
            [&echo].into_iter().map(|e| (e.name_any(), e)).collect();
        assert!(
            !is_echo(&echo, &by_name, "reward_mean"),
            "missing parent → cannot prove echo"
        );
    }

    // ---- Founding cohort gate (v85 bug) ----

    fn cold_start_exp(name: &str, phase: ExperimentPhase, value: f64) -> Experiment {
        let mut e = exp_with(name, phase, "reward_mean", value);
        e.spec.lineage = Some(ExperimentLineage {
            relation: DerivationRelation::Seed,
            parent: None,
            parent_uid: None,
            generation: None,
            strategy: None,
            perturbations: Vec::new(),
            salt: None,
        });
        e
    }

    #[test]
    fn founding_cohort_open_while_any_cold_start_nonterminal() {
        let done = cold_start_exp("c-000", ExperimentPhase::Succeeded, 1.0);
        let running = cold_start_exp("c-001", ExperimentPhase::Running, 0.0);
        let failed = cold_start_exp("c-002", ExperimentPhase::Failed, 0.0);
        let child = warm_child("c-003", "c-000", 2.0);

        assert!(is_cold_start(&done) && is_terminal(&done));
        assert!(is_cold_start(&running) && !is_terminal(&running));
        assert!(
            is_cold_start(&failed) && is_terminal(&failed),
            "failed is terminal"
        );
        assert!(
            !is_cold_start(&child),
            "child with parent is not a cold start"
        );

        // The v85 scenario: cohort open iff any cold start is non-terminal.
        let cohort = [&done, &running, &failed, &child];
        let open = cohort.iter().any(|e| is_cold_start(e) && !is_terminal(e));
        assert!(open, "running cold start keeps the founding cohort open");

        let all_done = [&done, &failed, &child];
        let open = all_done.iter().any(|e| is_cold_start(e) && !is_terminal(e));
        assert!(!open, "cohort closes once every cold start is terminal");
    }
}
