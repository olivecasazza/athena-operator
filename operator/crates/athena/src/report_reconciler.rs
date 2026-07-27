//! Reconcile a `ResearchReport` — assemble the curated dossier and publish it.
//!
//! The report **spec** is scientist-authored curation; this controller owns the
//! observed **status**. Each pass:
//!   1. Resolves the referenced campaign and assembles the curated dossier via
//!      the shared [`athena_api::dossier::assemble`] (in-process — no Job, no PVC
//!      mount; the operator can't write the workspace PVC).
//!   2. Publishes the composed Markdown into an owner-referenced ConfigMap
//!      (`<report>-dataset`), rewriting only when the content changed so a
//!      level-triggered requeue never churns the ConfigMap or `lastAssembledTime`.
//!   3. Stamps `status` (phase, datasetUri, includedCount, conditions).
//!
//! The dossier is regenerable from the spec at any time, so the ConfigMap is a
//! convenience snapshot; the durable product state remains the report spec.

use std::sync::Arc;
use std::time::{Duration, Instant};

use athena_api::dossier::{self, Curation};
use athena_api::research_report::ResearchReport;
use k8s_openapi::api::core::v1::ConfigMap;
use kube::ResourceExt;
use kube::api::{Api, Patch, PatchParams};
use kube::runtime::controller::Action;
use serde_json::{Value, json};
use tracing::warn;

use crate::Context;
use crate::telemetry;

const MANAGER: &str = "athena-report";
/// ConfigMap data is capped at ~1 MiB by the API server; stay well under it.
const MAX_DATASET_BYTES: usize = 900_000;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("kube error: {0}")]
    Kube(#[from] kube::Error),
}

pub fn error_policy(report: Arc<ResearchReport>, err: &Error, _ctx: Arc<Context>) -> Action {
    warn!(report = %report.name_any(), %err, "report reconcile error, retrying in 60s");
    Action::requeue(Duration::from_secs(60))
}

fn condition(type_: &str, status: &str, reason: &str, message: &str) -> Value {
    json!({
        "type": type_,
        "status": status,
        "reason": reason,
        "message": message,
        "lastTransitionTime": chrono::Utc::now().to_rfc3339(),
    })
}

#[tracing::instrument(skip(report, ctx), fields(
    report.name = %report.name_any(),
    report.namespace = %report.namespace().unwrap_or_default(),
))]
pub async fn reconcile(report: Arc<ResearchReport>, ctx: Arc<Context>) -> Result<Action, Error> {
    let started = Instant::now();
    let name = report.name_any();
    let ns = report.namespace().unwrap_or_else(|| "default".to_string());
    let campaign_ref = report.spec.campaign_ref.clone();
    let version = env!("CARGO_PKG_VERSION");
    let generation = report.metadata.generation;
    let reports: Api<ResearchReport> = Api::namespaced(ctx.client.clone(), &ns);

    // Previous observed status. We gate every status write on an actual change:
    // the controller watches ResearchReport, so writing status with a fresh
    // condition timestamp on every pass would re-trigger reconcile in a hot loop.
    // Steady state = no write; the periodic requeue re-checks.
    let prev = report.status.as_ref();
    let prev_phase = prev.and_then(|s| s.phase.as_deref());
    let prev_gen = prev.and_then(|s| s.observed_generation);

    // Assemble the curated dossier in-process. A missing campaign/template is a
    // user config error, not a controller failure — record it in status and back
    // off, rather than thrashing the error path.
    let curation = Curation::from_spec(&report.spec);
    let dossier::Dossier { markdown: doc, latex: doc_tex, included } =
        match dossier::assemble(&ctx.client, &campaign_ref, &ns, Some(&curation)).await {
            Ok(d) => d,
            Err(e) => {
                if prev_phase != Some("Error") || prev_gen != generation {
                    let status = json!({ "status": {
                        "phase": "Error",
                        "observedGeneration": generation,
                        "controllerVersion": version,
                        "conditions": [condition(
                            "CampaignResolved", "False", "CampaignUnavailable",
                            &format!("cannot assemble dossier for campaign '{campaign_ref}': {e}"),
                        )],
                    }});
                    reports
                        .patch_status(&name, &PatchParams::apply(MANAGER), &Patch::Merge(&status))
                        .await?;
                }
                telemetry::record_reconcile(&ns, &campaign_ref, "Error", "ok", started.elapsed());
                return Ok(Action::requeue(Duration::from_secs(60)));
            }
        };

    // OKF conformance gate. The dossier is consumed by agents, so it has to be
    // a valid Open Knowledge Format document; publishing a malformed one would
    // push the failure downstream to every consumer instead of stopping here.
    //
    // Fails CLOSED: on violation we publish NOTHING, stamp the specific rule
    // that broke in the condition message, and requeue. Because assembly is
    // level-triggered, that requeue IS the retry — a fixed spec or template is
    // picked up on the next pass without operator intervention.
    let okf = dossier::okf_check(&doc);
    if !okf.ok() {
        let detail = okf.violations.join("; ");
        if prev_phase != Some("Error") || prev_gen != generation {
            let status = json!({ "status": {
                "phase": "Error",
                "includedCount": included,
                "observedGeneration": generation,
                "controllerVersion": version,
                "conditions": [
                    condition("CampaignResolved", "True", "CampaignResolved", "campaign resolved"),
                    condition(
                        "Assembled", "False", "OkfInvalid",
                        &format!("dossier violates Open Knowledge Format: {detail}"),
                    ),
                ],
            }});
            reports
                .patch_status(&name, &PatchParams::apply(MANAGER), &Patch::Merge(&status))
                .await?;
        }
        warn!(%name, %detail, "dossier failed OKF validation; not publishing");
        telemetry::record_reconcile(&ns, &campaign_ref, "Error", "ok", started.elapsed());
        return Ok(Action::requeue(Duration::from_secs(120)));
    }

    // Never write an oversized ConfigMap; ask the scientist to prune instead.
    if doc.len() + doc_tex.len() > MAX_DATASET_BYTES {
        if prev_phase != Some("Error") || prev_gen != generation {
            let status = json!({ "status": {
                "phase": "Error",
                "includedCount": included,
                "observedGeneration": generation,
                "controllerVersion": version,
                "conditions": [
                    condition("CampaignResolved", "True", "CampaignResolved", "campaign resolved"),
                    condition(
                        "Assembled", "False", "DatasetTooLarge",
                        &format!(
                            "assembled dossier is {} bytes (> {MAX_DATASET_BYTES}); prune experiments",
                            doc.len()
                        ),
                    ),
                ],
            }});
            reports
                .patch_status(&name, &PatchParams::apply(MANAGER), &Patch::Merge(&status))
                .await?;
        }
        telemetry::record_reconcile(&ns, &campaign_ref, "Error", "ok", started.elapsed());
        return Ok(Action::requeue(Duration::from_secs(120)));
    }

    // Publish into an owner-referenced ConfigMap, but only when the content
    // changed (idempotent under level-triggered requeues).
    let cm_name = format!("{name}-dataset");
    let cms: Api<ConfigMap> = Api::namespaced(ctx.client.clone(), &ns);
    let current = cms.get_opt(&cm_name).await?;
    let unchanged = current
        .as_ref()
        .and_then(|c| c.data.as_ref())
        .and_then(|d| d.get("dossier.md"))
        .map(|existing| existing.as_str() == doc.as_str())
        .unwrap_or(false);

    if !unchanged {
        // SSA body carries apiVersion/kind/metadata (a typed CR/ConfigMap does not
        // serialize those). The ownerReference GCs the ConfigMap with the report.
        let mut body = json!({
            "apiVersion": "v1",
            "kind": "ConfigMap",
            "metadata": {
                "name": cm_name,
                "namespace": ns,
                "labels": {
                    "app.kubernetes.io/name": "athena-research-report",
                    "athena.nixlab.io/report": name,
                },
            },
            "data": { "dossier.md": doc, "dossier.tex": doc_tex },
        });
        if let Some(uid) = report.metadata.uid.as_ref() {
            body["metadata"]["ownerReferences"] = json!([{
                "apiVersion": "research.nixlab.io/v1alpha1",
                "kind": "ResearchReport",
                "name": name,
                "uid": uid,
                "controller": true,
                "blockOwnerDeletion": true,
            }]);
        }
        cms.patch(&cm_name, &PatchParams::apply(MANAGER).force(), &Patch::Apply(&body))
            .await?;
    }

    // Deterministic citation/formatting reconciliation: pure check over the
    // spec; surfaced as a condition so the console and dossier agree about it.
    // Link integrity is reported, never enforced: OKF's LINK_TOL makes broken
    // links warnings, so a dangling pointer must not block a document that is
    // otherwise valid. Surfacing them as a condition is what stops them rotting
    // silently — the whole point of a trust signal is that it is visible.
    let link_ok = okf.warnings.is_empty();
    let link_detail = if link_ok {
        "all links resolve".to_string()
    } else {
        okf.warnings.join("; ")
    };

    let cc = dossier::citation_check(&report.spec.sections, &report.spec.references);
    let citations_ok = cc.cited_undefined.is_empty() && cc.defined_uncited.is_empty();
    let citation_condition = if citations_ok {
        condition(
            "CitationsReconciled", "True", "CitationsConsistent",
            &format!("{} reference(s), all keys resolve and are cited", report.spec.references.len()),
        )
    } else {
        condition(
            "CitationsReconciled", "False", "CitationMismatch",
            &format!(
                "cited-but-undefined: [{}]; defined-but-uncited: [{}]",
                cc.cited_undefined.join(", "),
                cc.defined_uncited.join(", ")
            ),
        )
    };

    let link_condition = condition(
        "LinksResolved",
        if link_ok { "True" } else { "False" },
        if link_ok { "LinksResolve" } else { "BrokenLinks" },
        &link_detail,
    );

    // Stamp status only when something meaningful changed — otherwise a fresh
    // condition timestamp every pass would re-trigger reconcile forever. In steady
    // state (content unchanged, same phase/generation/count) we skip the write and
    // let the periodic requeue re-check.
    let prev_included = prev.and_then(|s| s.included_count);
    let prev_citations_ok = report
        .status
        .as_ref()
        .and_then(|s| s.conditions.as_ref())
        .and_then(|cs| {
            cs.iter()
                .find(|c| c.condition_type.as_deref() == Some("CitationsReconciled"))
        })
        .map(|c| c.status.as_deref() == Some("True"));
    let needs_write = !unchanged
        || prev_phase != Some("Assembled")
        || prev_gen != generation
        || prev_included != Some(included as u32)
        || prev_citations_ok != Some(citations_ok);

    if needs_write {
        let mut status_obj = json!({
            "phase": "Assembled",
            "datasetUri": format!("configmap://{ns}/{cm_name}#dossier.md"),
            "includedCount": included,
            "observedGeneration": generation,
            "controllerVersion": version,
            "conditions": [
                condition("CampaignResolved", "True", "CampaignResolved", "campaign resolved"),
                condition(
                    "Assembled", "True", "DossierAssembled",
                    &format!("assembled {included} experiment(s) into ConfigMap {cm_name}"),
                ),
                citation_condition,
                link_condition,
            ],
        });
        // lastAssembledTime advances only when the dossier content actually changed.
        if !unchanged {
            status_obj["lastAssembledTime"] = json!(chrono::Utc::now().to_rfc3339());
        }
        reports
            .patch_status(
                &name,
                &PatchParams::apply(MANAGER),
                &Patch::Merge(&json!({ "status": status_obj })),
            )
            .await?;
    }

    telemetry::record_reconcile(&ns, &campaign_ref, "Assembled", "ok", started.elapsed());
    Ok(Action::requeue(Duration::from_secs(120)))
}
