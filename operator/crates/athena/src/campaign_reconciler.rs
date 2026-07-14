//! Reconcile a `ResearchCampaign` — the autonomous Auto-RL loop.
//!
//! Each pass:
//!   1. Resolve the campaign's `ExperimentTemplate` (objective + parameter space).
//!   2. List the campaign's `Experiment`s, partition by phase.
//!   3. Evaluate succeeded experiments against the objective, pick the best, and
//!      stamp each one's `status.decision` (Keep on the best, Discard otherwise).
//!      The experiment reconciler owns phase/metrics; the campaign owns decision.
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
use athena_api::experiment::{
    CheckpointPolicy, Experiment, ExperimentDecision, ExperimentPhase, ExperimentSpec,
};
use athena_api::experiment_template::{ExperimentTemplate, ObjectiveGoal, ObjectiveSpec};
use athena_api::research_campaign::{InferenceMeshSpec, ResearchCampaign};
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
use kube::api::{Api, DeleteParams, ListParams, ObjectMeta, Patch, PatchParams, PostParams};
use kube::runtime::controller::Action;
use serde_json::{Value, json};
use tracing::{info, warn};

use crate::Context;

const MANAGER: &str = "athena-campaign";
const CAMPAIGN_LABEL: &str = "athena.nixlab.io/campaign";
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

    // 3. Evaluate: pick best by objective.
    let best = pick_best(&completed, objective);

    // 3b. Decision. When a benchmark suite is configured, the campaign does NOT
    // stamp Keep/Discard from the raw training objective — instead it ensures a
    // BenchmarkRun per succeeded experiment and lets the benchmark's gate results
    // drive `status.decision` (via promotionPolicy.updateExperimentStatus). Else,
    // keep the objective-based decision.
    if let Some(suite) = campaign.spec.benchmark_suite_ref.as_deref() {
        for e in &completed {
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

    // 3c. Ephemeral inference mesh (mesh-llm): bring it up while the campaign is
    // active, gate experiment generation on its readiness, and tear it down at
    // terminal phase (NOT object deletion — completed campaigns linger for
    // decision evaluation, so ownerReference-only GC would outlive the run).
    let at_budget = total >= campaign.spec.budget.max_experiments;
    // "The run ended" = budget reached AND all experiments terminal (running == 0).
    // at_budget alone only means "done generating": the final `concurrency`
    // experiments are still Running when total hits budget and must keep their mesh
    // endpoint until they finish. Keep the mesh up (ensure self-heals) through that
    // drain window; tear down only once nothing is left running.
    let all_done = at_budget && running == 0;
    let mesh_ready = match &campaign.spec.inference_mesh {
        Some(mesh) if !all_done => ensure_mesh(&ctx, &ns, &campaign, &name, mesh).await?,
        Some(_) => {
            teardown_mesh(&ctx, &ns, &name).await?;
            true
        }
        None => true,
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
    // When an inference mesh is configured, hold experiment creation until it is
    // Ready so the first prover Jobs don't launch against a dead LLM_BASE_URL.
    if !at_budget && mesh_ready {
        let want = concurrency.saturating_sub(running);
        let budget_left = campaign.spec.budget.max_experiments - total;
        // The best succeeded experiment is the seed for both strategies: its
        // params drive perturbation and (for PBT) its latest checkpoint warm-
        // starts the children's weights.
        let best_exp: Option<&Experiment> = best
            .as_ref()
            .and_then(|(bn, _)| completed.iter().find(|e| &e.name_any() == bn).copied());
        let best_ctx = best
            .as_ref()
            .zip(best_exp)
            .map(|((bn, _), e)| (bn.as_str(), &e.spec.parameters));
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
                    chosen.1 = format!("{} [replicate: local search space exhausted]", chosen.1);
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

    // 5. Update campaign status.
    let status = json!({ "status": {
        "runningExperiments": running + created,
        "succeededExperiments": succeeded,
        "failedExperiments": failed,
        "totalExperiments": total + created,
        "bestExperiment": best.as_ref().map(|b| b.0.clone()),
        "bestObjective": best.as_ref().map(|b| b.1),
        "phase": if at_budget { "Completed" } else { "Running" },
        "observedGeneration": campaign.metadata.generation,
        "controllerVersion": env!("CARGO_PKG_VERSION"),
    }});
    let campaigns: Api<ResearchCampaign> = Api::namespaced(ctx.client.clone(), &ns);
    campaigns
        .patch_status(&name, &PatchParams::apply(MANAGER), &Patch::Merge(&status))
        .await?;

    // Poll faster while the loop is active so it advances promptly between runs.
    Ok(Action::requeue(Duration::from_secs(if at_budget {
        300
    } else {
        15
    })))
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
    match &campaign.spec.inference_mesh {
        Some(mesh) => vec![EnvVar {
            name: "LLM_BASE_URL".to_string(),
            value: Some(format!(
                "http://mesh-llm-{campaign_name}.{ns}.svc.cluster.local:{}/v1",
                mesh.port
            )),
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

    #[test]
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
            },
            status: None,
        };
        let (params, hypothesis) = next_experiment(&t, None, 0, 0);
        let child = build_experiment(&campaign, "c", "default", 0, params, hypothesis, None);
        assert!(child.spec.checkpoint_policy.is_none());
    }
}
