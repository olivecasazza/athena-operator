# Athena Console UX

Last reviewed: 2026-11-14

This is the canonical story, panel-ownership and component inventory for the
console. Update it in place when adding or removing a panel. Kubernetes owns
research state; the console presents observations and edits ResearchReport
**specs**, never status. A panel needs a story, not merely an available widget.

## Product direction: review evidence while research continues

The confirmed product context lives in [PRODUCT.md](../PRODUCT.md). Athena is
autoresearch as a Kubernetes-native library: operators and CRDs own research
and HPC execution, including the user-described SkyPilot/Ray/Kueue scope.
Campaign projections and ResearchReports make the accumulated evidence useful.
This is not a generic cluster dashboard or a robotics-only video viewer.

The user selected a **review inbox with autonomous progress alongside it** as
the primary console job. For Spot, the concrete task is watching a candidate
walk compared with the **last retained best version for the same task**.
Human visual review is **advisory or retrospective**: research does not wait
for a verdict. The research agent takes feedback into consideration according
to its content; a negative observation has no agreed automatic veto, rollback,
reward-edit or cancellation meaning.

The console implements a **Review** entry with candidate/reference evidence,
controller observations and advisory ResearchReport editing. Results is a
newest-first experiment browser, **not an authoritative required-review queue**:
admission, urgency and completion policy remain open. Agent-feedback consumption
and acknowledgment are not implemented by this UI. The sections below distinguish
the implemented behavior, its data limits and the still-proposed J4 story.

### High-level principles and their interface consequences

| Principle | Concrete consequence | Reject |
| --- | --- | --- |
| Design around the human decision. | The default entry answers what merits review and opens its evidence; execution inventories are supporting destinations. | One equally prominent panel or navigation item per CRD. |
| Put evidence before chrome. | A review centers the candidate and explicit baseline; task identity, machine checks and feedback remain in context. | Requiring users to assemble a comparison by rearranging windows. |
| Make comparisons trustworthy. | Identify both versions, the task, artifact provenance and relevant evaluation conditions. Flag unavailable or non-comparable evidence; do not silently substitute another baseline. | Calling the newest run an improvement or inferring visual quality from a passing check. |
| Keep different authorities distinct. | Machine results show their declared check/input context; human observations are attributed judgments; agent interpretations remain distinguishable. Preserve literal controller-owned status. | A single green badge standing for execution health, gate validity, visual quality and human approval. |
| Make autonomy and intervention explicit. | Research progress remains visible while an item is unreviewed. Any actual steering or promotion action is separate, authorized and explicit. | Treating unreviewed as blocked, or treating feedback submission as a stop/promote/rollback command. |
| Reveal supporting complexity progressively. | Keep campaign context, metrics and relevant check evidence near the comparison; open manifests, queue admission, node power and detailed traces when needed. | A wall of infrastructure widgets before the user sees the result. |
| Preserve context and a usable default. | Navigation retains the selected task/candidate/baseline and protects an unsaved feedback/report draft. Desktop comparison uses the available space; compact layouts retain identities and media controls without page overflow. | Requiring layout customization, losing a draft on selection change, or shrinking evidence into a fixed-height remainder. |
| Preserve identity without encoding truth in color. | Keep Athena's pink/blue roles and consistent panel-kit controls. Explicit text distinguishes selected, failed, unavailable, unreviewed and running. | Another blanket grayscale override or a color-only verdict. |

These are Operate-mode principles: task completion, legibility, trustworthy
state and familiar controls outrank decoration. Panel-kit supplies shared
interaction and rendering contracts; it does not determine the information
architecture. No replacement visual identity is proposed.

### Prioritized job stories

The priorities follow the interview. Acceptance statements are the product
contract, not claims that every historical resource supplies the required
artifacts. Implementation and verification boundaries are recorded below.
Existing R/O/W identifiers remain capability coverage, not the priority order.

| ID / priority | User story | Acceptance and boundary |
| --- | --- | --- |
| J1 — primary, confirmed | When I return to autoresearch, I want to find results worth my judgment so I can review useful evidence rather than browse every object. | Reach a candidate's review with task and baseline identity intact. Autonomous progress stays visible. Which results qualify and how they are ordered remain open; do not pretend every completed run requires review. |
| J2 — primary, confirmed | When I review a Spot candidate, I want to watch it walk against the last retained best for the same task so I can judge visible improvement or regression. | Both artifacts and version labels are available in one comparison context. Playback controls remain usable. State material differences in conditions; show a missing baseline/artifact explicitly. Do not assume synchronized timestamps or interpolate evidence to make it comparable. |
| J3 — primary, confirmed intent | When I notice a meaningful difference, I want to give the research agent grounded feedback so it can consider that observation in its research. | Bind the observation to the candidate, baseline, task and relevant evidence. Make save/failure/permission state explicit and preserve unsaved work. The minimum form, timestamps/tags and automatic steering semantics are not agreed. Feedback alone is not a machine-gate result or a promotion decision. |
| J4 — proposed, needs product confirmation | After I give feedback, I want to understand whether and how the agent addressed it so I know the observation was not silently lost. | Link to an agent response or relevant follow-up experiment when a real contract supplies one. Saving a report is not proof that an agent consumed it. No acknowledgment states, service or controller status fields are invented by this brief. |
| J5 — supporting, confirmed scope | While research runs, I want to understand progress and investigate failures or waiting so I can distinguish research outcomes from execution problems. | Retain machine-check evidence and campaign context; disclose operator-owned Job/Ray/SkyPilot execution and Kueue admission where supported by real APIs. Name actual reasons for waiting. A pending human review is not one of those reasons in this workflow. |
| J6 — supporting, confirmed scope | When reviewing a body of work, I want to roll campaign evidence and findings into projections and ResearchReports so the research has durable, queryable memory. | Conclusions link back to experiments and evidence; human observations remain distinguishable from measurements. Preserve authorized spec-only writes, dirty drafts and resourceVersion conflict handling. Do not imply a generated summary has been verified or a new projection schema has been agreed. |
| J7 — supporting, existing capability | When execution context matters, I want to inspect configuration and capacity without losing the research subject I was investigating. | Keep Catalog/Infrastructure and detailed manifests reachable as secondary tools. Do not add imperative deployment controls or claim that the console already launches every execution backend. |

### Implemented information hierarchy

New sessions enter Review. Desktop uses a dominant evidence comparison and a
smaller Results browser; compact layouts lead with evidence and provide native
links to choose a candidate and return to the comparison. Existing saved views
and custom layouts remain intact; Review is added without resetting v5 storage.

1. **Review entry:** a bounded place to find reviewable results, with research
   progress alongside rather than an equal-weight fleet of resource panels.
   Admission, grouping, ranking and counts need a defined policy before they
   can be presented as authoritative.
2. **Evidence comparison:** selected task and campaign context, candidate versus
   retained-best evidence, relevant deterministic-check results and a place for
   grounded feedback. Supporting detail must not displace the comparison.
3. **Research explanation:** campaign history, hypotheses, metrics and reports,
   reachable without losing the review subject.
4. **Execution explanation:** trace, manifest, queue, capacity and backend detail
   when they explain what happened or why progress stopped.
5. **Configuration and workspace tools:** secondary destinations. Named layouts
   and panel customization remain optional, not prerequisites for J1–J3.

The comparison carries campaign snapshot counts, provenance and check evidence.
Refresh explicitly reloads observations; this change does not claim a Kubernetes
watch/SSE implementation. Layout reset/save controls are secondary disclosure,
and supporting panels are retained rather than removed merely to simplify a mock.

### Data contracts and honest capability gaps

- `src/review.rs` owns review selection, retained-reference resolution, paired
  artifacts and benchmark presentation. The first displayed candidate is pinned
  rather than silently changing when a refreshed snapshot contains a newer run.
- The default baseline is the selected candidate's **current campaign-reported
  incumbent**, in the same namespace, with a `Keep` decision and matching declared
  template, behavior/robot, objective/goal and benchmark-suite context. These
  declarations are not proof that evaluator inputs, seeds or budgets match.
- Campaign status does not retain historical incumbent transitions, and the
  controller may rewrite old winners to `Discard`. If the candidate is itself
  the incumbent, do not guess the previous best from dates, scores or row order.
  The unavailable state is explicit; users may choose a compatible manual
  reference, labeled as manual rather than historically retained best.
- A missing or changed manual reference remains unavailable. It never silently
  falls back to a different incumbent. Object UIDs keep a replaced same-named
  resource from becoming the selected experiment.
- The BFF exposes real object UID/resourceVersion, execution image, declared
  artifact URLs and condition messages. Private workspace paths and
  credential-bearing URLs are not published as direct browser links. Panathenaia's existing
  read-only `/api/v1/experiments/{name}/figures/{file}` contract is used only for
  its `apps` namespace, not another namespace's same-named experiment.
- Recorded images have paired scale/scroll and full-resolution links; directly
  published MP4/WebM evidence uses native playback controls. A filmstrip is
  labeled as frames, never a continuous movie or synchronized sequence.
  Artifact loading, failure and explicit Retry are visible rather than removing
  a failed image silently.
- Declared policy URLs can be opened in the existing Spot gym or requested in
  isolated embedded players. This is an independent external simulation: the
  operator must check morphology/environment, the players are not synchronized,
  and mounting its canvas is not proof of correct policy execution or a controlled
  benchmark. Non-robotics evidence does not require that renderer.
- Benchmark gates are controller-published values/thresholds and recorded suite/
  reproducibility hashes, not a client-created pass. Target kind/namespace/name,
  creation time and observed generation must agree before results are associated
  with a run. Missing timestamps or stale generations remain unverified. Hashes
  identify recorded inputs; the console does not prove evaluator determinism.
- `src/curator.rs` reuses its Draft, auth/save gate and report API for advisory
  observations. Report specs include an `Experiment` subject with `Assessing`
  motivation, candidate inclusion, cited references, captured task/object versions
  and an Observation section. No experiment status or promotion action is written.
- An unsaved observation survives selection changes and has tab-scoped session
  recovery across reload/authentication navigation. A synchronous before-unload
  flush preserves the latest edit; recovery pins the original reference or
  standalone context instead of substituting a newer incumbent. A fresh
  Kubernetes snapshot still validates object identity. The backup is cleared
  after confirmed save or explicit reversion/discard, not treated as a review
  ledger or a successful write. Create/update errors retain the typed HTTP
  failure and draft. The editor leads with the outcome and recovery action,
  not a Kubernetes exception dump; full diagnostics remain under a closed,
  bounded Technical details disclosure. Transport/upstream errors say **Save
  not confirmed**, never inventing whether persistence happened. Retry is
  explicit; no automatic resubmission runs.
- A saved observation can be compared after a conflict or uncertain outcome.
  Keeping it against that revision is explicit and preserves unrelated latest
  sections. Context/name comparison follows the same boundary-whitespace
  normalization as report serialization, so an unchanged persisted context is
  not mistaken for changed evidence. Actual subject/reference/context changes
  still block the merge. **Keep draft as a new report** retains content and
  evidence but clears the old name/version; creating it is a separate action.
  Explicit reversion clears stale error/comparison/preview state as well as
  restoring the base draft. Existing resourceVersion protection remains.
- Opening a saved report from Reports, campaign links or the curator chooser
  never implicitly discards a dirty draft. The exact namespace/name stays in an
  inline switch notice; **Keep editing** cancels only that navigation intent.
  **Discard draft and open report** is an explicit local discard, not a save.
  A running save finishes before switching can proceed. Late report reads
  cannot replace a newer selection, an explicitly reset document, or edits
  typed while the request was pending. Starting another new report is disabled
  while the current draft is dirty, including an unsaved new report.
- ResearchReport supplies durable research memory, not a feedback-consumption
  receipt. Nothing here asserts that the research agent read, accepted or acted
  on an observation.
- Durable observations, references and any future consumption evidence belong
  in Kubernetes-owned research memory. Clients author permitted specs;
  controllers own observed status. Browser-local state is only transient UI
  context or a draft, never the authoritative review queue or feedback ledger.
- The change adds optional, explicitly named BFF projection fields, not CRDs, observed
  statuses, write endpoints or dependencies. SkyPilot/Ray/Kueue are not claimed
  to expose identical console data or new launch controls.

### Open decisions; do not fill these with defaults

- Review selection and ranking: every run, new/retained best, anomalies,
  agent nominations, or another policy. The suggested radio option in a
  **cancelled** interview round is not a user decision.
- Review completion: what counts as reviewed, whether dismissal or deferral
  exists, and when changed candidate/baseline evidence calls for re-review.
  Opening media or saving feedback does not establish an agreed completion
  rule; do not invent one in UI state.
- A second concrete workload: Spot is the example, not permission to hard-code
  a robotics-only product or invent another workload's review needs.
- Richer feedback and review scale: the implemented minimum is an Observation
  text field with captured context. Structured moments/tags, workload volume and
  time-to-review targets are not established.
- Agent response/accountability: J4 is a recommendation. Whether an agent must
  acknowledge feedback, how it links follow-up work and when it may disagree
  remain unconfirmed.
- A universal task-equivalence/renderer contract: current matching uses the
  declarations above, not inferred task semantics or a claimed identical
  evaluation environment. The existing pink/blue visual identity stays intact.

## Application and dependency boundary

`Dockerfile.console:1-22` builds the Dioxus 0.6 SPA in
`operator/crates/athena-console-web`, then the feature-gated axum
`console-server`. The older Iced `athena-console` application remains the target
of the root flake's native console package/dev command. It does not consume
panel-kit and is not the deployed browser console; this change does not remove
that separate application or change its behavior.

The web package pins both panel-kit crates to canonical
`ocasazza/panel-kit@321a7a40b73ae92a0941396551fc16f5c93af1ea` in its Cargo.toml,
matching upstream `main` checked on 2026-11-09. The former `72b0510` pin was not
current. The update includes the table's keyboard-cursor/ring state and newer
windowed-table entry point, alongside Bevy/canary changes. Both direct crates
and the transitive grammar crate resolve to the same revision in Cargo.lock.
Dioxus CLI remains 0.6.x and wasm-bindgen CLI 0.2.121; no dependency is added.

The panel-kit checkout has two remotes: `origin` is the olivecasazza fork,
`upstream` is ocasazza, the actual Cargo source. Both were fetched before the
inventory. The personal fork is still at `8e4de76` (195 reachable commits),
86 commits behind canonical `321a7a4` (281 reachable commits), with no unique
main commits. Fork synchronization is proposed in
[panel-kit PR #5](https://github.com/olivecasazza/panel-kit/pull/5), left
unmerged for review without touching the separate CI/Hydra PR #4.
The consumer does not resolve through that stale fork. The complete canonical
log was read newest first through `7940325`, including merged histories;
checking only the personal fork would incorrectly recommend a downgrade.

## Existing capability coverage and canonical homes

| ID | Persona and need | Panel / entry point | Shared components and acceptance |
| --- | --- | --- | --- |
| R1 | Researcher: identify a stalled drive and the template gate blocking promotion. | Research, global level | Literal phase/condition `Badge`s and typed `DataTable` per stage. Show objective, succeeded count, gate result, promotion timestamp and stagnation without inventing a gate decision. |
| R2 | Researcher: inspect a campaign, compare its runs and scope its metrics. | Research campaign breadcrumb → Experiment Detail / Metrics / Manifest / Run Activity | One `DataTable` campaign browser, one member-run table; selecting a campaign scopes Grafana by campaign and actual run window. Breadcrumb returns to the global fleet. |
| R3 | Researcher: compare experiments across campaigns, sort/filter by outcome and inspect one run. | Experiments view → canonical inspector; Review for paired evidence | `DataTable` typed numeric/enum columns, search, column visibility/persistence, export, keyboard navigation, selected row. Inspector owns metadata and artifact links; Review owns paired visual evidence instead of duplicating a hard-coded filmstrip renderer. |
| R4 | Run debugger: inspect observed creation/start/end milestones and their facts. | Run Activity after selecting a run/campaign | Shared `Trace` graph/query/inspector and alternate `TraceRail` for an individual experiment. Only present timestamps become events; unknown terminal outcomes are not success. No fabricated actors, tool calls, token counts or durations. |
| R5 | Researcher: inspect the selected resource's metrics and manifest without losing selection. | Experiment Metrics / Experiment Manifest | `GrafanaDashboard` scoped by experiment/campaign and start/end with buffer; `IdePanel` local YAML buffer. Editing this buffer does not deploy or write status. |
| R6 | Scientist: find and review a report, edit its narrative/hypotheses, curate included runs, preview and save without overwriting a concurrent version. | Reports → Report Curator; campaign report rows use the same destination | `DataTable` for reports, dataset, references and status conditions; `Badge` for literal phase/conditions. Existing Dataset/Narrative/Hypotheses/References/Preview/Status tabs, auth, dirty-draft protection, resourceVersion conflict handling and spec-only writes remain. |
| O1 | Operator: inspect templates, benchmark suites/runs and execution profiles. | Catalog view | `DataTable` for each list and `IdePanel` template YAML. No imperative deployment controls. |
| O2 | Operator: distinguish queue admission from node power and inference readiness. | Infrastructure → GPU Pools & Queues / Node Power / Inference Backends | `DataTable` for workloads, nodes and endpoints; literal power/serving chips. Pool quota/usage summary remains separate from individual workload rows. Server-authorized, read-only scheduling API. |
| W1 | Any user: arrange/minimize/maximize panels, switch task views, reset or save a custom layout. | Review / Research / Experiments / Reports / Catalog, dock and Layout disclosure | Shared reducer/frame/dock/chrome and `workspace::panel_element`; host-owned named view persistence. Existing v5 saved/custom layouts are preserved; new sessions start in Review. |

### Identity and panel sizing

- Preserve Athena's pink and blue through panel-kit's `secondary` and `primary`
  theme roles. Do not flatten all brand, live-state and status roles into grey.
  Conditions still display literal type/status rather than interpreting every
  `True` as healthy.
- Keep panel-kit's pointer-dependent hit-target and density contract. A blanket
  44px minimum on every fine-pointer control inflates table rows and spends
  scarce panel space on chrome; coarse-pointer geometry belongs to the kit.
- Manifest editors fill the panel body's remaining height. Template panels split
  the available space between the bounded, internally scrolling catalog and its
  editor; the editor fills the lower region. Neither uses a fixed 260px wrapper.
  Verify ordinary, maximized, narrow and coarse-pointer surfaces.
- Resource names may wrap instead of hiding every distinguishing suffix behind
  an ellipsis. Detail copy uses the shared body size, not a smaller caption size.
- Tiny gate-evidence tables show search and a tally, not the complete nine-control
  power-user toolbar. Catalog tables retain search, columns and tally; experiment
  and campaign comparison tables keep the full shared controls.
- Standalone tables and trace overviews take the space left after their headings,
  not 100% of the body plus the heading again. Benchmark tables split one panel;
  research/curator tables remain content-sized. The overview/rail switch must
  not duplicate the inner trace component's Graph/Lanes/Collab mode labels.

### Stories with no data-backed home

- **Agent debugger: inspect tool arguments/results, model calls, reasoning trees,
  tokens, forked agent sessions and collaboration.** No such DTO or endpoint is
  supplied by Athena's console API. Run Activity explicitly states this limit.
  Panel-kit's trace/session models support these inputs, but creation/start/end
  timestamps are not a substitute. A future CRD-owned artifact/observed-status
  contract must supply bounded, redacted events before this story can be served.
- **Researcher: compare causal lineage as a general node/edge graph.** Parent and
  generation are available as table/inspector facts, but a general graph panel
  has not been added merely because panel-kit now supplies a graph painter.
  There was no hand-rolled general graph or agent-trace renderer to migrate.

### Panels without a distinct story

The standalone **Campaigns** panel duplicated Research's campaign browser; it
has been removed, including its preset and route. Research's separate
experiment and report leaf renderers duplicated the inspector/curator; those
routes are removed. Their required content moved to the canonical inspector
and curator, not discarded. No other registered panel is orphaned. Catalog
panels are reachable from Catalog; operator panels from Admin; other panels
can be restored from the dock. Curator's campaign picker remains a scoped
change-subject action, not a second always-visible campaign workspace.

## Baseline survey and sprawl report

The following citations refer to pre-change commit `8bcb8855366101f955128f1aa09260b2e0cde1d7`.
All paths in this section start at `operator/crates/athena-console-web/src/`.
They remain meaningful after consolidation changes line numbers.

| Baseline panel / surface | Data and implementation | Finding and disposition |
| --- | --- | --- |
| Experiments (`lib.rs:846-889`) | `/api/snapshot.experiments`, shared universal columns and panel-kit DataTable via `tables.rs:118-166` | Retained for cross-campaign comparison; not redundant with the campaign-scoped membership list. Phase becomes an enum filter and shared literal chip. |
| Experiment Detail (`lib.rs:900-924`) | Selected ResourceSummary; handmade metadata grid | Phase/namespace repeated in heading and grid. One identity heading; metadata and formerly separate leaf information live here. |
| Experiment Metrics (`lib.rs:968-993`) | Selected resource + Grafana run window | Already shared GrafanaDashboard; retained. No unstable d-solo panel IDs. |
| Experiment Manifest (`lib.rs:996-1015`) | `/api/manifest/{namespace}/{kind}/{name}`, local buffer | Already IdePanel; retained. It is a local draft, not a save/deploy action. |
| Campaigns (`lib.rs:1017-1062`) | Snapshot campaigns, `campaign_columns/campaign_row` | Same data and column contract as Research global (`1292-1312,1361-1370`). Removed; Research campaign activation now also selects metrics/inspector. |
| Templates (`lib.rs:1090-1151`) | Snapshot templates + `/api/template/{namespace}/{name}` | Shared table/editor retained; no replacement editor required for plain YAML inspection. |
| Runtime Profiles (`lib.rs:1153-1161`) | Snapshot runtime_profiles | Shared generic resource table retained. |
| Benchmarks (`lib.rs:1163-1207`) | Snapshot benchmark_suites/benchmark_runs | Two distinct resource kinds, not duplicate lists. Shared tables retained. |
| Research global (`lib.rs:1289-1372`) | Snapshot drives, stage evidence, campaigns | Stage gate tables hand-rolled at `1338-1356`; migrated to typed tables. Drive evidence has no other home. |
| Research campaign (`lib.rs:1373-1472`) | Snapshot campaign members + reports | Retained scoped lists; rows open canonical destinations. Namespace participates in membership lookup. |
| Research experiment leaf (`lib.rs:1473-1519`) | Cached selected row, hypothesis, filmstrip, another Open detail panels action | Removed extra stop/action; direct inspector selection retains hypothesis, filmstrip, decision, parent and timestamps. |
| Research report leaf (`lib.rs:1520-1541`) | Snapshot sections and seeded hypotheses | Competing read-only copy of curator Narrative/Hypotheses. Removed; report rows load the full resourceVersion-backed curator. |
| Reports (`lib.rs:1550-1608`) | Snapshot reports, Open → full report API | Retained cross-campaign index; curator's chooser is scoped and serves editing context. |
| Report Curator (`curator.rs:251-433`) | Snapshot membership, full report GET, spec create/update and preview endpoints | Retain editing/auth/conflict semantics. References (`616-648`) and conditions (`677-707`) were handwritten tables; migrate both. |
| GPU Pools & Queues (`lib.rs:1614-1663`) | `/api/scheduling`: pools and active workloads | Summary vs workload evidence are distinct; retain both. Existing shared workload table gets typed state filtering. |
| Node Power (`lib.rs:1665-1692`) | Scheduling nodes | Handwritten table and ornamental power dots; use typed table and literal power chip. |
| Inference Backends (`lib.rs:1694-1729`) | Scheduling inference | Handwritten table and serving/starting glyphs; use typed table and literal serving chip. |
| Workspace shell (`workspace/mod.rs:40-57`) | Host projection + catalog | Repeated shared shell/chrome/body/grip composition; replaced with `panel_kit::workspace::panel_element`. Keep app-specific row-major named-view lifecycle instead of introducing a persistence migration unrelated to the stories. |

Additional conflicting conventions:

- `lib.rs:1213-1235` and `curator.rs:698`: every `True` condition was green,
  every other value red. Conditions have positive and negative polarity;
  literal type/status/reason must be shown without asserting health.
- `lib.rs:343,392-410,437-444`: ad-hoc hue aliases and rounded app chips/buttons
  bypassed the square shared chrome and theme roles. Pink is Athena's identity,
  not the defect: preserve it through `secondary`, with blue through `primary`.
  Use shared badges and explicit status text rather than bespoke condition CSS.
- `tables.rs:276,352` and generic/template/report rows keyed only by name.
  The table's stable-ID contract requires namespace identity. Resource rows
  now use manifest paths; namespaced catalog/report rows use namespace/name.
- `tables.rs:332` called succeeded counts `OK`, while other lists used
  `Succeeded`; use the authoritative count name consistently.
- Registered panels were reachable; the problem was duplicate destinations,
  not a collection of hidden unimplemented panels. No backend endpoint,
  benchmark behavior, Kubernetes state or native-console behavior is removed.

## Panel-kit component inventory

Paths below are relative to canonical `ocasazza/panel-kit` at `321a7a4`.
All listed APIs are current unless explicitly marked superseded/not exported.
Public web components are Dioxus painters; neutral models live in
`crates/panel-kit-core/src/`. They are not Iced widgets.

### Trace and graph are different contracts

- **`widgets::trace::Trace`** (`src/widgets/trace/swimlane/mod.rs:281`):
  `id`, host-owned `GraphState`, `Rc<Vec<Event>>`, optional `Rc<SessionTree>`,
  explicit epoch-seconds `now`, optional selected item/open inspector node;
  emits `GraphAction`, selected item and inspector-node callbacks. One widget
  contains GRAPH, LANES sequence and COLLAB pair-table modes, query/range/tree
  controls, legend and node facts. Host calls `GraphState::reduce`. Event
  timestamps are **microseconds**, not milliseconds. No actor may be inferred
  from a campaign/template name.
- **`widgets::trace::TraceRail`** (`src/widgets/trace/rails.rs:273`): resolved
  `RailView`, host-owned `Expansion`, toggle/select/open-all/close-all events.
  `rails::vertical_with(events, ItemRef, SessionTree, Expansion, wrap).view(opts)`
  derives trunk/forks/timestamps and expanded facts. It is the single-item
  trace view, not a separate general graph engine.
- **Agent/session trace support** (`widgets/trace/rails/panels.rs`): neutral
  `TraceNode` reasoning trees (`TraceKind`, `TraceStatus`, children),
  `trace_rows(roots, collapsed)`; `RunTick` and `run_trajectory(events, run_id)`;
  metric rows, session state, token-per-minute buckets, sparkline and tool
  call/result descriptions. These are current **models/helpers**, not an
  exported web `AgentTrace` component or an Athena data source.
- **Timeline models** (`widgets/trace/rails/timeline.rs`): `TlNode`, `TlEdge`,
  `TimelineGraph` or caller-supplied `FlatLane`; derive TimelineLane/View with
  current marker and earlier-item truncation. No standalone public Dioxus
  Timeline component is exported by `widgets::trace`.
- **General graph** (`src/widgets/graph.rs:41`): `graph_svg(GraphView,
  positions: &[[f32;2]], selected, on_select)` paints a node/edge graph over
  host-solved normalized positions. `graph_notice(title, detail)` is the
  capability-unavailable body. Core `GraphModel` has nodes (id, label, sort,
  size/color/species/attributes) and index-based directed edges;
  `GraphLayout` names solver/parameters. WebGPU graph-view and graph-layouts
  are vendored crates, with SVG and terminal/braille paths sharing the model;
  this is **not** the Trace activity graph. Athena has no existing painter
  for this contract to replace.

### Tables, chips, controls and content

| Component / source | Purpose and current props/data contract | Status / replacement |
| --- | --- | --- |
| `widgets::DataTable`, `src/widgets/data_table/mod.rs` | `id`, typed `Vec<DataColumnSpec>`, shared `TableData`; optional custom cell callback, TableDefaults, storage_key, accessible label, empty Element, placeholder, row activation, host selection/state and event/query callbacks, tags/ribbon, toolbar, footer, exports, virtual window/max_height. | Current unified table. ColumnKind Text/Enum/Number controls filtering, sorting and storage; `.enumerated()` and `.number()` are the declaration seam. Host-owned state and server query mode are supported. |
| `widgets::table::{table,bound_table,windowed_table}`, `src/widgets/table.rs` | Borrowed TableView or binding + TableModel and optional row-ID callback; adapts authored/provider table content to the unified table. `windowed_table(name, view, cursor)` seeds a keyboard cursor/ring and mounts a virtual row window. | Current spec-content adapters, not rival interactive table implementations. |
| `table::Table`, `src/table.rs:38` | Earlier text-only columns/row matrices, row-index selection, dense/class, optional index click. | Historical module remains on disk but is **not exported** by current lib.rs; do not adopt it. Stateful DataTable supersedes it for consumers. |
| `badge::Badge`, `src/badge.rs:212` | BadgeSpec(kind, field/value, active, x/plus, small, tint/accent, click/hover semantics, tone/variant), optional web CSS accent; emits BadgeAction. | Current metadata/filter/navigation chip. `widgets::badge::strip` paints a borrowed list. Square tone variants replace ad-hoc `.chip/.cond` markup in Athena. Metadata activation copies its value. |
| `widgets::Button`, `src/widgets/button.rs:17` | pressed Option<bool>, compact, forwarded global/button attributes, on_press, children. | Current square content action; separate from header controls. |
| `PanelHeaderButton`, `src/lib.rs:318` | label, title, active, disabled, on_press; stops drag gestures. | Current header-slot action; not a replacement for every content button. |
| `widgets::Dropdown`, `src/widgets/dropdown.rs:17` | Grouped DropdownItems, host Signal<DropdownState>, selected, placeholder, searchable, on_action. | Current host-state selector; older `src/dropdown.rs` owns state internally and is no longer exported. |
| `widgets::CascadingDropdown`, `src/widgets/cascade.rs:17` | CascadeItems with paths, host Signal<CascadeState>, selected path/label, placeholder, show_breadcrumb, on_action. | Current Miller-column selector; leaf-only trigger retains full tooltip. Old root cascade module is not exported. |
| `IdePanel`, `src/ide.rs:16` | value String, language/title, read_only, on_change(full text). | Current lightweight line-numbered editor; not superseded by Monaco. Appropriate to existing Athena YAML buffers. |
| `MonacoEditor`, `src/editor.rs` | Optional bound Signal<String>/initial text, language, read_only, theme, word_wrap, on_change/on_ready and submit callback; EditorHandle controls value/language/read-only/layout. | Current richer editor, vendored worker/asset base required. Nix grammar and bound read-only fixes landed later; no need to replace Athena's simpler editor solely for appearance. |
| `GrafanaDashboard`, `src/grafana.rs:124` | base_url/dashboard_uid, slug, vars, theme, from/to, title. | Current whole-dashboard `/d/` embed plus open link/loading. Original combined GrafanaPanel split in 39b7881. |
| `GrafanaPanel`, `src/grafana.rs:170` | Same fields plus required stable numeric panel_id. | Current single `/d-solo/` visualization; not interchangeable with Dashboard. Athena keeps Dashboard because panel IDs are not pinned. |
| `LoadingWorkspace`, `src/lib.rs:259` | title, status, optional measured fraction. BOOT_HTML/BOOT_CSS are the pre-WASM static sibling contract. | Current first-paint shell; no made-up percentage. |
| `loading::ProgressBar/LoadingGate/GlobalLoadingBar` | Fraction/label/detail/compact; LoadingStore+children; registry aggregate. loading_store(id,label) owns begin/update/succeed/fail transitions. | Current async hydration. `use_loading_store` was renamed to `loading_store` in 5b582e3. |
| `widgets::PanelHydrating`, `build_progress.rs:41` | label and optional measured fraction; progress_bar(Option<f32>) helper. | Current presentational first fetch state, distinct from transient spinner and stateful LoadingGate. |
| `Spinner`, `widgets::spinner` | Optional label wrapper; neutral SpinnerModel plus deterministic tick painter. | Current transient-action spinner; reduced-motion fix 0431d11. |
| `widgets::forms::{Slider,IntRow,CheckRow,TextRow}` | Label/current value/on commit; numeric min/max, optional logarithmic scale/title/note/text as applicable. | Current shared label+control rows, core parsing/clamping. Not a complete report-draft form controller. |
| `widgets::SelectableCard`, `selection.rs:33` | SelectionCard(identity, name/subtitle/chips/description, disabled reason, default/viewing/ghost/class), on_select, children; card_chip helper. | Current one-of-N selection, not a reason to wrap every metadata section in cards. |
| `widgets::pills::pill_list` | field, Vec<Pill> (id/label/tip/tint), truncation count, ID on_select. | Current entity-neighbor navigation, distinct from field/value filter badges. |
| `widgets::frontmatter::{frontmatter_grid,fm_value_cell}` | JSON string leftover rows / JSON value. | Current collapsed long/nested/null metadata rendering; core chip detector owns the complementary scalar walk. |
| `widgets::detect::status_color` | Case-insensitive known status → optional theme role/CSS color. | Current presentation helper, **not** Kubernetes condition-polarity logic. Athena keeps literal condition status neutral. |
| `widgets::hints::{HintBar,publish,clear,shown}` | Host-published Hint(source,title/body/hotkeys); latest entry wins. | Current contextual hint registry/bar. |
| `widgets::charts::time_series` | &[SeriesView] of named points/color, unit. | Current shared time-series painter. |
| `widgets::charts::gauges` | &[GaugeModel] label/value/capacity/color. | Current capacity bars. |
| `widgets::charts::flamegraph` | &[FlameSpanModel] flat preorder depth/value/label/color. | Current shared flame layout/painter; no Athena span DTO exists. |
| `widgets::charts::boxplot` | &[BoxItemView] borrowed five-number summaries and label/color. | Current distribution painter. |
| `widgets::meter::meter` | MeterModel(label, ratio, text, optional RGB), clamped fill. | Current accessible progressbar. |
| `widgets::status::status` | StatusModel(label, coarse state, RGB). | Current live status readout, not authoritative research evaluation. |
| `widgets::scroll::text` | Borrowed text + ScrollPolicy. | Current text overflow painter. |
| `BevyCanvas`, `src/bevy.rs:59` | Opt-in bevy feature: module_path, canvas_id, loading_text; get_bevy_handle(canvas_id). | Current imperative WASM canvas mount; not needed by this Dioxus console. |

### Workspace, specification and renderer components

- `widgets::root`, `panel`, `panel_layout`, `dock`: independently callable
  root/shell/body/chrome/traffic-light/grip/dock painters over projected frames,
  catalog metadata and WorkspaceEvent callbacks. `workspace::panel_element`
  composes a single panel; `PanelWorkspace`, WorkspaceOptions, reducer-aware
  event handlers, persistence and viewport helpers own the shared web glue.
- `core::views::SavedViews` and `persist`: named layouts and save/restore
  contracts; the old monolithic Workspace controller/use_views API is removed
  by the 1.0/1.1 clean cutover (`b5609e7`, `0f5c9b2`). Athena's local named-view
  adapter remains because it also owns row-major presets, per-view storage,
  custom-view actions and admin tile-height policy.
- `keepalive::render_keepalive` and `render_keepalive_with_shell`: retained DOM
  for selected hidden panels, with host keep-mounted predicate/body and optional
  host shell. Current web-only composition, not a terminal state model.
- `spec_workspace`, `section_view`, `autoloop_host`, `canary`: promoted
  specification/Topos/auto-loop demo host surfaces (`fb1af12`), not additional
  Athena personas. WorkspaceSpec, ContentSpec/ContentView, DataSource bindings,
  external-component requirements and GraphModel feed renderer-neutral plans;
  panel-kit-grammar parses sites/morphisms/section edits. A component's existence
  does not license adding an Athena orchestration flow outside Kubernetes.
- panel-kit-tui mirrors table/meter/status/spinner/scroll/chart/badge/frame
  contracts over ratatui; browser terminal hosts use ratzilla/beamterm. The
  Bevy backend additionally paints table/graph/selection/windowing through
  target cameras. These are current alternative renderers, not drop-in
  Dioxus components for Athena.
- `graph_nav` and `keymap` are current neutral graph-cursor/marks/jumplist and
  data-authored intent-routing/tour models (`2877fef`, `72b0510`), replacing
  unread ad-hoc CommandBinding fields. No console graph command layer is added.

### Replacement chronology (newest first)

- `321a7a4` / `2f418e8` / `d3acf03`: named target-camera Bevy input/rendering
  and seeded keyboard cursor/ring behavior; Athena now consumes this revision.
- `3abc237`, `af4e0ae`, `fb1af12`: shared selection/forms/hints/build/pills/
  frontmatter models and painters, and example-host glue promoted into library.
- `9b271d7`, `36d4588`, `807e911`: Monaco wrap, bound read-only updates, Nix
  language and submit action; IdePanel remains a supported simpler choice.
- `9aa33b7`: **EventGraph renamed Trace**; GRAPH/LANES/COLLAB are modes of the
  same activity widget, not three independent panels. `fcd4f2d` adds node-fact
  expansion; `100c8ff` derives fork lanes only from real events when no tree is
  supplied. No synthetic agent assignment is necessary.
- `3a2dd60`, `0baa35c`, `2065988`, `ac32ad4`, `d8aca0a`: one typed table,
  host-owned state, stable-ID selection, filtering/grouping, incremental order,
  export and scaled virtualization replace the initial simple/search table.
- `85dae0c`, `98d2ebe`, `7177b9f`: square Button/semantic badge chips, role-named
  theme tokens, square chrome. Per-app legacy color aliases are not the API.
- `0f5c9b2`, `b5609e7`: reducer/frame/widget composition replaces controller
  entry points and aligns web/terminal surface/token contracts.
- `930c067`, `5b582e3`: store-shaped loading replaces loose spinner-only
  hydration; store accessor renamed (not a hook).
- `afee000`, `6557db7`, `2351de7`: fit-to-text optional cascade breadcrumb,
  Miller columns, grouped search selector.
- `39b7881`, `f281edb`: whole-dashboard versus single-panel Grafana split and
  initial shared IDE; `6f633b8`/`b4974e6` establish the static boot sibling.
- `4160e43`, `f6ac787`, `4a77a51`, `7940325`: table/status/meter/text, charts,
  shared core+web+terminal, then initial Dioxus workspace foundation.

## Private access: identity before research data

Authentication is not a report-save enhancement. Research resources are private
business data, whether requested by the browser, REST or MCP. A Kubernetes
ServiceAccount limits backend privilege; it does not authorize the human or
agent making an HTTP request. Cloudflare Access remains an additional ingress
boundary, not a role grant trusted by the BFF.

### Access stories and existing homes

| ID | User story | Home and contract |
| --- | --- | --- |
| A1 | As a returning researcher, I sign in before loading research data and see whether access is missing, expired or denied. | One workspace entry state, shared buttons/chips, protected `/api/auth/session`; no wall of identical panel errors and no anonymous snapshot fallback. |
| A2 | As a viewer, I inspect research evidence, reports and manifests without acquiring report-write or infrastructure privileges. | Existing Review/Research/Experiments/Reports/Catalog panels; authenticated reads, explicit read-only save explanation, no Infrastructure entry. |
| A3 | As a researcher, I author a ResearchReport spec without overwriting another revision or losing a draft during sign-in. | Existing feedback composer and Report Curator; server write capability, namespace/body agreement and resourceVersion checks. Advisory and standalone curator drafts recover separately in this tab. |
| A4 | As an operator, I inspect queues, workload admission, power and inference without confusing capacity reads with research mutation. | Infrastructure panels and protected scheduling endpoint; namespace-scoped workloads/inference plus explicitly cluster-wide capacity reads. |
| A5 | As a proposer, I authenticate as a bounded research-memory reader and stop before asking the model if prior-art access fails. | Existing MCP tools, stateless proposer Job, namespace-local credential Secret and Prime Agent's supported bearer environment reference. No scheduling tool, report writes or Kubernetes token. |
| A6 | As the owner of shared Grafana, I grant datasource access deliberately rather than exposing query APIs to anonymous dashboard visitors. | Existing Grafana SSO and `GrafanaDashboard`; explicit Grafana client role, strict role mapping, anonymous access disabled and a separate sign-in link. |

No new panels or AuthProfile CRD are needed. These stories constrain the
existing surfaces instead of adding another administration dashboard.

### Permission and namespace contract

Tokens require the configured issuer and exact **resource URL** audience, a
nonempty subject and an issuer-advertised signing algorithm. Merely having the
browser client ID in `aud`, a matching `azp`, or a global realm-admin role is
not application authorization. Roles come only from
`resource_access[OIDC_CLIENT_ID].roles`; unknown/unassigned application roles
are denied. Multiple assigned roles union their capabilities.

| Athena client role | Research reads / preview | Report spec create/update | Infrastructure reads |
| --- | --- | --- | --- |
| viewer | Yes | No | No |
| researcher | Yes | Yes | No |
| operator | Yes | No | Yes |
| admin | Yes | Yes | Yes |

Every business REST route and MCP operation is protected. One static operation
registry supplies the REST capability check, filtered MCP tool list and MCP
call policy. Preview reads research state but persists nothing. Health, public
OIDC configuration, OAuth resource metadata, OpenAPI and static assets remain
discoverable; they do not contain research records.

The server's named profile supplies an explicit namespace allowlist. Snapshot
and list acquisition use namespaced Kubernetes requests, not a cluster-wide
query followed by UI filtering. Manifest kinds are an allowlist of research
resources, not arbitrary `kubectl` resource arguments. Path/body namespace
checks precede Kubernetes access; report replacements must also match the URL
name. The profile is a shared allowed-namespace boundary, not per-user tenant
impersonation. Kubernetes audit sees the console ServiceAccount.

The backend SA has read access to research resources, Jobs, Kueue workloads
and RayJobs in the allowed namespaces; its only research writes are
ResearchReport create/update. It has no Secret, status-write, delete or
experiment/campaign/template mutation permission. A separate read-only
ClusterRole covers ClusterQueues and MetalMachines for capacity inspection.
Only the HTTP infrastructure capability can reach that endpoint.

Production defaults to `ATHENA_AUTH_MODE=required`. Required settings are
`ATHENA_ACCESS_PROFILE`, `ATHENA_NAMESPACES`, `OIDC_ISSUER_URL`,
`OIDC_CLIENT_ID` and `OIDC_RESOURCE_URL`. `OIDC_JWKS_URL` selects the trusted
public-key endpoint; the deployment uses internal Keycloak while retaining
public HTTPS issuer/resource identifiers. Missing configuration fails startup.
Unauthenticated development requires explicit `disabled-local` and a loopback
listener; it cannot silently expose a wildcard-bound server.

### Session and draft behavior

All business reads and writes use the same bearer transport. Only auth
discovery and the public-client token exchange are unauthenticated. The
browser obtains capabilities from the server, not from token presence or a
client-decoded role. This remains the initial-snapshot/manual-refresh UI:
there is no invented SSE/watch implementation.

The PKCE callback consumes state, verifier and the original redirect URI as one
single-use record. Callback cleanup removes OAuth parameters without erasing
the application's path/query. Expired credentials are distinguished from an
initial signed-out visitor; server rejection invalidates the visible session.
Neither permission failures nor reauthentication submit or discard a draft.
Review feedback and standalone Report Curator recovery reuse the same
tab-scoped draft mechanism with separate keys; they are not a research ledger
or proof of a successful write.

### GitOps profile and shared-Grafana boundary

The canonical deployment definition is
`nixlab:modules/k8s/apps/athena-access-profile.json`, consumed by console,
Keycloak, Grafana and declarative ResearchDrive modules. Content-addressed
ConfigMap names force the affected processes to pick up environment changes.
The browser remains a public PKCE client; no browser-client secret is added.

The four already-declared human realm administrators receive explicit Athena
admin client roles and **Grafana Viewer only**. Existing realm-admin grants,
builtin composites and identity-provider account-linking policy are not
reworked by this cutover. The machine client receives only Athena viewer.
The existing local Grafana break-glass/operator credential path is retained.

Grafana Viewer is **not namespace isolation**: authorized viewers can query the
shared configured datasources, not merely the visible Athena folder. Athena
roles do not automatically grant Grafana membership. The metrics panel links
to Grafana sign-in and never puts a console bearer in an iframe URL. Existing
anonymous embeds throughout the shared instance will need an authenticated
Grafana session; account for that impact and the Grafana Recreate rollout.

Proposer authentication is additive
`ResearchDrive.spec.proposer.harness.mcpAuth`: token URL, client ID, resource
and local `clientSecretRef`. The Job mounts only that credential key read-only,
without a Kubernetes token. The reader token lifetime is 1,200 seconds; the
runner requires it to cover the configured Job budget plus 120 seconds and
verifies MCP initialize/tool discovery before invoking the model. Redirects
cannot forward its secret or bearer. Proposer egress adds only the Keycloak
token endpoint and uses the actual MCPServer pod label; MCP targets port 3000.

The three existing drive specs become GitOps-owned configuration inputs,
without copying status or rewriting their research history. Their observed
pause settings are preserved (arena active; curriculum and Spot paused), and
Flux pruning is disabled for these research-memory CRDs. Changing authentication
must not resume a drive or manufacture an agent acknowledgment.

Review the exact principal bindings, SA grants, shared-Grafana impact and
immutable artifacts **before deployment**. Provision the reader credential in
the existing GCP `auth-keycloak-clients` secret's
`athena-research-reader-secret` property; ExternalSecret projects only that
property into `apps/athena-research-reader`. Do not paste it in chat. Import
the new client/roles, install the additive CRD/operator and runner changes,
then cut over the private console/Grafana profile. Source-main chart changes
also reconcile through Flux, so source merge is part of this reviewed rollout,
not a harmless code-only step.

## Verification contract

For review-first verification, exercise J1–J3 with real or explicitly labeled
fixture contracts: candidate/reference identity, absent/changed baselines,
non-comparable conditions, stale benchmark generations, artifact loading/error/
retry, image scaling, video playback, unsigned writes, draft preservation and
create/update conflict recovery. Do not turn report-save success into a J4
agent acknowledgment. Keep actual Kubernetes, OIDC and external-renderer
integration claims separate from intercepted fixture-write assertions.

Build the deployed SPA with Dioxus CLI 0.6.3 (`dx build --platform web --release`)
and the native BFF with `cargo build -p athena-console-web --features server
--bin console-server`. Run the complete web package tests with `--features
server` and clippy for all its targets. Do not weaken or re-pin existing tests.

For browser verification, serve the actual built bundle with console-server;
use clearly labeled read fixtures and intercept fixture writes rather than
mutating Kubernetes or adding a durable fixture-state service. Exercise R1–W1,
empty/error/loading states, run selection and trace expansion, report tabs,
Catalog/Infrastructure, named-view persistence, and a constrained viewport. Save screenshots
and text captures outside the repository; distinguish fixture rendering proof
from live OIDC/Grafana/Kubernetes integration. No claim about agent traces can
be made without their missing data contract.

### Draft-safe report navigation verification (2026-11-14)

- The console package's 73 library and 21 BFF/auth tests pass. Five additional
  regressions cover dirty report switches, out-of-order responses, edits made
  during a read, explicit document resets and navigation requested during a
  save. Clippy retains only its three existing warnings.
- The production `Dockerfile.console` recipe builds. Eighteen browser
  assertions against its actual bundle and `console-server` cover both Reports
  and chooser entry points, explicit keep/discard, delayed response ordering,
  edits during loading and a new document created before an earlier read returns.
  Navigation submits no report write; six report reads use intercepted synthetic
  fixtures. The loopback-only server runs non-root/read-only with a credential-free
  fixture kubeconfig, not a live cluster connection.
- The inspection found the dense theme's 24px hit token was too small for the
  new recovery actions. The corrected controls measure at least 44px in the
  390px confirmation viewport. Long Kubernetes names wrap without page or notice
  overflow; copy contrast measures 16.91:1 and the safe action has a 2px solid,
  2px-offset focus ring. Desktop and compact captures remain outside the repo.
- This refinement changes no auth policy, panel-kit pin, CRD, dependencies or
  controller status. It does not establish live SSO/Grafana access or deploy
  the held private-access cutover.

### Report recovery verification (2026-11-14)

- The console package's 68 library and 21 BFF/auth tests pass, including
  regression coverage for canonicalized persisted review context, preservation
  of evidence when forking an uncertain save, and stale recovery-state cleanup
  after explicit reversion. Clippy retains only its three existing warnings.
- The production `Dockerfile.console` recipe builds. Seventeen browser
  assertions against that bundle cover uncertain-write copy, no automatic
  retry, complete collapsed diagnostics, bounded expanded diagnostics at 390px,
  conflict comparison, explicit latest-version observation merge, preservation
  of concurrent sections/evidence, safe fork and reversion. The standalone
  curator uses the same notice and retains its separate draft backup.
- The first browser pass exposed a real normalization mismatch: the report DTO
  trims section boundaries but the old merge guard compared raw editor text.
  The corrected guard accepts that canonical round trip while still refusing
  changed evidence. Desktop and compact confirmation captures are outside the
  repository; no visual-identity replacement or dependencies were introduced.
- All requests in this proof used a synthetic, loopback-only fixture with no
  Kubernetes client or credentials. Fixture success/failure is not a real save
  or a controller outcome. Live SSO remains a separate owner-passkey prerequisite;
  the held auth/GitOps cutover was not merged or deployed by this refinement.

### Private access verification (2026-11-13)

- All **65 library and 21 BFF/auth tests** pass. Coverage includes role unions,
  issuer/resource/subject and algorithm rejection, anonymous business reads,
  REST/MCP viewer-write parity, namespace refusal, unsupported manifest kinds,
  single-use PKCE and naturally expired versus missing credentials.
- Seven isolated proposer HTTP tests pass: valid research access, insufficient
  token lifetime, bad/partial credentials, missing prior-art tools, and refusal
  to forward either client secret or bearer through redirects.
- The production console and proposer Docker recipes build. The declared Nix
  operator-image output and x86_64 `athena-console-web` / `proposer-auth`
  checks build; no GitHub Actions Nix build or Hydra-executor success is claimed.
- The actual release bundle/BFF ran as UID 65532 with a read-only root,
  no capabilities and a fixture-only kubeconfig. Real browser PKCE requests
  verified the original redirect query and server-verified permissions.
  No business snapshot/scheduling/report request occurred before sign-in.
- Viewer reads, disabled report saves and absent Infrastructure entry are
  verified. Researcher saves pass the HTTP capability boundary; a deliberately
  rejected **fixture** persistence request retains the draft and exposes Retry.
  Operator infrastructure reads carry a bearer while report writes remain
  disabled. Seven additional BFF HTTP boundary checks and a signed machine
  OAuth/MCP initialize/tool-discovery round trip pass.
- Both advisory and standalone curator drafts survive authentication
  navigation. A natural-expiry browser scenario caught the initial
  signed-out/expired ambiguity; the corrected bundle shows the explicit
  expired state and a usable Sign in action without losing either draft.
  Desktop and 390px surfaces were inspected; the compact page has no
  horizontal overflow. Captures/logs remain outside the repository.
- Kubernetes fixture inputs are read-only captures. The fixture server rejects
  persistence; no real research object/spec/status was written for this proof.
  Empty infrastructure fixtures do not establish real capacity or backend health.
  Live Google/Keycloak/Grafana SSO and a real report save are **not** claimed.
  The public browser handoff stopped at Keycloak's existing-account challenge,
  without account linking or live security changes.
- The security reviewer returned an unsupported-provider notice, not findings.
  Independent LLM security approval is unavailable; the cutover still requires
  owner review of the exact generated bindings, datasource scope and rollout.

### Review-first implementation evidence (2026-11-10)

- `cargo test --locked -p athena-console-web --features server`: 62 library
  and 17 server/auth tests pass, including retained/manual reference boundaries,
  namespace/task/object-identity mismatches, stale benchmark generations,
  credential/private-path exclusion and concurrent observation-merge protection.
- The native server and Dioxus 0.6.3 browser bundle build. The complete
  `Dockerfile.console` production recipe also builds on Rust 1.89/Bookworm from
  a tracked-source archive plus the implementation files, without local caches,
  kubeconfig or unrelated workspace files in the image context.
- Seventeen browser fixture scenarios pass, including a repeat against the
  production image: paired evidence, native selection, image scale, benchmark
  freshness, missing artifacts/references, empty-observation blocking, draft
  preservation, comparative and standalone reload recovery including the latest
  edit, create conflict, explicit update recovery preserving a concurrent
  section, native video decode/playback and desktop/compact layout. All four
  fixture report writes were intercepted;
  fixture auth was disabled only in the intercepted fixture config, never in
  the actual preview or cluster.
- Four captured report references have matching narrative citations. No
  Kubernetes research objects, report specs or observed statuses were mutated
  for verification.
- Fourteen real-data UI/API checks pass against the read-only cluster snapshot
  (402 experiments, 68 campaigns at verification), including actual object
  identities/manual reference, OIDC-enabled unsigned-write blocking, secondary
  views/Admin and bounded evidence-first compact layout. Two declared-policy
  frames mount canvases and finish the external WASM loading state; this does
  not establish policy compatibility, active inference or evaluation quality.
- Historical declared evaluation-frame URLs can return missing artifacts.
  Missing media, unknown task declarations and absent incumbent history are
  presented explicitly, not replaced with fixture content or a guessed winner.
- Screenshots and browser reports are outside the repository under
  `/tmp/athena-review-*`. The labeled fixture comparison is rendering/interaction
  evidence, not a recorded robot measurement or a deployed-release claim.
- Delegated investigation/implementation models were unavailable. The fallback
  was inline implementation, manual consistency review and actual browser
  verification; no independent LLM review approval is claimed.

### Historical migration evidence and limits (`221f0a3`)

- `cargo test -p athena-console-web --features server`: 55 frontend/model tests
  and 15 BFF/auth tests pass, including timestamp overflow/unknown-outcome and
  namespace separation checks. Existing test assertions were not weakened.
- `cargo clippy -p athena-console-web --features server --all-targets --no-deps`
  completes with three pre-existing console warnings: auth-flow nested `if`,
  an identity map in an auth test, and a default-field assignment in an existing
  curator test. `-D warnings` fails on these; including dependency lint also
  finds 18 existing athena-api warnings. No blanket lint suppression was added.
- `dx build --platform web --package athena-console-web --bin athena-console-web`
  bundles the SPA successfully; the native console-server build succeeds.
  `Dockerfile.console` uses Rust 1.89.0 on Debian Bookworm: the pinned panel-kit
  dependency `serde-saphyr` requires 1.89, so the former 1.88 builder cannot
  compile this lockfile. The standard Docker release SPA and BFF build succeeds,
  and the resulting image starts with OIDC enabled as UID 65532 on a read-only
  filesystem; health, auth discovery and the SPA return HTTP 200.
  The separate Rust 1.96 verification shell still hits a Dioxus 0.6.3 wasm
  optimizer opcode incompatibility; no optimizer bypass or dependency downgrade
  is used by the production Docker recipe.
  Browser auth uses JavaScript `Date.now()` for epoch seconds instead of
  native-only `SystemTime`, which panics on wasm32. The production release
  bundle renders the enabled-OIDC Sign in control without a browser panic;
  the complete 70-test console package still passes.
- Nine live-browser checks and five read-interception fixture checks pass.
  Real resources cover drive evidence, campaign/experiment inspection,
  timestamp graph/rail disclosure, manifest/template loading, all read-only
  curator tabs, workload/node tables, filtering and saved views. Fixtures cover
  populated benchmark/inference data and empty/loading/error snapshots.
- Desktop (1440px) and compact (390px) screenshots were inspected; the compact
  document has no horizontal page overflow. Secondary experiment/campaign
  columns start hidden, remain available through the shared Columns menu, and
  existing user column choices remain persisted.
- Grafana variable/time-range URL construction is verified, but its external
  iframe remained on the loading surface. Live OIDC login and live report
  writes were not exercised. No Kubernetes resources or report specs were
  mutated for verification. Agent tool/model tracing remains the explicitly
  homeless story described above, not a claimed integration.

### Current-kit identity and viewport correction

The original achromatic override and fixed editor wrappers were regressions,
not panel-kit defaults. The correction restores pink/blue roles, the kit's
fine/coarse-pointer sizing, available-height editor/table/trace hosts, and
content-sized flow tables. Wide resource tables use the shared title-column
class so identity receives spare width; names wrap only when the space is
actually constrained.

With the canonical `321a7a4` pin, all 70 console package tests pass. Nineteen
direct browser checks cover pink role values, ordinary/maximized template and
manifest editor fill, tablet resize, coarse-pointer compact behavior, bounded
table and trace viewports, resource-name width, and the current keyboard ring.
The inspected 1440×1100 maximized manifest editor uses 996px and the maximized
template editor uses 549px, rather than a fixed 260px wrapper. These are observed
viewport examples, not new fixed dimensions.

Review evidence is manual source/browser inspection plus the mechanical layout
scan. The delegated reviewer returned an unsupported-provider notice instead
of findings, and the remaining stalled assessment was cancelled; no independent
review approval is claimed.
