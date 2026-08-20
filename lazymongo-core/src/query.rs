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
    let value: serde_json::Value =
        json5::from_str(s).map_err(|e| format!("invalid filter: {e}"))?;
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
    let value: serde_json::Value =
        json5::from_str(s).map_err(|e| format!("invalid pipeline: {e}"))?;
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
