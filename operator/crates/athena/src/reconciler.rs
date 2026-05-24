use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;

use athena_api::experiment::{
    Experiment, ExperimentEnvironment, ExperimentPhase, ExperimentStatus,
};
use athena_api::experiment_template::ExperimentTemplate;
use athena_api::research_campaign::ResearchCampaign;
use athena_api::runtime_profile::{ExecutionMode, RuntimeProfile};
use k8s_openapi::api::batch::v1::{Job, JobSpec};
use k8s_openapi::api::core::v1::{
    Container, EnvVar, PersistentVolumeClaimVolumeSource, PodSpec, PodTemplateSpec, ResourceRequirements,
    Volume, VolumeMount,
};
use k8s_openapi::apimachinery::pkg::api::resource::Quantity;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;
use kube::api::{Api, ListParams, Patch, PatchParams, PostParams};
use kube::ResourceExt;
use kube::runtime::controller::Action;
use serde_json::json;
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

    info!(name, namespace = ns, campaign, ?phase, trace_id, span_id, "reconciling Experiment");

    if phase == ExperimentPhase::Pending {
        ensure_experiment_job(&experiment, ctx.clone(), ns, name).await?;
    }

    update_experiment_metrics(ctx.clone(), ns).await?;
    telemetry::record_reconcile(ns, campaign, &phase_label, "ok", started_at.elapsed());

    Ok(Action::requeue(Duration::from_secs(30)))
}

async fn update_experiment_metrics(ctx: Arc<Context>, ns: &str) -> Result<(), Error> {
    let experiments: Api<Experiment> = Api::namespaced(ctx.client.clone(), ns);
    let mut counts: BTreeMap<(String, String), f64> = BTreeMap::new();

    for experiment in experiments.list(&ListParams::default()).await? {
        let campaign = experiment.spec.campaign_ref.clone();
        let phase = experiment
            .status
            .as_ref()
            .map(|status| format!("{:?}", status.phase))
            .unwrap_or_else(|| "Pending".to_string());
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
    let profile = profiles
        .get_opt(&template.spec.runtime_profile_ref)
        .await?
        .ok_or_else(|| Error::MissingRef {
            kind: "RuntimeProfile",
            namespace: ns.to_string(),
            name: template.spec.runtime_profile_ref.clone(),
        })?;

    if profile.spec.runtime.mode != ExecutionMode::BatchJob {
        return Err(Error::UnsupportedRuntimeMode(template.spec.runtime_profile_ref.clone()));
    }

    let job_name = format!("exp-{}", name);
    let workspace_path = format!("/workspace/runs/{}/{}", experiment.spec.campaign_ref, name);
    let metrics_path = template.spec.metrics.parser.path.clone();
    let jobs: Api<Job> = Api::namespaced(ctx.client.clone(), ns);
    if jobs.get_opt(&job_name).await?.is_none() {
        jobs.create(
            &PostParams::default(),
            &build_job(
                experiment,
                &profile,
                &job_name,
                &workspace_path,
                &metrics_path,
                ns,
                name,
            ),
        )
        .await?;
    }

    let experiments: Api<Experiment> = Api::namespaced(ctx.client.clone(), ns);
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
        environment: Some(ExperimentEnvironment {
            namespace: Some(ns.to_string()),
            job_name: Some(job_name),
            ..Default::default()
        }),
        message: Some("created Kubernetes Job for batch runtime".to_string()),
        ..Default::default()
    };
    experiments
        .patch_status(
            name,
            &PatchParams::apply("athena"),
            &Patch::Merge(&json!({ "status": status })),
        )
        .await?;

    Ok(())
}

fn build_job(
    experiment: &Experiment,
    profile: &RuntimeProfile,
    job_name: &str,
    _workspace_path: &str,
    metrics_path: &str,
    namespace: &str,
    experiment_name: &str,
) -> Job {
    let labels = BTreeMap::from([
        ("app.kubernetes.io/name".to_string(), "athena-experiment".to_string()),
        ("athena.nixlab.io/campaign".to_string(), experiment.spec.campaign_ref.clone()),
        ("athena.nixlab.io/experiment".to_string(), experiment_name.to_string()),
        ("athena.nixlab.io/runtime-profile".to_string(), profile.name_any()),
    ]);
    let spec_json = serde_json::to_string(&experiment.spec).unwrap_or_else(|_| "{}".to_string());

    Job {
        metadata: ObjectMeta {
            name: Some(job_name.to_string()),
            namespace: Some(namespace.to_string()),
            labels: Some(labels.clone()),
            ..Default::default()
        },
        spec: Some(JobSpec {
            backoff_limit: Some(0),
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
                        env: Some(vec![
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
                                name: "ATHENA_METRICS_PATH".to_string(),
                                value: Some(metrics_path.to_string()),
                                ..Default::default()
                            },
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

fn resource_requirements(resources: &athena_api::runtime_profile::ResourceProfile) -> ResourceRequirements {
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
    let ns = experiment.metadata.namespace.as_deref().unwrap_or("default");
    let phase = experiment
        .status
        .as_ref()
        .map(|s| format!("{:?}", s.phase))
        .unwrap_or_else(|| "Pending".to_string());
    telemetry::record_reconcile(ns, &experiment.spec.campaign_ref, &phase, "error", Duration::ZERO);
    Action::requeue(Duration::from_secs(30))
}
