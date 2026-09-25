//! Tiny axum backend for the Dioxus console.
//!
//! Reuses `athena-api` + `kube` (which only work natively) to list the Athena
//! custom resources, collapses each into the wasm-safe [`models`] DTOs, and
//! serves them as JSON. It also proxies resource/template manifests as YAML via
//! `kubectl` (matching the native console) and serves the built SPA.
//!
//! Run with: `cargo run -p athena-console-web --features server --bin console-server`
//! Endpoints:
//! Endpoints are declared once in [`api::ops`]; `GET /api/openapi.json` is
//! generated from that registry and the same registry is served as MCP tools
//! at `POST /mcp`. `GET /*` serves the SPA (ATHENA_CONSOLE_DIST, default ./dist).

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
        .route(
            "/api/reports/:namespace/:name",
            get(get_report).put(replace_report),
        )
        .route("/api/reports/preview", post(preview_report))
        // Filtered, newest-first lists (the agent-facing read surface).
        .route("/api/campaigns", get(api::list_campaigns))
        .route("/api/experiments", get(api::list_experiments))
        .route("/api/reports", get(api::list_reports).post(create_report))
        .route("/api/drives", get(api::list_drives))
        // One registry describes every endpoint: OpenAPI for humans/tools,
        // MCP tools for agents. Neither is hand-written JSON.
        .route("/api/openapi.json", get(api::openapi))
        .route("/mcp", post(api::mcp).get(api::mcp_get))
        .fallback_service(ServeDir::new(dist));

    let listener = tokio::net::TcpListener::bind(&addr).await?;
    api::set_self_addr(listener.local_addr()?);
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

// ---------------------------------------------------------------------------
// API registry: one declaration per endpoint drives the OpenAPI document and
// the MCP tool list. Schemas come from the Rust types (schemars), so the
// published contract cannot drift from what the handlers accept and return.
// MCP tool calls are dispatched as real HTTP requests to this server, so an
// agent gets exactly the REST semantics (validation, 409 conflicts) the
// console gets. Writes are ResearchReport specs only; no status, no deletes.
// ---------------------------------------------------------------------------
mod api {
    use super::*;
    use axum::extract::Query;
    use schemars::JsonSchema;
    use schemars::r#gen::{SchemaGenerator, SchemaSettings};
    use schemars::schema::Schema;
    use serde::{Deserialize, Serialize};
    use serde_json::{Value, json};
    use std::net::SocketAddr;
    use std::sync::OnceLock;

    const DEFAULT_LIMIT: usize = 50;
    const MAX_LIMIT: usize = 500;

    // --- list endpoints -------------------------------------------------------

    /// Filters for `GET /api/campaigns`.
    #[derive(Deserialize, JsonSchema, Default)]
    pub struct CampaignQuery {
        /// Whitespace-separated terms; all must match name, template or progress text.
        pub query: Option<String>,
        /// Only campaigns owned by this ResearchDrive.
        pub drive: Option<String>,
        /// Only this phase (e.g. Running, Completed, InferenceFailed).
        pub phase: Option<String>,
        /// Max items (default 50, max 500).
        pub limit: Option<usize>,
    }

    /// Filters for `GET /api/experiments`.
    #[derive(Deserialize, JsonSchema, Default)]
    pub struct ExperimentQuery {
        /// Only experiments of this campaign.
        pub campaign: Option<String>,
        /// Whitespace-separated terms; all must match name, hypothesis or status message.
        pub query: Option<String>,
        /// Only this phase (e.g. Succeeded, Failed).
        pub phase: Option<String>,
        /// Only this decision (Keep, Discard, NeedsReview).
        pub decision: Option<String>,
        /// Max items (default 50, max 500).
        pub limit: Option<usize>,
    }

    /// Filters for `GET /api/reports`.
    #[derive(Deserialize, JsonSchema, Default)]
    pub struct ReportQuery {
        /// Only reports of this campaign.
        pub campaign: Option<String>,
        /// Whitespace-separated terms; all must match name, title or section text.
        pub query: Option<String>,
        /// Max items (default 50, max 500).
        pub limit: Option<usize>,
    }

    /// Newest-first page of results.
    #[derive(Serialize, JsonSchema)]
    pub struct Page<T> {
        /// Matches before the limit.
        pub total: usize,
        pub items: Vec<T>,
    }

    /// Report listing row: headings only; `GET /api/reports/{ns}/{name}` has the text.
    #[derive(Serialize, JsonSchema)]
    pub struct ReportListItem {
        pub namespace: String,
        pub name: String,
        pub campaign: String,
        pub title: String,
        pub phase: String,
        pub created_at: Option<String>,
        pub sections: Vec<String>,
        pub seeded_hypotheses: usize,
        pub excluded: usize,
    }

    type ApiResult<T> = Result<Json<T>, (StatusCode, String)>;

    fn terms_match(query: Option<&str>, hay: &[&str]) -> bool {
        let Some(q) = query.filter(|q| !q.trim().is_empty()) else {
            return true;
        };
        let hay = hay.join(" ").to_lowercase();
        q.split_whitespace()
            .all(|t| hay.contains(&t.to_lowercase()))
    }

    fn eq_opt(want: Option<&str>, have: Option<&str>) -> bool {
        want.is_none_or(|w| have.is_some_and(|h| h.eq_ignore_ascii_case(w)))
    }

    fn page<T>(
        mut items: Vec<T>,
        created: impl Fn(&T) -> Option<&String>,
        limit: Option<usize>,
    ) -> Page<T> {
        items.sort_by_key(|i| {
            std::cmp::Reverse(
                created(i)
                    .and_then(|s| s.parse::<i64>().ok())
                    .unwrap_or(i64::MIN),
            )
        });
        let total = items.len();
        items.truncate(limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT));
        Page { total, items }
    }

    async fn snap() -> Result<ClusterSnapshot, (StatusCode, String)> {
        load_snapshot().await.map_err(ise)
    }

    pub async fn list_campaigns(
        Query(q): Query<CampaignQuery>,
    ) -> ApiResult<Page<ResourceSummary>> {
        let items = snap()
            .await?
            .campaigns
            .into_iter()
            .filter(|c| eq_opt(q.drive.as_deref(), c.drive.as_deref()))
            .filter(|c| eq_opt(q.phase.as_deref(), Some(&c.phase)))
            .filter(|c| {
                terms_match(
                    q.query.as_deref(),
                    &[&c.name, c.template.as_deref().unwrap_or(""), &c.detail],
                )
            })
            .collect();
        Ok(Json(page(items, |c| c.created_at.as_ref(), q.limit)))
    }

    pub async fn list_experiments(
        Query(q): Query<ExperimentQuery>,
    ) -> ApiResult<Page<ResourceSummary>> {
        let items = snap()
            .await?
            .experiments
            .into_iter()
            .filter(|e| eq_opt(q.campaign.as_deref(), e.campaign.as_deref()))
            .filter(|e| eq_opt(q.phase.as_deref(), Some(&e.phase)))
            .filter(|e| eq_opt(q.decision.as_deref(), e.decision.as_deref()))
            .filter(|e| {
                terms_match(
                    q.query.as_deref(),
                    &[&e.name, e.hypothesis.as_deref().unwrap_or(""), &e.detail],
                )
            })
            .collect();
        Ok(Json(page(items, |e| e.created_at.as_ref(), q.limit)))
    }

    pub async fn list_reports(Query(q): Query<ReportQuery>) -> ApiResult<Page<ReportListItem>> {
        let items = snap()
            .await?
            .reports
            .into_iter()
            .filter(|r| q.campaign.as_deref().is_none_or(|c| r.campaign_ref == c))
            .filter(|r| {
                let mut hay = vec![r.name.as_str(), r.title.as_str()];
                hay.extend(r.sections.values().map(String::as_str));
                terms_match(q.query.as_deref(), &hay)
            })
            .map(|r| ReportListItem {
                sections: r.sections.keys().cloned().collect(),
                seeded_hypotheses: r.seeded_hypotheses.len(),
                excluded: r.excluded_count,
                namespace: r.namespace,
                name: r.name,
                campaign: r.campaign_ref,
                title: r.title,
                phase: r.phase,
                created_at: r.created_at,
            })
            .collect();
        Ok(Json(page(items, |r| r.created_at.as_ref(), q.limit)))
    }

    pub async fn list_drives() -> ApiResult<Vec<DriveSummary>> {
        Ok(Json(snap().await?.drives))
    }

    // --- registry ---------------------------------------------------------------

    type SchemaFn = fn(&mut SchemaGenerator) -> Schema;

    fn schema<T: JsonSchema>(g: &mut SchemaGenerator) -> Schema {
        g.subschema_for::<T>()
    }

    /// One HTTP operation. `id` is both the OpenAPI operationId and the MCP
    /// tool name.
    pub struct Op {
        pub id: &'static str,
        pub method: &'static str,
        /// Path template; `{param}` segments are string path parameters.
        pub path: &'static str,
        pub summary: &'static str,
        pub query: Option<SchemaFn>,
        pub body: Option<SchemaFn>,
        /// `None` = `text/plain` response.
        pub response: Option<SchemaFn>,
        /// Exposed as an MCP tool.
        pub tool: bool,
    }

    pub fn ops() -> Vec<Op> {
        vec![
            Op {
                id: "list_campaigns",
                method: "get",
                path: "/api/campaigns",
                summary: "Campaigns newest first: drive, template, strategy, phase, counts, best experiment/objective, conditions.",
                query: Some(schema::<CampaignQuery>),
                body: None,
                response: Some(schema::<Page<ResourceSummary>>),
                tool: true,
            },
            Op {
                id: "list_experiments",
                method: "get",
                path: "/api/experiments",
                summary: "Experiments newest first: campaign, template, phase, decision, objective and best value, lineage parent/generation, runtime, hypothesis. Search here for prior art before proposing work.",
                query: Some(schema::<ExperimentQuery>),
                body: None,
                response: Some(schema::<Page<ResourceSummary>>),
                tool: true,
            },
            Op {
                id: "list_reports",
                method: "get",
                path: "/api/reports",
                summary: "ResearchReports newest first with section headings. The research memory: conclusions, footguns, negative results.",
                query: Some(schema::<ReportQuery>),
                body: None,
                response: Some(schema::<Page<ReportListItem>>),
                tool: true,
            },
            Op {
                id: "list_drives",
                method: "get",
                path: "/api/drives",
                summary: "ResearchDrives: phase, current curriculum stage, stagnation, conditions, per-stage template gate evidence.",
                query: None,
                body: None,
                response: Some(schema::<Vec<DriveSummary>>),
                tool: true,
            },
            Op {
                id: "get_report",
                method: "get",
                path: "/api/reports/{namespace}/{name}",
                summary: "Full editable ResearchReport spec (including resource_version) plus controller status and conditions.",
                query: None,
                body: None,
                response: Some(schema::<ReportDetailDto>),
                tool: true,
            },
            Op {
                id: "get_manifest",
                method: "get",
                path: "/api/manifest/{namespace}/{kind}/{name}",
                summary: "YAML of one resource (kind: experiment, researchcampaign, researchreport, researchdrive, experimenttemplate, runtimeprofile, benchmarksuite, benchmarkrun).",
                query: None,
                body: None,
                response: None,
                tool: true,
            },
            Op {
                id: "get_scheduling",
                method: "get",
                path: "/api/scheduling",
                summary: "GPU scheduling: Kueue pools and workloads, node power, inference backends.",
                query: None,
                body: None,
                response: Some(schema::<athena_api::scheduling::SchedulingSnapshot>),
                tool: true,
            },
            Op {
                id: "preview_report",
                method: "post",
                path: "/api/reports/preview",
                summary: "Compose the dossier Markdown for an unsaved report spec. Writes nothing.",
                query: None,
                body: Some(schema::<ReportSpecDto>),
                response: None,
                tool: true,
            },
            Op {
                id: "create_report",
                method: "post",
                path: "/api/reports",
                summary: "Create a ResearchReport (spec only). 400 if the name is not a DNS label or the campaign is missing; 409 if the name exists.",
                query: None,
                body: Some(schema::<ReportSpecDto>),
                response: Some(schema::<ReportDetailDto>),
                tool: true,
            },
            Op {
                id: "update_report",
                method: "put",
                path: "/api/reports/{namespace}/{name}",
                summary: "Replace a ResearchReport spec. body.resource_version must come from get_report; 409 if the report changed since (reload, reapply). Pass references/about through unchanged.",
                query: None,
                body: Some(schema::<ReportSpecDto>),
                response: Some(schema::<ReportDetailDto>),
                tool: true,
            },
            Op {
                id: "get_snapshot",
                method: "get",
                path: "/api/snapshot",
                summary: "Everything the console shows, unfiltered (large).",
                query: None,
                body: None,
                response: Some(schema::<ClusterSnapshot>),
                tool: false,
            },
        ]
    }

    fn path_params(path: &str) -> Vec<&str> {
        path.split('/')
            .filter_map(|seg| seg.strip_prefix('{').and_then(|s| s.strip_suffix('}')))
            .collect()
    }

    fn to_value(s: Schema) -> Value {
        serde_json::to_value(s).unwrap_or_default()
    }

    /// Query-struct properties as OpenAPI query parameters.
    fn query_params(g: &mut SchemaGenerator, f: SchemaFn) -> Vec<Value> {
        let root = to_value(f(g));
        let resolved = resolve(&root, g);
        let required: Vec<String> = resolved["required"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        resolved["properties"]
            .as_object()
            .map(|props| {
                props
                    .iter()
                    .map(|(name, schema)| {
                        json!({
                            "name": name, "in": "query",
                            "required": required.contains(name),
                            "description": schema.get("description").cloned().unwrap_or(Value::Null),
                            "schema": schema,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Follow a single `$ref` into the generator's definitions.
    fn resolve(v: &Value, g: &SchemaGenerator) -> Value {
        if let Some(r) = v.get("$ref").and_then(Value::as_str) {
            let name = r.rsplit('/').next().unwrap_or("");
            if let Some(def) = g.definitions().get(name) {
                return serde_json::to_value(def).unwrap_or_default();
            }
        }
        v.clone()
    }

    /// `GET /api/openapi.json` — OpenAPI 3.0 generated from [`ops`].
    pub async fn openapi() -> Json<Value> {
        Json(openapi_doc())
    }

    pub fn openapi_doc() -> Value {
        let mut g = SchemaSettings::openapi3().into_generator();
        let mut paths = serde_json::Map::new();
        for op in ops() {
            let mut params: Vec<Value> = path_params(op.path)
                .into_iter()
                .map(|p| json!({ "name": p, "in": "path", "required": true, "schema": { "type": "string" } }))
                .collect();
            if let Some(q) = op.query {
                params.extend(query_params(&mut g, q));
            }
            let response = match op.response {
                Some(f) => json!({ "application/json": { "schema": to_value(f(&mut g)) } }),
                None => json!({ "text/plain": { "schema": { "type": "string" } } }),
            };
            let mut operation = json!({
                "operationId": op.id,
                "summary": op.summary,
                "parameters": params,
                "responses": {
                    "200": { "description": "OK", "content": response },
                    "400": { "description": "Invalid request" },
                    "409": { "description": "Conflict" },
                },
            });
            if let Some(b) = op.body {
                operation["requestBody"] = json!({
                    "required": true,
                    "content": { "application/json": { "schema": to_value(b(&mut g)) } },
                });
            }
            paths
                .entry(op.path.to_string())
                .or_insert_with(|| json!({}))
                .as_object_mut()
                .expect("path item is an object")
                .insert(op.method.to_string(), operation);
        }
        let schemas: serde_json::Map<String, Value> = g
            .take_definitions()
            .into_iter()
            .map(|(k, v)| (k, serde_json::to_value(v).unwrap_or_default()))
            .collect();
        json!({
            "openapi": "3.0.3",
            "info": {
                "title": "Athena console API",
                "version": env!("CARGO_PKG_VERSION"),
                "description": "Read Athena research resources; write ResearchReport specs only. Controllers own status. Timestamps are epoch-millis strings.",
            },
            "paths": paths,
            "components": { "schemas": schemas },
        })
    }

    // --- MCP ----------------------------------------------------------------------

    static SELF_ADDR: OnceLock<SocketAddr> = OnceLock::new();

    pub fn set_self_addr(addr: SocketAddr) {
        let _ = SELF_ADDR.set(addr);
    }

    const INSTRUCTIONS: &str = "Athena research platform (research.nixlab.io, namespace apps). \
The CRDs are the research record: search prior art with list_experiments / list_reports (query) \
before proposing work, and record conclusions as ResearchReports. Report writes are spec-only; \
update_report needs the resource_version from get_report and fails with 409 if the report changed. \
Tools mirror the REST API in /api/openapi.json. Timestamps are epoch-millis strings.";

    /// MCP tool input schema for an op: path params + query fields + `body`.
    /// Self-contained (inlined subschemas) because MCP clients do not resolve refs.
    fn tool_schema(op: &Op) -> Value {
        let mut settings = SchemaSettings::draft07();
        settings.inline_subschemas = true;
        let mut g = settings.into_generator();
        let mut props = serde_json::Map::new();
        let mut required: Vec<Value> = Vec::new();
        for p in path_params(op.path) {
            let default = if p == "namespace" {
                " (use \"apps\")"
            } else {
                ""
            };
            props.insert(
                p.into(),
                json!({ "type": "string", "description": format!("path parameter{default}") }),
            );
            required.push(json!(p));
        }
        if let Some(q) = op.query {
            let qs = to_value(q(&mut g));
            if let Some(obj) = qs["properties"].as_object() {
                props.extend(obj.clone());
            }
        }
        if let Some(b) = op.body {
            props.insert("body".into(), to_value(b(&mut g)));
            required.push(json!("body"));
        }
        json!({ "type": "object", "properties": props, "required": required })
    }

    pub async fn mcp_get() -> impl IntoResponse {
        (StatusCode::METHOD_NOT_ALLOWED, "POST JSON-RPC 2.0 to /mcp")
    }

    pub async fn mcp(Json(req): Json<Value>) -> axum::response::Response {
        let Some(id) = req.get("id").cloned() else {
            // Notifications get no JSON-RPC response.
            return StatusCode::ACCEPTED.into_response();
        };
        let method = req.get("method").and_then(Value::as_str).unwrap_or("");
        let params = req.get("params").cloned().unwrap_or(Value::Null);
        let result = match method {
            "initialize" => Ok(json!({
                "protocolVersion": params.get("protocolVersion").and_then(Value::as_str).unwrap_or("2025-03-26"),
                "capabilities": { "tools": { "listChanged": false } },
                "serverInfo": { "name": "athena", "version": env!("CARGO_PKG_VERSION") },
                "instructions": INSTRUCTIONS,
            })),
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({
                "tools": ops().iter().filter(|o| o.tool).map(|o| json!({
                    "name": o.id,
                    "description": o.summary,
                    "inputSchema": tool_schema(o),
                })).collect::<Vec<_>>()
            })),
            "tools/call" => Ok(call(&params).await),
            other => {
                Err(json!({ "code": -32601, "message": format!("method not found: {other}") }))
            }
        };
        let body = match result {
            Ok(r) => json!({ "jsonrpc": "2.0", "id": id, "result": r }),
            Err(e) => json!({ "jsonrpc": "2.0", "id": id, "error": e }),
        };
        Json(body).into_response()
    }

    fn tool_result(text: String, is_error: bool) -> Value {
        json!({ "content": [{ "type": "text", "text": text }], "isError": is_error })
    }

    /// Execute a tool as the HTTP request it describes, against this server.
    async fn call(params: &Value) -> Value {
        let name = params.get("name").and_then(Value::as_str).unwrap_or("");
        let args = params
            .get("arguments")
            .cloned()
            .unwrap_or_else(|| json!({}));
        let Some(op) = ops().into_iter().find(|o| o.tool && o.id == name) else {
            return tool_result(format!("unknown tool: {name}"), true);
        };
        let Some(addr) = SELF_ADDR.get() else {
            return tool_result("server address not initialised".into(), true);
        };
        let mut path = op.path.to_string();
        for p in path_params(op.path) {
            let Some(v) = args
                .get(p)
                .and_then(Value::as_str)
                .filter(|v| !v.is_empty())
            else {
                return tool_result(format!("missing argument: {p}"), true);
            };
            if v.contains('/') {
                return tool_result(format!("invalid {p}: {v}"), true);
            }
            path = path.replace(&format!("{{{p}}}"), v);
        }
        let host = if addr.ip().is_unspecified() {
            "127.0.0.1".to_string()
        } else {
            addr.ip().to_string()
        };
        let url = format!("http://{host}:{}{path}", addr.port());
        let client = reqwest::Client::new();
        let mut req = match op.method {
            "get" => client.get(&url),
            "post" => client.post(&url),
            "put" => client.put(&url),
            _ => return tool_result("unsupported method".into(), true),
        };
        if op.query.is_some() {
            let pairs: Vec<(String, String)> = args
                .as_object()
                .into_iter()
                .flatten()
                .filter(|(k, _)| {
                    !path_params(op.path).contains(&k.as_str()) && k.as_str() != "body"
                })
                .filter_map(|(k, v)| match v {
                    Value::String(s) => Some((k.clone(), s.clone())),
                    Value::Number(n) => Some((k.clone(), n.to_string())),
                    Value::Bool(b) => Some((k.clone(), b.to_string())),
                    _ => None,
                })
                .collect();
            req = req.query(&pairs);
        }
        if op.body.is_some() {
            req = req.json(args.get("body").unwrap_or(&Value::Null));
        }
        match req.send().await {
            Ok(resp) => {
                let ok = resp.status().is_success();
                let status = resp.status();
                let text = resp.text().await.unwrap_or_default();
                // Pretty JSON reads better for models; text/plain passes through.
                let text = serde_json::from_str::<Value>(&text)
                    .ok()
                    .and_then(|v| serde_json::to_string_pretty(&v).ok())
                    .unwrap_or(text);
                if ok {
                    tool_result(text, false)
                } else {
                    tool_result(format!("HTTP {status}: {text}"), true)
                }
            }
            Err(e) => tool_result(e.to_string(), true),
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn openapi_documents_every_op_with_resolvable_refs() {
            let doc = openapi_doc();
            for op in ops() {
                assert!(
                    doc["paths"][op.path][op.method].is_object(),
                    "{} {}",
                    op.method,
                    op.path
                );
            }
            let text = doc.to_string();
            for r in text.split("\"$ref\":\"#/components/schemas/").skip(1) {
                let name = &r[..r.find('"').unwrap()];
                assert!(
                    doc["components"]["schemas"][name].is_object(),
                    "dangling ref {name}"
                );
            }
            let params = doc["paths"]["/api/experiments"]["get"]["parameters"]
                .as_array()
                .unwrap();
            assert!(
                params
                    .iter()
                    .any(|p| p["name"] == "decision" && p["in"] == "query")
            );
        }

        #[test]
        fn tool_schemas_are_self_contained_and_require_path_params() {
            for op in ops().iter().filter(|o| o.tool) {
                let s = tool_schema(op);
                assert!(!s.to_string().contains("$ref"), "{} has refs", op.id);
                for p in path_params(op.path) {
                    assert!(
                        s["required"].as_array().unwrap().contains(&json!(p)),
                        "{} {p}",
                        op.id
                    );
                }
            }
            let upd = ops().into_iter().find(|o| o.id == "update_report").unwrap();
            let s = tool_schema(&upd);
            assert!(s["properties"]["body"]["properties"]["resource_version"].is_object());
        }
    }
}
