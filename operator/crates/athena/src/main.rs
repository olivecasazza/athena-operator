mod crd;
pub mod metrics;
mod reconciler;
mod telemetry;

use std::sync::Arc;

use athena_api::experiment::Experiment;
use futures::StreamExt;
use kube::{
    Api, Client,
    runtime::{Controller, watcher::Config},
};
use tracing::{error, info};

pub struct Context {
    pub client: Client,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let tracer_provider = telemetry::init_telemetry()?;

    if let Some(arg) = std::env::args().nth(1) {
        if arg == "export-crds" {
            crd::export_crds();
            return Ok(());
        }
    }

    info!("starting Athena research operator");
    metrics::init();
    telemetry::init_metrics();
    let metrics_port = std::env::var("METRICS_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(8080u16);
    tokio::spawn(metrics::serve(metrics_port));

    let client = Client::try_default().await?;
    let ctx = Arc::new(Context {
        client: client.clone(),
    });
    let experiments: Api<Experiment> = Api::all(client);

    Controller::new(experiments, Config::default())
        .shutdown_on_signal()
        .run(reconciler::reconcile, reconciler::error_policy, ctx)
        .for_each(|res| async move {
            match res {
                Ok(o) => info!(?o, "reconciled Experiment"),
                Err(e) => error!(%e, "Experiment reconcile error"),
            }
        })
        .await;

    if let Some(provider) = tracer_provider {
        if let Err(e) = provider.shutdown() {
            error!(%e, "failed to shutdown OTEL tracer provider");
        }
    }

    Ok(())
}
