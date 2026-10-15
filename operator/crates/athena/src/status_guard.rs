//! The one place a reconciler decides whether to write `/status`.
//!
//! Every controller here watches the kind whose status it writes, so each status
//! write comes back as a watch event and schedules an immediate reconcile of the
//! same object. A reconcile that stamps a fresh `lastTransitionTime` on every pass
//! therefore changes its own object on every pass and re-triggers itself at API
//! round-trip speed. On 2026-10-15 that was ~19.65 `researchcampaigns/status`
//! PATCH/s across 68 campaigns, each pass also LISTing every Experiment
//! (~2.67 MB); with the same LIST on every Experiment reconcile that was ~48 MB/s
//! of billed relay egress.
//!
//! Two rules break the loop, and both live here so no reconciler can apply half:
//!
//! 1. **A condition's `lastTransitionTime` moves only when its `status` changes**
//!    (the `meta.SetStatusCondition` rule). Reason and message edits keep it.
//!    Without this "nothing changed" is unobservable: consecutive passes always
//!    differ by a timestamp, so no comparison can ever come out equal.
//! 2. **A status write that would change nothing is not sent.**
//!
//! A write that *does* change something still wakes the controller once; that one
//! extra pass finds nothing to change and stops. That is the converging behaviour
//! wanted, and why this guard is preferred over filtering the watch with
//! `predicates::generation`: that predicate also swallows metadata-only events
//! (the drive's approve-proposal annotation) and status written by *other*
//! controllers (the campaign stamps `Experiment.status.decision`, the benchmark
//! promotes metrics onto it), both of which a reconciler needs to see; it would
//! also need kube-runtime's `unstable-runtime-stream-control` feature to build the
//! filtered stream.

use serde::Serialize;
use serde_json::{Value, json};

/// Keys that say *when* something was last touched rather than *what* the state
/// is. A change in one of these alone is never a reason to write, otherwise a
/// stamp refreshed every pass would defeat the guard. It is still sent whenever
/// another field forces a write, so it reads as "when the content last changed".
const STAMP_KEYS: [&str; 1] = ["lastUpdated"];

/// Decide what to write for `desired` (a status object) given the object's
/// `current` status.
///
/// Returns the body for a merge `patch_status` (`{"status": …}`), or `None` when
/// applying it would change nothing, in which case the caller must not write.
///
/// Fails open: a `current` that cannot be serialised cannot be proven equal, so it
/// is treated as absent and the write goes ahead.
pub fn status_patch<S: Serialize>(current: Option<&S>, mut desired: Value) -> Option<Value> {
    let current = current
        .and_then(|status| serde_json::to_value(status).ok())
        .filter(|status| !status.is_null());
    if let Some(current) = &current {
        pin_transition_times(&mut desired, current);
    }
    if merge_is_noop(current.as_ref(), &desired) {
        return None;
    }
    Some(json!({ "status": desired }))
}

/// Carry each condition's previous `lastTransitionTime` forward when its `status`
/// is unchanged. A condition with no previous entry of the same `type`, or whose
/// `status` flipped, keeps the time its builder stamped (now).
fn pin_transition_times(desired: &mut Value, current: &Value) {
    let (Some(wanted), Some(had)) = (
        desired.get_mut("conditions").and_then(Value::as_array_mut),
        current.get("conditions").and_then(Value::as_array),
    ) else {
        return;
    };
    for condition in wanted {
        let Some(condition) = condition.as_object_mut() else {
            continue;
        };
        let Some(prior) = condition
            .get("type")
            .and_then(|ty| had.iter().find(|p| p.get("type") == Some(ty)))
        else {
            continue;
        };
        let carried = prior
            .get("lastTransitionTime")
            .filter(|_| prior.get("status") == condition.get("status"))
            .cloned();
        if let Some(time) = carried {
            condition.insert("lastTransitionTime".to_owned(), time);
        }
    }
}

/// Would merge-patching `patch` onto `current` leave it as it is (RFC 7386)?
fn merge_is_noop(current: Option<&Value>, patch: &Value) -> bool {
    match (patch, current) {
        (Value::Object(patch), Some(Value::Object(current))) => patch
            .iter()
            .filter(|(key, _)| !is_stamp(key))
            .all(|(key, value)| match value {
                // `null` deletes the key, so it is a no-op exactly when the key is
                // already gone. The apiserver prunes nulls on store, so a stored
                // `null` and an absent key are the same thing.
                Value::Null => current.get(key).is_none_or(Value::is_null),
                _ => merge_is_noop(current.get(key), value),
            }),
        // An object patched onto nothing, or onto a non-object, builds a new value.
        (Value::Object(_), _) => false,
        // Anything else replaces the target outright, arrays included.
        (_, Some(current)) => same_value(current, patch),
        (_, None) => false,
    }
}

/// Structural equality that tolerates what an apiserver round trip does to JSON:
/// `1.0` is stored and returned as `1`, and a `null` member is pruned.
fn same_value(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => {
            x == y || x.as_f64().zip(y.as_f64()).is_some_and(|(x, y)| x == y)
        }
        (Value::Array(x), Value::Array(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(x, y)| same_value(x, y))
        }
        (Value::Object(x), Value::Object(y)) => {
            let live = |(key, value): &(&String, &Value)| !is_stamp(key) && !value.is_null();
            x.iter().filter(live).count() == y.iter().filter(live).count()
                && x.iter()
                    .filter(live)
                    .all(|(key, x)| y.get(key).is_some_and(|y| same_value(x, y)))
        }
        _ => a == b,
    }
}

fn is_stamp(key: &str) -> bool {
    STAMP_KEYS.contains(&key)
}

#[cfg(test)]
mod tests {
    use super::*;
    use athena_api::experiment::ExperimentCondition;
    use athena_api::research_campaign::ResearchCampaignStatus;

    const T0: &str = "2026-10-15T00:00:00+00:00";
    const NOW: &str = "2026-10-15T09:30:00+00:00";

    fn condition(ty: &str, status: &str, message: &str, at: &str) -> Value {
        json!({
            "type": ty,
            "status": status,
            "reason": "Healthy",
            "message": message,
            "lastTransitionTime": at,
        })
    }

    fn typed_condition(ty: &str, status: &str, message: &str, at: &str) -> ExperimentCondition {
        ExperimentCondition {
            condition_type: Some(ty.into()),
            status: Some(status.into()),
            reason: Some("Healthy".into()),
            message: Some(message.into()),
            last_transition_time: Some(at.into()),
        }
    }

    /// What the apiserver holds after a reconcile wrote `running` experiments and a
    /// healthy condition at `T0`.
    fn stored(running: u32, message: &str) -> ResearchCampaignStatus {
        ResearchCampaignStatus {
            running_experiments: running,
            phase: Some("Running".into()),
            conditions: Some(vec![typed_condition(
                "ExperimentsHealthy",
                "True",
                message,
                T0,
            )]),
            ..Default::default()
        }
    }

    /// What the next pass computes from the same world, stamped `NOW` by its
    /// condition builder exactly as the reconcilers do.
    fn recomputed(running: u32, message: &str) -> Value {
        json!({
            "runningExperiments": running,
            "phase": "Running",
            "bestExperiment": null,
            "conditions": [condition("ExperimentsHealthy", "True", message, NOW)],
        })
    }

    #[test]
    fn condition_keeps_its_time_while_its_status_is_unchanged() {
        let current = json!({ "conditions": [condition("A", "True", "x", T0)] });
        // Same status, different reason/message and a fresh builder timestamp.
        let mut desired = json!({ "conditions": [condition("A", "True", "y", NOW)] });
        pin_transition_times(&mut desired, &current);
        assert_eq!(desired["conditions"][0]["lastTransitionTime"], T0);
        assert_eq!(
            desired["conditions"][0]["message"], "y",
            "only the time is pinned; the rest of the condition is the new one"
        );
    }

    #[test]
    fn condition_takes_the_new_time_when_its_status_flips() {
        let current = json!({ "conditions": [condition("A", "True", "x", T0)] });
        let mut desired = json!({ "conditions": [condition("A", "False", "x", NOW)] });
        pin_transition_times(&mut desired, &current);
        assert_eq!(desired["conditions"][0]["lastTransitionTime"], NOW);
    }

    #[test]
    fn a_condition_type_seen_for_the_first_time_keeps_its_own_time() {
        let current = json!({ "conditions": [condition("A", "True", "x", T0)] });
        let mut desired = json!({ "conditions": [
            condition("A", "True", "x", NOW),
            condition("B", "True", "x", NOW),
        ]});
        pin_transition_times(&mut desired, &current);
        assert_eq!(desired["conditions"][0]["lastTransitionTime"], T0);
        assert_eq!(desired["conditions"][1]["lastTransitionTime"], NOW);
    }

    #[test]
    fn nothing_changed_means_nothing_is_written() {
        // The storm: a status recomputed from an unchanged world differs from the
        // stored one only by the timestamp its condition builder stamped.
        let current = stored(3, "0 succeeded, 0 failed, 3 running");
        let desired = recomputed(3, "0 succeeded, 0 failed, 3 running");
        assert_eq!(status_patch(Some(&current), desired), None);
    }

    #[test]
    fn a_changed_field_is_written_but_an_unchanged_condition_keeps_its_time() {
        let current = stored(3, "0 succeeded, 0 failed, 3 running");
        // One experiment finished: the count and the message move, the condition's
        // status does not.
        let desired = recomputed(2, "1 succeeded, 0 failed, 2 running");
        let patch = status_patch(Some(&current), desired).expect("a real change is written");
        assert_eq!(patch["status"]["runningExperiments"], 2);
        let cond = &patch["status"]["conditions"][0];
        assert_eq!(cond["message"], "1 succeeded, 0 failed, 2 running");
        assert_eq!(cond["lastTransitionTime"], T0);
    }

    #[test]
    fn a_status_flip_is_written_with_the_new_time() {
        let current = stored(3, "ok");
        let mut desired = recomputed(3, "ok");
        desired["conditions"][0]["status"] = json!("False");
        let patch = status_patch(Some(&current), desired).expect("a flip is a change");
        assert_eq!(patch["status"]["conditions"][0]["lastTransitionTime"], NOW);
    }

    #[test]
    fn the_first_status_is_always_written() {
        let none: Option<&ResearchCampaignStatus> = None;
        assert!(status_patch(none, recomputed(0, "x")).is_some());
        // A status that serialises to null is "no status" too.
        assert!(status_patch(Some(&Value::Null), recomputed(0, "x")).is_some());
    }

    #[test]
    fn null_clears_a_present_key_and_is_idle_on_an_absent_one() {
        // Merge patch: `null` deletes. Sending it for a key that is already gone is
        // a no-op; sending it for a key that is set must go out or it never clears.
        let current = json!({ "phase": "Running", "pendingProposalJob": "job-1" });
        let clears = json!({ "phase": "Running", "pendingProposalJob": null });
        assert!(status_patch(Some(&current), clears.clone()).is_some());
        let settled = json!({ "phase": "Running" });
        assert_eq!(status_patch(Some(&settled), clears), None);
    }

    #[test]
    fn whole_floats_match_the_integers_an_apiserver_hands_back() {
        // We send 1.0; the apiserver stores and returns 1.
        let current = json!({ "metrics": { "reward": 1, "loss": 0.5 } });
        let desired = json!({ "metrics": { "reward": 1.0, "loss": 0.5 } });
        assert_eq!(status_patch(Some(&current), desired), None);
        let moved = json!({ "metrics": { "reward": 2.0, "loss": 0.5 } });
        assert!(status_patch(Some(&current), moved).is_some());
    }

    #[test]
    fn a_stamp_alone_never_forces_a_write_but_rides_along_with_one() {
        let current = json!({ "metricsDetail": { "latest": { "x": 1 }, "lastUpdated": T0 }});
        let refreshed = json!({ "metricsDetail": { "latest": { "x": 1 }, "lastUpdated": NOW }});
        assert_eq!(status_patch(Some(&current), refreshed), None);

        let moved = json!({ "metricsDetail": { "latest": { "x": 2 }, "lastUpdated": NOW }});
        let patch = status_patch(Some(&current), moved).expect("content changed");
        assert_eq!(patch["status"]["metricsDetail"]["lastUpdated"], NOW);
    }

    #[test]
    fn arrays_replace_wholesale_so_a_dropped_element_is_a_change() {
        let current = json!({ "conditions": [
            condition("A", "True", "x", T0),
            condition("B", "True", "x", T0),
        ]});
        let desired = json!({ "conditions": [condition("A", "True", "x", NOW)] });
        assert!(status_patch(Some(&current), desired).is_some());
    }

    #[test]
    fn a_null_member_inside_an_array_element_equals_an_absent_one() {
        // The apiserver prunes the null on store, so the stored element has no key.
        let current = json!({ "branches": [{ "campaign": "c" }] });
        let desired = json!({ "branches": [{ "campaign": "c", "note": null }] });
        assert_eq!(status_patch(Some(&current), desired), None);
    }
}
