# Athena / n-autoresearch Final Integration Report

## Branch and commits

Integration worktree: `/home/olive/Repositories/n-autoresearch/.athena-workflow/worktrees/integrate-build`

Branch: `athena/integrate-build`

Local commits created:

1. `87c41d6 docs(openspec): integrate benchmark architecture review`
2. `b393a2c feat(operator): add benchmark API CRDs`
3. `5af8db6 feat(console): scaffold benchmark resource views`

No push was performed.

## Review feedback merged into OpenSpec

Inspected:

- OpenSpec source: `.athena-workflow/worktrees/openspec/openspec.md`
- GPT-5.5 review: `.athena-workflow/worktrees/review-gpt55/REVIEW.md`
- Gemini review: `.athena-workflow/worktrees/review-gemini/REVIEW.md`
- Claude worktree: no `REVIEW.md` was present; inspected `.athena-workflow/worktrees/review-claude/openspec.md`, `README.md`, and `program.md`.

Merged the strongest review feedback into `openspec.md`:

- Tightened phase-1 scope to a small vertical slice before broad LLM/campaign implementation.
- Replaced “immutable-ish” suite language with `suiteVersion` plus `resolvedSuiteHash` comparability rules.
- Added CRD/API guardrails: additive v1alpha1 changes, structural schemas, bounded status, conditions, status ownership, and future v1beta1 planning.
- Removed undefined `ModelArtifact` from phase-1 target examples.
- Added suite snapshotting before task Job creation.
- Added BenchmarkRun suspend, max parallel tasks, retry attempts, ownerReferences, finalizer, cleanup policy, and cancellation semantics.
- Limited phase-1 MetricSource support to safe file JSON/JSONL parsing.
- Added denominator/confidence-interval metric guidance.
- Added hp01-hp03 8GB GPU fit constraints and concurrency/starvation guidance.
- Added SeaweedFS artifact immutability, finalization, checksum, and retention requirements.
- Added evaluator/code sandboxing, holdout redaction, patch integrity, and same-suite-hash comparison rules.
- Added console namespace scoping, pagination, redaction, and SSE reconnect/backpressure requirements.
- Updated the roadmap so phase 1 includes CRD/Helm/RBAC scaffolding and phase 2 is the minimal runnable BenchmarkRun vertical slice.

## Implementation slice built

### Operator API / CRDs

Added new Rust CRD API modules:

- `operator/crates/athena-api/src/common.rs`
- `operator/crates/athena-api/src/benchmark_suite.rs`
- `operator/crates/athena-api/src/benchmark_run.rs`
- `operator/crates/athena-api/src/metric_source.rs`

New CRDs:

- `BenchmarkSuite` (`benchmarksuites`, shortname `bsuite`)
- `BenchmarkRun` (`benchmarkruns`, shortname `brun`)
- `MetricSource` (`metricsources`, shortname `msrc`)

Included reusable structs/enums for:

- object references
- conditions
- budgets
- metric goals/aggregates
- artifact URIs
- failure classes
- benchmark taxonomy/integration/mode/phase
- cleanup policy and bounded task result summaries

Updated:

- `operator/crates/athena-api/src/lib.rs` to export the new API modules.
- `operator/crates/athena/src/crd.rs` to include new CRDs in `athena export-crds`.
- `charts/athena/crds/athena-crds.yaml` regenerated from the operator.
- `charts/athena/templates/rbac.yaml` to grant operator access to new resources and `/status` subresources.

### Examples

Added example manifests:

- `examples/metric-source-file-json.yaml`
- `examples/benchmark-suite-toy-rl.yaml`
- `examples/benchmark-run-canary.yaml`

These model the phase-1 safe path: file JSON metrics plus a grouped CPU toy RL canary run.

### Console scaffold

Updated the Go BFF:

- Added generic list handlers for research resources.
- Added endpoints for:
  - `/api/v1/experiments`
  - `/api/v1/benchmark-suites`
  - `/api/v1/benchmark-runs`
  - `/api/v1/metric-sources`
  - `/api/v1/runtime-profiles`
  - `/api/v1/campaigns`
  - compatibility aliases under `/api/...`
- Added basic `limit` and `namespace` query support.

Updated Vue UI:

- Preserved existing console behavior and dark/pink visual direction.
- Added benchmark run table with phase, suite, target, GPU hours, and report URI.
- Added benchmark suite list.
- Kept experiment list visible.
- No scoped CSS was added.

## Validation run

Passed:

- `cd operator && cargo test --workspace`
- `cd operator && cargo fmt --check`
- `cd athena-console/api && go test ./...`
- `cd athena-console/web && npm install && npm run build`
- `helm lint charts/athena`
- `helm template athena charts/athena`
- `git diff --check`
- `nix flake check`

Notes:

- The first web build failed because `vue-tsc` was not installed in `node_modules`; fixed by running `npm install`, then `npm run build` passed.
- `package-lock.json` is ignored by the repo and was not committed.

## Next steps

Recommended next shippable slice:

1. Implement the `BenchmarkRun` controller watch loop alongside the existing `Experiment` controller.
2. Add deterministic 63-character-safe benchmark Job naming tests.
3. Implement status initialization for Pending BenchmarkRuns, including observedGeneration and conditions.
4. Add finalizer/suspend/cancel behavior before creating real Jobs.
5. Create the first CPU toy canary Job from `examples/benchmark-run-canary.yaml`.
6. Parse file JSON metrics into bounded `BenchmarkRun.status.taskResults` and `aggregateMetrics`.
7. Add Prometheus metrics for benchmark run/task counts and parse errors.
8. Add console detail endpoints after the list DTOs settle.
