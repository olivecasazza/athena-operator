use athena_api::experiment::Experiment;
use athena_api::experiment_template::ExperimentTemplate;
use athena_api::research_campaign::ResearchCampaign;
use athena_api::runtime_profile::RuntimeProfile;
use kube::CustomResourceExt;

pub fn export_crds() {
    for crd in [
        RuntimeProfile::crd(),
        ExperimentTemplate::crd(),
        ResearchCampaign::crd(),
        Experiment::crd(),
    ] {
        println!("---");
        println!("{}", serde_json::to_string_pretty(&crd).unwrap());
    }
}
