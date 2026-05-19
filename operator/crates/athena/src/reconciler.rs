use std::sync::Arc;
use std::time::Duration;

use athena_api::experiment::{
    Experiment, ExperimentEnvironment, ExperimentPhase, ExperimentStatus,
};
use kube::api::{Api, Patch, PatchParams};
use kube::runtime::controller::Action;
use serde_json::json;
use tracing::{info, warn};

use crate::Context;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("kube error: {0}")]
    Kube(#[from] kube::Error),
}

pub async fn reconcile(experiment: Arc<Experiment>, ctx: Arc<Context>) -> Result<Action, Error> {
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

    info!(name, namespace = ns, ?phase, "reconciling Experiment");

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

    Ok(Action::requeue(Duration::from_secs(30)))
}

pub fn error_policy(experiment: Arc<Experiment>, err: &Error, _ctx: Arc<Context>) -> Action {
    let name = experiment.metadata.name.as_deref().unwrap_or("unknown");
    warn!(name, %err, "error reconciling experiment, retrying");
    Action::requeue(Duration::from_secs(30))
}
