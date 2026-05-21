use std::time::Duration;

use anyhow::Context;
use opentelemetry::{KeyValue, global, trace::{TraceContextExt, TracerProvider}};
use opentelemetry_otlp::{MetricExporter, SpanExporter, WithExportConfig};
use opentelemetry_sdk::{Resource, metrics::SdkMeterProvider, trace::SdkTracerProvider};
use opentelemetry_semantic_conventions::resource::{SERVICE_NAME, SERVICE_VERSION};
use prometheus::{HistogramOpts, HistogramVec, IntCounterVec, Opts};
use tracing::Span;
use tracing_opentelemetry::OpenTelemetrySpanExt;
use tracing_subscriber::{EnvFilter, Registry, layer::SubscriberExt, util::SubscriberInitExt};

use crate::metrics::REGISTRY;

pub const DEFAULT_SERVICE_NAME: &str = "athena-operator";

pub fn init_telemetry() -> anyhow::Result<Option<SdkTracerProvider>> {
    global::set_text_map_propagator(opentelemetry_sdk::propagation::TraceContextPropagator::new());

    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| "info,athena=debug".into());
    let fmt_layer = tracing_subscriber::fmt::layer().json();

    if let Some(endpoint) = otlp_endpoint() {
        let resource = telemetry_resource(DEFAULT_SERVICE_NAME);
        let span_exporter = SpanExporter::builder()
            .with_tonic()
            .with_endpoint(endpoint.clone())
            .build()
            .context("failed to build OTLP trace exporter")?;
        let tracer_provider = SdkTracerProvider::builder()
            .with_batch_exporter(span_exporter)
            .with_resource(resource.clone())
            .build();
        let tracer = tracer_provider.tracer(DEFAULT_SERVICE_NAME);

        let metric_exporter = MetricExporter::builder()
            .with_tonic()
            .with_endpoint(endpoint)
            .build()
            .context("failed to build OTLP metric exporter")?;
        let meter_provider = SdkMeterProvider::builder()
            .with_periodic_exporter(metric_exporter)
            .with_resource(resource)
            .build();
        global::set_meter_provider(meter_provider);

        Registry::default()
            .with(filter)
            .with(fmt_layer)
            .with(tracing_opentelemetry::layer().with_tracer(tracer))
            .try_init()
            .context("failed to initialize tracing subscriber")?;
        Ok(Some(tracer_provider))
    } else {
        Registry::default()
            .with(filter)
            .with(fmt_layer)
            .try_init()
            .context("failed to initialize tracing subscriber")?;
        Ok(None)
    }
}

fn otlp_endpoint() -> Option<String> {
    std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT")
        .ok()
        .filter(|v| !v.trim().is_empty())
}

fn telemetry_resource(default_service_name: &str) -> Resource {
    let service_name = std::env::var("OTEL_SERVICE_NAME")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| default_service_name.to_string());
    Resource::builder()
        .with_attributes([
            KeyValue::new(SERVICE_NAME, service_name),
            KeyValue::new(SERVICE_VERSION, env!("CARGO_PKG_VERSION")),
        ])
        .build()
}

pub static RECONCILE_TOTAL: once_cell::sync::Lazy<IntCounterVec> = once_cell::sync::Lazy::new(|| {
    let counter = IntCounterVec::new(
        Opts::new(
            "athena_operator_reconcile_total",
            "Total Experiment reconcile attempts by namespace, campaign, phase, and result",
        ),
        &["namespace", "campaign", "phase", "result"],
    )
    .unwrap();
    REGISTRY.register(Box::new(counter.clone())).unwrap();
    counter
});

pub static RECONCILE_DURATION_SECONDS: once_cell::sync::Lazy<HistogramVec> = once_cell::sync::Lazy::new(|| {
    let histogram = HistogramVec::new(
        HistogramOpts::new(
            "athena_operator_reconcile_duration_seconds",
            "Experiment reconcile duration in seconds",
        )
        .buckets(vec![0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0]),
        &["namespace", "campaign", "phase", "result"],
    )
    .unwrap();
    REGISTRY.register(Box::new(histogram.clone())).unwrap();
    histogram
});

pub fn init_metrics() {
    once_cell::sync::Lazy::force(&RECONCILE_TOTAL);
    once_cell::sync::Lazy::force(&RECONCILE_DURATION_SECONDS);
}

pub fn record_reconcile(namespace: &str, campaign: &str, phase: &str, result: &str, duration: Duration) {
    let labels = [namespace, campaign, phase, result];
    RECONCILE_TOTAL.with_label_values(&labels).inc();
    RECONCILE_DURATION_SECONDS
        .with_label_values(&labels)
        .observe(duration.as_secs_f64());
}

pub fn current_trace_ids() -> (String, String) {
    let context = Span::current().context();
    let span_context = context.span().span_context().clone();
    if span_context.is_valid() {
        (
            span_context.trace_id().to_string(),
            span_context.span_id().to_string(),
        )
    } else {
        (String::new(), String::new())
    }
}
