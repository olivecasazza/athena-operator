use std::collections::{BTreeMap, BTreeSet};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::Arc;
use std::time::Duration;

use athena_api::benchmark_run::{
    BenchmarkRun, BenchmarkRunCost, BenchmarkRunPhase, BenchmarkRunStatus, GateResult,
    TaskResultSummary,
};
use athena_api::benchmark_suite::{
    BenchmarkGate, BenchmarkSuite, BenchmarkTask, GateOperator, NamedMetricSourceRef,
};
use athena_api::common::{
    ArtifactUris, Condition, ConditionStatus, LocalObjectReference, MetricAggregate, MetricGoal,
};
use athena_api::experiment::{
    Experiment, ExperimentDecision, ExperimentMetricPoint, ExperimentMetricSeries,
    ExperimentMetrics,
};
use athena_api::metric_source::{MetricMapping, MetricSource, MetricValueType};
use athena_api::runtime_profile::{ExecutionMode, RuntimeProfile};
use k8s_openapi::api::batch::v1::{Job, JobSpec};
use k8s_openapi::api::core::v1::{
    Container, EnvVar, PersistentVolumeClaim, PersistentVolumeClaimSpec,
    PersistentVolumeClaimVolumeSource, Pod, PodSpec, PodTemplateSpec, ResourceRequirements, Volume,
    VolumeMount, VolumeResourceRequirements,
};
use k8s_openapi::apimachinery::pkg::api::resource::Quantity;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;
use kube::ResourceExt;
use kube::api::{Api, ListParams, Patch, PatchParams, PostParams};
use kube::runtime::controller::Action;
use serde_json::{Value, json};
use tracing::warn;

use crate::Context;
use crate::metrics;

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
}

pub async fn reconcile(run: Arc<BenchmarkRun>, ctx: Arc<Context>) -> Result<Action, Error> {
    let name = run.name_any();
    let ns = run.namespace().unwrap_or_else(|| "default".to_string());
    if run.spec.suspend {
        patch_status(
            ctx,
            &ns,
            &name,
            BenchmarkRunStatus {
                phase: BenchmarkRunPhase::Skipped,
                observed_generation: run.metadata.generation,
                conditions: vec![condition(
                    "Ready",
                    ConditionStatus::False,
                    "Suspended",
                    "BenchmarkRun is suspended",
                )],
                controller_version: Some(env!("CARGO_PKG_VERSION").to_string()),
                ..Default::default()
            },
        )
        .await?;
        return Ok(Action::requeue(Duration::from_secs(60)));
    }

    let suite = get_suite(ctx.clone(), &ns, &run.spec.suite_ref).await?;
    let profile_ref =
        run.spec
            .runtime_profile_ref
            .clone()
            .unwrap_or_else(|| LocalObjectReference {
                name: run.spec.target_ref.name.clone(),
                namespace: run.spec.target_ref.namespace.clone(),
            });
    let profile = get_profile(ctx.clone(), &ns, &profile_ref).await?;
    if profile.spec.runtime.mode != ExecutionMode::BatchJob {
        return Err(Error::UnsupportedRuntimeMode(profile_ref.name));
    }
    let metric_sources = get_metric_sources(ctx.clone(), &ns, &suite.spec.metric_sources).await?;
    let tasks = selected_tasks(&suite, &run);
    ensure_workspace_claim(ctx.clone(), &ns, &profile).await?;
    ensure_jobs(&run, ctx.clone(), &ns, &suite, &profile, &tasks).await?;

    let status = observed_status(ctx.clone(), &ns, &run, &suite, &metric_sources, &tasks).await?;
    if status.phase == BenchmarkRunPhase::Succeeded
        && run.spec.promotion_policy.update_experiment_status
    {
        promote_experiment(ctx.clone(), &ns, &run, &suite, &status).await?;
    }
    patch_status(ctx.clone(), &ns, &name, status).await?;
    update_benchmark_metrics(ctx, &ns).await?;

    Ok(Action::requeue(Duration::from_secs(30)))
}

async fn update_benchmark_metrics(ctx: Arc<Context>, ns: &str) -> Result<(), Error> {
    let runs: Api<BenchmarkRun> = Api::namespaced(ctx.client.clone(), ns);
    let mut counts: BTreeMap<(String, String), f64> = BTreeMap::new();

    for run in runs.list(&ListParams::default()).await? {
        let suite = run.spec.suite_ref.name.clone();
        let phase = run
            .status
            .as_ref()
            .map(|status| format!("{:?}", status.phase))
            .unwrap_or_else(|| "Pending".to_string());
        *counts.entry((suite, phase)).or_insert(0.0) += 1.0;
    }

    metrics::BENCHMARK_RUNS_TOTAL.reset();
    for ((suite, phase), count) in counts {
        metrics::BENCHMARK_RUNS_TOTAL
            .with_label_values(&[ns, suite.as_str(), phase.as_str()])
            .set(count);
    }

    Ok(())
}

async fn get_suite(
    ctx: Arc<Context>,
    ns: &str,
    reference: &LocalObjectReference,
) -> Result<BenchmarkSuite, Error> {
    let namespace = reference.namespace.as_deref().unwrap_or(ns);
    Api::<BenchmarkSuite>::namespaced(ctx.client.clone(), namespace)
        .get_opt(&reference.name)
        .await?
        .ok_or_else(|| Error::MissingRef {
            kind: "BenchmarkSuite",
            namespace: namespace.to_string(),
            name: reference.name.clone(),
        })
}

async fn get_profile(
    ctx: Arc<Context>,
    ns: &str,
    reference: &LocalObjectReference,
) -> Result<RuntimeProfile, Error> {
    let namespace = reference.namespace.as_deref().unwrap_or(ns);
    Api::<RuntimeProfile>::namespaced(ctx.client.clone(), namespace)
        .get_opt(&reference.name)
        .await?
        .ok_or_else(|| Error::MissingRef {
            kind: "RuntimeProfile",
            namespace: namespace.to_string(),
            name: reference.name.clone(),
        })
}

async fn get_metric_sources(
    ctx: Arc<Context>,
    ns: &str,
    refs: &[NamedMetricSourceRef],
) -> Result<Vec<MetricSource>, Error> {
    let mut sources = Vec::new();
    for named in refs {
        let reference = &named.metric_source_ref;
        let namespace = reference.namespace.as_deref().unwrap_or(ns);
        let source = Api::<MetricSource>::namespaced(ctx.client.clone(), namespace)
            .get_opt(&reference.name)
            .await?
            .ok_or_else(|| Error::MissingRef {
                kind: "MetricSource",
                namespace: namespace.to_string(),
                name: reference.name.clone(),
            })?;
        sources.push(source);
    }
    Ok(sources)
}

fn selected_tasks(suite: &BenchmarkSuite, run: &BenchmarkRun) -> Vec<BenchmarkTask> {
    let Some(selector) = &run.spec.task_selector else {
        return suite.spec.tasks.clone();
    };
    suite
        .spec
        .tasks
        .iter()
        .filter(|task| {
            (selector.names.is_empty() || selector.names.contains(&task.name))
                && selector
                    .labels
                    .iter()
                    .all(|(key, value)| task.labels.get(key) == Some(value))
        })
        .cloned()
        .collect()
}

async fn ensure_workspace_claim(
    ctx: Arc<Context>,
    ns: &str,
    profile: &RuntimeProfile,
) -> Result<(), Error> {
    let storage = &profile.spec.storage;
    if !storage.create_workspace_claim {
        return Ok(());
    }
    let claims: Api<PersistentVolumeClaim> = Api::namespaced(ctx.client.clone(), ns);
    if claims
        .get_opt(&storage.workspace_claim_name)
        .await?
        .is_some()
    {
        return Ok(());
    }

    let requests = BTreeMap::from([(
        "storage".to_string(),
        Quantity(storage.workspace_size.clone()),
    )]);
    claims
        .create(
            &PostParams::default(),
            &PersistentVolumeClaim {
                metadata: ObjectMeta {
                    name: Some(storage.workspace_claim_name.clone()),
                    namespace: Some(ns.to_string()),
                    labels: Some(BTreeMap::from([(
                        "app.kubernetes.io/name".to_string(),
                        "athena-workspace".to_string(),
                    )])),
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
    Ok(())
}

async fn ensure_jobs(
    run: &BenchmarkRun,
    ctx: Arc<Context>,
    ns: &str,
    suite: &BenchmarkSuite,
    profile: &RuntimeProfile,
    tasks: &[BenchmarkTask],
) -> Result<(), Error> {
    let jobs: Api<Job> = Api::namespaced(ctx.client.clone(), ns);
    for task in tasks {
        for seed in seeds_for_task(run, task) {
            let job_name = benchmark_job_name(&run.name_any(), &task.name, seed);
            if jobs.get_opt(&job_name).await?.is_none() {
                jobs.create(
                    &PostParams::default(),
                    &build_job(run, suite, profile, task, seed, ns, &job_name),
                )
                .await?;
            }
        }
    }
    Ok(())
}

fn seeds_for_task(run: &BenchmarkRun, task: &BenchmarkTask) -> Vec<i64> {
    if let Some(seed_matrix) = &run.spec.seed_matrix {
        if !seed_matrix.seeds.is_empty() {
            return seed_matrix.seeds.clone();
        }
    }
    if let Some(seed_spec) = &task.seeds {
        if !seed_spec.values.is_empty() {
            return seed_spec.values.clone();
        }
    }
    vec![0]
}

fn benchmark_job_name(run_name: &str, task_name: &str, seed: i64) -> String {
    let base = sanitize_label_value(&format!("br-{run_name}-{task_name}-{seed}"));
    if base.len() <= 63 {
        return base;
    }
    let mut hasher = DefaultHasher::new();
    base.hash(&mut hasher);
    format!("{}-{:x}", &base[..50], hasher.finish())
}

fn build_job(
    run: &BenchmarkRun,
    suite: &BenchmarkSuite,
    profile: &RuntimeProfile,
    task: &BenchmarkTask,
    seed: i64,
    namespace: &str,
    job_name: &str,
) -> Job {
    let run_name = run.name_any();
    let workspace_path = run
        .spec
        .output
        .as_ref()
        .and_then(|output| output.workspace_path.clone())
        .unwrap_or_else(|| format!("/workspace/benchmarks/runs/{namespace}/{run_name}"));
    let metrics_path = format!("{workspace_path}/{}/{seed}/metrics.json", task.name);
    let mut labels = BTreeMap::from([
        (
            "app.kubernetes.io/name".to_string(),
            "athena-benchmark-run".to_string(),
        ),
        (
            "athena.nixlab.io/benchmark-run".to_string(),
            run_name.clone(),
        ),
        (
            "athena.nixlab.io/benchmark-task".to_string(),
            task.name.clone(),
        ),
        ("athena.nixlab.io/seed".to_string(), seed.to_string()),
    ]);
    // Kueue admission: opt in by labeling the Job with its LocalQueue. Kueue
    // only manages Jobs carrying this label (manageJobsWithoutQueueName=false),
    // so leaving queue_name unset preserves direct scheduling.
    if let Some(queue) = &profile.spec.scheduling.queue_name {
        labels.insert("kueue.x-k8s.io/queue-name".to_string(), queue.clone());
    }
    let suite_json = serde_json::to_string(&suite.spec).unwrap_or_else(|_| "{}".to_string());
    let run_json = serde_json::to_string(&run.spec).unwrap_or_else(|_| "{}".to_string());

    Job {
        metadata: ObjectMeta {
            name: Some(job_name.to_string()),
            namespace: Some(namespace.to_string()),
            labels: Some(labels.clone()),
            // GC the Job (and its pods) with the BenchmarkRun; without this a
            // deleted run orphans its Jobs and a recreated one adopts the
            // stale Job spec instead of rebuilding it.
            owner_references: kube::Resource::controller_owner_ref(run, &()).map(|r| vec![r]),
            ..Default::default()
        },
        spec: Some(JobSpec {
            backoff_limit: Some(run.spec.budget.max_retries.unwrap_or(0) as i32),
            ttl_seconds_after_finished: run.spec.cleanup_policy.ttl_seconds_after_finished,
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
                        name: "benchmark".to_string(),
                        image: task
                            .evaluator
                            .as_ref()
                            .and_then(|evaluator| evaluator.image.clone())
                            .or_else(|| Some(profile.spec.image.clone())),
                        image_pull_policy: profile.spec.pull_policy.clone(),
                        command: task
                            .evaluator
                            .as_ref()
                            .and_then(|evaluator| {
                                (!evaluator.command.is_empty()).then(|| evaluator.command.clone())
                            })
                            .or_else(|| {
                                (!profile.spec.command.is_empty())
                                    .then(|| profile.spec.command.clone())
                            }),
                        args: if profile.spec.args.is_empty() {
                            None
                        } else {
                            Some(profile.spec.args.clone())
                        },
                        env: Some(vec![
                            env("ATHENA_BENCHMARK_RUN", &run_name),
                            env("ATHENA_BENCHMARK_TASK", &task.name),
                            env("ATHENA_BENCHMARK_SUITE", &suite.name_any()),
                            env("ATHENA_SEED", &seed.to_string()),
                            env("ATHENA_NAMESPACE", namespace),
                            env("ATHENA_TARGET_KIND", &run.spec.target_ref.kind),
                            env("ATHENA_TARGET_NAME", &run.spec.target_ref.name),
                            env("ATHENA_WORKSPACE_PATH", &workspace_path),
                            env("ATHENA_METRICS_PATH", &metrics_path),
                            env("ATHENA_BENCHMARK_SUITE_SPEC", &suite_json),
                            env("ATHENA_BENCHMARK_RUN_SPEC", &run_json),
                        ]),
                        resources: Some(resource_requirements(&profile.spec.resources)),
                        volume_mounts: Some(vec![VolumeMount {
                            name: "workspace".to_string(),
                            mount_path: profile.spec.storage.workspace_mount_path.clone(),
                            ..Default::default()
                        }]),
                        ..Default::default()
                    }],
                    volumes: Some(vec![Volume {
                        name: "workspace".to_string(),
                        persistent_volume_claim: Some(PersistentVolumeClaimVolumeSource {
                            claim_name: profile.spec.storage.workspace_claim_name.clone(),
                            ..Default::default()
                        }),
                        ..Default::default()
                    }]),
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
                    ..Default::default()
                }),
            },
            ..Default::default()
        }),
        ..Default::default()
    }
}

async fn observed_status(
    ctx: Arc<Context>,
    ns: &str,
    run: &BenchmarkRun,
    suite: &BenchmarkSuite,
    metric_sources: &[MetricSource],
    tasks: &[BenchmarkTask],
) -> Result<BenchmarkRunStatus, Error> {
    let jobs: Api<Job> = Api::namespaced(ctx.client.clone(), ns);
    let pods: Api<Pod> = Api::namespaced(ctx.client.clone(), ns);
    let run_name = run.name_any();
    let mut task_results = Vec::new();
    let mut job_names = Vec::new();
    let mut pending = false;
    let mut running = false;
    let mut failed = false;
    let mut missing_required = Vec::new();
    let mut first_reproducibility_hash = None;

    for task in tasks {
        for seed in seeds_for_task(run, task) {
            let job_name = benchmark_job_name(&run_name, &task.name, seed);
            job_names.push(job_name.clone());
            let Some(job) = jobs.get_opt(&job_name).await? else {
                pending = true;
                continue;
            };
            let task_pods = pods
                .list(
                    &ListParams::default()
                        .labels(&format!("athena.nixlab.io/benchmark-run={run_name},athena.nixlab.io/benchmark-task={},athena.nixlab.io/seed={seed}", task.name)),
                )
                .await?
                .items;
            let (phase, metrics, metric_series, repro_hash, missing) =
                task_status(&job, &task_pods, task, metric_sources);
            if matches!(
                phase,
                BenchmarkRunPhase::Pending | BenchmarkRunPhase::Preparing
            ) {
                pending = true;
            }
            if phase == BenchmarkRunPhase::Running {
                running = true;
            }
            if matches!(phase, BenchmarkRunPhase::Failed | BenchmarkRunPhase::Error) {
                failed = true;
            }
            if first_reproducibility_hash.is_none() {
                first_reproducibility_hash = repro_hash;
            }
            missing_required.extend(missing);
            task_results.push(TaskResultSummary {
                name: task.name.clone(),
                seed: Some(seed),
                phase,
                metrics,
                metric_series,
                artifacts: ArtifactUris {
                    metrics_uri: Some(format!(
                        "{}/{}/{seed}/metrics.json",
                        run.spec
                            .output
                            .as_ref()
                            .and_then(|output| output.workspace_path.as_deref())
                            .unwrap_or("/workspace/benchmarks/runs"),
                        task.name
                    )),
                    ..Default::default()
                },
                node_name: task_pods
                    .first()
                    .and_then(|pod| pod.spec.as_ref()?.node_name.clone()),
                ..Default::default()
            });
        }
    }

    let aggregate_metrics = aggregate_metrics(&task_results);
    let metric_series = task_results
        .iter()
        .flat_map(|result| result.metric_series.clone())
        .collect::<Vec<_>>();
    let gates = evaluate_gates(&suite.spec.reporting.gates, &aggregate_metrics);
    let gates_failed = gates.iter().any(|gate| !gate.passed);
    let phase = if failed || !missing_required.is_empty() || gates_failed {
        BenchmarkRunPhase::Failed
    } else if running {
        BenchmarkRunPhase::Running
    } else if pending || task_results.is_empty() {
        BenchmarkRunPhase::Preparing
    } else {
        BenchmarkRunPhase::Succeeded
    };
    let message = if !missing_required.is_empty() {
        format!(
            "required metrics missing: {}",
            missing_required
                .into_iter()
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect::<Vec<_>>()
                .join(", ")
        )
    } else if gates_failed {
        "one or more benchmark gates failed".to_string()
    } else {
        format!("BenchmarkRun observed phase {phase:?}")
    };

    Ok(BenchmarkRunStatus {
        phase: phase.clone(),
        observed_generation: run.metadata.generation,
        observed_suite_hash: suite_hash(suite),
        reproducibility_hash: first_reproducibility_hash,
        job_names,
        task_results,
        aggregate_metrics,
        metric_series,
        cost: BenchmarkRunCost::default(),
        gates,
        logs_link: Some(format!(
            r#"https://grafana.casazza.io/explore?left={{"datasource":"Loki","queries":[{{"expr":"{{namespace=\"{}\",athena_nixlab_io_benchmark_run=\"{}\"}}"}}]}}"#,
            ns, run_name
        )),
        metrics_link: Some(
            "https://grafana.casazza.io/d/athena-athena-experiment-debugging/athena-experiment-debugging".to_string(),
        ),
        conditions: vec![condition(
            "Ready",
            if matches!(phase, BenchmarkRunPhase::Succeeded) {
                ConditionStatus::True
            } else {
                ConditionStatus::False
            },
            format!("{phase:?}"),
            message,
        )],
        controller_version: Some(env!("CARGO_PKG_VERSION").to_string()),
        ..Default::default()
    })
}

fn task_status(
    job: &Job,
    pods: &[Pod],
    task: &BenchmarkTask,
    metric_sources: &[MetricSource],
) -> (
    BenchmarkRunPhase,
    BTreeMap<String, f64>,
    Vec<ExperimentMetricSeries>,
    Option<String>,
    Vec<String>,
) {
    if job_condition_is_true(job, "Failed") {
        return (
            BenchmarkRunPhase::Failed,
            BTreeMap::new(),
            Vec::new(),
            None,
            Vec::new(),
        );
    }
    if !job_condition_is_true(job, "Complete") {
        if pods
            .iter()
            .any(|pod| pod.status.as_ref().and_then(|s| s.phase.as_deref()) == Some("Running"))
        {
            return (
                BenchmarkRunPhase::Running,
                BTreeMap::new(),
                Vec::new(),
                None,
                Vec::new(),
            );
        }
        return (
            BenchmarkRunPhase::Preparing,
            BTreeMap::new(),
            Vec::new(),
            None,
            Vec::new(),
        );
    }

    let Some(raw_metrics) = pods.iter().find_map(termination_metrics_json) else {
        return (
            BenchmarkRunPhase::Failed,
            BTreeMap::new(),
            Vec::new(),
            None,
            task.metrics.required.clone(),
        );
    };
    let (metrics, repro_hash, missing) = normalize_metrics(&raw_metrics, task, metric_sources);
    let series = metric_series(&raw_metrics);
    let phase = if missing.is_empty() {
        BenchmarkRunPhase::Succeeded
    } else {
        BenchmarkRunPhase::Failed
    };
    (phase, metrics, series, repro_hash, missing)
}

fn metric_series(raw: &Value) -> Vec<ExperimentMetricSeries> {
    let Some(series) = raw.get("metric_series") else {
        return Vec::new();
    };
    let tag = series
        .get("tag")
        .and_then(Value::as_str)
        .unwrap_or("benchmark")
        .to_string();
    let iteration = series
        .get("iteration")
        .and_then(Value::as_i64)
        .unwrap_or_default();
    let objective = series
        .get("objective")
        .and_then(Value::as_str)
        .unwrap_or("score")
        .to_string();
    let goal = series
        .get("goal")
        .and_then(Value::as_str)
        .unwrap_or("maximize")
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

fn normalize_metrics(
    raw: &Value,
    task: &BenchmarkTask,
    metric_sources: &[MetricSource],
) -> (BTreeMap<String, f64>, Option<String>, Vec<String>) {
    let mappings: Vec<MetricMapping> = metric_sources
        .iter()
        .flat_map(|source| source.spec.metrics.clone())
        .collect();
    let mut metrics = BTreeMap::new();
    let mut missing = Vec::new();
    let mut repro_hash = None;

    for mapping in mappings {
        let value = extract_json_path(raw, &mapping.path);
        match (value, &mapping.metric_type) {
            (Some(value), MetricValueType::Number | MetricValueType::Integer) => {
                if let Some(mut number) = value.as_f64() {
                    if let Some(normalize) = &mapping.normalize {
                        if let Some(multiply) = normalize.multiply {
                            number *= multiply;
                        }
                        if let Some(offset) = normalize.offset {
                            number += offset;
                        }
                    }
                    metrics.insert(mapping.name.clone(), number);
                } else if mapping.required {
                    missing.push(mapping.name.clone());
                }
            }
            (Some(value), MetricValueType::String) => {
                if mapping.name == "reproducibility_hash" {
                    repro_hash = value.as_str().map(ToString::to_string);
                }
            }
            (Some(_), MetricValueType::Boolean) => {}
            (None, _) if mapping.required => missing.push(mapping.name.clone()),
            (None, _) => {}
        }
    }

    for required in &task.metrics.required {
        if required == "reproducibility_hash" {
            if repro_hash.is_none() {
                missing.push(required.clone());
            }
        } else if !metrics.contains_key(required) {
            missing.push(required.clone());
        }
    }
    (metrics, repro_hash, missing)
}

fn extract_json_path<'a>(value: &'a Value, path: &str) -> Option<&'a Value> {
    let mut current = value;
    for part in path.strip_prefix("$.")?.split('.') {
        current = current.get(part)?;
    }
    Some(current)
}

fn aggregate_metrics(results: &[TaskResultSummary]) -> BTreeMap<String, MetricAggregate> {
    let mut values: BTreeMap<String, Vec<f64>> = BTreeMap::new();
    for result in results
        .iter()
        .filter(|result| result.phase == BenchmarkRunPhase::Succeeded)
    {
        for (name, value) in &result.metrics {
            values.entry(name.clone()).or_default().push(*value);
        }
    }

    values
        .into_iter()
        .map(|(name, values)| {
            let count = values.len() as f64;
            let mean = values.iter().sum::<f64>() / count;
            let variance = values
                .iter()
                .map(|value| {
                    let delta = value - mean;
                    delta * delta
                })
                .sum::<f64>()
                / count;
            let aggregate = MetricAggregate {
                mean: Some(mean),
                std: Some(variance.sqrt()),
                min: values.iter().copied().reduce(f64::min),
                max: values.iter().copied().reduce(f64::max),
                count: Some(values.len() as u32),
                ..Default::default()
            };
            (name, aggregate)
        })
        .collect()
}

fn evaluate_gates(
    gates: &[BenchmarkGate],
    metrics: &BTreeMap<String, MetricAggregate>,
) -> Vec<GateResult> {
    gates
        .iter()
        .map(|gate| {
            let actual = metrics.get(&gate.metric).and_then(|metric| metric.mean);
            let passed = actual
                .map(|actual| match gate.operator {
                    GateOperator::GreaterThanOrEqual => actual >= gate.value,
                    GateOperator::GreaterThan => actual > gate.value,
                    GateOperator::LessThanOrEqual => actual <= gate.value,
                    GateOperator::LessThan => actual < gate.value,
                })
                .unwrap_or(false);
            GateResult {
                metric: gate.metric.clone(),
                passed,
                threshold: gate.value,
                actual,
            }
        })
        .collect()
}

async fn promote_experiment(
    ctx: Arc<Context>,
    ns: &str,
    run: &BenchmarkRun,
    suite: &BenchmarkSuite,
    status: &BenchmarkRunStatus,
) -> Result<(), Error> {
    if run.spec.target_ref.kind != "Experiment" {
        return Ok(());
    }
    let namespace = run.spec.target_ref.namespace.as_deref().unwrap_or(ns);
    let experiments: Api<Experiment> = Api::namespaced(ctx.client.clone(), namespace);
    let primary = suite
        .spec
        .tasks
        .first()
        .map(|task| task.metrics.primary.clone())
        .unwrap_or_else(|| "score".to_string());
    let goal = suite
        .spec
        .tasks
        .first()
        .map(|task| match task.metrics.goal {
            MetricGoal::Maximize => "maximize",
            MetricGoal::Minimize => "minimize",
        })
        .unwrap_or("maximize");
    let latest = serde_json::to_value(&status.aggregate_metrics).unwrap_or_else(|_| json!({}));
    let metrics = status
        .aggregate_metrics
        .iter()
        .filter_map(|(name, aggregate)| aggregate.mean.map(|mean| (name.clone(), json!(mean))))
        .collect::<BTreeMap<_, _>>();
    let decision = if status.gates.iter().all(|gate| gate.passed) {
        ExperimentDecision::Keep
    } else {
        ExperimentDecision::Discard
    };

    experiments
        .patch_status(
            &run.spec.target_ref.name,
            &PatchParams::apply("athena"),
            &Patch::Merge(&json!({
                "status": {
                    "metrics": metrics,
                    "decision": decision,
                    "metricsDetail": ExperimentMetrics {
                        objective_name: Some(primary),
                        objective_goal: Some(goal.to_string()),
                        latest: Some(latest.clone()),
                        best: Some(latest),
                        metrics_path: run.spec.output.as_ref().and_then(|output| output.workspace_path.clone()),
                        series: status.metric_series.clone(),
                        last_updated: Some(chrono::Utc::now().to_rfc3339()),
                    },
                }
            })),
        )
        .await?;
    Ok(())
}

fn suite_hash(suite: &BenchmarkSuite) -> Option<String> {
    if let Some(hash) = &suite.spec.suite_hash {
        return Some(hash.clone());
    }
    let spec = serde_json::to_string(&suite.spec).ok()?;
    let mut hasher = DefaultHasher::new();
    spec.hash(&mut hasher);
    Some(format!("{:x}", hasher.finish()))
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

async fn patch_status(
    ctx: Arc<Context>,
    ns: &str,
    name: &str,
    status: BenchmarkRunStatus,
) -> Result<(), Error> {
    Api::<BenchmarkRun>::namespaced(ctx.client.clone(), ns)
        .patch_status(
            name,
            &PatchParams::apply("athena"),
            &Patch::Merge(&json!({ "status": status })),
        )
        .await?;
    Ok(())
}

fn condition(
    condition_type: impl Into<String>,
    status: ConditionStatus,
    reason: impl Into<String>,
    message: impl Into<String>,
) -> Condition {
    Condition {
        condition_type: condition_type.into(),
        status,
        reason: Some(reason.into()),
        message: Some(message.into()),
        last_transition_time: Some(chrono::Utc::now().to_rfc3339()),
    }
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

fn env(name: &str, value: &str) -> EnvVar {
    EnvVar {
        name: name.to_string(),
        value: Some(value.to_string()),
        ..Default::default()
    }
}

fn sanitize_label_value(value: &str) -> String {
    value
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect()
}

pub fn error_policy(run: Arc<BenchmarkRun>, err: &Error, _ctx: Arc<Context>) -> Action {
    warn!(
        name = run.metadata.name.as_deref().unwrap_or("unknown"),
        %err,
        "error reconciling BenchmarkRun, retrying"
    );
    Action::requeue(Duration::from_secs(30))
}

#[cfg(test)]
mod tests {
    use super::*;
    use athena_api::runtime_profile::RuntimeProfileSpec;

    fn test_run() -> BenchmarkRun {
        let mut run: BenchmarkRun = serde_json::from_value(json!({
            "apiVersion": "research.nixlab.io/v1alpha1",
            "kind": "BenchmarkRun",
            "metadata": { "name": "canary", "namespace": "apps", "uid": "run-uid" },
            "spec": {
                "suiteRef": { "name": "suite" },
                "targetRef": {
                    "apiVersion": "research.nixlab.io/v1alpha1",
                    "kind": "Experiment",
                    "name": "exp"
                }
            }
        }))
        .expect("run deserializes");
        run.metadata.uid = Some("run-uid".to_string());
        run
    }

    fn test_suite_and_task() -> (BenchmarkSuite, BenchmarkTask) {
        let suite: BenchmarkSuite = serde_json::from_value(json!({
            "apiVersion": "research.nixlab.io/v1alpha1",
            "kind": "BenchmarkSuite",
            "metadata": { "name": "suite", "namespace": "apps" },
            "spec": {
                "taxonomy": "runtimeHealth",
                "suiteVersion": "v1",
                "tasks": [{
                    "name": "toy",
                    "integration": "customCommand",
                    "metrics": { "primary": "reward_mean", "goal": "maximize" },
                    "budget": {}
                }]
            }
        }))
        .expect("suite deserializes");
        let task = suite.spec.tasks[0].clone();
        (suite, task)
    }

    fn profile(queue_name: Option<&str>) -> RuntimeProfile {
        let mut scheduling = json!({});
        if let Some(queue) = queue_name {
            scheduling = json!({ "queueName": queue });
        }
        let spec: RuntimeProfileSpec = serde_json::from_value(
            athena_api::defaults::apply_runtime_profile_defaults(json!({
                "runtime": { "type": "pytorch", "mode": "batchJob" },
                "image": "ghcr.io/example/bench:v1",
                "command": ["python", "bench.py"],
                "scheduling": scheduling,
            })),
        )
        .expect("profile spec deserializes");
        RuntimeProfile::new("bench", spec)
    }

    // A queued profile flows the benchmark Job through Kueue: queue label +
    // created suspended (Kueue unsuspends on admission), and the Job is owned
    // by its BenchmarkRun so deletion GCs it.
    #[test]
    fn queued_profile_labels_and_suspends_job() {
        let run = test_run();
        let (suite, task) = test_suite_and_task();
        let job = build_job(
            &run,
            &suite,
            &profile(Some("athena-gpu")),
            &task,
            0,
            "apps",
            "br-canary-toy-0",
        );

        let labels = job.metadata.labels.as_ref().expect("labels");
        assert_eq!(
            labels.get("kueue.x-k8s.io/queue-name"),
            Some(&"athena-gpu".to_string())
        );
        assert_eq!(job.spec.as_ref().unwrap().suspend, Some(true));
        let owners = job.metadata.owner_references.as_ref().expect("owner refs");
        assert_eq!(owners[0].kind, "BenchmarkRun");
        assert_eq!(owners[0].name, "canary");
    }

    // Without a queueName the Job must schedule directly: no Kueue label, not
    // suspended (manageJobsWithoutQueueName=false ignores unlabeled Jobs).
    #[test]
    fn unqueued_profile_schedules_directly() {
        let run = test_run();
        let (suite, task) = test_suite_and_task();
        let job = build_job(
            &run,
            &suite,
            &profile(None),
            &task,
            0,
            "apps",
            "br-canary-toy-0",
        );

        let labels = job.metadata.labels.as_ref().expect("labels");
        assert!(!labels.contains_key("kueue.x-k8s.io/queue-name"));
        assert_eq!(job.spec.as_ref().unwrap().suspend, None);
    }
}
