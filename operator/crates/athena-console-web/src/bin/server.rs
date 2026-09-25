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
    ClusterSnapshot, ConditionDto, ConditionMessageDto, DriveSummary, ReportDetailDto,
    ReportSpecDto, ReportSummary, ResourceSummary, StageProgressDto, TemplateProgressDto,
    TemplateSummary,
};
use axum::{
    Json, Router,
    extract::Path,
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
};
use k8s_openapi::api::batch::v1::Job;
use kube::api::{Api, ListParams, PostParams};
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
        .route(
            "/api/reports/:namespace/:name",
            get(get_report).put(replace_report),
        )
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

    // Experiments carry no template ref; it lives on the owning campaign.
    let campaign_template: HashMap<String, String> = campaign_list
        .items
        .iter()
        .map(|c| (c.name_any(), c.spec.template_ref.clone()))
        .collect();
    // Campaigns get their objective metric/goal from their template.
    let template_objective: HashMap<String, (String, String)> = tpl_list
        .items
        .iter()
        .map(|t| {
            (
                t.name_any(),
                (
                    t.spec.objective.metric.clone(),
                    format!("{:?}", t.spec.objective.goal).to_lowercase(),
                ),
            )
        })
        .collect();

    let experiments = exp_list
        .items
        .into_iter()
        .map(|e| {
            let status = e.status.as_ref();
            let jt = job_times.get(&format!("exp-{}", e.name_any())).cloned();
            let detail = status.and_then(|s| s.metrics_detail.as_ref());
            let objective = detail.and_then(|d| d.objective_name.clone());
            let objective_value = detail
                .and_then(|d| d.best.as_ref())
                .zip(objective.as_deref())
                .and_then(|(best, name)| best.get(name))
                .and_then(|v| v.as_f64());
            let lineage = e.spec.lineage.as_ref();
            let cost = status.and_then(|s| s.cost.as_ref());
            // Controller-observed Job window; status.cost is not populated yet.
            let runtime_seconds = cost.and_then(|c| c.runtime_seconds).or_else(|| {
                let (s, en) = jt.as_ref()?;
                let s: i64 = s.as_ref()?.parse().ok()?;
                let en: i64 = en.as_ref()?.parse().ok()?;
                Some((en - s) / 1000)
            });
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
                created_at: to_ms(&e.metadata.creation_timestamp),
                drive: None,
                template: campaign_template.get(&e.spec.campaign_ref).cloned(),
                objective_goal: detail.and_then(|d| d.objective_goal.clone()),
                objective,
                objective_value,
                decision: status
                    .and_then(|s| s.decision.as_ref())
                    .map(|d| format!("{d:?}")),
                parent: lineage.and_then(|l| l.parent.clone()),
                generation: lineage.and_then(|l| l.generation),
                runtime_seconds,
                gpu_hours: cost.and_then(|c| c.gpu_hours),
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
                ..Default::default()
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
                created_at: to_ms(&c.metadata.creation_timestamp),
                // Owning drive, so the viewer can drop an echoed drive prefix
                // from legacy branch names without renaming the CR.
                drive: c
                    .metadata
                    .owner_references
                    .as_ref()
                    .and_then(|o| o.iter().find(|r| r.kind == "ResearchDrive"))
                    .map(|r| r.name.clone()),
                template: Some(c.spec.template_ref.clone()),
                strategy: Some(c.spec.strategy.strategy_type.clone()),
                objective: template_objective
                    .get(&c.spec.template_ref)
                    .map(|o| o.0.clone()),
                objective_goal: template_objective
                    .get(&c.spec.template_ref)
                    .map(|o| o.1.clone()),
                objective_value: status.and_then(|s| s.best_objective),
                best_experiment: status.and_then(|s| s.best_experiment.clone()),
                succeeded: status.map(|s| s.succeeded_experiments),
                failed: status.map(|s| s.failed_experiments),
                running: status.map(|s| s.running_experiments),
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
                ..Default::default()
            }
        })
        .collect();

    let templates = tpl_list
        .items
        .into_iter()
        .map(|t| TemplateSummary {
            namespace: t.namespace().unwrap_or_else(|| "default".to_string()),
            name: t.name_any(),
            created_at: to_ms(&t.metadata.creation_timestamp),
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
            created_at: to_ms(&s.metadata.creation_timestamp),
            drive: None,
            campaign: None,
            mode: None,
            hypothesis: None,
            conditions: Vec::new(),
            ..Default::default()
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
                created_at: to_ms(&r.metadata.creation_timestamp),
                drive: None,
                campaign: None,
                mode: None,
                hypothesis: None,
                conditions: Vec::new(),
                ..Default::default()
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
            created_at: to_ms(&p.metadata.creation_timestamp),
            drive: None,
            campaign: None,
            mode: None,
            hypothesis: None,
            conditions: Vec::new(),
            ..Default::default()
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
                created_at: to_ms(&r.metadata.creation_timestamp),
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
                created_at: to_ms(&d.metadata.creation_timestamp),
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

fn spec_from_dto(dto: &ReportSpecDto) -> Result<ResearchReportSpec, (StatusCode, String)> {
    let bad = |what: &str, e: serde_json::Error| {
        (StatusCode::BAD_REQUEST, format!("invalid {what}: {e}"))
    };
    Ok(ResearchReportSpec {
        campaign_ref: dto.campaign_ref.clone(),
        title: dto.title.clone(),
        included_experiments: dto.included_experiments.clone(),
        excluded_experiments: dto.excluded_experiments.clone(),
        sections: dto.sections.clone(),
        seeded_hypotheses: dto.seeded_hypotheses.clone(),
        references: if dto.references.is_null() {
            vec![]
        } else {
            serde_json::from_value(dto.references.clone()).map_err(|e| bad("references", e))?
        },
        about: if dto.about.is_null() {
            None
        } else {
            serde_json::from_value(dto.about.clone()).map_err(|e| bad("about", e))?
        },
    })
}

/// RFC 1123 DNS label rules for a new report name.
fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 63
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !name.starts_with('-')
        && !name.ends_with('-')
}

fn detail_of(r: &ResearchReport) -> ReportDetailDto {
    let status = r.status.as_ref();
    ReportDetailDto {
        spec: ReportSpecDto {
            namespace: r.namespace().unwrap_or_else(|| "default".to_string()),
            name: r.name_any(),
            campaign_ref: r.spec.campaign_ref.clone(),
            title: r.spec.title.clone(),
            included_experiments: r.spec.included_experiments.clone(),
            excluded_experiments: r.spec.excluded_experiments.clone(),
            sections: r.spec.sections.clone(),
            seeded_hypotheses: r.spec.seeded_hypotheses.clone(),
            references: serde_json::to_value(&r.spec.references).unwrap_or_default(),
            about: serde_json::to_value(&r.spec.about).unwrap_or_default(),
            resource_version: r.metadata.resource_version.clone(),
        },
        phase: status.and_then(|s| s.phase.clone()),
        included_count: status.and_then(|s| s.included_count),
        dataset_uri: status.and_then(|s| s.dataset_uri.clone()),
        last_assembled_time: status.and_then(|s| s.last_assembled_time.clone()),
        conditions: status
            .and_then(|s| s.conditions.clone())
            .unwrap_or_default()
            .into_iter()
            .map(|c| ConditionMessageDto {
                ctype: c.condition_type.unwrap_or_default(),
                status: c.status.unwrap_or_default(),
                reason: c.reason.unwrap_or_default(),
                message: c.message.unwrap_or_default(),
            })
            .collect(),
        created_at: to_ms(&r.metadata.creation_timestamp),
    }
}

/// `GET /api/reports/{ns}/{name}` — full editable spec + status.
async fn get_report(
    Path((namespace, name)): Path<(String, String)>,
) -> Result<Json<ReportDetailDto>, (StatusCode, String)> {
    let client = Client::try_default().await.map_err(ise)?;
    let reports: Api<ResearchReport> = Api::namespaced(client, &namespace);
    match reports.get_opt(&name).await.map_err(ise)? {
        Some(r) => Ok(Json(detail_of(&r))),
        None => Err((
            StatusCode::NOT_FOUND,
            format!("report {namespace}/{name} not found"),
        )),
    }
}

/// `PUT /api/reports/{ns}/{name}` — replace the spec of an existing report.
/// Requires the resourceVersion the draft was loaded from, so a concurrent
/// change (another curator, the drive's write-up) is a 409, not a silent
/// overwrite. Replace — not apply — so removing a section removes it.
async fn replace_report(
    Path((namespace, name)): Path<(String, String)>,
    Json(dto): Json<ReportSpecDto>,
) -> Result<Json<ReportDetailDto>, (StatusCode, String)> {
    let Some(rv) = dto.resource_version.clone() else {
        return Err((
            StatusCode::BAD_REQUEST,
            "resourceVersion is required to update".into(),
        ));
    };
    let spec = spec_from_dto(&dto)?;
    let client = Client::try_default().await.map_err(ise)?;
    let reports: Api<ResearchReport> = Api::namespaced(client, &namespace);
    let Some(mut current) = reports.get_opt(&name).await.map_err(ise)? else {
        return Err((
            StatusCode::NOT_FOUND,
            format!("report {namespace}/{name} not found"),
        ));
    };
    current.spec = spec;
    current.metadata.resource_version = Some(rv);
    match reports
        .replace(&name, &PostParams::default(), &current)
        .await
    {
        Ok(r) => Ok(Json(detail_of(&r))),
        Err(kube::Error::Api(e)) if e.code == 409 => Err((
            StatusCode::CONFLICT,
            "the report changed since it was loaded; reload it and reapply your edits".into(),
        )),
        Err(e) => Err((StatusCode::BAD_GATEWAY, e.to_string())),
    }
}

fn ise<E: std::fmt::Display>(e: E) -> (StatusCode, String) {
    (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
}

/// `POST /api/reports` — create a new ResearchReport (spec only; the
/// controller owns status). Rejects invalid names, a missing campaign, and an
/// existing name (409): updates go through `PUT` with a resourceVersion.
async fn create_report(
    Json(dto): Json<ReportSpecDto>,
) -> Result<Json<ReportDetailDto>, (StatusCode, String)> {
    if !valid_name(&dto.name) {
        return Err((
            StatusCode::BAD_REQUEST,
            "name must be a DNS label: lowercase letters, digits, '-', max 63".into(),
        ));
    }
    if dto.campaign_ref.trim().is_empty() {
        return Err((StatusCode::BAD_REQUEST, "campaignRef is required".into()));
    }
    let spec = spec_from_dto(&dto)?;
    let client = Client::try_default().await.map_err(ise)?;
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
    let report = ResearchReport::new(&dto.name, spec);
    let reports: Api<ResearchReport> = Api::namespaced(client, &dto.namespace);
    match reports.create(&PostParams::default(), &report).await {
        Ok(r) => Ok(Json(detail_of(&r))),
        Err(kube::Error::Api(e)) if e.code == 409 => Err((
            StatusCode::CONFLICT,
            format!("a report named '{}' already exists", dto.name),
        )),
        Err(e) => Err((StatusCode::BAD_GATEWAY, e.to_string())),
    }
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
    let spec = spec_from_dto(&dto)?;
    let curation = Curation::from_spec(&spec);
    dossier::assemble(&client, &dto.campaign_ref, &dto.namespace, Some(&curation))
        .await
        .map(|d| d.markdown)
        .map_err(|e| (StatusCode::BAD_GATEWAY, e.to_string()))
}
