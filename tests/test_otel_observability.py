from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]


def read(path: str) -> str:
    return (ROOT / path).read_text()


def check_operator_has_otel_tracing_and_metrics_wiring():
    cargo = read("operator/Cargo.toml")
    assert "tracing-opentelemetry" in cargo
    assert "opentelemetry-otlp" in cargo
    assert "opentelemetry_sdk" in cargo or "opentelemetry-sdk" in cargo

    telemetry = read("operator/crates/athena/src/telemetry.rs")
    assert "init_telemetry" in telemetry
    assert "OTEL_EXPORTER_OTLP_ENDPOINT" in telemetry
    assert "OTEL_SERVICE_NAME" in telemetry
    assert "TraceContextPropagator" in telemetry
    assert "athena_operator_reconcile_total" in telemetry
    assert "athena_operator_reconcile_duration_seconds" in telemetry

    main = read("operator/crates/athena/src/main.rs")
    assert "mod telemetry;" in main
    assert "telemetry::init_telemetry" in main
    assert "provider.shutdown()" in main

    reconciler = read("operator/crates/athena/src/reconciler.rs")
    assert "#[tracing::instrument" in reconciler
    assert "trace_id" in reconciler
    assert "record_reconcile" in reconciler


def check_console_api_has_otel_http_trace_propagation_and_metrics():
    go_mod = read("athena-console/api/go.mod")
    assert "go.opentelemetry.io/otel" in go_mod
    assert "go.opentelemetry.io/contrib/instrumentation/net/http/otelhttp" in go_mod

    main_go = read("athena-console/api/main.go")
    telemetry_go = read("athena-console/api/telemetry.go")
    assert "initTelemetry" in main_go
    assert "instrumentHandlerFunc" in main_go
    assert "otelhttp.NewHandler" in telemetry_go
    assert "TraceContext{}" in telemetry_go
    assert "otel.Meter" in telemetry_go
    assert "http.server.request.duration" in telemetry_go
    assert "trace_id" in telemetry_go


def check_frontend_has_browser_otel_and_trace_header_injection():
    package_json = read("athena-console/web/package.json")
    assert "@opentelemetry/api" in package_json
    assert "@opentelemetry/sdk-trace-web" in package_json
    assert "@opentelemetry/exporter-trace-otlp-http" in package_json

    telemetry_ts = read("athena-console/web/src/telemetry.ts")
    assert "WebTracerProvider" in telemetry_ts
    assert "OTLPTraceExporter" in telemetry_ts
    assert "TraceContextPropagator" in telemetry_ts
    assert "VITE_OTEL_EXPORTER_OTLP_ENDPOINT" in telemetry_ts
    assert "injectTraceHeaders" in telemetry_ts

    app_vue = read("athena-console/web/src/App.vue")
    assert "startUiSpan" in app_vue
    assert "injectTraceHeaders" in app_vue
    assert "traceparent" in telemetry_ts


def check_canary_experiment_exports_otel_spans_and_trace_metrics():
    pyproject = read("pyproject.toml")
    assert "opentelemetry-exporter-otlp" in pyproject
    assert "opentelemetry-sdk" in pyproject

    canary = read("examples/canary/canary_train.py")
    assert "init_observability" in canary
    assert "OTEL_EXPORTER_OTLP_ENDPOINT" in canary
    assert "TraceContextTextMapPropagator" in canary
    assert "start_as_current_span" in canary
    assert '"trace_id"' in canary
    assert '"span_id"' in canary


if __name__ == "__main__":
    checks = [
        check_operator_has_otel_tracing_and_metrics_wiring,
        check_console_api_has_otel_http_trace_propagation_and_metrics,
        check_frontend_has_browser_otel_and_trace_header_injection,
        check_canary_experiment_exports_otel_spans_and_trace_metrics,
    ]
    for check in checks:
        check()
        print(f"ok - {check.__name__}")
