//! Application model and update loop (Elm-style): one `App` struct, updated
//! by input events and core events, rendered by `ui::draw`.

use std::collections::HashSet;
use std::time::{Duration, Instant};

use anyhow::Result;
use lazymongo_core::actor;
use lazymongo_core::bson::Document;
use lazymongo_core::query::{parse_filter, parse_optional_doc};
use lazymongo_core::types::{
    CollectionInfo, Command, CoreEvent, DatabaseInfo, FindSpec, BATCH_SIZE,
};
use ratatui::crossterm::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::layout::{Position, Rect};
use tokio::sync::mpsc;

use crate::input::char_to_byte;
use crate::json_view::{doc_lines, RLine};
use crate::modal::{DocView, Modal, QueryEditor};
use crate::{event, term, ui, util};

/// Memory cap: max documents held in the sliding window (NFR-3).
pub const MAX_DOCS: usize = 2000;
const TOAST_TTL: Duration = Duration::from_secs(4);
/// Max columns inferred for the table view (FR-22).
const MAX_TABLE_COLS: usize = 24;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pane {
    Explorer,
    Results,
    Query,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewMode {
    Json,
    Table,
}

pub enum ConnState {
    Connecting,
    Connected { version: String, ping_ms: u64 },
    Failed(String),
}

pub struct DbNode {
    pub info: DatabaseInfo,
    pub expanded: bool,
    pub colls: Option<Vec<CollectionInfo>>,
    pub loading: bool,
}

#[derive(Debug, Clone, Copy)]
pub enum ExplorerRow {
    Db(usize),
    Coll { db: usize, coll: usize },
}

#[derive(Default)]
pub struct Explorer {
    pub dbs: Vec<DbNode>,
    pub selected: usize,
    pub scroll: usize,
    pub loading: bool,
    pub filter: String,
    pub filtering: bool, // true while the user is typing in the filter
}

impl Explorer {
    /// Flattened, filtered rows currently visible in the sidebar.
    pub fn rows(&self) -> Vec<ExplorerRow> {
        let needle = self.filter.to_lowercase();
        let mut rows = Vec::new();
        for (di, node) in self.dbs.iter().enumerate() {
            let db_match = needle.is_empty() || node.info.name.to_lowercase().contains(&needle);
            let matching_colls: Vec<usize> = node
                .colls
                .iter()
                .flatten()
                .enumerate()
                .filter(|(_, c)| needle.is_empty() || c.name.to_lowercase().contains(&needle))
                .map(|(i, _)| i)
                .collect();
            if !db_match && matching_colls.is_empty() {
                continue;
            }
            rows.push(ExplorerRow::Db(di));
            if node.expanded {
                for ci in &matching_colls {
                    rows.push(ExplorerRow::Coll { db: di, coll: *ci });
                }
            }
        }
        rows
    }
}

/// Table view state (FR-22). Column x-ranges are filled in at render time
/// for mouse hit-testing.
#[derive(Default)]
pub struct TableView {
    pub columns: Vec<String>,
    pub widths: Vec<u16>,
    pub active_col: usize,
    pub row: usize,
    pub scroll_row: usize,
    pub col_offset: usize,
    /// (x_start, x_end, column index) of visible columns, from last render.
    pub col_hit: Vec<(u16, u16, usize)>,
}

impl TableView {
    fn recompute(&mut self, docs: &[Document]) {
        let mut columns: Vec<String> = Vec::new();
        for doc in docs {
            for key in doc.keys() {
                if !columns.iter().any(|c| c == key) {
                    columns.push(key.clone());
                    if columns.len() >= MAX_TABLE_COLS {
                        break;
                    }
                }
            }
            if columns.len() >= MAX_TABLE_COLS {
                break;
            }
        }
        // _id first when present.
        if let Some(pos) = columns.iter().position(|c| c == "_id") {
            let id = columns.remove(pos);
            columns.insert(0, id);
        }
        let mut widths: Vec<u16> = columns
            .iter()
            .map(|c| c.chars().count().clamp(4, 30) as u16)
            .collect();
        for doc in docs.iter().take(200) {
            for (i, col) in columns.iter().enumerate() {
                if let Some(v) = doc.get(col) {
                    let w = util::bson_to_compact(v).chars().count().clamp(4, 30) as u16;
                    widths[i] = widths[i].max(w);
                }
            }
        }
        self.columns = columns;
        self.widths = widths;
        self.active_col = self.active_col.min(self.columns.len().saturating_sub(1));
    }
}

#[derive(Default)]
pub struct Results {
    /// (db, collection) currently open.
    pub target: Option<(String, String)>,
    pub docs: Vec<Document>,
    /// Collapsed fold paths per document ("" = whole doc collapsed).
    pub folds: Vec<HashSet<String>>,
    /// Cached rendered lines; rebuilt when `dirty`.
    pub lines: Vec<RLine>,
    pub dirty: bool,
    pub cursor: usize,
    pub scroll: usize,
    pub exhausted: bool,
    pub loading: bool,
    pub total_estimate: Option<u64>,
    /// Documents evicted from the front of the window.
    pub evicted: u64,
    /// Spec used for the active find (already parsed).
    pub active_spec: FindSpec,
    pub table: TableView,
}

impl Results {
    pub fn rebuild_lines(&mut self) {
        self.lines.clear();
        for (i, doc) in self.docs.iter().enumerate() {
            let number = self.evicted + i as u64 + 1;
            self.lines.extend(doc_lines(i, number, doc, &self.folds[i]));
        }
        self.cursor = self.cursor.min(self.lines.len().saturating_sub(1));
        self.table.recompute(&self.docs);
        self.table.row = self.table.row.min(self.docs.len().saturating_sub(1));
        self.dirty = false;
    }
}

#[derive(Default)]
pub struct QueryBar {
    pub input: String,
    pub cursor: usize, // char index
    pub history: Vec<String>,
    pub hist_pos: Option<usize>,
    pub error: Option<String>,
}

/// The non-filter parts of the find spec, kept as user-entered strings.
#[derive(Default, Clone)]
pub struct SpecExtras {
    pub projection: String,
    pub sort: String,
    pub limit: String,
    pub skip: String,
}

impl SpecExtras {
    pub fn is_default(&self) -> bool {
        self.projection.trim().is_empty()
            && self.sort.trim().is_empty()
            && self.limit.trim().is_empty()
            && self.skip.trim().is_empty()
    }
}

pub struct App {
    pub conn: ConnState,
    pub uri_display: String,
    pub read_only: bool,
    pub focus: Pane,
    pub view: ViewMode,
    pub explorer: Explorer,
    pub results: Results,
    pub query: QueryBar,
    pub extras: SpecExtras,
    pub modal: Modal,
    pub toast: Option<(String, bool, Instant)>, // (message, is_error, when)
    pub spinner_frame: usize,
    pub should_quit: bool,
    pub generation: u64,
    cmd_tx: mpsc::Sender<Command>,
    // Pane hit-test rects, updated by ui::draw each frame.
    pub explorer_area: Rect,
    pub results_area: Rect,
    pub query_area: Rect,
}

impl App {
    pub fn new(uri_display: String, cmd_tx: mpsc::Sender<Command>, read_only: bool) -> Self {
        Self {
            conn: ConnState::Connecting,
            uri_display,
            read_only,
            focus: Pane::Explorer,
            view: ViewMode::Json,
            explorer: Explorer::default(),
            results: Results::default(),
            query: QueryBar::default(),
            extras: SpecExtras::default(),
            modal: Modal::None,
            toast: None,
            spinner_frame: 0,
            should_quit: false,
            generation: 0,
            cmd_tx,
            explorer_area: Rect::default(),
            results_area: Rect::default(),
            query_area: Rect::default(),
        }
    }

    pub fn send(&mut self, cmd: Command) {
        if self.cmd_tx.try_send(cmd).is_err() {
            self.toast_err("busy: command queue full, try again".into());
        }
    }

    pub fn toast_err(&mut self, msg: String) {
        self.toast = Some((msg, true, Instant::now()));
    }

    pub fn toast_info(&mut self, msg: String) {
        self.toast = Some((msg, false, Instant::now()));
    }

    // ---------- core events ----------

    pub fn on_core(&mut self, ev: CoreEvent) {
        match ev {
            CoreEvent::Connected {
                server_version,
                ping_ms,
            } => {
                self.conn = ConnState::Connected {
                    version: server_version,
                    ping_ms,
                };
                self.explorer.loading = true;
                self.send(Command::ListDatabases);
            }
            CoreEvent::ConnectFailed(e) => self.conn = ConnState::Failed(e),
            CoreEvent::Databases(dbs) => {
                self.explorer.loading = false;
                self.explorer.dbs = dbs
                    .into_iter()
                    .map(|info| DbNode {
                        info,
                        expanded: false,
                        colls: None,
                        loading: false,
                    })
                    .collect();
                self.explorer.selected = 0;
            }
            CoreEvent::Collections { db, colls } => {
                if let Some(node) = self.explorer.dbs.iter_mut().find(|n| n.info.name == db) {
                    node.colls = Some(colls);
                    node.loading = false;
                }
            }
            CoreEvent::Batch {
                generation,
                docs,
                exhausted,
                total_estimate,
            } => {
                if generation != self.generation {
                    return; // stale query
                }
                self.results.loading = false;
                self.results.exhausted = exhausted;
                if let Some(t) = total_estimate {
                    self.results.total_estimate = Some(t);
                }
                for _ in 0..docs.len() {
                    let mut collapsed = HashSet::new();
                    collapsed.insert(String::new()); // docs arrive collapsed
                    self.results.folds.push(collapsed);
                }
                self.results.docs.extend(docs);
                // Sliding window eviction (NFR-3).
                while self.results.docs.len() > MAX_DOCS {
                    let n = BATCH_SIZE.min(self.results.docs.len());
                    self.results.docs.drain(..n);
                    self.results.folds.drain(..n);
                    self.results.evicted += n as u64;
                }
                if self.results.evicted > 0 {
                    self.toast_info(format!(
                        "{} earlier docs unloaded to cap memory (r reloads from start)",
                        self.results.evicted
                    ));
                }
                self.results.dirty = true;
            }
            CoreEvent::ExplainResult(plan) => {
                let warn = if util::has_collscan(&plan) {
                    Some("COLLSCAN: this query does a full collection scan (no index)".into())
                } else {
                    None
                };
                self.modal =
                    Modal::DocView(DocView::new("Explain (executionStats)".into(), plan, warn));
            }
            CoreEvent::Ping { ms } => {
                if let ConnState::Connected { ping_ms, .. } = &mut self.conn {
                    *ping_ms = ms;
                }
            }
            CoreEvent::Error(e) => {
                self.results.loading = false;
                self.toast_err(e);
            }
            // Wired up with the write flows (M3) and aggregations (M4).
            CoreEvent::WriteDone { summary, .. } => self.toast_info(summary),
            CoreEvent::CountResult { .. }
            | CoreEvent::Indexes { .. }
            | CoreEvent::AggBatch { .. } => {}
        }
    }

    pub fn on_tick(&mut self) {
        self.spinner_frame = self.spinner_frame.wrapping_add(1);
        if let Some((_, _, when)) = &self.toast {
            if when.elapsed() > TOAST_TTL {
                self.toast = None;
            }
        }
    }

    // ---------- input ----------

    pub fn on_input(&mut self, ev: Event) {
        match ev {
            Event::Key(key) if key.kind == KeyEventKind::Press => self.on_key(key),
            Event::Mouse(m) => self.on_mouse(m),
            _ => {}
        }
    }

    fn on_key(&mut self, key: KeyEvent) {
        // Ctrl-C always quits.
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.should_quit = true;
            return;
        }
        if self.modal.is_open() {
            return self.on_key_modal(key);
        }
        // Text-input modes capture printable keys first.
        if self.focus == Pane::Query {
            return self.on_key_query(key);
        }
        if self.focus == Pane::Explorer && self.explorer.filtering {
            return self.on_key_explorer_filter(key);
        }

        match key.code {
            KeyCode::Char('q') => self.should_quit = true,
            KeyCode::Char('?') => self.modal = Modal::Help,
            KeyCode::Tab => self.cycle_focus(false),
            KeyCode::BackTab => self.cycle_focus(true),
            KeyCode::Char('1') => self.focus = Pane::Explorer,
            KeyCode::Char('2') => self.focus = Pane::Results,
            KeyCode::Char('3') => self.focus = Pane::Query,
            KeyCode::Char('r') => self.refresh(),
            _ => match self.focus {
                Pane::Explorer => self.on_key_explorer(key),
                Pane::Results => self.on_key_results(key),
                Pane::Query => unreachable!(),
            },
        }
    }

    // ---------- modals ----------

    fn on_key_modal(&mut self, key: KeyEvent) {
        match &mut self.modal {
            Modal::None => {}
            Modal::Help => self.modal = Modal::None,
            Modal::DocView(view) => {
                let page = 20usize;
                match key.code {
                    KeyCode::Esc | KeyCode::Char('q') => self.modal = Modal::None,
                    KeyCode::Down | KeyCode::Char('j') => {
                        view.cursor = (view.cursor + 1).min(view.lines.len().saturating_sub(1))
                    }
                    KeyCode::Up | KeyCode::Char('k') => view.cursor = view.cursor.saturating_sub(1),
                    KeyCode::PageDown => {
                        view.cursor = (view.cursor + page).min(view.lines.len().saturating_sub(1))
                    }
                    KeyCode::PageUp => view.cursor = view.cursor.saturating_sub(page),
                    KeyCode::Char('g') | KeyCode::Home => view.cursor = 0,
                    KeyCode::Char('G') | KeyCode::End => {
                        view.cursor = view.lines.len().saturating_sub(1)
                    }
                    KeyCode::Enter | KeyCode::Char(' ') => view.toggle_fold_at_cursor(),
                    KeyCode::Char('y') => {
                        let text = util::doc_to_pretty(&view.doc);
                        match util::clipboard_copy(&text) {
                            Ok(()) => self.toast_info("document copied".into()),
                            Err(e) => self.toast_err(e),
                        }
                    }
                    _ => {}
                }
            }
            Modal::QueryEditor(editor) => match key.code {
                KeyCode::Esc => self.modal = Modal::None,
                KeyCode::Tab | KeyCode::Down => editor.focus = (editor.focus + 1) % 5,
                KeyCode::BackTab | KeyCode::Up => editor.focus = (editor.focus + 4) % 5,
                KeyCode::Enter => self.submit_query_editor(),
                _ => {
                    editor.fields[editor.focus].on_key(key);
                    editor.error = None;
                }
            },
        }
    }

    fn open_query_editor(&mut self) {
        if self.results.target.is_none() {
            self.toast_err("select a collection first".into());
            return;
        }
        self.modal = Modal::QueryEditor(QueryEditor::new(
            &self.query.input,
            &self.extras.projection,
            &self.extras.sort,
            &self.extras.limit,
            &self.extras.skip,
        ));
    }

    fn submit_query_editor(&mut self) {
        let Modal::QueryEditor(editor) = &mut self.modal else {
            return;
        };
        let texts: Vec<String> = editor.fields.iter().map(|f| f.text.clone()).collect();
        match build_spec(&texts[0], &texts[1], &texts[2], &texts[3], &texts[4]) {
            Err(e) => editor.error = Some(e),
            Ok(spec) => {
                self.query.input = texts[0].clone();
                self.query.cursor = self.query.input.chars().count();
                self.extras.projection = texts[1].clone();
                self.extras.sort = texts[2].clone();
                self.extras.limit = texts[3].clone();
                self.extras.skip = texts[4].clone();
                self.modal = Modal::None;
                self.push_history();
                let Some((db, coll)) = self.results.target.clone() else {
                    return;
                };
                self.focus = Pane::Results;
                self.start_find(db, coll, spec);
            }
        }
    }

    fn cycle_focus(&mut self, back: bool) {
        self.focus = match (self.focus, back) {
            (Pane::Explorer, false) => Pane::Results,
            (Pane::Results, false) => Pane::Query,
            (Pane::Query, false) => Pane::Explorer,
            (Pane::Explorer, true) => Pane::Query,
            (Pane::Results, true) => Pane::Explorer,
            (Pane::Query, true) => Pane::Results,
        };
    }

    fn refresh(&mut self) {
        match self.focus {
            Pane::Explorer => {
                self.explorer.loading = true;
                self.send(Command::ListDatabases);
                let expanded: Vec<String> = self
                    .explorer
                    .dbs
                    .iter()
                    .filter(|n| n.expanded)
                    .map(|n| n.info.name.clone())
                    .collect();
                for db in expanded {
                    self.send(Command::ListCollections { db });
                }
            }
            Pane::Results | Pane::Query => self.rerun_find(),
        }
    }

    pub fn rerun_find(&mut self) {
        let Some((db, coll)) = self.results.target.clone() else {
            return;
        };
        let spec = self.results.active_spec.clone();
        self.start_find(db, coll, spec);
    }

    fn start_find(&mut self, db: String, coll: String, spec: FindSpec) {
        self.generation += 1;
        let r = &mut self.results;
        r.target = Some((db.clone(), coll.clone()));
        r.docs.clear();
        r.folds.clear();
        r.lines.clear();
        r.cursor = 0;
        r.scroll = 0;
        r.table.row = 0;
        r.table.scroll_row = 0;
        r.exhausted = false;
        r.loading = true;
        r.total_estimate = None;
        r.evicted = 0;
        r.active_spec = spec.clone();
        r.dirty = true;
        let generation = self.generation;
        self.send(Command::StartFind {
            generation,
            db,
            coll,
            spec,
        });
    }

    // ---------- explorer ----------

    fn on_key_explorer(&mut self, key: KeyEvent) {
        let rows = self.explorer.rows();
        match key.code {
            KeyCode::Down | KeyCode::Char('j') => {
                if self.explorer.selected + 1 < rows.len() {
                    self.explorer.selected += 1;
                }
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.explorer.selected = self.explorer.selected.saturating_sub(1);
            }
            KeyCode::Char('g') | KeyCode::Home => self.explorer.selected = 0,
            KeyCode::Char('G') | KeyCode::End => {
                self.explorer.selected = rows.len().saturating_sub(1)
            }
            KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') => {
                self.activate_explorer_row();
            }
            KeyCode::Left | KeyCode::Char('h') => {
                if let Some(row) = rows.get(self.explorer.selected).copied() {
                    match row {
                        ExplorerRow::Db(di) => self.explorer.dbs[di].expanded = false,
                        ExplorerRow::Coll { db, .. } => {
                            self.explorer.dbs[db].expanded = false;
                            // Jump selection to the parent db row.
                            let rows = self.explorer.rows();
                            if let Some(pos) = rows
                                .iter()
                                .position(|r| matches!(r, ExplorerRow::Db(d) if *d == db))
                            {
                                self.explorer.selected = pos;
                            }
                        }
                    }
                }
            }
            KeyCode::Char('/') => {
                self.explorer.filtering = true;
            }
            KeyCode::Esc => {
                self.explorer.filter.clear();
            }
            _ => {}
        }
        let n = self.explorer.rows().len();
        self.explorer.selected = self.explorer.selected.min(n.saturating_sub(1));
    }

    fn on_key_explorer_filter(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                self.explorer.filter.clear();
                self.explorer.filtering = false;
            }
            KeyCode::Enter => self.explorer.filtering = false,
            KeyCode::Backspace => {
                self.explorer.filter.pop();
            }
            KeyCode::Char(c) => self.explorer.filter.push(c),
            _ => {}
        }
        let n = self.explorer.rows().len();
        self.explorer.selected = self.explorer.selected.min(n.saturating_sub(1));
    }

    fn activate_explorer_row(&mut self) {
        let rows = self.explorer.rows();
        let Some(row) = rows.get(self.explorer.selected).copied() else {
            return;
        };
        match row {
            ExplorerRow::Db(di) => {
                let node = &mut self.explorer.dbs[di];
                node.expanded = !node.expanded;
                if node.expanded && node.colls.is_none() && !node.loading {
                    node.loading = true;
                    let db = node.info.name.clone();
                    self.send(Command::ListCollections { db });
                }
            }
            ExplorerRow::Coll { db, coll } => {
                let db_name = self.explorer.dbs[db].info.name.clone();
                let coll_name = self.explorer.dbs[db].colls.as_ref().unwrap()[coll]
                    .name
                    .clone();
                self.query.input.clear();
                self.query.cursor = 0;
                self.query.error = None;
                self.extras = SpecExtras::default();
                self.focus = Pane::Results;
                self.start_find(db_name, coll_name, FindSpec::default());
            }
        }
    }

    // ---------- results ----------

    /// Index of the document under the cursor/selection in either view.
    pub fn current_doc_idx(&self) -> Option<usize> {
        match self.view {
            ViewMode::Json => self
                .results
                .lines
                .get(self.results.cursor)
                .map(|l| l.doc_idx),
            ViewMode::Table => {
                (self.results.table.row < self.results.docs.len()).then_some(self.results.table.row)
            }
        }
    }

    fn on_key_results(&mut self, key: KeyEvent) {
        // View-independent actions first.
        match key.code {
            KeyCode::Char('v') => {
                self.view = match self.view {
                    ViewMode::Json => ViewMode::Table,
                    ViewMode::Table => ViewMode::Json,
                };
                return;
            }
            KeyCode::Char('F') => return self.open_query_editor(),
            KeyCode::Char('x') => return self.explain_current(),
            KeyCode::Char('o') => return self.open_doc_view(),
            KeyCode::Char('y') => return self.copy_current_doc(),
            KeyCode::Char('E') => return self.export_results(),
            _ => {}
        }
        match self.view {
            ViewMode::Json => self.on_key_results_json(key),
            ViewMode::Table => self.on_key_results_table(key),
        }
    }

    fn on_key_results_json(&mut self, key: KeyEvent) {
        let len = self.results.lines.len();
        let page = self.results_area.height.saturating_sub(2) as usize; // borders
        match key.code {
            KeyCode::Down | KeyCode::Char('j') => self.move_results_cursor(1),
            KeyCode::Up | KeyCode::Char('k') => self.move_results_cursor(-1),
            KeyCode::PageDown => self.move_results_cursor(page as isize),
            KeyCode::PageUp => self.move_results_cursor(-(page as isize)),
            KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.move_results_cursor((page / 2) as isize)
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.move_results_cursor(-((page / 2) as isize))
            }
            KeyCode::Char('g') | KeyCode::Home => {
                self.results.cursor = 0;
            }
            KeyCode::Char('G') | KeyCode::End => {
                self.results.cursor = len.saturating_sub(1);
                self.maybe_fetch_more();
            }
            KeyCode::Enter | KeyCode::Char(' ') => self.toggle_fold_at_cursor(),
            _ => {}
        }
    }

    fn on_key_results_table(&mut self, key: KeyEvent) {
        let rows = self.results.docs.len();
        let page = self.results_area.height.saturating_sub(3) as usize; // borders + header
        let t = &mut self.results.table;
        match key.code {
            KeyCode::Down | KeyCode::Char('j') => {
                t.row = (t.row + 1).min(rows.saturating_sub(1));
                self.maybe_fetch_more();
            }
            KeyCode::Up | KeyCode::Char('k') => t.row = t.row.saturating_sub(1),
            KeyCode::PageDown => {
                t.row = (t.row + page).min(rows.saturating_sub(1));
                self.maybe_fetch_more();
            }
            KeyCode::PageUp => t.row = t.row.saturating_sub(page),
            KeyCode::Char('g') | KeyCode::Home => t.row = 0,
            KeyCode::Char('G') | KeyCode::End => {
                t.row = rows.saturating_sub(1);
                self.maybe_fetch_more();
            }
            KeyCode::Left | KeyCode::Char('h') => {
                t.active_col = t.active_col.saturating_sub(1);
                t.col_offset = t.col_offset.min(t.active_col);
            }
            KeyCode::Right | KeyCode::Char('l') => {
                t.active_col = (t.active_col + 1).min(t.columns.len().saturating_sub(1));
            }
            KeyCode::Char('s') => self.toggle_sort_active_col(),
            KeyCode::Enter => self.open_doc_view(),
            _ => {}
        }
    }

    /// Server-side sort on the table's active column (FR-22).
    fn toggle_sort_active_col(&mut self) {
        let t = &self.results.table;
        let Some(col) = t.columns.get(t.active_col).cloned() else {
            return;
        };
        let current = self
            .results
            .active_spec
            .sort
            .as_ref()
            .and_then(|s| s.get_i32(&col).ok());
        self.extras.sort = match current {
            Some(1) => format!("{{ \"{col}\": -1 }}"),
            Some(-1) => String::new(),
            _ => format!("{{ \"{col}\": 1 }}"),
        };
        self.run_query();
    }

    fn open_doc_view(&mut self) {
        let Some(idx) = self.current_doc_idx() else {
            return;
        };
        let doc = self.results.docs[idx].clone();
        let number = self.results.evicted + idx as u64 + 1;
        let title = match &self.results.target {
            Some((db, coll)) => format!("{db}.{coll} — doc {number}"),
            None => format!("doc {number}"),
        };
        self.modal = Modal::DocView(DocView::new(title, doc, None));
    }

    fn explain_current(&mut self) {
        let Some((db, coll)) = self.results.target.clone() else {
            self.toast_err("select a collection first".into());
            return;
        };
        self.toast_info("explaining…".into());
        let spec = self.results.active_spec.clone();
        self.send(Command::Explain { db, coll, spec });
    }

    fn copy_current_doc(&mut self) {
        let Some(idx) = self.current_doc_idx() else {
            return;
        };
        let text = util::doc_to_pretty(&self.results.docs[idx]);
        match util::clipboard_copy(&text) {
            Ok(()) => self.toast_info("document copied to clipboard".into()),
            Err(e) => self.toast_err(e),
        }
    }

    /// Export the loaded window: JSON array in JSON view, CSV in table view.
    fn export_results(&mut self) {
        let Some((db, coll)) = self.results.target.clone() else {
            return;
        };
        if self.results.docs.is_empty() {
            self.toast_err("nothing to export".into());
            return;
        }
        let result = match self.view {
            ViewMode::Json => util::export_json(&db, &coll, &self.results.docs),
            ViewMode::Table => {
                util::export_csv(&db, &coll, &self.results.table.columns, &self.results.docs)
            }
        };
        match result {
            Ok(path) => self.toast_info(format!(
                "exported {} docs to {path}",
                self.results.docs.len()
            )),
            Err(e) => self.toast_err(format!("export failed: {e}")),
        }
    }

    fn move_results_cursor(&mut self, delta: isize) {
        let len = self.results.lines.len();
        if len == 0 {
            return;
        }
        let cur = self.results.cursor as isize + delta;
        self.results.cursor = cur.clamp(0, len as isize - 1) as usize;
        self.maybe_fetch_more();
    }

    /// Infinite scroll: request the next batch when the cursor nears the end.
    fn maybe_fetch_more(&mut self) {
        let r = &self.results;
        if r.loading || r.exhausted || r.target.is_none() {
            return;
        }
        let near_end = match self.view {
            ViewMode::Json => r.cursor + 30 >= r.lines.len(),
            ViewMode::Table => r.table.row + 15 >= r.docs.len(),
        };
        if near_end {
            self.results.loading = true;
            let generation = self.generation;
            self.send(Command::NextBatch { generation });
        }
    }

    fn toggle_fold_at_cursor(&mut self) {
        let Some(rline) = self.results.lines.get(self.results.cursor) else {
            return;
        };
        let Some(path) = rline.fold_path.clone() else {
            return;
        };
        let doc_idx = rline.doc_idx;
        let folds = &mut self.results.folds[doc_idx];
        if !folds.remove(&path) {
            folds.insert(path);
        }
        // Keep the cursor on the same document header after rebuild.
        let lines_before: usize = self
            .results
            .lines
            .iter()
            .take_while(|l| l.doc_idx < doc_idx)
            .count();
        let offset_in_doc = self.results.cursor - lines_before;
        self.results.rebuild_lines();
        let doc_start: usize = self
            .results
            .lines
            .iter()
            .take_while(|l| l.doc_idx < doc_idx)
            .count();
        let doc_len = self
            .results
            .lines
            .iter()
            .filter(|l| l.doc_idx == doc_idx)
            .count();
        self.results.cursor = doc_start + offset_in_doc.min(doc_len.saturating_sub(1));
    }

    // ---------- query bar ----------

    fn on_key_query(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                self.focus = Pane::Results;
                self.query.error = None;
                return;
            }
            KeyCode::Enter => {
                self.push_history();
                self.run_query();
                return;
            }
            KeyCode::Tab => {
                self.cycle_focus(false);
                return;
            }
            KeyCode::BackTab => {
                self.cycle_focus(true);
                return;
            }
            _ => {}
        }
        let q = &mut self.query;
        match key.code {
            KeyCode::Backspace => {
                if q.cursor > 0 {
                    let idx = char_to_byte(&q.input, q.cursor - 1);
                    q.input.remove(idx);
                    q.cursor -= 1;
                }
            }
            KeyCode::Delete => {
                if q.cursor < q.input.chars().count() {
                    let idx = char_to_byte(&q.input, q.cursor);
                    q.input.remove(idx);
                }
            }
            KeyCode::Left => q.cursor = q.cursor.saturating_sub(1),
            KeyCode::Right => q.cursor = (q.cursor + 1).min(q.input.chars().count()),
            KeyCode::Home => q.cursor = 0,
            KeyCode::End => q.cursor = q.input.chars().count(),
            KeyCode::Up => {
                if q.history.is_empty() {
                    return;
                }
                let pos = match q.hist_pos {
                    None => q.history.len() - 1,
                    Some(p) => p.saturating_sub(1),
                };
                q.hist_pos = Some(pos);
                q.input = q.history[pos].clone();
                q.cursor = q.input.chars().count();
            }
            KeyCode::Down => {
                let Some(p) = q.hist_pos else { return };
                if p + 1 < q.history.len() {
                    q.hist_pos = Some(p + 1);
                    q.input = q.history[p + 1].clone();
                } else {
                    q.hist_pos = None;
                    q.input.clear();
                }
                q.cursor = q.input.chars().count();
            }
            KeyCode::Char(c) => {
                let idx = char_to_byte(&q.input, q.cursor);
                q.input.insert(idx, c);
                q.cursor += 1;
                q.error = None;
            }
            _ => {}
        }
    }

    fn push_history(&mut self) {
        let text = self.query.input.trim().to_string();
        if !text.is_empty() && self.query.history.last() != Some(&text) {
            self.query.history.push(text);
        }
        self.query.hist_pos = None;
    }

    /// Run the current query bar filter + spec extras.
    fn run_query(&mut self) {
        let Some((db, coll)) = self.results.target.clone() else {
            self.toast_err("select a collection first".into());
            return;
        };
        match build_spec(
            &self.query.input,
            &self.extras.projection,
            &self.extras.sort,
            &self.extras.limit,
            &self.extras.skip,
        ) {
            Err(e) => self.query.error = Some(e),
            Ok(spec) => {
                self.query.error = None;
                self.focus = Pane::Results;
                self.start_find(db, coll, spec);
            }
        }
    }

    // ---------- mouse ----------

    fn on_mouse(&mut self, m: MouseEvent) {
        let pos = Position {
            x: m.column,
            y: m.row,
        };
        if self.modal.is_open() {
            match (&mut self.modal, m.kind) {
                (Modal::Help, MouseEventKind::Down(_)) => self.modal = Modal::None,
                (Modal::DocView(view), MouseEventKind::ScrollDown) => {
                    view.scroll = (view.scroll + 3).min(view.lines.len().saturating_sub(1));
                }
                (Modal::DocView(view), MouseEventKind::ScrollUp) => {
                    view.scroll = view.scroll.saturating_sub(3);
                }
                _ => {}
            }
            return;
        }
        match m.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if self.explorer_area.contains(pos) {
                    self.focus = Pane::Explorer;
                    let row = (m.row.saturating_sub(self.explorer_area.y + 1)) as usize
                        + self.explorer.scroll;
                    if row < self.explorer.rows().len() {
                        self.explorer.selected = row;
                        // Single click activates: expand/collapse db, open collection.
                        self.activate_explorer_row();
                    }
                } else if self.results_area.contains(pos) {
                    self.focus = Pane::Results;
                    match self.view {
                        ViewMode::Json => {
                            let line = (m.row.saturating_sub(self.results_area.y + 1)) as usize
                                + self.results.scroll;
                            if line < self.results.lines.len() {
                                if self.results.cursor == line {
                                    self.toggle_fold_at_cursor();
                                } else {
                                    self.results.cursor = line;
                                }
                            }
                        }
                        ViewMode::Table => self.on_table_click(m),
                    }
                } else if self.query_area.contains(pos) {
                    self.focus = Pane::Query;
                }
            }
            MouseEventKind::ScrollDown => self.scroll_under_mouse(pos, 3),
            MouseEventKind::ScrollUp => self.scroll_under_mouse(pos, -3),
            _ => {}
        }
    }

    fn on_table_click(&mut self, m: MouseEvent) {
        let header_y = self.results_area.y + 1;
        if m.row == header_y {
            // Click on a column header: select it and toggle sort.
            let hit = self
                .results
                .table
                .col_hit
                .iter()
                .find(|(x0, x1, _)| m.column >= *x0 && m.column < *x1)
                .map(|(_, _, i)| *i);
            if let Some(i) = hit {
                self.results.table.active_col = i;
                self.toggle_sort_active_col();
            }
            return;
        }
        let row = (m.row.saturating_sub(header_y + 1)) as usize + self.results.table.scroll_row;
        if row < self.results.docs.len() {
            if self.results.table.row == row {
                self.open_doc_view();
            } else {
                self.results.table.row = row;
            }
        }
    }

    fn scroll_under_mouse(&mut self, pos: Position, delta: isize) {
        if self.explorer_area.contains(pos) {
            let n = self.explorer.rows().len();
            let s = self.explorer.scroll as isize + delta;
            self.explorer.scroll = s.clamp(0, n.saturating_sub(1) as isize) as usize;
        } else if self.results_area.contains(pos) {
            match self.view {
                ViewMode::Json => {
                    let n = self.results.lines.len();
                    let s = self.results.scroll as isize + delta;
                    self.results.scroll = s.clamp(0, n.saturating_sub(1) as isize) as usize;
                    if self.results.scroll + (self.results_area.height as usize) + 10 >= n {
                        self.maybe_fetch_more();
                    }
                }
                ViewMode::Table => {
                    let n = self.results.docs.len();
                    let s = self.results.table.scroll_row as isize + delta;
                    self.results.table.scroll_row =
                        s.clamp(0, n.saturating_sub(1) as isize) as usize;
                    if self.results.table.scroll_row + (self.results_area.height as usize) + 10 >= n
                    {
                        self.maybe_fetch_more();
                    }
                }
            }
        }
    }
}

/// Build a FindSpec from user-entered strings.
fn build_spec(
    filter: &str,
    projection: &str,
    sort: &str,
    limit: &str,
    skip: &str,
) -> Result<FindSpec, String> {
    let parse_num = |s: &str, what: &str| -> Result<Option<i64>, String> {
        let s = s.trim();
        if s.is_empty() {
            return Ok(None);
        }
        s.parse::<i64>()
            .map(Some)
            .map_err(|_| format!("{what} must be an integer"))
    };
    Ok(FindSpec {
        filter: parse_filter(filter)?,
        projection: parse_optional_doc(projection).map_err(|e| format!("projection: {e}"))?,
        sort: parse_optional_doc(sort).map_err(|e| format!("sort: {e}"))?,
        limit: parse_num(limit, "limit")?,
        skip: parse_num(skip, "skip")?.map(|n| n.max(0) as u64),
    })
}

/// Strip credentials from a connection string for display.
pub fn redact_uri(uri: &str) -> String {
    if let Some(scheme_end) = uri.find("://") {
        let rest = &uri[scheme_end + 3..];
        if let Some(at) = rest.find('@') {
            return format!("{}***@{}", &uri[..scheme_end + 3], &rest[at + 1..]);
        }
    }
    uri.to_string()
}

/// Main event loop: draw, then wait for the next input/core/tick event.
pub async fn run(terminal: &mut term::Term, uri: String, read_only: bool) -> Result<()> {
    let (cmd_tx, mut core_rx) = actor::spawn(read_only);
    let mut input_rx = event::input_channel();
    let mut app = App::new(redact_uri(&uri), cmd_tx, read_only);
    app.send(Command::Connect { uri });

    let mut tick = tokio::time::interval(Duration::from_millis(250));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    while !app.should_quit {
        if app.results.dirty {
            app.results.rebuild_lines();
        }
        terminal.draw(|f| ui::draw(f, &mut app))?;
        tokio::select! {
            Some(ev) = input_rx.recv() => {
                app.on_input(ev);
                // Drain any queued input so a burst renders in one frame.
                while let Ok(ev) = input_rx.try_recv() {
                    app.on_input(ev);
                }
            }
            Some(ev) = core_rx.recv() => app.on_core(ev),
            _ = tick.tick() => app.on_tick(),
        }
    }
    Ok(())
}
