//! # Universal filter language
//!
//! The filter bar above every table / collection grid accepts one small
//! language that compiles both to a PostgreSQL `WHERE` expression
//! ([`to_sql`]) and to a MongoDB filter document ([`to_mongo`]).
//!
//! ## Conditions
//!
//! | Syntax                         | Meaning                                       |
//! |--------------------------------|-----------------------------------------------|
//! | `age = 18`, `age == 18`        | equal                                         |
//! | `age != 18`, `age <> 18`       | not equal                                     |
//! | `<` `<=` `>` `>=`              | ordering                                      |
//! | `name ~ "jo"`                  | contains (case-insensitive)                   |
//! | `name !~ "test"`               | does not contain (case-insensitive)           |
//! | `email ^= "admin"`             | starts with (case-insensitive)                |
//! | `email $= ".cz"`               | ends with (case-insensitive)                  |
//! | `title like "foo%bar_"`        | SQL wildcards (`%` any run, `_` one char), case-sensitive; `ilike` ignores case |
//! | `name matches "^J.*n$"`        | regular expression (case-sensitive)           |
//! | `name = /^j.*n$/i`             | regex literal (flags `i`, `m`, `s`, `x`; PostgreSQL supports only `i`) |
//! | `tags in ["a", "b", 3]`        | one of (also `not in`; `( … )` works too)     |
//! | `deleted_at is null`           | null / missing (also `is not null`)           |
//! | `age between 18 and 65`        | inclusive range (also `not between`)          |
//! | `profile exists`               | field is present (also `not exists`)          |
//!
//! `like`, `ilike`, `matches` and `between` can be negated with a leading
//! `not` (`name not like "a%"`). `x = null` is the same as `x is null`.
//!
//! ## Combining
//!
//! `and` / `&&`, `or` / `||`, `not` / `!`, and parentheses. Precedence:
//! `not` binds tightest, then `and`, then `or`:
//! `not (a = 1 or b = 2) and c > 3`.
//!
//! ## Fields
//!
//! Plain names (`status`), dotted paths into documents / JSON columns
//! (`address.city`, `items.0.sku`), and backtick-quoted names for anything
//! else (`` `weird column` = 1 ``, a doubled backtick inside means one).
//!
//! ## Values
//!
//! * text: `"double"` or `'single'` quoted, escapes `\" \' \\ \n \t \r \uXXXX`
//! * numbers: `42`, `-7`, `3.14`, `1e6`
//! * `true`, `false`, `null`
//! * `date("2024-01-01")`, `date("2024-01-01T10:00:00Z")` — a timestamp
//! * `oid("65a1…")` — a MongoDB ObjectId (24 hex characters)
//! * `/pattern/flags` — a regex literal
//!
//! Keywords are case-insensitive. Bare words are never values:
//! `status = active` is an error (write `status = "active"`).
//!
//! ## Semantics notes
//!
//! * SQL output inlines every literal as an untyped, safely quoted string
//!   (`'30'`) so PostgreSQL coerces it to the column type; identifiers are
//!   always double-quoted. The output never contains bind parameters.
//! * On PostgreSQL, dotted paths require a `json`/`jsonb` column and compare
//!   the value as text (`#>>`); ordering comparisons with numbers cast to
//!   `numeric`. On array columns, `=`/`!=`/`in` test membership
//!   (`'a' = ANY("tags")`), mirroring MongoDB.
//! * Comparisons follow each database's NULL rules: in SQL `a != 1` does not
//!   match rows where `a` is NULL, while MongoDB's `$ne` also matches
//!   documents where the field is missing.

mod lexer;
mod mongo;
mod parser;
mod sql;
#[cfg(test)]
mod tests;

use std::fmt;
use std::ops::Range;

pub use mongo::to_mongo;
pub use parser::parse;
pub use sql::to_sql;

/// One line of the filter cheat sheet: examples (each a complete filter)
/// and what they do.
pub struct HelpEntry {
    pub examples: &'static [&'static str],
    pub meaning: &'static str,
}

/// The filter bar's cheat sheet, grouped into titled sections.
pub const HELP: &[(&str, &[HelpEntry])] = &[
    (
        "Compare",
        &[
            HelpEntry { examples: &["age >= 18", "status != \"done\""], meaning: "=  !=  <  <=  >  >=" },
            HelpEntry { examples: &["age between 18 and 65"], meaning: "In a range, both ends included" },
            HelpEntry { examples: &["tags in [\"a\", \"b\", 3]", "tags not in [\"x\"]"], meaning: "One of / none of" },
            HelpEntry { examples: &["deleted_at is null", "deleted_at is not null"], meaning: "Empty / not empty" },
        ],
    ),
    (
        "Text",
        &[
            HelpEntry { examples: &["name ~ \"jo\"", "name !~ \"test\""], meaning: "Contains / doesn't contain, any case" },
            HelpEntry { examples: &["email ^= \"admin\"", "email $= \".cz\""], meaning: "Starts / ends with, any case" },
            HelpEntry { examples: &["title like \"foo%\"", "title ilike \"a_c\""], meaning: "Wildcards: % any text, _ one character" },
            HelpEntry { examples: &["name matches \"^J.*n$\"", "name = /^j/i"], meaning: "Regular expression" },
        ],
    ),
    (
        "Fields and values",
        &[
            HelpEntry { examples: &["address.city = \"Prague\""], meaning: "Nested field" },
            HelpEntry { examples: &["`odd name` = 1"], meaning: "Field name with spaces or symbols" },
            HelpEntry { examples: &["profile exists", "profile not exists"], meaning: "Field is present / missing" },
            HelpEntry { examples: &["created > date(\"2024-01-01\")"], meaning: "Date or timestamp" },
            HelpEntry { examples: &["_id = oid(\"65a1f0c2e4b0a1b2c3d4e5f6\")"], meaning: "MongoDB ObjectId" },
        ],
    ),
    (
        "Combine",
        &[
            HelpEntry { examples: &["status = \"paid\" and total > 100"], meaning: "Both must match (also &&)" },
            HelpEntry { examples: &["country = \"CZ\" or country = \"SK\""], meaning: "Either matches (also ||)" },
            HelpEntry { examples: &["not (a = 1 or b = 2)"], meaning: "Negate (also !), group with ( )" },
        ],
    ),
];

/// A filter syntax or compilation error, with the byte range of the
/// offending input so the UI can underline it.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct FilterError {
    pub message: String,
    pub span: Range<usize>,
}

impl FilterError {
    pub fn new(message: impl Into<String>, span: Range<usize>) -> Self {
        Self {
            message: message.into(),
            span,
        }
    }
}

/// A (possibly dotted) field path: `address.city` → `["address", "city"]`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FieldPath(pub Vec<String>);

impl FieldPath {
    /// Mongo-style dotted form (`address.city`).
    pub fn dotted(&self) -> String {
        self.0.join(".")
    }
}

/// A field reference together with where it appeared in the input.
#[derive(Debug, Clone, PartialEq)]
pub struct Field {
    pub path: FieldPath,
    pub span: Range<usize>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Literal {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    String(String),
    /// `date("…")`: the text as written (validated as `YYYY-MM-DD` or an
    /// ISO 8601 / RFC 3339 timestamp).
    Date(String),
    /// `oid("…")`: 24 hex characters.
    ObjectId(String),
    /// `/pattern/flags`; flags are a sorted subset of `imsx`.
    Regex {
        pattern: String,
        flags: String,
    },
}

/// Binary comparison operators (`in`, `between`, `is null` and `exists`
/// have their own [`Expr`] variants).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompareOp {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    /// `~`: contains, case-insensitive.
    Contains,
    /// `!~`: does not contain, case-insensitive.
    NotContains,
    /// `^=`: starts with, case-insensitive.
    StartsWith,
    /// `$=`: ends with, case-insensitive.
    EndsWith,
    /// `like`: SQL wildcard pattern, case-sensitive.
    Like,
    /// `ilike`: SQL wildcard pattern, case-insensitive.
    ILike,
    /// `matches` (or a regex literal): regular expression.
    Matches,
}

impl CompareOp {
    pub fn symbol(self) -> &'static str {
        match self {
            CompareOp::Eq => "=",
            CompareOp::Ne => "!=",
            CompareOp::Lt => "<",
            CompareOp::Le => "<=",
            CompareOp::Gt => ">",
            CompareOp::Ge => ">=",
            CompareOp::Contains => "~",
            CompareOp::NotContains => "!~",
            CompareOp::StartsWith => "^=",
            CompareOp::EndsWith => "$=",
            CompareOp::Like => "like",
            CompareOp::ILike => "ilike",
            CompareOp::Matches => "matches",
        }
    }
}

/// Parsed filter expression. `span` fields cover the whole condition.
#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    /// All of (flattened: `a and b and c` is one node with three children).
    And(Vec<Expr>),
    /// Any of (flattened like [`Expr::And`]).
    Or(Vec<Expr>),
    Not(Box<Expr>),
    Compare {
        field: Field,
        op: CompareOp,
        value: Literal,
        span: Range<usize>,
    },
    In {
        field: Field,
        values: Vec<Literal>,
        negated: bool,
        span: Range<usize>,
    },
    IsNull {
        field: Field,
        negated: bool,
        span: Range<usize>,
    },
    Between {
        field: Field,
        low: Literal,
        high: Literal,
        span: Range<usize>,
    },
    Exists {
        field: Field,
        negated: bool,
        span: Range<usize>,
    },
}

/// A table column as reported by the database catalog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SqlColumn {
    pub name: String,
    /// PostgreSQL type name, e.g. `integer`, `text`, `jsonb`, `ARRAY`, `_int4`.
    pub data_type: String,
}

impl SqlColumn {
    pub fn new(name: impl Into<String>, data_type: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            data_type: data_type.into(),
        }
    }
}

/// Parse and compile to SQL in one step. `Ok(None)` for an empty filter.
pub fn compile_sql(input: &str, columns: &[SqlColumn]) -> Result<Option<String>, FilterError> {
    parse(input)?.map(|e| to_sql(&e, columns)).transpose()
}

/// Parse and compile to a Mongo filter in one step. An empty filter yields
/// an empty document (matches everything).
pub fn compile_mongo(input: &str) -> Result<bson::Document, FilterError> {
    match parse(input)? {
        Some(e) => to_mongo(&e),
        None => Ok(bson::Document::new()),
    }
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

pub(crate) const KEYWORDS: &[&str] = &[
    "and", "or", "not", "in", "is", "null", "true", "false", "between", "like", "ilike", "matches",
    "exists",
];

pub(crate) fn is_keyword(word: &str) -> bool {
    KEYWORDS.iter().any(|k| k.eq_ignore_ascii_case(word))
}

/// Normalize a `date("…")` argument to an RFC 3339 string. Accepts
/// `YYYY-MM-DD` (UTC midnight), `YYYY-MM-DD[T ]HH:MM[:SS[.fff]][Z|±HH:MM]`
/// (no zone means UTC). Returns `None` when it is not a valid date.
pub(crate) fn normalize_date(s: &str) -> Option<String> {
    let s = s.trim();
    let b = s.as_bytes();
    let is_ymd = |b: &[u8]| {
        b.len() == 10
            && b[4] == b'-'
            && b[7] == b'-'
            && b.iter()
                .enumerate()
                .all(|(i, c)| i == 4 || i == 7 || c.is_ascii_digit())
    };
    if b.len() < 10 || !is_ymd(&b[..10]) {
        return None;
    }
    let out = if b.len() == 10 {
        format!("{s}T00:00:00Z")
    } else {
        if b[10] != b'T' && b[10] != b't' && b[10] != b' ' {
            return None;
        }
        let rest = &s[11..];
        // Split the time part from a trailing zone designator.
        let zone_at = rest.find(['Z', 'z', '+', '-']).unwrap_or(rest.len());
        let (time, zone) = rest.split_at(zone_at);
        let time = if time.len() == 5 {
            format!("{time}:00")
        } else {
            time.to_string()
        };
        let zone = if zone.is_empty() { "Z" } else { zone };
        format!("{}T{time}{zone}", &s[..10])
    };
    bson::DateTime::parse_rfc3339_str(&out).ok().map(|_| out)
}

fn is_plain_ident(s: &str, first: bool) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_alphabetic() || c == '_' || (!first && c.is_ascii_digit()) => {}
        _ => return false,
    }
    chars.all(|c| c.is_alphanumeric() || c == '_') && !is_keyword(s)
}

// ---------------------------------------------------------------------------
// Display: canonical filter syntax (round-trips through `parse`).
// ---------------------------------------------------------------------------

impl fmt::Display for FieldPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, seg) in self.0.iter().enumerate() {
            if i > 0 {
                f.write_str(".")?;
            }
            if is_plain_ident(seg, i == 0) {
                f.write_str(seg)?;
            } else {
                write!(f, "`{}`", seg.replace('`', "``"))?;
            }
        }
        Ok(())
    }
}

fn write_quoted(f: &mut fmt::Formatter<'_>, s: &str) -> fmt::Result {
    f.write_str("\"")?;
    for c in s.chars() {
        match c {
            '"' => f.write_str("\\\"")?,
            '\\' => f.write_str("\\\\")?,
            '\n' => f.write_str("\\n")?,
            '\t' => f.write_str("\\t")?,
            '\r' => f.write_str("\\r")?,
            c if (c as u32) < 0x20 => write!(f, "\\u{:04x}", c as u32)?,
            c => write!(f, "{c}")?,
        }
    }
    f.write_str("\"")
}

impl fmt::Display for Literal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Literal::Null => f.write_str("null"),
            Literal::Bool(b) => write!(f, "{b}"),
            Literal::Int(i) => write!(f, "{i}"),
            Literal::Float(x) => write!(f, "{x:?}"),
            Literal::String(s) => write_quoted(f, s),
            Literal::Date(s) => {
                f.write_str("date(")?;
                write_quoted(f, s)?;
                f.write_str(")")
            }
            Literal::ObjectId(s) => {
                f.write_str("oid(")?;
                write_quoted(f, s)?;
                f.write_str(")")
            }
            Literal::Regex { pattern, flags } => {
                write!(f, "/{}/{flags}", pattern.replace('/', "\\/"))
            }
        }
    }
}

impl fmt::Display for Expr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fn child(f: &mut fmt::Formatter<'_>, e: &Expr, paren_and: bool) -> fmt::Result {
            let paren = matches!(e, Expr::Or(_)) || (paren_and && matches!(e, Expr::And(_)));
            if paren {
                write!(f, "({e})")
            } else {
                write!(f, "{e}")
            }
        }
        match self {
            Expr::And(items) | Expr::Or(items) => {
                let (sep, is_and) = match self {
                    Expr::And(_) => (" and ", true),
                    _ => (" or ", false),
                };
                for (i, e) in items.iter().enumerate() {
                    if i > 0 {
                        f.write_str(sep)?;
                    }
                    child(f, e, !is_and)?;
                }
                Ok(())
            }
            Expr::Not(inner) => {
                f.write_str("not ")?;
                match **inner {
                    Expr::And(_) | Expr::Or(_) => write!(f, "({inner})"),
                    _ => write!(f, "{inner}"),
                }
            }
            Expr::Compare {
                field, op, value, ..
            } => {
                write!(f, "{} {} {value}", field.path, op.symbol())
            }
            Expr::In {
                field,
                values,
                negated,
                ..
            } => {
                write!(
                    f,
                    "{} {}in [",
                    field.path,
                    if *negated { "not " } else { "" }
                )?;
                for (i, v) in values.iter().enumerate() {
                    if i > 0 {
                        f.write_str(", ")?;
                    }
                    write!(f, "{v}")?;
                }
                f.write_str("]")
            }
            Expr::IsNull { field, negated, .. } => {
                write!(
                    f,
                    "{} is {}null",
                    field.path,
                    if *negated { "not " } else { "" }
                )
            }
            Expr::Between {
                field, low, high, ..
            } => {
                write!(f, "{} between {low} and {high}", field.path)
            }
            Expr::Exists { field, negated, .. } => {
                write!(
                    f,
                    "{} {}exists",
                    field.path,
                    if *negated { "not " } else { "" }
                )
            }
        }
    }
}
