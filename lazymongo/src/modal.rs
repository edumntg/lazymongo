//! Modal state: overlays that capture input while open.

use std::collections::HashSet;

use lazymongo_core::bson::Document;

use crate::input::Input;
use crate::json_view::{doc_lines, RLine};

pub enum Modal {
    None,
    Help,
    QueryEditor(QueryEditor),
    DocView(DocView),
}

impl Modal {
    pub fn is_open(&self) -> bool {
        !matches!(self, Modal::None)
    }
}

/// Structured find editor (FR-12): filter / projection / sort / limit / skip.
pub struct QueryEditor {
    pub fields: [Input; 5],
    pub focus: usize,
    pub error: Option<String>,
}

pub const QUERY_FIELD_LABELS: [&str; 5] = ["filter", "projection", "sort", "limit", "skip"];

impl QueryEditor {
    pub fn new(filter: &str, projection: &str, sort: &str, limit: &str, skip: &str) -> Self {
        Self {
            fields: [
                Input::with_text(filter),
                Input::with_text(projection),
                Input::with_text(sort),
                Input::with_text(limit),
                Input::with_text(skip),
            ],
            focus: 0,
            error: None,
        }
    }
}

/// Full-screen scrollable single-document view (FR-23), also used for
/// explain plans (FR-15).
pub struct DocView {
    pub title: String,
    pub doc: Document,
    pub folds: HashSet<String>,
    pub lines: Vec<RLine>,
    pub cursor: usize,
    pub scroll: usize,
    /// Warning banner, e.g. COLLSCAN flag on explain output.
    pub warn: Option<String>,
}

impl DocView {
    pub fn new(title: String, doc: Document, warn: Option<String>) -> Self {
        let mut v = Self {
            title,
            doc,
            folds: HashSet::new(), // open fully expanded
            lines: Vec::new(),
            cursor: 0,
            scroll: 0,
            warn,
        };
        v.rebuild();
        v
    }

    pub fn rebuild(&mut self) {
        self.lines = doc_lines(0, 1, &self.doc, &self.folds);
        self.cursor = self.cursor.min(self.lines.len().saturating_sub(1));
    }

    pub fn toggle_fold_at_cursor(&mut self) {
        let Some(rline) = self.lines.get(self.cursor) else {
            return;
        };
        let Some(path) = rline.fold_path.clone() else {
            return;
        };
        if !self.folds.remove(&path) {
            self.folds.insert(path);
        }
        self.rebuild();
    }
}
