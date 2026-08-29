# AGENTS.md — Athena research memory

The console renders Athena's research record. That record is also **agent memory**:
a steering agent reads it to learn what was already tried, what failed, and which
footguns are known. These rules govern how any agent produces that record.

## The record is the CRDs, nothing else

| Object | Owns |
|---|---|
| `Experiment` | one run: `spec.hypothesis`, `spec.parameters`, `spec.lineage`, controller-written `status.metrics` |
| `ResearchCampaign` | the search: `templateRef`, budget, canary gate, `status.bestObjective`/`bestExperiment` |
| `ResearchReport` | the analysis: `sections`, `seededHypotheses`, curated experiment set |
| `ResearchDrive` | the perpetual loop: proposals, decisions, curriculum stage, stagnation |

A finding that is not in one of these does not exist. Chat messages, git commit
bodies, `kubectl annotate`, local `*.md` notes, and script stdout are **not** the
record — none of them are queryable by the console or by the next agent.

## Read before you run

Prior art first. An investigation that repeats a recorded failure is waste.

```
kubectl get researchcampaign -n apps -l research.nixlab.io/curriculum-robot=spot
kubectl get researchreport -n apps -o custom-columns=NAME:.metadata.name,CAMPAIGN:.spec.campaignRef
kubectl get researchreport <name> -n apps -o jsonpath='{.spec.sections}'
kubectl get experiments -n apps -o json | jq '.items[] | {n:.metadata.name, h:.spec.hypothesis, m:.status.metrics}'
```

## Every investigation is a campaign

This includes work that does not feel like ML: capacity probes, throughput
comparisons, scheduler behavior, hardware qualification, bug hunts. If the answer
is a **measurement**, it is an experiment.

- Put the question in `Experiment.spec.hypothesis` as a falsifiable claim, not a
  task description.
- Put the variable in `spec.parameters`. Two arms that differ in one parameter is
  a controlled comparison; two ad-hoc runs are not.
- Let the controller write `status.metrics`. Never synthesize or patch status.
- A `RuntimeProfile` + `ExperimentTemplate` is the cost of doing this properly.
  Pay it. A local script that prints a number leaves nothing behind.

## Never delete to clean up

Deleting a campaign or experiment after extracting its number **erases the
memory**. There is no undo and no history to recover it from.

- Retire work by letting `budget` complete, or by removing the template from a
  drive's `templateRefs` so nothing new is proposed onto it.
- Prune noisy runs from an analysis with `ResearchReport.spec.excludedExperiments`
  — curation, not destruction.
- `kubectl delete` on a campaign/experiment is acceptable only for objects created
  in error that produced no measurement.

## Write the conclusion down

When an investigation concludes, create a `ResearchReport`:

```yaml
spec:
  campaignRef: <campaign that produced the data>
  title: <the claim, stated as a result>
  about:                     # optional: scope to one branch of the search tree
    kind: Experiment
    name: <experiment>       # includes its lineage descendants
  sections:
    Findings: <what the numbers show, with the numbers inline>
    Method: <what varied, what was held fixed, what hardware>
    Footguns: <what misled us, what a future agent must not repeat>
    Limitations: <what this does NOT establish>
  seededHypotheses:
    - <follow-up, phrased as a testable claim>
```

Required content, not optional polish:

- **Numbers inline.** "0.86x an RTX 4000 (9,827 vs 11,393 steps/min)" is memory;
  "CPU is competitive" is not.
- **Negative results.** A recorded failure is the highest-value entry in the
  system; it is what stops the next agent burning GPU-days.
- **Footguns.** Metrics that read healthy while the robot fails, hardware that
  cannot run an image, thresholds that fire for the wrong reason. State the
  symptom AND the tell, so it is recognizable next time.
- **Refuted hypotheses.** Record the ones you checked and disproved, with the
  evidence. Otherwise the next agent re-derives them.

## Keep it queryable

- Label campaigns/experiments/reports with the dimensions someone will filter on
  (robot, stage, purpose). Unlabeled objects are invisible in the console.
- Keep metric keys stable lowercase snake_case; keep Prometheus labels
  low-cardinality (never experiment UID, full SHA, or free-text).
- Prefer a bounded status summary plus an artifact link over dumping raw data
  into status.

## Console-specific

- The console reads Kubernetes via `kube` with local kubeconfig and must never
  write authoritative status or recompute campaign/benchmark verdicts client-side.
- Redact secrets, hidden holdout details, and private dataset paths from DTOs.
- If a finding matters to a steering agent, it must be reachable from a campaign
  or report view — not only from an annotation or a log line.
