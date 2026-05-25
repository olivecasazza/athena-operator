import importlib.util
import json
import os
import sys
import tempfile
import types
import unittest
from pathlib import Path
from unittest.mock import patch


REPO_ROOT = Path(__file__).resolve().parents[1]
CANARY_PATH = REPO_ROOT / "examples" / "canary" / "canary_train.py"


class _DummyModule(types.ModuleType):
    def __getattr__(self, name):
        value = _DummyModule(f"{self.__name__}.{name}")
        setattr(self, name, value)
        return value

    def __call__(self, *args, **kwargs):
        return _DummyModule(f"{self.__name__}.call")


class _DummyContext:
    def __enter__(self):
        return _DummySpan()

    def __exit__(self, exc_type, exc, tb):
        return False


class _DummyTracer:
    def start_as_current_span(self, *args, **kwargs):
        return _DummyContext()


class _DummySpan:
    def set_attribute(self, *args, **kwargs):
        return None

    def get_span_context(self):
        return types.SimpleNamespace(is_valid=False)


def _module(name: str) -> _DummyModule:
    module = _DummyModule(name)
    module.__path__ = []
    return module


def _install_canary_import_stubs():
    modules = {}
    for name in [
        "opentelemetry",
        "opentelemetry.baggage",
        "opentelemetry.context",
        "opentelemetry.propagate",
        "opentelemetry.exporter",
        "opentelemetry.exporter.otlp",
        "opentelemetry.exporter.otlp.proto",
        "opentelemetry.exporter.otlp.proto.grpc",
        "opentelemetry.exporter.otlp.proto.grpc.trace_exporter",
        "opentelemetry.sdk",
        "opentelemetry.sdk.resources",
        "opentelemetry.sdk.trace",
        "opentelemetry.sdk.trace.export",
        "opentelemetry.trace",
        "opentelemetry.trace.propagation",
        "opentelemetry.trace.propagation.tracecontext",
    ]:
        modules[name] = _module(name)
    modules["opentelemetry.sdk.resources"].SERVICE_NAME = "service.name"
    modules["opentelemetry.sdk.resources"].SERVICE_VERSION = "service.version"
    modules["opentelemetry.trace"].SpanKind = types.SimpleNamespace(INTERNAL="internal")
    modules["opentelemetry.trace"].Tracer = _DummyTracer
    modules["opentelemetry.trace"].get_current_span = lambda: _DummySpan()
    modules["torch"] = _module("torch")
    modules["torch.nn"] = _module("torch.nn")
    modules["torch.nn.functional"] = _module("torch.nn.functional")
    setattr(modules["torch"], "nn", modules["torch.nn"])
    setattr(modules["torch.nn"], "Module", object)
    return patch.dict(sys.modules, modules)


def load_canary_module():
    with _install_canary_import_stubs():
        spec = importlib.util.spec_from_file_location("canary_train", CANARY_PATH)
        assert spec is not None
        module = importlib.util.module_from_spec(spec)
        assert spec.loader is not None
        spec.loader.exec_module(module)
        return module


class CanaryIterationMetricTests(unittest.TestCase):
    def test_config_loads_iteration_identity_from_crd_parameters(self):
        canary = load_canary_module()
        crd_spec = {
            "parameters": {
                "experimentTag": "attention-sweep",
                "experimentIteration": 7,
                "parentExperimentId": "exp-006",
                "baselineValBpb": 4.8,
            }
        }

        with patch.dict(os.environ, {"ATHENA_EXPERIMENT_SPEC": json.dumps(crd_spec)}, clear=True):
            config = canary.load_config()

        self.assertEqual(config.experiment_tag, "attention-sweep")
        self.assertEqual(config.experiment_iteration, 7)
        self.assertEqual(config.parent_experiment_id, "exp-006")
        self.assertEqual(config.baseline_val_bpb, 4.8)

    def test_legacy_env_overrides_iteration_identity(self):
        canary = load_canary_module()
        env = {
            "ATHENA_EXPERIMENT_SPEC": json.dumps(
                {
                    "parameters": {
                        "experimentTag": "spec-tag",
                        "experimentIteration": 2,
                        "baselineValBpb": 5.0,
                    }
                }
            ),
            "ATHENA_EXPERIMENT_TAG": "env-tag",
            "ATHENA_EXPERIMENT_ITERATION": "3",
            "ATHENA_PARENT_EXPERIMENT_ID": "env-parent",
            "ATHENA_BASELINE_VAL_BPB": "4.4",
        }

        with patch.dict(os.environ, env, clear=True):
            config = canary.load_config()

        self.assertEqual(config.experiment_tag, "env-tag")
        self.assertEqual(config.experiment_iteration, 3)
        self.assertEqual(config.parent_experiment_id, "env-parent")
        self.assertEqual(config.baseline_val_bpb, 4.4)

    def test_metrics_payload_includes_iteration_tags_and_improvement_series(self):
        canary = load_canary_module()
        with tempfile.TemporaryDirectory() as tmpdir:
            config = canary.CanaryConfig(
                metrics_path=str(Path(tmpdir) / "metrics.json"),
                experiment_tag="attention-sweep",
                experiment_iteration=4,
                parent_experiment_id="exp-003",
                baseline_val_bpb=5.0,
            )
            metrics = canary.build_metrics_payload(
                config=config,
                val_bpb_initial=5.0,
                val_bpb=4.25,
                training_seconds=12.345,
                optimizer_steps=8,
                peak_vram_mb=123.4,
                status="completed",
                num_params=4567,
                device="cuda",
                trace_metrics={"trace_id": "abc", "span_id": "def"},
            )

        self.assertEqual(metrics["experiment_tag"], "attention-sweep")
        self.assertEqual(metrics["experiment_iteration"], 4)
        self.assertEqual(metrics["parent_experiment_id"], "exp-003")
        self.assertEqual(metrics["baseline_val_bpb"], 5.0)
        self.assertEqual(metrics["val_bpb_initial"], 5.0)
        self.assertEqual(metrics["val_bpb"], 4.25)
        self.assertEqual(metrics["val_bpb_delta"], 0.75)
        self.assertEqual(metrics["improved"], True)
        self.assertEqual(metrics["metric_series"]["tag"], "attention-sweep")
        self.assertEqual(metrics["metric_series"]["iteration"], 4)
        self.assertEqual(metrics["metric_series"]["points"][-1]["name"], "val_bpb_delta")
        self.assertEqual(metrics["metric_series"]["points"][-1]["value"], 0.75)
        self.assertIn("attention-sweep iteration 4", metrics["summary"])
        self.assertIn("reproducibility_hash", metrics)

    def test_synthetic_metric_projection_improves_monotonically_across_iterations(self):
        canary = load_canary_module()
        projected = [
            canary.project_iteration_metrics(
                canary.CanaryConfig(
                    experiment_tag="karpathy-canary",
                    experiment_iteration=iteration,
                    parent_experiment_id=f"canary-run-{iteration - 1:02d}",
                    baseline_val_bpb=8.5,
                )
            )
            for iteration in [1, 2, 3]
        ]

        self.assertGreater(projected[0]["val_bpb"], projected[1]["val_bpb"])
        self.assertGreater(projected[1]["val_bpb"], projected[2]["val_bpb"])
        self.assertLess(projected[0]["val_bpb_delta"], projected[1]["val_bpb_delta"])
        self.assertLess(projected[1]["val_bpb_delta"], projected[2]["val_bpb_delta"])
        self.assertEqual([m["experiment_iteration"] for m in projected], [1, 2, 3])

    def test_canary_writes_metrics_to_termination_log_for_operator_status(self):
        canary = CANARY_PATH.read_text()

        self.assertIn("/dev/termination-log", canary)
        self.assertIn("json.dump(metrics, termination)", canary)


if __name__ == "__main__":
    unittest.main()
