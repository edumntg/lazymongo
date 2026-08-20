//! Clipboard, export, and BSON <-> pretty-JSON helpers.

use std::io::Write as _;
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use lazymongo_core::bson::{Bson, Document};

/// Pretty relaxed Extended JSON for display, copying, and editing.
/// Relaxed keeps `$oid`/`$date` wrappers (round-trippable) but renders
/// numbers plainly, which is what humans want to edit.
pub fn doc_to_pretty(doc: &Document) -> String {
    let value = Bson::Document(doc.clone()).into_relaxed_extjson();
    serde_json::to_string_pretty(&value).unwrap_or_else(|_| format!("{doc:?}"))
}

/// Compact single-line form for CSV cells and summaries.
pub fn bson_to_compact(v: &Bson) -> String {
    match v {
        Bson::String(s) => s.clone(),
        Bson::ObjectId(oid) => oid.to_string(),
        Bson::DateTime(dt) => dt
            .try_to_rfc3339_string()
            .unwrap_or_else(|_| format!("{dt}")),
        Bson::Document(d) => format!("{{…{}}}", d.len()),
        Bson::Array(a) => format!("[…{}]", a.len()),
        Bson::Null => String::new(),
        Bson::Binary(b) => format!("Binary({} bytes)", b.bytes.len()),
        other => other.to_string(),
    }
}

/// Copy text to the system clipboard by shelling out (no heavy deps).
pub fn clipboard_copy(text: &str) -> Result<(), String> {
    let candidates: &[(&str, &[&str])] = if cfg!(target_os = "macos") {
        &[("pbcopy", &[])]
    } else if cfg!(target_os = "windows") {
        &[("clip", &[])]
    } else {
        &[
            ("wl-copy", &[]),
            ("xclip", &["-selection", "clipboard"]),
            ("xsel", &["--clipboard", "--input"]),
        ]
    };
    for (bin, args) in candidates {
        let child = Command::new(bin)
            .args(*args)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
        if let Ok(mut child) = child {
            if let Some(stdin) = child.stdin.as_mut() {
                if stdin.write_all(text.as_bytes()).is_ok() {
                    let _ = child.wait();
                    return Ok(());
                }
            }
            let _ = child.wait();
        }
    }
    Err("no clipboard tool found (pbcopy/wl-copy/xclip/xsel/clip)".into())
}

fn timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Session clock "hh:mm:ss" (UTC) for the ops log.
#[allow(dead_code)] // wired up by the write-ops log (M3)
pub fn clock_utc() -> String {
    let s = timestamp();
    format!("{:02}:{:02}:{:02}", (s / 3600) % 24, (s / 60) % 60, s % 60)
}

/// Export loaded docs as a pretty JSON array. Returns the file path.
pub fn export_json(db: &str, coll: &str, docs: &[Document]) -> Result<String, String> {
    let path = format!("lazymongo-{db}.{coll}-{}.json", timestamp());
    let values: Vec<serde_json::Value> = docs
        .iter()
        .map(|d| Bson::Document(d.clone()).into_relaxed_extjson())
        .collect();
    let body = serde_json::to_string_pretty(&values).map_err(|e| e.to_string())?;
    std::fs::write(&path, body).map_err(|e| e.to_string())?;
    Ok(path)
}

/// Export loaded docs as CSV over the given columns. Returns the file path.
pub fn export_csv(
    db: &str,
    coll: &str,
    columns: &[String],
    docs: &[Document],
) -> Result<String, String> {
    let path = format!("lazymongo-{db}.{coll}-{}.csv", timestamp());
    let mut out = String::new();
    out.push_str(
        &columns
            .iter()
            .map(|c| csv_escape(c))
            .collect::<Vec<_>>()
            .join(","),
    );
    out.push('\n');
    for doc in docs {
        let row: Vec<String> = columns
            .iter()
            .map(|c| doc.get(c).map(bson_to_compact).unwrap_or_default())
            .map(|s| csv_escape(&s))
            .collect();
        out.push_str(&row.join(","));
        out.push('\n');
    }
    std::fs::write(&path, out).map_err(|e| e.to_string())?;
    Ok(path)
}

fn csv_escape(s: &str) -> String {
    if s.contains([',', '"', '\n']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

/// Recursively search an explain plan for a COLLSCAN stage (FR-15).
pub fn has_collscan(doc: &Document) -> bool {
    for (k, v) in doc.iter() {
        match v {
            Bson::String(s) if k == "stage" && s == "COLLSCAN" => return true,
            Bson::Document(d) => {
                if has_collscan(d) {
                    return true;
                }
            }
            Bson::Array(items) => {
                for item in items {
                    if let Bson::Document(d) = item {
                        if has_collscan(d) {
                            return true;
                        }
                    }
                }
            }
            _ => {}
        }
    }
    false
}
