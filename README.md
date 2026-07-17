# Athena Operator

Athena is a Kubernetes-native autonomous ML/RL research platform. Experiments,
benchmarks, and research campaigns are custom resources; a Rust kube-rs operator
reconciles them into Jobs, parses declared metric artifacts, and owns observed
status. Kubernetes is the product API and source of truth.

Start here:

- docs/openspec.md — Athena architecture, CRD/API shape, benchmark behavior, rollout
- docs/tier3-lean-prover-spec.md — formal theorem proving as auto-RL (design spec)
- operator/ — Rust kube-rs operator and CRD API crates
- operator/crates/athena-console/ — Rust/Iced console for watching Athena resources
- examples/ — example Experiment, BenchmarkRun, and ResearchCampaign resources
- experiments/ — stateless train/eval workload code run by Kubernetes Jobs
- nix/athena/, modules/k8s/, charts/athena/ — Nix-rendered Helm/manifests (GitOps via Flux)

Common commands:

```bash
nix build .#k8s-manifests          # regenerate modules/k8s/manifests.yaml
nix build .#helm-chart             # build the Helm chart
nix fmt                            # format Nix
nix run .#athena-console           # launch the console (uses local kubeconfig)
uv sync                            # sync workload-only Python deps
uv run experiments/train.py        # local workload smoke run
uv run pytest tests/               # smoke/manifest/observability checks
```
