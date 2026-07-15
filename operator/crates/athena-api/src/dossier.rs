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

    // ── 8b. References (curated) ─────────────────────────────────────────────
    if let Some(c) = curation {
        if !c.references.is_empty() {
            writeln!(out, "## References")?;
            writeln!(out)?;
            for r in c.references {
                let mut line = format!("- **[{}]** {}", r.key, r.title);
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
                    writeln!(out, "cited but undefined: {}", check.cited_undefined.join(", "))?;
                }
                if !check.defined_uncited.is_empty() {
                    writeln!(out, "defined but never cited: {}", check.defined_uncited.join(", "))?;
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
    Ok(Dossier { markdown, latex, included })
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
pub fn citation_check(sections: &BTreeMap<String, String>, references: &[Reference]) -> CitationCheck {
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
        .filter_map(|r| if seen.insert(r.key.as_str()) { Some(r.key.clone()) } else { None })
        .collect();

    CitationCheck { cited_undefined, defined_uncited }
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
    writeln!(out, r"  \item Metric: \texttt{{{}}}", tex_escape(&obj_spec.metric))?;
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
    writeln!(out, r"  \item Max experiments: {}", campaign.spec.budget.max_experiments)?;
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
                    distinct.entry(k.clone()).or_default().insert(format!("{}", fv));
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
    let git_commit = template.spec.source.git.commit.as_deref().unwrap_or("not recorded");
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
                if parts.is_empty() { "not recorded".to_string() } else { parts.join(", ") }
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
                if parts.is_empty() { "not recorded".to_string() } else { parts.join(", ") }
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
        let workspace = artifacts.and_then(|a| a.workspace_uri.as_deref()).unwrap_or("\u{2014}");
        let journal = artifacts.and_then(|a| a.journal_uri.as_deref()).unwrap_or("\u{2014}");
        let provenance = artifacts.and_then(|a| a.provenance_uri.as_deref()).unwrap_or("\u{2014}");
        let checkpoints = artifacts.and_then(|a| a.checkpoints_uri.as_deref()).unwrap_or("\u{2014}");
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

    fn experiment(name: &str, hypothesis: &str, val_bpb: f64, decision: ExperimentDecision) -> Experiment {
        Experiment {
            metadata: ObjectMeta { name: Some(name.to_string()), ..Default::default() },
            spec: ExperimentSpec {
                campaign_ref: "camp".into(),
                hypothesis: hypothesis.into(),
                parameters: BTreeMap::from([("lr".to_string(), json!(0.001))]),
                patch: None,
                checkpoint_policy: None,
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
                inference_mesh: None,
                inference_cluster: None,
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
            references: &[],
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

    // Helper: build an experiment with a specified lr value instead of the
    // fixed 0.001 that `experiment()` uses.
    fn experiment_lr(
        name: &str,
        hypothesis: &str,
        val_bpb: f64,
        lr: f64,
    ) -> Experiment {
        Experiment {
            metadata: ObjectMeta { name: Some(name.to_string()), ..Default::default() },
            spec: ExperimentSpec {
                campaign_ref: "camp".into(),
                hypothesis: hypothesis.into(),
                parameters: BTreeMap::from([("lr".to_string(), serde_json::json!(lr))]),
                patch: None,
                checkpoint_policy: None,
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
        assert!(doc.contains(r"\section{Related Work}"), "narrative section missing: {doc}");
        assert!(doc.contains("wider models close the gap"), "seeded hypothesis missing: {doc}");
        assert!(!doc.contains("exp-a"), "pruned experiment must not appear: {doc}");
        assert!(doc.contains("exp-b"), "included experiment missing: {doc}");

        // Every \begin{ must have a matching \end{.
        let begins = doc.matches(r"\begin{").count();
        let ends = doc.matches(r"\end{").count();
        assert_eq!(begins, ends, "unbalanced \\begin{{}} / \\end{{}} (begins={begins}, ends={ends}):\n{doc}");
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
        render_latex(&mut a, "camp", "ns", &campaign(), &template(), &exps, &runs, None).unwrap();
        let mut b = String::new();
        render_latex(&mut b, "camp", "ns", &campaign(), &template(), &exps, &runs, None).unwrap();
        assert_eq!(a, b, "render_latex must be a pure function of inputs (no wall clock)");
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
        let exps = vec![experiment("exp-a", "baseline", 2.34, ExperimentDecision::Keep)];
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
        };

        // ── Markdown checks ───────────────────────────────────────────────────
        let mut md = String::new();
        render(&mut md, "camp", "research", &campaign(), &template(), &exps, &runs, Some(&cur))
            .expect("infallible");

        assert!(md.contains("## References"), "missing ## References: {md}");
        assert!(md.contains("Good Paper"), "missing reference title: {md}");
        assert!(
            md.contains("## Citation Reconciliation"),
            "missing ## Citation Reconciliation: {md}"
        );
        assert!(md.contains("ghost"), "ghost (cited but undefined) not in reconciliation: {md}");
        assert!(md.contains("orphan"), "orphan (defined but uncited) not in reconciliation: {md}");

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

        assert!(tex.contains(r"\cite{good}"), r"missing \cite{{good}}: {tex}");
        assert!(tex.contains(r"\bibitem{good}"), r"missing \bibitem{{good}}: {tex}");
        assert!(tex.contains("thebibliography"), "missing thebibliography env: {tex}");
        assert!(!tex.contains("[@good]"), "literal [@good] must be replaced by \\cite in latex: {tex}");
    }
}
