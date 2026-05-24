# n-autoresearch

`n-autoresearch` is Athena: a Kubernetes-native autonomous ML/RL research platform. Kubernetes custom resources are the product API; controllers own execution, observed status, metrics, events, and lifecycle.

Start here:

- `AGENTS.md` — repository rules for Kubernetes-native Athena work
- `docs/openspec.md` — Athena benchmark/operator build spec
- `operator/` — Rust kube-rs operator and CRD API crates
- `athena-console/` — Go BFF + Vue console for Athena resources
- `nix/athena/` — Nix-rendered Helm/Kubernetes deployment definitions
- `modules/k8s/` — generated GitOps Kubernetes manifests
- `examples/` — example Athena custom resources and canaries
- `experiments/` — stateless train/eval workload code invoked by Kubernetes Jobs

## Architecture

Athena models research as Kubernetes resources under `research.nixlab.io/v1alpha1`:

- `Experiment` owns one concrete trial's desired state and controller-observed status.
- `BenchmarkSuite` defines benchmark tasks, metrics, budgets, gates, and integrity policy.
- `BenchmarkRun` owns one suite execution and the Jobs/artifacts used to evaluate it.
- `MetricSource` defines how controllers parse declared metric artifacts.
- `ResearchCampaign` owns autonomous multi-run policy, novelty constraints, promotion gates, and aggregate outcomes.
- `RuntimeProfile` owns execution policy, workspace mounts, scheduling, resources, and sandbox settings.

Workloads may run Python or other runner code, but they must be stateless implementation details behind CRD-owned desired state and controller-owned observed state.

## Common Commands

```bash
nix build .#k8s-manifests
nix build .#helm-chart
nix fmt
```

Workload-only local smoke commands, useful only when changing `experiments/` code:

```bash
uv sync
uv run experiments/train.py
```

Do not use local Python scripts, local databases, log scraping, or legacy workers as Athena orchestration, status, benchmark, campaign, or deployment paths.
