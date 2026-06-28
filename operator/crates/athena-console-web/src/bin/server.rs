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
use athena_api::experiment::Experiment;
use athena_api::experiment_template::ExperimentTemplate;
use athena_api::research_campaign::ResearchCampaign;
use athena_api::runtime_profile::RuntimeProfile;
use athena_console_web::models::{ClusterSnapshot, ResourceSummary, TemplateSummary};
use axum::{Json, Router, extract::Path, http::StatusCode, response::IntoResponse, routing::get};
use kube::api::{Api, ListParams};
use kube::{Client, ResourceExt};
use std::process::Command;
use tower_http::services::ServeDir;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let dist = std::env::var("ATHENA_CONSOLE_DIST").unwrap_or_else(|_| "dist".to_string());
    let addr = std::env::var("ATHENA_CONSOLE_ADDR").unwrap_or_else(|_| "0.0.0.0:8080".to_string());

    let app = Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route("/api/snapshot", get(snapshot))
        .route("/api/manifest/{namespace}/{kind}/{name}", get(manifest))
        .route("/api/template/{namespace}/{name}", get(template))
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
    let profiles_api = Api::<RuntimeProfile>::all(client);

    let (exp_list, campaign_list, tpl_list, suite_list, run_list, profile_list) = tokio::try_join!(
        experiments_api.list(&lp),
        campaigns_api.list(&lp),
        templates_api.list(&lp),
        suites_api.list(&lp),
        runs_api.list(&lp),
        profiles_api.list(&lp),
    )?;

    let experiments = exp_list
        .items
        .into_iter()
        .map(|e| {
            let status = e.status.as_ref();
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
        })
        .collect();

    Ok(ClusterSnapshot {
        experiments,
        campaigns,
        templates,
        benchmark_suites,
        benchmark_runs,
        runtime_profiles,
    })
}
