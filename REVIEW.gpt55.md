# Premium Review: Athena Benchmark OpenSpec

## Executive assessment

The OpenSpec is directionally strong and much more complete than a typical first-pass CRD/operator proposal. It correctly identifies the core entities (`BenchmarkSuite`, `BenchmarkRun`, `MetricSource`), preserves the existing `research.nixlab.io/v1alpha1` API group, treats benchmark artifacts as first-class SeaweedFS outputs, and explicitly includes Prometheus/Grafana/Loki plus hp01-hp03 GPU scheduling constraints.

However, it is still too broad and underspecified to hand directly to implementation agents without creating schema churn and controller dead ends. The biggest risk is that v1alpha1 becomes a dumping ground for many loosely typed fields and status blobs before the operator has a minimal executable benchmark path. The spec should tighten API boundaries, add CRD conversion/versioning discipline, specify evaluator sandbox/security controls, and split Phase 1 into a minimal vertical slice that actually runs a toy benchmark before adding broad LLM/research-loop scope.

My recommendation: accept the architecture, but block implementation until the spec is patched for the blocking issues below. Prioritize a toy RL/runtime-health vertical slice: CRDs + one BenchmarkRun controller path + immutable artifact contract + metrics + console list/watch. Add LLM benchmark standards only after the core execution semantics are proven.

## Blocking issues

1. `ModelArtifact` appears in `BenchmarkSuite.spec.targetRefPolicy.allowedKinds`, but no CRD or schema is defined for it.
   - Either define `ModelArtifact` as a future/non-phase-1 CRD, remove it from phase-1 examples, or make `targetRef` a generic Kubernetes `ObjectReference` with explicit allowed `apiVersion/kind` validation.
   - Leaving it undefined will produce ambiguous implementation and broken validation examples.

2. The CRD schemas do not yet define structural-schema/CEL boundaries.
   - kube-rs can generate permissive CRDs, but Kubernetes requires structural OpenAPI schemas for reliable pruning/defaulting/status behavior.
   - The spec should require enums for taxonomy/mode/phase/integration/goal/failureClass, `x-kubernetes-preserve-unknown-fields` only for intentionally opaque maps, max string lengths for names/paths/URLs, and CEL validations where possible.
   - Avoid unbounded `serde_json::Value` in spec except for explicitly named extension maps.

3. Versioning and immutability are underdefined for `v1alpha1`.
   - The spec says keep `research.nixlab.io/v1alpha1`, but also introduces many fields likely to change.
   - Add a rule that phase-1 fields are additive only, no required fields added to existing CRDs without defaults, no renames inside v1alpha1, and every CRD has `status.observedGeneration` plus Conditions.
   - Add a deprecation/conversion plan for `v1beta1` before broad adoption.

4. BenchmarkSuite immutability is described as “immutable-ish”, which is not enough for benchmark integrity.
   - If a suite changes after a run starts, old results become incomparable.
   - Require controller snapshotting of the resolved suite spec into SeaweedFS and `BenchmarkRun.status.observedSuiteHash` before job creation.
   - Recommend treating any spec mutation that changes `resolvedSuiteHash` as a new suite version for comparison/gating purposes.

5. Status forgery risk is acknowledged but not enforceable as written.
   - Kubernetes RBAC can still allow users/agents to patch `/status` if they have broad CRD permissions.
   - The Helm RBAC currently grants full write permissions on all research resources and status to the operator service account; implementation must also ensure human/agent roles only create/update spec, not status.
   - Add an admission/RBAC requirement: only the Athena operator service account may update `benchmarkruns/status`, `experiments/status`, and campaign benchmark status fields.

6. Controller feasibility is underestimated.
   - The current operator reconciles only `Experiment` and does not create Jobs yet. The spec asks for multiple new controllers, artifact parsing, metrics, campaign gates, SSE console, and integrity scanning.
   - Phase 1 should not be just “API scaffolding and examples”; that creates paper APIs without proving feasibility. A minimal runnable `BenchmarkRun` controller must land before expanding to LLM suites.

7. MetricSource design is too powerful without a sandbox/trust model.
   - `custom` parsers, `stdoutRegex`, HTTP JSON, Prometheus, and Loki sources all imply different trust and network boundaries.
   - Phase 1 should support only file JSON/JSONL from the workspace. Prometheus/Loki should be read-only observability links, not arbitrary metric sources until auth, timeout, and query restrictions exist.

8. LLM benchmark standards are incomplete for modern eval rigor.
   - Missing or optional: MMLU-Pro/MMMU/BBH/IFEval/AIME-style math where appropriate, HELM-style metadata, calibration/refusal metrics, stderr/invalid-output rates, bootstrap confidence intervals, and exact prompt/generation parameter capture.
   - The spec should not imply GSM8K/MATH/HumanEval/MBPP/GPQA alone are “industry-standard” coverage.

9. Code benchmark security is underspecified.
   - HumanEval/MBPP/LiveCodeBench/SWE-bench execution requires strong sandboxing: no host mounts except scratch/workspace subdir, no Kubernetes service account token, default-deny egress, CPU/memory/pid/seccomp/AppArmor restrictions, timeouts, and test isolation.
   - Without this, generated code can exfiltrate holdouts, attack cluster metadata, or forge artifacts.

10. hp01-hp03 8GB GPU fit needs explicit model/runtime caps.
    - Quadro RTX 4000 8GB cards cannot run many modern LLM eval workloads locally except small models or remote/API-backed targets.
    - Add constraints: default LLM evals on hp GPUs are for small local models, quantized runners, or API target evaluation; no vLLM assumption for large models on 8GB; one GPU per job; memory requests must leave headroom.

11. SeaweedFS “immutable artifacts” need enforcement details.
    - The spec describes layout but not write-once semantics.
    - Require content-addressed artifact snapshots, manifest checksums, controller finalization marker, and a policy that completed run directories are never modified except by appending controller-owned status/report metadata.

12. No explicit finalizers/garbage-collection policy.
    - BenchmarkRun creates Jobs and artifacts. The spec must define ownerReferences/finalizers, TTL for Jobs, whether artifacts are retained forever, and how cancelled/deleted runs behave.

13. Existing CRD extension examples may break backwards compatibility if implemented naively.
    - Existing resources should not require new fields such as `budget`, `provenance`, or `benchmarkSuites`.
    - New spec fields must be optional/defaulted. Status additions must not replace existing `metrics`, `metricsDetail`, `artifacts`, or `cost` fields.

14. Console endpoints lack auth/RBAC and pagination constraints.
    - BFF endpoints must apply namespace scoping, redaction, pagination, watch reconnect/resourceVersion handling, and avoid exposing Secret refs or hidden holdout paths.
    - SSE without backpressure/pagination can become fragile once BenchmarkRuns contain per-task/per-seed status.

## Recommended spec changes

### CRD/API changes

- Replace string refs like `suiteRef: llm-core-v1` and `runtimeProfileRef: hp-8gb-eval` with explicit local object refs:
  - `name`
  - optional `namespace` only if cross-namespace refs are intentionally supported
  - optional `apiVersion/kind` for generic target refs
- Use a common reference struct across BenchmarkSuite, BenchmarkRun, ExperimentTemplate, and Campaign policies.
- Add `status.observedGeneration`, `status.conditions[]`, `status.lastTransitionTime`, and `status.controllerVersion` to every new CRD.
- Add `metadata.finalizers: [athena.nixlab.io/benchmark-run]` behavior for BenchmarkRun while child Jobs are active or artifact finalization is incomplete.
- Add a `spec.suspend: bool` field to BenchmarkRun and ResearchCampaign benchmark policy for safe GitOps pauses.
- Add a clear phase enum including `Pending`, `Preparing`, `Running`, `Succeeded`, `Failed`, `Error`, `Cancelled`, and `Skipped` if task-level skipped states are allowed.
- Keep high-volume per-item results out of CR status for large suites. Store full item-level results in SeaweedFS and put only aggregates plus bounded task summaries in status.
- Bound status size explicitly. Kubernetes object size limits make full per-seed/per-task/per-item details dangerous.

### BenchmarkSuite changes

- Rename `version` to either `suiteVersion` or make it semver/date plus `resolvedSuiteHash`; avoid ambiguity with CRD API version.
- Define task labels as a map and require stable `taskId`/`taskVersion` for benchmark comparability.
- Require `datasetRef` to include source type (`hfDataset`, `seaweedfs`, `s3`, `pvc`, `secretHoldout`) and immutable revision/hash.
- Add `evaluationProtocol` fields for prompt template, answer extraction, generation parameters, scoring function version, and timeout.
- Add optional statistical gate fields: `minDelta`, `confidenceLevel`, `bootstrapSamples`, `pairedTest`, and `minSeeds`.
- Require suite snapshots to be written to `/workspace/benchmarks/suites/<suite-name>/<suite-hash>/suite.json` before task Jobs launch.

### BenchmarkRun/controller changes

- Add explicit concurrency policy:
  - per-run `maxParallelTasks`
  - cluster/profile-level `maxConcurrentRuns`
  - campaign-level budget arbitration
- Add cancellation semantics:
  - `spec.suspend=true` stops new Jobs
  - deletion/finalizer cancels active Jobs and records final status if possible
- Require ownerReferences from task Jobs to BenchmarkRun.
- Require deterministic job name hashing with collision handling and Kubernetes 63-character limit tests.
- Add retry attempt numbering to job labels and artifact path: `attempt-<n>`.
- Add `podSpecTemplate` only if tightly controlled by RuntimeProfile, not agent-provided BenchmarkRun spec.
- Define how the controller reads SeaweedFS artifacts: PVC mount in controller, sidecar uploader, or Kubernetes Job logs/artifacts. The current spec assumes workspace prep but not the actual controller access path.

### Metric and report changes

- Add required denominator fields for all rates: `attempted_run_count`, `failed_run_count`, `evaluated_item_count`, `passed_item_count`, `invalid_item_count`.
- Add confidence interval fields for aggregate metrics: `ci_low`, `ci_high`, `confidence_level`, `method`.
- Add contamination/leakage metrics: `leakage_scan_matches`, `contamination_risk`, `blocked_item_count`.
- Add invalid/refusal/nonparseable metrics for LLM evals: `invalid_output_count`, `abstain_count`, `format_error_count`.
- Use Prometheus histograms/counters with bounded labels only. Do not expose one metric per benchmark metric key if the key is unbounded.

### Observability/GitOps changes

- Add Helm chart/RBAC tasks when new CRDs are introduced:
  - CRDs included under `charts/athena/crds/`
  - ClusterRole includes new resources and `/status`
  - ServiceMonitor still scrapes `/metrics`
  - GrafanaDashboard has panels for benchmark runs, task failures, queue latency, parse errors, and GPU-hours
- For nixlab GitOps fit, add a later explicit phase for `/home/olive/Repositories/nixlab` only after repo-local CRDs/controller pass:
  - Flux HelmRelease values
  - SeaweedFS PVC wiring
  - RuntimeProfile resources for hp01-hp03
  - CiliumNetworkPolicy/default-deny allowances for DNS, Kubernetes API if needed, SeaweedFS service CIDR, Prometheus scrape, and restricted/no-egress evaluator Jobs
- Add Loki label cardinality guidance: labels for suite/run/task/phase are OK; no git SHA, hypothesis, full image digest, or prompt hash as labels.

### Integrity/anti-cheating changes

- Make holdout execution a separate evaluator-only container or Job stage with no agent-visible mount containing hidden data.
- Disable automounting Kubernetes service account tokens in evaluator/code-execution Jobs unless needed.
- Require default-deny network policies for code-generation benchmark sandboxes; allow egress only for explicitly declared package mirrors or none at all.
- Add patch integrity checks for symlinks/path traversal, generated files, hidden files, submodules, and attempts to modify benchmark harness/eval code.
- Require baseline and candidate to use identical suite hash, evaluator image digest, dataset hashes, runtime profile generation, and budget for claims of improvement.
- Add replay tolerance schema: metric-specific absolute/relative tolerance and nondeterminism notes.
- Add audit trail: controller records who/what created the BenchmarkRun, target artifact digest, and status update provenance.

## Suggested first implementation milestones

### Milestone 0: repo reality check and API guardrails

- Inspect existing CRD generation flow and Helm chart packaging.
- Add a small `api/common.rs` with reusable refs, conditions, budget, metric summary, artifact URI, and failure class structs.
- Add tests that generated CRDs are structural and existing example resources still deserialize.
- No controller expansion yet except compile/test plumbing.

### Milestone 1: minimal runnable BenchmarkRun vertical slice

- Implement `BenchmarkSuite`, `BenchmarkRun`, and `MetricSource` with only:
  - taxonomy `runtimeHealth` and `rlTraining`
  - integration `toyRl` and `customCommand`
  - MetricSource `file/json`
- Add generated CRDs to `charts/athena/crds/athena-crds.yaml`.
- Update Helm RBAC for new resources and status.
- Implement a BenchmarkRun controller that creates one CPU toy canary Job, reads `/workspace/.../metrics.json`, writes bounded status, and emits Prometheus counters/histograms.
- Verify with `cargo test --workspace`, `cargo fmt --check`, and a generated-CRD freshness check.

### Milestone 2: SeaweedFS artifact and reproducibility contract

- Write `suite.json`, `run.json`, `artifact-manifest.json`, `metrics.json`, `report.json`, and `report.md` for the toy benchmark.
- Compute `resolvedSuiteHash` and `reproducibilityHash` from normalized JSON.
- Enforce image digest and git commit requirements when enabled.
- Add replay mode for the toy runner with metric tolerance.

### Milestone 3: hp01-hp03 GPU canary

- Add RuntimeProfile example for `hp-8gb-gpu-benchmark` with one `nvidia.com/gpu`, node affinity/selector constrained to hp01-hp03, conservative memory requests, and no multi-GPU assumptions.
- Implement a CUDA/PyTorch smoke runner that records GPU model, visible device count, allocated GPU count, memory, wall-clock, and artifact hash.
- Add queue latency and pod start latency metrics.

### Milestone 4: observability and console read path

- Add benchmark Prometheus metrics and a meaningful GrafanaDashboard example with more than a single vanity panel.
- Add BFF list/detail endpoints for BenchmarkSuite and BenchmarkRun with redaction and pagination.
- Add Vue list/detail views using existing Nord/pink/global styling conventions.
- Add SSE watch only after list/detail DTOs are stable.

### Milestone 5: first LLM eval runner contract

- Add lm-evaluation-harness integration for a tiny, cheap public task first.
- Record harness version, task version, prompt template hash, model ID, generation parameters, answer extraction policy, invalid outputs, and exact raw result URI.
- Do not add SWE-bench execution until sandbox/network policy is complete.

### Milestone 6: campaign integration and promotion gates

- Add baseline creation and candidate promotion gates only after BenchmarkRun status and artifacts are stable.
- Implement seed aggregation, failed-run denominators, confidence intervals, and “same suite hash/budget/evaluator” checks for improvement claims.

## Missing benchmark standards and anti-cheating controls

### Benchmark standards to consider

- MMLU-Pro or MMLU as a general knowledge baseline, with exact prompt/scoring version pinned.
- BBH or BIG-Bench-derived reasoning tasks for chain-of-thought-sensitive evaluation, if appropriate.
- IFEval for instruction-following compliance.
- AIME/AMC-style math or another contamination-aware math set if testing frontier math behavior; keep private variants hidden.
- MMMU or other multimodal tasks only if Athena later supports multimodal artifacts/runners; do not include in phase 1.
- HELM-style metadata fields: scenario, adapter, prompt, model deployment, decoding, seeds, software versions, and tokenizer details.
- For coding: EvalPlus variants for HumanEval/MBPP if feasible, plus strict sandboxing before executing generated code.

### Anti-cheating controls to add

- Per-run evaluator sandbox with `automountServiceAccountToken: false`, read-only root filesystem where possible, seccomp/runtime default, no privileged mode, no hostPath, bounded ephemeral storage, and no cluster credentials.
- Cilium/default-deny NetworkPolicy for evaluator/code execution Jobs; explicit DNS/package-mirror exceptions only when required.
- No hidden holdout data in shared SeaweedFS paths visible to agent Jobs.
- Redacted status/report DTOs for private holdouts: show counts/categories/aggregate scores only.
- Patch scanning for path traversal, symlink escapes, submodule changes, binary blobs, modifications to eval/data/benchmark directories, and changes to lockfiles when not allowed.
- Dataset contamination checks against prompt/test signatures and generated patches/prompts.
- Required baseline under identical suite hash, dataset hash, evaluator image digest, runtime profile generation, and budget.
- Replay mode with metric-specific tolerance before a campaign can mark a result as reproducible.
- Immutable finalization marker for completed artifacts plus manifest checksums for every file referenced by reports.
- RBAC/admission split: agents may create BenchmarkRuns and Experiments but must not update `/status` or controller-owned report fields.

## Obvious doc fixes applied

I patched `openspec.md` to address two immediate correctness issues:

1. Clarified that `ModelArtifact` is not part of phase 1 unless a CRD/schema is added.
2. Added initial CRD implementation requirements for structural schemas, bounded status, suite snapshotting, and status ownership/RBAC.
