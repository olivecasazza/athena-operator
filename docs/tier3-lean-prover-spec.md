# Tier 3 — Formal theorem proving as auto-RL on Athena

Status: **design spec** (not implemented). Scope owner: research campaigns.
Last updated: 2026-07-02.

## Purpose

Tier 3 of the campaign ladder (validate → open-empirical → **frontier**) reframes
"study/prove math" as what it actually is in the state of the art: a
**reinforcement-learning search problem where a formal proof-assistant kernel
supplies the reward**. This document specs how that maps onto Athena's existing
CRD model, what is reused vs. genuinely new, and — importantly — the honest
capability ceiling.

> **Honest scope, up front.** This reaches *competition / undergraduate formal
> benchmarks* (miniF2F, PutnamBench, formalized IMO), **not** open research
> conjectures and **not** Millennium problems (Navier–Stokes global regularity is
> out of scope — see §6). The realistic Athena deliverable is to *orchestrate an
> existing open-weights prover + Lean verifier as an auto-RL campaign over a
> formal benchmark* and self-document the run — not to train a frontier prover
> from scratch.

## 1. The RL formulation (what the field actually does)

Modern automated theorem proving is explicitly RL: a policy/value model proposes
proof steps, a search algorithm explores, and a **formal kernel certifies**. The
reward is a **binary deductive check** (proof closes = 1, else 0) — not a numeric
or empirical objective. That verifier-as-reward is the load-bearing soundness
property: RL *searches*, the kernel *proves*, so you cannot reward-hack a false
"proof" the way you can a numeric metric [AlphaProof; DeepSeek-Prover; HTPS —
§References].

| RL element | In formal proving |
|---|---|
| State | Lean proof/goal state (hypotheses + remaining goals) |
| Action | A Lean tactic emitted as text |
| Policy + value | One model proposing tactics + estimating provability |
| Search | MCTS / HyperTree Proof Search / best-first over the proof tree |
| **Reward** | **Lean kernel verification: proof closes → 1, else 0** |
| Learning signal | Expert iteration — verified proofs fed back to fine-tune |

## 2. Mapping onto Athena CRDs

The mapping is clean because Athena already models "run a Job with a declared
objective, parse an artifact, loop with a strategy." The verifier just replaces
the numeric objective, and the loop strategy changes from hyperparameter
perturbation to expert iteration.

| Athena object | Tier-3 role |
|---|---|
| `RuntimeProfile` | Lean 4 + mathlib + prover-inference image (the *environment*) |
| `Experiment` | One proof attempt on one formal statement (or a batch/curriculum slice) |
| `Experiment.spec.hypothesis` | The formal statement being attempted (the literal conjecture) |
| `MetricSource` | Reads the **kernel result** (proof_closed 1/0) — the reward contract |
| `ExperimentTemplate.spec.objective` | `metric: proof_closed`, `goal: Maximize` |
| `BenchmarkSuite` / `BenchmarkRun` | Pass@k over a formal benchmark (miniF2F/PutnamBench) |
| `ResearchCampaign` | The **expert-iteration loop** (prove → collect verified → fine-tune → harder curriculum) |
| Journal / provenance / dossier | Self-documentation of the run (already built) |

### 2.1 RuntimeProfile (the environment) — illustrative

```yaml
apiVersion: research.nixlab.io/v1alpha1
kind: RuntimeProfile
metadata: { name: lean-prover-gpu, namespace: apps }
spec:
  runtime: { type: vllm, mode: batchJob }   # serves the prover model; Lean runs in-container
  image: "ghcr.io/olivecasazza/lean4-prover:<digest>"  # Lean4 + mathlib + verifier + prover weights
  command: ["python", "prove.py"]
  resources: { requests: { cpu: "4", memory: "24Gi", nvidia.com/gpu: "1" }, limits: { memory: "48Gi" } }
  storage:
    workspaceClaimName: athena-workspace
    workspaceMountPath: /workspace
    createWorkspaceClaim: true
    workspaceSize: 50Gi
  metricsEndpoint: { enabled: true, port: 9108, path: /metrics }
```

Note: `runtime.type` has no `lean` variant today (`pytorch|mlx|ollama|vllm|…`) —
`vllm`/custom-command works; a dedicated `type: prover` is optional polish.

### 2.2 MetricSource (verifier-as-reward) — the key new contract

The workload writes a JSON artifact the operator parses; the **binary field is
kernel-certified**, not self-reported score:

```yaml
apiVersion: research.nixlab.io/v1alpha1
kind: MetricSource
metadata: { name: lean-kernel-result, namespace: apps }
spec:
  sourceType: file
  path: result.json
  format: json
  metrics:
    - { name: proof_closed,   path: "$.proof_closed",   type: boolean }  # Lean kernel verdict
    - { name: statement_hash, path: "$.statement_hash",  type: string }
    - { name: tactic_steps,   path: "$.tactic_steps",    type: integer }
    - { name: wall_seconds,   path: "$.wall_seconds",    type: number }
  failureRules:
    - { metric: proof_closed, equals: false }   # a non-closing attempt is not a success
```

`result.json` is emitted **only after** the Lean kernel independently verifies the
candidate proof (a `SafeVerify`-style guard against axiom cheating, per AlphaProof).
The provenance manifest records the mathlib commit + Lean toolchain version +
declared axioms — that is the reproducibility contract for a claimed proof.

### 2.3 ExperimentTemplate — objective = proof closure

```yaml
apiVersion: research.nixlab.io/v1alpha1
kind: ExperimentTemplate
metadata: { name: minif2f-prover, namespace: apps }
spec:
  runtimeProfileRef: lean-prover-gpu
  source: { git: { url: "https://github.com/openai/miniF2F", ref: "main" } }
  objective: { metric: proof_closed, goal: maximize }
  metrics: { parser: { type: file, path: result.json } }
  parameterSchema:
    target_statement: { type: string,  description: "miniF2F/PutnamBench statement id" }
    sample_budget:    { type: integer, default: 64,  description: "proof attempts per statement (pass@k)" }
    temperature:      { type: number,  default: 1.0, description: "prover sampling temperature" }
    search:           { type: string,  default: "rmaxts", description: "best-first | mcts | rmaxts" }
  researchObjective: >-
    Reproduce the RL-prover capability frontier on formal benchmarks: what
    fraction of miniF2F/PutnamBench can a verifier-in-the-loop search close,
    and how does pass-rate scale with sample budget and search strategy.
```

### 2.4 ResearchCampaign — the expert-iteration loop

```yaml
apiVersion: research.nixlab.io/v1alpha1
kind: ResearchCampaign
metadata: { name: minif2f-expert-iteration, namespace: apps }
spec:
  templateRef: minif2f-prover
  concurrency: 4
  budget: { maxExperiments: 500, maxDuration: 72h }
  strategy: { type: expertIteration }   # NEW — see §3
  benchmarkSuiteRef: minif2f-v1         # pass@k over the whole set
  benchmarkRuntimeProfileRef: lean-prover-gpu
```

## 3. What is reused vs. genuinely new

**Reused (already in Athena):** Job orchestration + ownerReferences; the shared
workspace PVC; the self-documentation loop (research_journal.jsonl, provenance.json,
`athena dossier` / `ResearchReport`); `MetricSource` JSON parsing; `BenchmarkSuite`/
`BenchmarkRun` for pass@k over a set; status/conditions/metrics plumbing.

**Genuinely new (must be built):**
1. **The Lean-prover image** — Lean 4 + mathlib + a verifier harness + a prover
   (start with an open-weights model, e.g. DeepSeek-Prover-V2, served via vLLM;
   do **not** train one from scratch). Heavy image; mathlib build is non-trivial.
2. **The verifier-reward contract** — the `MetricSource` boolean above is only
   sound if `result.json` is written strictly downstream of an independent kernel
   check. This is a new artifact contract + a `SafeVerify`-style guard.
3. **`strategy.type: expertIteration`** — a new `campaign_reconciler` strategy.
   Unlike `heuristic`/`pbt` (which perturb *numeric hyperparameters*), expert
   iteration must: collect verified proof trajectories from a round's workspace
   artifacts → launch a **fine-tuning** Experiment on them → advance a **curriculum**
   (attempt harder statements next round). This is a train→prove→train cycle the
   current reconciler does not model. **This is the bulk of the new controller work.**
4. **Autoformalization** — only needed beyond pre-formalized benchmarks; a
   separate workload. For miniF2F/PutnamBench the statements are already in Lean,
   so v1 skips this. (For open problems it is a hard blocker — §6.)

## 4. Suggested build phases

1. **Prove-once slice:** Lean-prover image + `MetricSource` boolean contract +
   an `ExperimentTemplate`; prove a handful of miniF2F statements, verify the
   kernel result surfaces as `status.metrics.proof_closed` and lands in the
   dossier. No campaign loop yet. *(Proves the environment + reward are sound.)*
2. **Benchmark pass@k:** a `BenchmarkSuite` over miniF2F; a `BenchmarkRun`
   reporting pass-rate + mean±CI. *(Reproduces a published number — the honest
   validation that this works at all.)*
3. **Expert iteration:** the new `strategy.type: expertIteration` reconciler
   (prove → collect verified → fine-tune → curriculum), watched for the same
   settle/idempotency discipline as `report_reconciler`.

## 5. Where this sits in the overall plan

Ladder: **Tier 1 (validate a known GPT result)** → **Tier 2 (open empirical
search, human-framed)** → **Tier 3 (this — formal proving as auto-RL on
benchmarks)**. Tier 3 is the largest new workload and should come after Tiers 1–2
have exercised the deployed self-documentation loop. It depends on the
self-documentation + `ResearchReport` work being deployed first.

## 6. Honest limits (do not overclaim)

- **Benchmarks, not open problems.** Demonstrated SOTA is olympiad/undergraduate
  math *with known solutions on a fixed concept library*: AlphaProof at IMO-2024
  silver (28/42); Seed-Prover 5/6 IMO-2025 and ~99.6% miniF2F; DeepSeek-Prover-V2
  88.9% miniF2F but only ~50%/below on PutnamBench. **No RL/ML system has proved a
  previously-open research conjecture** [§References].
- **Navier–Stokes stays out of scope.** Not because "RL can't prove" (it can), but
  because: (a) open problems have **no ground-truth reward** to do RL against;
  (b) reaching research math needs autonomous **"theory building" — expanding the
  concept library** — which AlphaProof's authors call *monumental and unachieved*;
  (c) NS would first have to be **faithfully autoformalized in Lean**, itself
  unsolved. A Tier-3 campaign attacks formal benchmarks; it does not attempt NS.
- **Don't train a frontier prover.** Athena's realistic contribution is
  *orchestration + measurement + self-documentation* of an existing open prover in
  a verifier-in-the-loop RL campaign — not beating DeepSeek/DeepMind from scratch.
- **Conjecture generation is unproven.** Deciding *what* to prove (vs. proving a
  given statement) as RL has no demonstrated open-problem result — treat as
  research, not a capability.

## References

Verified against primary sources (Nature, arXiv, DeepMind, OpenReview) via a
3-vote adversarial deep-research pass, 2026-07-02 (25/25 claims confirmed).

- **AlphaProof** — "AI achieves silver-medal standard solving IMO problems,"
  Nature (2025), DOI `10.1038/s41586-025-09833-y`; DeepMind blog:
  `https://deepmind.google/blog/ai-solves-imo-problems-at-silver-medal-level/`.
  (AlphaZero + Lean; state/action/reward; kernel-guaranteed soundness; IMO-2024 silver; theory-building gap.)
- **HyperTree Proof Search (HTPS)** — Lample et al., Meta, `arXiv:2205.11491`.
  (AlphaZero-style policy+value over Lean/Metamath; online expert iteration.)
- **DeepSeek-Prover-V1.5** — `arXiv:2408.08152` (OpenReview `id=I4YAIwrsXa`).
  (RLPAF + GRPO + RMaxTS; binary Lean-verified reward; 63.5% miniF2F.)
- **DeepSeek-Prover-V2** — `arXiv:2504.21801`.
  (GRPO over binary Lean rewards; 88.9% miniF2F-test; 49/658 PutnamBench. Open weights — candidate prover to orchestrate.)
- **Seed-Prover** — `arXiv:2507.23726`.
  (RL reward = 1 iff proven; 5/6 IMO-2025; ~99.6% miniF2F; 331/657 PutnamBench.)
- **Process-Verified RL for Theorem Proving via Lean** — `arXiv:2606.20068`
  (NeurIPS 2025). (Lean elaboration as a *process-level* reward oracle — dense
  tactic-level credit. Recent; single-source — verify before relying.)

_Reward caveat:_ several systems add minor shaping terms (per-tactic penalty,
formatting/consistency rewards); the *correctness* signal is always the kernel
check. Seed-Prover's "5/6 IMO-2025" used extended search (4/6 + partial under
competition timing). All numbers are date-bounded in a fast-moving field.
