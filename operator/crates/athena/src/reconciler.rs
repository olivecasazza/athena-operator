use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;

use athena_api::experiment::{
    Experiment, ExperimentEnvironment, ExperimentPhase, ExperimentStatus,
};
use kube::api::{Api, Patch, PatchParams};
use kube::runtime::controller::Action;
use serde_json::json;
use tracing::{info, warn};

use crate::Context;
use crate::telemetry;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("kube error: {0}")]
    Kube(#[from] kube::Error),
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
        let api: Api<Experiment> = Api::namespaced(ctx.client.clone(), ns);
        let job_name = format!("exp-{}", name);
        let status = ExperimentStatus {
            phase: ExperimentPhase::Preparing,
            workspace_path: Some(format!(
                "/workspace/runs/{}/{}",
                experiment.spec.campaign_ref, name
            )),
            job_name: Some(job_name.clone()),
            environment: Some(ExperimentEnvironment {
                namespace: Some(ns.to_string()),
                job_name: Some(job_name),
                ..Default::default()
            }),
            message: Some("workspace preparation/job generation not implemented yet".to_string()),
            ..Default::default()
        };
        api.patch_status(
            name,
            &PatchParams::apply("athena"),
            &Patch::Merge(&json!({ "status": status })),
        )
        .await?;
    }

    telemetry::record_reconcile(ns, campaign, &phase_label, "ok", started_at.elapsed());

    Ok(Action::requeue(Duration::from_secs(30)))
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
