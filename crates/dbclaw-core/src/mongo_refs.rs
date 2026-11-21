//! Heuristics for inferring references between MongoDB collections.
//!
//! MongoDB has no foreign keys, but ObjectIds are globally unique: when the
//! value of `orders.userId` exists as an `_id` in `users`, `userId`
//! references `users`. These helpers pick candidate fields and guess target
//! collections by name; the driver then verifies the guess by looking the id
//! up in the candidate collection.

use bson::{Bson, Document};

/// A top-level field that looks like a reference.
#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    pub field: String,
    /// An id seen in this field (the `$id` of a DBRef).
    pub sample: Bson,
    /// The field holds arrays of ids.
    pub array: bool,
    /// Target collection named by a DBRef (`{$ref, $id}`).
    pub dbref: Option<String>,
}

/// Fields (except `_id`) holding ObjectIds, arrays of ObjectIds or DBRefs,
/// in first-seen order.
pub fn candidate_fields(docs: &[Document]) -> Vec<Candidate> {
    let mut out: Vec<Candidate> = Vec::new();
    for doc in docs {
        for (k, v) in doc {
            if k == "_id" || out.iter().any(|c| &c.field == k) {
                continue;
            }
            let found = match v {
                Bson::ObjectId(_) => Some((v.clone(), false, None)),
                Bson::Array(items) => items
                    .iter()
                    .find(|i| matches!(i, Bson::ObjectId(_)))
                    .map(|i| (i.clone(), true, None)),
                Bson::Document(d) => dbref(d).map(|(coll, id)| (id, false, Some(coll))),
                _ => None,
            };
            if let Some((sample, array, dbref)) = found {
                out.push(Candidate { field: k.clone(), sample, array, dbref });
            }
        }
    }
    out
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
}
