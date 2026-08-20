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
    /// Saved-connection picker shown at startup when no URI was given (FR-2),
    /// or any time via the C key.
    Connections {
        items: Vec<SavedConnection>,
        selected: usize,
    },
    /// Add/edit form for a saved connection.
    ConnForm(ConnForm),
    /// Fuzzy command palette (FR-36).
    Palette(Palette),
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
    ApplyEdit {
        id: Bson,
        doc: Document,
    },
    DeleteOne {
        id: Bson,
    },
    DeleteMany {
        filter: Document,
    },
    UpdateMany {
        filter: Document,
        update: Document,
    },
    CreateIndex {
        keys: Document,
    },
    DropIndex {
        name: String,
    },
    DropCollection {
        db: String,
        coll: String,
    },
    /// Remove a saved connection from config.toml.
    DeleteConnection {
        items: Vec<SavedConnection>,
        index: usize,
    },
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

/// Add/edit form for a saved connection. Fields: name, uri, uri_env;
/// plus a read-only toggle. Writes back to config.toml on save.
pub struct ConnForm {
    /// Snapshot of the full connections list being edited.
    pub items: Vec<SavedConnection>,
    /// Index being edited; None = adding a new connection.
    pub editing: Option<usize>,
    pub fields: [Input; 3],
    pub read_only: bool,
    /// 0..=2 = text fields, 3 = read-only toggle.
    pub focus: usize,
    pub error: Option<String>,
}

pub const CONN_FIELD_LABELS: [&str; 3] = ["name", "uri", "uri_env"];

impl ConnForm {
    pub fn new(items: Vec<SavedConnection>, editing: Option<usize>) -> Self {
        let (name, uri, uri_env, read_only) = match editing.and_then(|i| items.get(i)) {
            Some(c) => (
                c.name.clone(),
                c.uri.clone().unwrap_or_default(),
                c.uri_env.clone().unwrap_or_default(),
                c.read_only,
            ),
            None => Default::default(),
        };
        Self {
            items,
            editing,
            fields: [
                Input::with_text(name),
                Input::with_text(uri),
                Input::with_text(uri_env),
            ],
            read_only,
            focus: 0,
            error: None,
        }
    }

    /// Validate and build the connection from the form fields.
    pub fn build(&self) -> Result<SavedConnection, String> {
        let name = self.fields[0].text.trim().to_string();
        let uri = self.fields[1].text.trim().to_string();
        let uri_env = self.fields[2].text.trim().to_string();
        if name.is_empty() {
            return Err("name cannot be empty".into());
        }
        if uri.is_empty() && uri_env.is_empty() {
            return Err("set uri or uri_env".into());
        }
        let duplicate = self
            .items
            .iter()
            .enumerate()
            .any(|(i, c)| Some(i) != self.editing && c.name == name);
        if duplicate {
            return Err(format!("a connection named \"{name}\" already exists"));
        }
        Ok(SavedConnection {
            name,
            uri: (!uri.is_empty()).then_some(uri),
            uri_env: (!uri_env.is_empty()).then_some(uri_env),
            read_only: self.read_only,
        })
    }
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

/// Every UI action reachable from the command palette.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppAction {
    ToggleView,
    QueryEditor,
    Explain,
    DocView,
    CopyDoc,
    Export,
    EditDoc,
    InsertDoc,
    DeleteDoc,
    DeleteMany,
    UpdateMany,
    Indexes,
    OpsLog,
    Aggregate,
    OpenShell,
    Connections,
    Refresh,
    Help,
    Quit,
    SetTheme(&'static str),
}

/// Fuzzy command palette (FR-36): every feature reachable by name.
pub struct Palette {
    pub input: Input,
    pub actions: Vec<(String, AppAction)>,
    /// Indices into `actions`, best match first.
    pub filtered: Vec<usize>,
    pub selected: usize,
}

impl Palette {
    pub fn new(actions: Vec<(String, AppAction)>) -> Self {
        let filtered = (0..actions.len()).collect();
        Self {
            input: Input::default(),
            actions,
            filtered,
            selected: 0,
        }
    }

    pub fn refilter(&mut self) {
        let needle = self.input.text.trim().to_lowercase();
        let mut scored: Vec<(i32, usize)> = self
            .actions
            .iter()
            .enumerate()
            .filter_map(|(i, (label, _))| fuzzy_score(&needle, label).map(|s| (s, i)))
            .collect();
        scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        self.filtered = scored.into_iter().map(|(_, i)| i).collect();
        self.selected = self.selected.min(self.filtered.len().saturating_sub(1));
    }

    pub fn selected_action(&self) -> Option<AppAction> {
        self.filtered.get(self.selected).map(|&i| self.actions[i].1)
    }
}

/// Case-insensitive subsequence match; higher scores for consecutive runs
/// and earlier matches. None = no match.
pub fn fuzzy_score(needle: &str, haystack: &str) -> Option<i32> {
    if needle.is_empty() {
        return Some(0);
    }
    let hay: Vec<char> = haystack.to_lowercase().chars().collect();
    let mut score = 0i32;
    let mut hi = 0usize;
    let mut last_hit: Option<usize> = None;
    for nc in needle.chars() {
        let mut found = None;
        while hi < hay.len() {
            if hay[hi] == nc {
                found = Some(hi);
                break;
            }
            hi += 1;
        }
        let pos = found?;
        score += match last_hit {
            Some(prev) if pos == prev + 1 => 3, // consecutive bonus
            _ => 1,
        };
        last_hit = Some(pos);
        hi = pos + 1;
    }
    // Earlier first-hit is slightly better.
    score -= (last_hit.unwrap_or(0) as i32) / 8;
    Some(score)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fuzzy_basics() {
        assert!(fuzzy_score("", "anything").is_some());
        assert!(fuzzy_score("tbl", "view: toggle json/table (v)").is_some());
        assert!(fuzzy_score("zzz", "view: toggle").is_none());
        // consecutive run outranks scattered subsequence
        let a = fuzzy_score("theme", "theme: dark").unwrap();
        let b = fuzzy_score("theme", "t h e m e scattered").unwrap();
        assert!(a > b);
    }

    #[test]
    fn palette_filters_and_ranks() {
        let mut p = Palette::new(vec![
            ("view: toggle json/table".into(), AppAction::ToggleView),
            ("theme: dark".into(), AppAction::SetTheme("dark")),
            ("theme: termius".into(), AppAction::SetTheme("termius")),
        ]);
        p.input = Input::with_text("theme");
        p.refilter();
        assert_eq!(p.filtered.len(), 2);
        assert!(matches!(p.selected_action(), Some(AppAction::SetTheme(_))));
    }
}
