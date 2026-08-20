//! Aggregation screen state (FR-17/18/19): a full-screen pipeline editor
//! with a stage list and stage-by-stage result preview.

use std::collections::HashSet;

use lazymongo_core::bson::Document;
use lazymongo_core::query::{parse_pipeline, stage_name};
use ratatui::layout::Rect;

use crate::json_view::{doc_lines, RLine};
use crate::textarea::TextArea;

/// Docs fetched per preview run.
pub const AGG_PREVIEW_LIMIT: usize = 50;

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
