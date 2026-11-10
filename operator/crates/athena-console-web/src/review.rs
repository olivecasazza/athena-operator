//! Review is a presentation of Kubernetes evidence, not another execution gate.

use dioxus::prelude::*;
use panel_kit::widgets::DataColumnSpec;
use panel_kit::widgets::data_table::{Control, SortSpec};
use panel_kit_core::widgets::data_table::SortDir;

use crate::models::{ClusterSnapshot, ResourceSummary};
use crate::tables::{ConsoleTable, DataRow, SortKey, fmt_ms};
use crate::{auth_flow, curator};

/// Tab-scoped recovery only. This is not review completion or research memory.
#[derive(Default, serde::Deserialize)]
#[serde(default)]
pub(crate) struct PendingObservation {
    pub selected: Option<ResourceSummary>,
    pub reference: Option<(String, String)>,
    pub subject: Option<String>,
    pub loaded: Option<crate::models::ReportDetailDto>,
    pub pinned_campaign: Option<String>,
    pub base: curator::Draft,
    pub draft: curator::Draft,
}

pub(crate) fn restore_pending() -> PendingObservation {
    #[cfg(target_arch = "wasm32")]
    {
        let value = web_sys::window()
            .and_then(|w| w.session_storage().ok().flatten())
            .and_then(|s| s.get_item("athena.review.pending.v1").ok().flatten());
        if let Some(value) = value {
            match serde_json::from_str(&value) {
                Ok(pending) => return pending,
                Err(_) => {
                    web_sys::console::warn_1(&"Could not restore this tab's advisory draft.".into())
                }
            }
        }
    }
    PendingObservation::default()
}

pub(crate) fn preserve_pending(
    state: &curator::CuratorState,
    selected: Option<&ResourceSummary>,
    reference: Option<&(String, String)>,
    subject: Option<&str>,
    snapshot: Option<&ClusterSnapshot>,
) {
    #[cfg(target_arch = "wasm32")]
    {
        #[derive(serde::Serialize)]
        struct Pending<'a> {
            selected: Option<&'a ResourceSummary>,
            reference: Option<(&'a str, &'a str)>,
            subject: Option<&'a str>,
            loaded: Option<&'a crate::models::ReportDetailDto>,
            pinned_campaign: Option<&'a str>,
            base: &'a curator::Draft,
            draft: &'a curator::Draft,
        }
        let Some(storage) = web_sys::window().and_then(|w| w.session_storage().ok().flatten())
        else {
            web_sys::console::warn_1(&"Advisory draft recovery is unavailable in this tab.".into());
            return;
        };
        let result = if state.draft == state.base {
            storage.remove_item("athena.review.pending.v1")
        } else {
            let intent = subject.and_then(|key| key.split_once('|'));
            let selected = snapshot
                .and_then(|snap| {
                    let (uid, _) = intent?;
                    snap.experiments
                        .iter()
                        .find(|run| run.uid.as_deref() == Some(uid))
                })
                .or(selected);
            // Freeze the reference that the observation actually describes.
            // A later incumbent update must not rewrite the recovered context.
            let reference = reference
                .filter(|(uid, _)| intent.is_some_and(|(subject, _)| subject == uid.as_str()))
                .map(|(uid, target)| (uid.as_str(), target.as_str()))
                .or_else(|| {
                    intent.map(|(uid, target)| {
                        (
                            uid,
                            if target.is_empty() {
                                "standalone"
                            } else {
                                target
                            },
                        )
                    })
                });
            let pending = Pending {
                selected,
                reference,
                subject,
                loaded: state.loaded.as_ref(),
                pinned_campaign: state.pinned_campaign.as_deref(),
                base: &state.base,
                draft: &state.draft,
            };
            match serde_json::to_string(&pending) {
                Ok(value) => storage.set_item("athena.review.pending.v1", &value),
                Err(_) => {
                    web_sys::console::warn_1(
                        &"Could not serialize this tab's advisory draft.".into(),
                    );
                    return;
                }
            }
        };
        if result.is_err() {
            web_sys::console::warn_1(&"Could not preserve this tab's advisory draft. Keep the tab open or copy it before signing in.".into());
        }
    }
    #[cfg(not(target_arch = "wasm32"))]
    let _ = (state, selected, reference, subject, snapshot);
}

pub(crate) fn use_pending_flush(
    state: Signal<curator::CuratorState>,
    selected: Signal<Option<ResourceSummary>>,
    reference: Signal<Option<(String, String)>>,
    subject: Signal<Option<String>>,
    snapshot: Resource<Result<ClusterSnapshot, String>>,
) {
    #[cfg(target_arch = "wasm32")]
    {
        use wasm_bindgen::JsCast;
        let callback = use_hook(move || {
            std::rc::Rc::new(wasm_bindgen::closure::Closure::<dyn FnMut()>::new(
                move || {
                    let snapshot = snapshot.peek();
                    preserve_pending(
                        &state.peek(),
                        selected.peek().as_ref(),
                        reference.peek().as_ref(),
                        subject.peek().as_deref(),
                        snapshot.as_ref().and_then(|value| value.as_ref().ok()),
                    );
                },
            ))
        });
        let installed = callback.clone();
        use_effect(move || {
            if let Some(window) = web_sys::window()
                && window
                    .add_event_listener_with_callback(
                        "beforeunload",
                        installed.as_ref().as_ref().unchecked_ref(),
                    )
                    .is_err()
            {
                web_sys::console::warn_1(
                    &"Could not install advisory draft recovery before navigation.".into(),
                );
            }
        });
        use_drop(move || {
            if let Some(window) = web_sys::window() {
                let _ = window.remove_event_listener_with_callback(
                    "beforeunload",
                    callback.as_ref().as_ref().unchecked_ref(),
                );
            }
        });
    }
    #[cfg(not(target_arch = "wasm32"))]
    let _ = (state, selected, reference, subject, snapshot);
}

pub(crate) fn identity(run: &ResourceSummary) -> String {
    run.uid.clone().unwrap_or_else(|| run.manifest_path())
}

/// Match declared task context, not names, scores, scheduling order or age.
/// Equal declarations are not proof of equal evaluation inputs; the UI says so.
pub(crate) fn same_task(a: &ResourceSummary, b: &ResourceSummary) -> bool {
    a.kind == "experiment"
        && b.kind == "experiment"
        && a.namespace == b.namespace
        && a.template.as_deref().is_some_and(|t| !t.is_empty())
        && a.template == b.template
        && a.mode == b.mode
        && a.robot == b.robot
        && a.objective == b.objective
        && a.objective_goal == b.objective_goal
        && a.benchmark_suite == b.benchmark_suite
}

pub(crate) fn candidate<'a>(
    snap: &'a ClusterSnapshot,
    selected: Option<&ResourceSummary>,
) -> Option<&'a ResourceSummary> {
    if let Some(selected) = selected.filter(|s| s.kind == "experiment") {
        return snap.experiments.iter().find(|e| {
            e.namespace == selected.namespace && e.name == selected.name && e.uid == selected.uid
        });
    }
    snap.experiments
        .iter()
        .filter(|e| {
            selected
                .filter(|s| s.kind == "researchcampaign")
                .is_none_or(|c| {
                    e.namespace == c.namespace && e.campaign.as_deref() == Some(c.name.as_str())
                })
        })
        .max_by(|a, b| {
            let at =
                |e: &ResourceSummary| e.created_at.as_deref().and_then(|s| s.parse::<i64>().ok());
            at(a)
                .cmp(&at(b))
                .then_with(|| a.namespace.cmp(&b.namespace))
                .then_with(|| a.name.cmp(&b.name))
        })
}

/// Only the controller's current incumbent is authoritative. In particular,
/// once the candidate itself wins, the old incumbent is not recoverable by
/// sorting old Discard decisions: the controller rewrites those decisions.
pub(crate) fn retained_baseline<'a>(
    snap: &'a ClusterSnapshot,
    run: &ResourceSummary,
) -> Result<&'a ResourceSummary, &'static str> {
    let campaign = snap
        .campaigns
        .iter()
        .find(|c| c.namespace == run.namespace && run.campaign.as_deref() == Some(c.name.as_str()))
        .ok_or("The owning campaign is not in this snapshot.")?;
    let name = campaign
        .best_experiment
        .as_deref()
        .ok_or("The campaign has not reported a retained incumbent.")?;
    if name == run.name {
        return Err(
            "This candidate is the current incumbent. Previous incumbent history is not recorded; choose an explicit reference below.",
        );
    }
    let baseline = snap
        .experiments
        .iter()
        .find(|e| e.namespace == run.namespace && e.name == name)
        .ok_or("The retained incumbent is outside this snapshot or no longer exists.")?;
    if baseline.decision.as_deref() != Some("Keep") {
        return Err(
            "The campaign reference and the experiment's retained decision do not agree. Refresh before treating it as retained best.",
        );
    }
    if !same_task(run, baseline) {
        return Err(
            "The campaign incumbent has different or missing declared task context; it is not selected as this task's baseline.",
        );
    }
    Ok(baseline)
}

/// Typed target, namespace, object age and observed generation must agree.
/// A result for a deleted same-named object or an older spec is not this run's
/// scientific evidence. The CRD target has no UID, so missing dates stay unknown.
pub(crate) fn benchmark_targets_run(check: &ResourceSummary, run: &ResourceSummary) -> bool {
    if !check
        .target_kind
        .as_deref()
        .is_some_and(|k| k.eq_ignore_ascii_case("Experiment"))
        || check.target_name.as_deref() != Some(run.name.as_str())
        || check
            .target_namespace
            .as_deref()
            .unwrap_or(&check.namespace)
            != run.namespace
        || check.object_generation.is_none()
        || check.object_generation != check.observed_generation
    {
        return false;
    }
    let at = |r: &ResourceSummary| r.created_at.as_deref().and_then(|s| s.parse::<i64>().ok());
    matches!((at(check), at(run)), (Some(check), Some(run)) if check >= run)
}

pub(crate) fn selected_baseline<'a>(
    snap: &'a ClusterSnapshot,
    run: &ResourceSummary,
    manual: Option<&str>,
) -> Result<&'a ResourceSummary, &'static str> {
    let Some(target) = manual else {
        return retained_baseline(snap, run);
    };
    snap.experiments.iter().find(|other| {
        other.uid.as_deref() == Some(target) && other.name != run.name && same_task(run, other)
    }).ok_or("The selected reference is unavailable or no longer matches this task. It has not been replaced with another baseline.")
}

fn benchmark_evidence(snap: &ClusterSnapshot, run: &ResourceSummary) -> Element {
    let mut checks = snap
        .benchmark_runs
        .iter()
        .filter(|c| benchmark_targets_run(c, run))
        .peekable();
    if checks.peek().is_none() {
        return rsx! {
            details {
                summary { "Scientific gates · unavailable" }
                p { class: "muted", "No current-generation benchmark results with verifiable target identity/time are in this snapshot. Retention and job completion do not establish that scientific gates passed." }
            }
        };
    }
    rsx! {
        div { class: "review-benchmarks",
            h4 { "Scientific gates" }
            for check in checks {
                details {
                    summary { "{check.name} · {check.phase} · {check.gates.len()} gate results" }
                    dl { class: "detail-grid",
                        dt { "Suite hash" } dd { "{check.suite_hash.as_deref().unwrap_or(\"Not reported\")}" }
                        dt { "Reproducibility hash" } dd { "{check.reproducibility_hash.as_deref().unwrap_or(\"Not reported\")}" }
                    }
                    p { class: "muted", "Hashes identify recorded evaluation inputs; this view does not independently prove evaluator determinism." }
                    ConsoleTable {
                        id: format!("review-gates-{}-{}", check.namespace, check.name),
                        columns: vec![
                            DataColumnSpec::new("metric", "Metric"),
                            DataColumnSpec::new("verdict", "Controller verdict").enumerated(),
                            DataColumnSpec::new("actual", "Actual").number(),
                            DataColumnSpec::new("threshold", "Threshold").number(),
                        ],
                        rows: check.gates.iter().enumerate().map(|(index, gate)| {
                            let verdict = if gate.passed { "Passed" } else { "Failed" };
                            DataRow::new(format!("{index}-{}", gate.metric)).text(&gate.metric)
                                .cell(crate::metadata_chip("Gate", verdict), SortKey::text(verdict))
                                .num(gate.actual, gate.actual.map_or_else(|| "Not reported".into(), |v| v.to_string()))
                                .num(Some(gate.threshold), gate.threshold.to_string())
                        }).collect::<Vec<_>>(),
                        sort: SortSpec::new("metric", SortDir::Asc),
                        toolbar: vec![Control::Tally],
                        empty: "No scientific gate results reported.".to_string(),
                    }
                }
            }
        }
    }
}

pub(crate) fn results_view(
    snap: ClusterSnapshot,
    mut selected: Signal<Option<ResourceSummary>>,
) -> Element {
    let chosen = candidate(&snap, selected.read().as_ref()).map(identity);
    let rows = snap.experiments.iter().map(|run| {
        let pick = run.clone();
        let key = identity(run);
        let phase = run.phase.clone();
        let decision = run.decision.clone().unwrap_or_else(|| "Not reported".into());
        DataRow::new(&key)
            .cell(rsx! {
                a {
                    class: "row-link", href: "#review-comparison-heading",
                    title: "{run.namespace}/{run.name}",
                    onclick: move |_| selected.set(Some(pick.clone())),
                    "{run.name}"
                }
                span { class: "review-row-context", "{run.namespace} · {run.template.as_deref().unwrap_or(\"task not reported\")}" }
            }, SortKey::text(&run.name))
            .cell(rsx! { crate::MetadataChip { field: "Execution", value: phase.clone(), labeled: false } }, SortKey::text(&phase))
            .cell(rsx! { crate::MetadataChip { field: "Decision", value: decision.clone(), labeled: false } }, SortKey::text(&decision))
            .num(run.created_at.as_deref().and_then(|s| s.parse::<f64>().ok()), fmt_ms(&run.created_at))
            .selected(chosen.as_deref() == Some(key.as_str()))
    }).collect::<Vec<_>>();
    rsx! {
        div { class: "view-head", id: "review-results-heading", tabindex: "-1",
            h2 { "Research results" }
            p { "Browse evidence, not a required-review queue. Research continues without a human verdict." }
        }
        ConsoleTable {
            id: "athena-review-results",
            columns: vec![
                DataColumnSpec::new("candidate", "Candidate").class("pk-dt-col-title").pinned(),
                DataColumnSpec::new("execution", "Execution").enumerated().width("12ch"),
                DataColumnSpec::new("decision", "Decision").enumerated().width("12ch").hidden(),
                DataColumnSpec::new("created", "Created").number().hidden(),
            ],
            rows,
            sort: SortSpec::new("created", SortDir::Desc),
            toolbar: vec![Control::Quick, Control::Columns, Control::Tally],
            storage_key: "athena.table.review.results".to_string(),
            placeholder: "find a candidate…".to_string(),
            empty: "No experiment evidence yet. Research and Catalog remain available to inspect campaigns and execution configuration.".to_string(),
        }
    }
}

#[component]
pub(crate) fn ReviewEvidence(
    snap: ClusterSnapshot,
    selected: Signal<Option<ResourceSummary>>,
    mut reference: Signal<Option<(String, String)>>,
    feedback: Signal<curator::CuratorState>,
    feedback_subject: Signal<Option<String>>,
    auth: auth_flow::Phase,
    on_saved: EventHandler<()>,
    on_inspect: EventHandler<ResourceSummary>,
    on_campaign: EventHandler<(String, String)>,
) -> Element {
    let mut zoom = use_signal(|| 1_u8);
    let Some(run) = candidate(&snap, selected.read().as_ref()) else {
        return rsx! {
            div { class: "view-head",
                h2 { "Review research evidence" }
                p { "Choose an experiment from Results. If a selected object was replaced or disappeared, choose it again rather than reviewing a different object silently." }
            }
        };
    };
    let run = run.clone();
    let key = identity(&run);
    let selected_reference = reference
        .read()
        .as_ref()
        .filter(|(subject, _)| subject == &key)
        .map(|(_, target)| target.clone())
        .unwrap_or_default();
    let is_standalone = selected_reference == "standalone";
    let is_manual = !selected_reference.is_empty() && !is_standalone;
    let resolution = if is_standalone {
        Ok(None)
    } else {
        selected_baseline(
            &snap,
            &run,
            is_manual.then_some(selected_reference.as_str()),
        )
        .map(Some)
    };
    let baseline = resolution.as_ref().ok().and_then(|value| value.cloned());
    let unavailable_reference = is_manual && baseline.is_none();
    let campaign = snap
        .campaigns
        .iter()
        .find(|c| c.namespace == run.namespace && run.campaign.as_deref() == Some(c.name.as_str()));
    let inspect = run.clone();
    let campaign_target = run
        .campaign
        .as_ref()
        .map(|c| (run.namespace.clone(), c.clone()));
    let task = [
        run.template.as_deref(),
        run.robot.as_deref(),
        run.mode.as_deref(),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(" · ");
    let alternatives = snap
        .experiments
        .iter()
        .filter(|e| e.uid.is_some() && same_task(&run, e) && e.name != run.name);
    let subject = key.clone();
    rsx! {
        div { class: "review-workflow",
            div { class: "review-heading",
                div {
                    h2 { id: "review-comparison-heading", tabindex: "-1", "Compare research evidence" }
                    p { class: "muted", "{task}" }
                }
                div { class: "review-actions",
                    a { class: "btn", href: "#review-results-heading", "Choose candidate" }
                    button { class: "btn", onclick: move |_| on_inspect.call(inspect.clone()), "Inspect run" }
                    if let Some(target) = campaign_target {
                        button { class: "btn", onclick: move |_| on_campaign.call(target.clone()), "Open campaign" }
                    }
                }
            }
            if let Some(campaign) = campaign {
                div { class: "review-progress", aria_label: "controller-observed campaign progress",
                    {crate::metadata_chip("Campaign", &campaign.phase)}
                    span { "{campaign.running.map_or_else(|| \"—\".into(), |n| n.to_string())} running" }
                    span { "{campaign.succeeded.map_or_else(|| \"—\".into(), |n| n.to_string())} succeeded" }
                    span { "{campaign.failed.map_or_else(|| \"—\".into(), |n| n.to_string())} failed" }
                    span { class: "muted", "Visual feedback is advisory; it does not pause this campaign." }
                }
            }
            label { class: "review-reference-label",
                "Comparison reference"
                select {
                    class: "rc-select",
                    onchange: move |event| {
                        let value = event.value();
                        reference.set((value != "retained-best").then(|| (subject.clone(), value)));
                    },
                    option { value: "retained-best", selected: !is_manual && !is_standalone, "Controller-retained incumbent (when distinct and compatible)" }
                    option { value: "standalone", selected: is_standalone, "Standalone observation (no comparison reference)" }
                    if unavailable_reference {
                        option { value: selected_reference.clone(), selected: true, disabled: true, "Selected reference unavailable — choose another" }
                    }
                    for other in alternatives {
                        option { value: identity(other), selected: other.uid.as_deref() == Some(selected_reference.as_str()), "Manual reference: {other.name} · {other.decision.as_deref().unwrap_or(\"decision not reported\")}" }
                    }
                }
            }
            if let Err(reason) = resolution {
                p { class: "review-notice", role: "status", "{reason}" }
            } else if is_manual {
                p { class: "review-notice", "Explicitly selected reference—not a claim that this was the last retained best." }
            } else if is_standalone {
                p { class: "muted", "Standalone observation: no comparison reference is being asserted." }
            }
            label { class: "review-scale",
                "Recorded image scale"
                select {
                    class: "rc-select",
                    onchange: move |event| zoom.set(event.value().parse::<u8>().unwrap_or(1)),
                    option { value: "1", selected: zoom() == 1, "Fit" }
                    option { value: "2", selected: zoom() == 2, "2× — scroll each image" }
                    option { value: "4", selected: zoom() == 4, "4× — scroll each image" }
                }
            }
            div { class: "review-pair",
                Evidence { key: identity(&run), resource: run.clone(), zoom: zoom(), label: "Candidate".to_string(), checks: benchmark_evidence(&snap, &run) }
                if let Some(baseline) = &baseline {
                    Evidence { key: identity(baseline), resource: baseline.clone(), zoom: zoom(), label: if is_manual { "Manual reference".to_string() } else { "Retained best".to_string() }, checks: benchmark_evidence(&snap, baseline) }
                } else {
                    section { class: "review-evidence review-unavailable",
                        h3 { "No baseline selected" }
                        p { "No distinct, compatible retained reference is available. Choose an explicit same-task reference above, or record a standalone observation. No previous run is substituted." }
                    }
                }
            }
            if let Some(baseline) = &baseline {
                if run.image != baseline.image || run.image.is_none() || baseline.image.is_none() {
                    p { class: "review-notice", "Runtime images differ or are unreported. This is a visual comparison, not evidence of controlled benchmark improvement." }
                } else {
                    p { class: "muted", "Declared task and recorded runtime image match. Evaluation inputs, seeds and budgets are not proven equivalent by this view." }
                }
            }
            if unavailable_reference {
                p { class: "review-notice", "Resolve the unavailable reference before starting a new comparison observation. Any existing draft is preserved." }
            } else {
                {curator::review_feedback_view(feedback, feedback_subject, &run, baseline.as_ref(), on_saved, &auth)}
            }
        }
    }
}

#[component]
fn Evidence(resource: ResourceSummary, label: String, checks: Element, zoom: u8) -> Element {
    let mut failed = use_signal(|| false);
    let mut loaded = use_signal(|| false);
    let mut replay = use_signal(|| false);
    let objective_value = resource
        .objective_value
        .map_or_else(|| "—".to_string(), |value| format!("{value:.5}"));
    let replay_url = resource.policy_url.as_deref().and_then(|policy| {
        let policy = reqwest::Url::parse(policy).ok()?;
        if !matches!(policy.scheme(), "http" | "https") {
            return None;
        }
        let mut viewer = reqwest::Url::parse("https://spot.casazza.io/").ok()?;
        viewer
            .query_pairs_mut()
            .append_pair("policy", policy.as_str());
        Some(viewer.to_string())
    });
    let figure = resource.figures_url.as_deref().and_then(|url| {
        let parsed = reqwest::Url::parse(url).ok()?;
        matches!(parsed.scheme(), "http" | "https").then_some((
            url,
            parsed.path().ends_with(".mp4") || parsed.path().ends_with(".webm"),
        ))
    });
    rsx! {
        section { class: "review-evidence",
            h3 { "{label}" }
            p { class: "review-run-name", "{resource.name}" }
            p { class: "review-row-context", "{resource.namespace} · {resource.uid.as_deref().unwrap_or(\"UID not reported\")}" }
            div { class: "review-observations",
                {crate::metadata_chip("Execution", &resource.phase)}
                {crate::metadata_chip("Decision", resource.decision.as_deref().unwrap_or("Not reported"))}
            }
            if let Some((figure, is_video)) = figure {
                if failed() {
                    div { class: "review-artifact-missing", role: "status",
                        p { "Evaluation artifact could not be loaded. Its absence is not a failed scientific gate." }
                        button { class: "btn", onclick: move |_| { failed.set(false); loaded.set(false); }, "Retry artifact" }
                        a { href: figure, target: "_blank", rel: "noopener noreferrer", "Open artifact source" }
                    }
                } else {
                    figure { class: "review-figure",
                        if !loaded() {
                            div { class: "review-artifact-loading", aria_busy: "true",
                                panel_kit::widgets::PanelHydrating { label: "Loading reported evaluation artifact".to_string() }
                            }
                        }
                        div { class: "review-media-frame", tabindex: "0", aria_label: "Recorded evidence; scroll to inspect enlarged images",
                        if is_video {
                            video { src: figure, controls: true, playsinline: true, preload: "metadata", onloadedmetadata: move |_| loaded.set(true), onerror: move |_| failed.set(true) }
                        } else {
                            img {
                                src: figure,
                                alt: "Recorded evaluation evidence for {resource.namespace}/{resource.name}",
                                style: format!("width:{}%;", u16::from(zoom) * 100),
                                onload: move |_| loaded.set(true),
                                onerror: move |_| failed.set(true),
                            }
                        }
                        }
                        figcaption {
                            if is_video { "Recorded evaluation video. The two players are independent." }
                            else { "Recorded evaluation frames—not a continuous video or a synchronized replay." }
                        }
                        a { href: figure, target: "_blank", rel: "noopener noreferrer", "Open full-resolution evidence" }
                    }
                }
            } else {
                div { class: "review-artifact-missing",
                    p { "No browser-accessible evaluation artifact was reported for this run." }
                    p { class: "muted", "Its hypothesis, measured objective and controller observations remain available below." }
                }
            }
            if let Some(url) = replay_url {
                div { class: "review-replay-actions",
                    button { class: "btn", onclick: move |_| replay.toggle(), if replay() { "Close policy replay" } else { "Replay policy in Spot gym" } }
                    a { href: url.clone(), target: "_blank", rel: "noopener noreferrer", "Open replay separately" }
                }
                if replay() {
                    p { class: "review-notice", "Interactive external simulation of this policy, not the recorded evaluation. Verify the robot/environment in the gym. The two players are not synchronized." }
                    iframe { class: "review-policy", src: url.clone(), title: "Policy replay for {resource.name}", "loading": "lazy", "sandbox": "allow-scripts allow-same-origin" }
                }
            }
            if let Some(hypothesis) = &resource.hypothesis {
                p { class: "review-hypothesis", "{hypothesis}" }
            }
            details {
                summary { "Provenance and objective" }
            dl { class: "detail-grid review-facts",
                dt { "Objective" } dd { "{resource.objective.as_deref().unwrap_or(\"Not reported\")}: {objective_value}" }
                dt { "Image" } dd { "{resource.image.as_deref().unwrap_or(\"Not reported\")}" }
                dt { "Object version" } dd { "{resource.resource_version.as_deref().unwrap_or(\"Not reported\")}" }
            }
            }
            {checks}
            details { class: "review-checks",
                summary { "Controller observations · {resource.conditions.len()}" }
                if resource.conditions.is_empty() {
                    p { class: "muted", "No check conditions were reported. Execution phase or retention alone does not establish that every scientific gate passed." }
                } else {
                    ConsoleTable {
                        id: format!("review-checks-{}-{}", resource.namespace, resource.name),
                        columns: vec![
                            DataColumnSpec::new("check", "Check"),
                            DataColumnSpec::new("status", "Status").enumerated(),
                            DataColumnSpec::new("reason", "Reason / evidence"),
                        ],
                        rows: resource.conditions.iter().map(|c| {
                            DataRow::new(&c.ctype).text(&c.ctype).text(&c.status)
                                .text(format!("{} {}", c.reason, c.message).trim())
                        }).collect::<Vec<_>>(),
                        sort: SortSpec::new("check", SortDir::Asc),
                        toolbar: vec![Control::Tally],
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(name: &str, namespace: &str) -> ResourceSummary {
        ResourceSummary {
            namespace: namespace.into(),
            name: name.into(),
            kind: "experiment".into(),
            uid: Some(format!("uid-{name}")),
            template: Some("walk-task".into()),
            campaign: Some("campaign".into()),
            mode: Some("locomotion".into()),
            robot: Some("spot".into()),
            decision: Some("Keep".into()),
            ..Default::default()
        }
    }

    fn snapshot() -> ClusterSnapshot {
        ClusterSnapshot {
            experiments: vec![
                run("candidate", "apps"),
                run("best", "apps"),
                run("best", "other"),
            ],
            campaigns: vec![ResourceSummary {
                name: "campaign".into(),
                namespace: "apps".into(),
                best_experiment: Some("best".into()),
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    #[test]
    fn baseline_uses_controller_reference_and_rejects_task_or_decision_mismatch() {
        let mut snap = snapshot();
        assert_eq!(
            retained_baseline(&snap, &snap.experiments[0])
                .unwrap()
                .namespace,
            "apps"
        );
        snap.experiments[1].mode = Some("stance".into());
        assert!(retained_baseline(&snap, &snap.experiments[0]).is_err());
        snap.experiments[1].mode = Some("locomotion".into());
        snap.experiments[1].decision = Some("Discard".into());
        assert!(retained_baseline(&snap, &snap.experiments[0]).is_err());
        snap.experiments[1].decision = Some("Keep".into());
        snap.campaigns[0].best_experiment = Some("candidate".into());
        assert!(retained_baseline(&snap, &snap.experiments[0]).is_err());
    }

    #[test]
    fn task_matching_does_not_cross_namespace_robot_or_benchmark_contracts() {
        let a = run("a", "apps");
        let mut b = run("b", "apps");
        assert!(same_task(&a, &b));
        b.robot = Some("humanoid".into());
        assert!(!same_task(&a, &b));
        b = run("b", "other");
        assert!(!same_task(&a, &b));
        b = run("b", "apps");
        b.benchmark_suite = Some("different-suite".into());
        assert!(!same_task(&a, &b));
        b = run("b", "apps");
        b.template = None;
        assert!(!same_task(&a, &b));
    }

    #[test]
    fn replaced_selected_object_is_not_silently_swapped_for_new_object() {
        let snap = snapshot();
        let mut selected = snap.experiments[0].clone();
        selected.uid = Some("old-deleted-object".into());
        assert!(candidate(&snap, Some(&selected)).is_none());
    }
}

#[cfg(test)]
mod benchmark_identity_tests {
    use super::*;

    #[test]
    fn old_object_stale_generation_and_cross_namespace_gates_are_not_attributed() {
        let run = ResourceSummary {
            name: "same".into(),
            namespace: "apps".into(),
            created_at: Some("2000".into()),
            ..Default::default()
        };
        let mut check = ResourceSummary {
            namespace: "apps".into(),
            target_namespace: Some("apps".into()),
            target_name: Some("same".into()),
            target_kind: Some("Experiment".into()),
            created_at: Some("3000".into()),
            object_generation: Some(2),
            observed_generation: Some(2),
            ..Default::default()
        };
        assert!(benchmark_targets_run(&check, &run));
        check.created_at = Some("1000".into());
        assert!(!benchmark_targets_run(&check, &run));
        check.created_at = Some("3000".into());
        check.observed_generation = Some(1);
        assert!(!benchmark_targets_run(&check, &run));
        check.observed_generation = Some(2);
        check.target_namespace = Some("other".into());
        assert!(!benchmark_targets_run(&check, &run));
        check.target_namespace = Some("apps".into());
        check.created_at = None;
        assert!(!benchmark_targets_run(&check, &run));
    }
}

#[cfg(test)]
mod reference_selection_tests {
    use super::*;

    #[test]
    fn missing_manual_reference_never_falls_back_to_a_different_incumbent() {
        let run = ResourceSummary {
            name: "candidate".into(),
            namespace: "apps".into(),
            kind: "experiment".into(),
            uid: Some("candidate-uid".into()),
            template: Some("task".into()),
            campaign: Some("campaign".into()),
            ..Default::default()
        };
        let mut incumbent = run.clone();
        incumbent.name = "incumbent".into();
        incumbent.uid = Some("incumbent-uid".into());
        incumbent.decision = Some("Keep".into());
        let snap = ClusterSnapshot {
            experiments: vec![run.clone(), incumbent],
            campaigns: vec![ResourceSummary {
                name: "campaign".into(),
                namespace: "apps".into(),
                best_experiment: Some("incumbent".into()),
                ..Default::default()
            }],
            ..Default::default()
        };
        assert_eq!(
            selected_baseline(&snap, &run, None).unwrap().name,
            "incumbent"
        );
        assert!(selected_baseline(&snap, &run, Some("deleted-reference-uid")).is_err());
        assert!(selected_baseline(&snap, &run, Some("candidate-uid")).is_err());
    }
}
