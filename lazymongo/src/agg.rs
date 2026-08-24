//! Aggregation screen state (FR-17/18/19): a full-screen pipeline editor
//! with a stage list and stage-by-stage result preview.

use std::collections::HashSet;

use lazymongo_core::bson::{Bson, Document};
use lazymongo_core::query::{parse_pipeline, stage_name};
use ratatui::layout::Rect;

use crate::json_view::{doc_lines, RLine};
use crate::textarea::TextArea;

/// Docs fetched per preview run.
pub const AGG_PREVIEW_LIMIT: usize = 50;

/// How to draw chart-mode results.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChartKind {
    /// Line for date/numeric _ids, bars for categorical ones.
    Auto,
    Bars,
    Line,
    Scatter,
}

impl ChartKind {
    pub fn next(self) -> Self {
        match self {
            ChartKind::Auto => ChartKind::Bars,
            ChartKind::Bars => ChartKind::Line,
            ChartKind::Line => ChartKind::Scatter,
            ChartKind::Scatter => ChartKind::Auto,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            ChartKind::Auto => "auto",
            ChartKind::Bars => "bars",
            ChartKind::Line => "line",
            ChartKind::Scatter => "scatter",
        }
    }
}

/// What the X values of a series represent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum XKind {
    /// Milliseconds since the epoch.
    Date,
    Number,
}

pub const DEFAULT_PIPELINE: &str = "[\n  { $match: {} }\n]";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AggFocus {
    Stages,
    Editor,
    Results,
}

pub struct AggState {
    pub db: String,
    pub coll: String,
    pub editor: TextArea,
    pub focus: AggFocus,
    /// Stage names from the last successful parse (for the stage list).
    pub stages: Vec<String>,
    pub selected_stage: usize,
    pub error: Option<String>,
    pub running: bool,
    /// Stage index (0-based) the current preview ran through.
    pub ran_through: Option<usize>,
    pub docs: Vec<Document>,
    pub folds: Vec<HashSet<String>>,
    pub lines: Vec<RLine>,
    pub cursor: usize,
    pub scroll: usize,
    /// Render results as a bar chart when they are {_id, number}-shaped.
    pub chart: bool,
    pub chart_kind: ChartKind,
    // Hit-test rects, updated at render time.
    pub stages_area: Rect,
    pub editor_area: Rect,
    pub results_area: Rect,
}

impl AggState {
    pub fn new(db: String, coll: String, initial: String) -> Self {
        let mut s = Self {
            db,
            coll,
            editor: TextArea::from_text(&initial),
            focus: AggFocus::Editor,
            stages: Vec::new(),
            selected_stage: 0,
            error: None,
            running: false,
            ran_through: None,
            docs: Vec::new(),
            folds: Vec::new(),
            lines: Vec::new(),
            cursor: 0,
            scroll: 0,
            chart: false,
            chart_kind: ChartKind::Auto,
            stages_area: Rect::default(),
            editor_area: Rect::default(),
            results_area: Rect::default(),
        };
        // Populate the stage list from the initial text (ignore errors).
        let _ = s.parse();
        s
    }

    /// Parse the editor text; updates the stage list or the error banner.
    pub fn parse(&mut self) -> Option<Vec<Document>> {
        match parse_pipeline(&self.editor.text()) {
            Ok(stages) => {
                self.stages = stages.iter().map(stage_name).collect();
                self.selected_stage = self.selected_stage.min(self.stages.len() - 1);
                self.error = None;
                Some(stages)
            }
            Err(e) => {
                self.error = Some(e);
                None
            }
        }
    }

    pub fn set_docs(&mut self, docs: Vec<Document>, ran_through: usize) {
        self.folds = docs
            .iter()
            .map(|_| {
                let mut collapsed = HashSet::new();
                collapsed.insert(String::new());
                collapsed
            })
            .collect();
        self.docs = docs;
        self.running = false;
        self.ran_through = Some(ran_through);
        self.cursor = 0;
        self.scroll = 0;
        self.rebuild_lines();
    }

    pub fn rebuild_lines(&mut self) {
        self.lines.clear();
        for (i, doc) in self.docs.iter().enumerate() {
            self.lines
                .extend(doc_lines(i, i as u64 + 1, doc, &self.folds[i]));
        }
        self.cursor = self.cursor.min(self.lines.len().saturating_sub(1));
    }

    pub fn toggle_fold_at_cursor(&mut self) {
        let Some(rline) = self.lines.get(self.cursor) else {
            return;
        };
        let Some(path) = rline.fold_path.clone() else {
            return;
        };
        let doc_idx = rline.doc_idx;
        if !self.folds[doc_idx].remove(&path) {
            self.folds[doc_idx].insert(path);
        }
        self.rebuild_lines();
    }
}

/// Numeric value of a BSON scalar, if any.
fn numeric(v: &Bson) -> Option<f64> {
    match v {
        Bson::Int32(n) => Some(f64::from(*n)),
        Bson::Int64(n) => Some(*n as f64),
        Bson::Double(n) => Some(*n),
        _ => None,
    }
}

/// X coordinate of an _id for line/scatter charts: dates become epoch
/// millis, numbers pass through.
fn x_value(v: &Bson) -> Option<(f64, XKind)> {
    match v {
        Bson::DateTime(dt) => Some((dt.timestamp_millis() as f64, XKind::Date)),
        other => numeric(other).map(|n| (n, XKind::Number)),
    }
}

impl AggState {
    /// Extract (label, value) pairs when every result doc looks like
    /// `{_id, <numeric>}` — the shape $group/$count/$bucket produce.
    /// Prefers well-known value keys, else the first numeric field.
    pub fn chart_data(&self) -> Option<Vec<(String, u64)>> {
        if self.docs.is_empty() {
            return None;
        }
        const PREFERRED: [&str; 6] = ["n", "count", "total", "sum", "value", "avg"];
        let first = &self.docs[0];
        let value_key = PREFERRED
            .iter()
            .find(|k| first.get(**k).is_some_and(|v| numeric(v).is_some()))
            .map(|k| k.to_string())
            .or_else(|| {
                first
                    .iter()
                    .find(|(k, v)| *k != "_id" && numeric(v).is_some())
                    .map(|(k, _)| k.clone())
            })?;
        let mut data = Vec::with_capacity(self.docs.len());
        for doc in &self.docs {
            let value = numeric(doc.get(&value_key)?)?;
            let label = doc
                .get("_id")
                .map(lazymongo_core::display::bson_to_compact)
                .unwrap_or_default();
            data.push((label, value.max(0.0).round() as u64));
        }
        Some(data)
    }

    /// (x, y) series for line/scatter charts, when every _id is a date or a
    /// number. Points come back sorted by x.
    pub fn xy_series(&self) -> Option<(Vec<(f64, f64)>, XKind)> {
        if self.docs.is_empty() {
            return None;
        }
        let labeled = self.chart_data()?; // reuse value-key detection for y
        let mut kind = None;
        let mut points = Vec::with_capacity(self.docs.len());
        for (doc, (_, y)) in self.docs.iter().zip(labeled) {
            let (x, k) = x_value(doc.get("_id")?)?;
            match kind {
                None => kind = Some(k),
                Some(prev) if prev != k => return None, // mixed axis types
                _ => {}
            }
            points.push((x, y as f64));
        }
        points.sort_by(|a, b| a.0.total_cmp(&b.0));
        Some((points, kind?))
    }

    /// The chart kind that will actually be drawn.
    pub fn effective_chart_kind(&self) -> ChartKind {
        match self.chart_kind {
            ChartKind::Auto => {
                if self.xy_series().is_some() {
                    ChartKind::Line
                } else {
                    ChartKind::Bars
                }
            }
            other => other,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lazymongo_core::bson::doc;

    fn state_with(docs: Vec<Document>) -> AggState {
        let mut s = AggState::new("d".into(), "c".into(), DEFAULT_PIPELINE.into());
        s.docs = docs;
        s
    }

    #[test]
    fn group_count_shape_charts() {
        let s = state_with(vec![
            doc! { "_id": "active", "n": 334 },
            doc! { "_id": "inactive", "n": 166 },
        ]);
        let data = s.chart_data().unwrap();
        assert_eq!(data[0], ("active".into(), 334));
        assert_eq!(data[1], ("inactive".into(), 166));
    }

    #[test]
    fn falls_back_to_first_numeric_field() {
        let s = state_with(vec![doc! { "_id": 1, "revenue": 12.6 }]);
        assert_eq!(s.chart_data().unwrap()[0].1, 13);
    }

    #[test]
    fn date_ids_become_sorted_line_series() {
        use lazymongo_core::bson::DateTime;
        let s = state_with(vec![
            doc! { "_id": DateTime::from_millis(2_000), "n": 5 },
            doc! { "_id": DateTime::from_millis(1_000), "n": 3 },
        ]);
        let (points, kind) = s.xy_series().unwrap();
        assert_eq!(kind, XKind::Date);
        assert_eq!(points, vec![(1_000.0, 3.0), (2_000.0, 5.0)]); // sorted by x
        assert_eq!(s.effective_chart_kind(), ChartKind::Line);
    }

    #[test]
    fn numeric_ids_chart_as_line_strings_as_bars() {
        let nums = state_with(vec![doc! { "_id": 20, "n": 1 }, doc! { "_id": 30, "n": 2 }]);
        assert_eq!(nums.xy_series().unwrap().1, XKind::Number);
        let cats = state_with(vec![doc! { "_id": "active", "n": 1 }]);
        assert!(cats.xy_series().is_none());
        assert_eq!(cats.effective_chart_kind(), ChartKind::Bars);
    }

    #[test]
    fn non_chartable_shapes_rejected() {
        assert!(state_with(vec![]).chart_data().is_none());
        assert!(state_with(vec![doc! { "_id": 1, "name": "x" }])
            .chart_data()
            .is_none());
    }
}
