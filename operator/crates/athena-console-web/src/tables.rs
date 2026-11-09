//! Shared column sets for the console's resource tables.
//!
//! Every experiment list and every campaign list shows the same universal
//! columns — fields that exist for any Athena experiment (robot training,
//! LLM sweeps, citation audits) rather than domain parameters. Views differ
//! only in which columns start visible; users can toggle the rest from the
//! table's columns menu, and the choice persists per table.

use std::rc::Rc;

use dioxus::prelude::*;
use panel_kit::widgets::data_table::{Control, SortSpec, TableDefaults};
use panel_kit::widgets::{DataColumnSpec, DataTable, TableDataBuilder, Value};

use crate::models::ResourceSummary;

/// A cell's sort and filter value.
#[derive(Clone, Debug, PartialEq)]
pub enum SortKey {
    Text(String),
    Number(f64),
    Missing,
}

impl SortKey {
    pub fn text(value: impl Into<String>) -> Self {
        Self::Text(value.into())
    }

    pub fn num(value: f64) -> Self {
        Self::Number(value)
    }

    pub fn opt_num(value: Option<f64>) -> Self {
        value.map_or(Self::Missing, Self::Number)
    }

    fn value(&self) -> Value<'_> {
        match self {
            Self::Text(text) => Value::Text(text),
            Self::Number(number) => Value::Number(*number),
            Self::Missing => Value::Missing,
        }
    }
}

#[derive(Clone)]
struct RowCell {
    key: SortKey,
    view: Element,
}

/// One console table row: an id and one cell per declared column. A cell's
/// view renders inside the table's own `td`.
#[derive(Clone)]
pub struct DataRow {
    id: String,
    cells: Vec<RowCell>,
    selected: bool,
}

impl PartialEq for DataRow {
    // Cell views capture event handlers; never skip a render on a false "equal".
    fn eq(&self, _: &Self) -> bool {
        false
    }
}

impl DataRow {
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            cells: Vec::new(),
            selected: false,
        }
    }

    /// Mark as the current selection.
    pub fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    /// Arbitrary cell view with an explicit sort and filter value.
    pub fn cell(mut self, view: Element, key: SortKey) -> Self {
        self.cells.push(RowCell { key, view });
        self
    }

    /// Plain text cell: shown, sorted, and filtered by the same string.
    pub fn text(self, value: impl Into<String>) -> Self {
        let value = value.into();
        self.cell(
            rsx! { span { title: "{value}", "{value}" } },
            SortKey::Text(value),
        )
    }

    /// Text cell with an extra CSS class (e.g. `"muted"`).
    pub fn text_class(self, value: impl Into<String>, class: &'static str) -> Self {
        let value = value.into();
        let view = if class == "phase" {
            rsx! { crate::MetadataChip { field: "Phase", value: value.clone(), labeled: false } }
        } else {
            rsx! { span { class: "{class}", title: "{value}", "{value}" } }
        };
        self.cell(view, SortKey::Text(value))
    }

    /// Numeric cell shown as `display`, sorted by `value`.
    pub fn num(self, value: Option<f64>, display: impl Into<String>) -> Self {
        let display = display.into();
        self.cell(
            rsx! { span { title: "{display}", "{display}" } },
            SortKey::opt_num(value),
        )
    }
}

/// The console's table: panel-kit's [`DataTable`] over rows of [`DataRow`].
#[component]
pub fn ConsoleTable(
    id: String,
    columns: Vec<DataColumnSpec>,
    rows: Vec<DataRow>,
    sort: SortSpec,
    #[props(default)] storage_key: Option<String>,
    #[props(default)] empty: Option<String>,
    #[props(default)] placeholder: Option<String>,
    #[props(default)] toolbar: Option<Vec<Control>>,
) -> Element {
    let mut builder = TableDataBuilder::with_capacity(&columns, rows.len());
    for row in &rows {
        builder.row(&row.id, row.cells.iter().map(|cell| cell.key.value()));
    }
    let data = builder.build();
    let chosen: Vec<String> = rows
        .iter()
        .filter(|row| row.selected)
        .map(|row| row.id.clone())
        .collect();
    let selected = (!chosen.is_empty()).then_some(chosen);
    let rows = Rc::new(rows);
    let cell = Callback::new(move |(row, column): (usize, usize)| {
        rows[row]
            .cells
            .get(column)
            .map(|cell| cell.view.clone())
            .unwrap_or_else(|| rsx! {})
    });
    let defaults = TableDefaults {
        sort: vec![sort],
        ..TableDefaults::default()
    };
    let empty = empty.map(|text| rsx! { "{text}" });
    rsx! {
        DataTable {
            id: id.clone(),
            columns,
            data,
            cell,
            label: id.clone(),
            ribbon: false,
            defaults,
            toolbar,
            storage_key,
            empty,
            selected,
            placeholder: placeholder.unwrap_or_else(|| "filter rows".to_string()),
        }
    }
}

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

impl DataRow {
    /// Created timestamp: sorted by epoch millis, shown as UTC.
    pub fn date(self, ms: &Option<String>) -> Self {
        let shown = fmt_ms(ms);
        self.cell(rsx! { span { class: "date", "{shown}" } }, ms_key(ms))
    }

    /// Text cell, or a muted dash that sorts last when absent.
    pub fn opt_text(self, value: Option<String>) -> Self {
        match value {
            Some(v) => self.text(v),
            None => self.cell(rsx! { span { class: "muted", "{DASH}" } }, SortKey::Missing),
        }
    }
}

/// Universal experiment columns. `with_campaign` adds the Campaign column for
/// lists that span campaigns.
pub fn experiment_columns(with_campaign: bool) -> Vec<DataColumnSpec> {
    let mut cols = vec![
        DataColumnSpec::new("name", "Experiment")
            .pinned()
            .class("pk-dt-col-title"),
    ];
    if with_campaign {
        cols.push(DataColumnSpec::new("campaign", "Campaign").hidden());
    }
    cols.extend([
        DataColumnSpec::new("template", "Template").hidden(),
        DataColumnSpec::new("phase", "Phase")
            .enumerated()
            .width("16ch"),
        DataColumnSpec::new("decision", "Decision")
            .enumerated()
            .width("14ch"),
        DataColumnSpec::new("objective", "Objective")
            .number()
            .width("16ch"),
        DataColumnSpec::new("metric", "Metric").hidden(),
        DataColumnSpec::new("parent", "Parent").hidden(),
        DataColumnSpec::new("gen", "Gen").number().hidden(),
        DataColumnSpec::new("runtime", "Runtime").number().hidden(),
        DataColumnSpec::new("gpu_hours", "GPU-h").number().hidden(),
        DataColumnSpec::new("created", "Created").number().hidden(),
        DataColumnSpec::new("hypothesis", "Hypothesis").hidden(),
        DataColumnSpec::new("workspace", "Workspace").hidden(),
    ]);
    cols
}

/// One experiment row; `name_cell` is the caller's (usually clickable) first
/// cell.
pub fn experiment_row(e: &ResourceSummary, name_cell: Element, with_campaign: bool) -> DataRow {
    experiment_cells(DataRow::new(e.manifest_path()), e, name_cell, with_campaign)
}

/// Append the universal experiment cells to `row` (for tables that prepend
/// their own leading columns, e.g. an include checkbox).
pub fn experiment_cells(
    row: DataRow,
    e: &ResourceSummary,
    name_cell: Element,
    with_campaign: bool,
) -> DataRow {
    let mut row = row.cell(name_cell, SortKey::text(&e.name));
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
        DataColumnSpec::new("name", "Campaign")
            .pinned()
            .class("pk-dt-col-title"),
        DataColumnSpec::new("drive", "Drive").hidden(),
        DataColumnSpec::new("template", "Template").hidden(),
        DataColumnSpec::new("strategy", "Strategy").hidden(),
        DataColumnSpec::new("phase", "Phase")
            .enumerated()
            .width("16ch"),
        DataColumnSpec::new("best", "Best experiment").hidden(),
        DataColumnSpec::new("objective", "Best objective")
            .number()
            .width("16ch"),
        DataColumnSpec::new("metric", "Metric").hidden(),
        DataColumnSpec::new("succeeded", "Succeeded")
            .number()
            .width("11ch"),
        DataColumnSpec::new("failed", "Failed")
            .number()
            .width("9ch"),
        DataColumnSpec::new("running", "Running").number().hidden(),
        DataColumnSpec::new("created", "Created").number().hidden(),
    ]
}

fn count(n: Option<u32>) -> (Option<f64>, String) {
    (
        n.map(f64::from),
        n.map(|v| v.to_string()).unwrap_or_else(|| DASH.into()),
    )
}

/// One campaign row; `name_cell` renders the (clickable) display name,
/// sorted and searched by both `label` and the full CR name.
pub fn campaign_row(c: &ResourceSummary, name_cell: Element, label: &str) -> DataRow {
    let (ok, ok_s) = count(c.succeeded);
    let (bad, bad_s) = count(c.failed);
    let (run, run_s) = count(c.running);
    DataRow::new(c.manifest_path())
        .cell(name_cell, SortKey::text(format!("{label} {}", c.name)))
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
            let row = experiment_row(&e, rsx! { span {} }, with_campaign);
            assert_eq!(row.cells.len(), experiment_columns(with_campaign).len());
        }
        let row = campaign_row(&e, rsx! { span {} }, "x");
        assert_eq!(row.cells.len(), campaign_columns().len());
    }
}
