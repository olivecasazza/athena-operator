import importlib.util
import json
import os
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch


REPO_ROOT = Path(__file__).resolve().parents[1]
CANARY_PATH = REPO_ROOT / "examples" / "canary" / "canary_train.py"


def load_canary_module():
    spec = importlib.util.spec_from_file_location("canary_train", CANARY_PATH)
    assert spec is not None
    module = importlib.util.module_from_spec(spec)
    assert spec.loader is not None
    spec.loader.exec_module(module)
    return module


class CanaryConfigTests(unittest.TestCase):
    def test_config_loads_from_transparent_crd_spec_env(self):
        canary = load_canary_module()
        crd_spec = {
            "parameters": {
                "depth": 3,
                "deviceBatchSize": 2,
                "totalBatchSize": 256,
                "timeBudget": 7,
                "seqLen": 48,
                "vocabSize": 128,
            },
            "metrics": {"parser": {"path": "/workspace/runs/canary/metrics.json"}},
        }

        with patch.dict(os.environ, {"ATHENA_EXPERIMENT_SPEC": json.dumps(crd_spec)}, clear=True):
            config = canary.load_config()

        self.assertEqual(config.depth, 3)
        self.assertEqual(config.device_batch_size, 2)
        self.assertEqual(config.total_batch_size, 256)
        self.assertEqual(config.time_budget, 7)
        self.assertEqual(config.seq_len, 48)
        self.assertEqual(config.vocab_size, 128)
        self.assertEqual(config.metrics_path, "/workspace/runs/canary/metrics.json")

    def test_legacy_env_overrides_crd_spec_for_backward_compatibility(self):
        canary = load_canary_module()
        env = {
            "ATHENA_EXPERIMENT_SPEC": json.dumps(
                {
                    "parameters": {
                        "depth": 3,
                        "deviceBatchSize": 2,
                        "totalBatchSize": 256,
                        "timeBudget": 7,
                        "seqLen": 48,
                        "vocabSize": 128,
                    },
                    "metrics": {"parser": {"path": "/workspace/runs/canary/metrics.json"}},
                }
            ),
            "ATHENA_DEPTH": "1",
            "ATHENA_DEVICE_BATCH_SIZE": "4",
            "ATHENA_METRICS_PATH": "override.json",
        }

        with patch.dict(os.environ, env, clear=True):
            config = canary.load_config()

        self.assertEqual(config.depth, 1)
        self.assertEqual(config.device_batch_size, 4)
        self.assertEqual(config.total_batch_size, 256)
        self.assertEqual(config.time_budget, 7)
        self.assertEqual(config.seq_len, 48)
        self.assertEqual(config.vocab_size, 128)
        self.assertEqual(config.metrics_path, "override.json")

    def test_config_loads_crd_spec_from_file(self):
        canary = load_canary_module()
        with tempfile.TemporaryDirectory() as tmpdir:
            spec_path = Path(tmpdir) / "experiment-spec.json"
            spec_path.write_text(
                json.dumps(
                    {
                        "parameters": {"depth": 4, "timeBudget": 9},
                        "metrics": {"parser": {"path": "file-metrics.json"}},
                    }
                )
            )
            with patch.dict(os.environ, {"ATHENA_EXPERIMENT_SPEC_PATH": str(spec_path)}, clear=True):
                config = canary.load_config()

        self.assertEqual(config.depth, 4)
        self.assertEqual(config.time_budget, 9)
        self.assertEqual(config.device_batch_size, 4)
        self.assertEqual(config.total_batch_size, 1024)
        self.assertEqual(config.seq_len, 64)
        self.assertEqual(config.vocab_size, 256)
        self.assertEqual(config.metrics_path, "file-metrics.json")

    def test_flat_crd_spec_parameters_are_supported(self):
        canary = load_canary_module()
        env = {
            "ATHENA_EXPERIMENT_SPEC": json.dumps(
                {
                    "depth": 5,
                    "deviceBatchSize": 3,
                    "totalBatchSize": 384,
                    "timeBudget": 6,
                }
            )
        }

        with patch.dict(os.environ, env, clear=True):
            config = canary.load_config()

        self.assertEqual(config.depth, 5)
        self.assertEqual(config.device_batch_size, 3)
        self.assertEqual(config.total_batch_size, 384)
        self.assertEqual(config.time_budget, 6)

    def test_invalid_crd_spec_json_exits_cleanly(self):
        canary = load_canary_module()

        with patch.dict(os.environ, {"ATHENA_EXPERIMENT_SPEC": "not-json"}, clear=True):
            with self.assertRaises(SystemExit) as cm:
                canary.load_config()

        self.assertEqual(cm.exception.code, 2)


if __name__ == "__main__":
    unittest.main()
