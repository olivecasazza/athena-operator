//! Operator-embedded defaults for RuntimeProfile.
//!
//! Reasonable defaults (resources, sky TTLs) are deep-merged UNDER the applied
//! manifest before deserialization: user fields win at ANY depth, defaults only
//! fill holes, and a nested user override never erases sibling defaults.
//!
//! Implementation choice: merge on `serde_json::Value` trees, then deserialize
//! the merged tree into the typed spec. This is the simplest unit-testable
//! shape — no per-field `Option` gymnastics in the typed structs, and the merge
//! semantics are one 15-line recursive function.
//!
//! Sky defaults are conditional: they merge only when the manifest carries a
//! `sky` block. An absent `sky` means "on-prem k8s Job" and must stay absent —
//! an unconditional default would silently burst every profile to the cloud.

use serde_json::{Value, json};

/// Deep-merge `user` over `defaults`; user wins at any depth.
///
/// - object vs object: recurse per key; keys only in either side survive.
/// - explicit `null` in user is treated as "absent" (the default wins) — the
///   apiserver normally prunes nulls, and `cloud: null` in a manifest means
///   "no pin, use the default behavior", not "erase".
/// - any other user value (scalar, array) wins wholesale; arrays are not
///   element-merged.
pub fn deep_merge(defaults: Value, user: Value) -> Value {
    match (defaults, user) {
        (Value::Object(default_map), Value::Object(mut user_map)) => {
            let mut merged = serde_json::Map::with_capacity(default_map.len() + user_map.len());
            for (key, default_value) in default_map {
                match user_map.remove(&key) {
                    Some(user_value) => {
                        merged.insert(key, deep_merge(default_value, user_value));
                    }
                    None => {
                        merged.insert(key, default_value);
                    }
                }
            }
            // User keys with no default counterpart pass through untouched.
            merged.extend(user_map);
            Value::Object(merged)
        }
        (defaults, Value::Null) => defaults,
        (_, user) => user,
    }
}

/// The defaults tree for a RuntimeProfile spec, shaped by what the user
/// actually wrote (sky defaults only when a `sky` block is present).
///
/// Keys use the wire (camelCase) names since this merges against the raw
/// manifest before deserialization.
pub fn runtime_profile_defaults(user: &Value) -> Value {
    let mut defaults = json!({
        // Modest baseline requests so experiment pods are never BestEffort;
        // the Kueue ClusterQueue covers cpu/memory, so these stay admissible.
        // No limits by default — clamping is the user's call.
        "resources": {
            "requests": {
                "cpu": "500m",
                "memory": "512Mi",
            },
        },
    });

    // Presence of the block is the opt-in; fill in the burst knobs.
    if user.get("sky").is_some_and(|sky| !sky.is_null()) {
        defaults["sky"] = json!({
            "enabled": true,
            "useSpot": true,
            "ttlMinutes": 480,
            "idleMinutesToAutostop": 30,
        });
    }

    defaults
}

/// Deep-merge the operator defaults under a raw RuntimeProfile spec manifest.
pub fn apply_runtime_profile_defaults(user: Value) -> Value {
    let defaults = runtime_profile_defaults(&user);
    deep_merge(defaults, user)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime_profile::RuntimeProfileSpec;

    fn effective(spec: Value) -> RuntimeProfileSpec {
        serde_json::from_value(apply_runtime_profile_defaults(spec))
            .expect("merged spec deserializes")
    }

    fn base_spec(extra: Value) -> Value {
        let mut spec = json!({
            "runtime": { "type": "pytorch", "mode": "batchJob" },
            "image": "ghcr.io/example/trainer:latest",
        });
        if let (Value::Object(spec_map), Value::Object(extra_map)) = (&mut spec, extra) {
            spec_map.extend(extra_map);
        }
        spec
    }

    // An empty `sky: {}` block opts in and picks up every burst default,
    // notably the hard TTL.
    #[test]
    fn empty_sky_block_gets_ttl_default() {
        let spec = effective(base_spec(json!({ "sky": {} })));
        let sky = spec.sky.expect("sky present");
        assert!(sky.enabled);
        assert!(sky.use_spot);
        assert_eq!(sky.ttl_minutes, Some(480));
        assert_eq!(sky.idle_minutes_to_autostop, Some(30));
    }

    // A user-set ttlMinutes survives the merge; siblings still default.
    #[test]
    fn user_ttl_survives_merge() {
        let spec = effective(base_spec(json!({ "sky": { "ttlMinutes": 60 } })));
        let sky = spec.sky.expect("sky present");
        assert_eq!(sky.ttl_minutes, Some(60));
        // Sibling defaults are untouched by the override.
        assert_eq!(sky.idle_minutes_to_autostop, Some(30));
        assert!(sky.use_spot);
    }

    // A nested override wins without erasing sibling defaults at that depth.
    #[test]
    fn nested_override_keeps_sibling_defaults() {
        let spec = effective(base_spec(json!({
            "resources": { "requests": { "cpu": "8" } },
        })));
        assert_eq!(spec.resources.requests.get("cpu").unwrap(), "8");
        // The memory default sits next to the overridden cpu, not erased by it.
        assert_eq!(spec.resources.requests.get("memory").unwrap(), "512Mi");
    }

    // No sky block in the manifest => no sky block after defaulting. Absence
    // is the on-prem switch and must never be manufactured by defaults.
    #[test]
    fn absent_sky_stays_absent() {
        let spec = effective(base_spec(json!({})));
        assert!(spec.sky.is_none());
    }

    // Explicit user null behaves like absence: the default fills it.
    #[test]
    fn explicit_null_takes_default() {
        let merged = apply_runtime_profile_defaults(base_spec(json!({
            "sky": { "ttlMinutes": null },
        })));
        assert_eq!(merged["sky"]["ttlMinutes"], json!(480));
    }

    // User keys unknown to the defaults tree pass through unmodified.
    #[test]
    fn unrelated_user_fields_pass_through() {
        let merged = apply_runtime_profile_defaults(base_spec(json!({
            "scheduling": { "queueName": "athena-local" },
            "resources": { "limits": { "nvidia.com/gpu": "1" } },
        })));
        assert_eq!(merged["scheduling"]["queueName"], json!("athena-local"));
        assert_eq!(merged["resources"]["limits"]["nvidia.com/gpu"], json!("1"));
        // And the requests defaults still landed alongside the user limits.
        assert_eq!(merged["resources"]["requests"]["cpu"], json!("500m"));
    }

    // Arrays are replaced wholesale, never element-merged.
    #[test]
    fn arrays_replace_wholesale() {
        let merged = deep_merge(
            json!({ "list": [1, 2, 3], "keep": true }),
            json!({ "list": [9] }),
        );
        assert_eq!(merged["list"], json!([9]));
        assert_eq!(merged["keep"], json!(true));
    }
}
