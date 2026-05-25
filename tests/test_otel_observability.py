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


def check_console_is_rust_iced_kube_workbench():
    cargo = read("operator/Cargo.toml")
    assert "crates/athena-console" in cargo
    assert "iced" in cargo

    console = read("operator/crates/athena-console/src/main.rs")
    assert "iced::application" in console
    assert "Client::try_default" in console
    assert "Api::<Experiment>::all" in console
    assert "Api::<ExperimentTemplate>::all" in console
    assert "PhaseFilter" in console


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
        check_console_is_rust_iced_kube_workbench,
        check_canary_experiment_exports_otel_spans_and_trace_metrics,
    ]
    for check in checks:
        check()
        print(f"ok - {check.__name__}")
