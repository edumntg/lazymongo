//! Rendering: pure view of the App model, plus per-frame scroll clamping and
//! hit-test rect bookkeeping.

use lazymongo_core::types::CollectionInfo;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, BorderType, Clear, Paragraph};
use ratatui::Frame;

use crate::app::{App, ConnState, ExplorerRow, Pane, ViewMode};
use crate::modal::{
    Confirm, DocView, IndexesView, JsonEditor, Modal, Prompt, QueryEditor, QUERY_FIELD_LABELS,
};
use crate::util;

const SPINNER: [&str; 8] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧"];

fn focused_style(focused: bool) -> Style {
    if focused {
        Style::new().fg(Color::Cyan)
    } else {
        Style::new().fg(Color::DarkGray)
    }
}

pub fn draw(f: &mut Frame, app: &mut App) {
    let [status, main, query, help] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(3),
        Constraint::Length(3),
        Constraint::Length(1),
    ])
    .areas(f.area());

    let sidebar_w = (f.area().width as f32 * 0.28).clamp(24.0, 44.0) as u16;
    let [explorer, results] =
        Layout::horizontal([Constraint::Length(sidebar_w), Constraint::Min(20)]).areas(main);

    app.explorer_area = explorer;
    app.results_area = results;
    app.query_area = query;

    draw_status(f, app, status);
    draw_explorer(f, app, explorer);
    draw_results(f, app, results);
    draw_query(f, app, query);
    draw_help_bar(f, app, help);
    draw_modal(f, app);
}

fn spinner(app: &App) -> &'static str {
    SPINNER[app.spinner_frame % SPINNER.len()]
}

fn draw_status(f: &mut Frame, app: &App, area: Rect) {
    let sep = Span::styled("  •  ", Style::new().fg(Color::DarkGray));
    let mut spans = vec![Span::styled(
        " lazymongo ",
        Style::new()
            .fg(Color::Black)
            .bg(Color::Green)
            .add_modifier(Modifier::BOLD),
    )];
    if app.read_only {
        spans.push(Span::styled(
            " RO ",
            Style::new()
                .fg(Color::White)
                .bg(Color::Red)
                .add_modifier(Modifier::BOLD),
        ));
    }
    spans.push(Span::raw(" "));
    spans.push(Span::styled(
        app.uri_display.clone(),
        Style::new().fg(Color::White),
    ));
    match &app.conn {
        ConnState::Connecting => {
            spans.push(sep.clone());
            spans.push(Span::styled(
                format!("{} connecting…", spinner(app)),
                Style::new().fg(Color::Yellow),
            ));
        }
        ConnState::Connected { version, ping_ms } => {
            spans.push(sep.clone());
            spans.push(Span::styled(
                format!("MongoDB {version}"),
                Style::new().fg(Color::Green),
            ));
            spans.push(sep.clone());
            spans.push(Span::styled(
                format!("{ping_ms}ms"),
                Style::new().fg(Color::Green),
            ));
        }
        ConnState::Failed(e) => {
            spans.push(sep.clone());
            spans.push(Span::styled(
                format!("connection failed: {e}"),
                Style::new().fg(Color::Red),
            ));
        }
    }
    if let Some((msg, is_err, _)) = &app.toast {
        spans.push(sep);
        spans.push(Span::styled(
            msg.clone(),
            Style::new().fg(if *is_err { Color::Red } else { Color::Yellow }),
        ));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = bytes as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{bytes}B")
    } else {
        format!("{v:.1}{}", UNITS[u])
    }
}

fn human_count(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}k", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

fn draw_explorer(f: &mut Frame, app: &mut App, area: Rect) {
    let focused = app.focus == Pane::Explorer;
    let mut title = String::from(" 1 Explorer ");
    if app.explorer.loading {
        title = format!(" 1 Explorer {} ", spinner(app));
    }
    if app.explorer.filtering || !app.explorer.filter.is_empty() {
        title.push_str(&format!("/{}", app.explorer.filter));
        if app.explorer.filtering {
            title.push('▏');
        }
        title.push(' ');
    }
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(focused_style(focused))
        .title(title);
    let inner = block.inner(area);
    f.render_widget(block, area);

    let rows = app.explorer.rows();
    let height = inner.height as usize;

    // Keep selection visible.
    if app.explorer.selected < app.explorer.scroll {
        app.explorer.scroll = app.explorer.selected;
    } else if height > 0 && app.explorer.selected >= app.explorer.scroll + height {
        app.explorer.scroll = app.explorer.selected - height + 1;
    }

    let mut lines: Vec<Line> = Vec::with_capacity(height);
    for (i, row) in rows
        .iter()
        .enumerate()
        .skip(app.explorer.scroll)
        .take(height)
    {
        let selected = i == app.explorer.selected;
        let mut line = match *row {
            ExplorerRow::Db(di) => {
                let node = &app.explorer.dbs[di];
                let marker = if node.expanded { "▾ " } else { "▸ " };
                let mut spans = vec![
                    Span::styled(marker, Style::new().fg(Color::Yellow)),
                    Span::styled(
                        node.info.name.clone(),
                        Style::new().fg(Color::White).add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        format!("  {}", human_size(node.info.size_on_disk)),
                        Style::new().fg(Color::DarkGray),
                    ),
                ];
                if node.loading {
                    spans.push(Span::styled(
                        format!(" {}", spinner(app)),
                        Style::new().fg(Color::Yellow),
                    ));
                }
                Line::from(spans)
            }
            ExplorerRow::Coll { db, coll } => {
                let c: &CollectionInfo = &app.explorer.dbs[db].colls.as_ref().unwrap()[coll];
                let count = c
                    .estimated_count
                    .map(|n| format!("  ~{}", human_count(n)))
                    .unwrap_or_default();
                Line::from(vec![
                    Span::raw("   "),
                    Span::styled(c.name.clone(), Style::new().fg(Color::Cyan)),
                    Span::styled(count, Style::new().fg(Color::DarkGray)),
                ])
            }
        };
        if selected {
            line.style = Style::new().bg(if focused {
                Color::Blue
            } else {
                Color::DarkGray
            });
        }
        lines.push(line);
    }
    if rows.is_empty() && !app.explorer.loading {
        lines.push(Line::from(Span::styled(
            "  no databases",
            Style::new().fg(Color::DarkGray),
        )));
    }
    f.render_widget(Paragraph::new(Text::from(lines)), inner);
}

fn results_title(app: &App) -> String {
    match &app.results.target {
        None => " 2 Results ".to_string(),
        Some((db, coll)) => {
            let view = match app.view {
                ViewMode::Json => "json",
                ViewMode::Table => "table",
            };
            let mut t = format!(" 2 Results ─ {db}.{coll} [{view}] ");
            if let Some(total) = app.results.total_estimate {
                t.push_str(&format!("~{} docs ", human_count(total)));
            }
            let loaded = app.results.docs.len();
            if app.results.evicted > 0 {
                t.push_str(&format!(
                    "(docs {}–{} in window) ",
                    app.results.evicted + 1,
                    app.results.evicted + loaded as u64
                ));
            } else {
                t.push_str(&format!("({loaded} loaded"));
                t.push_str(if app.results.exhausted {
                    ", all) "
                } else {
                    "+) "
                });
            }
            if app.results.loading {
                t.push_str(spinner(app));
                t.push(' ');
            }
            t
        }
    }
}

fn draw_results(f: &mut Frame, app: &mut App, area: Rect) {
    let focused = app.focus == Pane::Results;
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(focused_style(focused))
        .title(results_title(app));
    let inner = block.inner(area);
    f.render_widget(block, area);

    if app.results.target.is_none() {
        let hint = Paragraph::new(Text::from(vec![
            Line::raw(""),
            Line::from(Span::styled(
                "  Select a collection in the Explorer to browse documents.",
                Style::new().fg(Color::DarkGray),
            )),
            Line::from(Span::styled(
                "  Press ? for all keybindings.",
                Style::new().fg(Color::DarkGray),
            )),
        ]));
        f.render_widget(hint, inner);
        return;
    }

    match app.view {
        ViewMode::Json => draw_results_json(f, app, inner, focused),
        ViewMode::Table => draw_results_table(f, app, inner, focused),
    }
}

fn draw_results_json(f: &mut Frame, app: &mut App, inner: Rect, focused: bool) {
    let height = inner.height as usize;
    let len = app.results.lines.len();

    // Keep cursor visible when it moved via keys.
    if app.results.cursor < app.results.scroll {
        app.results.scroll = app.results.cursor;
    } else if height > 0 && app.results.cursor >= app.results.scroll + height {
        app.results.scroll = app.results.cursor - height + 1;
    }
    app.results.scroll = app.results.scroll.min(len.saturating_sub(1));

    let mut lines: Vec<Line> = Vec::with_capacity(height);
    for (i, rline) in app
        .results
        .lines
        .iter()
        .enumerate()
        .skip(app.results.scroll)
        .take(height)
    {
        let mut line = rline.line.clone();
        if i == app.results.cursor {
            line.style = Style::new().bg(if focused {
                Color::Blue
            } else {
                Color::DarkGray
            });
        }
        lines.push(line);
    }
    if len == 0 && !app.results.loading {
        lines.push(Line::from(Span::styled(
            "  no documents match",
            Style::new().fg(Color::DarkGray),
        )));
    }
    f.render_widget(Paragraph::new(Text::from(lines)), inner);
}

fn pad_cell(s: &str, width: u16) -> String {
    let w = width as usize;
    let mut out: String = s.chars().take(w).collect();
    if s.chars().count() > w && w > 1 {
        out.truncate(out.chars().count().saturating_sub(1));
        let mut truncated: String = out.chars().take(w - 1).collect();
        truncated.push('…');
        out = truncated;
    }
    let pad = w.saturating_sub(out.chars().count());
    out.push_str(&" ".repeat(pad));
    out
}

fn draw_results_table(f: &mut Frame, app: &mut App, inner: Rect, focused: bool) {
    let height = inner.height.saturating_sub(1) as usize; // header
    let rows = app.results.docs.len();
    let t = &mut app.results.table;

    // Keep selected row visible.
    if t.row < t.scroll_row {
        t.scroll_row = t.row;
    } else if height > 0 && t.row >= t.scroll_row + height {
        t.scroll_row = t.row - height + 1;
    }
    t.scroll_row = t.scroll_row.min(rows.saturating_sub(1));

    // Keep active column visible: advance col_offset until it fits.
    t.col_offset = t.col_offset.min(t.active_col);
    loop {
        let mut x = 0u16;
        let mut fits = false;
        for i in t.col_offset..t.columns.len() {
            let w = t.widths[i] + 1;
            if i == t.active_col && x + w <= inner.width {
                fits = true;
            }
            x += w;
            if x > inner.width {
                break;
            }
        }
        if fits || t.col_offset >= t.active_col {
            break;
        }
        t.col_offset += 1;
    }

    // Header + hit ranges.
    t.col_hit.clear();
    let sort = app.results.active_spec.sort.clone();
    let mut header_spans: Vec<Span> = Vec::new();
    let mut x = inner.x;
    for i in t.col_offset..t.columns.len() {
        let w = t.widths[i];
        if x + w >= inner.x + inner.width {
            break;
        }
        let col = &t.columns[i];
        let arrow = match sort.as_ref().and_then(|s| s.get_i32(col.as_str()).ok()) {
            Some(1) => "↑",
            Some(-1) => "↓",
            _ => "",
        };
        let label = pad_cell(&format!("{col}{arrow}"), w);
        let style = if i == t.active_col && focused {
            Style::new()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
        } else {
            Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD)
        };
        header_spans.push(Span::styled(label, style));
        header_spans.push(Span::raw(" "));
        t.col_hit.push((x, x + w + 1, i));
        x += w + 1;
    }
    let mut lines: Vec<Line> = vec![Line::from(header_spans)];

    // Rows.
    for (ri, doc) in app
        .results
        .docs
        .iter()
        .enumerate()
        .skip(app.results.table.scroll_row)
        .take(height)
    {
        let t = &app.results.table;
        let mut spans: Vec<Span> = Vec::new();
        let mut x = inner.x;
        for i in t.col_offset..t.columns.len() {
            let w = t.widths[i];
            if x + w >= inner.x + inner.width {
                break;
            }
            let cell = doc
                .get(&t.columns[i])
                .map(util::bson_to_compact)
                .unwrap_or_default();
            let style = if i == t.active_col && focused {
                Style::new().fg(Color::White)
            } else {
                Style::new().fg(Color::Gray)
            };
            spans.push(Span::styled(pad_cell(&cell, w), style));
            spans.push(Span::raw(" "));
            x += w + 1;
        }
        let mut line = Line::from(spans);
        if ri == t.row {
            line.style = Style::new().bg(if focused {
                Color::Blue
            } else {
                Color::DarkGray
            });
        }
        lines.push(line);
    }
    if rows == 0 && !app.results.loading {
        lines.push(Line::from(Span::styled(
            "  no documents match",
            Style::new().fg(Color::DarkGray),
        )));
    }
    f.render_widget(Paragraph::new(Text::from(lines)), inner);
}

fn draw_query(f: &mut Frame, app: &App, area: Rect) {
    let focused = app.focus == Pane::Query;
    let mut title_spans = vec![Span::raw(" 3 Query (find filter) ")];
    if !app.extras.is_default() {
        title_spans.push(Span::styled(
            "+projection/sort/limit — F to edit ",
            Style::new().fg(Color::Yellow),
        ));
    }
    if let Some(e) = &app.query.error {
        title_spans = vec![
            Span::raw(" 3 Query ─ "),
            Span::styled(e.clone(), Style::new().fg(Color::Red)),
            Span::raw(" "),
        ];
    }
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(focused_style(focused))
        .title(Line::from(title_spans));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let mut spans = vec![Span::styled("filter> ", Style::new().fg(Color::Yellow))];
    if focused {
        // Render a visible cursor by splitting the input at the cursor column.
        let chars: Vec<char> = app.query.input.chars().collect();
        let before: String = chars[..app.query.cursor].iter().collect();
        let at: String = chars
            .get(app.query.cursor)
            .map(|c| c.to_string())
            .unwrap_or_else(|| " ".into());
        let after: String = if app.query.cursor < chars.len() {
            chars[app.query.cursor + 1..].iter().collect()
        } else {
            String::new()
        };
        spans.push(Span::raw(before));
        spans.push(Span::styled(
            at,
            Style::new().add_modifier(Modifier::REVERSED),
        ));
        spans.push(Span::raw(after));
    } else {
        spans.push(Span::raw(app.query.input.clone()));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), inner);
}

fn draw_help_bar(f: &mut Frame, app: &App, area: Rect) {
    let entries: &[(&str, &str)] = if app.modal.is_open() {
        match &app.modal {
            Modal::DocView(_) => &[
                ("↑↓/jk", "move"),
                ("↵", "fold"),
                ("y", "copy"),
                ("esc", "close"),
            ],
            Modal::QueryEditor(_) => &[("tab/↑↓", "field"), ("↵", "run"), ("esc", "cancel")],
            Modal::Editor(_) => &[("type", "edit json"), ("^s", "save"), ("esc", "cancel")],
            Modal::Confirm(c) if c.typed_required.is_some() => &[
                ("type", "confirm text"),
                ("↵", "confirm"),
                ("esc", "cancel"),
            ],
            Modal::Confirm(_) => &[("y/↵", "confirm"), ("n/esc", "cancel")],
            Modal::Indexes(_) => &[
                ("↑↓", "move"),
                ("c", "create"),
                ("d", "drop"),
                ("r", "refresh"),
                ("esc", "close"),
            ],
            Modal::Prompt(_) => &[("type", "name"), ("↵", "ok"), ("esc", "cancel")],
            _ => &[("any key", "close")],
        }
    } else {
        match app.focus {
            Pane::Explorer if app.explorer.filtering => {
                &[("type", "filter"), ("↵", "apply"), ("esc", "clear")]
            }
            Pane::Explorer => &[
                ("↑↓/jk", "move"),
                ("↵", "expand/open"),
                ("/", "filter"),
                ("r", "refresh"),
                ("tab", "pane"),
                ("?", "help"),
                ("q", "quit"),
            ],
            Pane::Results => match app.view {
                ViewMode::Json => &[
                    ("↑↓/jk", "move"),
                    ("↵", "fold"),
                    ("v", "table"),
                    ("o", "doc"),
                    ("F", "query"),
                    ("x", "explain"),
                    ("e", "edit"),
                    ("i", "insert"),
                    ("d", "delete"),
                    ("?", "help"),
                ],
                ViewMode::Table => &[
                    ("↑↓", "row"),
                    ("←→", "column"),
                    ("s", "sort"),
                    ("↵", "open doc"),
                    ("v", "json"),
                    ("e", "edit"),
                    ("d", "delete"),
                    ("?", "help"),
                ],
            },
            Pane::Query => &[
                ("↵", "run"),
                ("↑↓", "history"),
                ("F", "full editor"),
                ("esc", "back"),
                ("^c", "quit"),
            ],
        }
    };
    let mut spans = Vec::new();
    for (key, desc) in entries {
        spans.push(Span::styled(
            format!(" {key} "),
            Style::new().fg(Color::Black).bg(Color::DarkGray),
        ));
        spans.push(Span::styled(
            format!(" {desc}  "),
            Style::new().fg(Color::DarkGray),
        ));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

// ---------- modals ----------

fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let w = width.min(area.width.saturating_sub(2));
    let h = height.min(area.height.saturating_sub(2));
    Rect {
        x: area.x + (area.width - w) / 2,
        y: area.y + (area.height - h) / 2,
        width: w,
        height: h,
    }
}

fn draw_modal(f: &mut Frame, app: &mut App) {
    let spin = spinner(app);
    match &mut app.modal {
        Modal::None => {}
        Modal::Help => draw_help_overlay(f, f.area()),
        Modal::QueryEditor(editor) => draw_query_editor(f, f.area(), editor),
        Modal::DocView(view) => draw_doc_view(f, f.area(), view),
        Modal::Confirm(confirm) => draw_confirm(f, f.area(), confirm),
        Modal::Editor(editor) => draw_json_editor(f, f.area(), editor),
        Modal::Indexes(view) => draw_indexes(f, f.area(), view, spin),
        Modal::OpsLog { scroll } => draw_ops_log(f, f.area(), &app.ops_log, scroll),
        Modal::Prompt(prompt) => draw_prompt(f, f.area(), prompt),
    }
}

fn draw_confirm(f: &mut Frame, area: Rect, confirm: &Confirm) {
    let h = (confirm.body.len() as u16 + 5).max(8);
    let popup = centered(area, 64, h);
    f.render_widget(Clear, popup);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(Color::Red))
        .title(Line::from(Span::styled(
            format!(" {} ", confirm.title),
            Style::new().fg(Color::Red).add_modifier(Modifier::BOLD),
        )));
    let inner = block.inner(popup);
    f.render_widget(block, popup);

    let mut lines: Vec<Line> = confirm
        .body
        .iter()
        .map(|s| Line::from(Span::styled(format!(" {s}"), Style::new().fg(Color::White))))
        .collect();
    lines.push(Line::raw(""));
    if confirm.typed_required.is_some() {
        let mut spans = vec![Span::styled(" > ", Style::new().fg(Color::Yellow))];
        spans.extend(confirm.typed.spans(true));
        lines.push(Line::from(spans));
        let ok = confirm.typed_ok();
        lines.push(Line::from(Span::styled(
            if ok {
                " ↵ confirm · esc cancel"
            } else {
                " esc cancel"
            },
            Style::new().fg(if ok { Color::Green } else { Color::DarkGray }),
        )));
    } else {
        lines.push(Line::from(Span::styled(
            " y/↵ confirm · n/esc cancel",
            Style::new().fg(Color::DarkGray),
        )));
    }
    f.render_widget(Paragraph::new(Text::from(lines)), inner);
}

fn draw_json_editor(f: &mut Frame, area: Rect, editor: &mut JsonEditor) {
    let popup = centered(area, 84, area.height.saturating_sub(6));
    f.render_widget(Clear, popup);
    let title = format!(" {} ─ ^s save · esc cancel ", editor.title);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(Color::Cyan))
        .title(title);
    let mut inner = block.inner(popup);
    f.render_widget(block, popup);

    if let Some(e) = &editor.error {
        let err_area = Rect { height: 1, ..inner };
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                format!(" ✗ {e}"),
                Style::new().fg(Color::White).bg(Color::Red),
            ))),
            err_area,
        );
        inner.y += 1;
        inner.height = inner.height.saturating_sub(1);
    }
    editor.area.render(f, inner, true);
}

fn draw_indexes(f: &mut Frame, area: Rect, view: &IndexesView, spin: &str) {
    let popup = centered(area, 70, 18);
    f.render_widget(Clear, popup);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(Color::Cyan))
        .title(format!(
            " Indexes ─ {}.{} ─ c create · d drop · r refresh · esc close ",
            view.db, view.coll
        ));
    let inner = block.inner(popup);
    f.render_widget(block, popup);

    let mut lines: Vec<Line> = Vec::new();
    match &view.indexes {
        None => lines.push(Line::from(Span::styled(
            format!(" {spin} loading…"),
            Style::new().fg(Color::Yellow),
        ))),
        Some(list) if list.is_empty() => lines.push(Line::from(Span::styled(
            " no indexes",
            Style::new().fg(Color::DarkGray),
        ))),
        Some(list) => {
            for (i, idx) in list.iter().enumerate() {
                let unique = if idx.unique { "  [unique]" } else { "" };
                let keys_json = lazymongo_core::bson::Bson::Document(idx.keys.clone())
                    .into_relaxed_extjson()
                    .to_string();
                let mut line = Line::from(vec![
                    Span::styled(format!(" {:<24}", idx.name), Style::new().fg(Color::Cyan)),
                    Span::styled(keys_json, Style::new().fg(Color::White)),
                    Span::styled(unique, Style::new().fg(Color::Yellow)),
                ]);
                if i == view.selected {
                    line.style = Style::new().bg(Color::Blue);
                }
                lines.push(line);
            }
        }
    }
    f.render_widget(Paragraph::new(Text::from(lines)), inner);
}

fn draw_ops_log(f: &mut Frame, area: Rect, log: &[String], scroll: &mut usize) {
    let popup = centered(area, 90, 20);
    f.render_widget(Clear, popup);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(Color::Cyan))
        .title(format!(
            " Operations log ({} · session, UTC) ─ esc close ",
            log.len()
        ));
    let inner = block.inner(popup);
    f.render_widget(block, popup);

    *scroll = (*scroll).min(log.len().saturating_sub(1));
    let mut lines: Vec<Line> = Vec::new();
    if log.is_empty() {
        lines.push(Line::from(Span::styled(
            " no write operations this session",
            Style::new().fg(Color::DarkGray),
        )));
    }
    for entry in log.iter().skip(*scroll).take(inner.height as usize) {
        lines.push(Line::from(Span::styled(
            format!(" {entry}"),
            Style::new().fg(Color::White),
        )));
    }
    f.render_widget(Paragraph::new(Text::from(lines)), inner);
}

fn draw_prompt(f: &mut Frame, area: Rect, prompt: &Prompt) {
    let popup = centered(area, 54, 5);
    f.render_widget(Clear, popup);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(Color::Cyan))
        .title(format!(" {} ─ ↵ ok · esc cancel ", prompt.title));
    let inner = block.inner(popup);
    f.render_widget(block, popup);
    let mut spans = vec![Span::styled(" > ", Style::new().fg(Color::Yellow))];
    spans.extend(prompt.input.spans(true));
    f.render_widget(Paragraph::new(Line::from(spans)), inner);
}

fn draw_query_editor(f: &mut Frame, area: Rect, editor: &QueryEditor) {
    let popup = centered(area, 72, 11);
    f.render_widget(Clear, popup);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(Color::Cyan))
        .title(" Query editor ─ ↵ run · esc cancel ");
    let inner = block.inner(popup);
    f.render_widget(block, popup);

    let mut lines: Vec<Line> = Vec::new();
    for (i, field) in editor.fields.iter().enumerate() {
        let focused = i == editor.focus;
        let label_style = if focused {
            Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD)
        } else {
            Style::new().fg(Color::Cyan)
        };
        let mut spans = vec![Span::styled(
            format!(" {:<11}", QUERY_FIELD_LABELS[i]),
            label_style,
        )];
        spans.extend(field.spans(focused));
        lines.push(Line::from(spans));
        lines.push(Line::raw(""));
    }
    if let Some(e) = &editor.error {
        lines.push(Line::from(Span::styled(
            format!(" {e}"),
            Style::new().fg(Color::Red),
        )));
    }
    f.render_widget(Paragraph::new(Text::from(lines)), inner);
}

fn draw_doc_view(f: &mut Frame, area: Rect, view: &mut DocView) {
    let popup = centered(
        area,
        area.width.saturating_sub(8),
        area.height.saturating_sub(4),
    );
    f.render_widget(Clear, popup);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(Color::Cyan))
        .title(format!(" {} ", view.title));
    let mut inner = block.inner(popup);
    f.render_widget(block, popup);

    if let Some(warn) = &view.warn {
        let warn_area = Rect { height: 1, ..inner };
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                format!(" ⚠ {warn}"),
                Style::new().fg(Color::White).bg(Color::Red),
            ))),
            warn_area,
        );
        inner.y += 1;
        inner.height = inner.height.saturating_sub(1);
    }

    let height = inner.height as usize;
    let len = view.lines.len();
    if view.cursor < view.scroll {
        view.scroll = view.cursor;
    } else if height > 0 && view.cursor >= view.scroll + height {
        view.scroll = view.cursor - height + 1;
    }
    view.scroll = view.scroll.min(len.saturating_sub(1));

    let mut lines: Vec<Line> = Vec::with_capacity(height);
    for (i, rline) in view.lines.iter().enumerate().skip(view.scroll).take(height) {
        let mut line = rline.line.clone();
        if i == view.cursor {
            line.style = Style::new().bg(Color::Blue);
        }
        lines.push(line);
    }
    f.render_widget(Paragraph::new(Text::from(lines)), inner);
}

fn draw_help_overlay(f: &mut Frame, area: Rect) {
    let popup = centered(area, 66, 30);
    f.render_widget(Clear, popup);

    let key = |k: &str| Span::styled(format!("  {k:<14}"), Style::new().fg(Color::Cyan));
    let txt = |t: &str| Span::styled(t.to_string(), Style::new().fg(Color::White));
    let head = |t: &str| {
        Line::from(Span::styled(
            format!(" {t}"),
            Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD),
        ))
    };
    let lines = vec![
        head("Global"),
        Line::from(vec![key("q / ctrl-c"), txt("quit")]),
        Line::from(vec![key("tab / 1 2 3"), txt("switch pane")]),
        Line::from(vec![key("r"), txt("refresh current pane")]),
        Line::from(vec![key("?"), txt("toggle this help")]),
        Line::raw(""),
        head("Explorer"),
        Line::from(vec![key("↵ / → / l"), txt("expand db · open collection")]),
        Line::from(vec![key("/"), txt("filter databases & collections")]),
        Line::from(vec![key("N / X"), txt("new collection · drop collection")]),
        Line::raw(""),
        head("Results (both views)"),
        Line::from(vec![key("v"), txt("toggle json / table view")]),
        Line::from(vec![
            key("F"),
            txt("query editor (projection/sort/limit/skip)"),
        ]),
        Line::from(vec![key("x"), txt("explain query plan")]),
        Line::from(vec![key("o"), txt("open document full-screen")]),
        Line::from(vec![key("y"), txt("copy document to clipboard")]),
        Line::from(vec![key("E"), txt("export loaded docs (json / csv)")]),
        Line::raw(""),
        head("Writes (blocked in read-only mode)"),
        Line::from(vec![key("e / i"), txt("edit document · insert document")]),
        Line::from(vec![
            key("d / D"),
            txt("delete doc · delete by filter (dry run)"),
        ]),
        Line::from(vec![
            key("U"),
            txt("update many by filter (dry run + confirm)"),
        ]),
        Line::from(vec![key("I / L"), txt("indexes · operations log")]),
        Line::raw(""),
        head("Results · json view"),
        Line::from(vec![key("↵ / space"), txt("fold / unfold at cursor")]),
        Line::from(vec![key("^d ^u g G"), txt("scroll / jump")]),
        Line::raw(""),
        head("Results · table view"),
        Line::from(vec![key("← → / h l"), txt("select column")]),
        Line::from(vec![key("s / click header"), txt("server sort by column")]),
        Line::from(vec![key("↵"), txt("open row as document")]),
        Line::raw(""),
        head("Query bar"),
        Line::from(vec![
            key("↵ · ↑↓"),
            txt("run filter (mongosh syntax) · history"),
        ]),
    ];
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(Color::Cyan))
        .title(" Keybindings (any key closes) ");
    f.render_widget(Paragraph::new(Text::from(lines)).block(block), popup);
}
