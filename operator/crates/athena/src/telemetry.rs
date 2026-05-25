use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use opentelemetry::{
    KeyValue, global,
    trace::{TraceContextExt, TracerProvider},
};
use opentelemetry_otlp::{MetricExporter, SpanExporter, WithExportConfig};
use opentelemetry_sdk::{Resource, metrics::SdkMeterProvider, trace::SdkTracerProvider};
use opentelemetry_semantic_conventions::resource::{SERVICE_NAME, SERVICE_VERSION};
use prometheus::{HistogramOpts, HistogramVec, IntCounterVec, Opts};
use tracing::{Level, Span};
use tracing_opentelemetry::OpenTelemetrySpanExt;
use tracing_subscriber::{EnvFilter, Registry, layer::SubscriberExt, util::SubscriberInitExt};

use crate::metrics::REGISTRY;

pub const DEFAULT_SERVICE_NAME: &str = "athena-operator";

#[derive(Debug, Clone)]
pub struct TelemetryConfig {
    pub service_name: String,
    pub otlp_endpoint: Option<String>,
    pub observability_level: u8,
    pub protobuf_enabled: bool,
}

impl TelemetryConfig {
    pub fn from_env() -> Self {
        let service_name = std::env::var("OTEL_SERVICE_NAME")
            .ok()
            .filter(|v| !v.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_SERVICE_NAME.to_string());
        let otlp_endpoint = std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT")
            .ok()
            .filter(|v| !v.trim().is_empty());
        let observability_level = std::env::var("ATHENA_OBSERVABILITY_LEVEL")
            .or_else(|_| std::env::var("ATHENA_TRACE_LEVEL"))
            .ok()
            .and_then(|v| v.parse::<u8>().ok())
            .map(|v| v.clamp(1, 7))
            .unwrap_or(3);
        let protobuf_enabled = std::env::var("OTEL_EXPORTER_OTLP_PROTOCOL")
            .map(|v| matches!(v.as_str(), "grpc" | "protobuf" | "http/protobuf"))
            .unwrap_or(true);
        Self {
            service_name,
            otlp_endpoint,
            observability_level,
            protobuf_enabled,
        }
    }

    pub fn tracing_filter(&self) -> EnvFilter {
        if let Ok(filter) = EnvFilter::try_from_default_env() {
            return filter;
        }
        EnvFilter::new(match self.observability_level {
            1 => "warn,athena=info,kube=warn,kube_runtime=warn",
            2 => "info,athena=info,kube=warn,kube_runtime=info",
            3 => "info,athena=debug,kube=info,kube_runtime=info",
            4 => "debug,athena=debug,kube=info,kube_client=info,kube_runtime=debug",
            5 => "debug,athena=trace,kube=debug,kube_client=debug,kube_runtime=debug",
            6 => "trace,athena=trace,kube=debug,kube_client=debug,kube_runtime=trace",
            _ => "trace,athena=trace,kube=trace,kube_client=trace,kube_runtime=trace",
        })
    }

    pub fn action_level(&self) -> Level {
        match self.observability_level {
            1 => Level::WARN,
            2 | 3 => Level::INFO,
            4 | 5 => Level::DEBUG,
            _ => Level::TRACE,
        }
    }
}

pub type SharedTelemetryConfig = Arc<TelemetryConfig>;

pub fn init_telemetry() -> anyhow::Result<(Option<SdkTracerProvider>, SharedTelemetryConfig)> {
    let config = Arc::new(TelemetryConfig::from_env());
    global::set_text_map_propagator(opentelemetry_sdk::propagation::TraceContextPropagator::new());

    let filter = config.tracing_filter();
    let fmt_layer = tracing_subscriber::fmt::layer().json();

    if let Some(endpoint) = &config.otlp_endpoint {
        let resource = telemetry_resource(
            &config.service_name,
            config.observability_level,
            config.protobuf_enabled,
        );
        let span_exporter = SpanExporter::builder()
            .with_tonic()
            .with_endpoint(endpoint.clone())
            .build()
            .context("failed to build protobuf OTLP trace exporter")?;
        let tracer_provider = SdkTracerProvider::builder()
            .with_batch_exporter(span_exporter)
            .with_resource(resource.clone())
            .build();
        let tracer = tracer_provider.tracer(config.service_name.clone());

        let metric_exporter = MetricExporter::builder()
            .with_tonic()
            .with_endpoint(endpoint.clone())
            .build()
            .context("failed to build protobuf OTLP metric exporter")?;
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
        Ok((Some(tracer_provider), config))
    } else {
        Registry::default()
            .with(filter)
            .with(fmt_layer)
            .try_init()
            .context("failed to initialize tracing subscriber")?;
        Ok((None, config))
    }
}

fn telemetry_resource(
    service_name: &str,
    observability_level: u8,
    protobuf_enabled: bool,
) -> Resource {
    Resource::builder()
        .with_attributes([
            KeyValue::new(SERVICE_NAME, service_name.to_string()),
            KeyValue::new(SERVICE_VERSION, env!("CARGO_PKG_VERSION")),
            KeyValue::new("athena.observability.level", observability_level as i64),
            KeyValue::new(
                "athena.telemetry.protocol",
                if protobuf_enabled {
                    "otlp/protobuf"
                } else {
                    "otlp"
                },
            ),
        ])
        .build()
}

pub static RECONCILE_TOTAL: once_cell::sync::Lazy<IntCounterVec> =
    once_cell::sync::Lazy::new(|| {
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

pub static ACTION_TOTAL: once_cell::sync::Lazy<IntCounterVec> = once_cell::sync::Lazy::new(|| {
    let counter = IntCounterVec::new(
        Opts::new(
            "athena_operator_agent_action_total",
            "Total Athena operator agent actions by namespace, experiment, action, and result",
        ),
        &["namespace", "experiment", "action", "result"],
    )
    .unwrap();
    REGISTRY.register(Box::new(counter.clone())).unwrap();
    counter
});

pub static ACTION_DURATION_SECONDS: once_cell::sync::Lazy<HistogramVec> =
    once_cell::sync::Lazy::new(|| {
        let histogram = HistogramVec::new(
            HistogramOpts::new(
                "athena_operator_agent_action_duration_seconds",
                "Athena operator agent action duration in seconds",
            )
            .buckets(vec![
                0.001, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0,
            ]),
            &["namespace", "experiment", "action", "result"],
        )
        .unwrap();
        REGISTRY.register(Box::new(histogram.clone())).unwrap();
        histogram
    });

pub static RECONCILE_DURATION_SECONDS: once_cell::sync::Lazy<HistogramVec> =
    once_cell::sync::Lazy::new(|| {
        let histogram = HistogramVec::new(
            HistogramOpts::new(
                "athena_operator_reconcile_duration_seconds",
                "Experiment reconcile duration in seconds",
            )
            .buckets(vec![
                0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0,
            ]),
            &["namespace", "campaign", "phase", "result"],
        )
        .unwrap();
        REGISTRY.register(Box::new(histogram.clone())).unwrap();
        histogram
    });

pub fn init_metrics() {
    once_cell::sync::Lazy::force(&RECONCILE_TOTAL);
    once_cell::sync::Lazy::force(&RECONCILE_DURATION_SECONDS);
    once_cell::sync::Lazy::force(&ACTION_TOTAL);
    once_cell::sync::Lazy::force(&ACTION_DURATION_SECONDS);
}

pub fn record_reconcile(
    namespace: &str,
    campaign: &str,
    phase: &str,
    result: &str,
    duration: Duration,
) {
    let labels = [namespace, campaign, phase, result];
    RECONCILE_TOTAL.with_label_values(&labels).inc();
    RECONCILE_DURATION_SECONDS
        .with_label_values(&labels)
        .observe(duration.as_secs_f64());
}

pub fn record_action(
    namespace: &str,
    experiment: &str,
    action: &str,
    result: &str,
    duration: Duration,
) {
    let labels = [namespace, experiment, action, result];
    ACTION_TOTAL.with_label_values(&labels).inc();
    ACTION_DURATION_SECONDS
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
