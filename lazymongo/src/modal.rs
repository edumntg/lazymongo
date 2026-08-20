//! Modal state: overlays that capture input while open.

use std::collections::HashSet;

use lazymongo_core::bson::{Bson, Document};
use lazymongo_core::types::IndexInfo;

use crate::config::SavedConnection;
use crate::input::Input;
use crate::json_view::{doc_lines, RLine};
use crate::textarea::TextArea;

pub enum Modal {
    None,
    Help,
    QueryEditor(QueryEditor),
    DocView(DocView),
    Confirm(Confirm),
    Editor(JsonEditor),
    Indexes(IndexesView),
    OpsLog {
        scroll: usize,
    },
    Prompt(Prompt),
    /// Saved-connection picker shown at startup when no URI was given (FR-2).
    Connections {
        items: Vec<SavedConnection>,
        selected: usize,
    },
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

/// A destructive (or write) action waiting for confirmation (FR-29/30/31).
pub enum PendingAction {
    ApplyEdit { id: Bson, doc: Document },
    DeleteOne { id: Bson },
    DeleteMany { filter: Document },
    UpdateMany { filter: Document, update: Document },
    CreateIndex { keys: Document },
    DropIndex { name: String },
    DropCollection { db: String, coll: String },
}

/// Confirmation modal. When `typed_required` is set, the user must type
/// that exact string before `y`/Enter confirms (FR-29 delete-many,
/// FR-31 drop collection).
pub struct Confirm {
    pub title: String,
    pub body: Vec<String>,
    pub typed_required: Option<String>,
    pub typed: Input,
    pub action: PendingAction,
}

impl Confirm {
    pub fn typed_ok(&self) -> bool {
        match &self.typed_required {
            None => true,
            Some(required) => self.typed.text.trim() == required.as_str(),
        }
    }
}

/// What the JSON editor's submitted content is for.
pub enum EditorPurpose {
    /// Replace the document with this _id (FR-27).
    EditDoc { id: Bson, original: Document },
    /// Insert a new document (FR-28).
    InsertDoc,
    /// Update document for update-many over the current filter (FR-30).
    UpdateMany { filter: Document },
    /// Key spec for a new index (FR-31).
    CreateIndexKeys,
}

/// Multi-line JSON editor modal (Ctrl-S submits).
pub struct JsonEditor {
    pub title: String,
    pub area: TextArea,
    pub purpose: EditorPurpose,
    pub error: Option<String>,
}

/// Index list for the current collection (FR-10/FR-31).
pub struct IndexesView {
    pub db: String,
    pub coll: String,
    pub indexes: Option<Vec<IndexInfo>>, // None while loading
    pub selected: usize,
}

/// What a submitted prompt value is used for.
pub enum PromptAction {
    CreateCollection { db: String },
}

/// Tiny single-line prompt (e.g. new collection name).
pub struct Prompt {
    pub title: String,
    pub input: Input,
    pub action: PromptAction,
}
