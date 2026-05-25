# n-autoresearch

Start here:

- docs/README.md — project overview and usage guide
- docs/program.md — agent instructions for autoresearch loops
- openspec.md — Athena benchmark/operator build spec
- experiments/ — train/eval scripts used by experiment workers
- operator/ — Rust kube-rs Athena operator
- operator/crates/athena-console/ — Rust/Iced local Athena console
- workers/ — legacy iii orchestrator and GPU workers (kept for impl reference)
- examples/ — example manifests and canary resources
- config/ — local/runtime config files

Common commands:

```bash
uv run experiments/prepare.py
uv run experiments/train.py
iii --config config/iii-config.yaml
uv run python workers/orchestrator/orchestrator.py
```
