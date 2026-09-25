//! Shared column sets for the console's resource tables.
//!
//! Every experiment list and every campaign list shows the same universal
//! columns — fields that exist for any Athena experiment (robot training,
//! LLM sweeps, citation audits) rather than domain parameters. Views differ
//! only in which columns start visible; users can toggle the rest from the
//! table's columns menu, and the choice persists per table.

use dioxus::prelude::*;
use panel_kit::widgets::{DataColumnSpec, DataRow, SortKey};

use crate::models::ResourceSummary;

/// Epoch-millis string → `YYYY-MM-DD HH:MM` UTC, or an em dash.
pub fn fmt_ms(ms: &Option<String>) -> String {
    let Some(ms) = ms.as_deref().and_then(|s| s.parse::<i64>().ok()) else {
        return DASH.to_string();
    };
    let secs = ms.div_euclid(1000);
    let (y, m, d) = civil_from_days(secs.div_euclid(86_400));
    let rem = secs.rem_euclid(86_400);
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}",
        rem / 3600,
        (rem % 3600) / 60
    )
}

/// Days since 1970-01-01 → (year, month, day). Howard Hinnant's algorithm.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe + era * 400;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

const DASH: &str = "\u{2014}";

fn ms_key(ms: &Option<String>) -> SortKey {
    SortKey::opt_num(ms.as_deref().and_then(|s| s.parse::<f64>().ok()))
}

/// `3725` → `1h 02m`; under a minute → `45s`.
pub fn fmt_duration(secs: Option<i64>) -> String {
    match secs {
        None => DASH.to_string(),
        Some(s) if s < 60 => format!("{s}s"),
        Some(s) if s < 3600 => format!("{}m {:02}s", s / 60, s % 60),
        Some(s) => format!("{}h {:02}m", s / 3600, (s % 3600) / 60),
    }
}

/// Objective value with goal arrow, e.g. `0.8300 ↑`.
fn fmt_objective(value: Option<f64>, goal: Option<&str>) -> String {
    let Some(v) = value else {
        return DASH.to_string();
    };
    let arrow = match goal {
        Some("maximize") => " \u{2191}",
        Some("minimize") => " \u{2193}",
        _ => "",
    };
    if v.abs() >= 1000.0 {
        format!("{v:.1}{arrow}")
    } else {
        format!("{v:.4}{arrow}")
    }
}

/// Row helpers shared by every console table.
pub trait RowExt {
    fn date(self, ms: &Option<String>) -> Self;
    fn opt_text(self, value: Option<String>) -> Self;
}

impl RowExt for DataRow {
    fn date(self, ms: &Option<String>) -> Self {
        let shown = fmt_ms(ms);
        self.cell(rsx! { td { class: "date", "{shown}" } }, ms_key(ms), &shown)
    }

    fn opt_text(self, value: Option<String>) -> Self {
        match value {
            Some(v) => self.text(v),
            None => self.cell(
                rsx! { td { class: "muted", "{DASH}" } },
                SortKey::Missing,
                "",
            ),
        }
    }
}

/// Universal experiment columns. `with_campaign` adds the Campaign column for
/// lists that span campaigns.
pub fn experiment_columns(with_campaign: bool) -> Vec<DataColumnSpec> {
    let mut cols = vec![DataColumnSpec::new("name", "Experiment").pinned()];
    if with_campaign {
        cols.push(DataColumnSpec::new("campaign", "Campaign"));
    }
    cols.extend([
        DataColumnSpec::new("template", "Template"),
        DataColumnSpec::new("phase", "Phase"),
        DataColumnSpec::new("decision", "Decision"),
        DataColumnSpec::new("objective", "Objective"),
        DataColumnSpec::new("metric", "Metric").hidden(),
        DataColumnSpec::new("parent", "Parent"),
        DataColumnSpec::new("gen", "Gen"),
        DataColumnSpec::new("runtime", "Runtime"),
        DataColumnSpec::new("gpu_hours", "GPU-h").hidden(),
        DataColumnSpec::new("created", "Created"),
        DataColumnSpec::new("hypothesis", "Hypothesis").hidden(),
        DataColumnSpec::new("workspace", "Workspace").hidden(),
    ]);
    cols
}

/// One experiment row; `name_cell` is the caller's (usually clickable) first
/// cell and must render a `td`.
pub fn experiment_row(e: &ResourceSummary, name_cell: Element, with_campaign: bool) -> DataRow {
    let mut row = DataRow::new(e.name.clone()).cell(name_cell, SortKey::text(&e.name), &e.name);
    if with_campaign {
        row = row.opt_text(e.campaign.clone());
    }
    row.opt_text(e.template.clone())
        .text_class(e.phase.clone(), "phase")
        .opt_text(e.decision.clone())
        .num(
            e.objective_value,
            fmt_objective(e.objective_value, e.objective_goal.as_deref()),
        )
        .opt_text(e.objective.clone())
        .opt_text(e.parent.clone())
        .num(
            e.generation.map(f64::from),
            e.generation
                .map(|g| g.to_string())
                .unwrap_or_else(|| DASH.into()),
        )
        .num(
            e.runtime_seconds.map(|s| s as f64),
            fmt_duration(e.runtime_seconds),
        )
        .num(
            e.gpu_hours,
            e.gpu_hours
                .map(|h| format!("{h:.2}"))
                .unwrap_or_else(|| DASH.into()),
        )
        .date(&e.created_at)
        .text_class(e.hypothesis.clone().unwrap_or_default(), "muted")
        .text_class(e.workspace_path.clone().unwrap_or_default(), "muted")
}

/// Universal campaign columns.
pub fn campaign_columns() -> Vec<DataColumnSpec> {
    vec![
        DataColumnSpec::new("name", "Campaign").pinned(),
        DataColumnSpec::new("drive", "Drive"),
        DataColumnSpec::new("template", "Template"),
        DataColumnSpec::new("strategy", "Strategy"),
        DataColumnSpec::new("phase", "Phase"),
        DataColumnSpec::new("best", "Best experiment"),
        DataColumnSpec::new("objective", "Best objective"),
        DataColumnSpec::new("metric", "Metric").hidden(),
        DataColumnSpec::new("succeeded", "OK"),
        DataColumnSpec::new("failed", "Failed"),
        DataColumnSpec::new("running", "Running").hidden(),
        DataColumnSpec::new("created", "Created"),
    ]
}

fn count(n: Option<u32>) -> (Option<f64>, String) {
    (
        n.map(f64::from),
        n.map(|v| v.to_string()).unwrap_or_else(|| DASH.into()),
    )
}

/// One campaign row; `name_cell` renders the (clickable) display name `td`,
/// sorted and searched by both `label` and the full CR name.
pub fn campaign_row(c: &ResourceSummary, name_cell: Element, label: &str) -> DataRow {
    let (ok, ok_s) = count(c.succeeded);
    let (bad, bad_s) = count(c.failed);
    let (run, run_s) = count(c.running);
    DataRow::new(c.name.clone())
        .cell(
            name_cell,
            SortKey::text(label),
            &format!("{label} {}", c.name),
        )
        .opt_text(c.drive.clone())
        .opt_text(c.template.clone())
        .opt_text(c.strategy.clone())
        .text_class(c.phase.clone(), "phase")
        .opt_text(c.best_experiment.clone())
        .num(
            c.objective_value,
            fmt_objective(c.objective_value, c.objective_goal.as_deref()),
        )
        .opt_text(c.objective.clone())
        .num(ok, ok_s)
        .num(bad, bad_s)
        .num(run, run_s)
        .date(&c.created_at)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_epoch_millis_as_utc() {
        assert_eq!(fmt_ms(&Some("1790262480000".into())), "2026-09-24 15:08");
        assert_eq!(fmt_ms(&Some("0".into())), "1970-01-01 00:00");
        assert_eq!(fmt_ms(&None), DASH);
    }

    #[test]
    fn formats_durations_and_objectives() {
        assert_eq!(fmt_duration(Some(45)), "45s");
        assert_eq!(fmt_duration(Some(3725)), "1h 02m");
        assert_eq!(
            fmt_objective(Some(0.83), Some("maximize")),
            "0.8300 \u{2191}"
        );
        assert_eq!(
            fmt_objective(Some(4452.5), Some("minimize")),
            "4452.5 \u{2193}"
        );
    }

    #[test]
    fn row_cells_match_declared_columns() {
        let e = ResourceSummary::default();
        for with_campaign in [true, false] {
            let row = experiment_row(&e, rsx! { td {} }, with_campaign);
            assert_eq!(row.cells.len(), experiment_columns(with_campaign).len());
            assert_eq!(row.keys.len(), row.cells.len());
        }
        let row = campaign_row(&e, rsx! { td {} }, "x");
        assert_eq!(row.cells.len(), campaign_columns().len());
    }
}
