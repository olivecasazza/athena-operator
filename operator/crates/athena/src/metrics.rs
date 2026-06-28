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
    let gauge =
        GaugeVec::new(opts, &["namespace", "experiment", "campaign", "metric"]).unwrap();
    REGISTRY.register(Box::new(gauge.clone())).unwrap();
    gauge
});

pub fn init() {
    Lazy::force(&EXPERIMENTS_TOTAL);
    Lazy::force(&BENCHMARK_RUNS_TOTAL);
    Lazy::force(&EXPERIMENT_METRIC);
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
