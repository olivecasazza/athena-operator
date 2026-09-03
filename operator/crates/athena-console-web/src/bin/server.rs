//! Tiny axum backend for the Dioxus console.
//!
//! Reuses `athena-api` + `kube` (which only work natively) to list the Athena
//! custom resources, collapses each into the wasm-safe [`models`] DTOs, and
//! serves them as JSON. It also proxies resource/template manifests as YAML via
//! `kubectl` (matching the native console) and serves the built SPA.
//!
//! Run with: `cargo run -p athena-console-web --features server --bin console-server`
//! Endpoints:
//!   GET /api/snapshot                       -> ClusterSnapshot JSON
//!   GET /api/manifest/{namespace}/{kind}/{name} -> resource YAML (text)
//!   GET /api/template/{namespace}/{name}    -> ExperimentTemplate YAML (text)
//!   GET /*                                  -> static SPA (ATHENA_CONSOLE_DIST, default ./dist)

use athena_api::benchmark_run::BenchmarkRun;
use athena_api::benchmark_suite::BenchmarkSuite;
use athena_api::dossier::{self, Curation};
use athena_api::experiment::Experiment;
use athena_api::experiment_template::ExperimentTemplate;
use athena_api::research_campaign::ResearchCampaign;
use athena_api::research_drive::ResearchDrive;
use athena_api::research_report::{ResearchReport, ResearchReportSpec};
use athena_api::runtime_profile::RuntimeProfile;
use athena_console_web::models::{
    ClusterSnapshot, ConditionDto, DriveSummary, ReportSpecDto, ReportSummary, ResourceSummary,
    StageProgressDto, TemplateProgressDto, TemplateSummary,
};
use axum::{
    Json, Router,
    extract::Path,
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
};
use k8s_openapi::api::batch::v1::Job;
use kube::api::{Api, ListParams, Patch, PatchParams};
use kube::{Client, ResourceExt};
use std::collections::HashMap;
use std::process::Command;
use tower_http::services::ServeDir;

/// k8s `Time` → epoch-millis string (for scoping the Grafana embed).
fn to_ms(t: &Option<k8s_openapi::apimachinery::pkg::apis::meta::v1::Time>) -> Option<String> {
    t.as_ref().map(|x| x.0.timestamp_millis().to_string())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let dist = std::env::var("ATHENA_CONSOLE_DIST").unwrap_or_else(|_| "dist".to_string());
    let addr = std::env::var("ATHENA_CONSOLE_ADDR").unwrap_or_else(|_| "0.0.0.0:8080".to_string());

    let app = Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route("/api/snapshot", get(snapshot))
        .route("/api/scheduling", get(scheduling))
        .route("/api/manifest/:namespace/:kind/:name", get(manifest))
        .route("/api/template/:namespace/:name", get(template))
        // Report curation: persist a ResearchReport (spec only) and preview its
        // composed dossier from an unsaved draft.
        .route("/api/reports", post(create_report))
        .route("/api/reports/preview", post(preview_report))
        .fallback_service(ServeDir::new(dist));

    let listener = tokio::net::TcpListener::bind(&addr).await?;
    eprintln!("athena-console server listening on http://{addr}");
    axum::serve(listener, app).await?;
    Ok(())
}

async fn snapshot() -> Result<Json<ClusterSnapshot>, (StatusCode, String)> {
    load_snapshot()
        .await
        .map(Json)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

/// `GET /api/scheduling` → the GPU-scheduling/inference stack snapshot (Kueue
/// pools + workloads, Hephaestus node power, inference backends) for the admin
/// views. Reuses the shared `athena_api::scheduling` reader; camelCase wire.
async fn scheduling()
-> Result<Json<athena_api::scheduling::SchedulingSnapshot>, (StatusCode, String)> {
    let client = Client::try_default()
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(athena_api::scheduling::read_scheduling(&client).await))
}

async fn manifest(
    Path((namespace, kind, name)): Path<(String, String, String)>,
) -> impl IntoResponse {
    kubectl_yaml(&namespace, &kind, &name)
}

async fn template(Path((namespace, name)): Path<(String, String)>) -> impl IntoResponse {
    kubectl_yaml(&namespace, "experimenttemplate", &name)
}

/// `kubectl -n <ns> get <kind> <name> -o yaml` — same approach as the native
/// console's manifest loader.
fn kubectl_yaml(namespace: &str, kind: &str, name: &str) -> (StatusCode, String) {
    let output = Command::new("kubectl")
        .args(["-n", namespace, "get", kind, name, "-o", "yaml"])
        .output();
    match output {
        Ok(out) if out.status.success() => (
            StatusCode::OK,
            String::from_utf8_lossy(&out.stdout).into_owned(),
        ),
        Ok(out) => (
            StatusCode::BAD_GATEWAY,
            String::from_utf8_lossy(&out.stderr).trim().to_string(),
        ),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("failed to run kubectl: {e}"),
        ),
    }
}

async fn load_snapshot() -> anyhow::Result<ClusterSnapshot> {
    let client = Client::try_default().await?;
    let lp = ListParams::default().limit(500);

    let experiments_api = Api::<Experiment>::all(client.clone());
    let campaigns_api = Api::<ResearchCampaign>::all(client.clone());
    let templates_api = Api::<ExperimentTemplate>::all(client.clone());
    let suites_api = Api::<BenchmarkSuite>::all(client.clone());
    let runs_api = Api::<BenchmarkRun>::all(client.clone());
    let jobs_api = Api::<Job>::all(client.clone());
    let profiles_api = Api::<RuntimeProfile>::all(client.clone());
    let drives_api = Api::<ResearchDrive>::all(client.clone());
    let reports_api = Api::<ResearchReport>::all(client);

    let (
        exp_list,
        campaign_list,
        tpl_list,
        suite_list,
        run_list,
        profile_list,
        job_list,
        report_list,
        drive_list,
    ) = tokio::try_join!(
        experiments_api.list(&lp),
        campaigns_api.list(&lp),
        templates_api.list(&lp),
        suites_api.list(&lp),
        runs_api.list(&lp),
        profiles_api.list(&lp),
        jobs_api.list(&lp),
        reports_api.list(&lp),
        drives_api.list(&lp),
    )?;

    // Run windows from the experiment Jobs (exp-<name>) so the embed can scope its
    // Grafana time range to when the experiment actually ran.
    let job_times: HashMap<String, (Option<String>, Option<String>)> = job_list
        .items
        .into_iter()
        .filter_map(|j| {
            let name = j.metadata.name.clone()?;
            let st = j.status.as_ref();
            Some((
                name,
                (
                    st.and_then(|s| to_ms(&s.start_time)),
                    st.and_then(|s| to_ms(&s.completion_time)),
                ),
            ))
        })
        .collect();

    let experiments = exp_list
        .items
        .into_iter()
        .map(|e| {
            let status = e.status.as_ref();
            let jt = job_times.get(&format!("exp-{}", e.name_any())).cloned();
            ResourceSummary {
                namespace: e.namespace().unwrap_or_else(|| "default".to_string()),
                name: e.name_any(),
                kind: "experiment".to_string(),
                phase: status
                    .map(|s| format!("{:?}", s.phase))
                    .unwrap_or_else(|| "Pending".to_string()),
                detail: status
                    .and_then(|s| s.message.clone())
                    .unwrap_or_else(|| e.spec.hypothesis.clone()),
                workspace_path: status.and_then(|s| s.workspace_path.clone()),
                logs_link: status.and_then(|s| s.logs_link.clone()),
                metrics_link: status.and_then(|s| s.metrics_link.clone()),
                started_at: jt.as_ref().and_then(|(s, _)| s.clone()),
                ended_at: jt.as_ref().and_then(|(_, en)| en.clone()),
                campaign: Some(e.spec.campaign_ref.clone()),
                mode: e
                    .spec
                    .parameters
                    .get("mode")
                    .and_then(|v| v.as_str())
                    .map(String::from),
                // Truncated: the drill-down shows it inline; a 2 KB hypothesis
                // in every row would bloat the snapshot for no reader.
                hypothesis: Some(e.spec.hypothesis.chars().take(240).collect()),
                conditions: Vec::new(),
            }
        })
        .collect();

    let campaigns = campaign_list
        .items
        .into_iter()
        .map(|c| {
            let status = c.status.as_ref();
            ResourceSummary {
                namespace: c.namespace().unwrap_or_else(|| "default".to_string()),
                name: c.name_any(),
                kind: "researchcampaign".to_string(),
                phase: status
                    .and_then(|s| s.phase.clone())
                    .unwrap_or_else(|| "Pending".to_string()),
                detail: status
                    .map(|s| {
                        format!(
                            "running={} succeeded={} failed={} best={}",
                            s.running_experiments,
                            s.succeeded_experiments,
                            s.failed_experiments,
                            s.best_experiment.clone().unwrap_or_else(|| "-".to_string())
                        )
                    })
                    .unwrap_or_else(|| format!("template={}", c.spec.template_ref)),
                workspace_path: None,
                logs_link: None,
                metrics_link: None,
                // Campaign window: created → now (ended None ⇒ embed uses "now").
                started_at: to_ms(&c.metadata.creation_timestamp),
                ended_at: None,
                campaign: None,
                // Campaign mode needs a template fetch per campaign; the drive
                // summary carries stage context instead, so None is honest here.
                mode: None,
                hypothesis: None,
                conditions: status
                    .and_then(|s| s.conditions.clone())
                    .unwrap_or_default()
                    .into_iter()
                    .map(|c| ConditionDto {
                        ctype: c.condition_type.unwrap_or_default(),
                        status: c.status.unwrap_or_default(),
                        reason: c.reason.unwrap_or_default(),
                    })
                    .collect(),
            }
        })
        .collect();

    let templates = tpl_list
        .items
        .into_iter()
        .map(|t| TemplateSummary {
            namespace: t.namespace().unwrap_or_else(|| "default".to_string()),
            name: t.name_any(),
            objective: format!("{} / {:?}", t.spec.objective.metric, t.spec.objective.goal),
            detail: format!(
                "runtime={} source={}",
                t.spec.runtime_profile_ref, t.spec.source.git.url
            ),
        })
        .collect();

    let benchmark_suites = suite_list
        .items
        .into_iter()
        .map(|s| ResourceSummary {
            namespace: s.namespace().unwrap_or_else(|| "default".to_string()),
            name: s.name_any(),
            kind: "benchmarksuite".to_string(),
            phase: s
                .status
                .as_ref()
                .map(|st| if st.ready { "Ready" } else { "NotReady" }.to_string())
                .unwrap_or_else(|| "No status".to_string()),
            detail: format!("{:?} tasks={}", s.spec.taxonomy, s.spec.tasks.len()),
            workspace_path: None,
            logs_link: None,
            metrics_link: None,
            started_at: None,
            ended_at: None,
            campaign: None,
            mode: None,
            hypothesis: None,
            conditions: Vec::new(),
        })
        .collect();

    let benchmark_runs = run_list
        .items
        .into_iter()
        .map(|r| {
            let status = r.status.as_ref();
            ResourceSummary {
                namespace: r.namespace().unwrap_or_else(|| "default".to_string()),
                name: r.name_any(),
                kind: "benchmarkrun".to_string(),
                phase: status
                    .map(|s| format!("{:?}", s.phase))
                    .unwrap_or_else(|| "Pending".to_string()),
                detail: r
                    .spec
                    .output
                    .as_ref()
                    .and_then(|o| o.workspace_path.clone())
                    .unwrap_or_else(|| format!("suite={}", r.spec.suite_ref.name)),
                workspace_path: r
                    .spec
                    .output
                    .as_ref()
                    .and_then(|o| o.workspace_path.clone()),
                logs_link: status.and_then(|s| s.logs_link.clone()),
                metrics_link: status.and_then(|s| s.metrics_link.clone()),
                started_at: None,
                ended_at: None,
                campaign: None,
                mode: None,
                hypothesis: None,
                conditions: Vec::new(),
            }
        })
        .collect();

    let runtime_profiles = profile_list
        .items
        .into_iter()
        .map(|p| ResourceSummary {
            namespace: p.namespace().unwrap_or_else(|| "default".to_string()),
            name: p.name_any(),
            kind: "runtimeprofile".to_string(),
            phase: p
                .status
                .as_ref()
                .map(|st| if st.ready { "Ready" } else { "NotReady" }.to_string())
                .unwrap_or_else(|| "No status".to_string()),
            detail: format!(
                "{:?} {:?} image={}",
                p.spec.runtime.runtime_type, p.spec.runtime.mode, p.spec.image
            ),
            workspace_path: None,
            logs_link: None,
            metrics_link: None,
            started_at: None,
            ended_at: None,
            campaign: None,
            mode: None,
            hypothesis: None,
            conditions: Vec::new(),
        })
        .collect();

    let reports = report_list
        .items
        .into_iter()
        .map(|r| {
            let status = r.status.as_ref();
            ReportSummary {
                namespace: r.namespace().unwrap_or_else(|| "default".to_string()),
                name: r.name_any(),
                campaign_ref: r.spec.campaign_ref.clone(),
                title: r.spec.title.clone().unwrap_or_default(),
                phase: status
                    .and_then(|s| s.phase.clone())
                    .unwrap_or_else(|| "Draft".to_string()),
                excluded_count: r.spec.excluded_experiments.len(),
                sections: r.spec.sections.clone(),
                seeded_hypotheses: r.spec.seeded_hypotheses.clone(),
            }
        })
        .collect();

    let drives = drive_list
        .items
        .into_iter()
        .map(|d| {
            let st = d.status.clone().unwrap_or_default();
            let cur = st.curriculum.unwrap_or_default();
            DriveSummary {
                namespace: d.namespace().unwrap_or_else(|| "default".to_string()),
                name: d.name_any(),
                phase: st
                    .phase
                    .map(|p| format!("{p:?}"))
                    .unwrap_or_else(|| "Pending".to_string()),
                stage: cur.current_stage.clone(),
                stagnation: st.stagnation_counter,
                conditions: st
                    .conditions
                    .into_iter()
                    .map(|c| ConditionDto {
                        ctype: c.condition_type,
                        status: format!("{:?}", c.status),
                        reason: c.reason.unwrap_or_default(),
                    })
                    .collect(),
                stages: cur
                    .stage_history
                    .into_iter()
                    .map(|h| StageProgressDto {
                        name: h.name,
                        promoted_at: h.promoted_at,
                        templates: h
                            .template_progress
                            .into_iter()
                            .map(|t| TemplateProgressDto {
                                template_ref: t.template_ref,
                                best_objective: t.best_objective,
                                succeeded: t.succeeded_experiments,
                                passed: t.passed,
                            })
                            .collect(),
                    })
                    .collect(),
            }
        })
        .collect();

    Ok(ClusterSnapshot {
        experiments,
        campaigns,
        templates,
        benchmark_suites,
        benchmark_runs,
        runtime_profiles,
        reports,
        drives,
    })
}

// ---------------------------------------------------------------------------
// Report curation — the console's only WRITE path. Writes ResearchReport SPEC
// only (never status), via server-side apply with field manager "athena-console".
// ---------------------------------------------------------------------------

fn spec_from_dto(dto: &ReportSpecDto) -> ResearchReportSpec {
    ResearchReportSpec {
        campaign_ref: dto.campaign_ref.clone(),
        title: dto.title.clone(),
        included_experiments: dto.included_experiments.clone(),
        excluded_experiments: dto.excluded_experiments.clone(),
        sections: dto.sections.clone(),
        seeded_hypotheses: dto.seeded_hypotheses.clone(),
        references: vec![],
        about: None,
    }
}

fn ise<E: std::fmt::Display>(e: E) -> (StatusCode, String) {
    (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
}

/// `POST /api/reports` — validate + upsert a ResearchReport spec. Rejects an
/// empty name/campaign or a campaignRef that does not exist in the namespace.
async fn create_report(
    Json(dto): Json<ReportSpecDto>,
) -> Result<Json<ReportSummary>, (StatusCode, String)> {
    if dto.name.trim().is_empty() || dto.campaign_ref.trim().is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            "name and campaignRef are required".to_string(),
        ));
    }
    let client = Client::try_default().await.map_err(ise)?;

    // Validate the referenced campaign exists before persisting the report.
    let campaigns: Api<ResearchCampaign> = Api::namespaced(client.clone(), &dto.namespace);
    if campaigns
        .get_opt(&dto.campaign_ref)
        .await
        .map_err(ise)?
        .is_none()
    {
        return Err((
            StatusCode::BAD_REQUEST,
            format!(
                "campaign '{}' not found in namespace '{}'",
                dto.campaign_ref, dto.namespace
            ),
        ));
    }

    // Server-side apply needs apiVersion/kind/metadata in the body (a typed CR
    // does not serialize them), so apply a JSON document carrying spec only.
    let spec = spec_from_dto(&dto);
    let body = serde_json::json!({
        "apiVersion": "research.nixlab.io/v1alpha1",
        "kind": "ResearchReport",
        "metadata": { "name": dto.name, "namespace": dto.namespace },
        "spec": spec,
    });
    let reports: Api<ResearchReport> = Api::namespaced(client, &dto.namespace);
    reports
        .patch(
            &dto.name,
            &PatchParams::apply("athena-console").force(),
            &Patch::Apply(&body),
        )
        .await
        .map_err(|e| (StatusCode::BAD_GATEWAY, e.to_string()))?;

    Ok(Json(ReportSummary {
        namespace: dto.namespace.clone(),
        name: dto.name.clone(),
        campaign_ref: dto.campaign_ref.clone(),
        title: dto.title.clone().unwrap_or_default(),
        phase: "Draft".to_string(),
        excluded_count: dto.excluded_experiments.len(),
        sections: dto.sections.clone(),
        seeded_hypotheses: dto.seeded_hypotheses.clone(),
    }))
}

/// `POST /api/reports/preview` — assemble the curated dossier Markdown for an
/// unsaved draft spec. Read-only; nothing is persisted.
async fn preview_report(Json(dto): Json<ReportSpecDto>) -> Result<String, (StatusCode, String)> {
    if dto.campaign_ref.trim().is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            "campaignRef is required".to_string(),
        ));
    }
    let client = Client::try_default().await.map_err(ise)?;
    let spec = spec_from_dto(&dto);
    let curation = Curation::from_spec(&spec);
    dossier::assemble(&client, &dto.campaign_ref, &dto.namespace, Some(&curation))
        .await
        .map(|d| d.markdown)
        .map_err(|e| (StatusCode::BAD_GATEWAY, e.to_string()))
}
