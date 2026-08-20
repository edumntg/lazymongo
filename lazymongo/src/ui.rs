//! Rendering: pure view of the App model, plus per-frame scroll clamping and
//! hit-test rect bookkeeping.

use lazymongo_core::types::CollectionInfo;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, BorderType, Clear, Paragraph};
use ratatui::Frame;

use crate::app::{App, ConnState, ExplorerRow, Pane};

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

    if app.help_open {
        draw_help_overlay(f, f.area());
    }
}

fn spinner(app: &App) -> &'static str {
    SPINNER[app.spinner_frame % SPINNER.len()]
}

fn draw_status(f: &mut Frame, app: &App, area: Rect) {
    let sep = Span::styled("  •  ", Style::new().fg(Color::DarkGray));
    let mut spans = vec![
        Span::styled(
            " lazymongo ",
            Style::new()
                .fg(Color::Black)
                .bg(Color::Green)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
        Span::styled(app.uri_display.clone(), Style::new().fg(Color::White)),
    ];
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

fn draw_results(f: &mut Frame, app: &mut App, area: Rect) {
    let focused = app.focus == Pane::Results;
    let title = match &app.results.target {
        None => " 2 Results ".to_string(),
        Some((db, coll)) => {
            let mut t = format!(" 2 Results ─ {db}.{coll} ");
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
    };
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(focused_style(focused))
        .title(title);
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

fn draw_query(f: &mut Frame, app: &App, area: Rect) {
    let focused = app.focus == Pane::Query;
    let title = match &app.query.error {
        Some(e) => Line::from(vec![
            Span::raw(" 3 Query ─ "),
            Span::styled(e.clone(), Style::new().fg(Color::Red)),
            Span::raw(" "),
        ]),
        None => Line::raw(" 3 Query (find filter) "),
    };
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(focused_style(focused))
        .title(title);
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
    let entries: &[(&str, &str)] = match app.focus {
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
        Pane::Results => &[
            ("↑↓/jk", "move"),
            ("↵", "fold"),
            ("^d/^u", "half-page"),
            ("g/G", "top/end"),
            ("3", "query"),
            ("?", "help"),
            ("q", "quit"),
        ],
        Pane::Query => &[
            ("↵", "run"),
            ("↑↓", "history"),
            ("esc", "back"),
            ("^c", "quit"),
        ],
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

fn draw_help_overlay(f: &mut Frame, area: Rect) {
    let w = 62.min(area.width.saturating_sub(4));
    let h = 24.min(area.height.saturating_sub(2));
    let popup = Rect {
        x: area.x + (area.width - w) / 2,
        y: area.y + (area.height - h) / 2,
        width: w,
        height: h,
    };
    f.render_widget(Clear, popup);

    let key = |k: &str| Span::styled(format!("  {k:<12}"), Style::new().fg(Color::Cyan));
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
        Line::from(vec![key("tab / S-tab"), txt("cycle panes")]),
        Line::from(vec![key("1 2 3"), txt("jump to pane")]),
        Line::from(vec![key("r"), txt("refresh current pane")]),
        Line::from(vec![key("?"), txt("toggle this help")]),
        Line::raw(""),
        head("Explorer"),
        Line::from(vec![key("↑↓ / j k"), txt("move")]),
        Line::from(vec![key("↵ / → / l"), txt("expand db · open collection")]),
        Line::from(vec![key("← / h"), txt("collapse")]),
        Line::from(vec![key("/"), txt("filter databases & collections")]),
        Line::raw(""),
        head("Results"),
        Line::from(vec![key("↑↓ / j k"), txt("move line (auto-loads more)")]),
        Line::from(vec![key("↵ / space"), txt("fold / unfold at cursor")]),
        Line::from(vec![key("^d ^u pgup/dn"), txt("scroll pages")]),
        Line::from(vec![key("g / G"), txt("first / last line")]),
        Line::raw(""),
        head("Query"),
        Line::from(vec![key("↵"), txt("run filter (mongosh syntax ok)")]),
        Line::from(vec![key("↑ / ↓"), txt("history")]),
        Line::raw(""),
        Line::from(Span::styled(
            " Mouse: click to focus/select/open, wheel to scroll.",
            Style::new().fg(Color::DarkGray),
        )),
    ];
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(Color::Cyan))
        .title(" Keybindings (any key closes) ");
    f.render_widget(Paragraph::new(Text::from(lines)).block(block), popup);
}
