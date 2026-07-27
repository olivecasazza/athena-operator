use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;

use athena_api::common::FailureClass;
use athena_api::experiment::{
    CheckpointRef, Experiment, ExperimentArtifacts, ExperimentCondition, ExperimentEnvironment,
    ExperimentMetricPoint, ExperimentMetricSeries, ExperimentMetrics, ExperimentPhase,
    ExperimentStatus,
};
use athena_api::experiment_template::{ExperimentTemplate, ObjectiveGoal};
use athena_api::research_campaign::ResearchCampaign;
use athena_api::runtime_profile::{ExecutionMode, RuntimeProfile, SkySpec};
use k8s_openapi::api::batch::v1::{Job, JobSpec};
use k8s_openapi::api::core::v1::{
    Container, ContainerPort, EnvFromSource, EnvVar, LocalObjectReference, PersistentVolumeClaim,
    PersistentVolumeClaimSpec, PersistentVolumeClaimVolumeSource, Pod, PodSpec, PodTemplateSpec,
    ResourceRequirements, SecretEnvSource, SecretVolumeSource, Volume, VolumeMount,
    VolumeResourceRequirements,
};
use k8s_openapi::apimachinery::pkg::api::resource::Quantity;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;
use kube::ResourceExt;
use kube::api::{Api, ListParams, Patch, PatchParams, PostParams};
use kube::runtime::controller::Action;
use serde_json::{Value, json};
use tracing::{info, warn};

use crate::Context;
use crate::metrics;
use crate::telemetry;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("kube error: {0}")]
    Kube(#[from] kube::Error),
    #[error("missing {kind} {namespace}/{name}")]
    MissingRef {
        kind: &'static str,
        namespace: String,
        name: String,
    },
    #[error("runtime profile {0} is not batchJob mode")]
    UnsupportedRuntimeMode(String),
    #[error(
        "runtime profile {0} is sky-enabled but has no command/args; a sky task needs an explicit run command (the container image's entrypoint is not visible to the launcher)"
    )]
    SkyCommandRequired(String),
}

#[tracing::instrument(skip(experiment, ctx), fields(
    experiment.name = %experiment.metadata.name.as_deref().unwrap_or("unknown"),
    experiment.namespace = %experiment.metadata.namespace.as_deref().unwrap_or("default"),
    trace_id = tracing::field::Empty,
    span_id = tracing::field::Empty,
))]
pub async fn reconcile(experiment: Arc<Experiment>, ctx: Arc<Context>) -> Result<Action, Error> {
    let started_at = Instant::now();
    let name = experiment.metadata.name.as_deref().unwrap_or("unknown");
    let ns = experiment
        .metadata
        .namespace
        .as_deref()
        .unwrap_or("default");
    let phase = experiment
        .status
        .as_ref()
        .map(|s| s.phase.clone())
        .unwrap_or_default();
    let phase_label = format!("{:?}", phase);
    let campaign = experiment.spec.campaign_ref.as_str();
    let (trace_id, span_id) = telemetry::current_trace_ids();
    tracing::Span::current().record("trace_id", tracing::field::display(&trace_id));
    tracing::Span::current().record("span_id", tracing::field::display(&span_id));

    info!(
        name,
        namespace = ns,
        campaign,
        ?phase,
        trace_id,
        span_id,
        "reconciling Experiment"
    );

    if matches!(phase, ExperimentPhase::Pending | ExperimentPhase::Preparing) {
        ensure_experiment_job(&experiment, ctx.clone(), ns, name).await?;
    }
    reconcile_experiment_status(&experiment, ctx.clone(), ns, name).await?;

    update_experiment_metrics(ctx.clone(), ns).await?;
    telemetry::record_reconcile(ns, campaign, &phase_label, "ok", started_at.elapsed());

    Ok(Action::requeue(Duration::from_secs(30)))
}

async fn update_experiment_metrics(ctx: Arc<Context>, ns: &str) -> Result<(), Error> {
    let experiments: Api<Experiment> = Api::namespaced(ctx.client.clone(), ns);
    let mut counts: BTreeMap<(String, String), f64> = BTreeMap::new();

    // Rebuild both gauges from scratch each pass so removed experiments drop out.
    metrics::EXPERIMENT_METRIC.reset();
    for experiment in experiments.list(&ListParams::default()).await? {
        let campaign = experiment.spec.campaign_ref.clone();
        let phase = experiment
            .status
            .as_ref()
            .map(|status| format!("{:?}", status.phase))
            .unwrap_or_else(|| "Pending".to_string());

        // Re-publish each experiment's reported metrics as a durable gauge so the
        // dashboard has data even after the short-lived experiment pod is gone.
        if let Some(status) = experiment.status.as_ref() {
            let exp_name = experiment.metadata.name.clone().unwrap_or_default();
            for (metric, value) in &status.metrics {
                let v = match value {
                    serde_json::Value::Number(n) => n.as_f64(),
                    serde_json::Value::String(s) => s.parse().ok(),
                    _ => None,
                };
                if let Some(v) = v {
                    metrics::EXPERIMENT_METRIC
                        .with_label_values(&[ns, exp_name.as_str(), campaign.as_str(), metric])
                        .set(v);
                }
            }
        }

        *counts.entry((campaign, phase)).or_insert(0.0) += 1.0;
    }

    metrics::EXPERIMENTS_TOTAL.reset();
    for ((campaign, phase), count) in counts {
        metrics::EXPERIMENTS_TOTAL
            .with_label_values(&[ns, campaign.as_str(), phase.as_str()])
            .set(count);
    }

    Ok(())
}

async fn ensure_experiment_job(
    experiment: &Experiment,
    ctx: Arc<Context>,
    ns: &str,
    name: &str,
) -> Result<(), Error> {
    let campaigns: Api<ResearchCampaign> = Api::namespaced(ctx.client.clone(), ns);
    let campaign = campaigns
        .get_opt(&experiment.spec.campaign_ref)
        .await?
        .ok_or_else(|| Error::MissingRef {
            kind: "ResearchCampaign",
            namespace: ns.to_string(),
            name: experiment.spec.campaign_ref.clone(),
        })?;

    let templates: Api<ExperimentTemplate> = Api::namespaced(ctx.client.clone(), ns);
    let template = templates
        .get_opt(&campaign.spec.template_ref)
        .await?
        .ok_or_else(|| Error::MissingRef {
            kind: "ExperimentTemplate",
            namespace: ns.to_string(),
            name: campaign.spec.template_ref.clone(),
        })?;

    let profiles: Api<RuntimeProfile> = Api::namespaced(ctx.client.clone(), ns);
    let applied_profile = profiles
        .get_opt(&template.spec.runtime_profile_ref)
        .await?
        .ok_or_else(|| Error::MissingRef {
            kind: "RuntimeProfile",
            namespace: ns.to_string(),
            name: template.spec.runtime_profile_ref.clone(),
        })?;
    // Operator-embedded defaults deep-merged UNDER the applied manifest; user
    // fields win at any depth. Everything below sees the effective profile.
    let profile = effective_profile(&applied_profile);

    if profile.spec.runtime.mode != ExecutionMode::BatchJob {
        return Err(Error::UnsupportedRuntimeMode(
            template.spec.runtime_profile_ref.clone(),
        ));
    }

    let sky_burst = profile.spec.sky.as_ref().is_some_and(|sky| sky.enabled);
    let job_name = format!("exp-{}", name);
    let workspace_path = format!("/workspace/runs/{}/{}", experiment.spec.campaign_ref, name);
    let metrics_path = resolve_metrics_path(&workspace_path, &template.spec.metrics.parser.path);
    let jobs: Api<Job> = Api::namespaced(ctx.client.clone(), ns);
    // A cloud burst runs on the SkyPilot cluster's own disk — no on-prem
    // workspace PVC is created for it.
    let workspace_claim = if sky_burst {
        WorkspaceClaimStatus {
            message: format!(
                "sky launcher Job {job_name} bursts this experiment to the cloud via SkyPilot"
            ),
            condition: condition(
                "WorkspaceClaimReady",
                "Unknown",
                "SkyBurst",
                "cloud burst runs on the SkyPilot cluster's local disk; no workspace PVC is created",
            ),
        }
    } else {
        ensure_workspace_claim(ctx.clone(), ns, &profile).await?
    };
    if jobs.get_opt(&job_name).await?.is_some() {
        // Job already exists: reconcile_experiment_status owns the phase from
        // here. Re-stamping the initial Running status on every pass would
        // ping-pong with its Preparing verdict while a pod is unschedulable
        // (each patch re-triggering the watch → a permanent flap).
        return Ok(());
    }
    let job = if sky_burst {
        build_sky_launcher_job(
            experiment,
            &profile,
            &job_name,
            &workspace_path,
            &metrics_path,
            ns,
            name,
        )?
    } else {
        build_job(
            experiment,
            &profile,
            &job_name,
            &workspace_path,
            &metrics_path,
            ns,
            name,
        )
    };
    jobs.create(&PostParams::default(), &job).await?;

    let status = ExperimentStatus {
        phase: ExperimentPhase::Running,
        workspace_path: Some(workspace_path.clone()),
        job_name: Some(job_name.clone()),
        logs_link: Some(format!(
            r#"https://grafana.casazza.io/explore?left={{"datasource":"Loki","queries":[{{"expr":"{{namespace=\"{}\",job_name=\"{}\"}}"}}]}}"#,
            ns, job_name
        )),
        metrics_link: Some("https://grafana.casazza.io/d/athena-athena-experiment-debugging/athena-experiment-debugging".to_string()),
        dashboard: template.spec.dashboard.clone(),
        // Seed the objective from the authoritative ExperimentTemplate (already
        // fetched above) so status shows the right metric/goal even before any
        // metrics arrive, and for workloads that emit no metric_series header.
        metrics_detail: Some(ExperimentMetrics {
            objective_name: Some(template.spec.objective.metric.clone()),
            objective_goal: Some(objective_goal_str(&template.spec.objective.goal).to_string()),
            ..Default::default()
        }),
        // Self-documentation artifacts live at deterministic paths on the shared
        // workspace PVC. The runner writes them; we stamp the addresses at creation
        // so they're queryable immediately and survive later status merges (the
        // terminal-metrics merge only sets checkpoint/onnx fields, never clears these).
        artifacts: Some(ExperimentArtifacts {
            workspace_uri: Some(workspace_path.clone()),
            checkpoints_uri: Some(format!("{workspace_path}/checkpoints")),
            journal_uri: Some(format!("{workspace_path}/research_journal.jsonl")),
            provenance_uri: Some(format!("{workspace_path}/provenance.json")),
            figures_uri: Some(format!("{workspace_path}/figures")),
            ..Default::default()
        }),
        environment: Some(ExperimentEnvironment {
            namespace: Some(ns.to_string()),
            job_name: Some(job_name),
            ..Default::default()
        }),
        message: Some(workspace_claim.message.clone()),
        conditions: Some(vec![workspace_claim.condition]),
        ..Default::default()
    };
    patch_experiment_status(ctx, ns, name, status).await?;

    Ok(())
}

struct WorkspaceClaimStatus {
    message: String,
    condition: ExperimentCondition,
}

async fn ensure_workspace_claim(
    ctx: Arc<Context>,
    ns: &str,
    profile: &RuntimeProfile,
) -> Result<WorkspaceClaimStatus, Error> {
    let storage = &profile.spec.storage;
    if !storage.create_workspace_claim {
        return Ok(WorkspaceClaimStatus {
            message: format!(
                "workspace PVC {} is expected to exist in namespace {ns}",
                storage.workspace_claim_name
            ),
            condition: condition(
                "WorkspaceClaimReady",
                "Unknown",
                "WorkspaceClaimExternallyManaged",
                format!(
                    "workspace PVC {} is externally managed and must exist in namespace {ns}",
                    storage.workspace_claim_name
                ),
            ),
        });
    }

    let claims: Api<PersistentVolumeClaim> = Api::namespaced(ctx.client.clone(), ns);
    if claims
        .get_opt(&storage.workspace_claim_name)
        .await?
        .is_some()
    {
        return Ok(WorkspaceClaimStatus {
            message: format!(
                "workspace PVC {}/{} already exists; Kubernetes Job is ready to use it",
                ns, storage.workspace_claim_name
            ),
            condition: workspace_ready_condition(
                ns,
                &storage.workspace_claim_name,
                "WorkspaceClaimExists",
            ),
        });
    }

    let mut requests = BTreeMap::new();
    requests.insert(
        "storage".to_string(),
        Quantity(storage.workspace_size.clone()),
    );

    let labels = BTreeMap::from([
        (
            "app.kubernetes.io/name".to_string(),
            "athena-workspace".to_string(),
        ),
        (
            "athena.nixlab.io/runtime-profile".to_string(),
            profile.name_any(),
        ),
    ]);

    claims
        .create(
            &PostParams::default(),
            &PersistentVolumeClaim {
                metadata: ObjectMeta {
                    name: Some(storage.workspace_claim_name.clone()),
                    namespace: Some(ns.to_string()),
                    labels: Some(labels),
                    ..Default::default()
                },
                spec: Some(PersistentVolumeClaimSpec {
                    access_modes: Some(storage.workspace_access_modes.clone()),
                    resources: Some(VolumeResourceRequirements {
                        requests: Some(requests),
                        ..Default::default()
                    }),
                    storage_class_name: storage.workspace_storage_class_name.clone(),
                    ..Default::default()
                }),
                ..Default::default()
            },
        )
        .await?;

    Ok(WorkspaceClaimStatus {
        message: format!(
            "created workspace PVC {}/{} before creating the Kubernetes Job",
            ns, storage.workspace_claim_name
        ),
        condition: workspace_ready_condition(
            ns,
            &storage.workspace_claim_name,
            "WorkspaceClaimCreated",
        ),
    })
}

fn workspace_ready_condition(ns: &str, claim_name: &str, reason: &str) -> ExperimentCondition {
    condition(
        "WorkspaceClaimReady",
        "True",
        reason,
        format!("workspace PVC {ns}/{claim_name} exists for the experiment Job"),
    )
}

/// Canonical lowercase string for an objective goal, matching the `goal` value
/// workloads emit in their `metric_series` header.
fn objective_goal_str(goal: &ObjectiveGoal) -> &'static str {
    match goal {
        ObjectiveGoal::Minimize => "minimize",
        ObjectiveGoal::Maximize => "maximize",
    }
}

fn resolve_metrics_path(workspace_path: &str, metrics_path: &str) -> String {
    if metrics_path.starts_with('/') {
        metrics_path.to_string()
    } else {
        format!("{workspace_path}/{metrics_path}")
    }
}

async fn reconcile_experiment_status(
    experiment: &Experiment,
    ctx: Arc<Context>,
    ns: &str,
    name: &str,
) -> Result<(), Error> {
    let current = experiment.status.clone().unwrap_or_default();
    // Terminal experiments are immutable records: their Job may be GC'd or
    // deleted after the fact (seen with the 2026-07-15 etcd-snapshot
    // restore, where this check flipped restored succeeded/failed records
    // to error/JobMissing). Nothing below can improve a terminal status.
    if matches!(
        current.phase,
        ExperimentPhase::Succeeded | ExperimentPhase::Failed
    ) {
        return Ok(());
    }
    let job_name = current
        .job_name
        .clone()
        .unwrap_or_else(|| format!("exp-{}", name));
    let jobs: Api<Job> = Api::namespaced(ctx.client.clone(), ns);
    let Some(job) = jobs.get_opt(&job_name).await? else {
        if experiment.status.is_some() {
            patch_experiment_status(
                ctx,
                ns,
                name,
                ExperimentStatus {
                    phase: ExperimentPhase::Error,
                    job_name: Some(job_name.clone()),
                    workspace_path: current.workspace_path,
                    logs_link: current.logs_link,
                    metrics_link: current.metrics_link,
                    metrics: current.metrics,
                    decision: current.decision,
                    environment: current.environment,
                    resources: current.resources,
                    metrics_detail: current.metrics_detail,
                    artifacts: current.artifacts,
                    cost: current.cost,
                    dashboard: current.dashboard,
                    latest_checkpoint: current.latest_checkpoint,
                    checkpoints: current.checkpoints,
                    message: Some(format!("expected Kubernetes Job {job_name} was not found")),
                    conditions: Some(vec![condition(
                        "JobObserved",
                        "False",
                        "JobMissing",
                        format!("expected Kubernetes Job {job_name} was not found"),
                    )]),
                },
            )
            .await?;
        }
        return Ok(());
    };

    let pods = pods_for_job(ctx.clone(), ns, &job).await?;
    let terminal_metrics = pods.iter().find_map(termination_metrics_json);
    let (phase, message, conditions) = status_from_job_and_pods(&job, &pods);
    let pod_names: Vec<String> = pods
        .iter()
        .filter_map(|pod| pod.metadata.name.clone())
        .collect();
    let node_names: Vec<String> = pods
        .iter()
        .filter_map(|pod| pod.spec.as_ref()?.node_name.clone())
        .collect();
    let mut environment = current.environment.unwrap_or_default();
    environment.namespace = Some(ns.to_string());
    environment.job_name = Some(job_name.clone());
    if !pod_names.is_empty() {
        environment.pod_names = Some(pod_names);
    }
    if !node_names.is_empty() {
        environment.node_names = Some(node_names);
    }

    let (metrics, metrics_detail, artifacts, latest_checkpoint) = merge_terminal_metrics(
        current.metrics,
        current.metrics_detail,
        current.artifacts,
        current.latest_checkpoint,
        current.workspace_path.as_deref(),
        terminal_metrics.as_ref(),
    );

    patch_experiment_status(
        ctx,
        ns,
        name,
        ExperimentStatus {
            phase,
            job_name: Some(job_name),
            workspace_path: current.workspace_path,
            logs_link: current.logs_link,
            metrics_link: current.metrics_link,
            metrics,
            decision: current.decision,
            message: Some(message),
            environment: Some(environment),
            resources: current.resources,
            metrics_detail,
            artifacts,
            cost: current.cost,
            conditions: Some(conditions),
            dashboard: current.dashboard,
            latest_checkpoint,
            checkpoints: current.checkpoints,
        },
    )
    .await?;

    Ok(())
}

async fn pods_for_job(ctx: Arc<Context>, ns: &str, job: &Job) -> Result<Vec<Pod>, Error> {
    let job_name = job.name_any();
    let pods: Api<Pod> = Api::namespaced(ctx.client.clone(), ns);
    let list = pods
        .list(&ListParams::default().labels(&format!("batch.kubernetes.io/job-name={job_name}")))
        .await?;
    let Some(job_uid) = job.metadata.uid.as_deref() else {
        return Ok(list.items);
    };

    Ok(list
        .items
        .into_iter()
        .filter(|pod| {
            pod.metadata
                .labels
                .as_ref()
                .and_then(|labels| labels.get("batch.kubernetes.io/controller-uid"))
                .map(|uid| uid == job_uid)
                .unwrap_or(false)
        })
        .collect())
}

fn termination_metrics_json(pod: &Pod) -> Option<Value> {
    pod.status
        .as_ref()?
        .container_statuses
        .as_ref()?
        .iter()
        .filter_map(|status| {
            status
                .state
                .as_ref()?
                .terminated
                .as_ref()?
                .message
                .as_deref()
        })
        .find_map(|message| serde_json::from_str::<Value>(message).ok())
}

/// Artifact/checkpoint keys the trainer emits in its termination summary. These
/// are addresses, not metrics — they must not leak into `status.metrics` (or the
/// `latest`/`best` scalar snapshot); they are routed to artifacts/latestCheckpoint.
const ARTIFACT_KEYS: [&str; 3] = ["latest_checkpoint", "best_checkpoint", "onnx_uri"];

fn merge_terminal_metrics(
    current_metrics: BTreeMap<String, Value>,
    current_detail: Option<ExperimentMetrics>,
    current_artifacts: Option<ExperimentArtifacts>,
    current_latest_checkpoint: Option<CheckpointRef>,
    workspace_path: Option<&str>,
    raw: Option<&Value>,
) -> (
    BTreeMap<String, Value>,
    Option<ExperimentMetrics>,
    Option<ExperimentArtifacts>,
    Option<CheckpointRef>,
) {
    let Some(raw) = raw else {
        return (
            current_metrics,
            current_detail,
            current_artifacts,
            current_latest_checkpoint,
        );
    };

    // Copy every top-level scalar field the workload emitted, keeping the
    // operator domain-agnostic: a TSP run reports `tour_length`, a finance run
    // `sharpe_ratio`, an ML run `val_bpb`, etc. (Previously a hardcoded ML
    // allowlist silently dropped any metric whose name it didn't recognize.)
    // Artifact-address keys are excluded — they are not metrics.
    let mut metrics = current_metrics;
    let scalars: serde_json::Map<String, Value> = raw
        .as_object()
        .map(|obj| {
            obj.iter()
                .filter(|(k, v)| {
                    !ARTIFACT_KEYS.contains(&k.as_str())
                        && (v.is_number() || v.is_string() || v.is_boolean())
                })
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect()
        })
        .unwrap_or_default();
    for (name, value) in &scalars {
        metrics.insert(name.clone(), value.clone());
    }

    // The full scalar snapshot is the "latest" sample; the workload owns which
    // metrics it reports, so don't cherry-pick field names.
    let latest = Value::Object(scalars);
    let series = metric_series(raw);
    let mut detail = current_detail.unwrap_or_default();
    // Objective name/goal come from the workload's own metric_series header,
    // not a hardcoded `val_bpb`/`minimize`. Leave any prior value untouched if
    // the workload emitted no series.
    if let Some(first) = series.first() {
        detail.objective_name = Some(first.objective.clone());
        detail.objective_goal = Some(first.goal.clone());
    }
    detail.latest = Some(latest.clone());
    detail.best = Some(latest);
    detail.metrics_path = workspace_path.map(|path| format!("{path}/metrics.json"));
    detail.series = series;
    detail.last_updated = Some(chrono::Utc::now().to_rfc3339());

    // Route artifact addresses out of metrics and into the typed status fields the
    // trainer writes on success. Keep any prior value if the workload omitted one.
    let str_field = |key: &str| raw.get(key).and_then(Value::as_str).map(str::to_string);
    let latest_checkpoint = str_field("latest_checkpoint")
        .map(|uri| CheckpointRef {
            uri,
            ..Default::default()
        })
        .or(current_latest_checkpoint);
    let mut artifacts = current_artifacts.unwrap_or_default();
    if let Some(uri) = str_field("onnx_uri") {
        artifacts.onnx_uri = Some(uri);
    }
    if let Some(uri) = str_field("best_checkpoint") {
        artifacts.best_checkpoint_uri = Some(uri);
    }
    // The checkpoint root is the directory the operator injects as
    // ATHENA_CHECKPOINT_DIR; latestCheckpoint.uri points at a resumable subdir.
    if let Some(path) = workspace_path {
        artifacts.checkpoints_uri = Some(format!("{path}/checkpoints"));
    }

    (metrics, Some(detail), Some(artifacts), latest_checkpoint)
}

fn metric_series(raw: &Value) -> Vec<ExperimentMetricSeries> {
    let Some(series) = raw.get("metric_series") else {
        return Vec::new();
    };
    let tag = series
        .get("tag")
        .and_then(Value::as_str)
        .unwrap_or("canary")
        .to_string();
    let iteration = series
        .get("iteration")
        .and_then(Value::as_i64)
        .unwrap_or_default();
    let objective = series
        .get("objective")
        .and_then(Value::as_str)
        .unwrap_or("val_bpb")
        .to_string();
    let goal = series
        .get("goal")
        .and_then(Value::as_str)
        .unwrap_or("minimize")
        .to_string();
    let points = series
        .get("points")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|point| {
            Some(ExperimentMetricPoint {
                name: point.get("name")?.as_str()?.to_string(),
                value: point.get("value")?.as_f64()?,
                step: point.get("step").and_then(Value::as_i64),
            })
        })
        .collect();

    vec![ExperimentMetricSeries {
        tag,
        iteration,
        objective,
        goal,
        points,
    }]
}

fn status_from_job_and_pods(
    job: &Job,
    pods: &[Pod],
) -> (ExperimentPhase, String, Vec<ExperimentCondition>) {
    if job_condition_is_true(job, "Complete") {
        return (
            ExperimentPhase::Succeeded,
            "Kubernetes Job completed successfully".to_string(),
            vec![condition(
                "Completed",
                "True",
                "JobComplete",
                "Kubernetes Job completed successfully",
            )],
        );
    }

    // A pod lost to preemption / node drain / eviction is infrastructure churn,
    // not a workload failure. As long as the Job hasn't exhausted its backoff
    // budget it will recreate the pod, so keep the experiment retryable instead
    // of stamping Failed.
    if !job_condition_is_true(job, "Failed") {
        if let Some((reason, message)) = first_preempted_pod(pods) {
            return (
                ExperimentPhase::Running,
                message.clone(),
                vec![condition("Retrying", "True", reason, message)],
            );
        }
    }

    if job_condition_is_true(job, "Failed") {
        return (
            ExperimentPhase::Failed,
            "Kubernetes Job failed".to_string(),
            vec![condition(
                "Failed",
                "True",
                "JobFailed",
                "Kubernetes Job failed",
            )],
        );
    }

    if let Some((reason, message)) = first_unschedulable_pod(pods) {
        return (
            ExperimentPhase::Preparing,
            message.clone(),
            vec![condition("Scheduled", "False", reason, message)],
        );
    }

    if pods
        .iter()
        .any(|pod| pod.status.as_ref().and_then(|s| s.phase.as_deref()) == Some("Running"))
    {
        return (
            ExperimentPhase::Running,
            "Kubernetes Job has a running Pod".to_string(),
            vec![condition(
                "Running",
                "True",
                "PodRunning",
                "Kubernetes Job has a running Pod",
            )],
        );
    }

    if pods
        .iter()
        .any(|pod| pod.status.as_ref().and_then(|s| s.phase.as_deref()) == Some("Pending"))
    {
        return (
            ExperimentPhase::Preparing,
            "Kubernetes Job Pod is pending".to_string(),
            vec![condition(
                "Scheduled",
                "Unknown",
                "PodPending",
                "Kubernetes Job Pod is pending",
            )],
        );
    }

    if job
        .status
        .as_ref()
        .and_then(|status| status.active)
        .unwrap_or_default()
        > 0
    {
        return (
            ExperimentPhase::Running,
            "Kubernetes Job is active".to_string(),
            vec![condition(
                "Running",
                "True",
                "JobActive",
                "Kubernetes Job is active",
            )],
        );
    }

    (
        ExperimentPhase::Preparing,
        "Kubernetes Job has been created and is waiting for Pods".to_string(),
        vec![condition(
            "Scheduled",
            "Unknown",
            "WaitingForPods",
            "Kubernetes Job has been created and is waiting for Pods",
        )],
    )
}

fn job_condition_is_true(job: &Job, condition_type: &str) -> bool {
    job.status
        .as_ref()
        .and_then(|status| status.conditions.as_ref())
        .map(|conditions| {
            conditions
                .iter()
                .any(|condition| condition.type_ == condition_type && condition.status == "True")
        })
        .unwrap_or(false)
}

/// Detect a pod that was terminated by preemption, node deletion/shutdown, or
/// eviction. Returns `(reason, message)` where `reason` is the `FailureClass`
/// recorded on the retry condition. The `DisruptionTarget` pod condition is set
/// by the control plane for scheduler preemption, taint-manager deletion, node
/// shutdown, and pod GC; `status.reason` covers older kubelet eviction paths.
fn first_preempted_pod(pods: &[Pod]) -> Option<(String, String)> {
    let preempted = format!("{:?}", FailureClass::Preempted);
    pods.iter().find_map(|pod| {
        let status = pod.status.as_ref()?;
        if let Some(disruption) = status.conditions.as_ref().and_then(|conditions| {
            conditions
                .iter()
                .find(|c| c.type_ == "DisruptionTarget" && c.status == "True")
        }) {
            let detail = disruption
                .reason
                .clone()
                .or_else(|| disruption.message.clone())
                .unwrap_or_else(|| "DisruptionTarget".to_string());
            return Some((
                preempted.clone(),
                format!("pod disrupted ({detail}); retrying within backoff budget"),
            ));
        }
        let reason = status.reason.as_deref()?;
        if matches!(
            reason,
            "Evicted" | "NodeLost" | "Shutdown" | "NodeShutdown" | "Terminated" | "Preempted"
        ) {
            return Some((
                preempted.clone(),
                format!("pod {reason}; retrying within backoff budget"),
            ));
        }
        None
    })
}

fn first_unschedulable_pod(pods: &[Pod]) -> Option<(String, String)> {
    pods.iter()
        .filter_map(|pod| pod.status.as_ref()?.conditions.as_ref())
        .flat_map(|conditions| conditions.iter())
        .find(|condition| {
            condition.type_ == "PodScheduled"
                && condition.status == "False"
                && condition.reason.as_deref() == Some("Unschedulable")
        })
        .map(|condition| {
            (
                condition
                    .reason
                    .clone()
                    .unwrap_or_else(|| "Unschedulable".to_string()),
                condition
                    .message
                    .clone()
                    .unwrap_or_else(|| "Pod is unschedulable".to_string()),
            )
        })
}

fn condition(
    condition_type: impl Into<String>,
    status: impl Into<String>,
    reason: impl Into<String>,
    message: impl Into<String>,
) -> ExperimentCondition {
    ExperimentCondition {
        condition_type: Some(condition_type.into()),
        status: Some(status.into()),
        reason: Some(reason.into()),
        message: Some(message.into()),
        last_transition_time: Some(chrono::Utc::now().to_rfc3339()),
    }
}

async fn patch_experiment_status(
    ctx: Arc<Context>,
    ns: &str,
    name: &str,
    status: ExperimentStatus,
) -> Result<(), Error> {
    let experiments: Api<Experiment> = Api::namespaced(ctx.client.clone(), ns);
    experiments
        .patch_status(
            name,
            &PatchParams::apply("athena"),
            &Patch::Merge(&json!({ "status": status })),
        )
        .await?;
    Ok(())
}

/// Apply the operator-embedded defaults layer to an applied RuntimeProfile:
/// serialize the manifest's spec to a JSON tree, deep-merge the defaults under
/// it (user fields win at any depth), and deserialize back. Falls back to the
/// manifest as-is if the merged tree fails to deserialize (should not happen —
/// the merge only adds well-typed fields).
fn effective_profile(profile: &RuntimeProfile) -> RuntimeProfile {
    let raw = match serde_json::to_value(&profile.spec) {
        Ok(raw) => raw,
        Err(err) => {
            warn!(profile = %profile.name_any(), %err, "failed to serialize RuntimeProfile spec for defaulting; using manifest as-is");
            return profile.clone();
        }
    };
    let merged = athena_api::defaults::apply_runtime_profile_defaults(raw);
    match serde_json::from_value(merged) {
        Ok(spec) => RuntimeProfile {
            metadata: profile.metadata.clone(),
            spec,
            status: profile.status.clone(),
        },
        Err(err) => {
            warn!(profile = %profile.name_any(), %err, "failed to deserialize defaulted RuntimeProfile spec; using manifest as-is");
            profile.clone()
        }
    }
}

/// Common labels for both the direct experiment Job and the sky launcher Job,
/// including the Kueue queue label when the profile opts in.
fn experiment_labels(
    experiment: &Experiment,
    profile: &RuntimeProfile,
    experiment_name: &str,
) -> BTreeMap<String, String> {
    let mut labels = BTreeMap::from([
        (
            "app.kubernetes.io/name".to_string(),
            "athena-experiment".to_string(),
        ),
        (
            "athena.nixlab.io/campaign".to_string(),
            experiment.spec.campaign_ref.clone(),
        ),
        (
            "athena.nixlab.io/experiment".to_string(),
            experiment_name.to_string(),
        ),
        (
            "athena.nixlab.io/runtime-profile".to_string(),
            profile.name_any(),
        ),
    ]);
    // Kueue admission: opt in by labeling the Job with its LocalQueue. Kueue
    // only manages Jobs carrying this label (manageJobsWithoutQueueName=false),
    // so leaving queue_name unset preserves direct scheduling.
    if let Some(queue) = &profile.spec.scheduling.queue_name {
        labels.insert("kueue.x-k8s.io/queue-name".to_string(), queue.clone());
    }
    labels
}

/// The experiment's environment contract, identical for the on-prem Job path
/// and the sky task a launcher renders: operator-managed vars first, then
/// checkpointing controls, then the RuntimeProfile's additive env, then
/// experiment-level overrides. Operator vars stay authoritative.
fn experiment_env(
    experiment: &Experiment,
    profile: &RuntimeProfile,
    workspace_path: &str,
    metrics_path: &str,
    namespace: &str,
    experiment_name: &str,
) -> Vec<EnvVar> {
    let spec_json = serde_json::to_string(&experiment.spec).unwrap_or_else(|_| "{}".to_string());
    let metrics_endpoint = &profile.spec.metrics_endpoint;
    let checkpoint_dir = format!("{workspace_path}/checkpoints");

    // Operator-managed env first, then checkpointing controls, then the
    // RuntimeProfile's additive env. Operator vars stay authoritative.
    let mut container_env = vec![
        EnvVar {
            name: "ATHENA_EXPERIMENT_SPEC".to_string(),
            value: Some(spec_json),
            ..Default::default()
        },
        EnvVar {
            name: "ATHENA_EXPERIMENT".to_string(),
            value: Some(experiment_name.to_string()),
            ..Default::default()
        },
        EnvVar {
            name: "ATHENA_CAMPAIGN".to_string(),
            value: Some(experiment.spec.campaign_ref.clone()),
            ..Default::default()
        },
        EnvVar {
            name: "ATHENA_NAMESPACE".to_string(),
            value: Some(namespace.to_string()),
            ..Default::default()
        },
        EnvVar {
            name: "ATHENA_WORKSPACE_PATH".to_string(),
            value: Some(workspace_path.to_string()),
            ..Default::default()
        },
        EnvVar {
            name: "ATHENA_METRICS_PATH".to_string(),
            value: Some(metrics_path.to_string()),
            ..Default::default()
        },
        // Live metrics integration. The job exposes a Prometheus-text endpoint on
        // ATHENA_METRICS_PORT (scraped by the athena-experiment PodMonitor) for
        // transparent store, and queries ATHENA_PROMETHEUS_URL for live
        // display-retrieve. metrics.json stays the authoritative final summary.
        EnvVar {
            name: "ATHENA_METRICS_PORT".to_string(),
            value: Some(metrics_endpoint.port.to_string()),
            ..Default::default()
        },
        EnvVar {
            name: "ATHENA_METRICS_SCRAPE_PATH".to_string(),
            value: Some(metrics_endpoint.path.clone()),
            ..Default::default()
        },
        EnvVar {
            name: "ATHENA_METRICS_ENABLED".to_string(),
            value: Some(metrics_endpoint.enabled.to_string()),
            ..Default::default()
        },
        EnvVar {
            name: "ATHENA_PROMETHEUS_URL".to_string(),
            value: Some(
                metrics_endpoint
                    .prometheus_query_url
                    .clone()
                    .unwrap_or_default(),
            ),
            ..Default::default()
        },
        // Where the trainer writes checkpoints; this is the artifacts.checkpointsUri
        // root, and latestCheckpoint.uri points at a resumable subdir under it.
        EnvVar {
            name: "ATHENA_CHECKPOINT_DIR".to_string(),
            value: Some(checkpoint_dir),
            ..Default::default()
        },
    ];
    if let Some(policy) = experiment.spec.checkpoint_policy.as_ref() {
        if let Some(interval) = policy.interval_seconds {
            container_env.push(EnvVar {
                name: "ATHENA_CHECKPOINT_INTERVAL".to_string(),
                value: Some(interval.to_string()),
                ..Default::default()
            });
        }
        if let Some(resume_from) = policy.resume_from.as_ref() {
            container_env.push(EnvVar {
                name: "ATHENA_RESUME_FROM".to_string(),
                value: Some(resume_from.clone()),
                ..Default::default()
            });
        }
    }
    for var in &profile.spec.env {
        container_env.push(EnvVar {
            name: var.name.clone(),
            value: var.value.clone(),
            ..Default::default()
        });
    }
    // Experiment-level env overrides (e.g. campaign-injected LLM_BASE_URL for an
    // ephemeral inference mesh). Override a same-named profile var in place so we
    // never emit duplicate env entries; append otherwise.
    for var in &experiment.spec.env {
        if let Some(existing) = container_env.iter_mut().find(|e| e.name == var.name) {
            existing.value = var.value.clone();
        } else {
            container_env.push(EnvVar {
                name: var.name.clone(),
                value: var.value.clone(),
                ..Default::default()
            });
        }
    }

    container_env
}

fn build_job(
    experiment: &Experiment,
    profile: &RuntimeProfile,
    job_name: &str,
    workspace_path: &str,
    metrics_path: &str,
    namespace: &str,
    experiment_name: &str,
) -> Job {
    let labels = experiment_labels(experiment, profile, experiment_name);
    let metrics_endpoint = &profile.spec.metrics_endpoint;
    let container_env = experiment_env(
        experiment,
        profile,
        workspace_path,
        metrics_path,
        namespace,
        experiment_name,
    );

    // Workspace PVC mount plus any RuntimeProfile secret mounts. Volume names are
    // index-based so two mounts (or the same secret mounted twice) never collide.
    let mut volume_mounts = vec![VolumeMount {
        name: "workspace".to_string(),
        mount_path: profile.spec.storage.workspace_mount_path.clone(),
        ..Default::default()
    }];
    let mut volumes = vec![Volume {
        name: "workspace".to_string(),
        persistent_volume_claim: Some(PersistentVolumeClaimVolumeSource {
            claim_name: profile.spec.storage.workspace_claim_name.clone(),
            ..Default::default()
        }),
        ..Default::default()
    }];
    for (i, mount) in profile.spec.secret_mounts.iter().enumerate() {
        let volume_name = format!("secret-{i}");
        volumes.push(Volume {
            name: volume_name.clone(),
            secret: Some(SecretVolumeSource {
                secret_name: Some(mount.secret_name.clone()),
                ..Default::default()
            }),
            ..Default::default()
        });
        volume_mounts.push(VolumeMount {
            name: volume_name,
            mount_path: mount.mount_path.clone(),
            sub_path: mount.sub_path.clone(),
            ..Default::default()
        });
    }

    let image_pull_secrets = if profile.spec.image_pull_secrets.is_empty() {
        None
    } else {
        Some(
            profile
                .spec
                .image_pull_secrets
                .iter()
                .map(|s| LocalObjectReference {
                    name: s.name.clone(),
                })
                .collect(),
        )
    };

    Job {
        metadata: ObjectMeta {
            name: Some(job_name.to_string()),
            namespace: Some(namespace.to_string()),
            labels: Some(labels.clone()),
            // GC the Job (and its pods) with the Experiment; without this a
            // deleted Experiment orphans its Job and a recreated one adopts the
            // stale Job spec instead of rebuilding it.
            owner_references: kube::Resource::controller_owner_ref(experiment, &())
                .map(|r| vec![r]),
            ..Default::default()
        },
        spec: Some(JobSpec {
            // Small >0 budget so a pod lost to preemption / node scale-down is
            // recreated instead of failing the whole experiment.
            backoff_limit: Some(3),
            // Wall-clock cap so a *hung* (non-crashing) trainer can't squat a GPU
            // forever — backoff_limit only catches crashes, not freezes. A real
            // RLlib freeze once burned a node for 32h at 0% progress. 8h matches
            // the sky ttl default; any single on-prem experiment (15M timesteps
            // ~2-5h) finishes well under it. ponytail: blanket cap; make it a
            // RuntimeProfile field if a legit run ever needs >8h.
            active_deadline_seconds: Some(8 * 3600),
            // Kueue requires managed Jobs to start suspended; it unsuspends on
            // admission. Only when a queue is set — otherwise schedule directly.
            suspend: profile.spec.scheduling.queue_name.as_ref().map(|_| true),
            template: PodTemplateSpec {
                metadata: Some(ObjectMeta {
                    labels: Some(labels),
                    ..Default::default()
                }),
                spec: Some(PodSpec {
                    restart_policy: Some("Never".to_string()),
                    containers: vec![Container {
                        name: "experiment".to_string(),
                        image: Some(profile.spec.image.clone()),
                        image_pull_policy: profile.spec.pull_policy.clone(),
                        command: if profile.spec.command.is_empty() {
                            None
                        } else {
                            Some(profile.spec.command.clone())
                        },
                        args: if profile.spec.args.is_empty() {
                            None
                        } else {
                            Some(profile.spec.args.clone())
                        },
                        env: Some(container_env),
                        ports: if metrics_endpoint.enabled {
                            Some(vec![ContainerPort {
                                name: Some("metrics".to_string()),
                                container_port: metrics_endpoint.port,
                                protocol: Some("TCP".to_string()),
                                ..Default::default()
                            }])
                        } else {
                            None
                        },
                        resources: Some(resource_requirements(&profile.spec.resources)),
                        volume_mounts: Some(volume_mounts),
                        ..Default::default()
                    }],
                    volumes: Some(volumes),
                    image_pull_secrets,
                    node_selector: if profile.spec.scheduling.node_selector.is_empty() {
                        None
                    } else {
                        Some(profile.spec.scheduling.node_selector.clone())
                    },
                    tolerations: if profile.spec.scheduling.tolerations.is_empty() {
                        None
                    } else {
                        Some(
                            profile
                                .spec
                                .scheduling
                                .tolerations
                                .iter()
                                .filter_map(|value| serde_json::from_value(value.clone()).ok())
                                .collect(),
                        )
                    },
                    priority_class_name: profile.spec.scheduling.priority_class_name.clone(),
                    runtime_class_name: profile.spec.scheduling.runtime_class_name.clone(),
                    ..Default::default()
                }),
            },
            ..Default::default()
        }),
        ..Default::default()
    }
}

/// Default image for the SkyPilot launcher container (built from
/// Dockerfile.sky: python + skypilot[gcp] + gcloud). Overridable per-operator
/// via the ATHENA_SKY_LAUNCHER_IMAGE env var.
const DEFAULT_SKY_LAUNCHER_IMAGE: &str = "ghcr.io/olivecasazza/athena-sky-launcher:latest";

fn sky_launcher_image() -> String {
    std::env::var("ATHENA_SKY_LAUNCHER_IMAGE")
        .unwrap_or_else(|_| DEFAULT_SKY_LAUNCHER_IMAGE.to_string())
}

/// Launcher entrypoint: write the operator-rendered task file, `sky launch`
/// under a hard `timeout` (the TTL), then tear the cluster down
/// unconditionally. `sky launch` streams the task's logs in the foreground, so
/// the cloud run's output lands in this Job's log. `--down` autostops after
/// the task; `sky down` after the (possibly timed-out) launch is the zombie
/// guard — a burst never outlives its TTL.
const SKY_LAUNCHER_SCRIPT: &str = r#"set -u
printf '%s\n' "$SKY_TASK" > /tmp/task.yaml
echo '--- sky task ---'
cat /tmp/task.yaml
echo '-----------------'
timeout "${SKY_TTL_SECONDS}s" sky launch --yes --down \
  --idle-minutes-to-autostop "$SKY_IDLE_MINUTES" \
  --cluster "$SKY_CLUSTER_NAME" /tmp/task.yaml
status=$?
sky down --yes "$SKY_CLUSTER_NAME" || true
exit "$status"
"#;

/// Sky cluster names must be DNS-ish; experiment names already are, but keep a
/// defensive sanitize (lowercase, [a-z0-9-]) so a weird name can't break `sky
/// down` and leak a cluster.
fn sky_cluster_name(experiment_name: &str) -> String {
    let sanitized: String = experiment_name
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    format!("athena-{}", sanitized.trim_matches('-'))
}

/// POSIX single-quote shell quoting for the sky task's `run` line.
fn shell_quote(part: &str) -> String {
    if !part.is_empty()
        && part
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_./=:@%+,".contains(c))
    {
        part.to_string()
    } else {
        format!("'{}'", part.replace('\'', "'\\''"))
    }
}

fn shell_join<'a>(parts: impl Iterator<Item = &'a String>) -> String {
    parts
        .map(|part| shell_quote(part))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Render the SkyPilot task for an experiment: the same container image,
/// command, and env contract the on-prem Job path would use, expressed as a
/// sky task file. Emitted as pretty JSON — JSON is a YAML subset, so sky's
/// yaml.safe_load reads it directly and we avoid a yaml dependency.
fn render_sky_task(
    profile: &RuntimeProfile,
    sky: &SkySpec,
    task_name: &str,
    env: &[EnvVar],
) -> Result<String, Error> {
    if profile.spec.command.is_empty() && profile.spec.args.is_empty() {
        return Err(Error::SkyCommandRequired(profile.name_any()));
    }
    let run = shell_join(profile.spec.command.iter().chain(profile.spec.args.iter()));

    let mut resources = serde_json::Map::new();
    // Run the task inside the profile's container image so the cloud node
    // executes exactly what an on-prem pod would.
    resources.insert(
        "image_id".to_string(),
        json!(format!("docker:{}", profile.spec.image)),
    );
    resources.insert("use_spot".to_string(), json!(sky.use_spot));
    if let Some(accelerators) = &sky.accelerators {
        resources.insert("accelerators".to_string(), json!(accelerators));
    }
    // No pin => SkyPilot's ordered/cheapest cloud selection.
    if let Some(cloud) = &sky.cloud {
        resources.insert("cloud".to_string(), json!(cloud));
    }

    let envs: serde_json::Map<String, Value> = env
        .iter()
        .map(|var| {
            (
                var.name.clone(),
                json!(var.value.clone().unwrap_or_default()),
            )
        })
        .collect();

    let task = json!({
        "name": task_name,
        "resources": Value::Object(resources),
        "envs": Value::Object(envs),
        "run": run,
    });
    // Serializing a serde_json::Value cannot fail (string keys, no NaN); the
    // fallback keeps the operator panic-free regardless.
    Ok(serde_json::to_string_pretty(&task).unwrap_or_else(|_| task.to_string()))
}

/// Build the LAUNCHER Job for a sky-enabled profile. The launcher — not the
/// cloud node — is what Kueue admits: it carries the profile's queue label and
/// starts suspended, so cloud bursts flow through the same quota accounting as
/// on-prem runs (quota then means "concurrent experiments", not local GPUs —
/// intended). The launcher itself is tiny and fixed-size; the profile's
/// resources describe the workload, which runs on the SkyPilot cluster.
fn build_sky_launcher_job(
    experiment: &Experiment,
    profile: &RuntimeProfile,
    job_name: &str,
    workspace_path: &str,
    metrics_path: &str,
    namespace: &str,
    experiment_name: &str,
) -> Result<Job, Error> {
    let sky = profile.spec.sky.clone().unwrap_or_default();
    let mut labels = experiment_labels(experiment, profile, experiment_name);
    labels.insert(
        "athena.nixlab.io/sky-launcher".to_string(),
        "true".to_string(),
    );

    let cluster_name = sky_cluster_name(experiment_name);
    let task_env = experiment_env(
        experiment,
        profile,
        workspace_path,
        metrics_path,
        namespace,
        experiment_name,
    );
    let task = render_sky_task(profile, &sky, &cluster_name, &task_env)?;

    let ttl_minutes = sky.ttl_minutes.unwrap_or(480) as i64;
    let idle_minutes = sky.idle_minutes_to_autostop.unwrap_or(30);
    let launcher_env = vec![
        EnvVar {
            name: "SKY_TASK".to_string(),
            value: Some(task),
            ..Default::default()
        },
        EnvVar {
            name: "SKY_CLUSTER_NAME".to_string(),
            value: Some(cluster_name),
            ..Default::default()
        },
        EnvVar {
            name: "SKY_TTL_SECONDS".to_string(),
            value: Some((ttl_minutes * 60).to_string()),
            ..Default::default()
        },
        EnvVar {
            name: "SKY_IDLE_MINUTES".to_string(),
            value: Some(idle_minutes.to_string()),
            ..Default::default()
        },
    ];

    // Cloud credentials for the sky CLI, injected as env vars.
    let env_from = sky.env_secret_ref.as_ref().map(|secret| {
        vec![EnvFromSource {
            secret_ref: Some(SecretEnvSource {
                name: secret.clone(),
                optional: Some(false),
            }),
            ..Default::default()
        }]
    });

    let launcher_resources = ResourceRequirements {
        requests: Some(BTreeMap::from([
            ("cpu".to_string(), Quantity("250m".to_string())),
            ("memory".to_string(), Quantity("512Mi".to_string())),
        ])),
        ..Default::default()
    };

    Ok(Job {
        metadata: ObjectMeta {
            name: Some(job_name.to_string()),
            namespace: Some(namespace.to_string()),
            labels: Some(labels.clone()),
            owner_references: kube::Resource::controller_owner_ref(experiment, &())
                .map(|r| vec![r]),
            ..Default::default()
        },
        spec: Some(JobSpec {
            // No retries: a blind relaunch would spin up a second cloud
            // cluster / re-bill the task. Failures surface loudly instead.
            backoff_limit: Some(0),
            // Belt-and-braces around the in-script timeout + sky down: the TTL
            // plus margin for provisioning and teardown.
            active_deadline_seconds: Some(ttl_minutes * 60 + 900),
            suspend: profile.spec.scheduling.queue_name.as_ref().map(|_| true),
            template: PodTemplateSpec {
                metadata: Some(ObjectMeta {
                    labels: Some(labels),
                    ..Default::default()
                }),
                spec: Some(PodSpec {
                    restart_policy: Some("Never".to_string()),
                    containers: vec![Container {
                        name: "sky-launcher".to_string(),
                        image: Some(sky_launcher_image()),
                        command: Some(vec![
                            "bash".to_string(),
                            "-c".to_string(),
                            SKY_LAUNCHER_SCRIPT.to_string(),
                        ]),
                        env: Some(launcher_env),
                        env_from,
                        resources: Some(launcher_resources),
                        ..Default::default()
                    }],
                    ..Default::default()
                }),
            },
            ..Default::default()
        }),
        ..Default::default()
    })
}

fn resource_requirements(
    resources: &athena_api::runtime_profile::ResourceProfile,
) -> ResourceRequirements {
    ResourceRequirements {
        limits: quantity_map(&resources.limits),
        requests: quantity_map(&resources.requests),
        ..Default::default()
    }
}

fn quantity_map(values: &BTreeMap<String, String>) -> Option<BTreeMap<String, Quantity>> {
    if values.is_empty() {
        None
    } else {
        Some(
            values
                .iter()
                .map(|(key, value)| (key.clone(), Quantity(value.clone())))
                .collect(),
        )
    }
}

pub fn error_policy(experiment: Arc<Experiment>, err: &Error, _ctx: Arc<Context>) -> Action {
    let name = experiment.metadata.name.as_deref().unwrap_or("unknown");
    warn!(name, %err, "error reconciling experiment, retrying");
    let ns = experiment
        .metadata
        .namespace
        .as_deref()
        .unwrap_or("default");
    let phase = experiment
        .status
        .as_ref()
        .map(|s| format!("{:?}", s.phase))
        .unwrap_or_else(|| "Pending".to_string());
    telemetry::record_reconcile(
        ns,
        &experiment.spec.campaign_ref,
        &phase,
        "error",
        Duration::ZERO,
    );
    Action::requeue(Duration::from_secs(30))
}

#[cfg(test)]
mod tests {
    use super::*;
    use athena_api::experiment::ExperimentSpec;
    use athena_api::runtime_profile::RuntimeProfileSpec;

    fn test_experiment(name: &str) -> Experiment {
        let mut experiment = Experiment::new(
            name,
            ExperimentSpec {
                lineage: None,
                campaign_ref: "camp".to_string(),
                hypothesis: "sky bursts work".to_string(),
                parameters: BTreeMap::new(),
                patch: None,
                checkpoint_policy: None,
                env: Vec::new(),
            },
        );
        experiment.metadata.namespace = Some("athena".to_string());
        experiment
    }

    fn sky_profile() -> RuntimeProfile {
        let spec: RuntimeProfileSpec = serde_json::from_value(
            athena_api::defaults::apply_runtime_profile_defaults(json!({
                "runtime": { "type": "skypilot", "mode": "batchJob" },
                "image": "ghcr.io/example/trainer:v1",
                "command": ["python", "train.py"],
                "args": ["--lr", "3e-4"],
                "scheduling": { "queueName": "athena-local" },
                "sky": { "accelerators": "A10:1" },
            })),
        )
        .expect("profile spec deserializes");
        RuntimeProfile::new("sky-a10", spec)
    }

    // The launcher Job — not the cloud node — is what Kueue admits: it must
    // carry the profile's queue label and start suspended, and its hard TTL
    // must bound the Job via activeDeadlineSeconds.
    #[test]
    fn sky_launcher_flows_through_kueue_with_hard_ttl() {
        let experiment = test_experiment("burst-1");
        let profile = sky_profile();
        let job = build_sky_launcher_job(
            &experiment,
            &profile,
            "exp-burst-1",
            "/workspace/runs/camp/burst-1",
            "/workspace/runs/camp/burst-1/metrics.json",
            "athena",
            "burst-1",
        )
        .expect("launcher job builds");

        let labels = job.metadata.labels.as_ref().expect("labels");
        assert_eq!(
            labels.get("kueue.x-k8s.io/queue-name"),
            Some(&"athena-local".to_string())
        );
        assert_eq!(
            labels.get("athena.nixlab.io/sky-launcher"),
            Some(&"true".to_string())
        );

        let spec = job.spec.as_ref().expect("job spec");
        assert_eq!(spec.suspend, Some(true));
        // Defaulted ttlMinutes=480 → hard deadline of TTL + 15min margin.
        assert_eq!(spec.active_deadline_seconds, Some(480 * 60 + 900));
        // No blind relaunch of cloud clusters.
        assert_eq!(spec.backoff_limit, Some(0));

        // The rendered task carries the profile's image, command, spot flag,
        // and the experiment env contract.
        let container = &spec.template.spec.as_ref().unwrap().containers[0];
        let task_env = container
            .env
            .as_ref()
            .unwrap()
            .iter()
            .find(|e| e.name == "SKY_TASK")
            .expect("SKY_TASK env");
        let task: Value = serde_json::from_str(task_env.value.as_deref().unwrap())
            .expect("task is valid JSON (a YAML subset sky can read)");
        assert_eq!(
            task["resources"]["image_id"],
            json!("docker:ghcr.io/example/trainer:v1")
        );
        assert_eq!(task["resources"]["accelerators"], json!("A10:1"));
        assert_eq!(task["resources"]["use_spot"], json!(true));
        assert!(task["resources"].get("cloud").is_none(), "no cloud pin");
        assert_eq!(task["run"], json!("python train.py --lr 3e-4"));
        assert_eq!(task["envs"]["ATHENA_EXPERIMENT"], json!("burst-1"));
        assert_eq!(
            task["envs"]["ATHENA_WORKSPACE_PATH"],
            json!("/workspace/runs/camp/burst-1")
        );
    }

    // A sky profile without an explicit command cannot render a task (`run` is
    // mandatory; the image entrypoint is invisible to the launcher).
    #[test]
    fn sky_without_command_is_rejected() {
        let mut profile = sky_profile();
        profile.spec.command = Vec::new();
        profile.spec.args = Vec::new();
        let experiment = test_experiment("burst-2");
        let err = build_sky_launcher_job(
            &experiment,
            &profile,
            "exp-burst-2",
            "/workspace/runs/camp/burst-2",
            "/workspace/runs/camp/burst-2/metrics.json",
            "athena",
            "burst-2",
        )
        .expect_err("no command must fail");
        assert!(matches!(err, Error::SkyCommandRequired(_)));
    }

    fn onprem_profile() -> RuntimeProfile {
        let spec: RuntimeProfileSpec = serde_json::from_value(
            athena_api::defaults::apply_runtime_profile_defaults(json!({
                "runtime": { "type": "pytorch", "mode": "batchJob" },
                "image": "ghcr.io/example/trainer:v1",
                "command": ["python", "train.py"],
                "scheduling": { "queueName": "athena-gpu" },
            })),
        )
        .expect("profile spec deserializes");
        RuntimeProfile::new("onprem", spec)
    }

    // On-prem batchJobs must carry a wall-clock deadline so a hung (non-crashing)
    // trainer can't squat a GPU indefinitely — backoff_limit only catches crashes.
    #[test]
    fn onprem_job_has_hard_deadline() {
        let experiment = test_experiment("recover-1");
        let job = build_job(
            &experiment,
            &onprem_profile(),
            "exp-recover-1",
            "/workspace/runs/camp/recover-1",
            "/workspace/runs/camp/recover-1/metrics.json",
            "athena",
            "recover-1",
        );
        let spec = job.spec.expect("job spec present");
        assert_eq!(spec.active_deadline_seconds, Some(8 * 3600));
        assert_eq!(spec.backoff_limit, Some(3));
    }

    // Shell quoting for the run line: safe tokens pass through, everything
    // else is single-quoted with embedded quotes escaped.
    #[test]
    fn run_line_is_shell_quoted() {
        let parts = vec![
            "python".to_string(),
            "-c".to_string(),
            "print('hi world')".to_string(),
        ];
        assert_eq!(
            shell_join(parts.iter()),
            r#"python -c 'print('\''hi world'\'')'"#
        );
    }

    // A generic (non-ML) workload's metrics must survive ingestion: the operator
    // is domain-agnostic, so an arbitrary scalar like `tour_length` has to land in
    // status.metrics and the objective must come from the workload's own series.
    #[test]
    fn generic_metric_survives_and_objective_from_series() {
        let raw = json!({
            "status": "completed",
            "tour_length": 4.23,
            "improvement_pct": 12.5,
            "reproducibility_hash": "abc",
            "metric_series": {
                "objective": "tour_length",
                "goal": "minimize",
                "points": [{"name": "tour_length", "value": 4.23, "step": 0}],
            },
        });
        let (metrics, detail, _artifacts, _checkpoint) = merge_terminal_metrics(
            BTreeMap::new(),
            None,
            None,
            None,
            Some("/workspace/runs/x"),
            Some(&raw),
        );

        assert_eq!(metrics.get("tour_length"), Some(&json!(4.23)));
        assert_eq!(metrics.get("improvement_pct"), Some(&json!(12.5)));
        // metric_series is an object, not a scalar — it must not be copied flat.
        assert!(!metrics.contains_key("metric_series"));

        let detail = detail.expect("detail present");
        assert_eq!(detail.objective_name.as_deref(), Some("tour_length"));
        assert_eq!(detail.objective_goal.as_deref(), Some("minimize"));
    }

    // Artifact-address keys the trainer emits on success must be routed to the
    // typed status fields, NOT leaked into status.metrics or the latest snapshot.
    #[test]
    fn checkpoint_and_artifacts_routed_out_of_metrics() {
        let raw = json!({
            "val_bpb": 2.34,
            "latest_checkpoint": "/workspace/runs/c/e/checkpoints/step-100",
            "best_checkpoint": "/workspace/runs/c/e/checkpoints/best",
            "onnx_uri": "gs://bucket/model.onnx",
            "metric_series": {"objective": "val_bpb", "goal": "minimize", "points": []},
        });
        let (metrics, detail, artifacts, checkpoint) = merge_terminal_metrics(
            BTreeMap::new(),
            None,
            None,
            None,
            Some("/workspace/runs/c/e"),
            Some(&raw),
        );

        // Scalar objective still flows as a metric.
        assert_eq!(metrics.get("val_bpb"), Some(&json!(2.34)));
        // Artifact addresses must not pollute metrics or the latest/best snapshot.
        for key in ["latest_checkpoint", "best_checkpoint", "onnx_uri"] {
            assert!(!metrics.contains_key(key), "{key} leaked into metrics");
        }
        let latest = detail.unwrap().latest.unwrap();
        for key in ["latest_checkpoint", "best_checkpoint", "onnx_uri"] {
            assert!(latest.get(key).is_none(), "{key} leaked into latest");
        }

        let checkpoint = checkpoint.expect("latestCheckpoint present");
        assert_eq!(checkpoint.uri, "/workspace/runs/c/e/checkpoints/step-100");

        let artifacts = artifacts.expect("artifacts present");
        assert_eq!(
            artifacts.onnx_uri.as_deref(),
            Some("gs://bucket/model.onnx")
        );
        assert_eq!(
            artifacts.best_checkpoint_uri.as_deref(),
            Some("/workspace/runs/c/e/checkpoints/best")
        );
        assert_eq!(
            artifacts.checkpoints_uri.as_deref(),
            Some("/workspace/runs/c/e/checkpoints")
        );
    }

    // Backward compatibility: the original ML workload's val_bpb still flows.
    #[test]
    fn ml_metric_still_ingested() {
        let raw = json!({
            "val_bpb": 2.34,
            "metric_series": {"objective": "val_bpb", "goal": "minimize", "points": []},
        });
        let (metrics, detail, _artifacts, _checkpoint) =
            merge_terminal_metrics(BTreeMap::new(), None, None, None, None, Some(&raw));
        assert_eq!(metrics.get("val_bpb"), Some(&json!(2.34)));
        assert_eq!(detail.unwrap().objective_name.as_deref(), Some("val_bpb"));
    }
}
