//! Application model and update loop (Elm-style): one `App` struct, updated
//! by input events and core events, rendered by `ui::draw`.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use anyhow::Result;
use lazymongo_core::actor;
use lazymongo_core::bson::{Bson, Document};
use lazymongo_core::query::{parse_doc, parse_filter, parse_optional_doc};
use lazymongo_core::types::{
    pipeline_writes, CollectionInfo, Command, CoreEvent, DatabaseInfo, FindSpec, BATCH_SIZE,
};
use ratatui::crossterm::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::layout::{Position, Rect};
use tokio::sync::{mpsc, watch};

use crate::agg::{AggFocus, AggState, AGG_PREVIEW_LIMIT, DEFAULT_PIPELINE};
use crate::input::{char_to_byte, Input};
use crate::json_view::{doc_lines, RLine};
use crate::modal::{
    AppAction, Confirm, ConnForm, DocView, EditorPurpose, IndexesView, JsonEditor, Modal, Palette,
    PendingAction, Prompt, PromptAction, QueryEditor,
};
use crate::textarea::TextArea;
use crate::theme;
use crate::{config, event, term, ui, util};

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
    /// No connection yet (saved-connection picker is open).
    Idle,
    Connecting,
    Connected {
        version: String,
        ping_ms: u64,
    },
    Failed(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    Main,
    Agg,
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
            // A db whose own name matches shows all of its collections.
            let matching_colls: Vec<usize> = node
                .colls
                .iter()
                .flatten()
                .enumerate()
                .filter(|(_, c)| db_match || c.name.to_lowercase().contains(&needle))
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
    /// In-results search (FR-26).
    pub search: String,
    pub searching: bool,
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

/// What an in-flight countDocuments (dry run) is for.
enum PendingCount {
    DeleteMany { filter: Document },
    UpdateMany { filter: Document, update: Document },
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
    pub screen: Screen,
    /// Aggregation screen state; kept across visits within a session.
    pub agg: Option<AggState>,
    /// Persisted per-collection history and pipelines (FR-13/FR-19).
    pub state: config::State,
    /// Session log of completed write operations (FR-32).
    pub ops_log: Vec<String>,
    /// The --readonly CLI flag (a saved connection can only add to it).
    cli_read_only: bool,
    pending_counts: HashMap<u64, PendingCount>,
    next_req_id: u64,
    cancel_tx: watch::Sender<u64>,
    /// Theme name persisted in config.toml (None = default).
    pub config_theme: Option<String>,
    /// The real (unredacted) URI of the active connection, for `m` (mongosh).
    active_uri: Option<String>,
    /// Set by the `m` key; the run loop suspends the TUI and opens mongosh.
    pub pending_shell: bool,
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
    pub fn new(
        uri_display: String,
        cmd_tx: mpsc::Sender<Command>,
        cancel_tx: watch::Sender<u64>,
        read_only: bool,
    ) -> Self {
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
            screen: Screen::Main,
            agg: None,
            state: config::load_state(),
            ops_log: Vec::new(),
            cli_read_only: read_only,
            pending_counts: HashMap::new(),
            next_req_id: 0,
            cancel_tx,
            config_theme: None,
            active_uri: None,
            pending_shell: false,
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

    /// Abort the in-flight find/aggregation, if any (FR-16).
    fn cancel_current(&mut self) {
        let _ = self.cancel_tx.send(self.generation);
        self.toast_info("cancelling…".into());
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
            CoreEvent::WriteDone {
                namespace,
                summary,
                refresh,
            } => {
                self.ops_log
                    .push(format!("{}  {namespace}: {summary}", util::clock_utc()));
                self.toast_info(summary);
                let current_ns = self
                    .results
                    .target
                    .as_ref()
                    .map(|(d, c)| format!("{d}.{c}"));
                if refresh && current_ns.as_deref() == Some(namespace.as_str()) {
                    self.rerun_find();
                }
            }
            CoreEvent::Cancelled { generation } => {
                if generation == self.generation {
                    self.results.loading = false;
                    if let Some(agg) = &mut self.agg {
                        agg.running = false;
                    }
                    self.toast_info("query cancelled".into());
                }
            }
            CoreEvent::CountResult { req_id, n } => self.on_count_result(req_id, n),
            CoreEvent::Indexes { db, coll, indexes } => {
                if let Modal::Indexes(view) = &mut self.modal {
                    if view.db == db && view.coll == coll {
                        view.selected = view.selected.min(indexes.len().saturating_sub(1));
                        view.indexes = Some(indexes);
                    }
                }
            }
            CoreEvent::AggBatch { generation, docs } => {
                if generation != self.generation {
                    return;
                }
                if let Some(agg) = &mut self.agg {
                    let ran_through = agg.selected_stage;
                    agg.set_docs(docs, ran_through);
                }
            }
        }
    }

    /// A dry-run count came back: open the corresponding confirmation.
    fn on_count_result(&mut self, req_id: u64, n: u64) {
        let Some(pending) = self.pending_counts.remove(&req_id) else {
            return;
        };
        if n == 0 {
            self.toast_info("filter matches 0 documents — nothing to do".into());
            return;
        }
        match pending {
            PendingCount::DeleteMany { filter } => {
                self.modal = Modal::Confirm(Confirm {
                    title: "Delete many".into(),
                    body: vec![
                        format!("filter: {}", filter_display(&filter)),
                        format!("{n} document(s) will be PERMANENTLY deleted."),
                        String::new(),
                        format!("Type the count ({n}) to confirm:"),
                    ],
                    typed_required: Some(n.to_string()),
                    typed: Input::default(),
                    action: PendingAction::DeleteMany { filter },
                });
            }
            PendingCount::UpdateMany { filter, update } => {
                self.modal = Modal::Confirm(Confirm {
                    title: "Update many".into(),
                    body: vec![
                        format!("filter: {}", filter_display(&filter)),
                        format!("update: {}", filter_display(&update)),
                        String::new(),
                        format!("{n} document(s) will be modified."),
                    ],
                    typed_required: None,
                    typed: Input::default(),
                    action: PendingAction::UpdateMany { filter, update },
                });
            }
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
        if key.modifiers.contains(KeyModifiers::CONTROL)
            && key.code == KeyCode::Char('p')
            && self.screen == Screen::Main
            && !self.modal.is_open()
        {
            return self.open_palette();
        }
        if self.screen == Screen::Agg {
            return self.on_key_agg(key);
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
        if self.focus == Pane::Results && self.results.searching {
            return self.on_key_results_search(key);
        }

        if key.code == KeyCode::Char(':') {
            return self.open_palette();
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
            KeyCode::Char('C') => self.open_connections(),
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
            Modal::Confirm(confirm) => match key.code {
                KeyCode::Esc | KeyCode::Char('n') if confirm.typed_required.is_none() => {
                    self.modal = Modal::None;
                    self.toast_info("cancelled".into());
                }
                KeyCode::Esc => {
                    self.modal = Modal::None;
                    self.toast_info("cancelled".into());
                }
                KeyCode::Enter => self.confirm_execute(),
                KeyCode::Char('y') if confirm.typed_required.is_none() => self.confirm_execute(),
                _ => {
                    if confirm.typed_required.is_some() {
                        confirm.typed.on_key(key);
                    }
                }
            },
            Modal::Editor(editor) => match key.code {
                KeyCode::Esc => self.modal = Modal::None,
                KeyCode::Char('s') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    self.submit_json_editor();
                }
                _ => {
                    if editor.area.on_key(key) {
                        editor.error = None;
                    }
                }
            },
            Modal::Indexes(view) => {
                let count = view.indexes.as_ref().map(Vec::len).unwrap_or(0);
                match key.code {
                    KeyCode::Esc | KeyCode::Char('q') => self.modal = Modal::None,
                    KeyCode::Down | KeyCode::Char('j') => {
                        view.selected = (view.selected + 1).min(count.saturating_sub(1))
                    }
                    KeyCode::Up | KeyCode::Char('k') => {
                        view.selected = view.selected.saturating_sub(1)
                    }
                    KeyCode::Char('r') => {
                        let (db, coll) = (view.db.clone(), view.coll.clone());
                        view.indexes = None;
                        self.send(Command::ListIndexes { db, coll });
                    }
                    KeyCode::Char('c') => {
                        if self.guard_write() {
                            self.modal = Modal::Editor(JsonEditor {
                                title: "Create index — key spec".into(),
                                area: TextArea::from_text("{\n  \n}"),
                                purpose: EditorPurpose::CreateIndexKeys,
                                error: None,
                            });
                        }
                    }
                    KeyCode::Char('d') | KeyCode::Char('D') => {
                        let name = view
                            .indexes
                            .as_ref()
                            .and_then(|list| list.get(view.selected))
                            .map(|i| i.name.clone());
                        let Some(name) = name else { return };
                        if name == "_id_" {
                            self.toast_err("the _id index cannot be dropped".into());
                            return;
                        }
                        if self.guard_write() {
                            self.modal = Modal::Confirm(Confirm {
                                title: "Drop index".into(),
                                body: vec![format!("Drop index \"{name}\"?")],
                                typed_required: None,
                                typed: Input::default(),
                                action: PendingAction::DropIndex { name },
                            });
                        }
                    }
                    _ => {}
                }
            }
            Modal::OpsLog { scroll } => match key.code {
                KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('L') => self.modal = Modal::None,
                KeyCode::Down | KeyCode::Char('j') => *scroll += 1,
                KeyCode::Up | KeyCode::Char('k') => *scroll = scroll.saturating_sub(1),
                KeyCode::Char('g') | KeyCode::Home => *scroll = 0,
                _ => {}
            },
            Modal::Prompt(prompt) => match key.code {
                KeyCode::Esc => self.modal = Modal::None,
                KeyCode::Enter => self.submit_prompt(),
                _ => {
                    prompt.input.on_key(key);
                }
            },
            Modal::Palette(palette) => match key.code {
                KeyCode::Esc => self.modal = Modal::None,
                KeyCode::Down | KeyCode::Tab => {
                    palette.selected =
                        (palette.selected + 1).min(palette.filtered.len().saturating_sub(1))
                }
                KeyCode::Up | KeyCode::BackTab => {
                    palette.selected = palette.selected.saturating_sub(1)
                }
                KeyCode::Enter => {
                    if let Some(action) = palette.selected_action() {
                        self.modal = Modal::None;
                        self.run_action(action);
                    }
                }
                _ => {
                    if palette.input.on_key(key) {
                        palette.refilter();
                    }
                }
            },
            Modal::Connections { items, selected } => match key.code {
                // With no connection behind the picker, Esc/q quit the app;
                // otherwise they just close it.
                KeyCode::Esc | KeyCode::Char('q') => {
                    if matches!(self.conn, ConnState::Idle) {
                        self.should_quit = true;
                    } else {
                        self.modal = Modal::None;
                    }
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    *selected = (*selected + 1).min(items.len().saturating_sub(1))
                }
                KeyCode::Up | KeyCode::Char('k') => *selected = selected.saturating_sub(1),
                KeyCode::Enter => self.connect_selected(),
                KeyCode::Char(c @ '1'..='9') => {
                    let idx = (c as usize) - ('1' as usize);
                    if idx < items.len() {
                        *selected = idx;
                        self.connect_selected();
                    }
                }
                KeyCode::Char('a') => {
                    self.modal = Modal::ConnForm(ConnForm::new(items.clone(), None));
                }
                KeyCode::Char('e') => {
                    if !items.is_empty() {
                        self.modal = Modal::ConnForm(ConnForm::new(items.clone(), Some(*selected)));
                    }
                }
                KeyCode::Char('d') => {
                    if items.is_empty() {
                        return;
                    }
                    let name = items[*selected].name.clone();
                    self.modal = Modal::Confirm(Confirm {
                        title: "Delete connection".into(),
                        body: vec![format!("Remove \"{name}\" from config.toml?")],
                        typed_required: None,
                        typed: Input::default(),
                        action: PendingAction::DeleteConnection {
                            items: items.clone(),
                            index: *selected,
                        },
                    });
                }
                _ => {}
            },
            Modal::ConnForm(form) => match key.code {
                KeyCode::Esc => {
                    self.modal = Modal::Connections {
                        items: form.items.clone(),
                        selected: 0,
                    };
                }
                KeyCode::Tab | KeyCode::Down => form.focus = (form.focus + 1) % 4,
                KeyCode::BackTab | KeyCode::Up => form.focus = (form.focus + 3) % 4,
                KeyCode::Enter => self.submit_conn_form(),
                KeyCode::Char(' ') | KeyCode::Left | KeyCode::Right if form.focus == 3 => {
                    form.read_only = !form.read_only;
                }
                _ => {
                    if form.focus < 3 && form.fields[form.focus].on_key(key) {
                        form.error = None;
                    }
                }
            },
        }
    }

    /// Validate the connection form, write config.toml, reopen the picker.
    fn submit_conn_form(&mut self) {
        let Modal::ConnForm(form) = &mut self.modal else {
            return;
        };
        let conn = match form.build() {
            Ok(c) => c,
            Err(e) => {
                form.error = Some(e);
                return;
            }
        };
        let mut items = form.items.clone();
        let selected = match form.editing {
            Some(i) => {
                items[i] = conn;
                i
            }
            None => {
                items.push(conn);
                items.len() - 1
            }
        };
        let cfg = config::Config {
            theme: self.config_theme.clone(),
            connections: items.clone(),
        };
        match config::save_config(&cfg) {
            Ok(()) => {
                self.toast_info("connections saved".into());
                self.modal = Modal::Connections { items, selected };
            }
            Err(e) => form.error = Some(format!("could not save: {e}")),
        }
    }

    /// Open the connection manager (also reachable any time via C).
    fn open_connections(&mut self) {
        match config::load_config() {
            Err(e) => self.toast_err(e),
            Ok(cfg) => {
                self.modal = Modal::Connections {
                    items: cfg.connections,
                    selected: 0,
                };
            }
        }
    }

    /// Connect to the highlighted saved connection (FR-2/FR-4).
    fn connect_selected(&mut self) {
        let Modal::Connections { items, selected } = &self.modal else {
            return;
        };
        if items.is_empty() {
            return;
        }
        let conn = items[*selected].clone();
        match conn.resolve_uri() {
            Err(e) => self.toast_err(e),
            Ok(uri) => {
                // Reset all per-connection state (also handles re-connects).
                self.explorer = Explorer::default();
                self.results = Results::default();
                self.query = QueryBar::default();
                self.extras = SpecExtras::default();
                self.agg = None;
                self.screen = Screen::Main;
                self.focus = Pane::Explorer;
                self.generation += 1; // invalidate in-flight batches

                let effective_ro = self.cli_read_only || conn.read_only;
                self.read_only = effective_ro;
                self.uri_display = format!("{} ({})", conn.name, redact_uri(&uri));
                self.conn = ConnState::Connecting;
                self.modal = Modal::None;
                self.active_uri = Some(uri.clone());
                self.send(Command::SetReadOnly(effective_ro));
                self.send(Command::Connect { uri });
            }
        }
    }

    /// Execute the confirmed pending action.
    fn confirm_execute(&mut self) {
        let Modal::Confirm(confirm) = &self.modal else {
            return;
        };
        if !confirm.typed_ok() {
            self.toast_err("confirmation text does not match".into());
            return;
        }
        let Modal::Confirm(confirm) = std::mem::replace(&mut self.modal, Modal::None) else {
            return;
        };
        let target = self.results.target.clone();
        match confirm.action {
            PendingAction::ApplyEdit { id, doc } => {
                if let Some((db, coll)) = target {
                    self.send(Command::ReplaceOne { db, coll, id, doc });
                }
            }
            PendingAction::DeleteOne { id } => {
                if let Some((db, coll)) = target {
                    self.send(Command::DeleteOne { db, coll, id });
                }
            }
            PendingAction::DeleteMany { filter } => {
                if let Some((db, coll)) = target {
                    self.send(Command::DeleteMany { db, coll, filter });
                }
            }
            PendingAction::UpdateMany { filter, update } => {
                if let Some((db, coll)) = target {
                    self.send(Command::UpdateMany {
                        db,
                        coll,
                        filter,
                        update,
                    });
                }
            }
            PendingAction::CreateIndex { keys } => {
                if let Some((db, coll)) = target {
                    self.modal = Modal::Indexes(IndexesView {
                        db: db.clone(),
                        coll: coll.clone(),
                        indexes: None,
                        selected: 0,
                    });
                    self.send(Command::CreateIndex { db, coll, keys });
                }
            }
            PendingAction::DropIndex { name } => {
                if let Some((db, coll)) = target {
                    self.modal = Modal::Indexes(IndexesView {
                        db: db.clone(),
                        coll: coll.clone(),
                        indexes: None,
                        selected: 0,
                    });
                    self.send(Command::DropIndex { db, coll, name });
                }
            }
            PendingAction::DropCollection { db, coll } => {
                // If the dropped collection is open, clear the results pane.
                if self.results.target.as_ref() == Some(&(db.clone(), coll.clone())) {
                    self.results = Results::default();
                }
                self.send(Command::DropCollection { db, coll });
            }
            PendingAction::DeleteConnection { mut items, index } => {
                let name = items.remove(index).name;
                let cfg = config::Config {
                    theme: self.config_theme.clone(),
                    connections: items.clone(),
                };
                match config::save_config(&cfg) {
                    Ok(()) => self.toast_info(format!("removed connection \"{name}\"")),
                    Err(e) => self.toast_err(format!("could not save: {e}")),
                }
                let selected = index.min(items.len().saturating_sub(1));
                self.modal = Modal::Connections { items, selected };
            }
        }
    }

    /// Parse and route the JSON editor's content on Ctrl-S.
    fn submit_json_editor(&mut self) {
        let Modal::Editor(editor) = &mut self.modal else {
            return;
        };
        let text = editor.area.text();
        let parsed = match parse_doc(&text) {
            Ok(d) => d,
            Err(e) => {
                editor.error = Some(e);
                return;
            }
        };
        let Modal::Editor(editor) = std::mem::replace(&mut self.modal, Modal::None) else {
            return;
        };
        match editor.purpose {
            EditorPurpose::EditDoc { id, original } => {
                if let Some(new_id) = parsed.get("_id") {
                    if *new_id != id {
                        self.modal = Modal::Editor(JsonEditor {
                            error: Some(
                                "_id cannot be changed; revert it or remove the field".into(),
                            ),
                            ..editor_with(
                                editor.title,
                                text,
                                EditorPurpose::EditDoc { id, original },
                            )
                        });
                        return;
                    }
                }
                let mut doc = parsed;
                doc.remove("_id"); // replaceOne rejects _id in the replacement
                let summary = diff_summary(&original, &doc);
                self.modal = Modal::Confirm(Confirm {
                    title: "Apply edit".into(),
                    body: vec![
                        format!("_id: {}", bson_display(&id)),
                        format!("changes: {summary}"),
                    ],
                    typed_required: None,
                    typed: Input::default(),
                    action: PendingAction::ApplyEdit { id, doc },
                });
            }
            EditorPurpose::InsertDoc => {
                let Some((db, coll)) = self.results.target.clone() else {
                    return;
                };
                self.send(Command::InsertOne {
                    db,
                    coll,
                    doc: parsed,
                });
            }
            EditorPurpose::UpdateMany { filter } => {
                if !parsed.keys().any(|k| k.starts_with('$')) {
                    self.modal = Modal::Editor(JsonEditor {
                        error: Some("update must use operators like $set / $unset / $inc".into()),
                        ..editor_with(editor.title, text, EditorPurpose::UpdateMany { filter })
                    });
                    return;
                }
                self.request_count(PendingCount::UpdateMany {
                    filter,
                    update: parsed,
                });
            }
            EditorPurpose::CreateIndexKeys => {
                if parsed.is_empty() {
                    self.modal = Modal::Editor(JsonEditor {
                        error: Some("index key spec cannot be empty".into()),
                        ..editor_with(editor.title, text, EditorPurpose::CreateIndexKeys)
                    });
                    return;
                }
                self.modal = Modal::Confirm(Confirm {
                    title: "Create index".into(),
                    body: vec![format!("keys: {}", filter_display(&parsed))],
                    typed_required: None,
                    typed: Input::default(),
                    action: PendingAction::CreateIndex { keys: parsed },
                });
            }
        }
    }

    fn submit_prompt(&mut self) {
        let Modal::Prompt(prompt) = std::mem::replace(&mut self.modal, Modal::None) else {
            return;
        };
        let value = prompt.input.text.trim().to_string();
        if value.is_empty() {
            self.toast_err("name cannot be empty".into());
            return;
        }
        match prompt.action {
            PromptAction::CreateCollection { db } => {
                self.send(Command::CreateCollection { db, name: value });
            }
        }
    }

    /// Kick off a dry-run count; the confirm modal opens when it returns.
    fn request_count(&mut self, pending: PendingCount) {
        let Some((db, coll)) = self.results.target.clone() else {
            return;
        };
        let filter = match &pending {
            PendingCount::DeleteMany { filter } => filter.clone(),
            PendingCount::UpdateMany { filter, .. } => filter.clone(),
        };
        self.next_req_id += 1;
        let req_id = self.next_req_id;
        self.pending_counts.insert(req_id, pending);
        self.toast_info("counting matching documents…".into());
        self.send(Command::Count {
            req_id,
            db,
            coll,
            filter,
        });
    }

    /// False (with a toast) when writes are blocked (FR-4).
    fn guard_write(&mut self) -> bool {
        if self.read_only {
            self.toast_err("read-only mode: write operations are disabled".into());
            return false;
        }
        true
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

    /// Open the command palette (FR-36): every feature, fuzzy-searchable.
    fn open_palette(&mut self) {
        let mut actions: Vec<(String, AppAction)> = vec![
            (
                "view: toggle json / table (v)".into(),
                AppAction::ToggleView,
            ),
            (
                "query: structured editor — filter/projection/sort/limit (F)".into(),
                AppAction::QueryEditor,
            ),
            ("query: explain plan (x)".into(), AppAction::Explain),
            ("doc: open full-screen (o)".into(), AppAction::DocView),
            ("doc: copy to clipboard (y)".into(), AppAction::CopyDoc),
            (
                "export loaded docs as json/csv (E)".into(),
                AppAction::Export,
            ),
            ("write: edit document (e)".into(), AppAction::EditDoc),
            ("write: insert document (i)".into(), AppAction::InsertDoc),
            ("write: delete document (d)".into(), AppAction::DeleteDoc),
            (
                "write: delete many by filter (D)".into(),
                AppAction::DeleteMany,
            ),
            (
                "write: update many by filter (U)".into(),
                AppAction::UpdateMany,
            ),
            (
                "indexes: list / create / drop (I)".into(),
                AppAction::Indexes,
            ),
            ("operations log (L)".into(), AppAction::OpsLog),
            ("aggregation editor (a)".into(), AppAction::Aggregate),
            ("shell: open mongosh here (m)".into(), AppAction::OpenShell),
            (
                "connections: manage / switch (C)".into(),
                AppAction::Connections,
            ),
            ("refresh (r)".into(), AppAction::Refresh),
            ("help / keybindings (?)".into(), AppAction::Help),
            ("quit (q)".into(), AppAction::Quit),
        ];
        for name in theme::NAMES {
            actions.push((format!("theme: {name}"), AppAction::SetTheme(name)));
        }
        self.modal = Modal::Palette(Palette::new(actions));
    }

    fn run_action(&mut self, action: AppAction) {
        match action {
            AppAction::ToggleView => {
                self.view = match self.view {
                    ViewMode::Json => ViewMode::Table,
                    ViewMode::Table => ViewMode::Json,
                }
            }
            AppAction::QueryEditor => self.open_query_editor(),
            AppAction::Explain => self.explain_current(),
            AppAction::DocView => self.open_doc_view(),
            AppAction::CopyDoc => self.copy_current_doc(),
            AppAction::Export => self.export_results(),
            AppAction::EditDoc => self.edit_current_doc(),
            AppAction::InsertDoc => self.insert_doc_flow(),
            AppAction::DeleteDoc => self.delete_current_doc(),
            AppAction::DeleteMany => self.delete_many_flow(),
            AppAction::UpdateMany => self.update_many_flow(),
            AppAction::Indexes => self.open_indexes(),
            AppAction::OpsLog => self.modal = Modal::OpsLog { scroll: 0 },
            AppAction::Aggregate => self.open_agg(),
            AppAction::OpenShell => self.open_shell(),
            AppAction::Connections => self.open_connections(),
            AppAction::Refresh => self.refresh(),
            AppAction::Help => self.modal = Modal::Help,
            AppAction::Quit => self.should_quit = true,
            AppAction::SetTheme(name) => {
                if theme::set_by_name(name) {
                    self.config_theme = Some(name.to_string());
                    let mut cfg = config::load_config().unwrap_or_default();
                    cfg.theme = Some(name.to_string());
                    if let Err(e) = config::save_config(&cfg) {
                        self.toast_err(format!("theme set, but not saved: {e}"));
                    } else {
                        self.toast_info(format!("theme: {name} (saved)"));
                    }
                }
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
        // Load persisted history when switching collections (FR-13).
        let ns = format!("{db}.{coll}");
        if self
            .results
            .target
            .as_ref()
            .map(|(d, c)| format!("{d}.{c}"))
            != Some(ns.clone())
        {
            self.query.history = self.state.history.get(&ns).cloned().unwrap_or_default();
            self.query.hist_pos = None;
        }
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
            KeyCode::Char('N') => self.new_collection_prompt(),
            KeyCode::Char('X') => self.drop_collection_confirm(),
            KeyCode::Esc => {
                self.explorer.filter.clear();
            }
            _ => {}
        }
        let n = self.explorer.rows().len();
        self.explorer.selected = self.explorer.selected.min(n.saturating_sub(1));
    }

    /// Create a collection in the db under the cursor (FR-31).
    fn new_collection_prompt(&mut self) {
        if !self.guard_write() {
            return;
        }
        let rows = self.explorer.rows();
        let Some(row) = rows.get(self.explorer.selected).copied() else {
            return;
        };
        let db = match row {
            ExplorerRow::Db(di) | ExplorerRow::Coll { db: di, .. } => {
                self.explorer.dbs[di].info.name.clone()
            }
        };
        self.modal = Modal::Prompt(Prompt {
            title: format!("New collection in {db}"),
            input: Input::default(),
            action: PromptAction::CreateCollection { db },
        });
    }

    /// Drop the collection under the cursor; requires typing its name (FR-31).
    fn drop_collection_confirm(&mut self) {
        if !self.guard_write() {
            return;
        }
        let rows = self.explorer.rows();
        let Some(ExplorerRow::Coll { db, coll }) = rows.get(self.explorer.selected).copied() else {
            self.toast_err("select a collection to drop".into());
            return;
        };
        let db_name = self.explorer.dbs[db].info.name.clone();
        let coll_name = self.explorer.dbs[db].colls.as_ref().unwrap()[coll]
            .name
            .clone();
        self.modal = Modal::Confirm(Confirm {
            title: "Drop collection".into(),
            body: vec![
                format!("{db_name}.{coll_name} and ALL its documents will be dropped."),
                String::new(),
                format!("Type the collection name ({coll_name}) to confirm:"),
            ],
            typed_required: Some(coll_name.clone()),
            typed: Input::default(),
            action: PendingAction::DropCollection {
                db: db_name,
                coll: coll_name,
            },
        });
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
            KeyCode::Char('e') => return self.edit_current_doc(),
            KeyCode::Char('i') => return self.insert_doc_flow(),
            KeyCode::Char('d') if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                return self.delete_current_doc()
            }
            KeyCode::Char('D') => return self.delete_many_flow(),
            KeyCode::Char('U') => return self.update_many_flow(),
            KeyCode::Char('I') => return self.open_indexes(),
            KeyCode::Char('L') => {
                self.modal = Modal::OpsLog { scroll: 0 };
                return;
            }
            KeyCode::Char('a') => return self.open_agg(),
            KeyCode::Char('m') => return self.open_shell(),
            KeyCode::Char('/') => {
                self.results.searching = true;
                self.results.search.clear();
                return;
            }
            KeyCode::Char('n') if !self.results.search.is_empty() => {
                return self.jump_to_match(true, false)
            }
            KeyCode::Char('N') if !self.results.search.is_empty() => {
                return self.jump_to_match(false, false)
            }
            KeyCode::Esc if self.results.loading => return self.cancel_current(),
            _ => {}
        }
        match self.view {
            ViewMode::Json => self.on_key_results_json(key),
            ViewMode::Table => self.on_key_results_table(key),
        }
    }

    /// Suspend the TUI and open mongosh on the active connection (run loop
    /// performs the actual suspend/spawn/resume).
    fn open_shell(&mut self) {
        if self.read_only {
            self.toast_err("read-only mode: mongosh would bypass write blocking".into());
            return;
        }
        if self.active_uri.is_none() {
            self.toast_err("no active connection".into());
            return;
        }
        self.pending_shell = true;
    }

    pub fn take_shell_uri(&mut self) -> Option<String> {
        self.pending_shell = false;
        self.active_uri.clone()
    }

    // ---------- write flows (M3) ----------

    fn edit_current_doc(&mut self) {
        if !self.guard_write() {
            return;
        }
        let Some(idx) = self.current_doc_idx() else {
            return;
        };
        let doc = self.results.docs[idx].clone();
        let Some(id) = doc.get("_id").cloned() else {
            self.toast_err("document has no _id; cannot edit safely".into());
            return;
        };
        self.modal = Modal::Editor(JsonEditor {
            title: format!("Edit document _id={}", bson_display(&id)),
            area: TextArea::from_text(&util::doc_to_pretty(&doc)),
            purpose: EditorPurpose::EditDoc { id, original: doc },
            error: None,
        });
    }

    fn insert_doc_flow(&mut self) {
        if !self.guard_write() {
            return;
        }
        if self.results.target.is_none() {
            self.toast_err("select a collection first".into());
            return;
        }
        self.modal = Modal::Editor(JsonEditor {
            title: "Insert document".into(),
            area: TextArea::from_text("{\n  \n}"),
            purpose: EditorPurpose::InsertDoc,
            error: None,
        });
    }

    fn delete_current_doc(&mut self) {
        if !self.guard_write() {
            return;
        }
        let Some(idx) = self.current_doc_idx() else {
            return;
        };
        let doc = &self.results.docs[idx];
        let Some(id) = doc.get("_id").cloned() else {
            self.toast_err("document has no _id; cannot delete safely".into());
            return;
        };
        let mut preview: Vec<String> = doc
            .iter()
            .take(4)
            .map(|(k, v)| format!("  {k}: {}", util::bson_to_compact(v)))
            .collect();
        if doc.len() > 4 {
            preview.push("  …".into());
        }
        let mut body = vec![format!("_id: {}", bson_display(&id)), String::new()];
        body.extend(preview);
        body.push(String::new());
        body.push("Delete this document permanently?".into());
        self.modal = Modal::Confirm(Confirm {
            title: "Delete document".into(),
            body,
            typed_required: None,
            typed: Input::default(),
            action: PendingAction::DeleteOne { id },
        });
    }

    /// Delete everything matching the current filter (FR-29).
    fn delete_many_flow(&mut self) {
        if !self.guard_write() {
            return;
        }
        if self.results.target.is_none() {
            self.toast_err("select a collection first".into());
            return;
        }
        let filter = self.results.active_spec.filter.clone();
        self.request_count(PendingCount::DeleteMany { filter });
    }

    /// Update everything matching the current filter (FR-30).
    fn update_many_flow(&mut self) {
        if !self.guard_write() {
            return;
        }
        if self.results.target.is_none() {
            self.toast_err("select a collection first".into());
            return;
        }
        let filter = self.results.active_spec.filter.clone();
        self.modal = Modal::Editor(JsonEditor {
            title: format!("Update many — filter: {}", filter_display(&filter)),
            area: TextArea::from_text("{\n  $set: {\n    \n  }\n}"),
            purpose: EditorPurpose::UpdateMany { filter },
            error: None,
        });
    }

    // ---------- aggregation screen (M4) ----------

    fn open_agg(&mut self) {
        let Some((db, coll)) = self.results.target.clone() else {
            self.toast_err("select a collection first".into());
            return;
        };
        let ns = format!("{db}.{coll}");
        let reuse = self
            .agg
            .as_ref()
            .is_some_and(|a| a.db == db && a.coll == coll);
        if !reuse {
            let initial = self
                .state
                .pipelines
                .get(&ns)
                .cloned()
                .unwrap_or_else(|| DEFAULT_PIPELINE.to_string());
            self.agg = Some(AggState::new(db, coll, initial));
        }
        self.screen = Screen::Agg;
    }

    fn on_key_agg(&mut self, key: KeyEvent) {
        // Ctrl-R runs the full pipeline from any focus.
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('r') {
            self.run_agg(None);
            return;
        }
        if key.code == KeyCode::Esc && self.agg.as_ref().is_some_and(|a| a.running) {
            return self.cancel_current();
        }
        let Some(agg) = &mut self.agg else {
            self.screen = Screen::Main;
            return;
        };
        match agg.focus {
            AggFocus::Editor => match key.code {
                KeyCode::Esc => {
                    agg.focus = AggFocus::Stages;
                    agg.parse();
                }
                _ => {
                    if agg.editor.on_key(key) {
                        agg.error = None;
                    }
                }
            },
            AggFocus::Stages => match key.code {
                KeyCode::Esc | KeyCode::Char('q') => self.screen = Screen::Main,
                KeyCode::Tab => agg.focus = AggFocus::Results,
                KeyCode::Char('e') | KeyCode::Char('i') => agg.focus = AggFocus::Editor,
                KeyCode::Down | KeyCode::Char('j') => {
                    agg.selected_stage =
                        (agg.selected_stage + 1).min(agg.stages.len().saturating_sub(1));
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    agg.selected_stage = agg.selected_stage.saturating_sub(1);
                }
                KeyCode::Enter => {
                    let upto = agg.selected_stage;
                    self.run_agg(Some(upto));
                }
                _ => {}
            },
            AggFocus::Results => match key.code {
                KeyCode::Esc | KeyCode::Char('q') | KeyCode::BackTab => {
                    agg.focus = AggFocus::Stages
                }
                KeyCode::Tab => agg.focus = AggFocus::Editor,
                KeyCode::Down | KeyCode::Char('j') => {
                    agg.cursor = (agg.cursor + 1).min(agg.lines.len().saturating_sub(1));
                }
                KeyCode::Up | KeyCode::Char('k') => agg.cursor = agg.cursor.saturating_sub(1),
                KeyCode::Char('g') | KeyCode::Home => agg.cursor = 0,
                KeyCode::Char('G') | KeyCode::End => agg.cursor = agg.lines.len().saturating_sub(1),
                KeyCode::Enter | KeyCode::Char(' ') => agg.toggle_fold_at_cursor(),
                KeyCode::Char('y') => {
                    let doc = agg
                        .lines
                        .get(agg.cursor)
                        .map(|l| l.doc_idx)
                        .and_then(|i| agg.docs.get(i))
                        .cloned();
                    if let Some(doc) = doc {
                        match util::clipboard_copy(&util::doc_to_pretty(&doc)) {
                            Ok(()) => self.toast_info("document copied".into()),
                            Err(e) => self.toast_err(e),
                        }
                    }
                }
                _ => {}
            },
        }
    }

    /// Run the pipeline; `upto` = run only stages 0..=upto (FR-18).
    fn run_agg(&mut self, upto: Option<usize>) {
        let Some(agg) = &mut self.agg else { return };
        let Some(mut stages) = agg.parse() else {
            return;
        };
        if pipeline_writes(&stages) {
            agg.error = Some("$out/$merge write stages are not allowed in the preview".into());
            return;
        }
        let upto = upto.unwrap_or(stages.len() - 1).min(stages.len() - 1);
        stages.truncate(upto + 1);
        agg.selected_stage = upto;
        agg.running = true;
        let ns = format!("{}.{}", agg.db, agg.coll);
        let pipeline_text = agg.editor.text();
        let (db, coll) = (agg.db.clone(), agg.coll.clone());
        // Persist the pipeline text per collection (FR-19).
        self.state.pipelines.insert(ns, pipeline_text);
        if let Err(e) = config::save_state(&self.state) {
            self.toast_err(format!("could not save state: {e}"));
        }
        self.generation += 1;
        let generation = self.generation;
        self.send(Command::Aggregate {
            generation,
            db,
            coll,
            pipeline: stages,
            limit: AGG_PREVIEW_LIMIT,
        });
    }

    fn open_indexes(&mut self) {
        let Some((db, coll)) = self.results.target.clone() else {
            self.toast_err("select a collection first".into());
            return;
        };
        self.modal = Modal::Indexes(IndexesView {
            db: db.clone(),
            coll: coll.clone(),
            indexes: None,
            selected: 0,
        });
        self.send(Command::ListIndexes { db, coll });
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

    /// Typing mode for in-results search (FR-26): live-jumps as you type.
    fn on_key_results_search(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                self.results.searching = false;
                self.results.search.clear();
            }
            KeyCode::Enter => self.results.searching = false,
            KeyCode::Backspace => {
                self.results.search.pop();
                self.jump_to_match(true, true);
            }
            KeyCode::Char(c) => {
                self.results.search.push(c);
                self.jump_to_match(true, true);
            }
            _ => {}
        }
    }

    /// Move to the next/previous match; `include_current` keeps the cursor in
    /// place if it already matches (used while typing).
    fn jump_to_match(&mut self, forward: bool, include_current: bool) {
        let needle = self.results.search.to_lowercase();
        if needle.is_empty() {
            return;
        }
        match self.view {
            ViewMode::Json => {
                let n = self.results.lines.len();
                if n == 0 {
                    return;
                }
                let hit = |i: usize| {
                    rline_text(&self.results.lines[i])
                        .to_lowercase()
                        .contains(&needle)
                };
                let cur = self.results.cursor;
                if include_current && hit(cur) {
                    return;
                }
                let found = scan(n, cur, forward, include_current, hit);
                if let Some(i) = found {
                    self.results.cursor = i;
                } else {
                    self.toast_info(format!("no match for \"{}\"", self.results.search));
                }
            }
            ViewMode::Table => {
                let n = self.results.docs.len();
                if n == 0 {
                    return;
                }
                let cols = self.results.table.columns.clone();
                let hit = |i: usize| {
                    let doc = &self.results.docs[i];
                    cols.iter().any(|c| {
                        doc.get(c)
                            .map(|v| util::bson_to_compact(v).to_lowercase().contains(&needle))
                            .unwrap_or(false)
                    })
                };
                let cur = self.results.table.row;
                if include_current && hit(cur) {
                    return;
                }
                let found = scan(n, cur, forward, include_current, hit);
                if let Some(i) = found {
                    self.results.table.row = i;
                } else {
                    self.toast_info(format!("no match for \"{}\"", self.results.search));
                }
            }
        }
        self.maybe_fetch_more();
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
            self.query.history.push(text.clone());
            // Persist per collection (FR-13).
            if let Some((db, coll)) = &self.results.target {
                let ns = format!("{db}.{coll}");
                self.state.push_history(&ns, text);
                let _ = config::save_state(&self.state);
            }
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
        if self.screen == Screen::Agg {
            if let Some(agg) = &mut self.agg {
                match m.kind {
                    MouseEventKind::Down(MouseButton::Left) => {
                        if agg.stages_area.contains(pos) {
                            agg.focus = AggFocus::Stages;
                        } else if agg.editor_area.contains(pos) {
                            agg.focus = AggFocus::Editor;
                        } else if agg.results_area.contains(pos) {
                            agg.focus = AggFocus::Results;
                        }
                    }
                    MouseEventKind::ScrollDown if agg.results_area.contains(pos) => {
                        agg.scroll = (agg.scroll + 3).min(agg.lines.len().saturating_sub(1));
                    }
                    MouseEventKind::ScrollUp if agg.results_area.contains(pos) => {
                        agg.scroll = agg.scroll.saturating_sub(3);
                    }
                    _ => {}
                }
            }
            return;
        }
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

/// Concatenated plain text of a rendered line (for search).
fn rline_text(l: &RLine) -> String {
    l.line
        .spans
        .iter()
        .map(|s| s.content.as_ref())
        .collect::<String>()
}

/// Scan indices circularly from `start`, forward or backward, returning the
/// first index where `hit` is true.
fn scan(
    n: usize,
    start: usize,
    forward: bool,
    include_current: bool,
    hit: impl Fn(usize) -> bool,
) -> Option<usize> {
    let offsets: Box<dyn Iterator<Item = usize>> = if include_current {
        Box::new(0..n)
    } else {
        Box::new(1..=n)
    };
    for off in offsets {
        let i = if forward {
            (start + off) % n
        } else {
            (start + n - (off % n)) % n
        };
        if hit(i) {
            return Some(i);
        }
    }
    None
}

/// Compact relaxed-extjson rendering of a filter/update doc for summaries.
fn filter_display(doc: &Document) -> String {
    let s = Bson::Document(doc.clone())
        .into_relaxed_extjson()
        .to_string();
    if s.chars().count() > 120 {
        let truncated: String = s.chars().take(119).collect();
        format!("{truncated}…")
    } else {
        s
    }
}

fn bson_display(v: &Bson) -> String {
    match v {
        Bson::ObjectId(oid) => format!("ObjectId(\"{oid}\")"),
        Bson::String(s) => format!("\"{s}\""),
        other => other.to_string(),
    }
}

/// Top-level field diff between the original and edited document.
fn diff_summary(original: &Document, edited: &Document) -> String {
    let mut added = Vec::new();
    let mut removed = Vec::new();
    let mut changed = Vec::new();
    for (k, v) in edited.iter() {
        match original.get(k) {
            None => added.push(k.clone()),
            Some(old) if old != v => changed.push(k.clone()),
            _ => {}
        }
    }
    for k in original.keys() {
        if k != "_id" && !edited.contains_key(k) {
            removed.push(k.clone());
        }
    }
    let mut parts = Vec::new();
    if !changed.is_empty() {
        parts.push(format!("~ {}", changed.join(", ")));
    }
    if !added.is_empty() {
        parts.push(format!("+ {}", added.join(", ")));
    }
    if !removed.is_empty() {
        parts.push(format!("- {}", removed.join(", ")));
    }
    if parts.is_empty() {
        "none (documents are identical)".into()
    } else {
        parts.join("  ")
    }
}

/// Rebuild a JsonEditor preserving the typed text (used to re-show errors).
fn editor_with(title: String, text: String, purpose: EditorPurpose) -> JsonEditor {
    JsonEditor {
        title,
        area: TextArea::from_text(&text),
        purpose,
        error: None,
    }
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
pub async fn run(terminal: &mut term::Term, uri: Option<String>, read_only: bool) -> Result<()> {
    let (cmd_tx, mut core_rx, cancel_tx) = actor::spawn(read_only);
    let mut input_rx = event::input_channel();
    let mut app = App::new(String::new(), cmd_tx, cancel_tx, read_only);
    app.config_theme = config::load_config().ok().and_then(|c| c.theme);

    match uri {
        Some(uri) => {
            app.uri_display = redact_uri(&uri);
            app.active_uri = Some(uri.clone());
            app.send(Command::Connect { uri });
        }
        None => match config::load_config() {
            Err(e) => {
                app.uri_display = "config error".into();
                app.conn = ConnState::Failed(e);
            }
            Ok(cfg) if !cfg.connections.is_empty() => {
                app.conn = ConnState::Idle;
                app.uri_display = "select a connection".into();
                app.modal = Modal::Connections {
                    items: cfg.connections,
                    selected: 0,
                };
            }
            Ok(_) => {
                let uri = "mongodb://localhost:27017".to_string();
                app.uri_display = redact_uri(&uri);
                app.active_uri = Some(uri.clone());
                app.send(Command::Connect { uri });
            }
        },
    }

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
        if app.pending_shell {
            if let Some(uri) = app.take_shell_uri() {
                run_mongosh(terminal, &mut app, &uri);
            }
        }
    }
    Ok(())
}

/// Suspend the TUI, run mongosh attached to this terminal, resume.
fn run_mongosh(terminal: &mut term::Term, app: &mut App, uri: &str) {
    term::restore();
    println!("lazymongo: opening mongosh — type exit (or Ctrl-D) to return\n");
    let status = std::process::Command::new("mongosh").arg(uri).status();
    let _ = term::reenter(terminal);
    match status {
        Ok(_) => app.toast_info("back from mongosh".into()),
        Err(e) => app.toast_err(format!("could not launch mongosh: {e} (is it installed?)")),
    }
}
