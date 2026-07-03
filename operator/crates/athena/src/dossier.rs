//! `athena dossier` — read-only research dossier assembler (CLI).
//!
//! Thin wrapper: resolves the target and delegates fetch + render to
//! [`athena_api::dossier::assemble`], the one shared assembly path (also used by
//! the console BFF). STATELESS and READ-ONLY.
//!
//! - `--campaign <name>`: full, uncurated dossier for a campaign.
//! - `--report <name>`: apply a `ResearchReport`'s curation (prune, sections,
//!   seeded hypotheses) to that report's campaign.

use anyhow::{Context, Result};
use athena_api::dossier::{self, Curation};
use athena_api::research_report::ResearchReport;
use kube::{Api, Client};

/// Uncurated dossier for a campaign.
pub async fn run(campaign_name: &str, namespace: &str, latex: bool) -> Result<()> {
    let client = Client::try_default().await?;
    let d = dossier::assemble(&client, campaign_name, namespace, None)
        .await
        .with_context(|| format!("assembling dossier for campaign '{campaign_name}'"))?;
    print!("{}", if latex { d.latex } else { d.markdown });
    Ok(())
}

/// Dossier curated by a ResearchReport (resolves its campaign and applies the
/// report's include/exclude, sections and seeded hypotheses).
pub async fn run_report(report_name: &str, namespace: &str, latex: bool) -> Result<()> {
    let client = Client::try_default().await?;
    let reports: Api<ResearchReport> = Api::namespaced(client.clone(), namespace);
    let report = reports.get(report_name).await.with_context(|| {
        format!("ResearchReport '{report_name}' not found in namespace '{namespace}'")
    })?;
    let campaign_name = report.spec.campaign_ref.clone();
    let curation = Curation::from_spec(&report.spec);
    let d = dossier::assemble(&client, &campaign_name, namespace, Some(&curation))
        .await
        .with_context(|| format!("assembling curated dossier from report '{report_name}'"))?;
    print!("{}", if latex { d.latex } else { d.markdown });
    Ok(())
}
