//! Reconcile a `ResearchCampaign` — the autonomous Auto-RL loop.
//!
//! Each pass:
//!   1. Resolve the campaign's `ExperimentTemplate` (objective + parameter space).
//!   2. List the campaign's `Experiment`s, partition by phase.
//!   3. Evaluate succeeded experiments against the objective, pick the best, and
//!      stamp each one's `status.decision` (Keep on the best, Discard otherwise).
//!      The experiment reconciler owns phase/metrics; the campaign owns decision.
//!   4. If under `budget.maxExperiments` and below `concurrency`, generate the
//!      next experiment(s) via the strategy (heuristic hill-climb from the best:
//!      baseline from template defaults, then perturb one numeric parameter).
//!   5. Update campaign status (counts, bestExperiment, bestObjective, phase).
//!
//! Experiments are created with an ownerReference to the campaign (so they are
//! garbage-collected with it) and a campaign label (so this loop can find them).

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use athena_api::experiment::{Experiment, ExperimentDecision, ExperimentPhase, ExperimentSpec};
use athena_api::experiment_template::{ExperimentTemplate, ObjectiveGoal, ObjectiveSpec};
use athena_api::research_campaign::ResearchCampaign;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::OwnerReference;
use kube::ResourceExt;
use kube::api::{Api, ListParams, ObjectMeta, Patch, PatchParams, PostParams};
use kube::runtime::controller::Action;
use serde_json::{Value, json};
use tracing::{info, warn};

use crate::Context;

const MANAGER: &str = "athena-campaign";
const CAMPAIGN_LABEL: &str = "athena.nixlab.io/campaign";
/// Multiplicative step for the hill-climb perturbation of a numeric parameter.
const STEP: f64 = 0.5;

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
pub async fn reconcile(campaign: Arc<ResearchCampaign>, ctx: Arc<Context>) -> Result<Action, Error> {
    let name = campaign.name_any();
    let ns = campaign.namespace().unwrap_or_else(|| "default".to_string());

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

    // 3. Evaluate: pick best by objective; stamp decisions.
    let best = pick_best(&completed, objective);
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

    // 4. Generate next experiments within budget + concurrency.
    let at_budget = total >= campaign.spec.budget.max_experiments;
    let concurrency = campaign.spec.concurrency.max(1);
    let mut created = 0u32;
    if !at_budget {
        let want = concurrency.saturating_sub(running);
        let budget_left = campaign.spec.budget.max_experiments - total;
        for i in 0..want.min(budget_left) {
            let idx = total + i;
            let (params, hypothesis) = next_experiment(&template, best.as_ref(), idx);
            let exp = build_experiment(&campaign, &name, &ns, idx, params, hypothesis);
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

/// Build the parameter set + hypothesis for the next experiment.
///
/// idx 0 (or no best yet) → baseline from template defaults. Otherwise hill-climb
/// from the best experiment's parameters by perturbing one numeric parameter
/// (coordinate-wise, alternating direction each lap). Always stamps the loop
/// bookkeeping params (iteration/parent/tag) the runners read from the spec.
fn next_experiment(
    template: &ExperimentTemplate,
    best: Option<&(String, f64)>,
    idx: u32,
) -> (BTreeMap<String, Value>, String) {
    let mut params: BTreeMap<String, Value> = template.spec.defaults.clone();
    // Seed any parameter_schema defaults not already in `defaults`.
    for (k, spec) in &template.spec.parameter_schema {
        if let Some(d) = &spec.default {
            params.entry(k.clone()).or_insert_with(|| d.clone());
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
                // Coordinate descent: pick a parameter, alternate direction per lap.
                let lap = (idx as usize - 1) / keys.len();
                let key = &keys[(idx as usize - 1) % keys.len()];
                let factor = if lap % 2 == 0 { 1.0 + STEP } else { 1.0 - STEP };
                if let Some(cur) = params.get(key).and_then(value_as_f64) {
                    let next = cur * factor;
                    params.insert(
                        key.clone(),
                        serde_json::Number::from_f64(next).map(Value::Number).unwrap_or(Value::Null),
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

fn build_experiment(
    campaign: &ResearchCampaign,
    campaign_name: &str,
    ns: &str,
    idx: u32,
    parameters: BTreeMap<String, Value>,
    hypothesis: String,
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
        },
        status: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use athena_api::experiment::ExperimentStatus;
    use athena_api::experiment_template::{
        ExperimentTemplateSpec, MetricsSpec, ParameterSpec, SourceSpec,
        {GitSource},
    };

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
    fn pick_best_none_when_no_metric() {
        let a = exp_with("a", ExperimentPhase::Succeeded, "other", 2.0);
        assert_eq!(pick_best(&[&a], &obj("loss", ObjectiveGoal::Minimize)), None);
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
            },
            status: None,
        }
    }

    #[test]
    fn baseline_uses_defaults_and_stamps_bookkeeping() {
        let t = template_with_default_lr(0.1);
        let (params, hyp) = next_experiment(&t, None, 0);
        assert_eq!(params.get("lr"), Some(&json!(0.1)));
        assert_eq!(params.get("experimentIteration"), Some(&json!(0)));
        assert_eq!(params.get("parentExperimentId"), Some(&Value::Null));
        assert!(hyp.contains("baseline"));
    }

    #[test]
    fn hillclimb_perturbs_numeric_param_from_best() {
        let t = template_with_default_lr(0.1);
        let lr = |p: &BTreeMap<String, Value>| value_as_f64(p.get("lr").unwrap()).unwrap();
        // idx 1, lap 0 -> factor 1.5 -> ~0.15
        let (params, hyp) = next_experiment(&t, Some(&("c-000".into(), 1.0)), 1);
        assert!((lr(&params) - 0.15).abs() < 1e-9, "{:?}", params.get("lr"));
        assert_eq!(params.get("parentExperimentId"), Some(&json!("c-000")));
        assert!(hyp.contains("perturb lr"), "{hyp}");
        // idx 2 (lap 1, same single key) -> factor 0.5 -> ~0.05
        let (params2, _) = next_experiment(&t, Some(&("c-000".into(), 1.0)), 2);
        assert!((lr(&params2) - 0.05).abs() < 1e-9, "{:?}", params2.get("lr"));
    }
}
