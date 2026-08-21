mod benchmark_reconciler;
mod campaign_reconciler;
mod crd;
mod dossier;
mod drive_reconciler;
pub mod metrics;
mod reconciler;
mod report_reconciler;
mod telemetry;

use std::sync::Arc;

use athena_api::benchmark_run::BenchmarkRun;
use athena_api::experiment::Experiment;
use athena_api::research_campaign::ResearchCampaign;
use athena_api::research_drive::ResearchDrive;
use athena_api::research_report::ResearchReport;
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
    let (tracer_provider, _telemetry_config) = telemetry::init_telemetry()?;

    if let Some(arg) = std::env::args().nth(1) {
        if arg == "export-crds" {
            crd::export_crds();
            return Ok(());
        }
        if arg == "scheduling-dump" {
            let client = kube::Client::try_default().await?;
            let snap = athena_api::scheduling::read_scheduling(&client).await;
            println!("{}", serde_json::to_string_pretty(&snap)?);
            return Ok(());
        }
        if arg == "dossier" {
            let args: Vec<String> = std::env::args().collect();
            let mut campaign: Option<String> = None;
            let mut report: Option<String> = None;
            let mut namespace = "default".to_string();
            let mut latex = false;
            let mut i = 2usize;
            while i < args.len() {
                match args[i].as_str() {
                    "--campaign" => {
                        i += 1;
                        if i < args.len() {
                            campaign = Some(args[i].clone());
                        }
                    }
                    "--report" => {
                        i += 1;
                        if i < args.len() {
                            report = Some(args[i].clone());
                        }
                    }
                    "--namespace" | "-n" => {
                        i += 1;
                        if i < args.len() {
                            namespace = args[i].clone();
                        }
                    }
                    "--format" => {
                        i += 1;
                        latex = args.get(i).map(|f| f == "latex").unwrap_or(false);
                    }
                    _ => {}
                }
                i += 1;
            }
            // A ResearchReport curates one campaign; --report and --campaign are
            // mutually exclusive, report taking precedence when both are given.
            match (report, campaign) {
                (Some(report), _) => dossier::run_report(&report, &namespace, latex).await?,
                (None, Some(campaign)) => dossier::run(&campaign, &namespace, latex).await?,
                (None, None) => {
                    eprintln!(
                        "Usage: athena dossier (--campaign <name> | --report <name>) [--namespace <ns>] [--format latex|markdown]"
                    );
                    std::process::exit(2);
                }
            }
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
    let experiments: Api<Experiment> = Api::all(client.clone());
    let benchmark_runs: Api<BenchmarkRun> = Api::all(client.clone());
    let campaigns: Api<ResearchCampaign> = Api::all(client.clone());
    let drives: Api<ResearchDrive> = Api::all(client.clone());
    let reports: Api<ResearchReport> = Api::all(client);

    let experiment_controller = Controller::new(experiments, Config::default())
        .shutdown_on_signal()
        .run(reconciler::reconcile, reconciler::error_policy, ctx.clone())
        .for_each(|res| async move {
            match res {
                Ok(o) => info!(?o, "reconciled Experiment"),
                Err(e) => error!(%e, "Experiment reconcile error"),
            }
        });

    let benchmark_controller = Controller::new(benchmark_runs, Config::default())
        .shutdown_on_signal()
        .run(
            benchmark_reconciler::reconcile,
            benchmark_reconciler::error_policy,
            ctx.clone(),
        )
        .for_each(|res| async move {
            match res {
                Ok(o) => info!(?o, "reconciled BenchmarkRun"),
                Err(e) => error!(%e, "BenchmarkRun reconcile error"),
            }
        });

    let campaign_controller = Controller::new(campaigns, Config::default())
        .shutdown_on_signal()
        .run(
            campaign_reconciler::reconcile,
            campaign_reconciler::error_policy,
            ctx.clone(),
        )
        .for_each(|res| async move {
            match res {
                Ok(o) => info!(?o, "reconciled ResearchCampaign"),
                Err(e) => error!(%e, "ResearchCampaign reconcile error"),
            }
        });

    let report_controller = Controller::new(reports, Config::default())
        .shutdown_on_signal()
        .run(
            report_reconciler::reconcile,
            report_reconciler::error_policy,
            ctx.clone(),
        )
        .for_each(|res| async move {
            match res {
                Ok(o) => info!(?o, "reconciled ResearchReport"),
                Err(e) => error!(%e, "ResearchReport reconcile error"),
            }
        });

    let drive_controller = Controller::new(drives, Config::default())
        .shutdown_on_signal()
        .run(
            drive_reconciler::reconcile,
            drive_reconciler::error_policy,
            ctx.clone(),
        )
        .for_each(|res| async move {
            match res {
                Ok(o) => info!(?o, "reconciled ResearchDrive"),
                Err(e) => error!(%e, "ResearchDrive reconcile error"),
            }
        });

    futures::future::join5(
        experiment_controller,
        benchmark_controller,
        campaign_controller,
        report_controller,
        drive_controller,
    )
    .await;

    if let Some(provider) = tracer_provider {
        if let Err(e) = provider.shutdown() {
            error!(%e, "failed to shutdown OTEL tracer provider");
        }
    }

    Ok(())
}
