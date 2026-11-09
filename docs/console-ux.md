# Athena Console UX

Last reviewed: 2026-11-09

This is the canonical story, panel-ownership and component inventory for the
console. Update it in place when adding or removing a panel. Kubernetes owns
research state; the console presents observations and edits ResearchReport
**specs**, never status. A panel needs a story, not merely an available widget.

## Application and dependency boundary

`Dockerfile.console:1-22` builds the Dioxus 0.6 SPA in
`operator/crates/athena-console-web`, then the feature-gated axum
`console-server`. The older Iced `athena-console` application remains the target
of the root flake's native console package/dev command. It does not consume
panel-kit and is not the deployed browser console; this change does not remove
that separate application or change its behavior.

The web package pins both panel-kit crates to
`ocasazza/panel-kit@72b0510a7b9dee1416d78555d87a313cc4127614` in its Cargo.toml.
**No dependency bump is required.** This revision already contains the current
web trace, chip, typed-table and shared panel-composition contracts used here.
The later upstream commits mainly extend Bevy/canary behavior, not this app's
required web APIs. Dioxus CLI must remain 0.6.x; wasm-bindgen CLI must match
0.2.121. No new dependency is introduced.

The panel-kit checkout has two remotes: `origin` is the olivecasazza fork,
`upstream` is ocasazza, the actual Cargo source. Both were fetched before the
inventory. The fork's `origin/main` ends at `8e4de76` (195 reachable commits);
canonical `upstream/main` ends at `321a7a4` (281 reachable commits). The complete
canonical log was read newest first through the initial `7940325` commit,
including merged branch histories, not just the latest release notes. Inspecting
only the fork would incorrectly recommend downgrading Athena and remove its
lift/swap tile-drag APIs.

## User stories and canonical homes

| ID | Persona and need | Panel / entry point | Shared components and acceptance |
| --- | --- | --- | --- |
| R1 | Researcher: identify a stalled drive and the template gate blocking promotion. | Research, global level | Literal phase/condition `Badge`s and typed `DataTable` per stage. Show objective, succeeded count, gate result, promotion timestamp and stagnation without inventing a gate decision. |
| R2 | Researcher: inspect a campaign, compare its runs and scope its metrics. | Research campaign breadcrumb → Experiment Detail / Metrics / Manifest / Run Activity | One `DataTable` campaign browser, one member-run table; selecting a campaign scopes Grafana by campaign and actual run window. Breadcrumb returns to the global fleet. |
| R3 | Researcher: compare experiments across campaigns, sort/filter by outcome and inspect one run. | Experiments view → canonical inspector | `DataTable` typed numeric/enum columns, search, column visibility/persistence, export, keyboard navigation, selected row. Inspector owns hypothesis, timestamps, decision, parent, workspace, links and optional filmstrip. |
| R4 | Run debugger: inspect observed creation/start/end milestones and their facts. | Run Activity after selecting a run/campaign | Shared `Trace` graph/query/inspector and alternate `TraceRail` for an individual experiment. Only present timestamps become events; unknown terminal outcomes are not success. No fabricated actors, tool calls, token counts or durations. |
| R5 | Researcher: inspect the selected resource's metrics and manifest without losing selection. | Experiment Metrics / Experiment Manifest | `GrafanaDashboard` scoped by experiment/campaign and start/end with buffer; `IdePanel` local YAML buffer. Editing this buffer does not deploy or write status. |
| R6 | Scientist: find and review a report, edit its narrative/hypotheses, curate included runs, preview and save without overwriting a concurrent version. | Reports → Report Curator; campaign report rows use the same destination | `DataTable` for reports, dataset, references and status conditions; `Badge` for literal phase/conditions. Existing Dataset/Narrative/Hypotheses/References/Preview/Status tabs, auth, dirty-draft protection, resourceVersion conflict handling and spec-only writes remain. |
| O1 | Operator: inspect templates, benchmark suites/runs and execution profiles. | Catalog view | `DataTable` for each list and `IdePanel` template YAML. No imperative deployment controls. |
| O2 | Operator: distinguish queue admission from node power and inference readiness. | Admin → GPU Pools & Queues / Node Power / Inference Backends | `DataTable` for workloads, nodes and endpoints; literal power/serving chips. Pool quota/usage summary remains separate from individual workload rows. Read-only scheduling API. |
| W1 | Any user: arrange/minimize/maximize panels, switch task views, reset or save a custom layout. | Research / Experiments / Reports / Catalog, dock and frame controls | Shared reducer/frame/dock/chrome and `workspace::panel_element`; host-owned named view persistence. The catalog change uses new v5 layout keys, so removed panels are not restored from older layouts. |

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
- `lib.rs:343,392-410,437-444`: pink row links, blue phases, rounded app
  chips/buttons and three ad-hoc condition treatments conflicted with the
  square shared chrome. Removed old `.tbl`, `.chip`, `.cond` renderers/styles;
  use shared badges, role tokens, square controls and explicit status text.
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
| `widgets::table::{table,bound_table}`, `src/widgets/table.rs:153` | Borrowed TableView or binding + TableModel and optional row-ID callback; adapts authored/provider table content to the unified table. | Current spec-content adapter, not a rival interactive table implementation. |
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

- `321a7a4` / `d3acf03`: named target-camera Bevy input/rendering; no required
  Dioxus migration after the console's existing 72b0510 pin.
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

## Verification contract

Build the deployed SPA with Dioxus CLI 0.6.3 (`dx build --platform web --release`)
and the native BFF with `cargo build -p athena-console-web --features server
--bin console-server`. Run the complete web package tests with `--features
server` and clippy for all its targets. Do not weaken or re-pin existing tests.

For browser verification, serve the actual built bundle with console-server;
intercept read API requests with clearly labeled fixture DTOs rather than
mutating Kubernetes or adding a durable fixture-state service. Exercise R1–W1,
empty/error/loading states, run selection and trace expansion, report tabs,
Catalog/Admin, named-view persistence, and a constrained viewport. Save screenshots
and text captures outside the repository; distinguish fixture rendering proof
from live OIDC/Grafana/Kubernetes integration. No claim about agent traces can
be made without their missing data contract.

### Cutover evidence and limits

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
