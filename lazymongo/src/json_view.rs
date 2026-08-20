//! Renders BSON documents as foldable, syntax-highlighted lines.
//!
//! Each produced [`RLine`] carries the document index and the fold path it
//! belongs to, so the app can toggle folds from either a key press or a
//! mouse click on the line.

use std::collections::HashSet;

use lazymongo_core::bson::{Bson, Document};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};

/// One rendered line of the results pane.
pub struct RLine {
    pub doc_idx: usize,
    /// Fold path this line toggles ("" = whole document). None = not foldable.
    pub fold_path: Option<String>,
    pub line: Line<'static>,
}

const INDENT: &str = "  ";
const MAX_SUMMARY_FIELDS: usize = 4;
const MAX_INLINE_STR: usize = 32;

fn key_style() -> Style {
    Style::new().fg(Color::Cyan)
}
fn punct_style() -> Style {
    Style::new().fg(Color::DarkGray)
}
fn marker_style() -> Style {
    Style::new().fg(Color::Yellow)
}

fn value_span(v: &Bson) -> Span<'static> {
    match v {
        Bson::String(s) => {
            let mut s = s.clone();
            if s.chars().count() > MAX_INLINE_STR {
                s = s.chars().take(MAX_INLINE_STR).collect::<String>() + "…";
            }
            Span::styled(format!("\"{s}\""), Style::new().fg(Color::Green))
        }
        Bson::Int32(n) => Span::styled(n.to_string(), Style::new().fg(Color::Yellow)),
        Bson::Int64(n) => Span::styled(n.to_string(), Style::new().fg(Color::Yellow)),
        Bson::Double(n) => Span::styled(n.to_string(), Style::new().fg(Color::Yellow)),
        Bson::Decimal128(n) => Span::styled(n.to_string(), Style::new().fg(Color::Yellow)),
        Bson::Boolean(b) => Span::styled(b.to_string(), Style::new().fg(Color::Magenta)),
        Bson::Null => Span::styled("null", Style::new().fg(Color::Magenta)),
        Bson::ObjectId(oid) => Span::styled(
            format!("ObjectId(\"{oid}\")"),
            Style::new().fg(Color::LightMagenta),
        ),
        Bson::DateTime(dt) => Span::styled(
            dt.try_to_rfc3339_string()
                .unwrap_or_else(|_| format!("{dt}")),
            Style::new().fg(Color::Blue),
        ),
        Bson::Binary(b) => Span::styled(
            format!("Binary({:?}, {} bytes)", b.subtype, b.bytes.len()),
            Style::new().fg(Color::DarkGray),
        ),
        Bson::RegularExpression(r) => Span::styled(
            format!("/{}/{}", r.pattern, r.options),
            Style::new().fg(Color::Red),
        ),
        Bson::Timestamp(t) => Span::styled(
            format!("Timestamp({}, {})", t.time, t.increment),
            Style::new().fg(Color::Blue),
        ),
        other => Span::styled(format!("{other}"), Style::new().fg(Color::White)),
    }
}

/// Short inline form used in collapsed summaries.
fn short_value(v: &Bson) -> Span<'static> {
    match v {
        Bson::Document(d) => Span::styled(format!("{{…{}}}", d.len()), punct_style()),
        Bson::Array(a) => Span::styled(format!("[…{}]", a.len()), punct_style()),
        _ => value_span(v),
    }
}

/// Render one document into lines. `number` is the absolute 1-based document
/// number (survives window eviction). `folds` holds collapsed paths.
pub fn doc_lines(
    doc_idx: usize,
    number: u64,
    doc: &Document,
    folds: &HashSet<String>,
) -> Vec<RLine> {
    let mut out = Vec::new();
    let header_num = Span::styled(format!("[{number}] "), Style::new().fg(Color::DarkGray));

    if folds.contains("") {
        // Collapsed card: ▸ [n] { _id: …, name: "…", … }
        let mut spans = vec![
            Span::styled("▸ ", marker_style()),
            header_num,
            Span::styled("{ ", punct_style()),
        ];
        for (i, (k, v)) in doc.iter().take(MAX_SUMMARY_FIELDS).enumerate() {
            if i > 0 {
                spans.push(Span::styled(", ", punct_style()));
            }
            spans.push(Span::styled(k.clone(), key_style()));
            spans.push(Span::styled(": ", punct_style()));
            spans.push(short_value(v));
        }
        if doc.len() > MAX_SUMMARY_FIELDS {
            spans.push(Span::styled(", …", punct_style()));
        }
        spans.push(Span::styled(" }", punct_style()));
        out.push(RLine {
            doc_idx,
            fold_path: Some(String::new()),
            line: Line::from(spans),
        });
        return out;
    }

    out.push(RLine {
        doc_idx,
        fold_path: Some(String::new()),
        line: Line::from(vec![
            Span::styled("▾ ", marker_style()),
            header_num,
            Span::styled("{", punct_style()),
        ]),
    });
    for (k, v) in doc.iter() {
        render_entry(doc_idx, &mut out, folds, k, v, k.to_string(), 1);
    }
    out.push(RLine {
        doc_idx,
        fold_path: None,
        line: Line::from(Span::styled("}", punct_style())),
    });
    out
}

#[allow(clippy::too_many_arguments)]
fn render_entry(
    doc_idx: usize,
    out: &mut Vec<RLine>,
    folds: &HashSet<String>,
    key: &str,
    value: &Bson,
    path: String,
    depth: usize,
) {
    let pad = INDENT.repeat(depth);
    match value {
        Bson::Document(d) => {
            if folds.contains(&path) {
                out.push(RLine {
                    doc_idx,
                    fold_path: Some(path),
                    line: Line::from(vec![
                        Span::raw(pad),
                        Span::styled("▸ ", marker_style()),
                        Span::styled(key.to_string(), key_style()),
                        Span::styled(": ", punct_style()),
                        Span::styled(format!("{{…{}}}", d.len()), punct_style()),
                    ]),
                });
            } else {
                out.push(RLine {
                    doc_idx,
                    fold_path: Some(path.clone()),
                    line: Line::from(vec![
                        Span::raw(pad.clone()),
                        Span::styled("▾ ", marker_style()),
                        Span::styled(key.to_string(), key_style()),
                        Span::styled(": {", punct_style()),
                    ]),
                });
                for (k, v) in d.iter() {
                    render_entry(doc_idx, out, folds, k, v, format!("{path}.{k}"), depth + 1);
                }
                out.push(RLine {
                    doc_idx,
                    fold_path: None,
                    line: Line::from(vec![Span::raw(pad), Span::styled("}", punct_style())]),
                });
            }
        }
        Bson::Array(items) => {
            if folds.contains(&path) {
                out.push(RLine {
                    doc_idx,
                    fold_path: Some(path),
                    line: Line::from(vec![
                        Span::raw(pad),
                        Span::styled("▸ ", marker_style()),
                        Span::styled(key.to_string(), key_style()),
                        Span::styled(": ", punct_style()),
                        Span::styled(format!("[…{}]", items.len()), punct_style()),
                    ]),
                });
            } else {
                out.push(RLine {
                    doc_idx,
                    fold_path: Some(path.clone()),
                    line: Line::from(vec![
                        Span::raw(pad.clone()),
                        Span::styled("▾ ", marker_style()),
                        Span::styled(key.to_string(), key_style()),
                        Span::styled(": [", punct_style()),
                    ]),
                });
                for (i, v) in items.iter().enumerate() {
                    render_entry(
                        doc_idx,
                        out,
                        folds,
                        &i.to_string(),
                        v,
                        format!("{path}.{i}"),
                        depth + 1,
                    );
                }
                out.push(RLine {
                    doc_idx,
                    fold_path: None,
                    line: Line::from(vec![Span::raw(pad), Span::styled("]", punct_style())]),
                });
            }
        }
        scalar => {
            out.push(RLine {
                doc_idx,
                fold_path: None,
                line: Line::from(vec![
                    Span::raw(format!("{pad}  ")), // align with foldable markers
                    Span::styled(key.to_string(), key_style()),
                    Span::styled(": ", punct_style()),
                    value_span(scalar),
                ]),
            });
        }
    }
}
