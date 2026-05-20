# Review of Athena Benchmark Architecture (openspec.md)

## Executive Assessment

The proposed Athena benchmark architecture is a robust and comprehensive design that correctly identifies the critical requirements for autonomous ML/RL research on Kubernetes. The separation of concerns between `BenchmarkSuite`, `BenchmarkRun`, and `MetricSource` is excellent and provides the necessary flexibility for a diverse set of research tasks.

Key strengths:
- **Comprehensive Taxonomy**: Coverage of RL training, LLM capabilities, research loops, and runtime health.
- **Strong Integrity Controls**: The `reproducibility_hash`, holdout policies, and immutable input requirements are industry-standard and critical for trustworthy research.
- **Nixlab Alignment**: The spec explicitly targets `hp01-hp03` GPUs and SeaweedFS, ensuring feasibility within the existing infrastructure.
- **Structured Metrics**: A clear metric model that distinguishes between raw and normalized metrics.

## Blocking Issues

1.  **Shared Workspace Concurrency**: The "one Job per task/seed" model creates a risk of race conditions if multiple jobs attempt to write to or prepare the same shared SeaweedFS workspace concurrently. While the artifact layout is partitioned by `task/seed`, the *preparation* phase (mounting datasets, patching source) must be idempotent and concurrency-safe.
2.  **Holdout Information Leakage**: If an agent can read its own `Experiment` status, any holdout metrics written there are leaked. The spec must strictly enforce that `status.integrity.holdoutPassed` is the only information visible to the research agent, while the console/admin can see the full details.
3.  **Resource Exhaustion on hp01-hp03**: With only 3 GPU nodes and potentially many seeds/tasks, the operator must have a clear queuing and prioritization strategy beyond standard Kubernetes scheduling to prevent campaign starvation.

## Recommended Spec Changes

1.  **Grouped Execution by Default**: For small benchmark tasks (e.g., toy RL), running multiple seeds in a single Job should be the recommended pattern to reduce Kubernetes API overhead and pod startup latency. (Patched in `openspec.md`)
2.  **Job Cleanup Policy**: Added a `cleanupPolicy` to `BenchmarkRun` to manage the lifecycle of completed Jobs and prevent CRD/Job bloat in the cluster. (Patched in `openspec.md`)
3.  **Metric Source Sandboxing**: The `MetricSource` parsing logic should be explicitly limited to safe JSONPath/Regex operations to prevent any potential code injection into the operator context.
4.  **Artifact Retention**: Define a default retention policy for SeaweedFS artifacts (e.g., "delete artifacts for Discarded experiments after 30 days").

## Implementation Roadmap & Milestones

1.  **Milestone 1 (API & Toy RL)**: Implement `BenchmarkSuite` and `BenchmarkRun` CRDs. Successfully run a deterministic toy RL benchmark on a CPU node.
2.  **Milestone 2 (GPU Canary)**: Successfully schedule and run a GPU benchmark on `hp01-hp03`, verifying SeaweedFS mounting and Prometheus metric export.
3.  **Milestone 3 (Metric Normalization)**: Implement the `MetricSource` controller and demonstrate normalization from raw JSON results to CRD status.
4.  **Milestone 4 (Campaign Integration)**: Connect `ResearchCampaign` to `BenchmarkRun`, implementing the baseline-first and promotion-gate logic.

## Anti-Cheating & Integrity Improvements

- **Reproducibility Replay**: The `replay` mode is a vital feature. It should be mandatory for any experiment marked for `Keep` before final promotion.
- **Negative Sample Scanning**: In addition to leakage prevention, include "negative samples" in benchmarks (e.g., questions with slightly altered constraints) to detect agents that have memorized answers rather than learned reasoning.
- **Compute Budget Hard-Limits**: Ensure `maxGpuHours` is enforced at the controller level by terminating jobs that exceed their allocation.

## Conclusion

The spec is high-quality and ready for implementation. The identified risks are manageable through careful controller design and adherence to the partitioned artifact layout.
