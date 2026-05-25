from pathlib import Path
import re


ROOT = Path(__file__).resolve().parents[1]


def read(path: str) -> str:
    return (ROOT / path).read_text()


def test_canary_manifest_owns_workspace_pvc_and_runtime_autocreate_contract():
    canary = read("examples/canary/canary.yaml")

    assert "kind: PersistentVolumeClaim" in canary
    assert "name: athena-workspace" in canary
    assert "storageClassName: longhorn" in canary
    assert "createWorkspaceClaim: true" in canary
    assert "workspaceSize: 10Gi" in canary
    assert "workspaceStorageClassName: longhorn" in canary
    assert "opentelemetry-api" in canary
    assert "opentelemetry-sdk" in canary
    assert "opentelemetry-exporter-otlp" in canary
    assert "experimentTag:" in canary
    assert "experimentIteration:" in canary
    assert "baselineValBpb:" in canary


def test_operator_passes_experiment_metrics_path_under_workspace():
    reconciler = read("operator/crates/athena/src/reconciler.rs")

    assert "fn resolve_metrics_path" in reconciler
    assert "ATHENA_WORKSPACE_PATH" in reconciler
    assert 'format!("{workspace_path}/{metrics_path}")' in reconciler
    assert "value: Some(workspace_path.to_string())" in reconciler


def test_operator_retries_workspace_claim_creation_for_existing_pending_jobs():
    reconciler = read("operator/crates/athena/src/reconciler.rs")

    assert "ensure_workspace_claim(ctx.clone(), ns, &profile).await?;" in reconciler
    assert re.search(
        r"if matches!\(phase, ExperimentPhase::Pending \| ExperimentPhase::Preparing\).*ensure_experiment_job",
        reconciler,
        re.S,
    )
    assert "WorkspaceClaimReady" in reconciler
    assert "workspace_ready_condition" in reconciler


def test_operator_lifts_canary_iteration_metrics_into_experiment_status():
    api = read("operator/crates/athena-api/src/experiment.rs")
    reconciler = read("operator/crates/athena/src/reconciler.rs")

    assert "ExperimentMetricSeries" in api
    assert "pub series: Vec<ExperimentMetricSeries>" in api
    assert "termination_metrics_json" in reconciler
    assert "merge_terminal_metrics" in reconciler
    assert "val_bpb_delta" in reconciler
    assert "experiment_tag" in reconciler


def test_benchmark_promotion_preserves_metric_series_for_experiment_status():
    api = read("operator/crates/athena-api/src/benchmark_run.rs")
    reconciler = read("operator/crates/athena/src/benchmark_reconciler.rs")

    assert "pub metric_series: Vec<ExperimentMetricSeries>" in api
    assert "flat_map(|result| result.metric_series.clone())" in reconciler
    assert "series: status.metric_series.clone()" in reconciler


def test_gitops_manifests_include_actionable_smoke_canaries():
    deployment = read("nix/athena/deployment.nix")

    assert "improvement-canary.yaml" in deployment
    assert "grpo-smoke-template.yaml" in deployment


def test_grpo_smoke_runtime_can_create_workspace_claim():
    smoke = read("examples/grpo-smoke-template.yaml")

    assert "createWorkspaceClaim: true" in smoke
    assert "workspaceSize: 10Gi" in smoke
    assert "workspaceStorageClassName: longhorn" in smoke


def test_benchmark_smoke_runtime_can_create_workspace_claim():
    smoke = read("examples/benchmarks/improvement-canary.yaml")

    assert "createWorkspaceClaim: true" in smoke
    assert "workspaceSize: 1Gi" in smoke
    assert "workspaceStorageClassName: longhorn" in smoke
