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

pub use lazymongo_core::display::bson_to_compact;

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
pub fn clock_utc() -> String {
    let s = timestamp();
    format!("{:02}:{:02}:{:02}", (s / 3600) % 24, (s / 60) % 60, s % 60)
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
