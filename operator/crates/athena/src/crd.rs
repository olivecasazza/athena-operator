use athena_api::benchmark_run::BenchmarkRun;
use athena_api::benchmark_suite::BenchmarkSuite;
use athena_api::experiment::Experiment;
use athena_api::experiment_template::ExperimentTemplate;
use athena_api::metric_source::MetricSource;
use athena_api::research_campaign::ResearchCampaign;
use athena_api::research_drive::ResearchDrive;
use athena_api::research_report::ResearchReport;
use athena_api::runtime_profile::RuntimeProfile;
use kube::CustomResourceExt;

pub fn export_crds() {
    for crd in [
        RuntimeProfile::crd(),
        ExperimentTemplate::crd(),
        ResearchCampaign::crd(),
        ResearchDrive::crd(),
        Experiment::crd(),
        BenchmarkSuite::crd(),
        BenchmarkRun::crd(),
        MetricSource::crd(),
        ResearchReport::crd(),
    ] {
        println!("---");
        println!("{}", serde_json::to_string_pretty(&crd).unwrap());
    }
}
