# OpenSpec: Athena Benchmark Architecture for RL Autoresearch

## 1. Purpose and acceptance criteria

Athena must become an industry-standard benchmarking substrate for autonomous ML/RL research on Kubernetes. The benchmark system must answer four questions without ad-hoc scripts:

1. Did a training run actually improve the objective under a fixed budget?
2. Did an LLM/research agent improve capability on accepted public and private holdout suites?
3. Did an autonomous research loop discover novel, reproducible improvements rather than exploit the evaluator?
4. Is the operator/runtime healthy enough that benchmark results are trustworthy?

This document is the build spec for implementation agents. It intentionally defines API shapes, controller behavior, artifact contracts, console requirements, and rollout order. Do not implement code while editing this spec; implement in the phases in section 14.

Acceptance criteria:

- CRD API group remains `research.nixlab.io/v1alpha1` until an explicit version bump is planned.
- Existing `Experiment`, `ExperimentTemplate`, `ResearchCampaign`, and `RuntimeProfile` resources continue to work after additive schema changes.
- New first-class CRDs exist for `BenchmarkSuite`, `BenchmarkRun`, and `MetricSource`.
- Benchmark execution supports RL training, LLM capability evals, autonomous research-loop evals, and operator/runtime health suites.
- Every benchmark run writes immutable artifacts and structured metrics to the SeaweedFS workspace.
- Athena Console can list, watch, compare, and report benchmark results using the Rust/Iced workbench.
- Prometheus/Grafana/Loki observability is available from the first implementation phase, not bolted on later.
- hp01-hp03 GPU workers are schedulable targets for training/eval jobs, each with one Quadro RTX 4000 8GB GPU.

## 2. Current repository context

Repository: `/home/olive/Repositories/athena-operator`

Relevant code:

- `operator/`: Rust kube-rs operator and CRD API crate.
- `operator/crates/athena-api/src/experiment.rs`: current `Experiment` API.
- `operator/crates/athena-api/src/experiment_template.rs`: current `ExperimentTemplate` API.
- `operator/crates/athena-api/src/research_campaign.rs`: current `ResearchCampaign` API.
- `operator/crates/athena-api/src/runtime_profile.rs`: current `RuntimeProfile` API.
- `operator/crates/athena/src/reconciler.rs`: current experiment reconciliation/job generation.
- `operator/crates/athena/src/metrics.rs`: current Prometheus exporter.
- `operator/crates/athena-console/src/main.rs`: Rust/Iced console using typed Athena CRD APIs.
- `examples/canary.yaml` and `canary-test.yaml`: current canary resources.

Non-goals for this step:

- Do not edit `/home/olive/Repositories/nixlab`.
- Do not create GitOps manifests in this step.
- Do not implement controller, console, or CRD code while rewriting this spec.

## 3. Design principles

Athena separates intent, benchmark definition, metric ingestion, execution policy, and infrastructure. Phase 1 deliberately implements a small vertical slice first (`runtimeHealth`/`rlTraining`, `toyRl`/`customCommand`, and file JSON metrics) so the operator proves executable semantics before broad LLM/campaign scope:

- `BenchmarkSuite`: benchmark definition versioned by `suiteVersion` and `resolvedSuiteHash`. Defines tasks, datasets, evaluators, seed matrices, holdouts, metrics, budgets, and pass/fail gates. Any spec mutation that changes the resolved hash is treated as a new comparable suite version.
- `BenchmarkRun`: one execution of a suite or subset of a suite against a target artifact, model, image, git ref, experiment, campaign, or runtime profile.
- `MetricSource`: reusable metric ingestion definition. Describes where metrics come from and how to parse/normalize them.
- `ExperimentTemplate`: project-owned experiment contract. Extended to reference benchmark suites, integrity policy, and artifact schema.
- `Experiment`: one concrete trial. Extended to carry seed, budget, provenance, benchmark links, reproducibility hash, and normalized metric summaries.
- `ResearchCampaign`: multi-run loop. Extended to define benchmark gates, novelty constraints, duplicate-hypothesis detection, promotion criteria, and campaign-level comparisons.
- `RuntimeProfile`: admin-owned execution policy. Extended to define benchmark runners, GPU scheduling, cache/workspace mounts, metrics endpoints, and anti-cheating controls.

The operator owns reconciliation and status updates. Agents and humans may create specs and write decisions, but they must not forge benchmark status. RBAC/admission must grant `/status` writes only to the Athena operator service account for benchmark, experiment, and campaign status fields.

API guardrails:

- Phase-1 changes to existing CRDs are additive only. Do not add required fields without defaults and do not rename existing v1alpha1 fields.
- Every new CRD has structural OpenAPI schemas, bounded enums, `status.observedGeneration`, bounded `status.conditions[]`, and `status.controllerVersion`.
- Avoid unbounded `serde_json::Value` in specs except explicitly named extension maps. Large item-level results live in SeaweedFS artifacts, not CR status.
- Use common local object references with `name` and optional `namespace`; generic target refs include `apiVersion`, `kind`, and `name`. Cross-namespace refs are disabled unless explicitly allowed by policy.
- `ModelArtifact` is a future target kind and not part of phase 1 unless a CRD/schema is added in the same change.
- Plan a `v1beta1` conversion/deprecation step before broad multi-user adoption.

## 4. Benchmark taxonomy

Athena must model four benchmark classes with one common API.

### 4.1 RL training benchmarks

Purpose: measure training quality, stability, efficiency, and reproducibility under fixed compute/time budgets.

Canonical tasks:

- Toy/canary RL environments that run quickly and catch regressions.
- Offline RL or supervised-proxy training loops for deterministic CI.
- Project-specific training tasks from `ExperimentTemplate` source repos.
- Ablation matrices over algorithm, seed, reward function, learning rate, KL coefficient, entropy bonus, batch size, precision, and model size.

Required metrics:

- `reward_mean`: mean episodic or evaluation reward.
- `reward_std`: reward standard deviation across episodes/seeds.
- `kl`: KL divergence when policy/reference comparison exists.
- `entropy`: policy entropy or token entropy.
- `held_out_score`: score on held-out environment split or hidden eval set.
- `time_to_best_seconds`: wall-clock time until best objective metric.
- `gpu_hours`: GPU allocation multiplied by run duration.
- `wall_clock_seconds`: run duration from controller-observed start/end.
- `failed_run_rate`: campaign/suite aggregate failure fraction.
- `reproducibility_hash`: hash over source/image/data/config/seed/evaluator.

### 4.2 LLM capability eval benchmarks

Purpose: compare candidate models, prompts, fine-tunes, inference runtimes, and research outputs against accepted public eval suites and private holdouts.

Standard integrations:

- `lm-evaluation-harness` as the default harness for text capability tasks.
- GSM8K for grade-school math.
- MATH for competition math.
- HumanEval and MBPP for code generation.
- GPQA for graduate-level science QA.
- LiveCodeBench for time-aware coding tasks.
- SWE-bench-like hooks for repository issue-resolution tasks. Athena must define hooks and artifact contracts without requiring full SWE-bench implementation in phase 1.

Required metrics:

- `accuracy`, `exact_match`, `pass_at_1`, `pass_at_k`, `pass_k_denominator`.
- `held_out_score` for private/hidden splits.
- `tokens_per_second` for generation throughput.
- `total_tokens`, `prompt_tokens`, `completion_tokens`.
- `wall_clock_seconds`, `gpu_hours`.
- `failed_item_count`, `failed_run_rate`.
- `reproducibility_hash`.

### 4.3 Autonomous research-loop benchmarks

Purpose: score the whole autoresearch loop, not just a single training job.

Benchmark objects:

- A `ResearchCampaign` with a fixed budget and strategy.
- A starting source commit and one or more accepted output patches.
- A hypothesis stream generated by an agent.
- A set of baseline and holdout suites.

Required metrics:

- `best_objective`: best primary metric discovered.
- `time_to_best_seconds`: elapsed time from campaign start to best experiment completion.
- `experiments_to_best`: count of completed experiments before best.
- `duplicate_hypothesis_rate`: fraction of hypotheses whose normalized semantic/signature hash already appeared in the campaign.
- `novelty_score`: optional score from a configured novelty evaluator.
- `promotion_rate`: fraction of experiments marked `Keep`.
- `revert_rate`: fraction of kept patches later rejected by holdout or reproducibility checks.
- `failed_run_rate`.
- `gpu_hours` and `token_usage` aggregates.
- `held_out_score` of the final promoted artifact.

### 4.4 Operator/runtime health benchmarks

Purpose: distinguish research failure from infrastructure failure.

Required suites:

- `athena-canary-cpu`: no-GPU job that writes valid `metrics.json`.
- `athena-canary-gpu`: CUDA-visible job on hp01-hp03 that records GPU model and memory.
- `athena-canary-seaweedfs`: read/write/list/delete test in the workspace PVC.
- `athena-canary-metrics`: operator `/metrics` scrape and status propagation test.
- `athena-canary-logs`: Loki/Grafana log link generation test.
- `athena-canary-preemption`: optional retry/resume test for evicted jobs.

Required metrics:

- `queue_latency_seconds`.
- `pod_start_latency_seconds`.
- `workspace_prepare_seconds`.
- `metrics_parse_seconds`.
- `status_update_latency_seconds`.
- `retry_count`.
- `failed_run_rate`.
- `controller_reconcile_errors_total`.

## 5. CRD design

All new CRDs are namespaced, use `research.nixlab.io/v1alpha1`, and are implemented in `operator/crates/athena-api/src/` with generated CRDs included in the existing operator build flow.

### 5.1 `BenchmarkSuite`

Kind: `BenchmarkSuite`
Plural: `benchmarksuites`
Shortname: `bsuite`
Owner: ML engineer or benchmark maintainer.

Spec fields:

```yaml
apiVersion: research.nixlab.io/v1alpha1
kind: BenchmarkSuite
metadata:
  name: llm-core-v1
spec:
  taxonomy: llmCapability        # rlTraining | llmCapability | researchLoop | runtimeHealth
  suiteVersion: "2026-05-20"
  description: "Core LLM capability evals for Athena candidates"
  suiteHash: "sha256:<optional-precomputed-hash>"
  targetRefPolicy:
    allowedKinds: [Experiment, ResearchCampaign, RuntimeProfile]
  tasks:
    - name: gsm8k
      integration: lmEvaluationHarness
      datasetRef:
        name: gsm8k
        split: test
        revision: "sha256-or-hf-revision"
      evaluator:
        image: ghcr.io/nixlab/athena-lm-eval:<digest>
        command: ["athena-lm-eval"]
        argsTemplate:
          - "--task=gsm8k"
          - "--model={{ target.model }}"
      metrics:
        primary: exact_match
        goal: maximize
        required:
          - exact_match
          - wall_clock_seconds
          - tokens_per_second
          - reproducibility_hash
      budget:
        maxWallClock: "2h"
        maxGpuHours: 2
        maxTokens: 2000000
      seeds:
        values: [1, 2, 3]
        aggregation: meanStd
  metricSources:
    - name: harness-json
      metricSourceRef: lm-eval-json
  integrity:
    holdoutPolicy: publicOnly     # publicOnly | privateHoldout | mixed
    leakagePolicy: blockOnMatch   # warn | blockOnMatch
    immutableInputsRequired: true
    requireImageDigest: true
    requireGitCommit: true
    requireDatasetHash: true
  reporting:
    compareAgainst:
      - kind: BenchmarkRun
        name: baseline-llm-core-v1
    gates:
      - metric: exact_match
        operator: ">="
        value: 0.5
```

Status fields:

```yaml
status:
  ready: true
  observedGeneration: 3
  resolvedSuiteHash: sha256:...
  taskCount: 6
  publicTaskCount: 5
  holdoutTaskCount: 1
  conditions:
    - type: Ready
      status: "True"
      reason: Validated
  lastValidationTime: "2026-05-20T00:00:00Z"
```

Validation rules:

- `taxonomy` is required and enum-valued.
- Every task must define `name`, `integration`, `metrics.primary`, `metrics.goal`, and `budget`.
- `integrity.requireImageDigest=true` means evaluator images must use immutable digests, not mutable tags.
- Private holdout task details may reference Secrets or sealed paths, but status and console must not reveal answers.
- `suiteHash` is computed from normalized spec excluding status and mutable metadata when omitted.

### 5.2 `BenchmarkRun`

Kind: `BenchmarkRun`
Plural: `benchmarkruns`
Shortname: `brun`
Owner: agent, campaign controller, human researcher, or scheduled canary.

Spec fields:

```yaml
apiVersion: research.nixlab.io/v1alpha1
kind: BenchmarkRun
metadata:
  name: exp-042-llm-core
spec:
  suiteRef: llm-core-v1
  targetRef:
    apiVersion: research.nixlab.io/v1alpha1
    kind: Experiment
    name: exp-042
  mode: full                 # full | subset | smoke | holdoutOnly | replay
  taskSelector:
    names: [gsm8k, humaneval]
    labels:
      costTier: cheap
  runtimeProfileRef: hp-8gb-eval
  budget:
    maxWallClock: "4h"
    maxGpuHours: 4
    maxTokens: 5000000
    maxRetries: 1
  seedMatrix:
    seeds: [1, 2, 3]
    deterministic: true
  output:
    workspacePath: /workspace/benchmarks/llm-core-v1/runs/exp-042-llm-core
  promotionPolicy:
    updateExperimentStatus: true
    blockOnHoldoutFailure: true
```

Status fields:

```yaml
status:
  phase: Running             # Pending | Preparing | Running | Succeeded | Failed | Error | Cancelled
  startTime: "2026-05-20T00:00:00Z"
  completionTime: null
  observedSuiteHash: sha256:...
  reproducibilityHash: sha256:...
  jobNames:
    - athena-brun-exp-042-gsm8k-s1
  taskResults:
    - name: gsm8k
      seed: 1
      phase: Succeeded
      metrics:
        exact_match: 0.72
        wall_clock_seconds: 812
        tokens_per_second: 38.4
      artifacts:
        metricsUri: seaweedfs://athena/benchmarks/.../metrics.json
        reportUri: seaweedfs://athena/benchmarks/.../report.json
      nodeName: hp01
      gpuType: Quadro RTX 4000
  aggregateMetrics:
    exact_match:
      mean: 0.71
      std: 0.02
      min: 0.69
      max: 0.73
    pass_at_1:
      mean: 0.43
  cost:
    gpuHours: 1.8
    wallClockSeconds: 2210
    totalTokens: 1200000
  gates:
    - metric: exact_match
      passed: true
      threshold: 0.5
  logsLink: https://grafana/.../explore?query=...
  metricsLink: https://grafana/.../d/athena-benchmarks
  reportUri: seaweedfs://athena/benchmarks/llm-core-v1/runs/exp-042-llm-core/report.md
  conditions:
    - type: Complete
      status: "False"
      reason: RunningTasks
```

Controller behavior:

- Resolve `BenchmarkSuite`, `RuntimeProfile`, `MetricSource`, and target refs before creating jobs.
- Snapshot the resolved suite spec to `/workspace/benchmarks/suites/<suite-name>/<suite-hash>/suite.json` and set `BenchmarkRun.status.observedSuiteHash` before creating task jobs.
- Refuse to run if immutable image/git/data hashes required by the suite are missing.
- Create one Kubernetes `Job` per task/seed unless the suite task declares `execution.grouped=true`; grouped execution is preferred for tiny canaries to reduce pod startup/API overhead.
- Add `spec.suspend`, `spec.maxParallelTasks`, retry attempt numbering, deterministic 63-character-safe job names, ownerReferences, and the `athena.nixlab.io/benchmark-run` finalizer while jobs or artifact finalization are active.
- Support `cleanupPolicy` for completed task Jobs; artifacts are retained according to the artifact retention policy, independent of Job cleanup.
- Add labels to every job/pod: `athena.nixlab.io/benchmark-run`, `athena.nixlab.io/benchmark-suite`, `athena.nixlab.io/task`, `athena.nixlab.io/seed`, `athena.nixlab.io/target-kind`, `athena.nixlab.io/target-name`.
- Update status from Kubernetes job state, parsed metrics, and artifact existence.
- Compute aggregate metrics only after all required task results complete.
- Do not mark `Succeeded` if required metrics are missing or unparsable.
- On deletion, finalizer cancellation deletes active Jobs, records terminal status when possible, and leaves finalized artifacts intact unless retention policy says otherwise.

### 5.3 `MetricSource`

Kind: `MetricSource`
Plural: `metricsources`
Shortname: `msrc`
Owner: platform/benchmark maintainer.

Spec fields:

```yaml
apiVersion: research.nixlab.io/v1alpha1
kind: MetricSource
metadata:
  name: lm-eval-json
spec:
  sourceType: file             # file | stdoutRegex | prometheus | loki | httpJson | artifactManifest
  path: metrics.json
  format: json                 # json | jsonl | prometheusText | regex | junit | custom
  metrics:
    - name: exact_match
      path: $.results.gsm8k.exact_match
      type: number
      required: true
      normalize:
        multiply: 1.0
    - name: tokens_per_second
      path: $.performance.tokens_per_second
      type: number
      required: false
  timestampPath: $.created_at
  failureRules:
    - path: $.status
      equals: failed
      reason: evaluatorFailed
```

Status fields:

```yaml
status:
  ready: true
  lastValidationTime: "2026-05-20T00:00:00Z"
  sampleValidated: true
  message: "validated against examples/lm-eval-metrics.json"
```

Metric extraction requirements:

- Phase 1 supports only `sourceType=file` with `format=json` or `jsonl`. Regex/stdout, Prometheus, Loki, HTTP JSON, and custom parsers are later phases after timeout/auth/query restrictions are defined.
- Parsing must use safe JSONPath/simple extraction only; no dynamic code execution in the operator.
- Metric names are lowercase snake_case in status.
- Numeric values must remain numeric in JSON status and Prometheus labels must not contain unbounded metric names.
- Parser errors are status conditions, not controller panics.
- Raw metric files remain in SeaweedFS even if parsing fails.

## 6. Extensions to existing CRDs

### 6.1 `ExperimentTemplate` extensions

Add fields:

```yaml
spec:
  benchmarkSuites:
    baseline:
      - name: toy-rl-canary-v1
      - name: llm-core-v1
    holdout:
      - name: private-rl-holdout-v1
    promotion:
      requiredSuites: [toy-rl-canary-v1]
      optionalSuites: [llm-core-v1]
  artifactContract:
    metricsPath: metrics.json
    manifestPath: artifact-manifest.json
    checkpointGlob: checkpoints/**/*
    reportGlob: reports/**/*
  integrity:
    requireCleanGitTree: true
    requirePatchApplies: true
    allowedPatchPaths:
      - train.py
      - configs/**
    deniedPatchPaths:
      - data/**
      - eval/**
      - benchmarks/**
    requireReproducibilityHash: true
  metricSchema:
    required:
      - reward_mean
      - wall_clock_seconds
    optional:
      - reward_std
      - kl
      - entropy
      - held_out_score
      - tokens_per_second
```

Status additions:

```yaml
status:
  resolvedBenchmarkSuites:
    - toy-rl-canary-v1
  validation:
    artifactContractValid: true
    integrityPolicyValid: true
```

### 6.2 `Experiment` extensions

Add fields:

```yaml
spec:
  seed: 1
  seedGroup: default
  benchmarkRunRefs:
    - exp-042-toy-rl
  budget:
    maxWallClock: "30m"
    maxGpuHours: 0.5
    maxTokens: 0
  provenance:
    gitUrl: https://github.com/...
    gitRef: main
    gitCommit: <full-sha>
    containerImage: ghcr.io/nixlab/athena-runner@sha256:...
    datasetHashes:
      train: sha256:...
      eval: sha256:...
    parentExperimentRefs: [exp-041]
```

Status additions:

```yaml
status:
  normalizedMetrics:
    reward_mean: 123.4
    reward_std: 5.6
    kl: 0.02
    entropy: 1.8
    held_out_score: 0.71
    gpu_hours: 0.42
    tokens_per_second: 38.4
    wall_clock_seconds: 1512
    time_to_best_seconds: 903
  benchmarkRuns:
    - name: exp-042-toy-rl
      suite: toy-rl-canary-v1
      phase: Succeeded
      aggregateMetrics:
        reward_mean:
          mean: 123.4
          std: 5.6
  reproducibilityHash: sha256:...
  integrity:
    immutableInputsVerified: true
    leakageScanPassed: true
    holdoutPassed: true
    duplicateHypothesis: false
  failureClass: null          # userCode | infrastructure | budgetExceeded | integrityViolation | metricParseError
```

### 6.3 `ResearchCampaign` extensions

Add fields:

```yaml
spec:
  benchmarkPolicy:
    runBaselineBeforeSearch: true
    runOnEveryExperiment: [toy-rl-canary-v1]
    runOnPromotion: [llm-core-v1]
    runHoldoutOnBestOnly: true
    compareAgainstBaseline: true
  searchIntegrity:
    hypothesisSimilarityThreshold: 0.92
    maxDuplicateHypothesisRate: 0.2
    requireSeedMatrix: true
    minSeedsForPromotion: 3
  budget:
    maxExperiments: 300
    maxDuration: "24h"
    maxGpuHours: 72
    maxTokens: 100000000
    maxFailedRunRate: 0.25
```

Status additions:

```yaml
status:
  aggregateMetrics:
    best_objective: 0.73
    time_to_best_seconds: 18400
    experiments_to_best: 42
    duplicate_hypothesis_rate: 0.08
    failed_run_rate: 0.11
    gpu_hours: 51.2
  baselineBenchmarkRun: baseline-llm-core-v1
  bestBenchmarkRun: exp-042-llm-core
  promotedExperiments: [exp-042]
  rejectedForIntegrity: [exp-017]
  budgetUsed:
    gpuHours: 51.2
    tokens: 84000000
    wallClockSeconds: 70200
```

### 6.4 `RuntimeProfile` extensions

Add fields:

```yaml
spec:
  benchmarkRunner:
    supportedIntegrations:
      - lmEvaluationHarness
      - customCommand
      - toyRl
      - liveCodeBench
      - sweBenchHook
    defaultMetricSourceRef: file-metrics-json
    serviceDependencyMode: sidecarOrServiceRef
  scheduling:
    nodeSelector:
      nixlab.io/pool: hpc
    allowedNodeNames: [hp01, hp02, hp03]
    gpuResourceName: nvidia.com/gpu
    gpuProduct: Quadro RTX 4000
    maxGpuMemoryGiB: 8
  storage:
    workspaceClaimName: athena-workspace
    workspaceMountPath: /workspace
    cachePaths:
      datasets: /workspace/datasets
      models: /workspace/models
      evals: /workspace/evals
  observability:
    prometheusScrape: true
    metricsPort: 8080
    lokiLabels:
      - athena.nixlab.io/experiment
      - athena.nixlab.io/benchmark-run
  policy:
    requireImageDigest: true
    allowImageOverride: false
    allowCommandOverride: false
    allowSecretRefs: false
    allowedSecretRefs: []
```

Status additions:

```yaml
status:
  ready: true
  capacity:
    matchingNodes: 3
    gpuCount: 3
    gpuProduct: Quadro RTX 4000
  lastCanaryBenchmarkRun: athena-canary-gpu-20260520
```

## 7. Metric model

Athena has two metric layers:

1. Raw task metrics: stored exactly as emitted by evaluator/training code under the run artifact directory.
2. Normalized metrics: stable lowercase snake_case keys on CR status and reports.

Required normalized metric keys:

- `reward_mean`
- `reward_std`
- `kl`
- `entropy`
- `pass_at_k`
- `pass_at_1`
- `held_out_score`
- `gpu_hours`
- `tokens_per_second`
- `wall_clock_seconds`
- `failed_run_rate`
- `duplicate_hypothesis_rate`
- `time_to_best_seconds`
- `reproducibility_hash`

Recommended additional keys:

- `accuracy`
- `exact_match`
- `loss`
- `eval_loss`
- `samples_per_second`
- `total_tokens`
- `prompt_tokens`
- `completion_tokens`
- `queue_latency_seconds`
- `pod_start_latency_seconds`
- `workspace_prepare_seconds`
- `metrics_parse_seconds`
- `retry_count`

Aggregation rules:

- For seed matrices, compute `mean`, `std`, `min`, `max`, `count`, and list failed seeds.
- For pass@k, store both `pass_at_k` and `k`; do not infer k from the metric name alone.
- For all rates, include denominators. Example: `failed_run_rate=failed/attempted` with `attempted_run_count` and `failed_run_count`; item metrics include `evaluated_item_count`, `passed_item_count`, `invalid_item_count`, and `blocked_item_count` where applicable.
- Aggregate metrics may include `ci_low`, `ci_high`, `confidence_level`, and `method` when enough seeds/items exist.
- For `time_to_best_seconds`, use controller timestamps, not self-reported job timestamps.
- For `gpu_hours`, compute from requested/allocated GPU count and controller-observed runtime; evaluator-provided GPU hours may be stored as raw metrics only.
- For `reproducibility_hash`, hash normalized JSON containing suite hash, task name, seed, image digest, git commit, patch hash, dataset hashes, runtime profile name/generation, evaluator image digest, and metric source version.

Prometheus metrics to expose from `operator/crates/athena/src/metrics.rs`:

- `athena_experiments_total{namespace,campaign,phase}` (existing).
- `athena_benchmark_runs_total{namespace,suite,phase}`.
- `athena_benchmark_task_runs_total{namespace,suite,task,phase}`.
- `athena_benchmark_run_duration_seconds{namespace,suite}` histogram.
- `athena_benchmark_gpu_hours_total{namespace,suite}` counter.
- `athena_benchmark_failed_run_rate{namespace,suite}` gauge.
- `athena_operator_reconcile_errors_total{controller,kind}` counter.
- `athena_workspace_prepare_seconds{namespace}` histogram.
- `athena_metric_parse_errors_total{namespace,metric_source}` counter.

Avoid high-cardinality labels such as experiment UID, hypothesis text, git SHA, or full image digest in Prometheus. Put those in CR status and artifacts.

## 8. Standard benchmark integrations

### 8.1 `lm-evaluation-harness`

Integration enum: `lmEvaluationHarness`.

Runner requirements:

- Accept target model via template variables from `BenchmarkRun.spec.targetRef` or explicit target config.
- Emit raw harness JSON to `raw/lm-eval/results.json`.
- Emit normalized `metrics.json` at the task/seed artifact root.
- Support GSM8K, MATH, HumanEval, MBPP, and GPQA task names.
- Record harness version, task version, dataset revision, prompt template hash, generation parameters, and model identifier.

### 8.2 GSM8K and MATH

- Default metric: `exact_match` or harness-equivalent normalized score.
- Require prompt/template hash in artifact manifest.
- Holdout/private variants must hide examples and answers from agent-readable workspace paths.

### 8.3 HumanEval and MBPP

- Default metrics: `pass_at_1`, `pass_at_k`.
- Run generated code in a locked-down sandbox image.
- Store failing test names only when public; private holdout failures must expose counts and categories, not hidden assertions.

### 8.4 GPQA

- Default metric: `accuracy`.
- Record answer extraction policy and prompt hash.
- Store invalid/abstain counts separately.

### 8.5 LiveCodeBench

- Integration enum: `liveCodeBench`.
- Pin dataset release date/revision.
- Include contamination/leakage notes in artifact manifest.
- Default metrics: `pass_at_1`, `pass_at_k`, `wall_clock_seconds`.

### 8.6 SWE-bench-like hooks

Integration enum: `sweBenchHook`.

Phase 1 only needs hooks and artifact contracts:

- Input: repo URL, base commit, issue text, test command, public/hidden test split descriptor.
- Output: patch diff, apply status, public test status, hidden test aggregate, logs, reproduction script.
- Metrics: `resolved_rate`, `public_tests_passed`, `held_out_score`, `wall_clock_seconds`.

Do not block the benchmark CRD on a full SWE-bench implementation.

### 8.7 Toy RL/canary suites

Integration enum: `toyRl`.

Required canaries:

- Fast CPU deterministic toy run for CI and controller development.
- Fast GPU smoke run on hp01-hp03 that verifies CUDA, PyTorch, metrics, SeaweedFS, and Loki linkage.
- Canary metrics must include `reward_mean`, `reward_std`, `wall_clock_seconds`, and `reproducibility_hash`.

## 9. Execution model

### 9.1 Job generation

`BenchmarkRun` reconciliation creates jobs from the cartesian product of selected tasks and seeds unless the task declares grouped execution. Job names must be deterministic and below Kubernetes length limits:

`athena-brun-<run-hash>-<task-slug>-s<seed>`

Every job must:

- Mount SeaweedFS workspace at `RuntimeProfile.spec.storage.workspaceMountPath`.
- Write `artifact-manifest.json`, `metrics.json`, `stdout.log` or log stream, and raw evaluator outputs.
- Use immutable image digests when required.
- Set env vars:
  - `ATHENA_NAMESPACE`
  - `ATHENA_BENCHMARK_SUITE`
  - `ATHENA_BENCHMARK_RUN`
  - `ATHENA_TASK_NAME`
  - `ATHENA_SEED`
  - `ATHENA_WORKSPACE`
  - `ATHENA_TARGET_KIND`
  - `ATHENA_TARGET_NAME`
  - `ATHENA_REPRODUCIBILITY_HASH_INPUT`
- Use restart policy `Never`; retries are new jobs tracked in status.

### 9.2 Sequential and campaign workflows

Sequential workflow:

1. Human or agent creates `BenchmarkRun` targeting an `Experiment`, `ResearchCampaign`, model artifact, or runtime profile.
2. Controller runs selected tasks/seeds.
3. Controller writes aggregate status and report.
4. Console displays comparison against baseline.

Campaign workflow:

1. `ResearchCampaign.spec.benchmarkPolicy.runBaselineBeforeSearch=true` creates a baseline `BenchmarkRun` before agent exploration.
2. Each `Experiment` runs cheap smoke/canary suites.
3. Promotion candidates run full public suites.
4. Only best candidates run holdout suites when configured.
5. Campaign status updates aggregate metrics and budget usage.

### 9.3 Seed matrices

- Suites define default seeds; runs may narrow but not expand beyond policy unless allowed.
- Promotion requires the configured minimum seed count.
- Status must show per-seed results and aggregate mean/std.
- Failed seeds count toward failed run rate and must not be silently dropped.

### 9.4 Budget controls

Budget fields appear on suite tasks, benchmark runs, experiments, and campaigns. The effective budget is the minimum/most restrictive of all applicable budgets.

Controls:

- `maxWallClock`
- `maxGpuHours`
- `maxTokens`
- `maxRetries`
- `maxExperiments`
- `maxFailedRunRate`
- `maxConcurrency`

Controller behavior:

- Refuse new jobs once budget is exhausted.
- Mark budget exhaustion as `Failed` with `failureClass=budgetExceeded` when required tasks cannot complete.
- Mark optional skipped tasks as `Skipped` with reason `BudgetExhausted`.
- Record budget use in status even for failed runs.

### 9.5 hp01-hp03 GPU scheduling

Default GPU benchmark runtime profile:

```yaml
apiVersion: research.nixlab.io/v1alpha1
kind: RuntimeProfile
metadata:
  name: hp-8gb-gpu-benchmark
spec:
  runtime:
    type: pytorch
    mode: batchJob
  image: ghcr.io/nixlab/athena-benchmark-runner@sha256:<digest>
  resources:
    requests:
      cpu: "2"
      memory: 8Gi
      nvidia.com/gpu: "1"
    limits:
      cpu: "4"
      memory: 12Gi
      nvidia.com/gpu: "1"
  scheduling:
    nodeSelector:
      nixlab.io/pool: hpc
    allowedNodeNames: [hp01, hp02, hp03]
    gpuResourceName: nvidia.com/gpu
    gpuProduct: Quadro RTX 4000
  policy:
    requireImageDigest: true
    allowImageOverride: false
    allowCommandOverride: false
    allowSecretRefs: false
```

Implementation requirements:

- Add node affinity or node selector from `RuntimeProfile`, not from agent-controlled `Experiment` fields.
- Prefer one GPU per job; do not assume multi-GPU on 8GB cards.
- hp01-hp03 8GB GPUs are suitable for toy/canary, small local models, quantized runners, or API-target evals. Do not assume vLLM/large local LLM serving fits on these cards.
- Default precision should be safe for 8GB GPUs; benchmarks may use CPU-only profiles when GPU is unnecessary.
- If autoscaling is active, queue latency and pod start latency must be recorded.
- RuntimeProfile/profile-level concurrency limits must prevent GPU campaign starvation beyond default Kubernetes scheduling.

## 10. SeaweedFS artifact layout

Workspace root is mounted from the SeaweedFS PVC, default `/workspace`.

Required layout:

```text
/workspace/
  datasets/
    <dataset-name>/<revision-or-hash>/
  models/
    <model-name>/<revision-or-hash>/
  evals/
    lm-evaluation-harness/<version>/
    livecodebench/<version>/
  campaigns/
    <namespace>/<campaign-name>/
      baseline/
      experiments/<experiment-name>/
  benchmarks/
    suites/<suite-name>/<suite-hash>/suite.json
    runs/<namespace>/<benchmark-run-name>/
      run.json
      report.md
      report.json
      aggregate-metrics.json
      tasks/<task-name>/seed-<seed>/
        artifact-manifest.json
        metrics.json
        stdout.log
        raw/
        checkpoints/
        reports/
        patch.diff
        source-manifest.json
```

`artifact-manifest.json` required fields:

```json
{
  "apiVersion": "research.nixlab.io/v1alpha1",
  "kind": "AthenaArtifactManifest",
  "benchmarkSuite": "llm-core-v1",
  "benchmarkRun": "exp-042-llm-core",
  "task": "gsm8k",
  "seed": 1,
  "createdAt": "2026-05-20T00:00:00Z",
  "git": {"url": "...", "ref": "main", "commit": "...", "patchSha256": "..."},
  "images": {"runner": "ghcr.io/...@sha256:...", "evaluator": "ghcr.io/...@sha256:..."},
  "datasets": {"gsm8k": "sha256:..."},
  "runtimeProfile": {"name": "hp-8gb-gpu-benchmark", "generation": 2},
  "metricSources": ["lm-eval-json"],
  "reproducibilityHash": "sha256:..."
}
```

Artifact immutability and retention:

- Completed run directories are write-once after controller finalization. The controller may append final report/status metadata before writing a finalization marker; agents and evaluator jobs must not mutate completed artifacts.
- Reports reference content-addressed files and manifest checksums for every artifact they cite.
- Workspace preparation must be idempotent and concurrency-safe; task/seed/attempt writers never share mutable output paths.
- Default retention is indefinite for kept/promoted/baseline runs. Discarded experiment artifacts may be garbage-collected after a configured retention window, but suite snapshots and final reports needed for comparisons are retained.

## 11. Anti-cheating and research integrity

Athena must make benchmark cheating harder than honest improvement.

Controls:

- Holdout suites: private tasks can run with hidden data mounted only into evaluator containers. Agent-visible workspaces must not contain hidden prompts, labels, tests, or answer keys.
- Leakage prevention: suite definitions carry dataset revisions/hashes. Optional scanners compare generated code, prompts, and patches against known benchmark content. Policy may warn or block.
- Reward hacking controls: promotion requires held-out score when configured; public metric improvement alone is insufficient.
- Seed tracking: every task result records seed and deterministic flag. Missing seeds fail promotion gates.
- Immutable inputs: image digest, git commit, dataset hash, evaluator hash, and runtime profile generation are included in the reproducibility hash.
- Patch scope: `ExperimentTemplate.spec.integrity.allowedPatchPaths` and `deniedPatchPaths` prevent agents from editing evals, datasets, or benchmark definitions.
- No forged metrics: controller parses metrics from job artifacts and records parser provenance; agents cannot directly set benchmark aggregate status.
- Baseline comparison: campaign benchmarks must run a baseline under the same suite hash and budget before claiming improvement.
- Duplicate hypothesis detection: campaign controller stores normalized hypothesis hashes and optional embedding similarity scores. Duplicates count against `duplicate_hypothesis_rate`.
- Reproducibility replay: `BenchmarkRun.spec.mode=replay` reruns a prior run from manifest inputs and compares metrics within tolerance.
- Code/evaluator sandboxing: generated-code benchmarks run with no hostPath mounts, `automountServiceAccountToken: false`, non-privileged security context, seccomp/runtime default, bounded CPU/memory/pids/ephemeral storage, read-only root filesystem where practical, timeouts, and default-deny egress except explicitly declared package mirrors.
- Holdout redaction: agent-visible statuses expose only pass/fail and approved aggregates. Hidden prompts, labels, assertions, exact holdout item failures, and secret paths are visible only to admin/reporting paths with redaction controls.
- Patch integrity checks cover symlink escapes, path traversal, submodules, binary blobs, hidden files, and attempts to modify eval/data/benchmark harness paths when denied.
- Baseline/candidate improvement claims require identical suite hash, dataset hash, evaluator image digest, runtime profile generation, and budget unless the report explicitly marks the comparison as non-equivalent.

Integrity status must distinguish:

- `leakageScanFailed`
- `holdoutFailed`
- `immutableInputMissing`
- `patchPolicyViolation`
- `reproducibilityMismatch`
- `duplicateHypothesis`
- `metricParseError`

## 12. Athena Console requirements

Console architecture is a Rust/Iced local workbench built from the same `athena-api`
CRD types as the operator. It uses the user's kubeconfig through the Rust `kube`
client and must not maintain authoritative product state outside Kubernetes.

### 12.1 Rust/Iced workbench

Implement in `operator/crates/athena-console`.

Read surfaces:

- Experiments across namespaces, with phase filter, status message, workspace, logs, metrics, and Job/Pod references.
- ExperimentTemplates, with parameter schema, defaults, runtime profile, source, objective, and metric contract.
- ResearchCampaigns and campaign leaderboards.
- BenchmarkSuites and BenchmarkRuns, including aggregate metrics, gates, reproducibility hashes, artifacts, and report links.
- MetricSources and RuntimeProfiles.

Implementation requirements:

- Use typed CRD APIs from `athena-api` and the Kubernetes watch/list APIs.
- Include `resourceVersion`, namespace/name, labels, owner refs, and conditions in detail views.
- Never synthesize authoritative benchmark, campaign, experiment, or runtime status client-side.
- Do not expose secret refs, holdout answers, hidden dataset paths, or private benchmark answers.
- Writes must create/update Kubernetes specs only. Status remains controller-owned.
- The Nix app must provide reproducible packaged and autoreloading dev entrypoints.

### 12.2 Iced UI views

Required views:

- Overview dashboard: counts by phase, failed run rate, GPU-hours, active hp01-hp03 jobs, recent failures.
- Benchmark Suites: suite list, taxonomy, task count, readiness, suite hash, integrity policy.
- Benchmark Runs: run list with phase, target, suite, aggregate metrics, gates, cost, links.
- Run Detail: per-task/per-seed table, raw/normalized metrics, logs link, metrics link, artifact links, reproducibility hash.
- Campaign Leaderboard: best experiments, metric deltas vs baseline, time-to-best, duplicate hypothesis rate, failed run rate.
- Compare Report: side-by-side baseline vs candidate, pass/fail gates, confidence/seed std, budget deltas.
- Runtime Health: canary status for CPU, GPU, SeaweedFS, metrics, logs, and preemption.

Charts:

- Reward mean/std over time.
- KL and entropy over training steps when available.
- pass@k/accuracy bars by suite/task.
- Held-out vs public score scatter.
- GPU-hours vs objective improvement.
- Time-to-best per campaign.
- Failed run rate trend.
- Duplicate hypothesis rate trend.

UI conventions:

- Use Tailwind/global classes; do not add per-file scoped CSS for large styling changes.
- Preserve dark/Nord/pink visual direction already present.
- SSE updates should update tables/charts without manual refresh.

## 13. Observability requirements

Prometheus:

- Operator exposes `/metrics` and `/healthz`.
- Add benchmark metrics listed in section 7.
- ServiceMonitor/Helm/GitOps wiring is outside this repo for this step, but operator metrics must be ready.

Grafana:

- Status links should point at dashboard variables for namespace, suite, run, campaign, and task.
- Generated reports should include Grafana links where available.

Loki:

- Jobs need stable labels for experiment, benchmark run, suite, task, seed, and campaign.
- `BenchmarkRun.status.logsLink` and `Experiment.status.logsLink` must be populated when base Grafana URL is configured.

Kubernetes Events:

- Emit events for benchmark run start, task job creation, parse failure, budget exhaustion, integrity failure, and completion.

Failure classes:

- `userCode`
- `infrastructure`
- `budgetExceeded`
- `integrityViolation`
- `metricParseError`
- `preempted`
- `unknown`

## 14. Implementation roadmap

Each phase should end with tests and a commit. Do not touch `/home/olive/Repositories/nixlab` until a later GitOps phase explicitly requests it.

### Phase 1: API guardrails and benchmark CRD scaffolding

Files to touch:

- `operator/crates/athena-api/src/lib.rs`
- `operator/crates/athena-api/src/benchmark_suite.rs` (create)
- `operator/crates/athena-api/src/benchmark_run.rs` (create)
- `operator/crates/athena-api/src/metric_source.rs` (create)
- `operator/crates/athena-api/src/experiment.rs`
- `operator/crates/athena-api/src/experiment_template.rs`
- `operator/crates/athena-api/src/research_campaign.rs`
- `operator/crates/athena-api/src/runtime_profile.rs`
- `examples/benchmark-suite-toy-rl.yaml` (create)
- `examples/benchmark-run-canary.yaml` (create)
- `examples/metric-source-file-json.yaml` (create)

Tasks:

1. Add Rust structs/enums for `BenchmarkSuite`, `BenchmarkRun`, and `MetricSource`.
2. Add additive spec/status fields to existing CRDs.
3. Regenerate CRD YAML using the repo's existing CRD generation flow.
4. Add example manifests for toy RL canary and file JSON metric parsing.
5. Update Helm CRDs/RBAC for new resources and `/status` subresources.
6. Verify with `cargo test` for the operator workspace and any existing CRD generation checks.

### Phase 2: Minimal runnable BenchmarkRun vertical slice

Files to touch:

- `operator/crates/athena/src/main.rs`
- `operator/crates/athena/src/reconciler.rs` or new `benchmark_reconciler.rs`
- `operator/crates/athena/src/job.rs` if job generation is split out
- `operator/crates/athena/src/workspace.rs` if workspace prep is split out
- `operator/crates/athena/src/metrics.rs`
- `operator/crates/athena/src/error.rs` if present or create typed errors

Tasks:

1. Watch `BenchmarkRun` resources.
2. Resolve suite, runtime profile, metric sources, and target refs for `runtimeHealth`/`rlTraining` suites only.
3. Prepare idempotent SeaweedFS artifact directories and suite/run snapshots.
4. Generate a CPU toy canary Job first with required labels/env/mounts and sandbox defaults.
5. Track job phases, retry counts, cancellation/suspend, cleanup policy, and finalizer behavior.
6. Parse workspace `metrics.json` via phase-1 file JSON `MetricSource` definitions.
7. Aggregate bounded seed metrics and write status.
8. Compute and store reproducibility hash.
9. Emit Prometheus metrics and Kubernetes Events.
10. Add unit tests for job naming, budget enforcement, metric parsing, hash inputs, suspend/cancel, and status bounds.

### Phase 3: Benchmark integrations

Files/directories to create or modify:

- `runners/` or existing runner image build directory, if present.
- `examples/lm-eval-*.yaml`
- `examples/livecodebench-*.yaml`
- `examples/swebench-hook-*.yaml`
- `examples/toy-rl-*.yaml`
- Operator docs or README sections for runner contracts.

Tasks:

1. Define runner CLI contract for all integrations.
2. Implement or package toy RL runner first for deterministic CI.
3. Add `lm-evaluation-harness` runner contract and examples for GSM8K, MATH, HumanEval, MBPP, GPQA.
4. Add LiveCodeBench suite example with pinned revision fields.
5. Add SWE-bench-like hook example with artifact contract only.
6. Ensure every runner writes `artifact-manifest.json` and `metrics.json`.

### Phase 4: Campaign benchmark policy

Files to touch:

- `operator/crates/athena/src/reconciler.rs`
- `operator/crates/athena/src/campaign_reconciler.rs` if split out
- `operator/crates/athena-api/src/research_campaign.rs`
- `operator/crates/athena-api/src/experiment.rs`

Tasks:

1. Create baseline `BenchmarkRun` when `runBaselineBeforeSearch=true`.
2. Create cheap benchmark runs for every experiment when configured.
3. Create promotion benchmark runs for candidate `Keep` experiments.
4. Gate holdout runs to best candidates only.
5. Compute campaign aggregate metrics: time-to-best, duplicate hypothesis rate, failed run rate, GPU-hours.
6. Write promotion/rejection status with integrity reasons.

### Phase 5: Rust/Iced Console UI

Files to touch:

- `operator/crates/athena-console/src/main.rs`
- `operator/crates/athena-console/src/` helper modules as the workbench grows
- `operator/crates/athena-api/src/*` when console-visible API contracts need additive changes
- `flake.nix` for native and dev app wiring

Tasks:

1. Add typed workbench views for benchmark suites, runs, metric sources, campaigns, and runtime profiles.
2. Use Kubernetes list/watch APIs through `kube` and shared `athena-api` types.
3. Add live watch/subscription updates without browser-side tokens or local status synthesis.
4. Add comparison and leaderboard views.
5. Add Overview, Suite list, Run list, Run detail, Campaign leaderboard, Compare Report, and Runtime Health views.
6. Add charts for reward, KL, entropy, pass@k, held-out score, GPU-hours, time-to-best, failed run rate, and duplicate hypothesis rate.
7. Verify UI updates from SSE without refresh.

### Phase 6: Observability and reports

Files to touch:

- `operator/crates/athena/src/metrics.rs`
- `operator/crates/athena/src/report.rs` (create)
- `operator/crates/athena/src/links.rs` (create)
- `examples/grafana-dashboard-athena-benchmarks.json` (create if this repo owns examples)

Tasks:

1. Add all benchmark Prometheus metrics from section 7.
2. Generate `report.md` and `report.json` for every completed `BenchmarkRun`.
3. Populate logs and metrics links in status.
4. Add dashboard JSON example for Athena benchmark runs.
5. Add tests for report generation and link redaction.

### Phase 7: Integrity hardening

Files to touch:

- `operator/crates/athena/src/integrity.rs` (create)
- `operator/crates/athena/src/reproducibility.rs` (create)
- `operator/crates/athena-api/src/*`
- `examples/private-holdout-placeholder.yaml` (create redacted example)

Tasks:

1. Enforce image digest, git commit, dataset hash, evaluator hash, and runtime generation requirements.
2. Enforce patch allow/deny paths from `ExperimentTemplate`.
3. Add holdout redaction rules for status, reports, and console DTOs.
4. Add duplicate hypothesis hashing and status fields.
5. Add replay mode and reproducibility mismatch detection.

## 15. Verification commands for implementation agents

Use the exact commands available in the repo; if a command does not exist, add or document the correct repo-native equivalent in the implementation PR.

Expected baseline checks:

```bash
cd /home/olive/Repositories/athena-operator
cargo test --workspace
cargo fmt --check
```

If the operator has a CRD generation command, run it and fail the task if generated CRDs are stale.

For console changes:

```bash
cd /home/olive/Repositories/athena-operator/operator
cargo check -p athena-console
nix build .#athena-console
```

For docs-only changes to this file, verify:

```bash
git diff --check
```

## 16. Done definition

The benchmark architecture is implemented when:

- `kubectl get benchmarksuites,benchmarkruns,metricsources -A` works against generated CRDs.
- A toy RL `BenchmarkRun` succeeds and writes artifacts to SeaweedFS.
- A GPU canary run schedules on one of hp01, hp02, hp03 and records GPU/cost metrics.
- A sample lm-evaluation-harness suite produces normalized pass@k/exact-match metrics.
- A `ResearchCampaign` can run baseline, experiment, promotion, and holdout benchmark policies.
- Athena Console shows live SSE updates and comparison reports.
- Prometheus exposes benchmark metrics, Loki links resolve, and Grafana links are populated.
- Integrity checks block mutable images, missing git commits, missing dataset hashes, and patch attempts against denied eval/data paths.
