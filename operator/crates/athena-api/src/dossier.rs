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
use crate::research_report::{Reference, ResearchReportSpec};

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
    /// External citations from the report spec.
    pub references: &'a [Reference],
    /// Narrow the document to one experiment and its descendants.
    pub about: Option<&'a crate::research_report::ReportSubject>,
}

impl<'a> Curation<'a> {
    pub fn from_spec(spec: &'a ResearchReportSpec) -> Self {
        Curation {
            title: spec.title.as_deref(),
            included: &spec.included_experiments,
            excluded: &spec.excluded_experiments,
            sections: &spec.sections,
            seeded_hypotheses: &spec.seeded_hypotheses,
            references: &spec.references,
            about: spec.about.as_ref(),
        }
    }
}

/// Apply a curation to the experiment list: keep `included` (or all when empty),
/// drop `excluded`, preserving input order. Returns borrowed refs so callers can
/// also compute counts. `None` = every experiment, unfiltered.
pub fn curate<'a>(
    experiments: &'a [Experiment],
    curation: Option<&Curation>,
) -> Vec<&'a Experiment> {
    // `about` narrows the document to a branch of the search tree BEFORE the
    // explicit include/exclude lists are applied, so a scientist can still
    // prune within the subtree they scoped to.
    let subtree: Option<std::collections::HashSet<String>> = curation
        .and_then(|c| c.about)
        .filter(|a| a.kind == "Experiment")
        .map(|a| descendants_of(experiments, &a.name));
    experiments
        .iter()
        .filter(|e| {
            let Some(c) = curation else { return true };
            let name = e.name_any();
            if let Some(sub) = &subtree {
                if !sub.contains(&name) {
                    return false;
                }
            }
            let included = c.included.is_empty() || c.included.iter().any(|n| n == &name);
            let excluded = c.excluded.iter().any(|n| n == &name);
            included && !excluded
        })
        .collect()
}

/// `root` plus every experiment transitively derived from it via `spec.lineage`.
///
/// Breadth-first over the parent pointers rather than recursion, and it tracks
/// visited names so a malformed cycle terminates instead of hanging the
/// controller. Returns just the root when nothing derives from it.
pub fn descendants_of(experiments: &[Experiment], root: &str) -> std::collections::HashSet<String> {
    let mut children_of: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for e in experiments {
        if let Some(parent) = e.spec.lineage.as_ref().and_then(|l| l.parent.as_ref()) {
            children_of
                .entry(parent.clone())
                .or_default()
                .push(e.name_any());
        }
    }
    let mut out = std::collections::HashSet::new();
    let mut queue = vec![root.to_string()];
    while let Some(n) = queue.pop() {
        if !out.insert(n.clone()) {
            continue; // already seen — also the cycle guard
        }
        if let Some(kids) = children_of.get(&n) {
            queue.extend(kids.iter().cloned());
        }
    }
    out
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

    let title = curation
        .and_then(|c| c.title)
        .map(|t| t.to_string())
        .unwrap_or_else(|| format!("Research Dossier: {}", campaign_name));

    // ── 0. OKF frontmatter ───────────────────────────────────────────────────
    // Open Knowledge Format (Google Cloud, 2026). v0.1 hard rules the validator
    // exits non-zero on: valid UTF-8, a parseable YAML block delimited by `---`,
    // and a non-empty `type`. Broken links are warnings only. Without this block
    // the dossier fails FM_REQ and TYPE_REQ immediately.
    //
    // v0.2 trust signals answer "how much should I trust this?" for a document
    // an agent will read: `generated` (machine authorship), `verified` (human or
    // adversarial confirmation), `sources` (provenance), `status` (lifecycle).
    //
    // DETERMINISM: `generated.at` is the latest experiment completion time, NOT
    // a wall clock. render() must be a pure function of its inputs — the
    // reconciler content-diffs the output to decide whether to rewrite the
    // ConfigMap, so any clock in the body defeats that diff and hot-loops the
    // controller. A data-derived cutoff is both stable and more honest: it says
    // what the document is current AS OF.
    writeln!(out, "---")?;
    writeln!(out, "type: Research Report")?;
    writeln!(out, "title: {}", yaml_scalar(&title))?;
    writeln!(
        out,
        "status: {}",
        match campaign.status.as_ref().and_then(|s| s.phase.as_deref()) {
            Some("Completed") => "stable",
            _ => "draft",
        }
    )?;
    writeln!(out, "generated:")?;
    writeln!(out, "  by: athena/{}", env!("CARGO_PKG_VERSION"))?;
    if let Some(at) = data_cutoff(&exps) {
        writeln!(out, "  at: {at}")?;
    }
    writeln!(out, "tags:")?;
    writeln!(out, "  - campaign/{campaign_name}")?;
    writeln!(out, "  - namespace/{namespace}")?;
    if let Some(subject) = curation.and_then(|c| c.about) {
        // A scoped document records what it is about and WHY, per the W3C Web
        // Annotation motivation vocabulary.
        writeln!(out, "about:")?;
        writeln!(out, "  kind: {}", yaml_scalar(&subject.kind))?;
        writeln!(out, "  name: {}", yaml_scalar(&subject.name))?;
        if let Some(m) = subject.motivation {
            writeln!(out, "  motivation: {m:?}")?;
        }
    }
    let refs = curation.map(|c| c.references).unwrap_or(&[]);
    if !refs.is_empty() {
        writeln!(out, "sources:")?;
        for r in refs {
            writeln!(out, "  - id: {}", yaml_scalar(&r.key))?;
            if let Some(u) = r.url.as_deref().filter(|u| !u.is_empty()) {
                writeln!(out, "    resource: {}", yaml_scalar(u))?;
            } else if let Some(d) = r.doi.as_deref().filter(|d| !d.is_empty()) {
                writeln!(
                    out,
                    "    resource: {}",
                    yaml_scalar(&format!("https://doi.org/{d}"))
                )?;
            }
            writeln!(out, "    title: {}", yaml_scalar(&r.title))?;
            if let Some(sup) = r.supports.as_deref().filter(|s| !s.is_empty()) {
                writeln!(out, "    supports: {}", yaml_scalar(sup))?;
            }
        }
    }
    writeln!(out, "---")?;
    writeln!(out)?;

    // ── 1. Title ─────────────────────────────────────────────────────────────
    writeln!(out, "# {}", title)?;
    writeln!(out)?;
    // No wall-clock timestamp in the body: `render` must be a pure function of its
    // inputs so the report reconciler can content-diff the dossier without churning
    // (assembly time is recorded in ResearchReport `status.lastAssembledTime`).
    writeln!(
        out,
        "_Campaign: {} · Namespace: {}_",
        campaign_name, namespace
    )?;
    if curation.is_some() {
        writeln!(
            out,
            "_Curated: {} of {} experiments_",
            exps.len(),
            experiments.len()
        )?;
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
            // Authored sections cite as `[@key]` (the existing curation
            // contract); OKF v0.2 wants `[^key]` footnotes keyed to a source
            // id. Rewrite on render so authors keep one syntax and the emitted
            // document is conformant.
            writeln!(out, "{}", cites_to_footnotes(body))?;
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
    writeln!(
        out,
        "- Strategy: `{}`",
        campaign.spec.strategy.strategy_type
    )?;
    writeln!(
        out,
        "- Max experiments: {}",
        campaign.spec.budget.max_experiments
    )?;
    writeln!(
        out,
        "- Max duration: `{}`",
        campaign.spec.budget.max_duration
    )?;
    writeln!(out)?;

    if !template.spec.parameter_schema.is_empty() {
        writeln!(out, "**Parameter Glossary**")?;
        writeln!(out)?;
        writeln!(out, "| Name | Type | Description |")?;
        writeln!(out, "|------|------|-------------|")?;
        for (name, param) in &template.spec.parameter_schema {
            let desc = param.description.as_deref().unwrap_or("—");
            writeln!(
                out,
                "| `{}` | {} | {} |",
                name,
                param.parameter_type,
                md_cell(desc)
            )?;
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
    writeln!(
        out,
        "| # | Name | Hypothesis | Parameters | Objective Value | Phase | Decision |"
    )?;
    writeln!(
        out,
        "|---|------|------------|------------|-----------------|-------|----------|"
    )?;
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
    // ── Search Tree ────────────────────────────────────────────────────
    // The experiment list above is flat and creation-ordered, which hides the
    // shape of the search entirely. This renders the actual derivation tree
    // from spec.lineage: who came from whom, at what generation, by what
    // operation, and exactly what moved.
    writeln!(out, "## Search Tree")?;
    writeln!(out)?;
    let forest = build_forest(&exps);
    if forest.is_empty() {
        writeln!(
            out,
            "_No lineage recorded — these experiments predate `spec.lineage`._"
        )?;
        writeln!(out)?;
    } else {
        writeln!(out, "```")?;
        for root in &forest {
            render_tree_node(out, root, 0)?;
        }
        writeln!(out, "```")?;
        writeln!(out)?;
        // Roles are what make the tree readable: a campaign that spent its
        // budget on controls searched nothing, and that is invisible in a flat
        // list. This is exactly the failure that went unnoticed on v72.
        let mut roles: BTreeMap<String, usize> = BTreeMap::new();
        for e in exps.iter().copied() {
            if let Some(l) = &e.spec.lineage {
                *roles.entry(format!("{:?}", l.relation)).or_default() += 1;
            }
        }
        if !roles.is_empty() {
            let summary = roles
                .iter()
                .map(|(k, v)| format!("{k}: {v}"))
                .collect::<Vec<_>>()
                .join(" · ");
            writeln!(out, "**Node roles:** {summary}")?;
            writeln!(out)?;
        }
    }

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
        "**Best Objective (biased):** {} — max over N noisy draws; monotone by \
         construction and therefore NOT evidence of progress.",
        c_status
            .and_then(|s| s.best_objective)
            .map(|v| format!("{:.4}", v))
            .unwrap_or_else(|| "_not recorded_".to_string())
    )?;
    writeln!(out)?;
    // The honest counterpart. Publishing best_objective alone reports exactly
    // the number the CRD's own doc comment calls "not evidence of progress",
    // while the unbiased re-measurement sat unrendered.
    writeln!(
        out,
        "**Incumbent Re-measured (unbiased):** {}",
        c_status
            .and_then(|s| s.incumbent_remeasured)
            .map(|v| format!("{:.4}", v))
            .unwrap_or_else(|| "_no control runs yet_".to_string())
    )?;
    writeln!(out)?;
    writeln!(
        out,
        "**Seed noise (sigma):** {} over {} control run(s)",
        c_status
            .and_then(|s| s.seed_noise_sigma)
            .map(|v| format!("{:.4}", v))
            .unwrap_or_else(|| "_not yet estimated_".to_string()),
        c_status.map(|s| s.control_runs).unwrap_or(0)
    )?;
    writeln!(out)?;
    if let (Some(b), Some(i)) = (
        c_status.and_then(|s| s.best_objective),
        c_status.and_then(|s| s.incumbent_remeasured),
    ) {
        writeln!(
            out,
            "_Divergence between the two above is the maximization bias, made \
             directly visible: {:.4}._",
            b - i
        )?;
        writeln!(out)?;
    }

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
                writeln!(
                    out,
                    "**BenchmarkRun:** `{}`  Phase: `{}`",
                    run_name, phase_str
                )?;
                writeln!(out)?;
                if let Some(status) = &run.status {
                    if !status.aggregate_metrics.is_empty() {
                        writeln!(
                            out,
                            "| Metric | Mean | Std | Min | Max | Count | CI Low | CI High |"
                        )?;
                        writeln!(
                            out,
                            "|--------|------|-----|-----|-----|-------|--------|---------|"
                        )?;
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
                if parts.is_empty() {
                    "_not recorded_".to_string()
                } else {
                    parts.join(", ")
                }
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
                if parts.is_empty() {
                    "_not recorded_".to_string()
                } else {
                    parts.join(", ")
                }
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

    // ── 8b. References (curated) ─────────────────────────────────────────────
    if let Some(c) = curation {
        if !c.references.is_empty() {
            writeln!(out, "## References")?;
            writeln!(out)?;
            // OKF v0.2 keys citations to a source `id` using markdown footnote
            // notation, so the reference list is emitted as footnote
            // DEFINITIONS (`[^id]: ...`). Inline `[@id]` in authored sections is
            // rewritten to `[^id]` on render, which makes every citation
            // resolve to one of these — and makes an unresolved one detectable
            // by link_warnings instead of rendering as literal text.
            for r in c.references {
                let mut line = format!("[^{}]: {}", r.key, r.title);
                if let Some(url) = &r.url {
                    line.push_str(&format!(" — <{}>", url));
                }
                if let Some(doi) = &r.doi {
                    line.push_str(&format!(" (doi:{})", doi));
                }
                if let Some(supports) = &r.supports {
                    line.push_str(&format!(" — supports: \"{}\"", supports));
                }
                writeln!(out, "{}", line)?;
            }
            writeln!(out)?;

            let check = citation_check(c.sections, c.references);
            if !check.cited_undefined.is_empty() || !check.defined_uncited.is_empty() {
                writeln!(out, "## Citation Reconciliation")?;
                writeln!(out)?;
                if !check.cited_undefined.is_empty() {
                    writeln!(
                        out,
                        "cited but undefined: {}",
                        check.cited_undefined.join(", ")
                    )?;
                }
                if !check.defined_uncited.is_empty() {
                    writeln!(
                        out,
                        "defined but never cited: {}",
                        check.defined_uncited.join(", ")
                    )?;
                }
                writeln!(out)?;
            }
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
        let workspace = artifacts
            .and_then(|a| a.workspace_uri.as_deref())
            .unwrap_or("—");
        let journal = artifacts
            .and_then(|a| a.journal_uri.as_deref())
            .unwrap_or("—");
        let provenance = artifacts
            .and_then(|a| a.provenance_uri.as_deref())
            .unwrap_or("—");
        let checkpoints = artifacts
            .and_then(|a| a.checkpoints_uri.as_deref())
            .unwrap_or("—");
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

/// An assembled dossier in both output formats plus the curated trial count.
/// Both renderers are pure functions of the same fetched inputs, so the pair is
/// internally consistent and content-diffable via either member.
pub struct Dossier {
    pub markdown: String,
    pub latex: String,
    pub included: usize,
}

/// Fetch a campaign's resources (template, label-selected experiments ordered by
/// creation, and their benchmark runs) and render an optionally-curated dossier
/// in Markdown + LaTeX. Read-only. Shared by the operator CLI, the report
/// reconciler, and the console BFF.
pub async fn assemble(
    client: &Client,
    campaign_name: &str,
    namespace: &str,
    curation: Option<&Curation<'_>>,
) -> kube::Result<Dossier> {
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
    let mut markdown = String::new();
    render(
        &mut markdown,
        campaign_name,
        namespace,
        &campaign,
        &template,
        &experiments,
        &runs_by_experiment,
        curation,
    )
    .expect("writing to String is infallible");
    let mut latex = String::new();
    render_latex(
        &mut latex,
        campaign_name,
        namespace,
        &campaign,
        &template,
        &experiments,
        &runs_by_experiment,
        curation,
    )
    .expect("writing to String is infallible");
    Ok(Dossier {
        markdown,
        latex,
        included,
    })
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

/// Escape free text for embedding in a LaTeX document. Processes each character
/// individually so backslash → `\textbackslash{}` does not re-escape the braces
/// introduced by subsequent substitutions.
///
/// Must be called on every piece of user-supplied text before writing it to the
/// LaTeX output. Code-ish values (params, URIs) should additionally be wrapped
/// in `\texttt{...}`.
fn tex_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 2);
    for c in s.chars() {
        match c {
            '\\' => out.push_str(r"\textbackslash{}"),
            '{' => out.push_str(r"\{"),
            '}' => out.push_str(r"\}"),
            '$' => out.push_str(r"\$"),
            '&' => out.push_str(r"\&"),
            '#' => out.push_str(r"\#"),
            '%' => out.push_str(r"\%"),
            '^' => out.push_str(r"\^{}"),
            '_' => out.push_str(r"\_"),
            '~' => out.push_str(r"\~{}"),
            c => out.push(c),
        }
    }
    out
}

/// Returns true for bytes valid in a citation key `[@key]`.
#[inline]
fn is_key_char(b: u8) -> bool {
    matches!(b, b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'_' | b':' | b'.' | b'-')
}

/// Extract every citation key referenced as `[@key]` in `text`, in order of
/// appearance, possibly with duplicates. A key must be non-empty and match
/// `[A-Za-z0-9_:.-]+`. Malformed patterns (`[@]`, unclosed `[@`) are skipped.
fn extract_citation_keys(text: &str) -> Vec<String> {
    let mut keys = Vec::new();
    let bytes = text.as_bytes();
    let n = bytes.len();
    let mut i = 0;
    while i + 1 < n {
        if bytes[i] == b'[' && bytes[i + 1] == b'@' {
            let start = i + 2;
            let mut j = start;
            while j < n && is_key_char(bytes[j]) {
                j += 1;
            }
            if j > start && j < n && bytes[j] == b']' {
                // All bytes in [start..j] are ASCII, so slice is valid UTF-8.
                keys.push(text[start..j].to_string());
                i = j + 1;
                continue;
            }
        }
        i += 1;
    }
    keys
}

/// Render `text` for LaTeX: replace each `[@key]` with `\cite{key}` and
/// tex-escape all surrounding text. Keys are not escaped (their charset is
/// already safe). Text fragments between cites are escaped individually.
fn tex_body_with_cites(text: &str) -> String {
    let mut out = String::new();
    let bytes = text.as_bytes();
    let n = bytes.len();
    let mut i = 0;
    let mut frag_start = 0;
    while i + 1 < n {
        if bytes[i] == b'[' && bytes[i + 1] == b'@' {
            let start = i + 2;
            let mut j = start;
            while j < n && is_key_char(bytes[j]) {
                j += 1;
            }
            if j > start && j < n && bytes[j] == b']' {
                out.push_str(&tex_escape(&text[frag_start..i]));
                out.push_str(r"\cite{");
                out.push_str(&text[start..j]);
                out.push('}');
                frag_start = j + 1;
                i = j + 1;
                continue;
            }
        }
        i += 1;
    }
    out.push_str(&tex_escape(&text[frag_start..]));
    out
}

/// Result of a citation consistency check between section bodies and the reference list.
pub struct CitationCheck {
    /// Citation keys that appear in section bodies but have no matching `Reference.key`
    /// in the reference list (deduped, sorted).
    pub cited_undefined: Vec<String>,
    /// `Reference.key` values that are never cited in any section body (in
    /// reference-list order, deduped).
    pub defined_uncited: Vec<String>,
}

/// Pure check: scan all section bodies for `[@key]` citations and cross-reference
/// against the provided reference list. Seeded hypotheses and other fields are
/// NOT scanned — sections only.
pub fn citation_check(
    sections: &BTreeMap<String, String>,
    references: &[Reference],
) -> CitationCheck {
    // Collect every cited key into a set.
    let mut cited_set: HashSet<String> = HashSet::new();
    for body in sections.values() {
        for key in extract_citation_keys(body) {
            cited_set.insert(key);
        }
    }

    // cited_undefined: cited keys absent from the reference list, sorted.
    let ref_key_set: HashSet<&str> = references.iter().map(|r| r.key.as_str()).collect();
    let mut cited_undefined: Vec<String> = cited_set
        .iter()
        .filter(|k| !ref_key_set.contains(k.as_str()))
        .cloned()
        .collect();
    cited_undefined.sort();

    // defined_uncited: reference keys never cited, in reference-list order, deduped.
    let mut seen: HashSet<&str> = HashSet::new();
    let defined_uncited: Vec<String> = references
        .iter()
        .filter(|r| !cited_set.contains(&r.key))
        .filter_map(|r| {
            if seen.insert(r.key.as_str()) {
                Some(r.key.clone())
            } else {
                None
            }
        })
        .collect();

    CitationCheck {
        cited_undefined,
        defined_uncited,
    }
}

/// Assemble the dossier as a standalone LaTeX document into `out`, mirroring
/// every section produced by [`render`]. Writing to a `String` is infallible;
/// the `fmt::Result` is only propagated to satisfy the `write!` macros.
///
/// The document is deterministic (no wall-clock calls): `\date{\today}` resolves
/// at compile-time in LaTeX, not at render time in Rust.
pub fn render_latex(
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
    let obj_metric = &template.spec.objective.metric;

    let title = curation
        .and_then(|c| c.title)
        .map(|t| t.to_string())
        .unwrap_or_else(|| format!("Research Dossier: {}", campaign_name));

    // ── Preamble ─────────────────────────────────────────────────────────────
    writeln!(out, r"\documentclass{{article}}")?;
    writeln!(out, r"\usepackage[T1]{{fontenc}}")?;
    writeln!(out, r"\usepackage{{booktabs}}")?;
    writeln!(out, r"\usepackage{{pgfplots}}")?;
    writeln!(out, r"\pgfplotsset{{compat=1.18}}")?;
    writeln!(out, r"\usepackage{{hyperref}}")?;
    writeln!(out, r"\usepackage[margin=1in]{{geometry}}")?;
    writeln!(out, r"\title{{{}}}", tex_escape(&title))?;
    writeln!(out, r"\author{{Athena autonomous research platform}}")?;
    writeln!(out, r"\date{{\today}}")?;
    writeln!(out, r"\begin{{document}}")?;
    writeln!(out, r"\maketitle")?;
    writeln!(out)?;
    // Mirror render()'s campaign/namespace subtitle line.
    writeln!(
        out,
        r"\noindent\textit{{Campaign: {} \textperiodcentered{{}} Namespace: {}}}",
        tex_escape(campaign_name),
        tex_escape(namespace)
    )?;
    if curation.is_some() {
        writeln!(
            out,
            r"\\\textit{{Curated: {} of {} experiments}}",
            exps.len(),
            experiments.len()
        )?;
    }
    writeln!(out)?;

    // ── 1. Research Objective ─────────────────────────────────────────────────
    writeln!(out, r"\section{{Research Objective}}")?;
    writeln!(out)?;
    match &template.spec.research_objective {
        Some(obj) => writeln!(out, "{}", tex_escape(obj))?,
        None => writeln!(out, r"\textit{{not recorded}}")?,
    }
    writeln!(out)?;

    // ── 2. Scientist narrative sections (curated) ────────────────────────────
    if let Some(c) = curation {
        for (heading, body) in c.sections {
            writeln!(out, r"\section{{{}}}", tex_escape(heading))?;
            writeln!(out)?;
            writeln!(out, "{}", tex_body_with_cites(body))?;
            writeln!(out)?;
        }
    }

    // ── 3. Method & Setup ────────────────────────────────────────────────────
    writeln!(out, r"\section{{Method \& Setup}}")?;
    writeln!(out)?;

    let git = &template.spec.source.git;
    writeln!(out, r"\subsection*{{Source}}")?;
    writeln!(out, r"\begin{{itemize}}")?;
    writeln!(out, r"  \item URL: \texttt{{{}}}", tex_escape(&git.url))?;
    writeln!(out, r"  \item Ref: \texttt{{{}}}", tex_escape(&git.r#ref))?;
    if let Some(commit) = &git.commit {
        writeln!(out, r"  \item Commit: \texttt{{{}}}", tex_escape(commit))?;
    }
    writeln!(out, r"\end{{itemize}}")?;
    writeln!(out)?;

    let obj_spec = &template.spec.objective;
    writeln!(out, r"\subsection*{{Objective}}")?;
    writeln!(out, r"\begin{{itemize}}")?;
    writeln!(
        out,
        r"  \item Metric: \texttt{{{}}}",
        tex_escape(&obj_spec.metric)
    )?;
    writeln!(out, r"  \item Goal: \texttt{{{:?}}}", obj_spec.goal)?;
    writeln!(out, r"\end{{itemize}}")?;
    writeln!(out)?;

    writeln!(out, r"\subsection*{{Strategy \& Budget}}")?;
    writeln!(out, r"\begin{{itemize}}")?;
    writeln!(
        out,
        r"  \item Strategy: \texttt{{{}}}",
        tex_escape(&campaign.spec.strategy.strategy_type)
    )?;
    writeln!(
        out,
        r"  \item Max experiments: {}",
        campaign.spec.budget.max_experiments
    )?;
    writeln!(
        out,
        r"  \item Max duration: \texttt{{{}}}",
        tex_escape(&campaign.spec.budget.max_duration)
    )?;
    writeln!(out, r"\end{{itemize}}")?;
    writeln!(out)?;

    if !template.spec.parameter_schema.is_empty() {
        writeln!(out, r"\subsection*{{Parameter Glossary}}")?;
        writeln!(out, r"\begin{{tabular}}{{l l p{{7cm}}}}")?;
        writeln!(out, r"  \toprule")?;
        writeln!(out, r"  Name & Type & Description \\")?;
        writeln!(out, r"  \midrule")?;
        for (name, param) in &template.spec.parameter_schema {
            let desc = param.description.as_deref().unwrap_or("\u{2014}");
            writeln!(
                out,
                r"  \texttt{{{}}} & {} & {} \\",
                tex_escape(name),
                tex_escape(&param.parameter_type),
                tex_escape(desc)
            )?;
        }
        writeln!(out, r"  \bottomrule")?;
        writeln!(out, r"\end{{tabular}}")?;
        writeln!(out)?;
    }

    if let Some(dashboard) = &template.spec.dashboard {
        if !dashboard.metrics.is_empty() {
            writeln!(out, r"\subsection*{{Metric Glossary}}")?;
            writeln!(out, r"\begin{{tabular}}{{l l l p{{4cm}} r}}")?;
            writeln!(out, r"  \toprule")?;
            writeln!(out, r"  Name & Label & Unit & Description & Baseline \\")?;
            writeln!(out, r"  \midrule")?;
            for (name, metric) in &dashboard.metrics {
                let unit = metric.unit.as_deref().unwrap_or("\u{2014}");
                let desc = metric.description.as_deref().unwrap_or("\u{2014}");
                let baseline = metric
                    .baseline
                    .map(|b| format!("{}", b))
                    .unwrap_or_else(|| "\u{2014}".to_string());
                writeln!(
                    out,
                    r"  \texttt{{{}}} & {} & {} & {} & {} \\",
                    tex_escape(name),
                    tex_escape(&metric.label),
                    tex_escape(unit),
                    tex_escape(desc),
                    baseline
                )?;
            }
            writeln!(out, r"  \bottomrule")?;
            writeln!(out, r"\end{{tabular}}")?;
            writeln!(out)?;
        }
    }

    // ── 4. Experiments table ──────────────────────────────────────────────────
    writeln!(out, r"\section{{Experiments}}")?;
    writeln!(out)?;
    writeln!(out, r"\begin{{tabular}}{{r l p{{4cm}} p{{3.5cm}} r l l}}")?;
    writeln!(out, r"  \toprule")?;
    writeln!(
        out,
        r"  \# & Name & Hypothesis & Parameters & Objective Value & Phase & Decision \\"
    )?;
    writeln!(out, r"  \midrule")?;
    for (i, exp) in exps.iter().copied().enumerate() {
        let name = tex_escape(&exp.name_any());
        let hypothesis = tex_escape(&exp.spec.hypothesis);
        let params = tex_escape(&params_str(exp));
        let obj_str = fmt_f64_opt(best_objective(exp, obj_metric));
        let phase = exp
            .status
            .as_ref()
            .map(|s| format!("{:?}", s.phase))
            .unwrap_or_else(|| "\u{2014}".to_string());
        let decision = decision_str(exp);
        writeln!(
            out,
            r"  {} & {} & {} & \texttt{{{}}} & {} & {} & {} \\",
            i + 1,
            name,
            hypothesis,
            params,
            obj_str,
            tex_escape(&phase),
            tex_escape(&decision)
        )?;
    }
    writeln!(out, r"  \bottomrule")?;
    writeln!(out, r"\end{{tabular}}")?;
    writeln!(out)?;

    // ── 5. Results ────────────────────────────────────────────────────────────
    writeln!(out, r"\section{{Results}}")?;
    writeln!(out)?;
    let c_status = campaign.status.as_ref();
    let best_exp = c_status
        .and_then(|s| s.best_experiment.as_deref())
        .unwrap_or("not recorded");
    let best_obj = c_status
        .and_then(|s| s.best_objective)
        .map(|v| format!("{:.4}", v))
        .unwrap_or_else(|| "not recorded".to_string());
    writeln!(out, r"\textbf{{Best Experiment:}} {}", tex_escape(best_exp))?;
    writeln!(out)?;
    writeln!(out, r"\textbf{{Best Objective:}} {}", tex_escape(&best_obj))?;
    writeln!(out)?;

    for exp in exps.iter().copied() {
        let exp_name = exp.name_any();
        if let Some(runs) = runs_by_experiment.get(&exp_name) {
            writeln!(out, r"\subsection*{{Benchmark: {}}}", tex_escape(&exp_name))?;
            writeln!(out)?;
            for run in runs {
                let run_name = run.name_any();
                let phase_str = run
                    .status
                    .as_ref()
                    .map(|s| format!("{:?}", s.phase))
                    .unwrap_or_else(|| "\u{2014}".to_string());
                writeln!(
                    out,
                    r"\textbf{{BenchmarkRun:}} \texttt{{{}}}  Phase: \texttt{{{}}}",
                    tex_escape(&run_name),
                    tex_escape(&phase_str)
                )?;
                writeln!(out)?;
                if let Some(status) = &run.status {
                    if !status.aggregate_metrics.is_empty() {
                        writeln!(out, r"\begin{{tabular}}{{l r r r r r r r}}")?;
                        writeln!(out, r"  \toprule")?;
                        writeln!(
                            out,
                            r"  Metric & Mean & Std & Min & Max & Count & CI Low & CI High \\"
                        )?;
                        writeln!(out, r"  \midrule")?;
                        for (metric_name, agg) in &status.aggregate_metrics {
                            let count_str = agg
                                .count
                                .map(|c| c.to_string())
                                .unwrap_or_else(|| "\u{2014}".to_string());
                            writeln!(
                                out,
                                r"  {} & {} & {} & {} & {} & {} & {} & {} \\",
                                tex_escape(metric_name),
                                fmt_f64_opt(agg.mean),
                                fmt_f64_opt(agg.std),
                                fmt_f64_opt(agg.min),
                                fmt_f64_opt(agg.max),
                                count_str,
                                fmt_f64_opt(agg.ci_low),
                                fmt_f64_opt(agg.ci_high),
                            )?;
                        }
                        writeln!(out, r"  \bottomrule")?;
                        writeln!(out, r"\end{{tabular}}")?;
                        writeln!(out)?;
                    }
                    if !status.gates.is_empty() {
                        writeln!(out, r"\begin{{tabular}}{{l r r l}}")?;
                        writeln!(out, r"  \toprule")?;
                        writeln!(out, r"  Gate Metric & Threshold & Actual & Passed \\")?;
                        writeln!(out, r"  \midrule")?;
                        for gate in &status.gates {
                            let actual = fmt_f64_opt(gate.actual);
                            let passed = if gate.passed { "Yes" } else { "No" };
                            writeln!(
                                out,
                                r"  {} & {:.4} & {} & {} \\",
                                tex_escape(&gate.metric),
                                gate.threshold,
                                actual,
                                passed
                            )?;
                        }
                        writeln!(out, r"  \bottomrule")?;
                        writeln!(out, r"\end{{tabular}}")?;
                        writeln!(out)?;
                    }
                }
            }
        }
    }

    // ── 5b. Sensitivity figure: most-varying numeric parameter ────────────────
    {
        const SKIP_PARAMS: &[&str] = &["experimentIteration", "parentExperimentId"];

        // Count distinct numeric values per parameter key.
        let mut distinct: BTreeMap<String, HashSet<String>> = BTreeMap::new();
        for exp in exps.iter().copied() {
            for (k, v) in &exp.spec.parameters {
                if SKIP_PARAMS.contains(&k.as_str()) {
                    continue;
                }
                if let Some(fv) = v.as_f64() {
                    distinct
                        .entry(k.clone())
                        .or_default()
                        .insert(format!("{}", fv));
                }
            }
        }

        // Pick the key with the most distinct values (≥2).
        if let Some(param_key) = distinct
            .iter()
            .filter(|(_, vals)| vals.len() >= 2)
            .max_by_key(|(_, vals)| vals.len())
            .map(|(k, _)| k.clone())
        {
            // Collect (x, y) for experiments that have both the param and an objective.
            let mut points: Vec<(f64, f64)> = exps
                .iter()
                .copied()
                .filter_map(|exp| {
                    let x = exp.spec.parameters.get(&param_key)?.as_f64()?;
                    let y = best_objective(exp, obj_metric)?;
                    Some((x, y))
                })
                .collect();

            if points.len() >= 2 {
                points.sort_by(|a, b| a.0.total_cmp(&b.0));
                let coords: String = points
                    .iter()
                    .map(|(x, y)| format!("({},{})", x, y))
                    .collect::<Vec<_>>()
                    .join(" ");

                writeln!(out, r"\begin{{figure}}[h]")?;
                writeln!(out, r"\centering")?;
                writeln!(out, r"\begin{{tikzpicture}}")?;
                writeln!(
                    out,
                    r"\begin{{axis}}[xlabel={{{}}}, ylabel={{{}}}, only marks, grid=major, width=0.8\textwidth, height=6cm]",
                    tex_escape(&param_key),
                    tex_escape(obj_metric)
                )?;
                writeln!(out, r"\addplot coordinates {{ {} }};", coords)?;
                writeln!(out, r"\end{{axis}}")?;
                writeln!(out, r"\end{{tikzpicture}}")?;
                writeln!(
                    out,
                    r"\caption{{{} vs {} across campaign trials.}}",
                    tex_escape(obj_metric),
                    tex_escape(&param_key)
                )?;
                writeln!(out, r"\end{{figure}}")?;
                writeln!(out)?;
            }
        }
    }

    // ── 6. Reproducibility ───────────────────────────────────────────────────
    writeln!(out, r"\section{{Reproducibility}}")?;
    writeln!(out)?;
    let git_commit = template
        .spec
        .source
        .git
        .commit
        .as_deref()
        .unwrap_or("not recorded");
    writeln!(out, r"Git commit: \texttt{{{}}}", tex_escape(git_commit))?;
    writeln!(out)?;
    writeln!(out, r"\begin{{itemize}}")?;
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
                if parts.is_empty() {
                    "not recorded".to_string()
                } else {
                    parts.join(", ")
                }
            })
            .unwrap_or_else(|| "not recorded".to_string());
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
                if parts.is_empty() {
                    "not recorded".to_string()
                } else {
                    parts.join(", ")
                }
            })
            .unwrap_or_else(|| "not recorded".to_string());
        let provenance = exp
            .status
            .as_ref()
            .and_then(|s| s.artifacts.as_ref())
            .and_then(|a| a.provenance_uri.as_deref())
            .unwrap_or("not recorded");
        writeln!(
            out,
            r"  \item \textbf{{{}}}: params=[\texttt{{{}}}] | env={} | cost={} | provenance=\texttt{{{}}}",
            tex_escape(&exp_name),
            tex_escape(&params_str(exp)),
            tex_escape(&env_str),
            tex_escape(&cost_str),
            tex_escape(provenance)
        )?;
    }
    writeln!(out, r"\end{{itemize}}")?;
    writeln!(out)?;

    // ── 7. Campaign Journal ──────────────────────────────────────────────────
    writeln!(out, r"\section{{Campaign Journal}}")?;
    writeln!(out)?;
    writeln!(out, r"\begin{{itemize}}")?;
    for exp in exps.iter().copied() {
        let obj_str = fmt_f64_opt(best_objective(exp, obj_metric));
        writeln!(
            out,
            r"  \item \textbf{{{}}}: {} $\rightarrow$ objective {}, decision {}",
            tex_escape(&exp.name_any()),
            tex_escape(&exp.spec.hypothesis),
            obj_str,
            tex_escape(&decision_str(exp))
        )?;
    }
    writeln!(out, r"\end{{itemize}}")?;
    writeln!(out)?;

    // ── 8. Seeded Hypotheses / Future Work (curated) ─────────────────────────
    if let Some(c) = curation {
        if !c.seeded_hypotheses.is_empty() {
            writeln!(out, r"\section{{Seeded Hypotheses / Future Work}}")?;
            writeln!(out)?;
            writeln!(out, r"\begin{{itemize}}")?;
            for h in c.seeded_hypotheses {
                writeln!(out, r"  \item {}", tex_escape(h))?;
            }
            writeln!(out, r"\end{{itemize}}")?;
            writeln!(out)?;
        }
    }

    // ── 9. Artifact Index ─────────────────────────────────────────────────────
    writeln!(out, r"\section{{Artifact Index}}")?;
    writeln!(out)?;
    writeln!(out, r"\begin{{tabular}}{{l l l l l l}}")?;
    writeln!(out, r"  \toprule")?;
    writeln!(
        out,
        r"  Experiment & Workspace & Journal & Provenance & Checkpoints & Benchmark Report \\"
    )?;
    writeln!(out, r"  \midrule")?;
    for exp in exps.iter().copied() {
        let exp_name = exp.name_any();
        let artifacts = exp.status.as_ref().and_then(|s| s.artifacts.as_ref());
        let workspace = artifacts
            .and_then(|a| a.workspace_uri.as_deref())
            .unwrap_or("\u{2014}");
        let journal = artifacts
            .and_then(|a| a.journal_uri.as_deref())
            .unwrap_or("\u{2014}");
        let provenance = artifacts
            .and_then(|a| a.provenance_uri.as_deref())
            .unwrap_or("\u{2014}");
        let checkpoints = artifacts
            .and_then(|a| a.checkpoints_uri.as_deref())
            .unwrap_or("\u{2014}");
        let report_uri = runs_by_experiment
            .get(&exp_name)
            .and_then(|runs| runs.last())
            .and_then(|run| run.status.as_ref())
            .and_then(|s| s.report_uri.as_deref())
            .unwrap_or("\u{2014}");
        writeln!(
            out,
            r"  {} & \texttt{{{}}} & \texttt{{{}}} & \texttt{{{}}} & \texttt{{{}}} & \texttt{{{}}} \\",
            tex_escape(&exp_name),
            tex_escape(workspace),
            tex_escape(journal),
            tex_escape(provenance),
            tex_escape(checkpoints),
            tex_escape(report_uri)
        )?;
    }
    writeln!(out, r"  \bottomrule")?;
    writeln!(out, r"\end{{tabular}}")?;
    writeln!(out)?;

    // ── 10. Bibliography + Citation Reconciliation (curated) ─────────────────
    if let Some(c) = curation {
        if !c.references.is_empty() {
            let check = citation_check(c.sections, c.references);
            if !check.cited_undefined.is_empty() || !check.defined_uncited.is_empty() {
                writeln!(out, r"\section{{Citation Reconciliation}}")?;
                writeln!(out)?;
                if !check.cited_undefined.is_empty() {
                    writeln!(
                        out,
                        "Cited but undefined: {}.",
                        check.cited_undefined.join(", ")
                    )?;
                }
                if !check.defined_uncited.is_empty() {
                    writeln!(
                        out,
                        "Defined but never cited: {}.",
                        check.defined_uncited.join(", ")
                    )?;
                }
                writeln!(out)?;
            }

            writeln!(out, r"\begin{{thebibliography}}{{99}}")?;
            for r in c.references {
                write!(out, r"\bibitem{{{}}} {}", r.key, tex_escape(&r.title))?;
                if let Some(url) = &r.url {
                    write!(out, r". \url{{{}}}", url)?;
                }
                if let Some(doi) = &r.doi {
                    write!(out, " doi:{}", tex_escape(doi))?;
                }
                if let Some(supports) = &r.supports {
                    write!(out, " Supports: {}", tex_escape(supports))?;
                }
                writeln!(out, ".")?;
            }
            writeln!(out, r"\end{{thebibliography}}")?;
            writeln!(out)?;
        }
    }

    writeln!(out, r"\end{{document}}")?;

    Ok(())
}

// ── end of render_latex ───────────────────────────────────────────────────────

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
    use crate::research_report::Reference;
    use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;
    use serde_json::json;

    fn experiment(
        name: &str,
        hypothesis: &str,
        val_bpb: f64,
        decision: ExperimentDecision,
    ) -> Experiment {
        Experiment {
            metadata: ObjectMeta {
                name: Some(name.to_string()),
                ..Default::default()
            },
            spec: ExperimentSpec {
                campaign_ref: "camp".into(),
                hypothesis: hypothesis.into(),
                parameters: BTreeMap::from([("lr".to_string(), json!(0.001))]),
                patch: None,
                checkpoint_policy: None,
                lineage: None,
                env: vec![],
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
            metadata: ObjectMeta {
                name: Some("camp".into()),
                ..Default::default()
            },
            spec: ResearchCampaignSpec {
                template_ref: "tmpl".into(),
                concurrency: 1,
                budget: CampaignBudget {
                    max_experiments: 10,
                    max_duration: "1h".into(),
                },
                strategy: StrategySpec {
                    strategy_type: "heuristic".into(),
                },
                benchmark_suite_ref: None,
                benchmark_runtime_profile_ref: None,
                population_size: None,
                perturb_factor: None,
                inference_mesh: None,
                inference_cluster: None,
                canary: None,
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
            metadata: ObjectMeta {
                name: Some("tmpl".into()),
                ..Default::default()
            },
            spec: ExperimentTemplateSpec {
                runtime_profile_ref: "rp".into(),
                source: SourceSpec {
                    git: GitSource {
                        url: "u".into(),
                        r#ref: "main".into(),
                        commit: Some("abc123".into()),
                    },
                },
                objective: ObjectiveSpec {
                    metric: "val_bpb".into(),
                    goal: ObjectiveGoal::Minimize,
                },
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
            experiment(
                "exp-a",
                "baseline from defaults",
                2.34,
                ExperimentDecision::Discard,
            ),
            experiment(
                "exp-b",
                "perturb lr | higher is worse",
                2.10,
                ExperimentDecision::Keep,
            ),
        ];
        let runs: BTreeMap<String, Vec<&BenchmarkRun>> = BTreeMap::new();
        let mut doc = String::new();
        render(
            &mut doc,
            "camp",
            "research",
            &campaign(),
            &template(),
            &exps,
            &runs,
            None,
        )
        .expect("infallible");

        assert!(doc.contains("# Research Dossier: camp"), "{doc}");
        assert!(doc.contains("## Campaign Journal") && doc.contains("## Artifact Index"));
        assert!(doc.contains("exp-a") && doc.contains("exp-b"));
        assert!(doc.contains("**Best Experiment:** exp-b"));
        assert!(doc.contains("2.3400") && doc.contains("2.1000"));
        assert!(
            doc.contains("perturb lr \\| higher is worse"),
            "pipe not escaped: {doc}"
        );
    }

    // render() MUST be a pure function of its inputs: the report reconciler
    // content-diffs the assembled dossier to decide whether to rewrite the
    // ConfigMap + status. Any wall-clock/nondeterminism in the body defeats that
    // diff and hot-loops the controller (it watches ResearchReport).
    fn with_lineage(
        mut e: Experiment,
        relation: crate::experiment::DerivationRelation,
        parent: Option<&str>,
        generation: u32,
    ) -> Experiment {
        e.spec.lineage = Some(crate::experiment::ExperimentLineage {
            relation,
            parent: parent.map(|p| p.to_string()),
            parent_uid: None,
            generation: Some(generation),
            strategy: Some("pbt".into()),
            perturbations: Vec::new(),
            salt: None,
        });
        e
    }

    #[test]
    fn broken_links_warn_but_never_block_publication() {
        // OKF LINK_TOL: links are warnings, so a dangling citation must NOT
        // make the document invalid. If this ever fails we have made the gate
        // stricter than the spec and would be refusing to publish valid OKF.
        let doc = "---\ntype: X\n---\nbody citing [^nowhere] and gs://\n";
        let c = okf_check(doc);
        assert!(c.ok(), "must remain valid: {:?}", c.violations);
        assert!(
            c.warnings.iter().any(|w| w.contains("[^nowhere]")),
            "dangling citation must warn: {:?}",
            c.warnings
        );
        assert!(
            c.warnings.iter().any(|w| w.contains("malformed URI")),
            "bare scheme must warn: {:?}",
            c.warnings
        );

        // A citation WITH a definition is silent.
        let ok = "---\ntype: X\n---\ncites [^src]\n\n[^src]: A title — <https://example.com/p>\n";
        assert!(
            okf_check(ok).warnings.is_empty(),
            "{:?}",
            okf_check(ok).warnings
        );
    }

    #[test]
    fn authored_citations_are_rewritten_to_okf_footnotes() {
        assert_eq!(
            cites_to_footnotes("see [@smith24] here"),
            "see [^smith24] here"
        );
        // Malformed patterns are left exactly as authored rather than mangled.
        assert_eq!(
            cites_to_footnotes("[@] and [@unclosed"),
            "[@] and [@unclosed"
        );
        assert_eq!(cites_to_footnotes("no citations"), "no citations");
        // Multibyte content survives the byte-wise scan.
        assert_eq!(cites_to_footnotes("σ rose [@a] — ok"), "σ rose [^a] — ok");
    }

    /// Writes a rendered dossier to OKF_DUMP so the REAL `openknowledge
    /// validate` can check it. Ignored by default; this exists so conformance
    /// is verified against the actual spec implementation rather than my
    /// reading of it.
    #[test]
    #[ignore]
    fn dump_dossier_for_external_validation() {
        let exps = vec![experiment("e", "h", 2.0, ExperimentDecision::Keep)];
        let runs: BTreeMap<String, Vec<&BenchmarkRun>> = BTreeMap::new();
        let mut doc = String::new();
        render(
            &mut doc,
            "camp",
            "ns",
            &campaign(),
            &template(),
            &exps,
            &runs,
            None,
        )
        .unwrap();
        let path = std::env::var("OKF_DUMP").expect("set OKF_DUMP");
        std::fs::write(path, doc).unwrap();
    }

    #[test]
    fn rendered_dossier_is_valid_okf() {
        let exps = vec![experiment("e", "h", 2.0, ExperimentDecision::Keep)];
        let runs: BTreeMap<String, Vec<&BenchmarkRun>> = BTreeMap::new();
        let mut doc = String::new();
        render(
            &mut doc,
            "camp",
            "ns",
            &campaign(),
            &template(),
            &exps,
            &runs,
            None,
        )
        .unwrap();
        let c = okf_check(&doc);
        assert!(
            c.ok(),
            "dossier must satisfy OKF hard rules: {:?}",
            c.violations
        );
        assert!(
            doc.starts_with("---\n"),
            "frontmatter must be the first line"
        );
        assert!(
            doc.contains("\ntype: Research Report\n"),
            "TYPE_REQ field missing"
        );
    }

    #[test]
    fn okf_check_catches_each_hard_rule() {
        // FM_REQ — no opening delimiter at all (the pre-OKF dossier shape).
        let v = okf_check("# Title\n\nbody").violations;
        assert!(
            v.iter().any(|x| x.starts_with("concept-frontmatter")),
            "{v:?}"
        );

        // FM_REQ — opened but never closed.
        let v = okf_check("---\ntype: X\nstill going").violations;
        assert!(v.iter().any(|x| x.contains("never closed")), "{v:?}");

        // TYPE_REQ — well-formed block, no type.
        let v = okf_check("---\ntitle: \"x\"\n---\nbody").violations;
        assert!(v.iter().any(|x| x.starts_with("concept-type")), "{v:?}");

        // TYPE_REQ — present but empty.
        let v = okf_check("---\ntype: \n---\nbody").violations;
        assert!(v.iter().any(|x| x.contains("empty")), "{v:?}");

        // LINK_TOL — a broken link is a WARNING in OKF, never a failure. If this
        // ever starts failing we have made the gate stricter than the spec.
        assert!(okf_check("---\ntype: X\n---\n[dead](./nope.md)").ok());
    }

    #[test]
    fn frontmatter_survives_a_title_containing_yaml_metacharacters() {
        // An unquoted colon would terminate the scalar and break FM_REQ, taking
        // the whole document out of conformance because of a report title.
        let mut c = campaign();
        c.status.as_mut().unwrap().phase = Some("Completed".into());
        let exps: Vec<Experiment> = vec![];
        let runs: BTreeMap<String, Vec<&BenchmarkRun>> = BTreeMap::new();
        let empty: Vec<String> = vec![];
        let sections = BTreeMap::new();
        let refs: Vec<Reference> = vec![];
        let cur = Curation {
            title: Some("Spot: a study of #gait, \"quoted\""),
            included: &empty,
            excluded: &empty,
            sections: &sections,
            seeded_hypotheses: &empty,
            references: &refs,
            about: None,
        };
        let mut doc = String::new();
        render(
            &mut doc,
            "camp",
            "ns",
            &c,
            &template(),
            &exps,
            &runs,
            Some(&cur),
        )
        .unwrap();
        let chk = okf_check(&doc);
        assert!(chk.ok(), "{:?}\n{doc}", chk.violations);
        assert!(
            doc.contains("status: stable"),
            "completed campaign -> stable"
        );
    }

    #[test]
    fn about_scopes_the_document_to_a_subtree() {
        use crate::experiment::DerivationRelation as R;
        use crate::research_report::{ReportMotivation, ReportSubject};
        // c-000 -> c-001 -> c-003 ; c-002 is a separate branch off the root.
        let exps = vec![
            with_lineage(
                experiment("c-000", "root", 1.0, ExperimentDecision::Keep),
                R::Baseline,
                None,
                0,
            ),
            with_lineage(
                experiment("c-001", "a", 2.0, ExperimentDecision::Keep),
                R::Perturb,
                Some("c-000"),
                1,
            ),
            with_lineage(
                experiment("c-002", "b", 3.0, ExperimentDecision::Keep),
                R::Perturb,
                Some("c-000"),
                1,
            ),
            with_lineage(
                experiment("c-003", "a2", 4.0, ExperimentDecision::Keep),
                R::Perturb,
                Some("c-001"),
                2,
            ),
        ];
        let sub = descendants_of(&exps, "c-001");
        assert!(sub.contains("c-001") && sub.contains("c-003"), "{sub:?}");
        assert!(
            !sub.contains("c-002"),
            "sibling branch must be excluded: {sub:?}"
        );
        assert!(!sub.contains("c-000"), "ancestor must be excluded: {sub:?}");

        let empty: Vec<String> = vec![];
        let sections = BTreeMap::new();
        let refs: Vec<Reference> = vec![];
        let subject = ReportSubject {
            kind: "Experiment".into(),
            name: "c-001".into(),
            motivation: Some(ReportMotivation::Assessing),
        };
        let cur = Curation {
            title: None,
            included: &empty,
            excluded: &empty,
            sections: &sections,
            seeded_hypotheses: &empty,
            references: &refs,
            about: Some(&subject),
        };
        let kept: Vec<String> = curate(&exps, Some(&cur))
            .iter()
            .map(|e| e.name_any())
            .collect();
        assert_eq!(
            kept,
            vec!["c-001".to_string(), "c-003".to_string()],
            "{kept:?}"
        );
    }

    #[test]
    fn descendants_terminates_on_a_malformed_cycle() {
        use crate::experiment::DerivationRelation as R;
        // A cycle cannot be produced by the generator (parents are always
        // already-complete experiments), but a hand-edited object could create
        // one, and the controller must not hang on it.
        let exps = vec![
            with_lineage(
                experiment("x", "", 1.0, ExperimentDecision::Keep),
                R::Perturb,
                Some("y"),
                1,
            ),
            with_lineage(
                experiment("y", "", 1.0, ExperimentDecision::Keep),
                R::Perturb,
                Some("x"),
                1,
            ),
        ];
        let sub = descendants_of(&exps, "x");
        assert_eq!(sub.len(), 2, "must terminate and cover both: {sub:?}");
    }

    #[test]
    fn search_tree_nests_by_lineage_and_surfaces_role_counts() {
        use crate::experiment::DerivationRelation as R;
        let exps = vec![
            with_lineage(
                experiment("c-000", "baseline", 1.0, ExperimentDecision::Keep),
                R::Baseline,
                None,
                0,
            ),
            with_lineage(
                experiment("c-001", "child", 2.0, ExperimentDecision::Discard),
                R::Perturb,
                Some("c-000"),
                1,
            ),
            with_lineage(
                experiment("c-002", "control", 3.0, ExperimentDecision::Discard),
                R::Remeasure,
                Some("c-000"),
                1,
            ),
        ];
        let refs: Vec<&Experiment> = exps.iter().collect();
        let forest = build_forest(&refs);
        assert_eq!(forest.len(), 1, "one root");
        assert_eq!(forest[0].children.len(), 2, "two children of the baseline");

        let runs: BTreeMap<String, Vec<&BenchmarkRun>> = BTreeMap::new();
        let mut doc = String::new();
        render(
            &mut doc,
            "camp",
            "ns",
            &campaign(),
            &template(),
            &exps,
            &runs,
            None,
        )
        .unwrap();
        assert!(doc.contains("## Search Tree"), "tree section missing");
        assert!(doc.contains("c-000 [Baseline"), "{doc}");
        assert!(
            doc.contains("  c-001 [Perturb"),
            "child must be indented: {doc}"
        );
        // The role census is what makes a degenerate campaign visible: v72 spent
        // 9 of 12 slots on controls and searched nothing, which a flat list hid.
        assert!(
            doc.contains("Remeasure: 1") && doc.contains("Perturb: 1"),
            "{doc}"
        );
    }

    #[test]
    fn a_dangling_parent_becomes_a_root_rather_than_vanishing() {
        use crate::experiment::DerivationRelation as R;
        // Parent not present in the campaign (deleted, or a stale name reused).
        // The node must still appear — silently pruning a subtree would hide
        // real work from the report.
        let exps = vec![with_lineage(
            experiment("c-005", "orphan", 1.0, ExperimentDecision::Keep),
            R::Perturb,
            Some("c-999-missing"),
            2,
        )];
        let refs: Vec<&Experiment> = exps.iter().collect();
        let forest = build_forest(&refs);
        assert_eq!(forest.len(), 1, "orphan must surface as a root");
        assert_eq!(forest[0].exp.name_any(), "c-005");
    }

    #[test]
    fn results_report_the_unbiased_metric_not_only_the_biased_one() {
        let exps: Vec<Experiment> = Vec::new();
        let runs: BTreeMap<String, Vec<&BenchmarkRun>> = BTreeMap::new();
        let mut c = campaign();
        let st = c.status.as_mut().unwrap();
        st.best_objective = Some(6352.0);
        st.incumbent_remeasured = Some(4522.0);
        st.seed_noise_sigma = Some(1447.0);
        st.control_runs = 9;
        let mut doc = String::new();
        render(&mut doc, "camp", "ns", &c, &template(), &exps, &runs, None).unwrap();
        assert!(doc.contains("Incumbent Re-measured"), "{doc}");
        assert!(doc.contains("Seed noise (sigma)"), "{doc}");
        // The bias must be stated, not left for the reader to compute.
        assert!(
            doc.contains("1830.0000"),
            "divergence must be rendered: {doc}"
        );
        assert!(doc.contains("NOT evidence of progress"), "{doc}");
    }

    #[test]
    fn render_is_deterministic() {
        let exps = vec![experiment("e", "h", 2.0, ExperimentDecision::Keep)];
        let runs: BTreeMap<String, Vec<&BenchmarkRun>> = BTreeMap::new();
        let mut a = String::new();
        render(
            &mut a,
            "camp",
            "ns",
            &campaign(),
            &template(),
            &exps,
            &runs,
            None,
        )
        .unwrap();
        let mut b = String::new();
        render(
            &mut b,
            "camp",
            "ns",
            &campaign(),
            &template(),
            &exps,
            &runs,
            None,
        )
        .unwrap();
        assert_eq!(
            a, b,
            "render must be a pure function of inputs (no wall clock)"
        );
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
        let sections =
            BTreeMap::from([("Related Work".to_string(), "Prior art on Muon.".to_string())]);
        let seeds = vec!["wider models close the gap".to_string()];
        let cur = Curation {
            title: Some("Muon vs AdamW"),
            included: &[],
            excluded: &["exp-a".to_string()],
            sections: &sections,
            seeded_hypotheses: &seeds,
            references: &[],
            about: None,
        };
        let mut doc = String::new();
        render(
            &mut doc,
            "camp",
            "research",
            &campaign(),
            &template(),
            &exps,
            &runs,
            Some(&cur),
        )
        .expect("infallible");

        assert!(doc.contains("# Muon vs AdamW"), "{doc}");
        assert!(doc.contains("_Curated: 1 of 2 experiments_"));
        assert!(
            !doc.contains("exp-a"),
            "pruned experiment must not appear: {doc}"
        );
        assert!(doc.contains("exp-b"));
        assert!(doc.contains("## Related Work") && doc.contains("Prior art on Muon."));
        assert!(doc.contains("## Seeded Hypotheses / Future Work"));
        assert!(doc.contains("wider models close the gap"));
    }

    // Helper: build an experiment with a specified lr value instead of the
    // fixed 0.001 that `experiment()` uses.
    fn experiment_lr(name: &str, hypothesis: &str, val_bpb: f64, lr: f64) -> Experiment {
        Experiment {
            metadata: ObjectMeta {
                name: Some(name.to_string()),
                ..Default::default()
            },
            spec: ExperimentSpec {
                campaign_ref: "camp".into(),
                hypothesis: hypothesis.into(),
                parameters: BTreeMap::from([("lr".to_string(), serde_json::json!(lr))]),
                patch: None,
                checkpoint_policy: None,
                lineage: None,
                env: vec![],
            },
            status: Some(ExperimentStatus {
                phase: ExperimentPhase::Succeeded,
                metrics: BTreeMap::from([("val_bpb".to_string(), serde_json::json!(val_bpb))]),
                decision: Some(ExperimentDecision::Keep),
                ..Default::default()
            }),
        }
    }

    // render_latex must produce a document with balanced \begin{}/\end{} pairs,
    // the correct title, narrative sections, seeded hypotheses, and must omit
    // any excluded experiment.
    #[test]
    fn render_latex_compiles_shape() {
        let exps = vec![
            experiment("exp-a", "baseline", 2.34, ExperimentDecision::Discard),
            experiment("exp-b", "winner", 2.10, ExperimentDecision::Keep),
        ];
        let runs: BTreeMap<String, Vec<&BenchmarkRun>> = BTreeMap::new();
        let sections =
            BTreeMap::from([("Related Work".to_string(), "Prior art on Muon.".to_string())]);
        let seeds = vec!["wider models close the gap".to_string()];
        let cur = Curation {
            title: Some("Muon vs AdamW"),
            included: &[],
            excluded: &["exp-a".to_string()],
            sections: &sections,
            seeded_hypotheses: &seeds,
            references: &[],
            about: None,
        };
        let mut doc = String::new();
        render_latex(
            &mut doc,
            "camp",
            "research",
            &campaign(),
            &template(),
            &exps,
            &runs,
            Some(&cur),
        )
        .expect("infallible");

        assert!(doc.contains(r"\documentclass"), "missing preamble: {doc}");
        assert!(doc.contains(r"\end{document}"), "missing end: {doc}");
        assert!(doc.contains("Muon vs AdamW"), "title missing: {doc}");
        assert!(
            doc.contains(r"\section{Related Work}"),
            "narrative section missing: {doc}"
        );
        assert!(
            doc.contains("wider models close the gap"),
            "seeded hypothesis missing: {doc}"
        );
        assert!(
            !doc.contains("exp-a"),
            "pruned experiment must not appear: {doc}"
        );
        assert!(doc.contains("exp-b"), "included experiment missing: {doc}");

        // Every \begin{ must have a matching \end{.
        let begins = doc.matches(r"\begin{").count();
        let ends = doc.matches(r"\end{").count();
        assert_eq!(
            begins, ends,
            "unbalanced \\begin{{}} / \\end{{}} (begins={begins}, ends={ends}):\n{doc}"
        );
    }

    // Hypotheses with special LaTeX characters must appear only in escaped form;
    // three experiments with distinct lr values must produce a pgfplots figure.
    #[test]
    fn render_latex_escapes_and_plots() {
        let raw_hypothesis = "test 50% & $5 #1_lr";
        let exps = vec![
            experiment_lr("exp-x", raw_hypothesis, 2.3, 0.01),
            experiment_lr("exp-y", raw_hypothesis, 2.2, 0.02),
            experiment_lr("exp-z", raw_hypothesis, 2.1, 0.04),
        ];
        let runs: BTreeMap<String, Vec<&BenchmarkRun>> = BTreeMap::new();
        let mut doc = String::new();
        render_latex(
            &mut doc,
            "camp",
            "research",
            &campaign(),
            &template(),
            &exps,
            &runs,
            None,
        )
        .expect("infallible");

        // Raw special-char substring must not survive into the output.
        assert!(
            !doc.contains("50% & $5"),
            "raw special chars must not appear unescaped: {doc}"
        );
        // Their escaped equivalents must be present.
        assert!(
            doc.contains(r"50\% \& \$5"),
            "escaped form must be present: {doc}"
        );

        // Sensitivity figure: lr has 3 distinct values so a plot is expected.
        assert!(
            doc.contains(r"\addplot coordinates"),
            "sensitivity figure must be emitted: {doc}"
        );
        assert!(
            doc.contains("(0.02,"),
            "lr=0.02 coordinate must appear in plot: {doc}"
        );
    }

    // render_latex must be a pure function of its inputs (no wall-clock).
    #[test]
    fn render_latex_is_deterministic() {
        let exps = vec![experiment("e", "h", 2.0, ExperimentDecision::Keep)];
        let runs: BTreeMap<String, Vec<&BenchmarkRun>> = BTreeMap::new();
        let mut a = String::new();
        render_latex(
            &mut a,
            "camp",
            "ns",
            &campaign(),
            &template(),
            &exps,
            &runs,
            None,
        )
        .unwrap();
        let mut b = String::new();
        render_latex(
            &mut b,
            "camp",
            "ns",
            &campaign(),
            &template(),
            &exps,
            &runs,
            None,
        )
        .unwrap();
        assert_eq!(
            a, b,
            "render_latex must be a pure function of inputs (no wall clock)"
        );
    }

    // citation_check must identify cited-but-undefined and defined-but-uncited keys.
    #[test]
    fn citation_check_finds_mismatches() {
        let sections = BTreeMap::from([(
            "Analysis".to_string(),
            "See [@good] and also [@ghost].".to_string(),
        )]);
        let refs = vec![
            Reference {
                key: "good".into(),
                title: "Good Paper".into(),
                url: None,
                doi: None,
                supports: None,
            },
            Reference {
                key: "orphan".into(),
                title: "Orphan Paper".into(),
                url: None,
                doi: None,
                supports: None,
            },
        ];
        let check = citation_check(&sections, &refs);
        assert_eq!(check.cited_undefined, vec!["ghost".to_string()]);
        assert_eq!(check.defined_uncited, vec!["orphan".to_string()]);
    }

    // render() and render_latex() must handle references: markdown lists them
    // and surfaces reconciliation warnings; LaTeX replaces [@key] with \cite{key}
    // and emits a bibliography.
    #[test]
    fn render_includes_references_and_cites() {
        let exps = vec![experiment(
            "exp-a",
            "baseline",
            2.34,
            ExperimentDecision::Keep,
        )];
        let runs: BTreeMap<String, Vec<&BenchmarkRun>> = BTreeMap::new();
        // Sections cite [@good] (defined) and [@ghost] (undefined).
        let sections = BTreeMap::from([(
            "Analysis".to_string(),
            "See [@good] and also [@ghost].".to_string(),
        )]);
        // References define "good" (cited) and "orphan" (never cited).
        let refs = vec![
            Reference {
                key: "good".into(),
                title: "Good Paper".into(),
                url: Some("https://example.com/good".into()),
                doi: None,
                supports: None,
            },
            Reference {
                key: "orphan".into(),
                title: "Orphan Paper".into(),
                url: None,
                doi: None,
                supports: None,
            },
        ];
        let seeds: Vec<String> = vec![];
        let cur = Curation {
            title: Some("Citation Test"),
            included: &[],
            excluded: &[],
            sections: &sections,
            seeded_hypotheses: &seeds,
            references: &refs,
            about: None,
        };

        // ── Markdown checks ───────────────────────────────────────────────────
        let mut md = String::new();
        render(
            &mut md,
            "camp",
            "research",
            &campaign(),
            &template(),
            &exps,
            &runs,
            Some(&cur),
        )
        .expect("infallible");

        assert!(md.contains("## References"), "missing ## References: {md}");
        assert!(md.contains("Good Paper"), "missing reference title: {md}");
        assert!(
            md.contains("## Citation Reconciliation"),
            "missing ## Citation Reconciliation: {md}"
        );
        assert!(
            md.contains("ghost"),
            "ghost (cited but undefined) not in reconciliation: {md}"
        );
        assert!(
            md.contains("orphan"),
            "orphan (defined but uncited) not in reconciliation: {md}"
        );

        // ── LaTeX checks ──────────────────────────────────────────────────────
        let mut tex = String::new();
        render_latex(
            &mut tex,
            "camp",
            "research",
            &campaign(),
            &template(),
            &exps,
            &runs,
            Some(&cur),
        )
        .expect("infallible");

        assert!(
            tex.contains(r"\cite{good}"),
            r"missing \cite{{good}}: {tex}"
        );
        assert!(
            tex.contains(r"\bibitem{good}"),
            r"missing \bibitem{{good}}: {tex}"
        );
        assert!(
            tex.contains("thebibliography"),
            "missing thebibliography env: {tex}"
        );
        assert!(
            !tex.contains("[@good]"),
            "literal [@good] must be replaced by \\cite in latex: {tex}"
        );
    }
}

/// One node of the derivation forest.
pub struct TreeNode<'a> {
    pub exp: &'a Experiment,
    pub children: Vec<TreeNode<'a>>,
}

/// Build the derivation forest from `spec.lineage.parent`.
///
/// Roots are nodes with no parent, or whose parent is not in this campaign
/// (a dangling edge — kept as a root rather than dropped, so a broken pointer
/// is visible in the document instead of silently pruning a subtree).
/// Experiments with no lineage at all are skipped; the caller reports that.
pub fn build_forest<'a>(exps: &[&'a Experiment]) -> Vec<TreeNode<'a>> {
    let present: std::collections::HashSet<String> = exps.iter().map(|e| e.name_any()).collect();
    let mut children_of: BTreeMap<String, Vec<&'a Experiment>> = BTreeMap::new();
    let mut roots: Vec<&'a Experiment> = Vec::new();
    for e in exps.iter().copied() {
        let Some(l) = &e.spec.lineage else { continue };
        match l.parent.as_ref().filter(|p| present.contains(*p)) {
            Some(parent) => children_of.entry(parent.clone()).or_default().push(e),
            None => roots.push(e),
        }
    }
    roots.into_iter().map(|r| attach(r, &children_of)).collect()
}

fn attach<'a>(
    exp: &'a Experiment,
    children_of: &BTreeMap<String, Vec<&'a Experiment>>,
) -> TreeNode<'a> {
    // Depth is bounded by generation count in practice; a cycle would need a
    // parent pointer to an ancestor, which the generator cannot produce
    // (parents are always already-completed experiments).
    let children = children_of
        .get(&exp.name_any())
        .map(|kids| kids.iter().map(|k| attach(k, children_of)).collect())
        .unwrap_or_default();
    TreeNode { exp, children }
}

fn render_tree_node(out: &mut String, node: &TreeNode<'_>, depth: usize) -> std::fmt::Result {
    let pad = "  ".repeat(depth);
    let name = node.exp.name_any();
    let l = node.exp.spec.lineage.as_ref();
    let role = l
        .map(|l| format!("{:?}", l.relation))
        .unwrap_or_else(|| "?".into());
    let generation = l
        .and_then(|l| l.generation)
        .map(|g| format!("g{g}"))
        .unwrap_or_default();
    let deltas = l
        .map(|l| {
            l.perturbations
                .iter()
                .map(|d| match d.factor {
                    Some(f) => format!("{} x{:.2}", d.param, f),
                    None => format!("{}={:.4}", d.param, d.to),
                })
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default();
    let obj = node
        .exp
        .status
        .as_ref()
        .and_then(|s| s.metrics_detail.as_ref())
        .and_then(|m| m.best.as_ref())
        .and_then(|v| v.as_f64())
        .map(|v| format!("{v:.4}"))
        .unwrap_or_else(|| "-".into());
    writeln!(
        out,
        "{pad}{name} [{role}{}{}] obj={obj}{}",
        if generation.is_empty() { "" } else { " " },
        generation,
        if deltas.is_empty() {
            String::new()
        } else {
            format!("  ({deltas})")
        }
    )?;
    for c in &node.children {
        render_tree_node(out, c, depth + 1)?;
    }
    Ok(())
}

/// Quote a YAML scalar so a colon, `#`, or leading indicator cannot break the
/// frontmatter block — an unparseable block fails OKF's FM_REQ outright.
pub fn yaml_scalar(v: &str) -> String {
    format!("\"{}\"", v.replace('\\', "\\\\").replace('"', "\\\""))
}

/// Latest experiment completion time, used as the document's data cutoff.
///
/// Deliberately data-derived rather than `now()`: render() must stay pure so
/// the reconciler's content-diff works. Returns None when nothing has completed.
pub fn data_cutoff(exps: &[&Experiment]) -> Option<String> {
    exps.iter()
        .filter_map(|e| {
            e.status
                .as_ref()
                .and_then(|s| s.conditions.as_ref())
                .and_then(|cs| {
                    cs.iter()
                        .filter_map(|c| c.last_transition_time.clone())
                        .max()
                })
        })
        .max()
}

/// Outcome of checking a rendered document against OKF's v0.1 hard rules.
#[derive(Debug, Clone, PartialEq)]
pub struct OkfCheck {
    /// Hard-rule breaches. Non-empty means the reference validator would exit
    /// non-zero, so publication is blocked.
    pub violations: Vec<String>,
    /// Link problems. OKF's LINK_TOL makes these WARNINGS — they must never
    /// block publication — but the reference validator still reports them, and
    /// a dangling artifact pointer in a research record is worth surfacing.
    pub warnings: Vec<String>,
}

impl OkfCheck {
    pub fn ok(&self) -> bool {
        self.violations.is_empty()
    }
}

/// Validate a rendered dossier against the Open Knowledge Format hard rules.
///
/// OKF (Google Cloud, 2026) defines exactly three conditions the reference
/// validator exits non-zero on, and this mirrors them so a malformed document
/// is caught before it is published rather than by a downstream consumer:
///
///   UTF8_REQ  valid UTF-8            (a Rust `&str` is UTF-8 by construction,
///                                     so this cannot fail here; checked anyway
///                                     for control characters that break YAML)
///   FM_REQ    parseable YAML frontmatter delimited by `---`
///   TYPE_REQ  `type` present and non-empty
///
/// Broken links are LINK_TOL — warnings only, never a failure — so they are
/// deliberately not checked here.
///
/// This is a native implementation rather than a shell-out: the operator runs
/// as a distroless container with no PVC and no package manager, and the three
/// rules are small enough that vendoring a Go/Ruby CLI to enforce them would
/// add far more risk than it removes. The external `openknowledge validate`
/// remains the conformance authority for CI.
pub fn okf_check(doc: &str) -> OkfCheck {
    let mut violations = Vec::new();
    let warnings = link_warnings(doc);

    // concept-frontmatter: the block must open on line 1 and close later.
    let mut lines = doc.lines();
    if lines.next().map(str::trim_end) != Some("---") {
        violations
            .push("concept-frontmatter: concept document is missing YAML frontmatter".to_string());
        return OkfCheck {
            violations,
            warnings,
        };
    }
    let mut fm = Vec::new();
    let mut closed = false;
    for line in lines {
        if line.trim_end() == "---" {
            closed = true;
            break;
        }
        fm.push(line);
    }
    if !closed {
        violations
            .push("concept-frontmatter: frontmatter block is never closed with `---`".to_string());
        return OkfCheck {
            violations,
            warnings,
        };
    }

    // UTF8_REQ: &str is UTF-8 by construction; what can still break a YAML
    // parser is a raw control character, so that is what we look for.
    if fm
        .iter()
        .any(|l| l.chars().any(|c| c.is_control() && c != '\t'))
    {
        violations.push("utf8-content: frontmatter contains a control character".to_string());
    }

    // TYPE_REQ: a top-level `type:` key with a non-empty value.
    let type_value = fm
        .iter()
        .find(|l| l.starts_with("type:"))
        .map(|l| l.trim_start_matches("type:").trim().trim_matches('"'));
    match type_value {
        None => violations
            .push("concept-type: concept frontmatter must include non-empty type".to_string()),
        Some("") => violations.push(
            "concept-type: concept frontmatter must include non-empty type (empty)".to_string(),
        ),
        Some(_) => {}
    }

    OkfCheck {
        violations,
        warnings,
    }
}

/// Static link integrity for a rendered dossier.
///
/// OKF's LINK_TOL makes broken links warnings rather than failures, so these
/// never block publication — but the reference validator does report them, and
/// an artifact pointer that goes nowhere is exactly the kind of rot that makes
/// a research record untrustworthy while still "passing".
///
/// STATIC ONLY — deliberately no network. A reconcile loop that fetched every
/// URL would be slow, flaky, and non-deterministic, which would defeat the
/// content-diff that stops the controller hot-looping. Liveness is already
/// covered where it belongs: the citation audit workload fetches sources and
/// returns UNREACHABLE.
pub fn link_warnings(doc: &str) -> Vec<String> {
    let mut out = Vec::new();

    // Footnote citations must resolve to a definition. OKF v0.2 keys citations
    // to a source `id` via `[^id]`, so a reference with no definition is a
    // citation pointing at nothing.
    let mut refs: Vec<String> = Vec::new();
    let mut defs: std::collections::HashSet<String> = std::collections::HashSet::new();
    for line in doc.lines() {
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix("[^") {
            if let Some(end) = rest.find("]:") {
                defs.insert(rest[..end].to_string());
                continue;
            }
        }
        let mut rest = line;
        while let Some(i) = rest.find("[^") {
            rest = &rest[i + 2..];
            let Some(end) = rest.find(']') else { break };
            // A definition line was handled above; this is a use.
            if !rest[end..].starts_with("]:") {
                refs.push(rest[..end].to_string());
            }
            rest = &rest[end + 1..];
        }
    }
    let mut dangling: Vec<String> = refs.into_iter().filter(|r| !defs.contains(r)).collect();
    dangling.sort();
    dangling.dedup();
    for d in dangling {
        out.push(format!(
            "link-target: citation [^{d}] has no matching source definition"
        ));
    }

    // Artifact pointers rendered into the document. A URI that is empty, or
    // that is a bare scheme with nothing after it, is a dead pointer written
    // by convention rather than by observation — exactly what
    // ExperimentArtifacts does when it stamps paths whether or not the file
    // exists.
    for line in doc.lines() {
        for tok in line.split(|c: char| c.is_whitespace() || c == '|' || c == '`') {
            let t = tok.trim();
            for scheme in ["gs://", "s3://", "http://", "https://", "configmap://"] {
                if let Some(rest) = t.strip_prefix(scheme) {
                    if rest.is_empty() || rest.starts_with('/') {
                        out.push(format!("link-target: malformed URI `{t}`"));
                    }
                }
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

/// Rewrite `[@key]` citations to OKF v0.2 footnote form `[^key]`.
///
/// Only well-formed keys are rewritten, using the same charset as
/// `extract_citation_keys`, so malformed patterns are left exactly as the
/// author wrote them rather than being silently mangled.
pub fn cites_to_footnotes(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let bytes = text.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == b'[' && i + 1 < bytes.len() && bytes[i + 1] == b'@' {
            let start = i + 2;
            let mut j = start;
            while j < bytes.len() && is_key_char(bytes[j]) {
                j += 1;
            }
            if j > start && j < bytes.len() && bytes[j] == b']' {
                out.push_str("[^");
                out.push_str(&text[start..j]);
                out.push(']');
                i = j + 1;
                continue;
            }
        }
        let ch = text[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}
