//! Relaxed filter parsing: accepts the same leniency as `mongosh`
//! (unquoted keys, single quotes, trailing commas) via JSON5, then converts
//! through serde_json into BSON so canonical Extended JSON (`$oid`, `$date`,
//! `$numberLong`, ...) round-trips into real BSON types.

use mongodb::bson::{Bson, Document};

/// Parse a user-typed filter string into a BSON document.
/// An empty/blank string means "match everything" (`{}`).
pub fn parse_filter(input: &str) -> Result<Document, String> {
    let s = input.trim();
    if s.is_empty() {
        return Ok(Document::new());
    }
    let s = desugar_mongosh(s);
    let value: serde_json::Value =
        json5::from_str(&s).map_err(|e| format!("invalid filter: {e}"))?;
    let bson = Bson::try_from(value).map_err(|e| format!("invalid filter: {e}"))?;
    match bson {
        Bson::Document(doc) => Ok(doc),
        _ => Err("filter must be a JSON object, e.g. { status: \"active\" }".into()),
    }
}

/// Parse a JSON object (projection, sort, update document, insert doc, ...).
/// Unlike [`parse_filter`], an empty string is an error — use
/// [`parse_optional_doc`] where blank means "not set".
pub fn parse_doc(input: &str) -> Result<Document, String> {
    let s = input.trim();
    if s.is_empty() {
        return Err("expected a JSON object".into());
    }
    parse_filter(s)
}

/// Blank input -> None; otherwise a JSON object.
pub fn parse_optional_doc(input: &str) -> Result<Option<Document>, String> {
    let s = input.trim();
    if s.is_empty() {
        return Ok(None);
    }
    parse_doc(s).map(Some)
}

/// Parse an aggregation pipeline: a JSON5 array of stage objects.
/// A single bare object is accepted as a one-stage pipeline.
pub fn parse_pipeline(input: &str) -> Result<Vec<Document>, String> {
    let s = input.trim();
    if s.is_empty() {
        return Err("pipeline is empty".into());
    }
    let s = desugar_mongosh(s);
    let value: serde_json::Value =
        json5::from_str(&s).map_err(|e| format!("invalid pipeline: {e}"))?;
    let bson = Bson::try_from(value).map_err(|e| format!("invalid pipeline: {e}"))?;
    match bson {
        Bson::Array(items) => {
            let mut stages = Vec::with_capacity(items.len());
            for (i, item) in items.into_iter().enumerate() {
                match item {
                    Bson::Document(d) => stages.push(d),
                    _ => return Err(format!("stage {} is not an object", i + 1)),
                }
            }
            if stages.is_empty() {
                return Err("pipeline is empty".into());
            }
            Ok(stages)
        }
        Bson::Document(d) => Ok(vec![d]),
        _ => Err("pipeline must be an array of stage objects".into()),
    }
}

/// Rewrite mongosh-style constructor calls into canonical Extended JSON so
/// the JSON5 parser accepts filters typed or pasted straight from mongosh:
/// `ObjectId("...")` / `new ObjectId('...')` -> `{"$oid":"..."}`, plus
/// `ISODate`/`new Date`, `NumberLong`, `NumberInt`, `NumberDecimal`, `UUID`.
/// Occurrences inside string literals are left untouched, as is anything
/// that doesn't look like a rewritable call (JSON5 then reports the error).
fn desugar_mongosh(input: &str) -> String {
    let chars: Vec<char> = input.chars().collect();
    let mut out = String::with_capacity(input.len());
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            q @ ('"' | '\'') => {
                out.push(q);
                i += 1;
                while i < chars.len() {
                    let c = chars[i];
                    out.push(c);
                    i += 1;
                    if c == '\\' {
                        if i < chars.len() {
                            out.push(chars[i]);
                            i += 1;
                        }
                    } else if c == q {
                        break;
                    }
                }
            }
            c if is_ident_start(c) => {
                let start = i;
                while i < chars.len() && is_ident_char(chars[i]) {
                    i += 1;
                }
                let ident: String = chars[start..i].iter().collect();
                if ident == "new" {
                    // Peek past `new` for a constructor name; if the call
                    // rewrites, drop the `new`, otherwise keep it verbatim.
                    let mut j = i;
                    while j < chars.len() && chars[j].is_whitespace() {
                        j += 1;
                    }
                    let name_start = j;
                    while j < chars.len() && is_ident_char(chars[j]) {
                        j += 1;
                    }
                    let name: String = chars[name_start..j].iter().collect();
                    if let Some(end) = try_rewrite_call(&chars, j, &name, &mut out) {
                        i = end;
                        continue;
                    }
                    out.push_str(&ident);
                } else if let Some(end) = try_rewrite_call(&chars, i, &ident, &mut out) {
                    i = end;
                } else {
                    out.push_str(&ident);
                }
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    out
}

fn is_ident_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_' || c == '$'
}

fn is_ident_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '$'
}

/// If `name(...)` starts at `i` (after optional whitespace) and rewrites to
/// Extended JSON, push the replacement onto `out` and return the index just
/// past the closing paren.
fn try_rewrite_call(chars: &[char], mut i: usize, name: &str, out: &mut String) -> Option<usize> {
    if !matches!(
        name,
        "ObjectId" | "ISODate" | "Date" | "NumberLong" | "NumberInt" | "NumberDecimal" | "UUID"
    ) {
        return None;
    }
    while i < chars.len() && chars[i].is_whitespace() {
        i += 1;
    }
    if i >= chars.len() || chars[i] != '(' {
        return None;
    }
    i += 1;
    let args_start = i;
    let mut depth = 1usize;
    while i < chars.len() {
        match chars[i] {
            q @ ('"' | '\'') => {
                i += 1;
                while i < chars.len() {
                    let c = chars[i];
                    i += 1;
                    if c == '\\' {
                        i += 1;
                    } else if c == q {
                        break;
                    }
                }
                continue;
            }
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    break;
                }
            }
            _ => {}
        }
        i += 1;
    }
    if depth != 0 {
        return None;
    }
    let args: String = chars[args_start..i].iter().collect();
    let repl = rewrite_ctor(name, args.trim())?;
    out.push_str(&repl);
    Some(i + 1)
}

fn rewrite_ctor(name: &str, arg: &str) -> Option<String> {
    let quoted = parse_string_literal(arg);
    let json_str = |s: String| serde_json::Value::String(s).to_string();
    match name {
        "ObjectId" => Some(format!("{{\"$oid\":{}}}", json_str(quoted?))),
        // mongosh's bare Date() technically returns a string, but in a filter
        // the user almost certainly means a BSON date.
        "ISODate" | "Date" => {
            if let Some(s) = quoted {
                Some(format!("{{\"$date\":{}}}", json_str(s)))
            } else if is_int_literal(arg) {
                // millis since the epoch
                Some(format!("{{\"$date\":{{\"$numberLong\":\"{arg}\"}}}}"))
            } else {
                None
            }
        }
        "NumberLong" | "NumberInt" | "NumberDecimal" => {
            let n = match quoted {
                Some(s) => s,
                None if !arg.is_empty() && arg.chars().all(|c| !c.is_whitespace() && c != ',') => {
                    arg.to_string()
                }
                None => return None,
            };
            let key = match name {
                "NumberLong" => "$numberLong",
                "NumberInt" => "$numberInt",
                _ => "$numberDecimal",
            };
            Some(format!("{{\"{key}\":{}}}", json_str(n)))
        }
        "UUID" => Some(format!("{{\"$uuid\":{}}}", json_str(quoted?))),
        _ => None,
    }
}

/// `'abc'` / `"abc"` -> `abc` (with simple escapes resolved); None if the
/// text is not exactly one string literal.
fn parse_string_literal(s: &str) -> Option<String> {
    let mut cs = s.chars();
    let q = cs.next()?;
    if q != '"' && q != '\'' {
        return None;
    }
    let mut out = String::new();
    loop {
        let c = cs.next()?;
        if c == '\\' {
            match cs.next()? {
                'n' => out.push('\n'),
                't' => out.push('\t'),
                'r' => out.push('\r'),
                other => out.push(other),
            }
        } else if c == q {
            return if cs.next().is_none() { Some(out) } else { None };
        } else {
            out.push(c);
        }
    }
}

fn is_int_literal(s: &str) -> bool {
    let digits = s.strip_prefix('-').unwrap_or(s);
    !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit())
}

/// Human name of a stage document, e.g. "$match".
pub fn stage_name(stage: &Document) -> String {
    stage
        .keys()
        .next()
        .cloned()
        .unwrap_or_else(|| "(empty)".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use mongodb::bson::doc;

    #[test]
    fn empty_is_match_all() {
        assert_eq!(parse_filter("").unwrap(), Document::new());
        assert_eq!(parse_filter("   ").unwrap(), Document::new());
    }

    #[test]
    fn strict_json() {
        assert_eq!(
            parse_filter(r#"{ "status": "active" }"#).unwrap(),
            doc! { "status": "active" }
        );
    }

    #[test]
    fn relaxed_keys_and_quotes() {
        assert_eq!(
            parse_filter("{ status: 'active', age: { $gt: 21 } }").unwrap(),
            doc! { "status": "active", "age": { "$gt": 21 } }
        );
    }

    #[test]
    fn extended_json_object_id() {
        let d = parse_filter(r#"{ _id: { $oid: "507f1f77bcf86cd799439011" } }"#).unwrap();
        assert!(matches!(d.get("_id"), Some(Bson::ObjectId(_))));
    }

    #[test]
    fn mongosh_object_id() {
        let oid = mongodb::bson::oid::ObjectId::parse_str("507f1f77bcf86cd799439011").unwrap();
        for input in [
            r#"{ _id: ObjectId('507f1f77bcf86cd799439011') }"#,
            r#"{ _id: ObjectId("507f1f77bcf86cd799439011") }"#,
            r#"{ _id: new ObjectId('507f1f77bcf86cd799439011') }"#,
            r#"{ _id: ObjectId( "507f1f77bcf86cd799439011" ) }"#,
        ] {
            let d = parse_filter(input).unwrap();
            assert_eq!(d.get("_id"), Some(&Bson::ObjectId(oid)), "input: {input}");
        }
    }

    #[test]
    fn mongosh_object_id_invalid() {
        // bad hex parses syntactically but must fail BSON conversion
        assert!(parse_filter(r#"{ _id: ObjectId('nope') }"#).is_err());
        // no argument: nothing to rewrite, JSON5 rejects it
        assert!(parse_filter("{ _id: ObjectId() }").is_err());
    }

    #[test]
    fn constructor_inside_string_untouched() {
        let d = parse_filter(r#"{ note: "ObjectId('507f1f77bcf86cd799439011')" }"#).unwrap();
        assert_eq!(
            d.get("note"),
            Some(&Bson::String("ObjectId('507f1f77bcf86cd799439011')".into()))
        );
    }

    #[test]
    fn mongosh_dates_and_numbers() {
        let d = parse_filter(
            r#"{ a: ISODate("2024-01-02T03:04:05Z"), b: new Date(1700000000000),
                c: NumberLong("9007199254740993"), d: NumberInt(7),
                e: NumberDecimal("1.5") }"#,
        )
        .unwrap();
        assert!(matches!(d.get("a"), Some(Bson::DateTime(_))));
        assert!(matches!(d.get("b"), Some(Bson::DateTime(_))));
        assert_eq!(d.get("c"), Some(&Bson::Int64(9007199254740993)));
        assert_eq!(d.get("d"), Some(&Bson::Int32(7)));
        assert!(matches!(d.get("e"), Some(Bson::Decimal128(_))));
    }

    #[test]
    fn mongosh_uuid() {
        let d = parse_filter(r#"{ u: UUID("3b241101-e2bb-4255-8caf-4136c566a962") }"#).unwrap();
        assert!(matches!(d.get("u"), Some(Bson::Binary(_))), "{d:?}");
    }

    #[test]
    fn pipeline_with_object_id() {
        let p = parse_pipeline(r#"[{ $match: { _id: ObjectId('507f1f77bcf86cd799439011') } }]"#)
            .unwrap();
        assert!(matches!(
            p[0].get_document("$match").unwrap().get("_id"),
            Some(Bson::ObjectId(_))
        ));
    }

    #[test]
    fn non_object_rejected() {
        assert!(parse_filter("42").is_err());
        assert!(parse_filter("[1,2]").is_err());
    }

    #[test]
    fn garbage_rejected() {
        assert!(parse_filter("{ nope").is_err());
    }

    #[test]
    fn optional_doc() {
        assert_eq!(parse_optional_doc("  ").unwrap(), None);
        assert_eq!(
            parse_optional_doc("{ name: 1 }").unwrap(),
            Some(doc! { "name": 1 })
        );
        assert!(parse_optional_doc("nope").is_err());
    }

    #[test]
    fn pipeline_array() {
        let p = parse_pipeline("[{ $match: { age: { $gt: 21 } } }, { $count: 'n' }]").unwrap();
        assert_eq!(p.len(), 2);
        assert_eq!(stage_name(&p[0]), "$match");
        assert_eq!(stage_name(&p[1]), "$count");
    }

    #[test]
    fn pipeline_single_stage_object() {
        let p = parse_pipeline("{ $match: {} }").unwrap();
        assert_eq!(p.len(), 1);
    }

    #[test]
    fn pipeline_rejects_non_objects() {
        assert!(parse_pipeline("[1, 2]").is_err());
        assert!(parse_pipeline("[]").is_err());
        assert!(parse_pipeline("").is_err());
    }
}
