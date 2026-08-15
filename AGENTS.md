# AGENTS.md

Guidance for coding agents working in this repository.

## Prime Directive

This repository is Athena (`athena-operator`): a Kubernetes-native autonomous ML/RL research platform. Treat Kubernetes as the product API and source of truth. If a feature matters to research execution, benchmarking, observability, scheduling, state, policy, or operator control, model it as a Kubernetes-native component first.

Do not add or preserve durable product behavior in ad-hoc scripts, local files, SQLite, local KV stores, log scraping, bespoke daemons, legacy workers, or out-of-band REST state. Athena product state must be owned by Kubernetes objects, controller reconciliation, status fields, events, metrics, and GitOps-rendered artifacts. Non-Kubernetes code may exist only as stateless runner implementation behind CRD-owned desired state and controller-owned observed state.

## Kubernetes-Native Design Law

- Default to Kubernetes APIs. Start every design by asking which Kubernetes object owns the desired state, observed state, lifecycle, permissions, and relationships.
- Experiments are CRDs. Hypotheses, provenance, parentage, phase, seed, budget, objective, retained/reverted decision, crash state, artifacts, and final outcome belong in `Experiment` spec/status.
- Benchmarks are CRDs. Benchmark definitions, runs, metric ingestion, suites, gates, holdouts, reproducibility hashes, and comparisons belong in `BenchmarkSuite`, `BenchmarkRun`, and `MetricSource` resources.
- Campaigns are CRDs. Autonomous research loops, strategy, novelty constraints, promotion criteria, duplicate detection, and aggregate outcomes belong in `ResearchCampaign` spec/status.
- Execution is reconciled. Controllers create/watch Jobs, Pods, PVCs, Services, Events, Conditions, and ownerReferences. Avoid imperative orchestration loops that bypass reconciliation.
- Metrics are cluster data. BPB, reward, accuracy, tokens, VRAM, MFU, runtime, GPU-hours, queue latency, step count, parameter count, failures, and controller health must surface through CR status and Prometheus metrics.
- Dashboards are product surface. Grafana dashboards, ServiceMonitors, alerts, Loki/log links, and console views must land with the feature when this repo owns those surfaces, not as an afterthought.
- Status is authoritative. Humans and agents may write specs and decisions; controllers own observed status. Do not forge benchmark, experiment, campaign, or runtime status from clients.
- Relationships must be queryable. Use labels, annotations, owner references, object refs, conditions, events, and status summaries so `kubectl`, Prometheus, Grafana, and the console can explain the system.
- GitOps is the deployment path. Changes to deployed resources must flow through Nix-rendered manifests/Helm artifacts and Flux, not manual `kubectl apply`, patching, or live edits.
- Prefer composable primitives. A strong design lets CRDs, operator reconcilers, runner Jobs, metric parsers, dashboards, and the console evolve independently while sharing API contracts.

## No Legacy Support

- Do not preserve old orchestration APIs, worker flows, local experiment loops, or compatibility behavior.
- The legacy `workers/orchestrator/` and `workers/gpu/` paths have been removed. Do not reintroduce local worker/orchestrator flows; re-express useful behavior as CRD schema, controller reconciliation, Kubernetes Job behavior, status, metrics, events, dashboard panels, or console views.
- When legacy and Athena-native behavior conflict, remove, bypass, or quarantine the legacy path instead of bridging to it.
- Ignore legacy experiment-loop instructions in older docs unless the task is explicitly to remove or migrate them.
- `AGENTS.md` and `docs/openspec.md` override older docs whenever legacy local loops conflict with Athena Kubernetes-native design.

## Project Shape

- `docs/openspec.md` is the build spec for Athena architecture, CRD/API shape, benchmark behavior, observability, and rollout order.
- `README.md` provides overview and usage context.
- `operator/` contains the Rust kube-rs operator and CRD API crates.
- `operator/crates/athena-console/` contains the Rust/Iced console for watching and comparing Athena resources through local kubeconfig.
- `nix/athena/` and `modules/k8s/` define Nix-rendered Helm/Kubernetes deployment artifacts.
- `examples/` contains example Athena custom resources and canaries.
- `experiments/` contains stateless train/eval workload code invoked by Kubernetes-managed `Experiment` or `BenchmarkRun` Jobs. It must not own scheduling, durable state, status, promotion decisions, benchmark comparison, or orchestration.

## Implementation Rules

- Product behavior must be modeled through CRD schema, controller reconciliation, status conditions, Kubernetes Events, Prometheus metrics, dashboards, and console integration. Standalone scripts/services may only be stateless implementation details behind Kubernetes-owned specs and controller-owned status.
- Keep API changes additive for `research.nixlab.io/v1alpha1` unless the user explicitly asks for a versioning/migration plan.
- New CRDs must have structural OpenAPI schemas, bounded enums, `status.observedGeneration`, bounded `status.conditions[]`, and `status.controllerVersion`.
- Keep large item-level results in workspace artifacts; put normalized summaries, refs, hashes, and conditions in Kubernetes status.
- Use Kubernetes RBAC and `/status` subresources deliberately. Status writes must belong to the Athena operator service account.
- Keep changes minimal, focused, and consistent with nearby code.
- Prefer editing existing files over creating new files.
- Do not add dependencies unless the user explicitly asks.
- Do not commit, push, or create branches unless the user explicitly asks.
- Do not skip hooks with `--no-verify` or equivalent.
- Avoid editing generated files unless the generation workflow requires it.
- `uv.lock` is ignored by this repo; do not rely on it being committed.

## CRD API Rules

- Athena CRDs are namespaced by default under `research.nixlab.io/v1alpha1`.
- New CRDs must enable `/status` and use structural OpenAPI schemas.
- Existing v1alpha1 APIs must remain additive unless the user explicitly requests a versioning/conversion plan.
- Do not add required fields to existing CRDs without defaults.
- Conditions must be bounded, use stable `type` and `reason` values, and include `observedGeneration` semantics.
- Avoid unbounded `serde_json::Value`, arbitrary maps, or high-cardinality status fields except explicitly bounded extension points.
- Use local object references by default. Cross-namespace refs require explicit policy and RBAC rationale.

## Controller And Status Rules

- Controllers own observed status. Clients, console endpoints, scripts, and agents must not synthesize or write authoritative `status`.
- Controller reconciliation owns lifecycle transitions and phases for `Experiment`, `BenchmarkRun`, and `ResearchCampaign` resources.
- Status must be derived from Kubernetes Job/Pod state, parsed declared artifacts, controller-observed timestamps, and validated object references, not logs or client-side inference.
- Runner Jobs must be stateless. They report through declared artifacts, exit status, and Kubernetes-observed pod state; controllers parse and publish authoritative status.
- Use ownerReferences and stable labels between CRDs and created Jobs, PVCs, Services, ConfigMaps, Events, and related resources for garbage collection and queryability.
- Use finalizers when active Jobs, artifacts, or terminal status need cleanup or completion before deletion.
- Emit Kubernetes Events and status conditions for lifecycle transitions, parse failures, budget exhaustion, integrity failures, retries, reconciliation failures, terminal success, and terminal failure.
- Compute runtime and cost metrics such as `wall_clock_seconds`, `queue_latency_seconds`, and `gpu_hours` from controller-observed state, not self-reported workload values.

## Benchmark And Metric Rules

- Benchmark definitions belong in `BenchmarkSuite`.
- Benchmark executions belong in `BenchmarkRun`.
- Metric ingestion contracts belong in `MetricSource`.
- Benchmark controllers create/watch Kubernetes Jobs, parse declared metric artifacts, compute aggregates, update status, emit events, and expose Prometheus metrics.
- Controllers must expose Prometheus metrics for reconcile errors, status transitions, parse failures, job creation/completion/failure, and queue/runtime/cost counters where relevant.
- Agents may create or edit benchmark specs, but must not write benchmark result status directly.
- Normalized metric keys must be stable lowercase snake_case.
- Keep raw metrics in workspace artifacts; keep bounded summaries in status.
- Prometheus labels must be low-cardinality. Do not label by experiment UID, full git SHA, hypothesis text, full image digest, or arbitrary metric names.
- Parser errors must become status conditions and metrics counters, not controller panics.
- Required metric absence must prevent success for the relevant `BenchmarkRun`.

## Observability And Console

- New Athena features must include operator metrics, ServiceMonitor or Helm/Nix wiring, Grafana dashboard updates, Loki/log link behavior, and console visibility when this repo owns those surfaces.
- Dashboards and alerts must use owned metrics with low-cardinality labels, not log scraping.
- CR status must include logs, metrics, report, and artifact links when configured.
- Runtime health canaries must cover controller metrics, workspace access, logs, and GPU scheduling where relevant.
- Athena Console reads Kubernetes resources through the Rust `kube` client using local kubeconfig. It must still treat Kubernetes resources as the product API and must not write authoritative status.
- BFF responses must redact secrets, hidden holdout details, private dataset paths, and private benchmark answers.
- Watch/live updates must use Kubernetes watch/informers/SSE and include `resourceVersion`.
- Console views must reflect CR status rather than recomputing authoritative benchmark or campaign state client-side.

## GitOps And Deployment

- Do not use `kubectl apply`, `kubectl patch`, `kubectl edit`, or live cluster mutation for product changes.
- CRDs, RBAC, Helm chart changes, dashboards, ServiceMonitors, and manifests must be generated through repo-owned Nix/Helm flows.
- `modules/k8s/manifests.yaml` is generated from `.#k8s-manifests`; regenerate it with `nix build .#k8s-manifests` rather than hand-editing or hand-resolving.
- Read-only cluster inspection is allowed for debugging; writes require explicit user direction and must become Git/Nix/Flux changes unless the user explicitly requests live intervention.

## Integrity And Security

- Promotion claims require comparable benchmark inputs: suite hash, dataset hash, evaluator image digest, runtime profile generation, budget, and seed policy.
- Use immutable image digests when required by suite/profile policy.
- Hidden holdout data, answers, secret refs, and private test details must not appear in agent-visible status, console DTOs, logs, or artifacts.
- Generated-code/evaluator Jobs must use non-privileged security contexts, no hostPath mounts, bounded resources, default-deny egress unless a documented exception is required, and `automountServiceAccountToken: false` unless explicitly needed.
- Patch integrity checks must prevent edits to denied eval, data, benchmark, and holdout paths.

## Common Commands

- Build Kubernetes manifests: `nix build .#k8s-manifests`
- Build Helm chart: `nix build .#helm-chart`
- Format Nix: `nix fmt`
- Sync workload-only Python deps when changing runner/training code: `uv sync`
- Run workload-only local smoke tests only when explicitly relevant: `uv run experiments/train.py`
- Do not use local Python scripts as Athena orchestration, status, benchmark, campaign, or deployment paths.

## Jujutsu / Beads

- Use `jj` for small, issue-scoped "bead" changes when the user wants work tracked as independent items.
- Keep one user-visible problem per bead. If a change fixes multiple unrelated issues, split it before export.
- Initialize colocated mode once per worktree with `jj git init --colocate` when needed.
- Inspect current work with `jj status`, `jj diff`, and `jj log`.
- Name the current bead with `jj describe -m "<scope>: <change>"`.
- Start the next bead with `jj new`, then immediately give it a description.
- Jump back to an earlier bead with `jj edit <change-id>`.
- If a bead is too broad, use `jj split`. If it should fold into its parent, use `jj squash`.
- When the stack is ready to materialize as git commits, run `jj git export`.
- Do not use `jj` to hide unrelated changes. Keep the same discipline as with git: minimal diffs, no destructive resets, and no rewriting work the user did not ask to change.

## Validation

- For CRD/operator changes, run the relevant Rust checks/tests when practical and verify generated CRDs/manifests if affected.
- For Nix/deployment changes, run `nix fmt` and relevant `nix build` checks when practical.
- For console changes, run the relevant Rust/Iced checks when present and practical.
- For Python workload/runner code under `experiments/`, run local smoke tests only when the task explicitly concerns workload behavior. Do not use local runs to create or infer authoritative `Experiment`, `BenchmarkRun`, `ResearchCampaign`, or metric status.
- Do not fix unrelated failures; report them clearly.

## Generated / Deployment Artifacts

- `.pre-commit-config.yaml` is generated by `git-hooks.nix`; do not hand-edit it.
- `modules/k8s/manifests.yaml` is generated from `.#k8s-manifests`; regenerate instead of hand-resolving.
- `result*` paths are Nix build outputs and must not be edited.

## Safety

- Treat local logs, model outputs, and experiment artifacts as disposable unless the user says otherwise.
- Do not mutate Kubernetes cluster state imperatively for deployment changes; prefer Nix/GitOps-generated manifests.
- Read-only Kubernetes inspection is fine when useful; writes must be modeled in Git/Nix/Flux unless the user explicitly requests live intervention.
- Do not run destructive git commands such as `git reset --hard` without explicit user approval.

## GPU scheduling

All GPU workloads this operator creates flow through Kueue. The canonical strategy lives in the shared `gpu-scheduling` skill (olivecasazza/skills); the operator-local rules:

- Every RuntimeProfile that requests `nvidia.com/gpu` MUST set `scheduling.queueName`. `athena-gpu` (ns apps) for the modern pool (hp01-03 RTX 4000, seir RTX 5000); `athena-kepler` for tyan01's Titan Blacks (CUDA <= 11.4 images only). A GPU profile without a queueName bypasses quota and can strand physical capacity — treat it as a review-blocking defect.
- Experiment Jobs and BenchmarkRun Jobs are created suspended with the `kueue.x-k8s.io/queue-name` label when the profile sets queueName; Kueue unsuspends on admission. RayJobs (ResearchCampaign vLLM clusters) and the single-node inference mesh (GPU `inferenceMesh` Deployments, labels on the POD template) only need the labels — Kueue's ray.io/rayjob and pod integrations handle gating; CPU-only meshes stay unlabeled.
- Admission is quota-based, not node-based: hp01-03 power on via cluster-autoscaler + hephaestus after admission (~5-6 min to first pod start). Never pin GPU-less pods (Ray heads, viewers) to hp nodes — it keeps a physical server powered 24/7.
- Do not use raw nodeSelectors to grab GPUs outside a queue, and never target contra (its RTX 4000 SFF Ada is reserved for Plex transcode).
