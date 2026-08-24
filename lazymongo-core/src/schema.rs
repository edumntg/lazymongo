//! Schema analysis over a sample of documents: per top-level field, how
//! often it appears and with which BSON types (Compass-style schema tab).

use mongodb::bson::{Bson, Document};

#[derive(Debug, Clone)]
pub struct FieldStat {
    pub name: String,
    /// Distinct BSON type names seen, most frequent first.
    pub types: Vec<String>,
    /// Documents (out of the sample) that contain the field.
    pub present: u32,
}

pub fn type_name(v: &Bson) -> &'static str {
    match v {
        Bson::Double(_) => "double",
        Bson::String(_) => "string",
        Bson::Document(_) => "object",
        Bson::Array(_) => "array",
        Bson::Boolean(_) => "bool",
        Bson::Null => "null",
        Bson::Int32(_) => "int",
        Bson::Int64(_) => "long",
        Bson::Decimal128(_) => "decimal",
        Bson::ObjectId(_) => "objectId",
        Bson::DateTime(_) => "date",
        Bson::Binary(_) => "binary",
        Bson::RegularExpression(_) => "regex",
        Bson::Timestamp(_) => "timestamp",
        _ => "other",
    }
}

/// Analyze top-level fields across the sampled documents.
/// Sorted by presence (desc), then name; `_id` always first.
pub fn analyze(docs: &[Document]) -> Vec<FieldStat> {
    use std::collections::HashMap;
    let mut presence: HashMap<String, u32> = HashMap::new();
    let mut types: HashMap<String, HashMap<&'static str, u32>> = HashMap::new();
    for doc in docs {
        for (key, value) in doc.iter() {
            *presence.entry(key.clone()).or_default() += 1;
            *types
                .entry(key.clone())
                .or_default()
                .entry(type_name(value))
                .or_default() += 1;
        }
    }
    let mut fields: Vec<FieldStat> = presence
        .into_iter()
        .map(|(name, present)| {
            let mut ts: Vec<(&str, u32)> = types
                .remove(&name)
                .map(|m| m.into_iter().collect())
                .unwrap_or_default();
            ts.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
            FieldStat {
                name,
                types: ts.into_iter().map(|(t, _)| t.to_string()).collect(),
                present,
            }
        })
        .collect();
    fields.sort_by(|a, b| {
        let a_id = a.name == "_id";
        let b_id = b.name == "_id";
        b_id.cmp(&a_id)
            .then(b.present.cmp(&a.present))
            .then(a.name.cmp(&b.name))
    });
    fields
}

#[cfg(test)]
mod tests {
    use super::*;
    use mongodb::bson::doc;

    #[test]
    fn analyzes_presence_and_types() {
        let docs = vec![
            doc! { "_id": 1, "name": "a", "age": 30 },
            doc! { "_id": 2, "name": "b", "age": 31.5 },
            doc! { "_id": 3, "name": "c" },
        ];
        let fields = analyze(&docs);
        assert_eq!(fields[0].name, "_id"); // _id pinned first
        let name = fields.iter().find(|f| f.name == "name").unwrap();
        assert_eq!(name.present, 3);
        assert_eq!(name.types, vec!["string"]);
        let age = fields.iter().find(|f| f.name == "age").unwrap();
        assert_eq!(age.present, 2);
        assert_eq!(age.types.len(), 2); // int + double
    }

    #[test]
    fn empty_sample() {
        assert!(analyze(&[]).is_empty());
    }
}
