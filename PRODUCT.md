# Product

<!-- impeccable:product-schema 1 -->

## Platform

web

## Users

A human supervising autonomous research, reviewing its evidence and steering it
when useful. Research review and infrastructure operation are different tasks,
not necessarily different people. Kubernetes operators are the controllers that
own execution; do not confuse that term with the human using the console.

## Product Purpose

Athena is an autoresearch platform expressed as a Kubernetes-native library of
operators and custom resources. Its scope includes Kubernetes HPC jobs and
SkyPilot, Ray cluster and Kueue execution, together with campaign projections
and ResearchReport rollups. This describes the intended product scope, not a
claim that every execution adapter or console workflow is implemented.

The console's primary human job is to review results that merit judgment while
autoresearch continues. Research progress, execution debugging, configuration
and report curation support that job rather than competing equally for attention.

## Operating Context

- Machine gates must be deterministic. Whether they passed is important evidence,
  not a substitute for inspecting the result.
- The concrete visual-review example is Spot walking: watch a candidate robot
  gait against the last retained best version for the same task.
- Human visual review is advisory or retrospective. Autonomous progress does
  not wait for it; an unreviewed result is not thereby blocked execution.
- The research agent should take human feedback into consideration according to
  its content. The user has not prescribed an automatic mapping from negative
  feedback to rejection, reward changes, rollback, promotion or cancellation.
- Campaign evidence can be rolled up into projections and research reports.

## Capabilities and Constraints

Kubernetes objects own durable desired and observed product state. Controllers
own status and lifecycle transitions. The console may present observations and
make authorized spec edits, but must not invent gate outcomes or write observed
status. Advisory observations use ResearchReport specs, including the experiment
subject, cited candidate/reference evidence and captured object identity/version.
Browser storage is only presentation state or a tab-scoped unsaved-draft backup,
never the research-memory or review-completion ledger. Observation context is
recovered across reload/authentication navigation and cleared after confirmed
save or explicit draft reversion/discard; credentials and agent receipts are not
part of that backup.

The implemented review entry, data contracts and remaining proposals are
documented in [the console UX brief](docs/console-ux.md). Panel-kit supplies the
rendering and interaction vocabulary, not one panel per object. Existing named
views and supporting research/operator tools remain available.

The following choices remain open because the corresponding interview round
was cancelled:

- Which results enter review: all runs, new best/retained candidates, agent
  nominations, anomalies, or some combination. The current Results browser lists
  experiments newest-first; that presentation order is not a review-priority policy.
- A second concrete, non-Spot review workflow and its evidence contract.
- A richer feedback schema and expected review time or volume. The implemented
  default is a nonempty free-text observation with captured evidence/context and
  the existing report name/title controls, not an agreed preference for a final form.

Agent acknowledgment, follow-up linkage and agent decision-resolution policy
remain proposed, not implemented or agreed. Report-save success and editor
conflict recovery are not evidence of agent consumption.

Review completion is also undefined: opening evidence or submitting an
observation must not be assumed to mark a result reviewed, and no dismissal,
deferral or re-review policy has been agreed.

## Brand Commitments

Preserve Athena's pink identity and the established pink/blue semantic accents.
The user explicitly rejected losing the pink during a component migration.
No replacement visual identity has been requested.

## Evidence on Hand

- [Console UX](docs/console-ux.md): implemented review entry and acceptance
  evidence, existing panel ownership, component inventory and explicit limits.
- `operator/crates/athena-console-web`: deployed-browser application code;
  the separate native Iced console is not its component or layout authority.
- Spot gait comparison is a user-supplied workflow example. No second workload,
  review-volume figure or time-to-review target has been confirmed.

## Product Principles

1. Preserve autonomy: human observation must not silently become a blocking gate.
2. Ground judgment in evidence: distinguish measured checks, human observations
   and agent interpretations, including their provenance.
3. Compare against an explicit baseline for the same task; do not equate a later
   experiment or a passing machine check with a better result.
4. Keep research memory durable and queryable through Kubernetes-owned resources,
   with reports linking conclusions back to their evidence.
5. Let the task determine the interface; infrastructure and object inventories
   explain research execution rather than becoming the product's whole story.
