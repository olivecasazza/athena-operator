//! Harness proposers: a ResearchDrive's proposal produced by an agent harness
//! running in a stateless Job instead of one chat completion.
//!
//! Lifecycle, driven entirely by drive reconciles:
//!
//! 1. No pending Job → create `<drive>-proposal-<n>` (owned by the drive,
//!    labelled for queries and the harness NetworkPolicy) and record it in
//!    `status.pendingProposalJob`.
//! 2. Job still running → keep waiting (the drive stays `Proposing`).
//! 3. Job finished → read the pod termination message. The runner writes
//!    `{"summary", "actions"}` on success or `{"error"}` on failure; either
//!    way the pending pointer is cleared so the next pass starts fresh.
//!
//! The Job holds no Kubernetes credentials, runs non-root with a read-only
//! root filesystem and no privilege escalation, and its output is treated as
//! untrusted: the drive validates every action exactly as it does a chat
//! reply.

use std::collections::BTreeMap;
use std::sync::Arc;

use athena_api::research_drive::{HarnessType, ProposerHarness, ProposerSpec, ResearchDrive};
use k8s_openapi::api::batch::v1::{Job, JobSpec};
use k8s_openapi::api::core::v1::{
    Capabilities, Container, EmptyDirVolumeSource, EnvVar, EnvVarSource, Pod, PodSecurityContext,
    PodSpec, PodTemplateSpec, ResourceRequirements, SeccompProfile, SecretKeySelector,
    SecurityContext, Volume, VolumeMount,
};
use k8s_openapi::apimachinery::pkg::api::resource::Quantity;
use kube::api::{ListParams, ObjectMeta, PostParams};
use kube::{Api, Resource, ResourceExt};
use serde_json::Value;

use crate::Context;
use crate::drive_reconciler::Error;

/// Label on harness Jobs/pods: queryable, and the NetworkPolicy selector.
pub const ROLE_LABEL: &str = "athena.nixlab.io/role";
pub const ROLE_VALUE: &str = "proposer-harness";
const DRIVE_LABEL: &str = "athena.nixlab.io/drive";

/// Outcome of one reconcile's look at the harness.
pub enum HarnessPoll {
    /// A proposal is ready (untrusted; validate before use).
    Ready(Value),
    /// A Job is producing the proposal; check again later.
    Running,
}

/// Advance the harness proposer by one step. `pending` is the drive's
/// `status.pendingProposalJob`; it is updated in place.
#[allow(clippy::too_many_arguments)]
pub async fn poll_or_start(
    drive: &ResearchDrive,
    ctx: &Arc<Context>,
    ns: &str,
    drive_name: &str,
    proposal_id: &str,
    pending: &mut Option<String>,
    system: &str,
    user: &str,
) -> Result<HarnessPoll, Error> {
    let proposer = &drive.spec.proposer;
    let harness = proposer
        .harness
        .as_ref()
        .ok_or_else(|| Error::Proposer("no harness configured".into()))?;
    let jobs: Api<Job> = Api::namespaced(ctx.client.clone(), ns);

    if let Some(job_name) = pending.clone() {
        match jobs.get_opt(&job_name).await? {
            // Deleted out from under us (TTL, a human): start over.
            None => *pending = None,
            Some(job) => {
                let st = job.status.clone().unwrap_or_default();
                if st.succeeded.unwrap_or(0) == 0 && st.failed.unwrap_or(0) == 0 {
                    return Ok(HarnessPoll::Running);
                }
                *pending = None;
                let message = termination_message(ctx, ns, &job).await?;
                return match message {
                    Some(v) if v.get("error").is_some() => Err(Error::ProposerOutput(format!(
                        "harness job {job_name}: {}",
                        v["error"].as_str().unwrap_or("error")
                    ))),
                    Some(v) if st.succeeded.unwrap_or(0) > 0 => Ok(HarnessPoll::Ready(v)),
                    _ => Err(Error::Proposer(format!(
                        "harness job {job_name} failed without a proposal"
                    ))),
                };
            }
        }
    }

    let job = build_job(
        drive,
        drive_name,
        ns,
        proposal_id,
        proposer,
        harness,
        system,
        user,
    )?;
    let name = job.name_any();
    match jobs.create(&PostParams::default(), &job).await {
        Ok(_) => {}
        // Same proposal id already started (status write lost): adopt it.
        Err(kube::Error::Api(e)) if e.code == 409 => {}
        Err(e) => return Err(Error::Kube(e)),
    }
    *pending = Some(name.clone());
    Ok(HarnessPoll::Running)
}

async fn termination_message(
    ctx: &Arc<Context>,
    ns: &str,
    job: &Job,
) -> Result<Option<Value>, Error> {
    let pods: Api<Pod> = Api::namespaced(ctx.client.clone(), ns);
    let lp = ListParams::default().labels(&format!("job-name={}", job.name_any()));
    let list = pods.list(&lp).await?;
    Ok(list.items.iter().find_map(|pod| {
        pod.status
            .as_ref()?
            .container_statuses
            .as_ref()?
            .iter()
            .filter_map(|s| s.state.as_ref()?.terminated.as_ref()?.message.as_deref())
            .find_map(|m| serde_json::from_str::<Value>(m).ok())
    }))
}

fn env(name: &str, value: impl Into<String>) -> EnvVar {
    EnvVar {
        name: name.into(),
        value: Some(value.into()),
        ..Default::default()
    }
}

/// DNS-1123 name `<drive>-<proposal-id>`, capped at 63.
fn job_name(drive_name: &str, proposal_id: &str) -> String {
    let raw = format!("{drive_name}-{proposal_id}");
    let mut s: String = raw
        .to_lowercase()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect();
    if s.len() > 63 {
        // Keep the proposal id (the unique part) and trim the drive prefix.
        let keep = 63 - proposal_id.len() - 1;
        s = format!("{}-{}", &s[..keep].trim_end_matches('-'), proposal_id);
    }
    s.trim_matches('-').to_string()
}

#[allow(clippy::too_many_arguments)]
fn build_job(
    drive: &ResearchDrive,
    drive_name: &str,
    ns: &str,
    proposal_id: &str,
    proposer: &ProposerSpec,
    harness: &ProposerHarness,
    system: &str,
    user: &str,
) -> Result<Job, Error> {
    let HarnessType::PrimeAgent = harness.harness_type;
    let name = job_name(drive_name, proposal_id);
    let mut labels = BTreeMap::from([
        (ROLE_LABEL.to_string(), ROLE_VALUE.to_string()),
        (DRIVE_LABEL.to_string(), drive_name.to_string()),
        // The cloud-node guard (ValidatingAdmissionPolicy cloud-node-opt-in)
        // requires this label on any pod tolerating node.kubernetes.io/cloud.
        // A harness Job is CPU-only, node-agnostic work whose isolation comes
        // from the NetworkPolicy (DNS + OmniRoute + the console only), non-root
        // and a read-only rootfs — so letting it land on the always-on cloud
        // hosts when the always-on mac pool is down is safe, and without it the
        // proposer silently stops running.
        ("nixlab.io/cloud-opt-in".to_string(), "true".to_string()),
    ]);
    let owner = drive
        .controller_owner_ref(&())
        .ok_or_else(|| Error::Proposer("drive has no uid".into()))?;

    let mut envs = vec![
        env("LLM_BASE_URL", proposer.endpoint.clone()),
        env("LLM_MODEL", proposer.model.clone()),
        env("ATHENA_MCP_URL", harness.mcp_url.clone()),
        env("PROPOSER_SYSTEM", system),
        env("PROPOSER_CONTEXT", user),
        env("MAX_TURNS", harness.max_turns.to_string()),
        env("TIMEOUT_SECONDS", harness.timeout_seconds.to_string()),
    ];
    envs.push(match &proposer.api_key_secret_ref {
        Some(r) => EnvVar {
            name: "LLM_API_KEY".into(),
            value_from: Some(EnvVarSource {
                secret_key_ref: Some(SecretKeySelector {
                    name: r.name.clone(),
                    key: r.key.clone(),
                    optional: Some(false),
                }),
                ..Default::default()
            }),
            ..Default::default()
        },
        None => env("LLM_API_KEY", "unused"),
    });

    let q = |s: &str| Quantity(s.to_string());
    let empty = |n: &str| Volume {
        name: n.into(),
        empty_dir: Some(EmptyDirVolumeSource::default()),
        ..Default::default()
    };
    let mount = |n: &str, p: &str| VolumeMount {
        name: n.into(),
        mount_path: p.into(),
        ..Default::default()
    };

    Ok(Job {
        metadata: ObjectMeta {
            name: Some(name),
            namespace: Some(ns.to_string()),
            labels: Some(labels.clone()),
            owner_references: Some(vec![owner]),
            ..Default::default()
        },
        spec: Some(JobSpec {
            backoff_limit: Some(0),
            active_deadline_seconds: Some(i64::from(harness.timeout_seconds) + 120),
            ttl_seconds_after_finished: Some(3600),
            template: PodTemplateSpec {
                metadata: Some(ObjectMeta {
                    labels: Some(labels),
                    ..Default::default()
                }),
                spec: Some(PodSpec {
                    restart_policy: Some("Never".into()),
                    automount_service_account_token: Some(false),
                    enable_service_links: Some(false),
                    tolerations: Some(vec![k8s_openapi::api::core::v1::Toleration {
                        key: Some("node.kubernetes.io/cloud".into()),
                        operator: Some("Exists".into()),
                        effect: Some("NoSchedule".into()),
                        ..Default::default()
                    }]),
                    security_context: Some(PodSecurityContext {
                        run_as_non_root: Some(true),
                        run_as_user: Some(10001),
                        fs_group: Some(10001),
                        seccomp_profile: Some(SeccompProfile {
                            type_: "RuntimeDefault".into(),
                            ..Default::default()
                        }),
                        ..Default::default()
                    }),
                    containers: vec![Container {
                        name: "proposer".into(),
                        image: Some(harness.image.clone()),
                        env: Some(envs),
                        termination_message_policy: Some("File".into()),
                        resources: Some(ResourceRequirements {
                            requests: Some(BTreeMap::from([
                                ("cpu".into(), q("250m")),
                                ("memory".into(), q("768Mi")),
                            ])),
                            limits: Some(BTreeMap::from([
                                ("cpu".into(), q("2")),
                                ("memory".into(), q("3Gi")),
                            ])),
                            ..Default::default()
                        }),
                        security_context: Some(SecurityContext {
                            allow_privilege_escalation: Some(false),
                            read_only_root_filesystem: Some(true),
                            capabilities: Some(Capabilities {
                                drop: Some(vec!["ALL".into()]),
                                ..Default::default()
                            }),
                            ..Default::default()
                        }),
                        volume_mounts: Some(vec![mount("work", "/work"), mount("tmp", "/tmp")]),
                        ..Default::default()
                    }],
                    volumes: Some(vec![empty("work"), empty("tmp")]),
                    ..Default::default()
                }),
            },
            ..Default::default()
        }),
        ..Default::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn job_names_are_dns_labels_that_keep_the_proposal_id() {
        let n = job_name("multi-robot-curriculum-drive", "proposal-12");
        assert_eq!(n, "multi-robot-curriculum-drive-proposal-12");
        let long = job_name(&"x".repeat(80), "proposal-123");
        assert!(long.len() <= 63, "{long}");
        assert!(long.ends_with("-proposal-123"));
        assert!(!long.contains("--"));
    }
}
