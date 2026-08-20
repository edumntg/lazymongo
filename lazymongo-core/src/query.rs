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
}
