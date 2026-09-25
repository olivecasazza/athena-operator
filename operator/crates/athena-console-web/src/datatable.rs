//! Searchable, sortable table for the console's long resource lists.
//!
//! Every list of experiments, campaigns, reports, templates, or workloads
//! renders through [`DataTable`] so they share one interaction model: a filter
//! box that matches any cell text, click-to-sort headers, a row count, and a
//! created-date column rendered by [`fmt_ms`]. Rows own their cells as
//! pre-rendered elements (links, badges, checkboxes) plus a parallel list of
//! typed sort keys, so sorting never depends on rendered markup.

use dioxus::prelude::*;

/// One column header.
#[derive(Clone, PartialEq)]
pub struct Col {
    pub title: &'static str,
    /// Whether clicking the header sorts by this column.
    pub sortable: bool,
}

impl Col {
    pub const fn new(title: &'static str) -> Self {
        Self {
            title,
            sortable: true,
        }
    }

    /// A column that holds actions (buttons, checkboxes) rather than data.
    pub const fn action(title: &'static str) -> Self {
        Self {
            title,
            sortable: false,
        }
    }
}

/// Typed sort key for one cell. Missing values sort last in both directions.
#[derive(Clone, PartialEq, Debug)]
pub enum Key {
    Text(String),
    Num(f64),
    None,
}

impl Key {
    pub fn text(s: impl Into<String>) -> Self {
        Key::Text(s.into().to_lowercase())
    }

    /// Epoch-millis string (the DTO wire format) → numeric key.
    pub fn ms(ms: &Option<String>) -> Self {
        ms.as_deref()
            .and_then(|s| s.parse::<i64>().ok())
            .map(|v| Key::Num(v as f64))
            .unwrap_or(Key::None)
    }

    fn order(&self, other: &Key) -> std::cmp::Ordering {
        use std::cmp::Ordering::*;
        match (self, other) {
            (Key::Num(a), Key::Num(b)) => a.partial_cmp(b).unwrap_or(Equal),
            (Key::Text(a), Key::Text(b)) => a.cmp(b),
            (Key::None, Key::None) => Equal,
            (Key::None, _) => Greater,
            (_, Key::None) => Less,
            (Key::Num(_), Key::Text(_)) => Less,
            (Key::Text(_), Key::Num(_)) => Greater,
        }
    }
}

/// One table row.
#[derive(Clone)]
pub struct Row {
    /// Stable identity (resource name) for DOM keying.
    pub key: String,
    /// Rendered cells, one per column.
    pub cells: Vec<Element>,
    /// Sort keys, one per column (`Key::None` for action columns).
    pub keys: Vec<Key>,
    /// Lowercased haystack the filter box matches against.
    pub search: String,
    /// Highlight as the current selection.
    pub selected: bool,
}

impl PartialEq for Row {
    // Cells carry closures; never skip a re-render on a false "equal".
    fn eq(&self, _: &Self) -> bool {
        false
    }
}

impl Row {
    pub fn new(key: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            cells: Vec::new(),
            keys: Vec::new(),
            search: String::new(),
            selected: false,
        }
    }

    /// Plain text cell: rendered, sorted, and searched by the same string.
    pub fn text(mut self, s: impl Into<String>) -> Self {
        let s = s.into();
        self.search.push_str(&s.to_lowercase());
        self.search.push(' ');
        self.keys.push(Key::text(s.clone()));
        self.cells.push(rsx! { td { title: "{s}", "{s}" } });
        self
    }

    /// Muted secondary text cell (details, hypotheses).
    pub fn muted(mut self, s: impl Into<String>) -> Self {
        let s = s.into();
        self.search.push_str(&s.to_lowercase());
        self.search.push(' ');
        self.keys.push(Key::text(s.clone()));
        self.cells
            .push(rsx! { td { class: "muted", title: "{s}", "{s}" } });
        self
    }

    /// Phase cell with the shared `.phase` styling.
    pub fn phase(mut self, s: impl Into<String>) -> Self {
        let s = s.into();
        self.search.push_str(&s.to_lowercase());
        self.search.push(' ');
        self.keys.push(Key::text(s.clone()));
        self.cells.push(rsx! { td { class: "phase", "{s}" } });
        self
    }

    /// Created/started date from an epoch-millis string.
    pub fn date(mut self, ms: &Option<String>) -> Self {
        let shown = fmt_ms(ms);
        self.search.push_str(&shown);
        self.search.push(' ');
        self.keys.push(Key::ms(ms));
        self.cells.push(rsx! { td { class: "date", "{shown}" } });
        self
    }

    /// Custom cell with an explicit sort key and search text.
    pub fn cell(mut self, el: Element, key: Key, search: &str) -> Self {
        self.search.push_str(&search.to_lowercase());
        self.search.push(' ');
        self.keys.push(key);
        self.cells.push(el);
        self
    }

    pub fn selected(mut self, on: bool) -> Self {
        self.selected = on;
        self
    }
}

/// Epoch-millis string → `YYYY-MM-DD HH:MM` UTC, or an em dash.
pub fn fmt_ms(ms: &Option<String>) -> String {
    let Some(ms) = ms.as_deref().and_then(|s| s.parse::<i64>().ok()) else {
        return "\u{2014}".to_string();
    };
    let secs = ms.div_euclid(1000);
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
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
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Filter + sort `rows`; pure so it is testable without a DOM.
pub fn visible(rows: &[Row], query: &str, sort: Option<(usize, bool)>) -> Vec<usize> {
    let terms: Vec<String> = query.split_whitespace().map(|t| t.to_lowercase()).collect();
    let mut idx: Vec<usize> = rows
        .iter()
        .enumerate()
        .filter(|(_, r)| terms.iter().all(|t| r.search.contains(t.as_str())))
        .map(|(i, _)| i)
        .collect();
    if let Some((col, desc)) = sort {
        idx.sort_by(|&a, &b| {
            let ka = rows[a].keys.get(col).unwrap_or(&Key::None);
            let kb = rows[b].keys.get(col).unwrap_or(&Key::None);
            // Missing values stay last regardless of direction.
            match (ka, kb) {
                (Key::None, Key::None) => std::cmp::Ordering::Equal,
                (Key::None, _) => std::cmp::Ordering::Greater,
                (_, Key::None) => std::cmp::Ordering::Less,
                _ if desc => kb.order(ka),
                _ => ka.order(kb),
            }
        });
    }
    idx
}

/// Searchable, sortable table. `sort` is the initial (column, descending).
#[component]
pub fn DataTable(
    cols: Vec<Col>,
    rows: Vec<Row>,
    #[props(default)] sort: Option<(usize, bool)>,
    #[props(default = "Nothing found.".to_string())] empty: String,
    #[props(default = "filter…".to_string())] placeholder: String,
) -> Element {
    let mut query = use_signal(String::new);
    let mut order = use_signal(move || sort);

    let q = query();
    let shown = visible(&rows, &q, order());
    let total = rows.len();
    let ncols = cols.len().max(1);
    let current = order();

    rsx! {
        div { class: "dt",
            div { class: "dt-bar",
                input {
                    class: "rc-input dt-filter",
                    r#type: "search",
                    placeholder: "{placeholder}",
                    value: "{q}",
                    oninput: move |e| query.set(e.value()),
                }
                span { class: "muted dt-count",
                    if shown.len() == total { "{total}" } else { "{shown.len()} / {total}" }
                }
            }
            div { class: "scroll-tbl",
                table { class: "tbl",
                    thead {
                        tr {
                            for (i, c) in cols.iter().enumerate() {
                                {
                                    let arrow = match current {
                                        Some((col, true)) if col == i => " \u{25be}",
                                        Some((col, false)) if col == i => " \u{25b4}",
                                        _ => "",
                                    };
                                    let sortable = c.sortable;
                                    rsx! {
                                        th {
                                            class: if sortable { "sortable" } else { "" },
                                            onclick: move |_| {
                                                if !sortable {
                                                    return;
                                                }
                                                // Click: ascending; again: descending.
                                                let next = match order() {
                                                    Some((col, false)) if col == i => Some((i, true)),
                                                    _ => Some((i, false)),
                                                };
                                                order.set(next);
                                            },
                                            "{c.title}{arrow}"
                                        }
                                    }
                                }
                            }
                        }
                    }
                    tbody {
                        if shown.is_empty() {
                            tr { td { colspan: "{ncols}", class: "muted",
                                if total == 0 { "{empty}" } else { "No rows match the filter." }
                            } }
                        }
                        for i in shown {
                            tr {
                                key: "{rows[i].key}",
                                class: if rows[i].selected { "selected" } else { "" },
                                for cell in rows[i].cells.iter() {
                                    {cell.clone()}
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_epoch_millis_as_utc() {
        // 2026-09-24T15:08:00Z
        assert_eq!(fmt_ms(&Some("1790262480000".into())), "2026-09-24 15:08");
        assert_eq!(fmt_ms(&Some("0".into())), "1970-01-01 00:00");
        assert_eq!(fmt_ms(&None), "\u{2014}");
    }

    fn row(name: &str, ms: Option<&str>) -> Row {
        Row::new(name).text(name).date(&ms.map(String::from))
    }

    #[test]
    fn filter_matches_all_terms_case_insensitively() {
        let rows = vec![
            row("humanoid-recover", None),
            row("spider-recover", None),
            row("spot-locomotion", None),
        ];
        assert_eq!(visible(&rows, "RECOVER spi", None), vec![1]);
        assert_eq!(visible(&rows, "", None), vec![0, 1, 2]);
    }

    #[test]
    fn date_sort_orders_numerically_and_keeps_missing_last() {
        let rows = vec![
            row("a", Some("900")),
            row("b", None),
            row("c", Some("10000")),
        ];
        assert_eq!(visible(&rows, "", Some((1, true))), vec![2, 0, 1]);
        assert_eq!(visible(&rows, "", Some((1, false))), vec![0, 2, 1]);
    }
}
