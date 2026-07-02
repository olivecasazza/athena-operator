//! Shared research-dossier assembler.
//!
//! Pure rendering over already-fetched CRs — no Kubernetes client. Both the
//! operator's `athena dossier` subcommand and the console BFF fetch the resources
//! and call [`render`] so there is exactly one assembly code path. A [`Curation`]
//! (derived from a `ResearchReport` spec) filters the experiment set and splices
//! in scientist-authored narrative; passing `None` renders the full, uncurated
//! campaign.

use std::collections::{BTreeMap, HashSet};
use std::fmt::Write;

use kube::api::ListParams;
use kube::{Api, Client, ResourceExt};

use crate::benchmark_run::BenchmarkRun;
use crate::experiment::Experiment;
use crate::experiment_template::ExperimentTemplate;
use crate::research_campaign::ResearchCampaign;
use crate::research_report::ResearchReportSpec;

/// Label the campaign controller stamps on every child experiment Job/CR.
pub const CAMPAIGN_LABEL: &str = "athena.nixlab.io/campaign";

/// Scientist curation applied to a campaign's experiments when composing a
/// paper dataset. Borrows from a `ResearchReport` spec.
pub struct Curation<'a> {
    pub title: Option<&'a str>,
    /// Include only these experiments by name; empty = all.
    pub included: &'a [String],
    /// Prune these experiments by name (applied after inclusion).
    pub excluded: &'a [String],
    /// Extra narrative sections, keyed by heading.
    pub sections: &'a BTreeMap<String, String>,
    /// Hypotheses to record as future work.
    pub seeded_hypotheses: &'a [String],
}

impl<'a> Curation<'a> {
    pub fn from_spec(spec: &'a ResearchReportSpec) -> Self {
        Curation {
            title: spec.title.as_deref(),
            included: &spec.included_experiments,
            excluded: &spec.excluded_experiments,
            sections: &spec.sections,
            seeded_hypotheses: &spec.seeded_hypotheses,
        }
    }
}

/// Apply a curation to the experiment list: keep `included` (or all when empty),
/// drop `excluded`, preserving input order. Returns borrowed refs so callers can
/// also compute counts. `None` = every experiment, unfiltered.
pub fn curate<'a>(experiments: &'a [Experiment], curation: Option<&Curation>) -> Vec<&'a Experiment> {
    experiments
        .iter()
        .filter(|e| {
            let Some(c) = curation else { return true };
            let name = e.name_any();
            let included = c.included.is_empty() || c.included.iter().any(|n| n == &name);
            let excluded = c.excluded.iter().any(|n| n == &name);
            included && !excluded
        })
        .collect()
}

/// Assemble the dossier Markdown into `out`. Writing to a `String` is infallible;
/// the `fmt::Result` is only propagated to satisfy the `write!` macros.
pub fn render(
    out: &mut String,
    campaign_name: &str,
    namespace: &str,
    campaign: &ResearchCampaign,
    template: &ExperimentTemplate,
    experiments: &[Experiment],
    runs_by_experiment: &BTreeMap<String, Vec<&BenchmarkRun>>,
    curation: Option<&Curation>,
) -> std::fmt::Result {
    let exps = curate(experiments, curation);

    // ── 1. Title ─────────────────────────────────────────────────────────────
    let title = curation
        .and_then(|c| c.title)
        .map(|t| t.to_string())
        .unwrap_or_else(|| format!("Research Dossier: {}", campaign_name));
    writeln!(out, "# {}", title)?;
    writeln!(out)?;
    // No wall-clock timestamp in the body: `render` must be a pure function of its
    // inputs so the report reconciler can content-diff the dossier without churning
    // (assembly time is recorded in ResearchReport `status.lastAssembledTime`).
    writeln!(out, "_Campaign: {} · Namespace: {}_", campaign_name, namespace)?;
    if curation.is_some() {
        writeln!(out, "_Curated: {} of {} experiments_", exps.len(), experiments.len())?;
    }
    writeln!(out)?;

    // ── 2. Research Objective ────────────────────────────────────────────────
    writeln!(out, "## Research Objective")?;
    writeln!(out)?;
    match &template.spec.research_objective {
        Some(obj) => writeln!(out, "{}", obj)?,
        None => writeln!(out, "_not recorded_")?,
    }
    writeln!(out)?;

    // ── 2b. Scientist narrative sections (curated) ───────────────────────────
    if let Some(c) = curation {
        for (heading, body) in c.sections {
            writeln!(out, "## {}", heading)?;
            writeln!(out)?;
            writeln!(out, "{}", body)?;
            writeln!(out)?;
        }
    }

    // ── 3. Method & Setup ────────────────────────────────────────────────────
    writeln!(out, "## Method & Setup")?;
    writeln!(out)?;
    let git = &template.spec.source.git;
    writeln!(out, "**Source**")?;
    writeln!(out)?;
    writeln!(out, "- URL: `{}`", git.url)?;
    writeln!(out, "- Ref: `{}`", git.r#ref)?;
    if let Some(commit) = &git.commit {
        writeln!(out, "- Commit: `{}`", commit)?;
    }
    writeln!(out)?;

    let obj_spec = &template.spec.objective;
    writeln!(out, "**Objective**")?;
    writeln!(out)?;
    writeln!(out, "- Metric: `{}`", obj_spec.metric)?;
    writeln!(out, "- Goal: `{:?}`", obj_spec.goal)?;
    writeln!(out)?;

    writeln!(out, "**Strategy & Budget**")?;
    writeln!(out)?;
    writeln!(out, "- Strategy: `{}`", campaign.spec.strategy.strategy_type)?;
    writeln!(out, "- Max experiments: {}", campaign.spec.budget.max_experiments)?;
    writeln!(out, "- Max duration: `{}`", campaign.spec.budget.max_duration)?;
    writeln!(out)?;

    if !template.spec.parameter_schema.is_empty() {
        writeln!(out, "**Parameter Glossary**")?;
        writeln!(out)?;
        writeln!(out, "| Name | Type | Description |")?;
        writeln!(out, "|------|------|-------------|")?;
        for (name, param) in &template.spec.parameter_schema {
            let desc = param.description.as_deref().unwrap_or("—");
            writeln!(out, "| `{}` | {} | {} |", name, param.parameter_type, md_cell(desc))?;
        }
        writeln!(out)?;
    }

    if let Some(dashboard) = &template.spec.dashboard {
        if !dashboard.metrics.is_empty() {
            writeln!(out, "**Metric Glossary**")?;
            writeln!(out)?;
            writeln!(out, "| Name | Label | Unit | Description | Baseline |")?;
            writeln!(out, "|------|-------|------|-------------|----------|")?;
            for (name, metric) in &dashboard.metrics {
                let unit = metric.unit.as_deref().unwrap_or("—");
                let desc = metric.description.as_deref().unwrap_or("—");
                let baseline = metric
                    .baseline
                    .map(|b| format!("{}", b))
                    .unwrap_or_else(|| "—".to_string());
                writeln!(
                    out,
                    "| `{}` | {} | {} | {} | {} |",
                    name,
                    md_cell(&metric.label),
                    unit,
                    md_cell(desc),
                    baseline
                )?;
            }
            writeln!(out)?;
        }
    }

    // ── 4. Experiments table ─────────────────────────────────────────────────
    let obj_metric = &template.spec.objective.metric;
    writeln!(out, "## Experiments")?;
    writeln!(out)?;
    writeln!(out, "| # | Name | Hypothesis | Parameters | Objective Value | Phase | Decision |")?;
    writeln!(out, "|---|------|------------|------------|-----------------|-------|----------|")?;
    for (i, exp) in exps.iter().copied().enumerate() {
        let name = exp.name_any();
        let hypothesis = md_cell(&exp.spec.hypothesis);
        let params = md_cell(&params_str(exp));
        let obj_str = fmt_f64_opt(best_objective(exp, obj_metric));
        let phase = exp
            .status
            .as_ref()
            .map(|s| format!("{:?}", s.phase))
            .unwrap_or_else(|| "—".to_string());
        let decision = decision_str(exp);
        writeln!(
            out,
            "| {} | {} | {} | {} | {} | {} | {} |",
            i + 1,
            name,
            hypothesis,
            params,
            obj_str,
            phase,
            decision
        )?;
    }
    writeln!(out)?;

    // ── 5. Results ───────────────────────────────────────────────────────────
    writeln!(out, "## Results")?;
    writeln!(out)?;
    let c_status = campaign.status.as_ref();
    writeln!(
        out,
        "**Best Experiment:** {}",
        c_status
            .and_then(|s| s.best_experiment.as_deref())
            .unwrap_or("_not recorded_")
    )?;
    writeln!(out)?;
    writeln!(
        out,
        "**Best Objective:** {}",
        c_status
            .and_then(|s| s.best_objective)
            .map(|v| format!("{:.4}", v))
            .unwrap_or_else(|| "_not recorded_".to_string())
    )?;
    writeln!(out)?;

    for exp in exps.iter().copied() {
        let exp_name = exp.name_any();
        if let Some(runs) = runs_by_experiment.get(&exp_name) {
            writeln!(out, "### Benchmark: {}", exp_name)?;
            writeln!(out)?;
            for run in runs {
                let run_name = run.name_any();
                let phase_str = run
                    .status
                    .as_ref()
                    .map(|s| format!("{:?}", s.phase))
                    .unwrap_or_else(|| "—".to_string());
                writeln!(out, "**BenchmarkRun:** `{}`  Phase: `{}`", run_name, phase_str)?;
                writeln!(out)?;
                if let Some(status) = &run.status {
                    if !status.aggregate_metrics.is_empty() {
                        writeln!(out, "| Metric | Mean | Std | Min | Max | Count | CI Low | CI High |")?;
                        writeln!(out, "|--------|------|-----|-----|-----|-------|--------|---------|")?;
                        for (metric_name, agg) in &status.aggregate_metrics {
                            let count_str = agg
                                .count
                                .map(|c| c.to_string())
                                .unwrap_or_else(|| "—".to_string());
                            writeln!(
                                out,
                                "| {} | {} | {} | {} | {} | {} | {} | {} |",
                                metric_name,
                                fmt_f64_opt(agg.mean),
                                fmt_f64_opt(agg.std),
                                fmt_f64_opt(agg.min),
                                fmt_f64_opt(agg.max),
                                count_str,
                                fmt_f64_opt(agg.ci_low),
                                fmt_f64_opt(agg.ci_high),
                            )?;
                        }
                        writeln!(out)?;
                    }
                    if !status.gates.is_empty() {
                        writeln!(out, "| Gate Metric | Threshold | Actual | Passed |")?;
                        writeln!(out, "|-------------|-----------|--------|--------|")?;
                        for gate in &status.gates {
                            let actual = fmt_f64_opt(gate.actual);
                            let passed = if gate.passed { "✓" } else { "✗" };
                            writeln!(
                                out,
                                "| {} | {:.4} | {} | {} |",
                                gate.metric, gate.threshold, actual, passed
                            )?;
                        }
                        writeln!(out)?;
                    }
                }
            }
        }
    }

    // ── 6. Reproducibility ───────────────────────────────────────────────────
    writeln!(out, "## Reproducibility")?;
    writeln!(out)?;
    let git_commit = template
        .spec
        .source
        .git
        .commit
        .as_deref()
        .unwrap_or("_not recorded_");
    writeln!(out, "_Git commit: `{}`_", git_commit)?;
    writeln!(out)?;
    for exp in exps.iter().copied() {
        let exp_name = exp.name_any();
        let env_str = exp
            .status
            .as_ref()
            .and_then(|s| s.environment.as_ref())
            .map(|env| {
                let mut parts: Vec<String> = Vec::new();
                if let Some(nodes) = &env.node_names {
                    if !nodes.is_empty() {
                        parts.push(format!("nodes=[{}]", nodes.join(",")));
                    }
                }
                if let Some(pods) = &env.pod_names {
                    if !pods.is_empty() {
                        parts.push(format!("pods=[{}]", pods.join(",")));
                    }
                }
                if parts.is_empty() { "_not recorded_".to_string() } else { parts.join(", ") }
            })
            .unwrap_or_else(|| "_not recorded_".to_string());
        let cost_str = exp
            .status
            .as_ref()
            .and_then(|s| s.cost.as_ref())
            .map(|c| {
                let mut parts: Vec<String> = Vec::new();
                if let Some(gpu_h) = c.gpu_hours {
                    parts.push(format!("gpu_hours={:.2}", gpu_h));
                }
                if let Some(rt) = c.runtime_seconds {
                    parts.push(format!("runtime_seconds={}", rt));
                }
                if parts.is_empty() { "_not recorded_".to_string() } else { parts.join(", ") }
            })
            .unwrap_or_else(|| "_not recorded_".to_string());
        let provenance = exp
            .status
            .as_ref()
            .and_then(|s| s.artifacts.as_ref())
            .and_then(|a| a.provenance_uri.as_deref())
            .unwrap_or("_not recorded_");
        writeln!(
            out,
            "- **{}**: params=[{}] | env={} | cost={} | provenance=`{}`",
            exp_name,
            params_str(exp),
            env_str,
            cost_str,
            provenance
        )?;
    }
    writeln!(out)?;

    // ── 7. Campaign Journal ──────────────────────────────────────────────────
    writeln!(out, "## Campaign Journal")?;
    writeln!(out)?;
    for exp in exps.iter().copied() {
        let obj_str = fmt_f64_opt(best_objective(exp, obj_metric));
        writeln!(
            out,
            "- **{}**: {} → objective {}, decision {}",
            exp.name_any(),
            exp.spec.hypothesis,
            obj_str,
            decision_str(exp)
        )?;
    }
    writeln!(out)?;

    // ── 8. Seeded Hypotheses / Future Work (curated) ─────────────────────────
    if let Some(c) = curation {
        if !c.seeded_hypotheses.is_empty() {
            writeln!(out, "## Seeded Hypotheses / Future Work")?;
            writeln!(out)?;
            for h in c.seeded_hypotheses {
                writeln!(out, "- {}", h)?;
            }
            writeln!(out)?;
        }
    }

    // ── 9. Artifact Index ────────────────────────────────────────────────────
    writeln!(out, "## Artifact Index")?;
    writeln!(out)?;
    writeln!(
        out,
        "| Experiment | Workspace | Journal | Provenance | Checkpoints | Benchmark Report |"
    )?;
    writeln!(
        out,
        "|------------|-----------|---------|------------|-------------|------------------|"
    )?;
    for exp in exps.iter().copied() {
        let exp_name = exp.name_any();
        let artifacts = exp.status.as_ref().and_then(|s| s.artifacts.as_ref());
        let workspace = artifacts.and_then(|a| a.workspace_uri.as_deref()).unwrap_or("—");
        let journal = artifacts.and_then(|a| a.journal_uri.as_deref()).unwrap_or("—");
        let provenance = artifacts.and_then(|a| a.provenance_uri.as_deref()).unwrap_or("—");
        let checkpoints = artifacts.and_then(|a| a.checkpoints_uri.as_deref()).unwrap_or("—");
        let report_uri = runs_by_experiment
            .get(&exp_name)
            .and_then(|runs| runs.last())
            .and_then(|run| run.status.as_ref())
            .and_then(|s| s.report_uri.as_deref())
            .unwrap_or("—");
        writeln!(
            out,
            "| {} | {} | {} | {} | {} | {} |",
            exp_name, workspace, journal, provenance, checkpoints, report_uri
        )?;
    }
    writeln!(out)?;

    Ok(())
}

/// Fetch a campaign's resources (template, label-selected experiments ordered by
/// creation, and their benchmark runs) and render an optionally-curated dossier.
/// Returns `(markdown, included_experiment_count)`. Read-only. Shared by the
/// operator CLI, the report reconciler, and the console BFF.
pub async fn assemble(
    client: &Client,
    campaign_name: &str,
    namespace: &str,
    curation: Option<&Curation<'_>>,
) -> kube::Result<(String, usize)> {
    let campaigns: Api<ResearchCampaign> = Api::namespaced(client.clone(), namespace);
    let campaign = campaigns.get(campaign_name).await?;

    let templates: Api<ExperimentTemplate> = Api::namespaced(client.clone(), namespace);
    let template = templates.get(&campaign.spec.template_ref).await?;

    let experiments_api: Api<Experiment> = Api::namespaced(client.clone(), namespace);
    let label = format!("{CAMPAIGN_LABEL}={campaign_name}");
    let mut experiments = experiments_api
        .list(&ListParams::default().labels(&label))
        .await?
        .items;
    experiments.sort_by(|a, b| {
        let at = a.metadata.creation_timestamp.as_ref().map(|t| t.0);
        let bt = b.metadata.creation_timestamp.as_ref().map(|t| t.0);
        at.cmp(&bt).then_with(|| a.name_any().cmp(&b.name_any()))
    });

    let experiment_names: HashSet<String> = experiments.iter().map(|e| e.name_any()).collect();
    let benchmark_runs_api: Api<BenchmarkRun> = Api::namespaced(client.clone(), namespace);
    let benchmark_runs: Vec<BenchmarkRun> = benchmark_runs_api
        .list(&ListParams::default())
        .await?
        .items
        .into_iter()
        .filter(|br| experiment_names.contains(&br.spec.target_ref.name))
        .collect();

    let mut runs_by_experiment: BTreeMap<String, Vec<&BenchmarkRun>> = BTreeMap::new();
    for br in &benchmark_runs {
        runs_by_experiment
            .entry(br.spec.target_ref.name.clone())
            .or_default()
            .push(br);
    }

    let included = curate(&experiments, curation).len();
    let mut doc = String::new();
    render(
        &mut doc,
        campaign_name,
        namespace,
        &campaign,
        &template,
        &experiments,
        &runs_by_experiment,
        curation,
    )
    .expect("writing to String is infallible");
    Ok((doc, included))
}

// ── helpers ──────────────────────────────────────────────────────────────────

fn params_str(exp: &Experiment) -> String {
    exp.spec
        .parameters
        .iter()
        .map(|(k, v)| format!("{}={}", k, v))
        .collect::<Vec<_>>()
        .join(", ")
}

fn decision_str(exp: &Experiment) -> String {
    exp.status
        .as_ref()
        .and_then(|s| s.decision.as_ref())
        .map(|d| format!("{:?}", d))
        .unwrap_or_else(|| "—".to_string())
}

/// Best objective for an experiment: prefer the scalar `metrics_detail.best`,
/// fall back to `metrics[<objective>]`.
fn best_objective(exp: &Experiment, obj_metric: &str) -> Option<f64> {
    exp.status.as_ref().and_then(|s| {
        s.metrics_detail
            .as_ref()
            .and_then(|md| md.best.as_ref())
            .and_then(|v| v.as_f64())
            .or_else(|| s.metrics.get(obj_metric).and_then(|v| v.as_f64()))
    })
}

fn fmt_f64_opt(v: Option<f64>) -> String {
    v.map(|x| format!("{:.4}", x))
        .unwrap_or_else(|| "—".to_string())
}

/// Escape free text for a Markdown table cell so a stray `|` or newline can't
/// break the table layout.
fn md_cell(s: &str) -> String {
    s.replace('|', "\\|").replace(['\n', '\r'], " ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::experiment::{
        ExperimentDecision, ExperimentPhase, ExperimentSpec, ExperimentStatus,
    };
    use crate::experiment_template::{
        ExperimentTemplateSpec, GitSource, MetricsSpec, ObjectiveGoal, ObjectiveSpec, SourceSpec,
    };
    use crate::research_campaign::{
        CampaignBudget, ResearchCampaignSpec, ResearchCampaignStatus, StrategySpec,
    };
    use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;
    use serde_json::json;

    fn experiment(name: &str, hypothesis: &str, val_bpb: f64, decision: ExperimentDecision) -> Experiment {
        Experiment {
            metadata: ObjectMeta { name: Some(name.to_string()), ..Default::default() },
            spec: ExperimentSpec {
                campaign_ref: "camp".into(),
                hypothesis: hypothesis.into(),
                parameters: BTreeMap::from([("lr".to_string(), json!(0.001))]),
                patch: None,
                checkpoint_policy: None,
            },
            status: Some(ExperimentStatus {
                phase: ExperimentPhase::Succeeded,
                metrics: BTreeMap::from([("val_bpb".to_string(), json!(val_bpb))]),
                decision: Some(decision),
                ..Default::default()
            }),
        }
    }

    fn campaign() -> ResearchCampaign {
        ResearchCampaign {
            metadata: ObjectMeta { name: Some("camp".into()), ..Default::default() },
            spec: ResearchCampaignSpec {
                template_ref: "tmpl".into(),
                concurrency: 1,
                budget: CampaignBudget { max_experiments: 10, max_duration: "1h".into() },
                strategy: StrategySpec { strategy_type: "heuristic".into() },
                benchmark_suite_ref: None,
                benchmark_runtime_profile_ref: None,
                population_size: None,
                perturb_factor: None,
            },
            status: Some(ResearchCampaignStatus {
                best_experiment: Some("exp-b".into()),
                best_objective: Some(2.10),
                ..Default::default()
            }),
        }
    }

    fn template() -> ExperimentTemplate {
        ExperimentTemplate {
            metadata: ObjectMeta { name: Some("tmpl".into()), ..Default::default() },
            spec: ExperimentTemplateSpec {
                runtime_profile_ref: "rp".into(),
                source: SourceSpec {
                    git: GitSource { url: "u".into(), r#ref: "main".into(), commit: Some("abc123".into()) },
                },
                objective: ObjectiveSpec { metric: "val_bpb".into(), goal: ObjectiveGoal::Minimize },
                metrics: MetricsSpec::default(),
                parameter_schema: BTreeMap::new(),
                defaults: BTreeMap::new(),
                dashboard: None,
                research_objective: Some("Can Muon beat AdamW at this scale?".into()),
            },
            status: None,
        }
    }

    // Uncurated render exercises every section over None-heavy status, and the
    // md_cell pipe-escape must fire on a hypothesis containing '|'.
    #[test]
    fn render_uncurated_produces_sections_and_escapes_pipes() {
        let exps = vec![
            experiment("exp-a", "baseline from defaults", 2.34, ExperimentDecision::Discard),
            experiment("exp-b", "perturb lr | higher is worse", 2.10, ExperimentDecision::Keep),
        ];
        let runs: BTreeMap<String, Vec<&BenchmarkRun>> = BTreeMap::new();
        let mut doc = String::new();
        render(&mut doc, "camp", "research", &campaign(), &template(), &exps, &runs, None)
            .expect("infallible");

        assert!(doc.contains("# Research Dossier: camp"), "{doc}");
        assert!(doc.contains("## Campaign Journal") && doc.contains("## Artifact Index"));
        assert!(doc.contains("exp-a") && doc.contains("exp-b"));
        assert!(doc.contains("**Best Experiment:** exp-b"));
        assert!(doc.contains("2.3400") && doc.contains("2.1000"));
        assert!(doc.contains("perturb lr \\| higher is worse"), "pipe not escaped: {doc}");
    }

    // render() MUST be a pure function of its inputs: the report reconciler
    // content-diffs the assembled dossier to decide whether to rewrite the
    // ConfigMap + status. Any wall-clock/nondeterminism in the body defeats that
    // diff and hot-loops the controller (it watches ResearchReport).
    #[test]
    fn render_is_deterministic() {
        let exps = vec![experiment("e", "h", 2.0, ExperimentDecision::Keep)];
        let runs: BTreeMap<String, Vec<&BenchmarkRun>> = BTreeMap::new();
        let mut a = String::new();
        render(&mut a, "camp", "ns", &campaign(), &template(), &exps, &runs, None).unwrap();
        let mut b = String::new();
        render(&mut b, "camp", "ns", &campaign(), &template(), &exps, &runs, None).unwrap();
        assert_eq!(a, b, "render must be a pure function of inputs (no wall clock)");
    }

    // Curation: exclude prunes an experiment, title overrides, sections + seeds
    // render, and the count line reflects the filtered set.
    #[test]
    fn render_curated_prunes_and_splices() {
        let exps = vec![
            experiment("exp-a", "baseline", 2.34, ExperimentDecision::Discard),
            experiment("exp-b", "winner", 2.10, ExperimentDecision::Keep),
        ];
        let runs: BTreeMap<String, Vec<&BenchmarkRun>> = BTreeMap::new();
        let sections = BTreeMap::from([("Related Work".to_string(), "Prior art on Muon.".to_string())]);
        let seeds = vec!["wider models close the gap".to_string()];
        let cur = Curation {
            title: Some("Muon vs AdamW"),
            included: &[],
            excluded: &["exp-a".to_string()],
            sections: &sections,
            seeded_hypotheses: &seeds,
        };
        let mut doc = String::new();
        render(&mut doc, "camp", "research", &campaign(), &template(), &exps, &runs, Some(&cur))
            .expect("infallible");

        assert!(doc.contains("# Muon vs AdamW"), "{doc}");
        assert!(doc.contains("_Curated: 1 of 2 experiments_"));
        assert!(!doc.contains("exp-a"), "pruned experiment must not appear: {doc}");
        assert!(doc.contains("exp-b"));
        assert!(doc.contains("## Related Work") && doc.contains("Prior art on Muon."));
        assert!(doc.contains("## Seeded Hypotheses / Future Work"));
        assert!(doc.contains("wider models close the gap"));
    }
}
