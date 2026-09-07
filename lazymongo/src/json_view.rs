//! Renders BSON documents as foldable, syntax-highlighted lines.
//!
//! Each produced [`RLine`] carries the document index and the fold path it
//! belongs to, so the app can toggle folds from either a key press or a
//! mouse click on the line.

use std::collections::HashSet;

use lazymongo_core::bson::{Bson, Document};
use ratatui::style::Style;

use crate::theme;
use ratatui::text::{Line, Span};

/// One rendered line of the results pane.
pub struct RLine {
    pub doc_idx: usize,
    /// Fold path this line toggles ("" = whole document). None = not foldable.
    pub fold_path: Option<String>,
    /// Plain, untruncated value for scalar lines — what a click-to-copy
    /// puts on the clipboard.
    pub copy_text: Option<String>,
    pub line: Line<'static>,
}

/// The paste-friendly plain form of a scalar (full string, oid hex, rfc3339
/// date, bare number/bool) — unlike the display form, never truncated.
pub fn copy_value(v: &Bson) -> String {
    match v {
        Bson::String(s) => s.clone(),
        Bson::ObjectId(oid) => oid.to_string(),
        Bson::DateTime(dt) => dt
            .try_to_rfc3339_string()
            .unwrap_or_else(|_| format!("{dt}")),
        Bson::Int32(n) => n.to_string(),
        Bson::Int64(n) => n.to_string(),
        Bson::Double(n) => n.to_string(),
        Bson::Decimal128(n) => n.to_string(),
        Bson::Boolean(b) => b.to_string(),
        Bson::Null => "null".into(),
        other => format!("{other}"),
    }
}

const INDENT: &str = "  ";
const MAX_SUMMARY_FIELDS: usize = 4;
const MAX_INLINE_STR: usize = 32;

fn key_style() -> Style {
    Style::new().fg(theme::key())
}
fn punct_style() -> Style {
    Style::new().fg(theme::dim())
}
fn marker_style() -> Style {
    Style::new().fg(theme::marker())
}

fn value_span(v: &Bson) -> Span<'static> {
    match v {
        Bson::String(s) => {
            let mut s = s.clone();
            if s.chars().count() > MAX_INLINE_STR {
                s = s.chars().take(MAX_INLINE_STR).collect::<String>() + "…";
            }
            Span::styled(format!("\"{s}\""), Style::new().fg(theme::string()))
        }
        Bson::Int32(n) => Span::styled(n.to_string(), Style::new().fg(theme::number())),
        Bson::Int64(n) => Span::styled(n.to_string(), Style::new().fg(theme::number())),
        Bson::Double(n) => Span::styled(n.to_string(), Style::new().fg(theme::number())),
        Bson::Decimal128(n) => Span::styled(n.to_string(), Style::new().fg(theme::number())),
        Bson::Boolean(b) => Span::styled(b.to_string(), Style::new().fg(theme::keyword())),
        Bson::Null => Span::styled("null", Style::new().fg(theme::keyword())),
        Bson::ObjectId(oid) => Span::styled(
            format!("ObjectId(\"{oid}\")"),
            Style::new().fg(theme::object_id()),
        ),
        Bson::DateTime(dt) => Span::styled(
            dt.try_to_rfc3339_string()
                .unwrap_or_else(|_| format!("{dt}")),
            Style::new().fg(theme::date()),
        ),
        Bson::Binary(b) => Span::styled(
            format!("Binary({:?}, {} bytes)", b.subtype, b.bytes.len()),
            Style::new().fg(theme::dim()),
        ),
        Bson::RegularExpression(r) => Span::styled(
            format!("/{}/{}", r.pattern, r.options),
            Style::new().fg(theme::error()),
        ),
        Bson::Timestamp(t) => Span::styled(
            format!("Timestamp({}, {})", t.time, t.increment),
            Style::new().fg(theme::date()),
        ),
        other => Span::styled(format!("{other}"), Style::new().fg(theme::text())),
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

/// Fold paths of every nested container (document or array) in `doc`,
/// excluding the root path `""` — i.e. everything `collapse all` should fold
/// while keeping the document's first level visible.
pub fn foldable_paths(doc: &Document) -> HashSet<String> {
    fn walk(v: &Bson, path: &str, out: &mut HashSet<String>) {
        match v {
            Bson::Document(d) => {
                out.insert(path.to_string());
                for (k, v) in d.iter() {
                    walk(v, &format!("{path}.{k}"), out);
                }
            }
            Bson::Array(items) => {
                out.insert(path.to_string());
                for (i, v) in items.iter().enumerate() {
                    walk(v, &format!("{path}.{i}"), out);
                }
            }
            _ => {}
        }
    }
    let mut out = HashSet::new();
    for (k, v) in doc.iter() {
        walk(v, k, &mut out);
    }
    out
}

/// Toggle between fully expanded and all-inner-containers collapsed:
/// any existing fold (including a collapsed whole doc) -> expand everything;
/// fully expanded -> collapse every nested object/array.
pub fn toggle_all_folds(doc: &Document, folds: &mut HashSet<String>) {
    if folds.is_empty() {
        *folds = foldable_paths(doc);
    } else {
        folds.clear();
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
    let header_num = Span::styled(format!("[{number}] "), Style::new().fg(theme::dim()));

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
            copy_text: None,
            line: Line::from(spans),
        });
        return out;
    }

    out.push(RLine {
        doc_idx,
        fold_path: Some(String::new()),
        copy_text: None,
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
        copy_text: None,
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
                    copy_text: None,
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
                    copy_text: None,
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
                    copy_text: None,
                    line: Line::from(vec![Span::raw(pad), Span::styled("}", punct_style())]),
                });
            }
        }
        Bson::Array(items) => {
            if folds.contains(&path) {
                out.push(RLine {
                    doc_idx,
                    fold_path: Some(path),
                    copy_text: None,
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
                    copy_text: None,
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
                    copy_text: None,
                    line: Line::from(vec![Span::raw(pad), Span::styled("]", punct_style())]),
                });
            }
        }
        scalar => {
            out.push(RLine {
                doc_idx,
                fold_path: None,
                copy_text: Some(copy_value(scalar)),
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

#[cfg(test)]
mod tests {
    use super::*;
    use lazymongo_core::bson::doc;

    #[test]
    fn foldable_paths_nested() {
        let d = doc! {
            "a": 1,
            "b": { "c": { "d": 2 }, "e": [1, { "f": 3 }] },
        };
        let paths = foldable_paths(&d);
        let mut got: Vec<&str> = paths.iter().map(String::as_str).collect();
        got.sort_unstable();
        assert_eq!(got, ["b", "b.c", "b.e", "b.e.1"]);
    }

    #[test]
    fn toggle_all_folds_round_trip() {
        let d = doc! { "b": { "c": 1 } };
        let mut folds = HashSet::new();
        toggle_all_folds(&d, &mut folds); // expanded -> all collapsed
        assert!(folds.contains("b"));
        toggle_all_folds(&d, &mut folds); // any folds -> all expanded
        assert!(folds.is_empty());
        // A whole-doc collapse also expands on toggle.
        folds.insert(String::new());
        toggle_all_folds(&d, &mut folds);
        assert!(folds.is_empty());
    }
}
