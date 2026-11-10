//! Report Curator: review, edit, and create `ResearchReport`s.
//!
//! User stories it serves, in the order a scientist meets them:
//!
//! 1. **Find the report for what I'm looking at.** The curator follows the
//!    global selection (the campaign selected in Research, or the
//!    selected experiment's campaign) and lists that campaign's reports.
//!    "Change" opens a campaign table to pick another one explicitly.
//! 2. **Review it.** Opening a report loads its full spec and the
//!    controller-observed status (phase, included count, dataset URI,
//!    conditions with messages).
//! 3. **Edit without losing anything.** Every section key the report has —
//!    including the drive's auto-authored Findings / Method / Footguns — is
//!    editable, sections can be added and removed, and fields the curator does
//!    not edit (references, `about`) pass through unchanged. Saves replace the
//!    spec against the loaded `resourceVersion`, so a concurrent change is a
//!    conflict, never a silent overwrite.
//! 4. **Curate the dataset.** The campaign's experiments with the universal
//!    columns and an include toggle, plus bulk actions (include all, exclude
//!    failed, keep only `Keep` decisions) and a live included count.
//! 5. **See the result before saving.** Preview composes the dossier from the
//!    unsaved draft.
//! 6. **Know it worked.** Save is enabled only for a valid, changed draft;
//!    the outcome and the refreshed status show in place.

use std::collections::{BTreeMap, BTreeSet};

use dioxus::prelude::*;
use panel_kit::widgets::DataColumnSpec;
use panel_kit::widgets::data_table::SortSpec;
use panel_kit_core::widgets::data_table::SortDir;

use crate::models::{ClusterSnapshot, ReportDetailDto, ReportSpecDto, ResourceSummary};
use crate::tables::{
    ConsoleTable, DataRow, SortKey, campaign_columns, campaign_row, experiment_cells,
    experiment_columns, fmt_ms,
};

/// Section names offered as one-click additions (the drive's write-up uses
/// the first four; the rest are common paper sections).
const SUGGESTED_SECTIONS: &[&str] = &[
    "Findings",
    "Method",
    "Footguns",
    "Limitations",
    "Discussion",
    "Related Work",
];

#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub enum Tab {
    #[default]
    Dataset,
    Narrative,
    Hypotheses,
    References,
    Preview,
    Status,
}

impl Tab {
    const ALL: [Tab; 6] = [
        Tab::Dataset,
        Tab::Narrative,
        Tab::Hypotheses,
        Tab::References,
        Tab::Preview,
        Tab::Status,
    ];

    fn label(self) -> &'static str {
        match self {
            Tab::Dataset => "Dataset",
            Tab::Narrative => "Narrative",
            Tab::Hypotheses => "Hypotheses",
            Tab::References => "References",
            Tab::Preview => "Preview",
            Tab::Status => "Status",
        }
    }
}

/// Editable report content. Compared against the loaded base for dirtiness.
#[derive(Clone, PartialEq, Default, serde::Serialize, serde::Deserialize)]
pub struct Draft {
    pub name: String,
    pub title: String,
    /// Ordered for editing; persisted as a map.
    pub sections: Vec<(String, String)>,
    pub seeded: String,
    pub excluded: BTreeSet<String>,
    pub included: Vec<String>,
    pub references: serde_json::Value,
    pub about: serde_json::Value,
}

impl Draft {
    fn from_detail(d: &ReportDetailDto) -> Self {
        Self {
            name: d.spec.name.clone(),
            title: d.spec.title.clone().unwrap_or_default(),
            sections: d.spec.sections.clone().into_iter().collect(),
            seeded: d.spec.seeded_hypotheses.join("\n"),
            excluded: d.spec.excluded_experiments.iter().cloned().collect(),
            included: d.spec.included_experiments.clone(),
            references: d.spec.references.clone(),
            about: d.spec.about.clone(),
        }
    }

    fn to_dto(&self, namespace: &str, campaign: &str, rv: Option<String>) -> ReportSpecDto {
        let title = self.title.trim();
        ReportSpecDto {
            namespace: namespace.to_string(),
            name: self.name.trim().to_string(),
            campaign_ref: campaign.to_string(),
            title: (!title.is_empty()).then(|| title.to_string()),
            included_experiments: self.included.clone(),
            excluded_experiments: self.excluded.iter().cloned().collect(),
            sections: self
                .sections
                .iter()
                .filter(|(k, v)| !k.trim().is_empty() && !v.trim().is_empty())
                .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
                .collect::<BTreeMap<_, _>>(),
            seeded_hypotheses: self
                .seeded
                .lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .map(String::from)
                .collect(),
            references: self.references.clone(),
            about: self.about.clone(),
            resource_version: rv,
        }
    }

    /// First problem that blocks saving, if any.
    fn problem(&self, is_new: bool) -> Option<String> {
        if is_new && !valid_name(self.name.trim()) {
            return Some(
                "name: lowercase letters, digits and '-', 1–63 chars, no leading/trailing '-'"
                    .into(),
            );
        }
        let mut seen = BTreeSet::new();
        for (k, v) in &self.sections {
            let k = k.trim();
            if k.is_empty() && !v.trim().is_empty() {
                return Some("a section with text needs a heading".into());
            }
            if !k.is_empty() && !seen.insert(k.to_lowercase()) {
                return Some(format!("duplicate section heading '{k}'"));
            }
        }
        None
    }
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 63
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !name.starts_with('-')
        && !name.ends_with('-')
}

#[derive(Clone, PartialEq, Default)]
pub enum SaveState {
    #[default]
    Idle,
    Busy(&'static str),
    Done(String),
    Failed(String),
}

/// All curator state, one signal owned by the App.
#[derive(Clone, PartialEq, Default)]
pub struct CuratorState {
    /// Explicit campaign pick; `None` follows the global selection.
    pub pinned_campaign: Option<String>,
    pub picking: bool,
    /// The report being edited; `None` = composing a new one.
    pub loaded: Option<ReportDetailDto>,
    pub base: Draft,
    pub draft: Draft,
    pub tab: Tab,
    pub preview: Option<Result<String, String>>,
    pub save: SaveState,
    /// Read-only conflict comparison; never replaces an unsaved draft itself.
    pub latest_saved: Option<ReportDetailDto>,
}

impl CuratorState {
    fn dirty(&self) -> bool {
        self.draft != self.base
    }

    fn start_new(&mut self) {
        self.loaded = None;
        self.base = Draft::default();
        self.draft = Draft::default();
        self.preview = None;
        self.save = SaveState::Idle;
        self.latest_saved = None;
        self.tab = Tab::Dataset;
    }

    fn open(&mut self, detail: ReportDetailDto) {
        self.pinned_campaign = Some(detail.spec.campaign_ref.clone());
        self.base = Draft::from_detail(&detail);
        self.draft = self.base.clone();
        self.loaded = Some(detail);
        self.preview = None;
        self.save = SaveState::Idle;
        self.latest_saved = None;
    }
}

/// Load a report into the curator (used by the Reports panel's Open action).
pub fn open_report(mut state: Signal<CuratorState>, namespace: String, name: String) {
    state.write().save = SaveState::Busy("loading…");
    spawn(async move {
        match crate::fetch_report_detail(&namespace, &name).await {
            Ok(detail) => state.write().open(detail),
            Err(e) => state.write().save = SaveState::Failed(format!("load failed: {e}")),
        }
    });
}

/// Campaign the curator is working on: the loaded report's campaign, else an
/// explicit pick, else the global selection.
fn effective_campaign(
    state: &CuratorState,
    selected: Option<&ResourceSummary>,
    nav_campaign: Option<&str>,
) -> Option<String> {
    if let Some(r) = &state.loaded {
        return Some(r.spec.campaign_ref.clone());
    }
    if let Some(c) = &state.pinned_campaign {
        return Some(c.clone());
    }
    match selected {
        Some(s) if s.kind == "researchcampaign" => return Some(s.name.clone()),
        Some(s) if s.campaign.is_some() => return s.campaign.clone(),
        _ => {}
    }
    nav_campaign.map(String::from)
}

/// Prepare the existing report draft model; never invent a review verdict or
/// overwrite another unsaved observation when the comparison changes.
fn observation_draft(
    candidate: &ResourceSummary,
    baseline: Option<&ResourceSummary>,
) -> Result<Draft, String> {
    let uid = candidate
        .uid
        .as_deref()
        .ok_or("Candidate UID is not reported; refresh before recording feedback.")?;
    if candidate.campaign.is_none() {
        return Err("An owning campaign is required to record a ResearchReport.".into());
    }
    if let Some(baseline) = baseline
        && (!crate::review::same_task(candidate, baseline) || baseline.uid.is_none())
    {
        return Err(
            "The reference must have a Kubernetes identity and matching declared task.".into(),
        );
    }
    let reference = baseline
        .and_then(|b| b.uid.as_deref())
        .unwrap_or("standalone");
    let name = format!(
        "review-{uid}-{}",
        reference.chars().take(8).collect::<String>()
    );
    let mut context = String::from(
        "Advisory research observation. This report does not approve, block, promote, roll back or cancel execution. Agent consumption is not established by saving it.\n",
    );
    let mut references = Vec::new();
    for (key, run) in
        std::iter::once(("candidate", candidate)).chain(baseline.map(|b| ("reference", b)))
    {
        use std::fmt::Write as _;
        let _ = writeln!(
            context,
            "\n{key}: {}/{} [@{key}]\nUID: {}\nresourceVersion: {}\nTask: {} / {} / {}\nRuntime image: {}",
            run.namespace,
            run.name,
            run.uid.as_deref().unwrap_or("not reported"),
            run.resource_version.as_deref().unwrap_or("not reported"),
            run.template.as_deref().unwrap_or("not reported"),
            run.robot.as_deref().unwrap_or("not reported"),
            run.mode.as_deref().unwrap_or("not reported"),
            run.image.as_deref().unwrap_or("not reported")
        );
        references.push(serde_json::json!({
            "key": key,
            "title": format!("{key}: {}/{}", run.namespace, run.name),
            "url": run.manifest_path(),
            "supports": format!("Object UID {}, resourceVersion {}", run.uid.as_deref().unwrap_or("not reported"), run.resource_version.as_deref().unwrap_or("not reported"))
        }));
        for (kind, url) in [
            ("evaluation", &run.figures_url),
            ("policy", &run.policy_url),
        ] {
            if let Some(url) = url {
                let _ = writeln!(context, "{kind} evidence: {url} [@{key}-{kind}]");
                references.push(serde_json::json!({
                    "key": format!("{key}-{kind}"),
                    "title": format!("{key} {kind} evidence"),
                    "url": url,
                    "supports": "Evidence associated with this advisory observation"
                }));
            }
        }
    }
    if baseline.is_none() {
        context.push_str("\nNo comparison baseline was available or selected; this is a standalone observation.\n");
    }
    Ok(Draft {
        name,
        title: format!("Advisory observation: {}", candidate.name),
        sections: vec![
            ("Observation".into(), String::new()),
            ("Review context".into(), context),
        ],
        included: vec![candidate.name.clone()],
        about: serde_json::json!({"kind":"Experiment","name":candidate.name,"motivation":"Assessing"}),
        references: serde_json::Value::Array(references),
        ..Default::default()
    })
}

fn merge_observation(
    ours: &Draft,
    latest: &ReportDetailDto,
    namespace: &str,
    campaign: &str,
) -> Result<Draft, &'static str> {
    let context = ours
        .sections
        .iter()
        .find(|(key, _)| key == "Review context")
        .map(|(_, text)| text);
    if latest.spec.namespace != namespace
        || latest.spec.campaign_ref != campaign
        || latest.spec.name != ours.name
        || latest.spec.about != ours.about
        || latest.spec.references != ours.references
        || latest.spec.sections.get("Review context") != context
    {
        return Err(
            "The saved report's subject or evidence changed. Preserve this observation as a separate report instead of overwriting that context.",
        );
    }
    let observation = ours
        .sections
        .iter()
        .find(|(key, _)| key == "Observation")
        .map(|(_, text)| text.clone())
        .ok_or("The draft has no observation.")?;
    let mut merged = Draft::from_detail(latest);
    if let Some((_, text)) = merged
        .sections
        .iter_mut()
        .find(|(key, _)| key == "Observation")
    {
        *text = observation;
    } else {
        merged.sections.push(("Observation".into(), observation));
    }
    Ok(merged)
}

fn observation_recovery(
    mut state: Signal<CuratorState>,
    st: &CuratorState,
    namespace: &str,
    campaign: &str,
) -> Element {
    let SaveState::Failed(message) = &st.save else {
        return rsx! {};
    };
    let namespace = namespace.to_string();
    let compare_namespace = namespace.clone();
    let campaign = campaign.to_string();
    let name = st.draft.name.clone();
    let original_error = message.clone();
    rsx! {
        div { class: "review-recovery",
            p { "Your observation is preserved. Compare with the saved report before replacing a concurrent version, or give this draft a new report name." }
            button {
                class: "btn",
                onclick: move |_| {
                    let namespace = compare_namespace.clone();
                    let name = name.clone();
                    let original_error = original_error.clone();
                    state.write().save = SaveState::Busy("loading saved report…");
                    spawn(async move {
                        match crate::fetch_report_detail(&namespace, &name).await {
                            Ok(detail) => {
                                let mut st = state.write();
                                st.latest_saved = Some(detail);
                                st.save = SaveState::Failed(original_error);
                            }
                            Err(error) => state.write().save = SaveState::Failed(format!("Saved report could not be loaded: {error}")),
                        }
                    });
                },
                "Compare with saved report"
            }
            button {
                class: "btn",
                onclick: move |_| {
                    let mut st = state.write();
                    st.loaded = None;
                    st.base = Draft::default();
                    st.draft.name.clear();
                    st.latest_saved = None;
                    st.save = SaveState::Idle;
                },
                "Save draft as a new report"
            }
            if let Some(latest) = &st.latest_saved {
                p { "Saved {latest.spec.namespace}/{latest.spec.name} · version {latest.spec.resource_version.as_deref().unwrap_or(\"not reported\")}" }
                pre { class: "preview", "{latest.spec.sections.get(\"Observation\").map(String::as_str).unwrap_or(\"No saved observation section\")}" }
                p { "Keeping your observation replaces only the Observation section on the next save. Other fields from the latest report are retained." }
                button {
                    class: "btn",
                    onclick: move |_| {
                        let mut st = state.write();
                        let Some(latest) = st.latest_saved.as_ref() else { return; };
                        let merged = match merge_observation(&st.draft, latest, &namespace, &campaign) {
                            Ok(merged) => merged,
                            Err(reason) => { st.save = SaveState::Failed(reason.into()); return; }
                        };
                        if let Some(latest) = st.latest_saved.take() {
                            st.open(latest);
                            st.draft = merged;
                        }
                    },
                    "Keep my observation; use latest revision"
                }
            }
        }
    }
}

pub(crate) fn review_feedback_view(
    mut state: Signal<CuratorState>,
    mut subject: Signal<Option<String>>,
    candidate: &ResourceSummary,
    baseline: Option<&ResourceSummary>,
    on_saved: EventHandler<()>,
    auth: &crate::auth_flow::Phase,
) -> Element {
    let key = format!(
        "{}|{}",
        crate::review::identity(candidate),
        baseline.map(crate::review::identity).unwrap_or_default()
    );
    let st = state.read().clone();
    let active = subject.read().as_deref() == Some(key.as_str());
    if !active {
        if st.dirty() || matches!(st.save, SaveState::Busy(_)) {
            return rsx! {
                section { class: "review-feedback",
                    h3 { "Unsaved advisory observation" }
                    p { "The draft for “{st.draft.title}” is preserved. Return to its candidate/reference to finish it, or explicitly discard it before starting another." }
                    details { summary { "Draft context" }
                        for (heading, text) in &st.draft.sections {
                            p { "{heading}: {text}" }
                        }
                    }
                    button {
                        class: "btn", disabled: matches!(st.save, SaveState::Busy(_)),
                        onclick: move |_| { state.write().start_new(); subject.set(None); },
                        "Discard unsaved observation"
                    }
                }
            };
        }
        let draft = match observation_draft(candidate, baseline) {
            Ok(draft) => draft,
            Err(reason) => return rsx! { p { class: "review-notice", "{reason}" } },
        };
        let campaign_name = candidate.campaign.clone();
        return rsx! {
            section { class: "review-feedback",
                h3 { "Advisory feedback" }
                p { "Record what you observed in a ResearchReport. Research keeps running; saving is not proof that an agent considered the finding." }
                button {
                    class: "btn",
                    onclick: move |_| {
                        let mut st = state.write();
                        st.start_new();
                        st.pinned_campaign = campaign_name.clone();
                        st.draft = draft.clone();
                        subject.set(Some(key.clone()));
                    },
                    "Write an observation"
                }
            }
        };
    }
    let observation = st
        .draft
        .sections
        .iter()
        .find(|(k, _)| k == "Observation")
        .map(|(_, text)| text.clone())
        .unwrap_or_default();
    let problem = observation
        .trim()
        .is_empty()
        .then(|| "Write an observation before saving.".to_string());
    let Some(campaign) = candidate.campaign.as_deref() else {
        return rsx! { p { class: "review-notice", "The owning campaign is not available." } };
    };
    rsx! {
        section { class: "review-feedback",
            h3 { "Advisory observation" }
            label { r#for: "review-observation", "What improved, regressed, or deserves investigation?" }
            textarea {
                id: "review-observation", class: "rc-textarea",
                value: observation,
                oninput: move |event| {
                    if let Some((_, text)) = state.write().draft.sections.iter_mut().find(|(k, _)| k == "Observation") {
                        *text = event.value();
                    }
                },
            }
            p { class: "muted", "Candidate, reference, object versions and evidence links are captured in the report. This is not an execution gate." }
            {footer(state, &st, &candidate.namespace, campaign, on_saved, auth, problem)}
            {observation_recovery(state, &st, &candidate.namespace, campaign)}
            if matches!(st.save, SaveState::Done(_)) && !st.dirty() {
                p { class: "muted", "Saved as Kubernetes research memory. No agent-consumption acknowledgment or execution change is implied." }
            }
        }
    }
}

pub fn curator_view(
    snap: ClusterSnapshot,
    mut state: Signal<CuratorState>,
    selected: Signal<Option<ResourceSummary>>,
    nav_campaign: Option<String>,
    on_saved: EventHandler<()>,
    auth: crate::auth_flow::Phase,
) -> Element {
    let st = state.read().clone();
    let campaign_name = effective_campaign(&st, selected.read().as_ref(), nav_campaign.as_deref());
    let campaign = campaign_name
        .as_ref()
        .and_then(|n| snap.campaigns.iter().find(|c| &c.name == n))
        .cloned();
    let following = st.loaded.is_none() && st.pinned_campaign.is_none();
    let dirty = st.dirty();

    // --- subject bar -------------------------------------------------------
    let subject = rsx! {
        div { class: "cur-subject",
            span { class: "cur-label", "Campaign" }
            match &campaign {
                Some(c) => rsx! {
                    span { class: "cur-value", title: "{c.name}", "{crate::campaign_label(c)}" }
                    span { class: "muted", " {fmt_ms(&c.created_at)}" }
                },
                None => rsx! { span { class: "muted", "none selected" } },
            }
            if following {
                span { class: "muted cur-hint", "follows selection" }
            }
            button {
                class: "btn",
                disabled: dirty,
                title: if dirty { "save or revert first" } else { "pick a campaign" },
                onclick: move |_| {
                    let mut s = state.write();
                    s.picking = !s.picking;
                },
                if st.picking { "close" } else { "change" }
            }
            if !following && !dirty {
                button {
                    class: "btn",
                    title: "follow the campaign selected elsewhere",
                    onclick: move |_| {
                        let mut s = state.write();
                        s.pinned_campaign = None;
                        s.start_new();
                    },
                    "follow selection"
                }
            }
        }
    };

    if st.picking {
        let rows: Vec<DataRow> = snap
            .campaigns
            .iter()
            .map(|c| {
                let name = c.name.clone();
                let label = crate::campaign_label(c);
                let shown = label.clone();
                campaign_row(
                    c,
                    rsx! { span { title: "{c.name}",
                        button {
                            class: "row-link",
                            onclick: move |_| {
                                let mut s = state.write();
                                s.pinned_campaign = Some(name.clone());
                                s.picking = false;
                                s.start_new();
                            },
                            "{shown}"
                        }
                    } },
                    &label,
                )
            })
            .collect();
        return rsx! {
            {subject}
            ConsoleTable {
                id: "athena-curator-campaigns",
                columns: campaign_columns(),
                rows,
                sort: SortSpec::new("created", SortDir::Desc),
                storage_key: "athena.table.curator.campaigns".to_string(),
                placeholder: "filter campaigns…".to_string(),
            }
        };
    }

    let Some(campaign) = campaign else {
        return rsx! {
            {subject}
            p { class: "muted", "Select a campaign in Research, or use change." }
        };
    };

    // --- report chooser ------------------------------------------------------
    let reports: Vec<_> = snap
        .reports
        .iter()
        .filter(|r| r.campaign_ref == campaign.name)
        .cloned()
        .collect();
    let loaded_name = st.loaded.as_ref().map(|r| r.spec.name.clone());
    let chooser = rsx! {
        div { class: "cur-reports",
            span { class: "cur-label", "Report" }
            for r in reports {
                {
                    let active = loaded_name.as_deref() == Some(r.name.as_str());
                    let ns = r.namespace.clone();
                    let name = r.name.clone();
                    let label = if r.title.is_empty() { r.name.clone() } else { r.title.clone() };
                    rsx! {
                        button {
                            key: "{r.name}",
                            class: if active { "btn cur-report active" } else { "btn cur-report" },
                            title: "{r.name} · {r.phase}",
                            disabled: dirty && !active,
                            onclick: move |_| open_report(state, ns.clone(), name.clone()),
                            "{label}"
                        }
                    }
                }
            }
            button {
                class: if loaded_name.is_none() { "btn cur-report active" } else { "btn cur-report" },
                disabled: dirty && loaded_name.is_some(),
                onclick: move |_| state.write().start_new(),
                "+ new"
            }
        }
    };

    // --- tabs ----------------------------------------------------------------
    let tabs = rsx! {
        div { class: "cur-tabs", role: "tablist",
            for t in Tab::ALL {
                button {
                    key: "{t.label()}",
                    role: "tab",
                    class: if st.tab == t { "cur-tab active" } else { "cur-tab" },
                    aria_selected: "{st.tab == t}",
                    onclick: move |_| state.write().tab = t,
                    "{t.label()}"
                    {tab_badge(t, &st)}
                }
            }
        }
    };

    let exps: Vec<ResourceSummary> = snap
        .experiments
        .iter()
        .filter(|e| e.campaign.as_deref() == Some(campaign.name.as_str()))
        .cloned()
        .collect();

    let body = match st.tab {
        Tab::Dataset => dataset_tab(state, &st, exps),
        Tab::Narrative => narrative_tab(state, &st),
        Tab::Hypotheses => hypotheses_tab(state, &st),
        Tab::References => references_tab(&st),
        Tab::Preview => preview_tab(state, &st, &campaign),
        Tab::Status => status_tab(&st),
    };

    rsx! {
        div { class: "cur",
            {subject}
            {chooser}
            {tabs}
            div { class: "cur-body", {body} }
            {footer(state, &st, &campaign.namespace, &campaign.name, on_saved, &auth, None)}
        }
    }
}

fn tab_badge(t: Tab, st: &CuratorState) -> Element {
    let n = match t {
        Tab::Narrative => st.draft.sections.len(),
        Tab::Hypotheses => st
            .draft
            .seeded
            .lines()
            .filter(|l| !l.trim().is_empty())
            .count(),
        Tab::References => st.draft.references.as_array().map_or(0, Vec::len),
        Tab::Dataset => st.draft.excluded.len(),
        _ => 0,
    };
    if n == 0 {
        return rsx! {};
    }
    let text = if t == Tab::Dataset {
        format!("−{n}")
    } else {
        n.to_string()
    };
    rsx! { span { class: "cur-count", "{text}" } }
}

fn dataset_tab(
    mut state: Signal<CuratorState>,
    st: &CuratorState,
    exps: Vec<ResourceSummary>,
) -> Element {
    let total = exps.len();
    let included = exps
        .iter()
        .filter(|e| !st.draft.excluded.contains(&e.name))
        .count();
    let failed: Vec<String> = exps
        .iter()
        .filter(|e| e.phase == "Failed")
        .map(|e| e.name.clone())
        .collect();
    let not_kept: Vec<String> = exps
        .iter()
        .filter(|e| e.decision.as_deref() != Some("Keep"))
        .map(|e| e.name.clone())
        .collect();

    let mut columns = vec![DataColumnSpec::new("include", "In").pinned().unsorted()];
    columns.extend(experiment_columns(false));
    let rows: Vec<DataRow> = exps
        .iter()
        .map(|e| {
            let name = e.name.clone();
            let on = !st.draft.excluded.contains(&e.name);
            let lead = DataRow::new(e.manifest_path()).cell(
                rsx! { span {
                    input {
                        r#type: "checkbox",
                        checked: on,
                        aria_label: "include {name}",
                        onchange: move |_| {
                            let mut s = state.write();
                            if !s.draft.excluded.remove(&name) {
                                s.draft.excluded.insert(name.clone());
                            }
                        },
                    }
                } },
                SortKey::num(if on { 1.0 } else { 0.0 }),
            );
            let shown = e.name.clone();
            experiment_cells(lead, e, rsx! { span { "{shown}" } }, false)
        })
        .collect();

    rsx! {
        div { class: "cur-bulk",
            span { class: "cur-value", "{included} / {total} included" }
            button { class: "btn", onclick: move |_| state.write().draft.excluded.clear(), "include all" }
            button {
                class: "btn",
                disabled: failed.is_empty(),
                onclick: move |_| state.write().draft.excluded.extend(failed.iter().cloned()),
                "exclude failed"
            }
            button {
                class: "btn",
                title: "exclude everything without a Keep decision",
                onclick: move |_| state.write().draft.excluded.extend(not_kept.iter().cloned()),
                "only Keep"
            }
        }
        ConsoleTable {
            id: "athena-curator-dataset",
            columns,
            rows,
            sort: SortSpec::new("created", SortDir::Desc),
            storage_key: "athena.table.curator.dataset".to_string(),
            empty: "No experiments in this campaign.".to_string(),
            placeholder: "filter experiments…".to_string(),
        }
    }
}

fn narrative_tab(mut state: Signal<CuratorState>, st: &CuratorState) -> Element {
    let present: BTreeSet<String> = st
        .draft
        .sections
        .iter()
        .map(|(k, _)| k.trim().to_lowercase())
        .collect();
    let missing: Vec<&'static str> = SUGGESTED_SECTIONS
        .iter()
        .copied()
        .filter(|s| !present.contains(&s.to_lowercase()))
        .collect();
    rsx! {
        for (i, (key, text)) in st.draft.sections.iter().enumerate() {
            div { class: "cur-section", key: "{i}",
                div { class: "cur-section-head",
                    input {
                        class: "rc-input cur-heading",
                        value: "{key}",
                        placeholder: "Section heading",
                        oninput: move |e| {
                            if let Some(s) = state.write().draft.sections.get_mut(i) {
                                s.0 = e.value();
                            }
                        },
                    }
                    button {
                        class: "btn",
                        title: "remove section",
                        onclick: move |_| {
                            let mut s = state.write();
                            if i < s.draft.sections.len() {
                                s.draft.sections.remove(i);
                            }
                        },
                        "remove"
                    }
                }
                textarea {
                    class: "rc-textarea cur-text",
                    value: "{text}",
                    oninput: move |e| {
                        if let Some(s) = state.write().draft.sections.get_mut(i) {
                            s.1 = e.value();
                        }
                    },
                }
            }
        }
        div { class: "cur-add",
            span { class: "cur-label", "Add" }
            for name in missing {
                button {
                    key: "{name}",
                    class: "btn",
                    onclick: move |_| state.write().draft.sections.push((name.to_string(), String::new())),
                    "{name}"
                }
            }
            button {
                class: "btn",
                onclick: move |_| state.write().draft.sections.push((String::new(), String::new())),
                "custom…"
            }
        }
    }
}

fn hypotheses_tab(mut state: Signal<CuratorState>, st: &CuratorState) -> Element {
    rsx! {
        p { class: "muted", "One per line. Recorded as future work in the dossier; this does not launch experiments." }
        textarea {
            class: "rc-textarea cur-text tall",
            value: "{st.draft.seeded}",
            oninput: move |e| state.write().draft.seeded = e.value(),
        }
    }
}

fn references_tab(st: &CuratorState) -> Element {
    let refs = st.draft.references.as_array().cloned().unwrap_or_default();
    let field = |r: &serde_json::Value, k: &str| {
        r.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string()
    };
    let rows = refs
        .iter()
        .enumerate()
        .map(|(index, r)| {
            let key = field(r, "key");
            let url = field(r, "url");
            let link = if url.is_empty() { field(r, "doi") } else { url };
            DataRow::new(format!("{key}-{index}"))
                .text(format!("[@{key}]"))
                .text(field(r, "title"))
                .text(link)
                .text(field(r, "supports"))
        })
        .collect::<Vec<_>>();
    rsx! {
        p { class: "muted", "Read-only here; kept unchanged on save. Cite in section text as [@key]." }
        ConsoleTable {
            id: "athena-report-references",
            columns: vec![
                DataColumnSpec::new("key", "Key").pinned(),
                DataColumnSpec::new("title", "Title"),
                DataColumnSpec::new("link", "Link"),
                DataColumnSpec::new("supports", "Supports"),
            ],
            rows, sort: SortSpec::new("key", SortDir::Asc),
            empty: "No references; define references in the manifest.".to_string(),
        }
    }
}

fn preview_tab(
    mut state: Signal<CuratorState>,
    st: &CuratorState,
    campaign: &ResourceSummary,
) -> Element {
    let dto = st.draft.to_dto(&campaign.namespace, &campaign.name, None);
    let run = move |_| {
        let dto = dto.clone();
        state.write().preview = None;
        spawn(async move {
            let result = crate::preview_report(dto).await;
            state.write().preview = Some(result);
        });
    };
    rsx! {
        div { class: "cur-bulk",
            button { class: "btn", onclick: run, "compose from draft" }
            span { class: "muted", "Built from the unsaved draft; nothing is written." }
        }
        match &st.preview {
            None => rsx! { p { class: "muted", "Not composed yet." } },
            Some(Ok(md)) => rsx! { pre { class: "preview", "{md}" } },
            Some(Err(e)) => rsx! { p { class: "err", "Preview failed: {e}" } },
        }
    }
}

fn status_tab(st: &CuratorState) -> Element {
    let Some(r) = &st.loaded else {
        return rsx! { p { class: "muted", "New report: the controller publishes status after it is saved." } };
    };
    let dash = "\u{2014}".to_string();
    rsx! {
        dl { class: "detail-grid",
            dt { "Name" } dd { "{r.spec.namespace}/{r.spec.name}" }
            dt { "Phase" } dd { {crate::metadata_chip("Phase", r.phase.as_deref().unwrap_or("—"))} }
            dt { "Included" } dd { {r.included_count.map(|n| n.to_string()).unwrap_or_else(|| dash.clone())} }
            dt { "Dataset" } dd { {r.dataset_uri.clone().unwrap_or_else(|| dash.clone())} }
            dt { "Assembled" } dd { {r.last_assembled_time.clone().unwrap_or_else(|| dash.clone())} }
            dt { "Created" } dd { "{fmt_ms(&r.created_at)}" }
        }
        ConsoleTable {
            id: "athena-report-conditions",
            columns: vec![
                DataColumnSpec::new("type", "Condition").pinned(),
                DataColumnSpec::new("status", "Status").enumerated(),
                DataColumnSpec::new("reason", "Reason"),
                DataColumnSpec::new("message", "Message"),
            ],
            rows: r.conditions.iter().map(|c| {
                DataRow::new(&c.ctype).text(&c.ctype)
                    .cell(crate::metadata_chip(&c.ctype, &c.status), SortKey::text(&c.status))
                    .text(&c.reason).text(&c.message)
            }).collect::<Vec<_>>(),
            sort: SortSpec::new("type", SortDir::Asc),
            empty: "No conditions reported.".to_string(),
        }
    }
}

fn footer(
    mut state: Signal<CuratorState>,
    st: &CuratorState,
    namespace: &str,
    campaign_name: &str,
    on_saved: EventHandler<()>,
    auth: &crate::auth_flow::Phase,
    additional_problem: Option<String>,
) -> Element {
    let is_new = st.loaded.is_none();
    let dirty = st.dirty();
    let busy = matches!(st.save, SaveState::Busy(_));
    // Writing a ResearchReport is the research record, so it needs a live
    // session; reads above are unaffected. The gate lives in `auth_flow` so
    // its rules are unit-tested rather than re-implemented here.
    let (can_save, problem) = crate::auth_flow::save_gate(
        auth,
        additional_problem.or_else(|| st.draft.problem(is_new)),
        dirty,
        busy,
    );
    let rv = st
        .loaded
        .as_ref()
        .and_then(|r| r.spec.resource_version.clone());
    let dto = st.draft.to_dto(namespace, campaign_name, rv);
    let save = move |_| {
        let dto = dto.clone();
        {
            let mut st = state.write();
            st.save = SaveState::Busy("saving…");
            st.latest_saved = None;
        }
        spawn(async move {
            let result = if dto.resource_version.is_some() {
                crate::update_report(dto).await
            } else {
                crate::create_report(dto).await
            };
            match result {
                Ok(detail) => {
                    let name = detail.spec.name.clone();
                    let mut s = state.write();
                    s.open(detail);
                    s.save = SaveState::Done(format!("saved {name}"));
                    drop(s);
                    on_saved.call(());
                }
                Err(e) => state.write().save = SaveState::Failed(e),
            }
        });
    };
    rsx! {
        div { class: "cur-footer",
            if is_new {
                input {
                    class: "rc-input cur-name",
                    placeholder: "report-name",
                    aria_label: "report name",
                    value: "{st.draft.name}",
                    oninput: move |e| state.write().draft.name = e.value(),
                }
            } else {
                span { class: "cur-value", "{st.draft.name}" }
            }
            input {
                class: "rc-input cur-title",
                placeholder: "Title (optional)",
                aria_label: "report title",
                value: "{st.draft.title}",
                oninput: move |e| state.write().draft.title = e.value(),
            }
            if dirty {
                span { class: "cur-dirty", "unsaved" }
            }
            button {
                class: "btn",
                disabled: !dirty || busy,
                onclick: move |_| {
                    let mut s = state.write();
                    s.draft = s.base.clone();
                },
                "revert"
            }
            button {
                class: "btn primary",
                disabled: !can_save,
                title: problem.clone().unwrap_or_default(),
                onclick: save,
                if matches!(st.save, SaveState::Failed(_)) { "Retry save" } else if is_new { "create" } else { "save" }
            }
            match (&st.save, &problem) {
                (_, Some(p)) if dirty => rsx! { span { class: "err", "{p}" } },
                (SaveState::Busy(m), _) => rsx! { span { class: "muted", "{m}" } },
                (SaveState::Done(m), _) if !dirty => rsx! { span { class: "ok", "{m}" } },
                (SaveState::Failed(m), _) => rsx! { span { class: "err", "{m}" } },
                _ => rsx! {},
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn detail() -> ReportDetailDto {
        let mut d = ReportDetailDto::default();
        d.spec.name = "r".into();
        d.spec.sections = [("Findings".to_string(), "x".to_string())].into();
        d.spec.references = serde_json::json!([{ "key": "a", "title": "A" }]);
        d.spec.resource_version = Some("7".into());
        d
    }

    #[test]
    fn round_trip_preserves_uneditable_fields_and_sections() {
        let d = detail();
        let dto = Draft::from_detail(&d).to_dto("ns", "camp", d.spec.resource_version.clone());
        assert_eq!(dto.sections.get("Findings").map(String::as_str), Some("x"));
        assert_eq!(dto.references, d.spec.references);
        assert_eq!(dto.resource_version.as_deref(), Some("7"));
    }

    #[test]
    fn empty_sections_are_dropped_and_problems_block_saving() {
        let mut draft = Draft::from_detail(&detail());
        draft.sections.push(("Method".into(), "  ".into()));
        assert!(
            !draft
                .to_dto("ns", "c", None)
                .sections
                .contains_key("Method")
        );
        draft.sections.push(("findings".into(), "dup".into()));
        assert!(draft.problem(false).unwrap().contains("duplicate"));
        let mut new = Draft::default();
        new.name = "Bad_Name".into();
        assert!(new.problem(true).is_some());
        new.name = "good-name".into();
        assert!(new.problem(true).is_none());
    }

    #[test]
    fn opening_a_report_is_clean_until_edited() {
        let mut st = CuratorState::default();
        st.open(detail());
        assert!(!st.dirty());
        st.draft.excluded.insert("e1".into());
        assert!(st.dirty());
    }
}

#[cfg(test)]
mod observation_tests {
    use super::*;

    #[test]
    fn observations_refuse_unknown_identity_and_cross_task_references() {
        let mut candidate = ResourceSummary {
            name: "candidate".into(),
            namespace: "apps".into(),
            kind: "experiment".into(),
            campaign: Some("campaign".into()),
            template: Some("task".into()),
            ..Default::default()
        };
        assert!(observation_draft(&candidate, None).is_err());
        candidate.uid = Some("candidate-uid".into());
        let mut reference = candidate.clone();
        reference.name = "reference".into();
        reference.uid = Some("reference-uid".into());
        reference.namespace = "another-tenant".into();
        assert!(observation_draft(&candidate, Some(&reference)).is_err());
        reference.namespace = "apps".into();
        reference.template = Some("different-task".into());
        assert!(observation_draft(&candidate, Some(&reference)).is_err());
    }
}

#[cfg(test)]
mod observation_merge_tests {
    use super::*;

    #[test]
    fn explicit_conflict_resolution_preserves_latest_unrelated_work_and_rejects_new_evidence() {
        let latest = ReportDetailDto {
            spec: ReportSpecDto {
                namespace: "apps".into(),
                name: "review".into(),
                campaign_ref: "campaign".into(),
                sections: BTreeMap::from([
                    (
                        "Observation".into(),
                        "another reviewer's observation".into(),
                    ),
                    (
                        "Review context".into(),
                        "fixed candidate and baseline identity".into(),
                    ),
                    ("New finding".into(), "concurrent work must survive".into()),
                ]),
                resource_version: Some("new-version".into()),
                ..Default::default()
            },
            ..Default::default()
        };
        let mut ours = Draft::from_detail(&latest);
        ours.sections.retain(|(key, _)| key != "New finding");
        ours.sections
            .iter_mut()
            .find(|(key, _)| key == "Observation")
            .unwrap()
            .1 = "my unsaved observation".into();
        let merged = merge_observation(&ours, &latest, "apps", "campaign").unwrap();
        assert_eq!(
            merged
                .sections
                .iter()
                .find(|(key, _)| key == "New finding")
                .unwrap()
                .1,
            "concurrent work must survive"
        );
        assert_eq!(
            merged
                .sections
                .iter()
                .find(|(key, _)| key == "Observation")
                .unwrap()
                .1,
            "my unsaved observation"
        );
        let mut changed = latest.clone();
        changed
            .spec
            .sections
            .insert("Review context".into(), "a different baseline".into());
        assert!(merge_observation(&ours, &changed, "apps", "campaign").is_err());
        assert!(merge_observation(&ours, &latest, "other-namespace", "campaign").is_err());
    }
}
