use std::net::SocketAddr;

use axum::{Router, response::IntoResponse, routing::get};
use once_cell::sync::Lazy;
use prometheus::{Encoder, GaugeVec, Opts, Registry, TextEncoder};
use tracing::{error, info};

pub static REGISTRY: Lazy<Registry> = Lazy::new(Registry::new);

pub static EXPERIMENTS_TOTAL: Lazy<GaugeVec> = Lazy::new(|| {
    let opts =
        Opts::new("experiments_total", "Count of Athena experiments by phase").namespace("athena");
    let gauge = GaugeVec::new(opts, &["namespace", "campaign", "phase"]).unwrap();
    REGISTRY.register(Box::new(gauge.clone())).unwrap();
    gauge
});

pub static BENCHMARK_RUNS_TOTAL: Lazy<GaugeVec> = Lazy::new(|| {
    let opts = Opts::new(
        "benchmark_runs_total",
        "Count of Athena benchmark runs by suite and phase",
    )
    .namespace("athena");
    let gauge = GaugeVec::new(opts, &["namespace", "suite", "phase"]).unwrap();
    REGISTRY.register(Box::new(gauge.clone())).unwrap();
    gauge
});

/// Lifetime campaigns completed per ResearchDrive, by drive phase. The drive
/// is the perpetual outer loop; this is its progress signal.
pub static DRIVE_CAMPAIGNS_TOTAL: Lazy<GaugeVec> = Lazy::new(|| {
    let opts = Opts::new(
        "drive_campaigns_completed",
        "Campaigns completed per ResearchDrive by phase",
    )
    .namespace("athena");
    let gauge = GaugeVec::new(opts, &["namespace", "domain", "phase"]).unwrap();
    REGISTRY.register(Box::new(gauge.clone())).unwrap();
    gauge
});

/// Per-experiment reported metric values, re-published from each Experiment's
/// `status.metrics` (which the reconciler ingests from the pod termination
/// message). This is DURABLE regardless of pod lifetime — short-lived experiment
/// pods are scraped unreliably on :9108, so the dashboard reads this instead.
pub static EXPERIMENT_METRIC: Lazy<GaugeVec> = Lazy::new(|| {
    let opts = Opts::new(
        "experiment_metric",
        "Per-experiment reported metric values (re-exported from status.metrics)",
    )
    .namespace("athena");
    let gauge = GaugeVec::new(opts, &["namespace", "experiment", "campaign", "metric"]).unwrap();
    REGISTRY.register(Box::new(gauge.clone())).unwrap();
    gauge
});

/// Curriculum progress per ResearchDrive: 1 for the stage a drive is currently
/// in, 0 for every other declared stage. A gauge-per-stage rather than a single
/// numeric index so a dashboard can show which stage is live without hardcoding
/// an ordering, and so promotion shows up as one series falling while the next
/// rises.
///
/// `stage` is bounded by spec.curriculum.stages and low-cardinality by design
/// (stance/locomotion/forage/arena); never label by experiment or morphology
/// here, which would multiply series per run.
pub static DRIVE_CURRICULUM_STAGE: Lazy<GaugeVec> = Lazy::new(|| {
    let opts = Opts::new(
        "drive_curriculum_stage",
        "1 for the ResearchDrive's current curriculum stage, 0 for the others",
    )
    .namespace("athena");
    let gauge = GaugeVec::new(opts, &["namespace", "domain", "stage"]).unwrap();
    REGISTRY.register(Box::new(gauge.clone())).unwrap();
    gauge
});

/// Succeeded experiments observed in each curriculum stage, straight from
/// status.curriculum.stageHistory. Makes "is the promotion gate's
/// minExperiments satisfied yet" answerable from the dashboard.
pub static DRIVE_CURRICULUM_STAGE_EXPERIMENTS: Lazy<GaugeVec> = Lazy::new(|| {
    let opts = Opts::new(
        "drive_curriculum_stage_experiments",
        "Succeeded experiments per curriculum stage",
    )
    .namespace("athena");
    let gauge = GaugeVec::new(opts, &["namespace", "domain", "stage"]).unwrap();
    REGISTRY.register(Box::new(gauge.clone())).unwrap();
    gauge
});

pub fn init() {
    Lazy::force(&DRIVE_CURRICULUM_STAGE);
    Lazy::force(&DRIVE_CURRICULUM_STAGE_EXPERIMENTS);
    Lazy::force(&EXPERIMENTS_TOTAL);
    Lazy::force(&BENCHMARK_RUNS_TOTAL);
    Lazy::force(&EXPERIMENT_METRIC);
    Lazy::force(&DRIVE_CAMPAIGNS_TOTAL);
}

async fn metrics_handler() -> impl IntoResponse {
    let encoder = TextEncoder::new();
    let metric_families = REGISTRY.gather();
    let mut buffer = Vec::new();
    if let Err(e) = encoder.encode(&metric_families, &mut buffer) {
        error!(%e, "failed to encode metrics");
        return (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            "metric encoding error".to_string(),
        );
    }
    (
        axum::http::StatusCode::OK,
        String::from_utf8(buffer).unwrap_or_default(),
    )
}

async fn health_handler() -> impl IntoResponse {
    (axum::http::StatusCode::OK, "ok")
}

pub async fn serve(port: u16) {
    let app = Router::new()
        .route("/metrics", get(metrics_handler))
        .route("/healthz", get(health_handler));

    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    info!(%addr, "starting metrics server");

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .expect("failed to bind metrics port");

    axum::serve(listener, app)
        .await
        .expect("metrics server error");
}
