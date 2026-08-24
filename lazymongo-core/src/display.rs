//! Small display/formatting helpers shared by the TUI and the exporter.

use mongodb::bson::Bson;

/// Compact single-line rendering of a BSON value (CSV cells, summaries).
pub fn bson_to_compact(v: &Bson) -> String {
    match v {
        Bson::String(s) => s.clone(),
        Bson::ObjectId(oid) => oid.to_string(),
        Bson::DateTime(dt) => dt
            .try_to_rfc3339_string()
            .unwrap_or_else(|_| format!("{dt}")),
        Bson::Document(d) => format!("{{\u{2026}{}}}", d.len()),
        Bson::Array(a) => format!("[\u{2026}{}]", a.len()),
        Bson::Null => String::new(),
        Bson::Binary(b) => format!("Binary({} bytes)", b.bytes.len()),
        other => other.to_string(),
    }
}

pub fn csv_escape(s: &str) -> String {
    if s.contains([',', '"', '\n']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mongodb::bson::doc;

    #[test]
    fn compact_and_escape() {
        assert_eq!(bson_to_compact(&Bson::Int32(5)), "5");
        assert_eq!(
            bson_to_compact(&Bson::Document(doc! {"a": 1, "b": 2})),
            "{\u{2026}2}"
        );
        assert_eq!(csv_escape("plain"), "plain");
        assert_eq!(csv_escape("a,b"), "\"a,b\"");
        assert_eq!(csv_escape("say \"hi\""), "\"say \"\"hi\"\"\"");
    }
}
