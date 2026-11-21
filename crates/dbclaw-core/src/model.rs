//! Data model shared by drivers and UI: connection configuration, schema
//! objects, result sets and staged row changes.

use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Hash)]
#[serde(rename_all = "lowercase")]
pub enum DbKind {
    Postgres,
    Mongo,
}

impl DbKind {
    pub fn label(self) -> &'static str {
        match self {
            DbKind::Postgres => "PostgreSQL",
            DbKind::Mongo => "MongoDB",
        }
    }

    pub fn default_url(self) -> &'static str {
        match self {
            DbKind::Postgres => "postgres://postgres@localhost:5432/postgres",
            DbKind::Mongo => "mongodb://localhost:27017",
        }
    }

    /// Guess the kind from a connection URL scheme.
    pub fn from_url(url: &str) -> Option<DbKind> {
        let lower = url.trim().to_ascii_lowercase();
        if lower.starts_with("postgres://") || lower.starts_with("postgresql://") {
            Some(DbKind::Postgres)
        } else if lower.starts_with("mongodb://") || lower.starts_with("mongodb+srv://") {
            Some(DbKind::Mongo)
        } else {
            None
        }
    }
}

/// A saved connection.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct ConnectionConfig {
    pub id: String,
    pub name: String,
    pub kind: DbKind,
    /// Connection URL *without* the password.
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password: Option<String>,
    /// Refuse all writes (staged edits, console writes are still possible in
    /// the native console, but the grid becomes read-only).
    #[serde(default)]
    pub read_only: bool,
}

/// Split a password out of a connection URL. Returns the URL without the
/// password and the (percent-decoded) password, if there was one.
///
/// `postgres://bob:s3cr%40t@host/db` → (`postgres://bob@host/db`, `s3cr@t`)
pub fn split_url_password(url: &str) -> (String, Option<String>) {
    let Some(scheme_end) = url.find("://") else {
        return (url.to_string(), None);
    };
    let rest_start = scheme_end + 3;
    let rest = &url[rest_start..];
    // Authority ends at the first '/', '?' or '#'.
    let auth_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = &rest[..auth_end];
    let Some(at) = authority.rfind('@') else {
        return (url.to_string(), None);
    };
    let userinfo = &authority[..at];
    let Some(colon) = userinfo.find(':') else {
        return (url.to_string(), None);
    };
    let user = &userinfo[..colon];
    let password = percent_decode(&userinfo[colon + 1..]);
    let mut out = String::with_capacity(url.len());
    out.push_str(&url[..rest_start]);
    out.push_str(user);
    out.push_str(&rest[at..]);
    (out, Some(password))
}

pub fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = &s[i + 1..i + 3];
            if let Ok(b) = u8::from_str_radix(hex, 16) {
                out.push(b);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Kind of a schema object shown in the navigator.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ObjectKind {
    Table,
    PartitionedTable,
    View,
    MaterializedView,
    ForeignTable,
    Collection,
    MongoView,
}

impl ObjectKind {
    pub fn is_editable(self) -> bool {
        matches!(
            self,
            ObjectKind::Table | ObjectKind::PartitionedTable | ObjectKind::Collection
        )
    }
}

/// A table, view or collection.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ObjectRef {
    pub database: String,
    /// Postgres schema; empty for Mongo.
    pub schema: String,
    pub name: String,
    pub kind: ObjectKind,
}

impl ObjectRef {
    pub fn display_name(&self) -> String {
        if self.schema.is_empty() || self.schema == "public" {
            self.name.clone()
        } else {
            format!("{}.{}", self.schema, self.name)
        }
    }
}

#[derive(Clone, Debug)]
pub struct ObjectInfo {
    pub object: ObjectRef,
    /// Estimated row count, if cheaply known.
    pub estimated_rows: Option<i64>,
}

/// Coarse value category, used for alignment, styling and editing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ValueKind {
    #[default]
    Text,
    Number,
    Bool,
    DateTime,
    Json,
    Uuid,
    Binary,
    Array,
    Other,
}

impl ValueKind {
    /// Map a Postgres `pg_type.typcategory` (+ type name) to a kind.
    pub fn from_pg_category(category: &str, type_name: &str) -> ValueKind {
        match category {
            "N" => ValueKind::Number,
            "B" => ValueKind::Bool,
            "D" => ValueKind::DateTime,
            "A" => ValueKind::Array,
            "S" | "E" => ValueKind::Text,
            _ => match type_name {
                "json" | "jsonb" => ValueKind::Json,
                "uuid" => ValueKind::Uuid,
                "bytea" => ValueKind::Binary,
                _ => ValueKind::Other,
            },
        }
    }

    /// Map a Postgres type name (as returned by `Type::name()`) to a kind.
    pub fn from_pg_type_name(name: &str) -> ValueKind {
        match name {
            "int2" | "int4" | "int8" | "float4" | "float8" | "numeric" | "oid" | "money" => {
                ValueKind::Number
            }
            "bool" => ValueKind::Bool,
            "date" | "time" | "timetz" | "timestamp" | "timestamptz" | "interval" => {
                ValueKind::DateTime
            }
            "json" | "jsonb" => ValueKind::Json,
            "uuid" => ValueKind::Uuid,
            "bytea" => ValueKind::Binary,
            n if n.starts_with('_') => ValueKind::Array,
            _ => ValueKind::Text,
        }
    }

    pub fn is_right_aligned(self) -> bool {
        matches!(self, ValueKind::Number)
    }
}

#[derive(Clone, Debug, Default)]
pub struct ColumnMeta {
    pub name: String,
    /// Human readable type, e.g. `numeric(12,2)` or `string | int32` for Mongo.
    pub type_name: String,
    pub kind: ValueKind,
    pub is_pk: bool,
    pub nullable: bool,
    pub default: Option<String>,
    /// Identity / generated columns can't be written directly.
    pub generated: bool,
    /// Referenced table when this column is part of a foreign key, e.g.
    /// `customers(id)`.
    pub references: Option<String>,
}

/// A foreign key: `from_columns` of `from` reference `to_columns` of `to`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ForeignKey {
    pub name: String,
    pub from: ObjectRef,
    pub from_columns: Vec<String>,
    pub to: ObjectRef,
    pub to_columns: Vec<String>,
    /// Guessed (MongoDB) rather than declared by the database.
    pub inferred: bool,
}

impl ForeignKey {
    /// `customers(id)` / `analytics.events(a, b)`
    pub fn target_label(&self) -> String {
        format!("{}({})", self.to.display_name(), self.to_columns.join(", "))
    }

    pub fn source_label(&self) -> String {
        format!("{}({})", self.from.display_name(), self.from_columns.join(", "))
    }
}

/// One grid cell. Values are pre-rendered to text on the background thread so
/// painting is just text layout.
#[derive(Clone, Debug, PartialEq)]
pub enum Cell {
    /// Field absent (Mongo only).
    Missing,
    Null,
    Value(Arc<str>, ValueKind),
}

impl Cell {
    pub fn text(s: impl Into<Arc<str>>) -> Cell {
        Cell::Value(s.into(), ValueKind::Text)
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Cell::Value(s, _) => Some(s),
            _ => None,
        }
    }

    pub fn is_null(&self) -> bool {
        matches!(self, Cell::Null)
    }
}

/// A page of rows (table view) or one statement's result (console).
#[derive(Clone, Debug, Default)]
pub struct ResultSet {
    pub columns: Vec<ColumnMeta>,
    pub rows: Vec<Vec<Cell>>,
    /// Mongo: the source documents, index-aligned with `rows`.
    pub documents: Option<Vec<bson::Document>>,
    /// Postgres table view without a primary key: the `ctid` of each row.
    pub ctids: Option<Vec<String>>,
    /// Rows affected / returned as reported by the server (`CommandComplete`).
    pub affected: Option<u64>,
    /// The row limit was reached and the rest of the result was dropped.
    pub truncated: bool,
    /// Short status line, e.g. `UPDATE 3` or `inserted 1 document`.
    pub status: Option<String>,
}

/// Output of running something in the query console.
#[derive(Clone, Debug, Default)]
pub struct QueryOutput {
    pub sets: Vec<ResultSet>,
    pub notices: Vec<String>,
    pub elapsed: Duration,
}

/// Result of counting rows. Counting is time-limited so it can't load the
/// server: when the limit hits, a cheap estimate (or nothing) is returned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowCount {
    Exact(u64),
    /// From planner statistics / collection metadata.
    Estimate(u64),
    /// Too expensive to count within the time limit.
    Unknown,
}

/// Sort direction for the grid.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SortDir {
    Asc,
    Desc,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SortSpec {
    pub column: String,
    pub dir: SortDir,
}

/// Request for one page of a table / collection.
#[derive(Clone, Debug)]
pub struct PageRequest {
    pub object: ObjectRef,
    pub filter: Option<crate::filter::Expr>,
    pub sort: Vec<SortSpec>,
    pub offset: u64,
    pub limit: u64,
}

/// A row identity used to target updates and deletes.
#[derive(Clone, Debug, PartialEq)]
pub enum RowKey {
    /// Primary-key columns and their text values (Postgres).
    Pk(Vec<(String, Option<String>)>),
    /// Physical row id for tables without a primary key (Postgres).
    Ctid(String),
    /// Document `_id` (Mongo).
    Id(bson::Bson),
}

/// A staged value for one column/field.
#[derive(Clone, Debug, PartialEq)]
pub enum NewValue {
    /// Postgres text input value; `None` = NULL.
    Text(Option<String>),
    /// Use the column default (inserts only).
    Default,
    /// Mongo value.
    Bson(bson::Bson),
    /// Remove the field (Mongo `$unset`).
    Unset,
}

#[derive(Clone, Debug, PartialEq)]
pub enum RowChange {
    Update {
        key: RowKey,
        values: Vec<(String, NewValue)>,
    },
    Insert {
        values: Vec<(String, NewValue)>,
    },
    /// Replace a whole Mongo document.
    Replace {
        key: RowKey,
        document: bson::Document,
    },
    InsertDocument {
        document: bson::Document,
    },
    Delete {
        key: RowKey,
    },
}

#[derive(Clone, Debug, Default)]
pub struct ApplySummary {
    pub inserted: u64,
    pub updated: u64,
    pub deleted: u64,
}

impl std::fmt::Display for ApplySummary {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut parts = Vec::new();
        if self.inserted > 0 {
            parts.push(format!("{} inserted", self.inserted));
        }
        if self.updated > 0 {
            parts.push(format!("{} updated", self.updated));
        }
        if self.deleted > 0 {
            parts.push(format!("{} deleted", self.deleted));
        }
        if parts.is_empty() {
            write!(f, "no changes")
        } else {
            write!(f, "{}", parts.join(", "))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_password() {
        assert_eq!(
            split_url_password("postgres://bob:s3cr%40t@host:5432/db?x=1"),
            ("postgres://bob@host:5432/db?x=1".to_string(), Some("s3cr@t".to_string()))
        );
        assert_eq!(
            split_url_password("mongodb://u:p@a:1,b:2/?authSource=admin"),
            ("mongodb://u@a:1,b:2/?authSource=admin".to_string(), Some("p".to_string()))
        );
        assert_eq!(
            split_url_password("postgres://bob@host/db"),
            ("postgres://bob@host/db".to_string(), None)
        );
        assert_eq!(split_url_password("postgres://host/db?a=b@c").1, None);
        assert_eq!(split_url_password("not a url").1, None);
    }

    #[test]
    fn kind_from_url() {
        assert_eq!(DbKind::from_url("postgresql://x"), Some(DbKind::Postgres));
        assert_eq!(DbKind::from_url("mongodb+srv://x"), Some(DbKind::Mongo));
        assert_eq!(DbKind::from_url("mysql://x"), None);
    }
}
