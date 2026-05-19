# OpenSpec: Athena (Kubernetes Research Operator Template)

## 1. Motivation
The `n-autoresearch` project originally relied on the `iii` engine (a Python orchestrator and Rust GPU workers) to manage experiments, adaptive search strategies, and GPU resource allocation.

This specification outlines **Athena**: a generic Kubernetes Operator template for autonomous ML research. By mapping research primitives directly to Kubernetes Custom Resources, Athena leverages Kubernetes natively for scheduling, batch job execution, state management, and node autoscaling.

While designed for `n-autoresearch`, Athena is built as a reusable template supporting pluggable backend runtimes (PyTorch, MLX, Ollama, vLLM, SkyPilot) and highly parameterized execution profiles.

## 2. Core Architecture & Resource Model

Athena enforces strict separation of concerns between **Agent Intent** (what the AI wants to test), **Project Defaults** (how to measure success), and **Infrastructure** (how to execute it safely).

It introduces four Custom Resource Definitions (CRDs):

### A. `RuntimeProfile` (Infrastructure)
Owned by cluster administrators. Describes *how* to run a specific backend securely on specific hardware.
- **Execution Mode**: `batchJob` (training/eval), `service` (inference endpoints), or `externalJob` (SkyPilot).
- **Runtime Type**: `pytorch`, `mlx`, `ollama`, `vllm`, `custom`.
- **Scheduling**: Node selectors (e.g., `nixlab.io/pool: hpc`), tolerations.
- **Resources**: GPU requests (e.g., `nvidia.com/gpu: 1`), CPU, memory limits.
- **Storage**: Standard workspace mounts backed by **SeaweedFS**.
- **Security Policy**: Allow/deny lists for agent overrides (e.g., `allowCommandOverride: false`).

### B. `ExperimentTemplate` (Project Defaults)
Owned by ML engineers. Describes the parameter space, metrics, and source code baseline for a specific research project.
- **Source**: Git repository URL, target branch/commit.
- **Objective**: Primary metric to track (e.g., `val_bpb`) and goal (`minimize` or `maximize`).
- **Metric Parser**: How to extract metrics from the run (e.g., parsing a `metrics.json` file on the SeaweedFS workspace, or regex over `stdout`).
- **Parameter Schema**: Defined parameters and their safe defaults (e.g., `depth`, `batchSize`, `precision`).

### C. `ResearchCampaign` (Strategy & Concurrency)
Owned by the AI Agent or human researcher. Manages a cohort of experiments.
- **Template Reference**: Points to an `ExperimentTemplate`.
- **Budget**: Max concurrent experiments, max total runs, max wall-clock duration.
- **Strategy**: Pluggable search strategies (explore, exploit, ablation, combine) governing how the next parameters are chosen.

### D. `Experiment` (Agent Intent)
Owned by the AI Agent. Represents a single concrete trial.
- **Campaign Reference**: Points to the parent `ResearchCampaign`.
- **Hypothesis**: Natural language description of what the agent believes will happen.
- **Parameter Overrides**: Specific values chosen for this run (e.g., `depth: 4`, `learningRate: 1e-3`).
- **Code Patch**: A Git patch (or strategic merge patch) applied to the base source code before execution.
- **Status**: Updated by the operator with `phase` (Pending, Running, Succeeded, Failed), extracted `metrics`, execution `duration`, and links to the artifact workspace on SeaweedFS.
- **Decision**: Set by the agent post-run (`Keep`, `Discard`, `NeedsReview`).

## 3. Storage Architecture: SeaweedFS Workspace

To prevent redundant git clones, dataset downloads, and model weight transfers on every experiment, Athena requires a shared filesystem (SeaweedFS is the default for nixlab).

Every `RuntimeProfile` mounts a standard layout:
```text
/workspace (SeaweedFS PVC)
  \u251c\u2500\u2500 datasets/       # Read-only cache of training data
  \u251c\u2500\u2500 models/         # Read-only cache of base models / tokenizers
  \u2514\u2500\u2500 runs/
      \u2514\u2500\u2500 <campaign-name>/
          \u2514\u2500\u2500 <experiment-id>/
              \u251c\u2500\u2500 source/        # Checked-out code + applied agent patch
              \u251c\u2500\u2500 patch.diff     # The raw patch applied
              \u251c\u2500\u2500 stdout.log     # Execution logs
              \u251c\u2500\u2500 metrics.json   # Structured metrics written by the job
              \u2514\u2500\u2500 checkpoints/   # Saved weights if experiment succeeded
```

## 4. Execution Lifecycle & Agent Interface

Instead of WebSocket/REST APIs, the AI Agent interacts entirely via the Kubernetes API.

1. **Setup**: The admin creates the `RuntimeProfile` (e.g., `hp-8gb-pytorch`). The engineer creates the `ExperimentTemplate` (e.g., `nanochat-autoresearch`).
2. **Campaign**: The agent creates a `ResearchCampaign` with a concurrency budget.
3. **Dispatch**: The agent analyzes past results, generates a patch and parameters, and applies an `Experiment` CRD.
4. **Execution**: The Athena controller sees the `Experiment`. It provisions the workspace on SeaweedFS, applies the patch, and spawns a K8s `Job` mapping the `RuntimeProfile` constraints. The K8s cluster autoscaler spins up an `hp01-hp03` node to satisfy the GPU request.
5. **Collection**: The Job writes `metrics.json` to the workspace. Athena parses it, cleans up the Job, and updates `Experiment.status.metrics`.
6. **Evaluation**: The agent watches the `Experiment` CRD. Once `phase=Succeeded`, it evaluates the `val_bpb` metric and patches `Experiment.status.decision` to `Keep` or `Discard`.
## 5. Observability (Metrics, Logs, Dashboards)

Given the target environment (KubeRay, SkyPilot, Spot GPUs), Athena requires integrated observability:

1. **Metrics Scraping (Prometheus)**:
   - The Athena operator exposes a `/metrics` endpoint (`metricsPort: 8080`) providing gauges for queued, running, succeeded, and failed experiments per campaign.
   - A `ServiceMonitor` (kube-prometheus-stack) is bundled in the Helm chart to scrape these metrics automatically.

2. **Log Aggregation (Loki)**:
   - Experiment `Job` logs are ephemeral, especially on Spot instances (`hp01-hp03`) that may be pre-empted.
   - Athena leverages Promtail/Loki. `Experiment.status.logsLink` is populated with a direct Grafana Explore URL querying Loki for the specific `job_name`.

3. **Visualization (Grafana)**:
   - The Helm chart provisions a `GrafanaDashboard` CR (using `grafana.integreatly.org/v1beta1` like the `ray-cluster` modules).
   - The dashboard visualizes `val_bpb` (or the generic primary metric) over time, overlaid with experiment keeping/discarding decisions.

4. **SkyPilot/Ray Alignment**:
   - For `externalJob` runtime modes (SkyPilot), Athena injects tags into the SkyPilot YAML so Sky jobs are traceable back to the `Experiment` CRD.
   - For `service` runtime modes (vLLM on Ray), Athena exposes `metricsLink` pointing to the Ray Serve or vLLM metrics dashboard defined in `hpc/ray-cluster.nix`.

## 6. Sensible Defaults (nixlab hp01-hp03)

The default template profile targets the 8GB HP ProLiant GPU workers (`hp01-hp03`):

- **Resources**: CPU 2, RAM 8Gi, GPU 1.
- **Parameters**: `depth: 4`, `deviceBatchSize: 2`, `torchCompile: false` (to prevent OOMs on 8GB cards).
- **Precision**: `bfloat16`.
- **Node Selector**: `nixlab.io/pool: hpc`.

## 7. Template Implementation Plan

The Athena codebase will be generated as a reusable Rust/kube-rs operator template:
1. Strip business logic from the `hephaestus` boilerplate (BMC, IPMI, MetalMachine).
2. Scaffold the `RuntimeProfile`, `ExperimentTemplate`, `ResearchCampaign`, and `Experiment` CRDs.
3. Implement the SeaweedFS workspace provisioner and the `batchJob` K8s Job generator.
4. Package as a generic Helm chart with template variables for CRD groups (`research.nixlab.io`) and image repositories.
5. Add the concrete `n-autoresearch` deployment configuration to the `nixlab` GitOps repository.