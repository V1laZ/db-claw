//! Heuristics for inferring references between MongoDB collections.
//!
//! MongoDB has no foreign keys, but ObjectIds are globally unique: when the
//! value of `orders.userId` exists as an `_id` in `users`, `userId`
//! references `users`. These helpers pick candidate fields and guess target
//! collections by name; the driver then verifies the guess by looking the id
//! up in the candidate collection.

use bson::{Bson, Document};

/// Maximum nesting depth scanned for references (`a.b.c.d`).
pub const MAX_DEPTH: usize = 4;

/// A field (top-level or nested, as a dotted path) that looks like a
/// reference.
#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    /// Dotted path, e.g. `userId`, `address.cityId`, `items.productId`.
    pub field: String,
    /// An id seen in this field (the `$id` of a DBRef).
    pub sample: Bson,
    /// The path holds several ids per document (an array of ids, or a field
    /// inside an array of sub-documents).
    pub array: bool,
    /// Target collection named by a DBRef (`{$ref, $id}`).
    pub dbref: Option<String>,
}

/// Fields holding ObjectIds, arrays of ObjectIds or DBRefs, including ones
/// nested in sub-documents and in arrays of sub-documents (up to
/// [`MAX_DEPTH`] levels), in first-seen order. `_id` itself is skipped.
pub fn candidate_fields(docs: &[Document]) -> Vec<Candidate> {
    let mut out: Vec<Candidate> = Vec::new();
    for doc in docs {
        scan(doc, "", false, 1, &mut out);
    }
    out
}

fn scan(doc: &Document, prefix: &str, in_array: bool, depth: usize, out: &mut Vec<Candidate>) {
    for (k, v) in doc {
        if prefix.is_empty() && k == "_id" {
            continue;
        }
        let path = if prefix.is_empty() { k.clone() } else { format!("{prefix}.{k}") };
        let mut push = |sample: Bson, array: bool, dbref: Option<String>| {
            if !out.iter().any(|c| c.field == path) {
                out.push(Candidate { field: path.clone(), sample, array, dbref });
            }
        };
        match v {
            Bson::ObjectId(_) => push(v.clone(), in_array, None),
            Bson::Document(d) => match dbref(d) {
                Some((coll, id)) => push(id, in_array, Some(coll)),
                None if depth < MAX_DEPTH => scan(d, &path, in_array, depth + 1, out),
                None => {}
            },
            Bson::Array(items) => {
                if let Some(id) = items.iter().find(|i| matches!(i, Bson::ObjectId(_))) {
                    push(id.clone(), true, None);
                } else if let Some((coll, id)) =
                    items.iter().find_map(|i| i.as_document().and_then(dbref))
                {
                    push(id, true, Some(coll));
                } else if depth < MAX_DEPTH {
                    // Array of sub-documents: `items.productId`.
                    for item in items.iter().filter_map(Bson::as_document) {
                        scan(item, &path, true, depth + 1, out);
                    }
                }
            }
            _ => {}
        }
    }
}

/// All values at a dotted path, descending through arrays like MongoDB
/// queries do (`items.productId` collects the id of every item). DBRefs are
/// replaced by their `$id`; arrays at the end of the path are flattened.
pub fn values_at_path(doc: &Document, path: &str) -> Vec<Bson> {
    let mut out = Vec::new();
    let segments: Vec<&str> = path.split('.').collect();
    collect(&Bson::Document(doc.clone()), &segments, &mut out);
    out
}

fn collect(value: &Bson, segments: &[&str], out: &mut Vec<Bson>) {
    match (value, segments.split_first()) {
        (Bson::Array(items), _) => {
            for item in items {
                collect(item, segments, out);
            }
        }
        (Bson::Document(d), Some((first, rest))) => {
            if let Some(v) = d.get(*first) {
                collect(v, rest, out);
            }
        }
        (Bson::Document(d), None) => match dbref(d) {
            Some((_, id)) => out.push(id),
            None => out.push(value.clone()),
        },
        (Bson::Null, None) => {}
        (v, None) => out.push(v.clone()),
        _ => {}
    }
}

/// `{$ref: "users", $id: …}` → ("users", id)
pub fn dbref(d: &Document) -> Option<(String, Bson)> {
    let coll = d.get_str("$ref").ok()?;
    let id = d.get("$id")?;
    Some((coll.to_string(), id.clone()))
}

/// The entity name a field refers to: `userId` → `user`, `author_ids` →
/// `author`, `owner` → `owner`.
pub fn reference_base(field: &str) -> String {
    let field = field.rsplit('.').next().unwrap_or(field);
    for suffix in ["_ids", "Ids", "IDs", "_id", "Id", "ID"] {
        if let Some(base) = field.strip_suffix(suffix)
            && !base.is_empty() {
                return base.trim_end_matches('_').to_string();
            }
    }
    field.to_string()
}

fn normalize(s: &str) -> String {
    s.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect()
}

/// Does `collection` plausibly hold entities named `base`?
/// (`user` ~ `users`, `category` ~ `categories`, `box` ~ `boxes`, `Person` ~ `person`.)
pub fn collection_matches(base: &str, collection: &str) -> bool {
    let b = normalize(base);
    let c = normalize(collection);
    if b.is_empty() || c.is_empty() {
        return false;
    }
    if b == c || format!("{b}s") == c || format!("{b}es") == c {
        return true;
    }
    if let Some(stem) = b.strip_suffix('y')
        && format!("{stem}ies") == c {
            return true;
        }
    // Field already plural (`usersId` is rare, but `tags` holding ids is not).
    if let Some(stem) = b.strip_suffix('s')
        && (stem == c || format!("{stem}s") == c) {
            return true;
        }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use bson::{doc, oid::ObjectId};

    #[test]
    fn bases() {
        assert_eq!(reference_base("userId"), "user");
        assert_eq!(reference_base("user_id"), "user");
        assert_eq!(reference_base("tagIds"), "tag");
        assert_eq!(reference_base("author_ids"), "author");
        assert_eq!(reference_base("ownerID"), "owner");
        assert_eq!(reference_base("owner"), "owner");
        assert_eq!(reference_base("Id"), "Id");
        assert_eq!(reference_base("address.cityId"), "city");
        assert_eq!(reference_base("items.productId"), "product");
    }

    #[test]
    fn matching() {
        assert!(collection_matches("user", "users"));
        assert!(collection_matches("user", "Users"));
        assert!(collection_matches("category", "categories"));
        assert!(collection_matches("box", "boxes"));
        assert!(collection_matches("person", "person"));
        assert!(collection_matches("order_item", "orderItems"));
        assert!(collection_matches("tags", "tags"));
        assert!(!collection_matches("user", "orders"));
        assert!(!collection_matches("", "users"));
    }

    #[test]
    fn candidates() {
        let a = ObjectId::new();
        let b = ObjectId::new();
        let docs = vec![
            doc! { "_id": ObjectId::new(), "name": "x", "userId": a, "tags": ["t"] },
            doc! { "_id": ObjectId::new(), "tagIds": [b], "owner": { "$ref": "people", "$id": a }, "userId": b },
        ];
        let c = candidate_fields(&docs);
        assert_eq!(c.len(), 3);
        assert_eq!(c[0], Candidate { field: "userId".into(), sample: Bson::ObjectId(a), array: false, dbref: None });
        assert_eq!(c[1].field, "tagIds");
        assert!(c[1].array);
        assert_eq!(c[2].dbref.as_deref(), Some("people"));
        assert_eq!(c[2].sample, Bson::ObjectId(a));
    }

    #[test]
    fn nested_candidates() {
        let (city, p1, p2, u) = (ObjectId::new(), ObjectId::new(), ObjectId::new(), ObjectId::new());
        let docs = vec![doc! {
            "_id": ObjectId::new(),
            "address": { "street": "x", "cityId": city, "geo": { "a": { "b": { "deepId": u } } } },
            "items": [ { "productId": p1, "qty": 1 }, { "productId": p2, "qty": 2 } ],
            "audit": { "by": { "$ref": "users", "$id": u } },
        }];
        let c = candidate_fields(&docs);
        let fields: Vec<&str> = c.iter().map(|c| c.field.as_str()).collect();
        // `address.geo.a.b.deepId` is 5 levels deep: beyond MAX_DEPTH.
        assert_eq!(fields, vec!["address.cityId", "items.productId", "audit.by"]);
        assert!(!c[0].array);
        assert!(c[1].array);
        assert_eq!(c[2].dbref.as_deref(), Some("users"));
    }

    #[test]
    fn path_values() {
        let (city, p1, p2, u) = (ObjectId::new(), ObjectId::new(), ObjectId::new(), ObjectId::new());
        let d = doc! {
            "address": { "cityId": city },
            "items": [ { "productId": p1 }, { "productId": p2 }, { "qty": 3 } ],
            "tagIds": [u, u],
            "owner": { "$ref": "users", "$id": u },
            "gone": null,
        };
        assert_eq!(values_at_path(&d, "address.cityId"), vec![Bson::ObjectId(city)]);
        assert_eq!(values_at_path(&d, "items.productId"), vec![Bson::ObjectId(p1), Bson::ObjectId(p2)]);
        assert_eq!(values_at_path(&d, "tagIds"), vec![Bson::ObjectId(u), Bson::ObjectId(u)]);
        assert_eq!(values_at_path(&d, "owner"), vec![Bson::ObjectId(u)]);
        assert!(values_at_path(&d, "gone").is_empty());
        assert!(values_at_path(&d, "address.missing").is_empty());
    }
}
