//! Read-only projections of Kubernetes timestamps, not agent/tool traces.

use std::rc::Rc;

use dioxus::prelude::*;
use panel_kit::widgets::Button;
use panel_kit::widgets::trace::{Trace, TraceRail};
use panel_kit_core::widgets::trace::facts::Expansion;
use panel_kit_core::widgets::trace::query::GraphState;
use panel_kit_core::widgets::trace::rails::{RailOptions, vertical_with};
use panel_kit_core::widgets::trace::{Event, EventKind, ItemRef, SessionTree, Source};

use crate::models::{ClusterSnapshot, ResourceSummary};

fn milestones(resources: &[ResourceSummary]) -> Vec<Event> {
    let mut events = Vec::with_capacity(resources.len() * 3);
    for resource in resources {
        let item = resource.manifest_path();
        // Ended is not synonymous with succeeded: unknown terminal outcomes
        // remain a status observation, never a green RunFinished event.
        let end_kind = match resource.phase.as_str() {
            "Succeeded" | "Completed" => EventKind::RunFinished,
            "Failed" | "Crashed" => EventKind::RunFailed,
            "Cancelled" | "Canceled" => EventKind::RunCancelled,
            _ => EventKind::ItemStatus,
        };
        for (stamp, kind, label) in [
            (&resource.created_at, EventKind::ItemCreated, "Created"),
            (&resource.started_at, EventKind::RunStarted, "Started"),
            (&resource.ended_at, end_kind, "Ended"),
        ] {
            let Some(at) = stamp
                .as_deref()
                .and_then(|s| s.parse::<i64>().ok())
                .and_then(|ms| ms.checked_mul(1_000))
            else {
                continue;
            };
            let mut event = Event::new(format!("{item}:{label}"), at, kind)
                .item(&item)
                .source(Source::Run)
                .summary(format!("{label}: {}/{}", resource.namespace, resource.name));
            event.detail.title = Some(resource.name.clone());
            // Only an end timestamp observes the final status. Do not stamp
            // today's phase onto the historical creation/start observations.
            if label == "Ended" {
                event.detail.status = Some(resource.phase.clone());
            }
            events.push(event);
        }
    }
    events.sort_by_key(Event::order_key);
    events
}

#[component]
pub(crate) fn RunActivity(
    snapshot: ClusterSnapshot,
    selected: Signal<Option<ResourceSummary>>,
) -> Element {
    let Some(resource) = selected() else {
        return crate::no_selection();
    };
    let resources = if resource.kind == "researchcampaign" {
        snapshot
            .experiments
            .into_iter()
            .filter(|e| {
                e.namespace == resource.namespace
                    && e.campaign.as_deref() == Some(resource.name.as_str())
            })
            .collect()
    } else {
        vec![resource.clone()]
    };
    let key = resource.manifest_path();
    rsx! { ActivityBody { key: "{key}", resource, resources } }
}

#[component]
fn ActivityBody(resource: ResourceSummary, resources: Vec<ResourceSummary>) -> Element {
    let events = use_memo(use_reactive!(|resources| Rc::new(milestones(&resources))));
    let mut state = use_signal(|| {
        let observed = events();
        let first = observed.first().map(|e| e.at / 1_000);
        let last = observed.last().map(|e| e.at / 1_000);
        let day = |at: Option<i64>| {
            crate::tables::fmt_ms(&at.map(|n| n.to_string()))
                .chars()
                .take(10)
                .collect::<String>()
        };
        GraphState {
            window: match (first, last) {
                (Some(_), Some(_)) => format!("{}/{}", day(first), day(last)),
                _ => "24h".into(),
            },
            ..GraphState::default()
        }
    });
    let mut rail = use_signal(|| false);
    let mut expanded = use_signal(Expansion::new);
    let observed = events();
    let now = observed.last().map_or(0, |e| e.at / 1_000_000 + 1);
    let item = ItemRef {
        id: resource.manifest_path(),
        status: None,
    };
    let opts = RailOptions {
        title: "Observed run milestones".into(),
        dom_id: "athena-run-rail".into(),
        on_select: true,
        ..RailOptions::default()
    };
    let view = vertical_with(
        &observed,
        &item,
        &SessionTree::default(),
        &expanded(),
        opts.wrap,
    )
    .view(&opts);
    rsx! {
        div { class: "view-head",
            h2 { "Run Activity" }
            p { "Kubernetes creation/start/end observations only. Tool calls, model spans and agent attribution are not supplied by this API." }
        }
        if observed.is_empty() {
            p { class: "muted", "No observed timestamps for this selection." }
        } else {
            if resource.kind == "experiment" {
                div { class: "activity-modes",
                    Button { pressed: !rail(), on_press: move |_| rail.set(false), "Overview" }
                    Button { pressed: rail(), on_press: move |_| rail.set(true), "Trace rail" }
                }
            }
            if rail() && resource.kind == "experiment" {
                TraceRail {
                    view, expanded: expanded(),
                    on_toggle: move |id: String| { expanded.write().toggle(&id); },
                    on_open_all: move |ids: Vec<String>| { expanded.write().open_all(ids.iter().map(String::as_str)); },
                    on_close_all: move |_| { expanded.set(Expansion::new()); },
                }
            } else {
                Trace {
                    id: "athena-run-activity", state: state(), events: observed, now,
                    on_action: move |action| { state.write().reduce(action); },
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_end_is_not_success_and_missing_or_overflowed_stamps_are_not_events() {
        let rows = [ResourceSummary {
            name: "run".into(),
            namespace: "apps".into(),
            kind: "experiment".into(),
            phase: "Unknown".into(),
            created_at: Some(i64::MAX.to_string()),
            started_at: Some("not a timestamp".into()),
            ended_at: Some("1234".into()),
            ..ResourceSummary::default()
        }];
        let events = milestones(&rows);
        assert_eq!(
            events.iter().map(|e| (e.at, e.kind)).collect::<Vec<_>>(),
            vec![(1_234_000, EventKind::ItemStatus)]
        );
        assert_eq!(events[0].agent, None);
        assert_eq!(events[0].detail.status.as_deref(), Some("Unknown"));
    }

    #[test]
    fn milestones_preserve_namespace_and_terminal_outcome() {
        let rows: Vec<_> = [("apps", "Failed"), ("other", "Succeeded")]
            .into_iter()
            .map(|(namespace, phase)| ResourceSummary {
                name: "same-name".into(),
                namespace: namespace.into(),
                kind: "experiment".into(),
                phase: phase.into(),
                created_at: Some("1000".into()),
                started_at: Some("2000".into()),
                ended_at: Some("3000".into()),
                ..ResourceSummary::default()
            })
            .collect();
        let events = milestones(&rows);
        assert_eq!(
            events
                .iter()
                .filter(|e| e.kind == EventKind::RunFailed)
                .map(|e| e.item_id.as_deref().unwrap())
                .collect::<Vec<_>>(),
            vec!["k8s://apps/experiment/same-name.yaml"]
        );
        assert_eq!(
            events
                .iter()
                .filter(|e| e.kind == EventKind::RunFinished)
                .map(|e| e.item_id.as_deref().unwrap())
                .collect::<Vec<_>>(),
            vec!["k8s://other/experiment/same-name.yaml"]
        );
        assert!(
            events
                .iter()
                .filter(|e| e.kind == EventKind::ItemCreated)
                .all(|e| e.detail.status.is_none())
        );
    }
}
