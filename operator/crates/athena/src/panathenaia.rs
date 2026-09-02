//! Panathenaia — the games held in honor of Athena; this module serves the gym
//! that watches her campaigns.
//!
//! Read-only BFF for the public robot gym (spot.casazza.io): real
//! ResearchCampaign / Experiment / ResearchReport DTOs plus filmstrip PNGs
//! from the shared workspace PVC, served same-origin at `/api/...` behind
//! nginx. Every field in the DTOs below is publicly visible BY DESIGN — this
//! is the backend of a public exhibit, so the response structs are the
//! security boundary: an explicit allowlist, never a CRD-spec passthrough.

use std::{collections::BTreeMap, net::SocketAddr, path::PathBuf, sync::Arc};

use anyhow::Context as _;
use athena_api::experiment::{Experiment, ExperimentCondition};
use athena_api::experiment_template::ExperimentTemplate;
use athena_api::research_campaign::ResearchCampaign;
use athena_api::research_report::ResearchReport;
use axum::{
    Json, Router,
    extract::{Path, Request, State},
    http::{HeaderValue, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::get,
};
use kube::{Api, Client, Resource, ResourceExt, api::ListParams};
use serde::Serialize;
use tracing::{error, info, warn};

/// Leading path the trainer stamps on artifact URIs (`figures_uri` etc.);
/// remapped onto PANATHENAIA_WORKSPACE_MOUNT at read time, because the mount
/// point inside this pod need not be called `/workspace`.
const WORKSPACE_PREFIX: &str = "/workspace";

/// Curriculum labels the gym groups campaigns by (set by the drive reconciler).
const ROBOT_LABEL: &str = "research.nixlab.io/curriculum-robot";
const STAGE_LABEL: &str = "research.nixlab.io/curriculum-stage";

struct AppState {
    client: Client,
    namespace: String,
    workspace_mount: PathBuf,
}

// --- DTOs: the response surface -------------------------------------------
//
// REDACTION IS BY CONSTRUCTION: these structs are an explicit allowlist of
// fields and nothing else is ever serialized. `ResearchCampaignSpec.proposer`
// carries an `apiKeySecretRef`, so a whole-spec passthrough would publish live
// credentials on a public endpoint. Never flatten a CRD type in here; add
// named fields only.

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CampaignsResponse {
    campaigns: Vec<CampaignDto>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CampaignDto {
    name: String,
    robot: Option<String>,
    stage: Option<String>,
    template_ref: String,
    objective_metric: Option<String>,
    objective_goal: Option<String>,
    phase: Option<String>,
    best_experiment: Option<String>,
    best_objective: Option<f64>,
    incumbent_remeasured: Option<f64>,
    succeeded: u32,
    failed: u32,
    running: u32,
    total: u32,
    canary_state: Option<String>,
    // Stance this campaign's template declares (forage/arena/etc.); the gym
    // derives its view from it, so the listing must carry it.
    #[serde(skip_serializing_if = "Option::is_none")]
    mode: Option<String>,
    created_at: Option<String>,
    experiments: Vec<ExperimentDto>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ExperimentDto {
    name: String,
    phase: Option<String>,
    hypothesis: String,
    created_at: Option<String>,
    objective: Option<f64>,
    metrics: BTreeMap<String, f64>,
    onnx_url: Option<String>,
    filmstrip_url: Option<String>,
    image: Option<String>,
    // What THIS experiment declares; see experiment_dto for why no fallback.
    #[serde(skip_serializing_if = "Option::is_none")]
    mode: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ExperimentDetailDto {
    #[serde(flatten)]
    experiment: ExperimentDto,
    parameters: BTreeMap<String, serde_json::Value>,
    conditions: Vec<ConditionDto>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ConditionDto {
    #[serde(rename = "type")]
    condition_type: Option<String>,
    status: Option<String>,
    reason: Option<String>,
    message: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ReportsResponse {
    reports: Vec<ReportDto>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ReportDto {
    name: String,
    campaign_ref: String,
    title: Option<String>,
    sections: BTreeMap<String, String>,
    seeded_hypotheses: Vec<String>,
    created_at: Option<String>,
}

/// Entry point for `athena panathenaia`.
pub async fn serve() -> anyhow::Result<()> {
    let addr: SocketAddr = std::env::var("PANATHENAIA_ADDR")
        .unwrap_or_else(|_| "0.0.0.0:8090".to_string())
        .parse()
        .with_context(|| "PANATHENAIA_ADDR is not a valid socket address")?;
    let namespace = std::env::var("PANATHENAIA_NAMESPACE").unwrap_or_else(|_| "apps".to_string());
    let workspace_mount = PathBuf::from(
        std::env::var("PANATHENAIA_WORKSPACE_MOUNT")
            .unwrap_or_else(|_| WORKSPACE_PREFIX.to_string()),
    );
    let client = Client::try_default().await?;

    let state = Arc::new(AppState {
        client,
        namespace,
        workspace_mount,
    });

    let app = Router::new()
        .route("/api/v1/campaigns", get(list_campaigns))
        .route("/api/v1/experiments/:name", get(get_experiment))
        .route("/api/v1/experiments/:name/figures/:file", get(get_figure))
        .route("/api/v1/reports", get(list_reports))
        .route("/healthz", get(health))
        .layer(middleware::from_fn(cors_allow_all))
        .with_state(state);

    info!(%addr, "starting Panathenaia gym BFF");
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

/// Blanket `access-control-allow-origin: *`. The gym calls same-origin through
/// nginx today, but may later be embedded cross-origin from gym.casazza.io;
/// this crate does not depend on tower-http, so the header is added by hand
/// rather than pulling a dependency for one line.
async fn cors_allow_all(req: Request, next: Next) -> Response {
    let mut res = next.run(req).await;
    res.headers_mut().insert(
        header::ACCESS_CONTROL_ALLOW_ORIGIN,
        HeaderValue::from_static("*"),
    );
    res
}

async fn health() -> impl IntoResponse {
    (StatusCode::OK, "ok")
}

async fn list_campaigns(State(state): State<Arc<AppState>>) -> Response {
    // No cache: every request lists fresh from the API. A stale cache in a
    // viewer misreports live research (campaigns finish, experiments flip
    // phase), and the load is humans clicking, not a hot loop.
    let campaigns: Api<ResearchCampaign> = Api::namespaced(state.client.clone(), &state.namespace);
    let experiments: Api<Experiment> = Api::namespaced(state.client.clone(), &state.namespace);
    let (campaign_list, experiment_list) = match (
        campaigns.list(&ListParams::default()).await,
        experiments.list(&ListParams::default()).await,
    ) {
        (Ok(c), Ok(e)) => (c, e),
        (Err(e), _) | (_, Err(e)) => {
            error!(%e, "failed to list campaigns/experiments");
            return internal("listing failed");
        }
    };

    // Membership is spec.campaignRef, NOT the campaign label: hand-authored
    // experiments carry the ref but not the label, so a label-only query
    // silently under-counts — the exact bug the campaign reconciler fixed by
    // adopting ref-matched orphans.
    let mut by_campaign: BTreeMap<&str, Vec<&Experiment>> = BTreeMap::new();
    for exp in &experiment_list.items {
        by_campaign
            .entry(exp.spec.campaign_ref.as_str())
            .or_default()
            .push(exp);
    }
    let mut items = campaign_list.items;
    items.sort_by(|a, b| a.name_any().cmp(&b.name_any()));

    let mut dtos = Vec::with_capacity(items.len());
    for campaign in &items {
        let name = campaign.name_any();
        let (objective_metric, objective_goal, mode) =
            campaign_template_fields(&state.client, &state.namespace, &campaign.spec.template_ref)
                .await;

        let mut exps = by_campaign.remove(name.as_str()).unwrap_or_default();
        sort_experiments(&mut exps);

        let status = campaign.status.as_ref();
        let labels: Option<&BTreeMap<String, String>> = Some(campaign.labels());
        // Experiments are mapped BEFORE the struct literal so `objective_metric`
        // can be borrowed here and moved into the DTO afterwards.
        let experiments: Vec<ExperimentDto> = exps
            .iter()
            .map(|e| experiment_dto(e, objective_metric.as_deref()))
            .collect();
        dtos.push(CampaignDto {
            name: name.clone(),
            robot: labels.and_then(|l| l.get(ROBOT_LABEL).cloned()),
            stage: labels.and_then(|l| l.get(STAGE_LABEL).cloned()),
            template_ref: campaign.spec.template_ref.clone(),
            objective_metric,
            objective_goal,
            phase: status.and_then(|s| s.phase.clone()),
            best_experiment: status.and_then(|s| s.best_experiment.clone()),
            best_objective: status.and_then(|s| s.best_objective),
            incumbent_remeasured: status.and_then(|s| s.incumbent_remeasured),
            succeeded: status.map(|s| s.succeeded_experiments).unwrap_or(0),
            failed: status.map(|s| s.failed_experiments).unwrap_or(0),
            running: status.map(|s| s.running_experiments).unwrap_or(0),
            total: status.map(|s| s.total_experiments).unwrap_or(0),
            canary_state: status.and_then(|s| s.canary_state.clone()),
            mode,
            created_at: created_rfc3339(campaign),
            experiments,
        });
    }

    Json(CampaignsResponse { campaigns: dtos }).into_response()
}

async fn get_experiment(State(state): State<Arc<AppState>>, Path(name): Path<String>) -> Response {
    let experiments: Api<Experiment> = Api::namespaced(state.client.clone(), &state.namespace);
    let exp = match experiments.get(&name).await {
        Ok(e) => e,
        Err(e) if is_not_found(&e) => {
            // Absence is the common case (stale gym link), not a server fault.
            return not_found("experiment not found");
        }
        Err(e) => {
            error!(%e, experiment = %name, "experiment fetch failed");
            return internal("experiment fetch failed");
        }
    };

    // The objective column needs the campaign's template; a missing campaign
    // or template degrades to nulls rather than failing the whole view.
    let campaigns: Api<ResearchCampaign> = Api::namespaced(state.client.clone(), &state.namespace);
    let objective_metric = match campaigns.get(&exp.spec.campaign_ref).await {
        Ok(c) => {
            campaign_template_fields(&state.client, &state.namespace, &c.spec.template_ref)
                .await
                .0
        }
        Err(e) => {
            warn!(%e, campaign = %exp.spec.campaign_ref, "campaign fetch failed");
            None
        }
    };

    let conditions = exp
        .status
        .as_ref()
        .and_then(|s| s.conditions.as_ref())
        .map(|cs| cs.iter().map(condition_dto).collect())
        .unwrap_or_default();

    Json(ExperimentDetailDto {
        experiment: experiment_dto(&exp, objective_metric.as_deref()),
        parameters: exp.spec.parameters.clone(),
        conditions,
    })
    .into_response()
}

async fn get_figure(
    State(state): State<Arc<AppState>>,
    Path((name, file)): Path<(String, String)>,
) -> Response {
    // Traversal guard, layer 1: the requested {file} must be a single path
    // segment — no '/', no '..'. A public endpoint serving files off a shared
    // PVC must not become a directory-read primitive. (axum percent-decodes
    // path params, so an encoded %2F is caught here too.)
    if file.is_empty() || file.contains('/') || file.contains("..") {
        return not_found("no such figure");
    }

    let experiments: Api<Experiment> = Api::namespaced(state.client.clone(), &state.namespace);
    let exp = match experiments.get(&name).await {
        Ok(e) => e,
        Err(e) if is_not_found(&e) => return not_found("experiment not found"),
        Err(e) => {
            error!(%e, experiment = %name, "experiment fetch failed");
            return internal("experiment fetch failed");
        }
    };
    let Some(figures_uri) = exp
        .status
        .as_ref()
        .and_then(|s| s.artifacts.as_ref())
        .and_then(|a| a.figures_uri.as_deref())
    else {
        return not_found("no figures for experiment");
    };

    // figures_uri is written pod-side as /workspace/...; remap onto whatever
    // path this pod mounted the PVC at.
    let Some(rel) = figures_uri
        .strip_prefix(&format!("{WORKSPACE_PREFIX}/"))
        .or_else(|| figures_uri.strip_prefix(WORKSPACE_PREFIX))
    else {
        warn!(%figures_uri, "figures_uri outside workspace prefix");
        return not_found("no such figure");
    };
    let path = state.workspace_mount.join(rel).join(&file);

    // Traversal guard, layer 2: canonicalize both sides and require the file
    // to remain inside the mount root — catches symlinks planted in the
    // figures dir that point elsewhere on the PVC or the node.
    let Ok(canonical) = tokio::fs::canonicalize(&path).await else {
        return not_found("no such figure");
    };
    let root = match tokio::fs::canonicalize(&state.workspace_mount).await {
        Ok(r) => r,
        Err(e) => {
            error!(%e, mount = ?state.workspace_mount, "workspace mount not resolvable");
            return internal("figure storage unavailable");
        }
    };
    if !canonical.starts_with(&root) {
        return not_found("no such figure");
    }

    match tokio::fs::read(&canonical).await {
        Ok(bytes) => ([(header::CONTENT_TYPE, "image/png")], bytes).into_response(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => not_found("no such figure"),
        Err(e) => {
            error!(%e, figure = %canonical.display(), "figure read failed");
            internal("figure read failed")
        }
    }
}

async fn list_reports(State(state): State<Arc<AppState>>) -> Response {
    let reports: Api<ResearchReport> = Api::namespaced(state.client.clone(), &state.namespace);
    let list = match reports.list(&ListParams::default()).await {
        Ok(l) => l,
        Err(e) => {
            error!(%e, "failed to list reports");
            return internal("listing failed");
        }
    };

    let mut items = list.items;
    items.sort_by(|a, b| a.name_any().cmp(&b.name_any()));

    Json(ReportsResponse {
        reports: items
            .iter()
            .map(|r| ReportDto {
                name: r.name_any(),
                campaign_ref: r.spec.campaign_ref.clone(),
                title: r.spec.title.clone(),
                sections: r.spec.sections.clone(),
                seeded_hypotheses: r.spec.seeded_hypotheses.clone(),
                created_at: created_rfc3339(r),
            })
            .collect(),
    })
    .into_response()
}

/// objectiveMetric/objectiveGoal/mode live on the campaign's ExperimentTemplate,
/// not the campaign; one GET per campaign fetches all three. A deleted template
/// must degrade to nulls, not break the listing. `mode` comes from
/// `spec.defaults["mode"]` — the template's declared stance, which the gym
/// maps onto its pilot/gather/arena views.
async fn campaign_template_fields(
    client: &Client,
    namespace: &str,
    template_ref: &str,
) -> (Option<String>, Option<String>, Option<String>) {
    let templates: Api<ExperimentTemplate> = Api::namespaced(client.clone(), namespace);
    match templates.get(template_ref).await {
        Ok(t) => (
            Some(t.spec.objective.metric.clone()),
            enum_name(&t.spec.objective.goal),
            t.spec
                .defaults
                .get("mode")
                .and_then(|v| v.as_str())
                .map(String::from),
        ),
        Err(e) => {
            warn!(%e, template = %template_ref, "template fetch failed");
            (None, None, None)
        }
    }
}

fn experiment_dto(exp: &Experiment, objective_metric: Option<&str>) -> ExperimentDto {
    // Mode is what THIS experiment declares in spec.parameters — no fallback
    // to the campaign's template default. The BFF reports what each object
    // declared; blending defaults into observations is how a viewer starts
    // lying, and the frontend can layer defaults knowingly.
    let mode = exp
        .spec
        .parameters
        .get("mode")
        .and_then(|v| v.as_str())
        .map(String::from);

    let name = exp.name_any();
    let status = exp.status.as_ref();

    // Numeric entries only: status.metrics is a JSON map and the trainer may
    // park strings (links, notes) in it alongside measured values.
    let mut metrics = BTreeMap::new();
    if let Some(s) = status {
        for (k, v) in &s.metrics {
            if let Some(n) = v.as_f64() {
                metrics.insert(k.clone(), n);
            }
        }
    }

    // Advertise the policy download only when the trainer actually exported
    // one (`policies_exported >= 1`); a URL with nothing behind it breaks the
    // gym's model loader instead of degrading.
    let onnx_url = status
        .and_then(|s| s.metrics.get("policies_exported"))
        .and_then(|v| v.as_f64())
        .filter(|&n| n >= 1.0)
        .map(|_| {
            format!(
                "https://storage.googleapis.com/nixlab-spot-reruns/policies/{name}/walk_policy.onnx"
            )
        });

    let filmstrip_url = status
        .and_then(|s| s.artifacts.as_ref())
        .and_then(|a| a.figures_uri.as_deref())
        .map(|_| format!("/api/v1/experiments/{name}/figures/eval_filmstrip.png"));

    ExperimentDto {
        name,
        phase: status.and_then(|s| enum_name(&s.phase)),
        hypothesis: exp.spec.hypothesis.clone(),
        created_at: created_rfc3339(exp),
        objective: objective_metric.and_then(|m| metrics.get(m).copied()),
        metrics,
        mode,
        onnx_url,
        filmstrip_url,
        image: status
            .and_then(|s| s.environment.as_ref())
            .and_then(|e| e.image.as_deref())
            .map(short_image),
    }
}

fn condition_dto(c: &ExperimentCondition) -> ConditionDto {
    ConditionDto {
        condition_type: c.condition_type.clone(),
        status: c.status.clone(),
        reason: c.reason.clone(),
        message: c.message.clone(),
    }
}

/// `ghcr.io/org/spot-rapier-trainer:curriculum-v6@sha256:...` →
/// `spot-rapier-trainer:curriculum-v6`: drop the registry path and the digest
/// so the gym renders a readable tag.
fn short_image(image: &str) -> String {
    let tag = image.rsplit_once('/').map_or(image, |(_, tag)| tag);
    tag.split('@').next().unwrap_or(tag).to_string()
}

fn created_rfc3339<K: ResourceExt>(obj: &K) -> Option<String> {
    obj.meta()
        .creation_timestamp
        .as_ref()
        .map(|t| t.0.to_rfc3339())
}

/// Serde camelCase name of a unit-enum variant — the same spelling the CRD
/// uses on the wire, so the DTO never invents a second one.
fn enum_name<T: serde::Serialize>(value: &T) -> Option<String> {
    serde_json::to_value(value)
        .ok()?
        .as_str()
        .map(str::to_string)
}

/// Oldest-first, ties by name — the same order `athena dossier` assembles in.
fn sort_experiments(exps: &mut [&Experiment]) {
    exps.sort_by(|a, b| {
        let at = a.meta().creation_timestamp.clone().map(|t| t.0);
        let bt = b.meta().creation_timestamp.clone().map(|t| t.0);
        at.cmp(&bt).then_with(|| a.name_any().cmp(&b.name_any()))
    });
}

fn not_found(msg: &'static str) -> Response {
    (StatusCode::NOT_FOUND, msg).into_response()
}

/// K8s API "not found" — absence, as opposed to an upstream fault.
fn is_not_found(e: &kube::Error) -> bool {
    matches!(e, kube::Error::Api(ae) if ae.code == 404)
}

fn internal(msg: &'static str) -> Response {
    (StatusCode::INTERNAL_SERVER_ERROR, msg).into_response()
}
