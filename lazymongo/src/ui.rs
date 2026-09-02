//! Rendering: pure view of the App model, plus per-frame scroll clamping and
//! hit-test rect bookkeeping.

use lazymongo_core::types::CollectionInfo;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};

use crate::theme;
use ratatui::symbols;
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{
    Axis, BarChart, Block, BorderType, Chart, Clear, Dataset, GraphType, Paragraph,
};
use ratatui::Frame;

use crate::agg::{AggFocus, AggState, ChartKind, XKind};
use crate::app::{App, ConnState, ExplorerRow, Pane, Screen, ViewMode};
use crate::config::SavedConnection;
use crate::modal::{
    Confirm, ConnForm, DocView, IndexesView, JsonEditor, Modal, Palette, Prompt, QueryEditor,
    SchemaView, CONN_FIELD_LABELS, QUERY_FIELD_LABELS,
};
use crate::util;

const SPINNER: [&str; 8] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧"];

fn focused_style(focused: bool) -> Style {
    if focused {
        Style::new().fg(theme::accent())
    } else {
        Style::new().fg(theme::border_dim())
    }
}

pub fn draw(f: &mut Frame, app: &mut App) {
    if app.screen == Screen::Agg {
        return draw_agg_screen(f, app);
    }
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
    let sep = Span::styled("  •  ", Style::new().fg(theme::dim()));
    let mut spans = vec![Span::styled(
        " lazymongo ",
        Style::new()
            .fg(theme::badge_fg())
            .bg(theme::accent())
            .add_modifier(Modifier::BOLD),
    )];
    if app.read_only {
        spans.push(Span::styled(
            " RO ",
            Style::new()
                .fg(theme::badge_fg())
                .bg(theme::error())
                .add_modifier(Modifier::BOLD),
        ));
    }
    spans.push(Span::raw(" "));
    spans.push(Span::styled(
        app.uri_display.clone(),
        Style::new().fg(theme::text()),
    ));
    match &app.conn {
        ConnState::Idle => {}
        ConnState::Connecting => {
            spans.push(sep.clone());
            spans.push(Span::styled(
                format!("{} connecting…", spinner(app)),
                Style::new().fg(theme::warn()),
            ));
        }
        ConnState::Connected { version, ping_ms } => {
            spans.push(sep.clone());
            spans.push(Span::styled(
                format!("MongoDB {version}"),
                Style::new().fg(theme::ok()),
            ));
            spans.push(sep.clone());
            spans.push(Span::styled(
                format!("{ping_ms}ms"),
                Style::new().fg(theme::ok()),
            ));
        }
        ConnState::Failed(e) => {
            spans.push(sep.clone());
            spans.push(Span::styled(
                format!("connection failed: {e}"),
                Style::new().fg(theme::error()),
            ));
        }
    }
    if let Some((msg, is_err, _)) = &app.toast {
        spans.push(sep);
        spans.push(Span::styled(
            msg.clone(),
            Style::new().fg(if *is_err {
                theme::error()
            } else {
                theme::warn()
            }),
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
    // Discoverability: long lists advertise the filter shortcut.
    if focused
        && !app.explorer.filtering
        && app.explorer.filter.is_empty()
        && app.explorer.rows().len() > 12
    {
        title.push_str("(/ to filter) ");
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
                    Span::styled(marker, Style::new().fg(theme::warn())),
                    Span::styled(
                        node.info.name.clone(),
                        Style::new().fg(theme::text()).add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        format!("  {}", human_size(node.info.size_on_disk)),
                        Style::new().fg(theme::dim()),
                    ),
                ];
                if node.loading {
                    spans.push(Span::styled(
                        format!(" {}", spinner(app)),
                        Style::new().fg(theme::warn()),
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
                    Span::styled(c.name.clone(), Style::new().fg(theme::key())),
                    Span::styled(count, Style::new().fg(theme::dim())),
                ])
            }
        };
        if selected {
            line.style = Style::new().bg(if focused {
                theme::sel_bg()
            } else {
                theme::sel_bg_dim()
            });
        }
        lines.push(line);
    }
    if rows.is_empty() && !app.explorer.loading {
        lines.push(Line::from(Span::styled(
            "  no databases",
            Style::new().fg(theme::dim()),
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
            if !app.results.search.is_empty() || app.results.searching {
                t.push_str(&format!("/{}", app.results.search));
                if app.results.searching {
                    t.push('▏');
                }
                t.push(' ');
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
                Style::new().fg(theme::dim()),
            )),
            Line::from(Span::styled(
                "  Press ? for all keybindings.",
                Style::new().fg(theme::dim()),
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
                theme::sel_bg()
            } else {
                theme::sel_bg_dim()
            });
        }
        lines.push(line);
    }
    if len == 0 && !app.results.loading {
        lines.push(Line::from(Span::styled(
            "  no documents match",
            Style::new().fg(theme::dim()),
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
                .fg(theme::accent())
                .add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
        } else {
            Style::new().fg(theme::key()).add_modifier(Modifier::BOLD)
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
                Style::new().fg(theme::text()).add_modifier(Modifier::BOLD)
            } else {
                Style::new().fg(theme::text())
            };
            spans.push(Span::styled(pad_cell(&cell, w), style));
            spans.push(Span::raw(" "));
            x += w + 1;
        }
        let mut line = Line::from(spans);
        if ri == t.row {
            line.style = Style::new().bg(if focused {
                theme::sel_bg()
            } else {
                theme::sel_bg_dim()
            });
        }
        lines.push(line);
    }
    if rows == 0 && !app.results.loading {
        lines.push(Line::from(Span::styled(
            "  no documents match",
            Style::new().fg(theme::dim()),
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
            Style::new().fg(theme::warn()),
        ));
    }
    if let Some(e) = &app.query.error {
        title_spans = vec![
            Span::raw(" 3 Query ─ "),
            Span::styled(e.clone(), Style::new().fg(theme::error())),
            Span::raw(" "),
        ];
    }
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(focused_style(focused))
        .title(Line::from(title_spans));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let mut spans = vec![Span::styled("filter> ", Style::new().fg(theme::warn()))];
    spans.extend(crate::input::input_spans(
        &app.query.input,
        app.query.cursor,
        focused,
        inner.width.saturating_sub(8),
    ));
    f.render_widget(Paragraph::new(Line::from(spans)), inner);
}

fn draw_help_bar(f: &mut Frame, app: &App, area: Rect) {
    let entries: &[(&str, &str)] = if app.modal.is_open() {
        match &app.modal {
            Modal::DocView(_) => &[
                ("↑↓/jk", "move"),
                ("↵", "fold"),
                ("y/Y", "copy doc/node"),
                ("esc", "close"),
            ],
            Modal::QueryEditor(_) => &[("tab/↑↓", "field"), ("↵", "run"), ("esc", "cancel")],
            Modal::Editor(_) => &[
                ("type", "edit json"),
                ("^s/\u{21e7}\u{21b5}", "save"),
                ("^e", "$EDITOR"),
                ("esc", "cancel"),
            ],
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
            Modal::Palette(_) => &[
                ("type", "filter"),
                ("↑↓", "move"),
                ("↵", "run"),
                ("esc", "close"),
            ],
            Modal::Connections { .. } => &[
                ("↑↓", "move"),
                ("↵", "connect"),
                ("a", "add"),
                ("e", "edit"),
                ("d", "delete"),
                ("esc", "close/quit"),
            ],
            Modal::ConnForm(_) => &[
                ("tab/↑↓", "field"),
                ("space", "toggle ro"),
                ("↵", "save"),
                ("esc", "back"),
            ],
            _ => &[("any key", "close")],
        }
    } else {
        match app.focus {
            Pane::Explorer if app.explorer.filtering => {
                &[("type", "filter"), ("↵", "apply"), ("esc", "clear")]
            }
            Pane::Results if app.results.searching => &[
                ("type", "search"),
                ("↵", "apply"),
                ("n/N", "next/prev"),
                ("esc", "clear"),
            ],
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
                    ("r", "refresh"),
                    ("y/Y", "copy"),
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
            Style::new().fg(theme::badge_fg()).bg(theme::border_dim()),
        ));
        spans.push(Span::styled(
            format!(" {desc}  "),
            Style::new().fg(theme::dim()),
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
        Modal::Connections { items, selected } => draw_connections(f, f.area(), items, *selected),
        Modal::ConnForm(form) => draw_conn_form(f, f.area(), form),
        Modal::Palette(palette) => draw_palette(f, f.area(), palette),
        Modal::Schema(view) => draw_schema(f, f.area(), view, spin),
    }
}

fn draw_schema(f: &mut Frame, area: Rect, view: &mut SchemaView, spin: &str) {
    let popup = centered(area, 78, area.height.saturating_sub(6));
    f.render_widget(Clear, popup);
    let title = match &view.data {
        None => format!(" Schema ─ {}.{} {spin} sampling… ", view.db, view.coll),
        Some((sampled, _)) => format!(
            " Schema ─ {}.{} (sample of {sampled}) ─ r resample · esc close ",
            view.db, view.coll
        ),
    };
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(theme::accent()))
        .title(title);
    let inner = block.inner(popup);
    f.render_widget(block, popup);

    let Some((sampled, fields)) = &view.data else {
        return;
    };
    let sampled = (*sampled).max(1);
    view.scroll = view.scroll.min(fields.len().saturating_sub(1));
    let bar_w = 20usize;
    let mut lines: Vec<Line> = Vec::with_capacity(inner.height as usize);
    lines.push(Line::from(vec![
        Span::styled(format!(" {:<22}", "field"), Style::new().fg(theme::dim())),
        Span::styled(format!("{:<26}", "presence"), Style::new().fg(theme::dim())),
        Span::styled("types", Style::new().fg(theme::dim())),
    ]));
    for stat in fields
        .iter()
        .skip(view.scroll)
        .take(inner.height.saturating_sub(1) as usize)
    {
        let pct = f64::from(stat.present) * 100.0 / sampled as f64;
        let filled = ((pct / 100.0) * bar_w as f64).round() as usize;
        let bar: String = "█".repeat(filled.min(bar_w)) + &"░".repeat(bar_w - filled.min(bar_w));
        let mut name = stat.name.clone();
        if name.chars().count() > 21 {
            name = name.chars().take(20).collect::<String>() + "…";
        }
        lines.push(Line::from(vec![
            Span::styled(format!(" {name:<22}"), Style::new().fg(theme::key())),
            Span::styled(bar, Style::new().fg(theme::accent())),
            Span::styled(format!(" {pct:>3.0}%  "), Style::new().fg(theme::text())),
            Span::styled(stat.types.join(" | "), Style::new().fg(theme::dim())),
        ]));
    }
    f.render_widget(Paragraph::new(Text::from(lines)), inner);
}

fn draw_palette(f: &mut Frame, area: Rect, palette: &Palette) {
    let h = (palette.filtered.len() as u16 + 4).clamp(8, 20);
    let popup = centered(area, 72, h);
    f.render_widget(Clear, popup);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(theme::accent()))
        .title(format!(" {} ", palette.title));
    let inner = block.inner(popup);
    f.render_widget(block, popup);

    let mut lines: Vec<Line> = Vec::new();
    let mut prompt = vec![Span::styled(" > ", Style::new().fg(theme::marker()))];
    prompt.extend(palette.input.spans(true, inner.width.saturating_sub(4)));
    lines.push(Line::from(prompt));
    lines.push(Line::raw(""));

    let visible = inner.height.saturating_sub(2) as usize;
    let scroll = palette.selected.saturating_sub(visible.saturating_sub(1));
    for (row, &idx) in palette
        .filtered
        .iter()
        .enumerate()
        .skip(scroll)
        .take(visible)
    {
        let (label, _) = &palette.actions[idx];
        let mut line = Line::from(Span::styled(
            format!("  {label}"),
            Style::new().fg(theme::text()),
        ));
        if row == palette.selected {
            line.style = Style::new().bg(theme::sel_bg());
        }
        lines.push(line);
    }
    if palette.filtered.is_empty() {
        lines.push(Line::from(Span::styled(
            "  no matching command",
            Style::new().fg(theme::dim()),
        )));
    }
    f.render_widget(Paragraph::new(Text::from(lines)), inner);
}

fn draw_connections(f: &mut Frame, area: Rect, items: &[SavedConnection], selected: usize) {
    let h = (items.len() as u16 + 4).clamp(7, 20);
    let popup = centered(area, 64, h);
    f.render_widget(Clear, popup);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(theme::accent()))
        .title(" Connections ─ ↵ connect · a add · e edit · d delete ");
    let inner = block.inner(popup);
    f.render_widget(block, popup);

    let mut lines: Vec<Line> = Vec::new();
    if items.is_empty() {
        lines.push(Line::from(Span::styled(
            " no saved connections — press a to add one",
            Style::new().fg(theme::dim()),
        )));
    }
    for (i, conn) in items.iter().enumerate() {
        let ro = if conn.read_only { "  [read-only]" } else { "" };
        let source = match (&conn.uri, &conn.uri_env) {
            (Some(_), _) => String::new(),
            (None, Some(var)) => format!("  ${var}"),
            _ => "  (no uri!)".into(),
        };
        let mut line = Line::from(vec![
            Span::styled(format!(" {} ", i + 1), Style::new().fg(theme::dim())),
            Span::styled(conn.name.clone(), Style::new().fg(theme::text())),
            Span::styled(source, Style::new().fg(theme::dim())),
            Span::styled(ro, Style::new().fg(theme::error())),
        ]);
        if i == selected {
            line.style = Style::new().bg(theme::sel_bg());
        }
        lines.push(line);
    }
    f.render_widget(Paragraph::new(Text::from(lines)), inner);
}

fn draw_conn_form(f: &mut Frame, area: Rect, form: &ConnForm) {
    let popup = centered(area, 64, 13);
    f.render_widget(Clear, popup);
    let title = match form.editing {
        Some(_) => " Edit connection ─ ↵ save · esc back ",
        None => " Add connection ─ ↵ save · esc back ",
    };
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(theme::accent()))
        .title(title);
    let inner = block.inner(popup);
    f.render_widget(block, popup);

    let mut lines: Vec<Line> = Vec::new();
    for (i, field) in form.fields.iter().enumerate() {
        let focused = i == form.focus;
        let label_style = if focused {
            Style::new()
                .fg(theme::accent())
                .add_modifier(Modifier::BOLD)
        } else {
            Style::new().fg(theme::key())
        };
        let mut spans = vec![Span::styled(
            format!(" {:<10}", CONN_FIELD_LABELS[i]),
            label_style,
        )];
        spans.extend(field.spans(focused, inner.width.saturating_sub(13)));
        lines.push(Line::from(spans));
        lines.push(Line::raw(""));
    }
    let ro_focused = form.focus == 3;
    lines.push(Line::from(vec![
        Span::styled(
            " read_only ",
            if ro_focused {
                Style::new()
                    .fg(theme::accent())
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::new().fg(theme::key())
            },
        ),
        Span::styled(
            if form.read_only { "[x]" } else { "[ ]" },
            if ro_focused {
                Style::new()
                    .fg(theme::text())
                    .add_modifier(Modifier::REVERSED)
            } else {
                Style::new().fg(theme::text())
            },
        ),
        Span::styled("  (space toggles)", Style::new().fg(theme::dim())),
    ]));
    lines.push(Line::raw(""));
    lines.push(Line::from(Span::styled(
        " tip: leave uri empty and set uri_env to keep secrets out of the file",
        Style::new().fg(theme::dim()),
    )));
    if let Some(e) = &form.error {
        lines.push(Line::from(Span::styled(
            format!(" ✗ {e}"),
            Style::new().fg(theme::error()),
        )));
    }
    f.render_widget(Paragraph::new(Text::from(lines)), inner);
}

// ---------- aggregation screen ----------

fn draw_agg_screen(f: &mut Frame, app: &mut App) {
    let [status, main, help] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(3),
        Constraint::Length(1),
    ])
    .areas(f.area());
    draw_status(f, app, status);

    // Help bar for the agg screen.
    let entries: &[(&str, &str)] = match app.agg.as_ref().map(|a| a.focus) {
        Some(AggFocus::Editor) => &[
            ("type", "edit pipeline"),
            ("\u{21e7}\u{21b5}/^r", "run all"),
            ("esc", "stages"),
        ],
        Some(AggFocus::Stages) => &[
            ("↑↓", "stage"),
            ("↵", "run to stage"),
            ("^r", "run all"),
            ("e", "edit"),
            ("tab", "results"),
            ("esc", "back"),
        ],
        _ => &[
            ("↑↓/jk", "move"),
            ("↵", "fold"),
            ("g", "chart"),
            ("t", "chart kind"),
            ("y", "copy"),
            ("esc", "stages"),
        ],
    };
    let mut spans = Vec::new();
    for (k, d) in entries {
        spans.push(Span::styled(
            format!(" {k} "),
            Style::new().fg(theme::badge_fg()).bg(theme::border_dim()),
        ));
        spans.push(Span::styled(
            format!(" {d}  "),
            Style::new().fg(theme::dim()),
        ));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), help);

    let Some(agg) = &mut app.agg else { return };
    let spin = SPINNER[app.spinner_frame % SPINNER.len()];

    let [stages_a, right] =
        Layout::horizontal([Constraint::Length(26), Constraint::Min(30)]).areas(main);
    let [editor_a, results_a] =
        Layout::vertical([Constraint::Percentage(50), Constraint::Min(5)]).areas(right);
    agg.stages_area = stages_a;
    agg.editor_area = editor_a;
    agg.results_area = results_a;

    draw_agg_stages(f, agg, stages_a);
    draw_agg_editor(f, agg, editor_a);
    draw_agg_results(f, agg, results_a, spin);
}

/// Bar chart of {_id, number}-shaped aggregation results (e.g. $group +
/// $sum) — Compass-charts energy, zero extra dependencies.
fn draw_agg_chart(f: &mut Frame, agg: &AggState, inner: Rect) {
    match agg.effective_chart_kind() {
        ChartKind::Line => return draw_agg_xy(f, agg, inner, GraphType::Line),
        ChartKind::Scatter => return draw_agg_xy(f, agg, inner, GraphType::Scatter),
        _ => {}
    }
    let Some(data) = agg.chart_data() else {
        f.render_widget(
            Paragraph::new(Text::from(vec![
                Line::raw(""),
                Line::from(Span::styled(
                    "  These results are not chart-shaped.",
                    Style::new().fg(theme::warn()),
                )),
                Line::from(Span::styled(
                    "  Charts need { _id, <number> } docs — e.g. a $group with $sum/$count.",
                    Style::new().fg(theme::dim()),
                )),
            ])),
            inner,
        );
        return;
    };
    // Fit bars to the width; labels are truncated to the bar width.
    let n = data.len() as u16;
    let gap = 1u16;
    let bar_width = ((inner.width.saturating_sub(n * gap)) / n.max(1)).clamp(3, 14);
    let visible = (inner.width / (bar_width + gap)).max(1) as usize;
    let shown: Vec<(String, u64)> = data
        .into_iter()
        .take(visible)
        .map(|(label, v)| {
            let short: String = label.chars().take(bar_width as usize).collect();
            (short, v)
        })
        .collect();
    let refs: Vec<(&str, u64)> = shown.iter().map(|(l, v)| (l.as_str(), *v)).collect();
    let chart = BarChart::default()
        .data(&refs)
        .bar_width(bar_width)
        .bar_gap(gap)
        .bar_style(Style::new().fg(theme::accent()))
        .value_style(
            Style::new()
                .fg(theme::badge_fg())
                .bg(theme::accent())
                .add_modifier(Modifier::BOLD),
        )
        .label_style(Style::new().fg(theme::key()));
    f.render_widget(chart, inner);
}

/// Format an X coordinate for axis labels.
fn fmt_x(x: f64, kind: XKind) -> String {
    match kind {
        XKind::Number => {
            if x.fract() == 0.0 && x.abs() < 1e15 {
                format!("{}", x as i64)
            } else {
                format!("{x:.2}")
            }
        }
        XKind::Date => lazymongo_core::bson::DateTime::from_millis(x as i64)
            .try_to_rfc3339_string()
            .map(|s| {
                // "2026-08-24T15:04:05Z" -> "08-24 15:04"
                s.get(5..16).map(|t| t.replace('T', " ")).unwrap_or(s)
            })
            .unwrap_or_else(|_| format!("{x}")),
    }
}

/// Line / scatter chart over {_id: date|number, value} results.
fn draw_agg_xy(f: &mut Frame, agg: &AggState, inner: Rect, graph: GraphType) {
    let Some((points, xkind)) = agg.xy_series() else {
        f.render_widget(
            Paragraph::new(Text::from(vec![
                Line::raw(""),
                Line::from(Span::styled(
                    "  Line/scatter need date or numeric _id values.",
                    Style::new().fg(theme::warn()),
                )),
                Line::from(Span::styled(
                    "  Group by a date/number (e.g. $dateTrunc) — or press t for bars.",
                    Style::new().fg(theme::dim()),
                )),
            ])),
            inner,
        );
        return;
    };
    let (mut x_min, mut x_max) = (points[0].0, points[points.len() - 1].0);
    let mut y_min = points.iter().map(|p| p.1).fold(f64::INFINITY, f64::min);
    let mut y_max = points.iter().map(|p| p.1).fold(f64::NEG_INFINITY, f64::max);
    // Pad degenerate bounds so single points / flat lines still render.
    if x_min == x_max {
        x_min -= 1.0;
        x_max += 1.0;
    }
    if y_min == y_max {
        y_min -= 1.0;
        y_max += 1.0;
    }
    y_min = y_min.min(0.0);
    y_max += (y_max - y_min) * 0.05;

    let x_mid = (x_min + x_max) / 2.0;
    let y_mid = (y_min + y_max) / 2.0;
    let axis_style = Style::new().fg(theme::dim());
    let label = |t: String| Span::styled(t, Style::new().fg(theme::dim()));
    let dataset = Dataset::default()
        .marker(symbols::Marker::Braille)
        .graph_type(graph)
        .style(Style::new().fg(theme::accent()))
        .data(&points);
    let chart = Chart::new(vec![dataset])
        .x_axis(
            Axis::default()
                .style(axis_style)
                .bounds([x_min, x_max])
                .labels(vec![
                    label(fmt_x(x_min, xkind)),
                    label(fmt_x(x_mid, xkind)),
                    label(fmt_x(x_max, xkind)),
                ]),
        )
        .y_axis(
            Axis::default()
                .style(axis_style)
                .bounds([y_min, y_max])
                .labels(vec![
                    label(format!("{y_min:.0}")),
                    label(format!("{y_mid:.0}")),
                    label(format!("{y_max:.0}")),
                ]),
        );
    f.render_widget(chart, inner);
}

fn draw_agg_stages(f: &mut Frame, agg: &AggState, area: Rect) {
    let focused = agg.focus == AggFocus::Stages;
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(focused_style(focused))
        .title(format!(" Stages ─ {}.{} ", agg.db, agg.coll));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let mut lines: Vec<Line> = Vec::new();
    for (i, name) in agg.stages.iter().enumerate() {
        let ran = agg.ran_through.is_some_and(|r| i <= r);
        let marker = if ran { "● " } else { "○ " };
        let mut line = Line::from(vec![
            Span::styled(
                format!(" {marker}"),
                Style::new().fg(if ran { theme::ok() } else { theme::dim() }),
            ),
            Span::styled(format!("{} {name}", i + 1), Style::new().fg(theme::key())),
        ]);
        if i == agg.selected_stage {
            line.style = Style::new().bg(if focused {
                theme::sel_bg()
            } else {
                theme::sel_bg_dim()
            });
        }
        lines.push(line);
    }
    if agg.stages.is_empty() {
        lines.push(Line::from(Span::styled(
            " (invalid pipeline)",
            Style::new().fg(theme::error()),
        )));
    }
    f.render_widget(Paragraph::new(Text::from(lines)), inner);
}

fn draw_agg_editor(f: &mut Frame, agg: &mut AggState, area: Rect) {
    let focused = agg.focus == AggFocus::Editor;
    let title = match &agg.error {
        Some(e) => Line::from(vec![
            Span::raw(" Pipeline ─ "),
            Span::styled(e.clone(), Style::new().fg(theme::error())),
            Span::raw(" "),
        ]),
        None => Line::raw(" Pipeline (json5 · \u{21e7}\u{21b5}/^r run) "),
    };
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(focused_style(focused))
        .title(title);
    let inner = block.inner(area);
    f.render_widget(block, area);
    agg.editor.render(f, inner, focused);
}

fn draw_agg_results(f: &mut Frame, agg: &mut AggState, area: Rect, spin: &str) {
    let focused = agg.focus == AggFocus::Results;
    let mut title = match agg.ran_through {
        Some(r) => format!(
            " Preview{} ─ through stage {} ({} docs{}) ─ g {}{} ",
            if agg.chart {
                format!(" [chart:{}]", agg.effective_chart_kind().label())
            } else {
                String::new()
            },
            r + 1,
            agg.docs.len(),
            if agg.docs.len() >= crate::agg::AGG_PREVIEW_LIMIT {
                ", capped"
            } else {
                ""
            },
            if agg.chart { "json" } else { "chart" },
            if agg.chart { " · t kind" } else { "" },
        ),
        None => " Preview ─ run the pipeline (↵ on a stage / ^r) ".to_string(),
    };
    if agg.running {
        title.push_str(spin);
        title.push(' ');
    }
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(focused_style(focused))
        .title(title);
    let inner = block.inner(area);
    f.render_widget(block, area);

    if agg.chart && agg.ran_through.is_some() {
        draw_agg_chart(f, agg, inner);
        return;
    }

    let height = inner.height as usize;
    let len = agg.lines.len();
    if agg.cursor < agg.scroll {
        agg.scroll = agg.cursor;
    } else if height > 0 && agg.cursor >= agg.scroll + height {
        agg.scroll = agg.cursor - height + 1;
    }
    agg.scroll = agg.scroll.min(len.saturating_sub(1));

    let mut lines: Vec<Line> = Vec::with_capacity(height);
    for (i, rline) in agg.lines.iter().enumerate().skip(agg.scroll).take(height) {
        let mut line = rline.line.clone();
        if focused && i == agg.cursor {
            line.style = Style::new().bg(theme::sel_bg());
        }
        lines.push(line);
    }
    if len == 0 && !agg.running && agg.ran_through.is_some() {
        lines.push(Line::from(Span::styled(
            "  no documents produced",
            Style::new().fg(theme::dim()),
        )));
    }
    f.render_widget(Paragraph::new(Text::from(lines)), inner);
}

fn draw_confirm(f: &mut Frame, area: Rect, confirm: &Confirm) {
    let h = (confirm.body.len() as u16 + 5).max(8);
    let popup = centered(area, 64, h);
    f.render_widget(Clear, popup);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(theme::error()))
        .title(Line::from(Span::styled(
            format!(" {} ", confirm.title),
            Style::new().fg(theme::error()).add_modifier(Modifier::BOLD),
        )));
    let inner = block.inner(popup);
    f.render_widget(block, popup);

    let mut lines: Vec<Line> = confirm
        .body
        .iter()
        .map(|s| {
            Line::from(Span::styled(
                format!(" {s}"),
                Style::new().fg(theme::text()),
            ))
        })
        .collect();
    lines.push(Line::raw(""));
    if confirm.typed_required.is_some() {
        let mut spans = vec![Span::styled(" > ", Style::new().fg(theme::warn()))];
        spans.extend(confirm.typed.spans(true, inner.width.saturating_sub(4)));
        lines.push(Line::from(spans));
        let ok = confirm.typed_ok();
        lines.push(Line::from(Span::styled(
            if ok {
                " ↵ confirm · esc cancel"
            } else {
                " esc cancel"
            },
            Style::new().fg(if ok { theme::ok() } else { theme::dim() }),
        )));
    } else {
        lines.push(Line::from(Span::styled(
            " y/↵ confirm · n/esc cancel",
            Style::new().fg(theme::dim()),
        )));
    }
    f.render_widget(Paragraph::new(Text::from(lines)), inner);
}

fn draw_json_editor(f: &mut Frame, area: Rect, editor: &mut JsonEditor) {
    let popup = centered(area, 84, area.height.saturating_sub(6));
    f.render_widget(Clear, popup);
    let title = format!(" {} ─ ^s/\u{21e7}\u{21b5} save · esc cancel ", editor.title);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(theme::accent()))
        .title(title);
    let mut inner = block.inner(popup);
    f.render_widget(block, popup);

    if let Some(e) = &editor.error {
        let err_area = Rect { height: 1, ..inner };
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                format!(" ✗ {e}"),
                Style::new().fg(theme::badge_fg()).bg(theme::error()),
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
        .border_style(Style::new().fg(theme::accent()))
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
            Style::new().fg(theme::warn()),
        ))),
        Some(list) if list.is_empty() => lines.push(Line::from(Span::styled(
            " no indexes",
            Style::new().fg(theme::dim()),
        ))),
        Some(list) => {
            for (i, idx) in list.iter().enumerate() {
                let unique = if idx.unique { "  [unique]" } else { "" };
                let keys_json = lazymongo_core::bson::Bson::Document(idx.keys.clone())
                    .into_relaxed_extjson()
                    .to_string();
                let mut line = Line::from(vec![
                    Span::styled(format!(" {:<24}", idx.name), Style::new().fg(theme::key())),
                    Span::styled(keys_json, Style::new().fg(theme::text())),
                    Span::styled(unique, Style::new().fg(theme::warn())),
                ]);
                if i == view.selected {
                    line.style = Style::new().bg(theme::sel_bg());
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
        .border_style(Style::new().fg(theme::accent()))
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
            Style::new().fg(theme::dim()),
        )));
    }
    for entry in log.iter().skip(*scroll).take(inner.height as usize) {
        lines.push(Line::from(Span::styled(
            format!(" {entry}"),
            Style::new().fg(theme::text()),
        )));
    }
    // Wrap so long driver errors are fully readable.
    f.render_widget(
        Paragraph::new(Text::from(lines)).wrap(ratatui::widgets::Wrap { trim: false }),
        inner,
    );
}

fn draw_prompt(f: &mut Frame, area: Rect, prompt: &Prompt) {
    let popup = centered(area, 54, 5);
    f.render_widget(Clear, popup);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(theme::accent()))
        .title(format!(" {} ─ ↵ ok · esc cancel ", prompt.title));
    let inner = block.inner(popup);
    f.render_widget(block, popup);
    let mut spans = vec![Span::styled(" > ", Style::new().fg(theme::warn()))];
    spans.extend(prompt.input.spans(true, inner.width.saturating_sub(4)));
    f.render_widget(Paragraph::new(Line::from(spans)), inner);
}

fn draw_query_editor(f: &mut Frame, area: Rect, editor: &QueryEditor) {
    let popup = centered(area, 72, 11);
    f.render_widget(Clear, popup);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(theme::accent()))
        .title(" Query editor ─ ↵ run · esc cancel ");
    let inner = block.inner(popup);
    f.render_widget(block, popup);

    let mut lines: Vec<Line> = Vec::new();
    for (i, field) in editor.fields.iter().enumerate() {
        let focused = i == editor.focus;
        let label_style = if focused {
            Style::new()
                .fg(theme::accent())
                .add_modifier(Modifier::BOLD)
        } else {
            Style::new().fg(theme::key())
        };
        let mut spans = vec![Span::styled(
            format!(" {:<11}", QUERY_FIELD_LABELS[i]),
            label_style,
        )];
        spans.extend(field.spans(focused, inner.width.saturating_sub(12)));
        lines.push(Line::from(spans));
        lines.push(Line::raw(""));
    }
    if let Some(e) = &editor.error {
        lines.push(Line::from(Span::styled(
            format!(" {e}"),
            Style::new().fg(theme::error()),
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
        .border_style(Style::new().fg(theme::accent()))
        .title(format!(" {} ", view.title));
    let mut inner = block.inner(popup);
    f.render_widget(block, popup);

    if let Some(warn) = &view.warn {
        let warn_area = Rect { height: 1, ..inner };
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                format!(" ⚠ {warn}"),
                Style::new().fg(theme::badge_fg()).bg(theme::error()),
            ))),
            warn_area,
        );
        inner.y += 1;
        inner.height = inner.height.saturating_sub(1);
    }
    view.inner = inner;

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
            line.style = Style::new().bg(theme::sel_bg());
        }
        lines.push(line);
    }
    f.render_widget(Paragraph::new(Text::from(lines)), inner);
}

fn draw_help_overlay(f: &mut Frame, area: Rect) {
    let key = |k: &str| Span::styled(format!("  {k:<14}"), Style::new().fg(theme::key()));
    let txt = |t: &str| Span::styled(t.to_string(), Style::new().fg(theme::text()));
    let head = |t: &str| {
        Line::from(Span::styled(
            format!(" {t}"),
            Style::new()
                .fg(theme::accent())
                .add_modifier(Modifier::BOLD),
        ))
    };
    let lines = vec![
        head("Global"),
        Line::from(vec![key("q / ctrl-c"), txt("quit")]),
        Line::from(vec![key("tab / 1 2 3"), txt("switch pane")]),
        Line::from(vec![
            key("r"),
            txt("refresh: reload collection (+ sidebar)"),
        ]),
        Line::from(vec![key("C"), txt("connection manager (add/edit/switch)")]),
        Line::from(vec![key("^p / :"), txt("command palette (incl. themes)")]),
        Line::from(vec![key("^t"), txt("open collection by name (fuzzy)")]),
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
        Line::from(vec![key("y"), txt("copy whole document as JSON")]),
        Line::from(vec![key("Y"), txt("copy node under cursor as JSON")]),
        Line::from(vec![
            key("E"),
            txt("export FULL query to file (json / csv, streamed)"),
        ]),
        Line::from(vec![
            key("a"),
            txt("aggregation editor (g toggles bar chart)"),
        ]),
        Line::from(vec![
            key("S"),
            txt("schema: sampled field presence & types"),
        ]),
        Line::from(vec![key("m"), txt("open mongosh on this connection")]),
        Line::from(vec![
            key("/ · n N"),
            txt("search loaded results · next/prev match"),
        ]),
        Line::from(vec![key("esc"), txt("cancel a running query")]),
        Line::raw(""),
        head("Writes (blocked in read-only mode)"),
        Line::from(vec![key("e / ^e"), txt("edit document (in-app / $EDITOR)")]),
        Line::from(vec![
            key("i / c"),
            txt("insert document · duplicate selected"),
        ]),
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
        Line::raw(""),
        Line::from(Span::styled(
            " Mouse: click selects · click again copies the field (or folds).",
            Style::new().fg(theme::dim()),
        )),
        Line::from(Span::styled(
            " Right-click copies the clicked node (doc/object/value) as JSON.",
            Style::new().fg(theme::dim()),
        )),
        Line::from(Span::styled(
            " Native text selection: hold Option/Alt (or Shift) while dragging.",
            Style::new().fg(theme::dim()),
        )),
    ];
    // Size the popup to the content so no rows are clipped off.
    let popup = centered(area, 66, lines.len() as u16 + 2);
    f.render_widget(Clear, popup);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(theme::accent()))
        .title(" Keybindings (any key closes) ");
    f.render_widget(Paragraph::new(Text::from(lines)).block(block), popup);
}
