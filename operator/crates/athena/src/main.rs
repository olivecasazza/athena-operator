mod benchmark_reconciler;
mod campaign_reconciler;
mod crd;
mod dossier;
mod drive_reconciler;
pub mod metrics;
mod panathenaia;
mod proposer_harness;
mod reconciler;
mod report_reconciler;
mod status_guard;
mod telemetry;
#[cfg(test)]
mod test_support;

use std::sync::Arc;

use athena_api::benchmark_run::BenchmarkRun;
use athena_api::experiment::Experiment;
use athena_api::research_campaign::ResearchCampaign;
use athena_api::research_drive::ResearchDrive;
use athena_api::research_report::ResearchReport;
use futures::StreamExt;
use kube::{
    Api, Client,
    runtime::{Controller, controller, reflector::Store, watcher::Config},
};
use tracing::{error, info};

pub struct Context {
    pub client: Client,
    /// The Experiment controller's own watch cache. Reading it costs no API
    /// traffic, so whatever needs "all Experiments" (the campaign's orphan
    /// adoption, the benchmark's promote check) reads this instead of LISTing.
    pub experiments: Store<Experiment>,
}

/// Concurrent reconciles per controller.
///
/// kube-runtime's default is `0`, which means *unbounded*: every object that
/// is due reconciles at once. Under the status hot loop that was 108-179
/// simultaneous connections to the apiserver, all crossing the WireGuard hub.
/// A bound turns a surge into a queue. These are caps, not targets: a pass is
/// a handful of small requests, so steady state needs a fraction of them. The
/// campaign and drive caps leave headroom for a pass that waits on the LLM
/// proposer (up to `timeout_seconds`) without starving the rest.
const EXPERIMENT_CONCURRENCY: u16 = 16;
const BENCHMARK_CONCURRENCY: u16 = 8;
const CAMPAIGN_CONCURRENCY: u16 = 16;
const REPORT_CONCURRENCY: u16 = 4;
const DRIVE_CONCURRENCY: u16 = 4;

fn bounded(concurrency: u16) -> controller::Config {
    controller::Config::default().concurrency(concurrency)
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
        if arg == "panathenaia" {
            panathenaia::serve().await?;
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
    let experiments: Api<Experiment> = Api::all(client.clone());
    let benchmark_runs: Api<BenchmarkRun> = Api::all(client.clone());
    let campaigns: Api<ResearchCampaign> = Api::all(client.clone());
    let drives: Api<ResearchDrive> = Api::all(client.clone());
    let reports: Api<ResearchReport> = Api::all(client.clone());

    // The Experiment and BenchmarkRun controllers are built first because their
    // reflector caches are shared: one LIST+WATCH per kind, already maintained
    // for the controller itself, replaces a full LIST on every reconcile.
    let experiment_controller = Controller::new(experiments, Config::default())
        .with_config(bounded(EXPERIMENT_CONCURRENCY));
    let experiment_store = experiment_controller.store();
    let benchmark_controller = Controller::new(benchmark_runs, Config::default())
        .with_config(bounded(BENCHMARK_CONCURRENCY));
    let benchmark_store = benchmark_controller.store();

    let ctx = Arc::new(Context {
        client,
        experiments: experiment_store.clone(),
    });

    // Gauges are rebuilt from the caches on a timer, not from a LIST inside each
    // object's reconcile (which re-listed everything once per object per pass).
    tokio::spawn(reconciler::export_experiment_metrics(
        experiment_store.clone(),
    ));
    tokio::spawn(benchmark_reconciler::export_benchmark_metrics(
        benchmark_store,
    ));

    let experiment_controller = experiment_controller
        .shutdown_on_signal()
        .run(reconciler::reconcile, reconciler::error_policy, ctx.clone())
        .for_each(|res| async move {
            match res {
                Ok(o) => info!(?o, "reconciled Experiment"),
                Err(e) => error!(%e, "Experiment reconcile error"),
            }
        });

    let benchmark_controller = benchmark_controller
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
        .with_config(bounded(CAMPAIGN_CONCURRENCY))
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
    // The campaign loop adopts from the Experiment cache. Counting before that
    // cache has had its first LIST would under-count members and could
    // over-generate, so the loop does not start until the cache is primed.
    let campaign_controller = async move {
        let _ = experiment_store.wait_until_ready().await;
        campaign_controller.await
    };

    let report_controller = Controller::new(reports, Config::default())
        .with_config(bounded(REPORT_CONCURRENCY))
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
        .with_config(bounded(DRIVE_CONCURRENCY))
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

#[cfg(test)]
mod tests {
    use super::*;

    /// kube-runtime reads a concurrency of 0 as "unbounded". Every controller
    /// must set a real cap, or one surge puts every due object in flight at once.
    #[test]
    fn every_controller_has_a_real_concurrency_cap() {
        for (controller, cap) in [
            ("experiment", EXPERIMENT_CONCURRENCY),
            ("benchmark", BENCHMARK_CONCURRENCY),
            ("campaign", CAMPAIGN_CONCURRENCY),
            ("report", REPORT_CONCURRENCY),
            ("drive", DRIVE_CONCURRENCY),
        ] {
            assert!(cap > 0, "{controller} controller would be unbounded");
        }
    }

    #[test]
    fn bounded_sets_the_cap_the_default_leaves_open() {
        assert!(format!("{:?}", controller::Config::default()).contains("concurrency: 0"));
        assert!(format!("{:?}", bounded(7)).contains("concurrency: 7"));
    }
}
