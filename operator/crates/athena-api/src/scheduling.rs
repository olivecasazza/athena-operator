//! Shared reader for the GPU-scheduling / inference stack, surfaced by both
//! consoles (desktop iced + web/Dioxus). Read-only observability: it snapshots
//! Kueue (ClusterQueue/Workload), Hephaestus (MetalMachine power), and the
//! ephemeral inference backends (RayJob + campaign inferenceMesh/Cluster).
//!
//! Kueue/Ray/MetalMachine have no typed structs here, so everything is read as
//! `DynamicObject` and projected into the serde DTOs below. The desktop renders
//! these directly; the web backend serializes them to JSON for the frontend.

use kube::api::{Api, ApiResource, DynamicObject, ListParams};
use kube::core::GroupVersionKind;
use kube::Client;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// One GPU pool = one Kueue ClusterQueue: nominal quota vs live usage + queue depth.
#[derive(Serialize, Deserialize, Debug, Clone, Default, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct GpuPool {
    pub name: String,
    pub gpu_nominal: i64,
    pub gpu_used: i64,
    pub cpu_nominal: i64,
    pub cpu_used: i64,
    pub pending_workloads: i64,
    pub admitted_workloads: i64,
}

/// One Kueue Workload row (a Job/RayJob's admission record).
#[derive(Serialize, Deserialize, Debug, Clone, Default, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct WorkloadRow {
    pub name: String,
    pub namespace: String,
    pub queue: String,
    pub priority_class: String,
    /// Admitted | QuotaReserved | Pending | Finished
    pub state: String,
    pub gpus: i64,
}

/// A GPU node's power state (Hephaestus MetalMachine = scale-from-zero on metal).
#[derive(Serialize, Deserialize, Debug, Clone, Default, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct NodePower {
    pub name: String,
    pub powered: bool,
    pub phase: String,
    pub pool: String,
}

/// An ephemeral inference backend the operator brought up for a campaign.
#[derive(Serialize, Deserialize, Debug, Clone, Default, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct InferenceBackend {
    pub campaign: String,
    /// "mesh" (single-node mesh-llm Deployment) | "cluster" (multi-node vLLM RayJob)
    pub kind: String,
    pub name: String,
    pub serving: bool,
    pub endpoint: String,
}

/// Full snapshot of the scheduling/inference stack for the console.
#[derive(Serialize, Deserialize, Debug, Clone, Default, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SchedulingSnapshot {
    pub pools: Vec<GpuPool>,
    pub workloads: Vec<WorkloadRow>,
    pub nodes: Vec<NodePower>,
    pub inference: Vec<InferenceBackend>,
}

fn dyn_api(client: &Client, group: &str, version: &str, kind: &str) -> Api<DynamicObject> {
    let ar = ApiResource::from_gvk(&GroupVersionKind::gvk(group, version, kind));
    Api::all_with(client.clone(), &ar)
}

fn as_i64(v: &Value) -> i64 {
    match v {
        Value::Number(n) => n.as_i64().unwrap_or(0),
        // Kueue quotas can be quantity strings ("48", "192Gi") — take the leading int.
        Value::String(s) => s
            .chars()
            .take_while(|c| c.is_ascii_digit())
            .collect::<String>()
            .parse()
            .unwrap_or(0),
        _ => 0,
    }
}

/// Sum a Kueue flavor's resource entries (spec nominalQuota or status total) for `res`.
fn sum_flavor_resource(flavors: &Value, res: &str, key: &str) -> i64 {
    flavors
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|f| f.get("resources").and_then(|r| r.as_array()))
                .flatten()
                .filter(|r| r.get("name").and_then(|n| n.as_str()) == Some(res))
                .map(|r| r.get(key).map(as_i64).unwrap_or(0))
                .sum()
        })
        .unwrap_or(0)
}

/// Snapshot the scheduling/inference stack. Best-effort: a missing CRD (e.g. no
/// Kueue) yields empty sections rather than erroring, so the console still renders.
pub async fn read_scheduling(client: &Client) -> SchedulingSnapshot {
    let mut snap = SchedulingSnapshot::default();
    let lp = ListParams::default();

    // ClusterQueues → pools.
    let cqs = dyn_api(client, "kueue.x-k8s.io", "v1beta1", "ClusterQueue");
    if let Ok(list) = cqs.list(&lp).await {
        for cq in list {
            let spec_flavors = cq
                .data
                .pointer("/spec/resourceGroups/0/flavors")
                .cloned()
                .unwrap_or(Value::Null);
            let usage = cq
                .data
                .pointer("/status/flavorsUsage")
                .cloned()
                .unwrap_or(Value::Null);
            snap.pools.push(GpuPool {
                name: cq.metadata.name.clone().unwrap_or_default(),
                gpu_nominal: sum_flavor_resource(&spec_flavors, "nvidia.com/gpu", "nominalQuota"),
                gpu_used: sum_flavor_resource(&usage, "nvidia.com/gpu", "total"),
                cpu_nominal: sum_flavor_resource(&spec_flavors, "cpu", "nominalQuota"),
                cpu_used: sum_flavor_resource(&usage, "cpu", "total"),
                pending_workloads: cq
                    .data
                    .pointer("/status/pendingWorkloads")
                    .map(as_i64)
                    .unwrap_or(0),
                admitted_workloads: cq
                    .data
                    .pointer("/status/admittedWorkloads")
                    .map(as_i64)
                    .unwrap_or(0),
            });
        }
    }

    // Workloads → rows.
    let wls = dyn_api(client, "kueue.x-k8s.io", "v1beta1", "Workload");
    if let Ok(list) = wls.list(&lp).await {
        for w in list {
            let admitted = w
                .data
                .pointer("/status/conditions")
                .and_then(|c| c.as_array())
                .map(|conds| {
                    conds.iter().any(|c| {
                        c.get("type").and_then(|t| t.as_str()) == Some("Admitted")
                            && c.get("status").and_then(|s| s.as_str()) == Some("True")
                    })
                })
                .unwrap_or(false);
            let finished = w
                .data
                .pointer("/status/conditions")
                .and_then(|c| c.as_array())
                .map(|conds| {
                    conds
                        .iter()
                        .any(|c| c.get("type").and_then(|t| t.as_str()) == Some("Finished"))
                })
                .unwrap_or(false);
            let gpus = w
                .data
                .pointer("/spec/podSets")
                .and_then(|p| p.as_array())
                .map(|sets| {
                    sets.iter()
                        .map(|s| {
                            let count = s.get("count").map(as_i64).unwrap_or(1);
                            let per = s
                                .pointer("/template/spec/containers/0/resources/requests/nvidia.com~1gpu")
                                .map(as_i64)
                                .unwrap_or(0);
                            count * per
                        })
                        .sum()
                })
                .unwrap_or(0);
            snap.workloads.push(WorkloadRow {
                name: w.metadata.name.clone().unwrap_or_default(),
                namespace: w.metadata.namespace.clone().unwrap_or_default(),
                queue: w
                    .data
                    .pointer("/spec/queueName")
                    .and_then(|q| q.as_str())
                    .unwrap_or("")
                    .to_string(),
                priority_class: w
                    .data
                    .pointer("/spec/priorityClassName")
                    .and_then(|p| p.as_str())
                    .unwrap_or("")
                    .to_string(),
                state: if finished {
                    "Finished".into()
                } else if admitted {
                    "Admitted".into()
                } else {
                    "Pending".into()
                },
                gpus,
            });
        }
    }

    // MetalMachines (Hephaestus) → node power. group heph.nixlab.io/v1alpha1;
    // power = status.poweredOn, phase = status.phase, pool = spec.poolRef.
    let mms = dyn_api(client, "heph.nixlab.io", "v1alpha1", "MetalMachine");
    if let Ok(list) = mms.list(&lp).await {
        for m in list {
            snap.nodes.push(NodePower {
                name: m.metadata.name.clone().unwrap_or_default(),
                powered: m
                    .data
                    .pointer("/status/poweredOn")
                    .and_then(|p| p.as_bool())
                    .unwrap_or(false),
                phase: m
                    .data
                    .pointer("/status/phase")
                    .and_then(|p| p.as_str())
                    .unwrap_or("")
                    .to_string(),
                pool: m
                    .data
                    .pointer("/spec/poolRef")
                    .and_then(|p| p.as_str())
                    .unwrap_or("")
                    .to_string(),
            });
        }
    }

    // Inference backends: RayJobs named vllm-* (cluster) + Deployments named
    // mesh-llm-* (mesh). Campaign is the suffix.
    let rjs = dyn_api(client, "ray.io", "v1", "RayJob");
    if let Ok(list) = rjs.list(&lp).await {
        for rj in list {
            let name = rj.metadata.name.clone().unwrap_or_default();
            if let Some(campaign) = name.strip_prefix("vllm-") {
                let dep = rj
                    .data
                    .pointer("/status/jobDeploymentStatus")
                    .and_then(|s| s.as_str())
                    .unwrap_or("");
                snap.inference.push(InferenceBackend {
                    campaign: campaign.to_string(),
                    kind: "cluster".into(),
                    name: name.clone(),
                    serving: dep == "Running",
                    endpoint: format!(
                        "http://{name}.{}.svc:8000/v1",
                        rj.metadata.namespace.clone().unwrap_or_default()
                    ),
                });
            }
        }
    }

    snap
}
