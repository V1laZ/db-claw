//! Autocompletion ("whisperer") for the SQL console, the Mongo console and
//! the filter bar. Pure functions: given the text, the cursor and what is
//! known about the database, return the range to replace and ranked
//! suggestions. The UI adapts them to its completion menu.

use std::collections::HashMap;
use std::ops::Range;

use crate::model::{DbKind, ValueKind};

/// Tables and columns of one database (Postgres) or its collections (Mongo).
#[derive(Clone, Debug, Default)]
pub struct Catalog {
    pub tables: Vec<CatalogTable>,
}

#[derive(Clone, Debug, Default)]
pub struct CatalogTable {
    /// Postgres schema; empty for Mongo.
    pub schema: String,
    pub name: String,
    pub view: bool,
    pub columns: Vec<CatalogColumn>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct CatalogColumn {
    pub name: String,
    pub type_name: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SuggestionKind {
    Keyword,
    Function,
    Schema,
    Table,
    View,
    Column,
    Collection,
    Method,
    Operator,
    Value,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Suggestion {
    pub label: String,
    pub insert: String,
    pub kind: SuggestionKind,
    pub detail: Option<String>,
}

impl Suggestion {
    fn new(label: impl Into<String>, kind: SuggestionKind) -> Self {
        let label = label.into();
        Suggestion { insert: label.clone(), label, kind, detail: None }
    }

    fn insert(mut self, insert: impl Into<String>) -> Self {
        self.insert = insert.into();
        self
    }

    fn detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Completions {
    /// Byte range of the text the chosen suggestion replaces.
    pub replace: Range<usize>,
    pub items: Vec<Suggestion>,
}

/// Most suggestions returned at once.
pub const MAX_ITEMS: usize = 60;

// ---------------------------------------------------------------------------
// Shared helpers

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '$'
}

/// Start of the word ending at `cursor` (identifier chars only).
fn word_start(text: &str, cursor: usize) -> usize {
    text[..cursor]
        .char_indices()
        .rev()
        .take_while(|(_, c)| is_word(*c))
        .last()
        .map_or(cursor, |(i, _)| i)
}

/// Rank by how well `candidate` matches `prefix`: 0 = prefix match,
/// 1 = match at a word boundary (`id` → `customer_id`), None = no match.
fn match_rank(candidate: &str, prefix: &str) -> Option<u8> {
    if prefix.is_empty() {
        return Some(0);
    }
    let c = candidate.to_lowercase();
    let p = prefix.to_lowercase();
    if c.starts_with(&p) {
        return Some(0);
    }
    let boundary = c
        .match_indices(&p)
        .any(|(i, _)| i > 0 && matches!(c.as_bytes()[i - 1], b'_' | b'.' | b'$'));
    boundary.then_some(1)
}

/// Keep matching suggestions, ordered by (group, match quality, label).
fn rank(groups: Vec<Vec<Suggestion>>, prefix: &str) -> Vec<Suggestion> {
    let mut scored: Vec<(usize, u8, Suggestion)> = Vec::new();
    for (g, group) in groups.into_iter().enumerate() {
        for s in group {
            if let Some(r) = match_rank(&s.label, prefix) {
                // An exact, complete match is pointless to suggest.
                if !prefix.is_empty() && s.label.eq_ignore_ascii_case(prefix) && s.insert == s.label {
                    continue;
                }
                scored.push((g, r, s));
            }
        }
    }
    // Stable: without a prefix, keep the given order; with one, prefer
    // shorter (closer) matches.
    scored.sort_by(|a, b| {
        let by_len = if prefix.is_empty() { std::cmp::Ordering::Equal } else { a.2.label.len().cmp(&b.2.label.len()) };
        (a.1, a.0).cmp(&(b.1, b.0)).then(by_len)
    });
    let mut seen = std::collections::HashSet::new();
    scored
        .into_iter()
        .map(|(_, _, s)| s)
        .filter(|s| seen.insert((s.label.clone(), s.kind)))
        .take(MAX_ITEMS)
        .collect()
}

// ---------------------------------------------------------------------------
// SQL

const SQL_KEYWORDS: &[&str] = &[
    "SELECT", "FROM", "WHERE", "AND", "OR", "NOT", "IN", "IS", "NULL", "LIKE", "ILIKE", "BETWEEN",
    "EXISTS", "AS", "ON", "USING", "JOIN", "LEFT JOIN", "RIGHT JOIN", "INNER JOIN", "FULL JOIN",
    "CROSS JOIN", "GROUP BY", "ORDER BY", "HAVING", "LIMIT", "OFFSET", "DISTINCT", "UNION", "UNION ALL",
    "EXCEPT", "INTERSECT", "INSERT INTO", "VALUES", "UPDATE", "SET", "DELETE FROM", "RETURNING",
    "CREATE TABLE", "CREATE INDEX", "CREATE VIEW", "ALTER TABLE", "DROP TABLE", "TRUNCATE", "BEGIN",
    "COMMIT", "ROLLBACK", "WITH", "CASE", "WHEN", "THEN", "ELSE", "END", "ASC", "DESC", "NULLS FIRST",
    "NULLS LAST", "TRUE", "FALSE", "DEFAULT", "PRIMARY KEY", "REFERENCES", "EXPLAIN", "EXPLAIN ANALYZE",
    "ON CONFLICT", "DO NOTHING", "DO UPDATE SET", "FILTER", "OVER", "PARTITION BY", "LATERAL",
];

const SQL_FUNCTIONS: &[&str] = &[
    "count", "sum", "avg", "min", "max", "coalesce", "nullif", "greatest", "least", "now",
    "current_date", "current_timestamp", "date_trunc", "extract", "to_char", "to_date",
    "age", "lower", "upper", "length", "trim", "substring", "replace", "concat", "concat_ws",
    "split_part", "string_agg", "array_agg", "json_agg", "jsonb_agg", "json_build_object",
    "jsonb_build_object", "jsonb_array_elements", "jsonb_each", "unnest", "array_length",
    "round", "floor", "ceil", "abs", "random", "gen_random_uuid", "row_number", "rank",
    "dense_rank", "lag", "lead", "first_value", "last_value", "generate_series", "format",
];

/// Keywords that decide what comes next.
const SQL_TABLE_CONTEXT: &[&str] = &["FROM", "JOIN", "INTO", "UPDATE", "TABLE", "TRUNCATE"];
const SQL_COLUMN_CONTEXT: &[&str] = &[
    "SELECT", "WHERE", "AND", "OR", "ON", "BY", "SET", "HAVING", "RETURNING", "DISTINCT", "NOT", "WHEN",
    "THEN", "ELSE", "CASE",
];

#[derive(Debug, Clone, PartialEq)]
enum SqlTok {
    Word(String),
    Quoted(String),
    Punct(char),
}

/// Rough SQL tokenizer: words, quoted identifiers and punctuation; strings
/// and comments are skipped. Returns `None` if `text` ends inside a string
/// or comment (no completion there).
fn sql_tokens(text: &str) -> Option<Vec<SqlTok>> {
    let mut out = Vec::new();
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            i += 1;
        } else if c == '-' && chars.get(i + 1) == Some(&'-') {
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
            if i >= chars.len() {
                return None;
            }
        } else if c == '/' && chars.get(i + 1) == Some(&'*') {
            i += 2;
            loop {
                if i + 1 >= chars.len() {
                    return None;
                }
                if chars[i] == '*' && chars[i + 1] == '/' {
                    i += 2;
                    break;
                }
                i += 1;
            }
        } else if c == '\'' {
            i += 1;
            loop {
                if i >= chars.len() {
                    return None;
                }
                if chars[i] == '\'' {
                    if chars.get(i + 1) == Some(&'\'') {
                        i += 2;
                        continue;
                    }
                    i += 1;
                    break;
                }
                i += 1;
            }
            out.push(SqlTok::Punct('\''));
        } else if c == '"' {
            let start = i + 1;
            i += 1;
            while i < chars.len() && chars[i] != '"' {
                i += 1;
            }
            if i >= chars.len() {
                return None;
            }
            out.push(SqlTok::Quoted(chars[start..i].iter().collect()));
            i += 1;
        } else if is_word(c) {
            let start = i;
            while i < chars.len() && is_word(chars[i]) {
                i += 1;
            }
            out.push(SqlTok::Word(chars[start..i].iter().collect()));
        } else {
            out.push(SqlTok::Punct(c));
            i += 1;
        }
    }
    Some(out)
}

fn is_sql_keyword(w: &str) -> bool {
    let u = w.to_uppercase();
    SQL_KEYWORDS.iter().any(|k| k.split(' ').any(|p| p == u))
}

fn tok_name(t: &SqlTok) -> Option<String> {
    match t {
        SqlTok::Word(w) => Some(w.clone()),
        SqlTok::Quoted(q) => Some(q.clone()),
        SqlTok::Punct(_) => None,
    }
}

/// `alias → (schema, table)` from `FROM/JOIN/UPDATE/INTO x [AS] a` clauses.
fn sql_aliases(tokens: &[SqlTok]) -> Vec<(Option<String>, String, Option<String>)> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < tokens.len() {
        let is_from = matches!(&tokens[i], SqlTok::Word(w) if SQL_TABLE_CONTEXT.contains(&w.to_uppercase().as_str()));
        if !is_from {
            i += 1;
            continue;
        }
        let Some(first) = tokens.get(i + 1).and_then(tok_name) else {
            i += 1;
            continue;
        };
        let mut j = i + 2;
        let (schema, table) = if tokens.get(j) == Some(&SqlTok::Punct('.')) {
            match tokens.get(j + 1).and_then(tok_name) {
                Some(t) => {
                    j += 2;
                    (Some(first), t)
                }
                None => (None, first),
            }
        } else {
            (None, first)
        };
        if matches!(tokens.get(j), Some(SqlTok::Word(w)) if w.eq_ignore_ascii_case("as")) {
            j += 1;
        }
        let alias = match tokens.get(j) {
            Some(SqlTok::Word(w)) if !is_sql_keyword(w) => Some(w.clone()),
            Some(SqlTok::Quoted(q)) => Some(q.clone()),
            _ => None,
        };
        out.push((schema, table, alias));
        i = j;
    }
    out
}

/// Quote a Postgres identifier only when needed.
fn sql_ident(name: &str) -> String {
    let simple = name
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_lowercase() || c == '_')
        && name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '$')
        && !is_sql_keyword(name);
    if simple { name.to_string() } else { crate::sql::quote_ident(name) }
}

fn find_table<'a>(catalog: &'a Catalog, schema: Option<&str>, name: &str) -> Option<&'a CatalogTable> {
    let matches = |t: &&CatalogTable| {
        t.name.eq_ignore_ascii_case(name) && schema.is_none_or(|s| t.schema.eq_ignore_ascii_case(s))
    };
    catalog
        .tables
        .iter()
        .filter(matches)
        .min_by_key(|t| if t.schema == "public" { 0 } else { 1 })
}

fn table_suggestion(t: &CatalogTable, qualify: bool) -> Suggestion {
    let name = if qualify && !t.schema.is_empty() && t.schema != "public" {
        format!("{}.{}", sql_ident(&t.schema), sql_ident(&t.name))
    } else {
        sql_ident(&t.name)
    };
    let label = if !t.schema.is_empty() && t.schema != "public" && qualify {
        format!("{}.{}", t.schema, t.name)
    } else {
        t.name.clone()
    };
    Suggestion::new(label, if t.view { SuggestionKind::View } else { SuggestionKind::Table })
        .insert(name)
        .detail(if t.schema.is_empty() { String::new() } else { t.schema.clone() })
}

fn column_suggestions(t: &CatalogTable) -> Vec<Suggestion> {
    t.columns
        .iter()
        .map(|c| {
            Suggestion::new(c.name.clone(), SuggestionKind::Column)
                .insert(sql_ident(&c.name))
                .detail(format!("{} · {}", c.type_name, t.name))
        })
        .collect()
}

/// SQL completions at `cursor`. `invoked` = explicitly requested (Ctrl+Space):
/// suggestions are shown even with an empty prefix.
pub fn sql(text: &str, cursor: usize, catalog: &Catalog, invoked: bool) -> Completions {
    let cursor = cursor.min(text.len());
    let start = word_start(text, cursor);
    let prefix = &text[start..cursor];
    let empty = Completions { replace: start..cursor, items: vec![] };

    // Only the current statement matters.
    let stmt_start = crate::sql::split_statements(&text[..cursor])
        .last()
        .map(|r| r.start)
        .unwrap_or(0)
        .min(start);
    let Some(before) = sql_tokens(&text[stmt_start..start]) else {
        return empty; // inside a string or comment
    };
    let whole = sql_tokens(&text[stmt_start..]).unwrap_or_else(|| before.clone());
    let aliases = sql_aliases(&whole);

    // `alias.` / `table.` / `schema.` → members.
    if before.last() == Some(&SqlTok::Punct('.')) {
        let Some(qual) = before.len().checked_sub(2).and_then(|i| tok_name(&before[i])) else {
            return empty;
        };
        let mut items = Vec::new();
        let by_alias = aliases
            .iter()
            .find(|(_, t, a)| a.as_deref().is_some_and(|a| a.eq_ignore_ascii_case(&qual)) || t.eq_ignore_ascii_case(&qual));
        if let Some(t) = by_alias.and_then(|(s, t, _)| find_table(catalog, s.as_deref(), t)) {
            items.extend(column_suggestions(t));
        }
        let in_schema: Vec<Suggestion> = catalog
            .tables
            .iter()
            .filter(|t| t.schema.eq_ignore_ascii_case(&qual))
            .map(|t| table_suggestion(t, false))
            .collect();
        items.extend(in_schema);
        return Completions { replace: start..cursor, items: rank(vec![items], prefix) };
    }

    if prefix.is_empty() && !invoked {
        return empty;
    }

    let last_kw = before.iter().rev().find_map(|t| match t {
        SqlTok::Word(w) if is_sql_keyword(w) => Some(w.to_uppercase()),
        _ => None,
    });
    let lower = prefix.chars().all(|c| !c.is_uppercase()) && !prefix.is_empty();
    let keywords: Vec<Suggestion> = SQL_KEYWORDS
        .iter()
        .map(|k| {
            let text = if lower { k.to_lowercase() } else { k.to_string() };
            Suggestion::new(text, SuggestionKind::Keyword)
        })
        .collect();
    let functions: Vec<Suggestion> = SQL_FUNCTIONS
        .iter()
        .map(|f| Suggestion::new(*f, SuggestionKind::Function).insert(format!("{f}(")).detail("function"))
        .collect();
    let tables: Vec<Suggestion> = catalog.tables.iter().map(|t| table_suggestion(t, true)).collect();
    let mut schemas: Vec<String> = catalog
        .tables
        .iter()
        .map(|t| t.schema.clone())
        .filter(|s| !s.is_empty() && s != "public")
        .collect();
    schemas.sort();
    schemas.dedup();
    let schemas: Vec<Suggestion> = schemas
        .into_iter()
        .map(|s| Suggestion::new(s.clone(), SuggestionKind::Schema).insert(sql_ident(&s)).detail("schema"))
        .collect();
    let columns: Vec<Suggestion> = aliases
        .iter()
        .filter_map(|(s, t, _)| find_table(catalog, s.as_deref(), t))
        .flat_map(column_suggestions)
        .collect();

    let groups = match last_kw.as_deref() {
        Some(k) if SQL_TABLE_CONTEXT.contains(&k) => vec![tables, schemas, keywords],
        Some(k) if SQL_COLUMN_CONTEXT.contains(&k) => vec![columns, functions, keywords, tables],
        _ => vec![keywords, columns, tables, functions],
    };
    Completions { replace: start..cursor, items: rank(groups, prefix) }
}

// ---------------------------------------------------------------------------
// Mongo shell

const MONGO_COLLECTION_METHODS: &[(&str, &str)] = &[
    ("find", "find({})"),
    ("findOne", "findOne({})"),
    ("aggregate", "aggregate([])"),
    ("countDocuments", "countDocuments({})"),
    ("estimatedDocumentCount", "estimatedDocumentCount()"),
    ("distinct", "distinct(\"\")"),
    ("insertOne", "insertOne({})"),
    ("insertMany", "insertMany([])"),
    ("updateOne", "updateOne({}, {$set: {}})"),
    ("updateMany", "updateMany({}, {$set: {}})"),
    ("replaceOne", "replaceOne({}, {})"),
    ("deleteOne", "deleteOne({})"),
    ("deleteMany", "deleteMany({})"),
    ("createIndex", "createIndex({})"),
    ("getIndexes", "getIndexes()"),
    ("dropIndex", "dropIndex(\"\")"),
    ("drop", "drop()"),
];

const MONGO_CURSOR_METHODS: &[(&str, &str)] = &[
    ("sort", "sort({})"),
    ("limit", "limit()"),
    ("skip", "skip()"),
    ("projection", "projection({})"),
    ("count", "count()"),
    ("toArray", "toArray()"),
    ("pretty", "pretty()"),
];

const MONGO_DB_METHODS: &[(&str, &str)] = &[
    ("getCollection", "getCollection(\"\")"),
    ("runCommand", "runCommand({})"),
    ("adminCommand", "adminCommand({})"),
    ("createCollection", "createCollection(\"\")"),
];

const MONGO_OPERATORS: &[(&str, &str)] = &[
    ("$eq", "query"), ("$ne", "query"), ("$gt", "query"), ("$gte", "query"), ("$lt", "query"),
    ("$lte", "query"), ("$in", "query"), ("$nin", "query"), ("$exists", "query"), ("$regex", "query"),
    ("$and", "query"), ("$or", "query"), ("$nor", "query"), ("$not", "query"), ("$elemMatch", "query"),
    ("$size", "query"), ("$type", "query"), ("$set", "update"), ("$unset", "update"), ("$inc", "update"),
    ("$push", "update"), ("$pull", "update"), ("$addToSet", "update"), ("$rename", "update"),
    ("$match", "stage"), ("$group", "stage"), ("$sort", "stage"), ("$project", "stage"),
    ("$limit", "stage"), ("$skip", "stage"), ("$lookup", "stage"), ("$unwind", "stage"),
    ("$count", "stage"), ("$addFields", "stage"), ("$facet", "stage"), ("$sum", "accumulator"),
    ("$avg", "accumulator"), ("$min", "accumulator"), ("$max", "accumulator"),
    ("$first", "accumulator"), ("$last", "accumulator"), ("$push", "accumulator"),
];

/// What the Mongo completer needs from the app.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MongoNeed {
    /// Field paths of this collection (call [`mongo_shell`] again once known).
    Fields(String),
}

/// The collection referenced by the last `db.<name>.` / `db.getCollection("x")`
/// before `cursor`.
pub fn mongo_collection_before(text: &str, cursor: usize) -> Option<String> {
    let before = &text[..cursor];
    let mut best: Option<(usize, String)> = None;
    let mut search = 0;
    while let Some(pos) = before[search..].find("db.") {
        let at = search + pos;
        search = at + 3;
        let rest = &before[at + 3..];
        if let Some(arg) = rest.strip_prefix("getCollection(") {
            let q = arg.chars().next();
            if let Some(q @ ('"' | '\'')) = q
                && let Some(end) = arg[1..].find(q) {
                    best = Some((at, arg[1..1 + end].to_string()));
                }
            continue;
        }
        let name: String = rest.chars().take_while(|c| is_word(*c)).collect();
        if !name.is_empty() && rest[name.len()..].starts_with('.') {
            best = Some((at, name));
        }
    }
    best.map(|(_, n)| n)
}

/// MongoDB's name for a value kind (`objectId`, `string`, …).
pub fn mongo_kind_label(kind: ValueKind) -> &'static str {
    match kind {
        ValueKind::Text => "string",
        ValueKind::Number => "number",
        ValueKind::Bool => "bool",
        ValueKind::DateTime => "date",
        ValueKind::Json => "object",
        ValueKind::Array => "array",
        ValueKind::Uuid => "objectId",
        ValueKind::Binary => "binData",
        ValueKind::Other => "value",
    }
}

fn js_key(path: &str) -> String {
    let plain = path.chars().next().is_some_and(|c| c.is_alphabetic() || c == '_' || c == '$')
        && path.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '$');
    if plain { path.to_string() } else { format!("{path:?}") }
}

/// Mongo console completions. `fields(collection)` returns known field paths
/// (with kinds); when a collection's fields aren't known yet, the result says
/// so via the returned [`MongoNeed`].
pub fn mongo_shell(
    text: &str,
    cursor: usize,
    collections: &[String],
    fields: &HashMap<String, Vec<(String, ValueKind)>>,
    default_collection: Option<&str>,
    invoked: bool,
) -> (Completions, Option<MongoNeed>) {
    let cursor = cursor.min(text.len());
    let start = word_start(text, cursor);
    let prefix = &text[start..cursor];
    let before = &text[..start];
    let done = |items: Vec<Suggestion>| (Completions { replace: start..cursor, items }, None);

    if in_js_string(before) {
        return done(vec![]);
    }
    let method = |(name, insert): &(&str, &str)| {
        Suggestion::new(*name, SuggestionKind::Method).insert(*insert)
    };

    // `db.` → collections and db methods.
    if before.ends_with("db.") && !before[..before.len() - 3].ends_with(|c: char| is_word(c) || c == '.') {
        let mut items: Vec<Suggestion> = collections
            .iter()
            .map(|c| {
                let s = Suggestion::new(c.clone(), SuggestionKind::Collection);
                if c.chars().all(is_word) { s } else { s.insert(format!("getCollection({c:?})")) }
            })
            .collect();
        items.extend(MONGO_DB_METHODS.iter().map(method));
        return done(rank(vec![items], prefix));
    }
    // `db.coll.` / `db.getCollection("x").` → collection methods.
    let trimmed = before.trim_end_matches('.');
    if before.ends_with('.') && (is_collection_ref(trimmed)) {
        return done(rank(vec![MONGO_COLLECTION_METHODS.iter().map(method).collect()], prefix));
    }
    // `).` → cursor methods.
    if before.ends_with(").") {
        return done(rank(vec![MONGO_CURSOR_METHODS.iter().map(method).collect()], prefix));
    }
    // Operators.
    if prefix.starts_with('$') {
        let ops = MONGO_OPERATORS
            .iter()
            .map(|(op, d)| Suggestion::new(*op, SuggestionKind::Operator).detail(*d))
            .collect();
        return done(rank(vec![ops], prefix));
    }
    // Field names at a key position: `{ na|` or `, na|` inside an object.
    let key_position = before.trim_end().ends_with(['{', ',']) && depth_in_braces(before) > 0;
    if key_position && (!prefix.is_empty() || invoked) {
        let coll = mongo_collection_before(text, cursor).or_else(|| default_collection.map(str::to_string));
        if let Some(coll) = coll {
            match fields.get(&coll) {
                Some(list) => {
                    let items = list
                        .iter()
                        .map(|(path, kind)| {
                            Suggestion::new(path.clone(), SuggestionKind::Column)
                                .insert(js_key(path))
                                .detail(format!("{} · {coll}", mongo_kind_label(*kind)))
                        })
                        .collect();
                    return done(rank(vec![items], prefix));
                }
                None => return (Completions { replace: start..cursor, items: vec![] }, Some(MongoNeed::Fields(coll))),
            }
        }
    }
    // Start of a command.
    if (!prefix.is_empty() || invoked) && (before.trim().is_empty() || before.trim_end().ends_with([';', '\n'])) {
        let items = vec![
            Suggestion::new("db", SuggestionKind::Keyword).insert("db."),
            Suggestion::new("show collections", SuggestionKind::Keyword),
            Suggestion::new("show dbs", SuggestionKind::Keyword),
        ];
        return done(rank(vec![items], prefix));
    }
    done(vec![])
}

fn is_collection_ref(s: &str) -> bool {
    if let Some(rest) = s.strip_suffix(')') {
        // db.getCollection("x")
        return rest.rfind("db.getCollection(").is_some_and(|i| !rest[i..].contains('{'));
    }
    let tail: String = s.chars().rev().take_while(|c| is_word(*c)).collect::<String>();
    let name_len = tail.len();
    name_len > 0 && s[..s.len() - name_len].ends_with("db.") && {
        let head = &s[..s.len() - name_len - 3];
        !head.ends_with(|c: char| is_word(c) || c == '.')
    }
}

fn in_js_string(before: &str) -> bool {
    let mut quote: Option<char> = None;
    let mut escaped = false;
    for c in before.chars() {
        match quote {
            Some(q) => {
                if escaped {
                    escaped = false;
                } else if c == '\\' {
                    escaped = true;
                } else if c == q {
                    quote = None;
                }
            }
            None if c == '"' || c == '\'' => quote = Some(c),
            None => {}
        }
    }
    quote.is_some()
}

fn depth_in_braces(before: &str) -> i32 {
    let mut depth = 0;
    let mut quote: Option<char> = None;
    for c in before.chars() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => {}
            None => match c {
                '"' | '\'' => quote = Some(c),
                '{' => depth += 1,
                '}' => depth -= 1,
                _ => {}
            },
        }
    }
    depth
}

// ---------------------------------------------------------------------------
// Filter bar

/// A field the filter can reference.
#[derive(Clone, Debug, PartialEq)]
pub struct FilterField {
    pub name: String,
    pub kind: ValueKind,
    pub type_name: String,
}

#[derive(Debug, Clone, PartialEq)]
enum FTok {
    Ident(String),
    Str,
    Num,
    Op(String),
    Open,
    Close,
}

/// Tokenize filter text; `None` when it ends inside a string.
fn filter_tokens(text: &str) -> Option<Vec<FTok>> {
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            i += 1;
        } else if c == '"' || c == '\'' {
            i += 1;
            loop {
                if i >= chars.len() {
                    return None;
                }
                if chars[i] == '\\' {
                    i += 2;
                    continue;
                }
                if chars[i] == c {
                    i += 1;
                    break;
                }
                i += 1;
            }
            out.push(FTok::Str);
        } else if c == '`' {
            let start = i + 1;
            i += 1;
            while i < chars.len() && chars[i] != '`' {
                i += 1;
            }
            if i >= chars.len() {
                return None;
            }
            out.push(FTok::Ident(chars[start..i].iter().collect()));
            i += 1;
        } else if c.is_ascii_digit() || (c == '-' && chars.get(i + 1).is_some_and(char::is_ascii_digit)) {
            i += 1;
            while i < chars.len() && (chars[i].is_ascii_digit() || chars[i] == '.' || chars[i] == '_') {
                i += 1;
            }
            out.push(FTok::Num);
        } else if c.is_alphabetic() || c == '_' || c == '$' {
            let start = i;
            while i < chars.len() && (is_word(chars[i]) || chars[i] == '.') {
                i += 1;
            }
            out.push(FTok::Ident(chars[start..i].iter().collect()));
        } else if c == '(' || c == '[' {
            out.push(FTok::Open);
            i += 1;
        } else if c == ')' || c == ']' {
            out.push(FTok::Close);
            i += 1;
        } else if c == '/' {
            // regex literal
            i += 1;
            while i < chars.len() && chars[i] != '/' {
                i += 1;
            }
            if i >= chars.len() {
                return None;
            }
            i += 1;
            while i < chars.len() && chars[i].is_alphabetic() {
                i += 1;
            }
            out.push(FTok::Str);
        } else {
            let start = i;
            while i < chars.len() && "=!<>~^$&|,".contains(chars[i]) {
                i += 1;
            }
            if i == start {
                i += 1;
            }
            out.push(FTok::Op(chars[start..i].iter().collect()));
        }
    }
    Some(out)
}

const CONNECTIVES: &[&str] = &["and", "or", "&&", "||"];
const VALUE_WORDS: &[&str] = &["null", "true", "false"];

fn operators_for(kind: ValueKind, mongo: bool) -> Vec<Suggestion> {
    let op = |label: &str, insert: &str, detail: &str| {
        Suggestion::new(label, SuggestionKind::Operator).insert(insert).detail(detail)
    };
    let mut ops = vec![op("=", "= ", "equals"), op("!=", "!= ", "not equal")];
    match kind {
        ValueKind::Number | ValueKind::DateTime => ops.extend([
            op(">", "> ", "greater"),
            op(">=", ">= ", "greater or equal"),
            op("<", "< ", "less"),
            op("<=", "<= ", "less or equal"),
            op("between", "between ", "inclusive range"),
        ]),
        ValueKind::Bool => {}
        _ => ops.extend([
            op("~", "~ \"", "contains (any case)"),
            op("!~", "!~ \"", "does not contain"),
            op("^=", "^= \"", "starts with"),
            op("$=", "$= \"", "ends with"),
            op("like", "like \"", "SQL wildcards % _"),
            op("ilike", "ilike \"", "like, any case"),
            op("matches", "matches \"", "regular expression"),
            op(">", "> ", "greater"),
            op("<", "< ", "less"),
        ]),
    }
    ops.extend([
        op("in", "in [", "one of"),
        op("not in", "not in [", "none of"),
        op("is null", "is null", "missing / NULL"),
        op("is not null", "is not null", "has a value"),
    ]);
    if mongo {
        ops.extend([op("exists", "exists", "field present"), op("not exists", "not exists", "field absent")]);
    }
    ops
}

/// Filter-bar completions: fields at the start of a condition, operators
/// after a field, `and`/`or` after a value.
pub fn filter(text: &str, cursor: usize, fields: &[FilterField], db: DbKind, invoked: bool) -> Completions {
    let cursor = cursor.min(text.len());
    // Filter paths include dots: `address.city`.
    let start = text[..cursor]
        .char_indices()
        .rev()
        .take_while(|(_, c)| is_word(*c) || *c == '.')
        .last()
        .map_or(cursor, |(i, _)| i);
    let prefix = &text[start..cursor];
    let empty = Completions { replace: start..cursor, items: vec![] };
    let Some(tokens) = filter_tokens(&text[..start]) else {
        return empty; // inside a string
    };
    let mongo = db == DbKind::Mongo;
    let field_items = || -> Vec<Suggestion> {
        fields
            .iter()
            .map(|f| {
                let plain = f.name.chars().all(|c| is_word(c) || (mongo && c == '.'));
                let insert = if plain { f.name.clone() } else { format!("`{}`", f.name.replace('`', "")) };
                Suggestion::new(f.name.clone(), SuggestionKind::Column)
                    .insert(insert)
                    .detail(f.type_name.clone())
            })
            .collect()
    };
    let words = |list: &[&str], kind: SuggestionKind| -> Vec<Suggestion> {
        list.iter().map(|w| Suggestion::new(*w, kind)).collect()
    };
    let field_kind = |name: &str| fields.iter().find(|f| f.name == name).map(|f| f.kind);

    let last = tokens.last();
    let expect_field = match last {
        None | Some(FTok::Open) => true,
        Some(FTok::Ident(w)) => {
            let w = w.to_lowercase();
            CONNECTIVES.contains(&w.as_str()) || w == "not"
        }
        Some(FTok::Op(o)) => o == "&&" || o == "||" || o == "!",
        _ => false,
    };
    if expect_field {
        if prefix.is_empty() && !invoked {
            return empty;
        }
        let mut items = field_items();
        if !matches!(last, Some(FTok::Ident(w)) if w.eq_ignore_ascii_case("not")) {
            items.push(Suggestion::new("not", SuggestionKind::Keyword));
        }
        return Completions { replace: start..cursor, items: rank(vec![items], prefix) };
    }

    match last {
        // After a field: operators.
        Some(FTok::Ident(w)) if !is_filter_keyword(w) => {
            let kind = field_kind(w).unwrap_or_default();
            Completions { replace: start..cursor, items: rank(vec![operators_for(kind, mongo)], prefix) }
        }
        Some(FTok::Ident(w)) if w.eq_ignore_ascii_case("is") => Completions {
            replace: start..cursor,
            items: rank(vec![words(&["null", "not null"], SuggestionKind::Value)], prefix),
        },
        // After an operator: suggest literal helpers for some types.
        Some(FTok::Op(_)) => {
            let field = tokens.iter().rev().find_map(|t| match t {
                FTok::Ident(w) if !is_filter_keyword(w) => Some(w.clone()),
                _ => None,
            });
            let kind = field.as_deref().and_then(field_kind).unwrap_or_default();
            let items = match kind {
                ValueKind::Bool => words(&["true", "false"], SuggestionKind::Value),
                ValueKind::Uuid if mongo => vec![Suggestion::new("oid(\"…\")", SuggestionKind::Function).insert("oid(\"")],
                ValueKind::DateTime => vec![Suggestion::new("date(\"…\")", SuggestionKind::Function).insert("date(\"")],
                _ => vec![],
            };
            if prefix.is_empty() && !invoked && kind != ValueKind::Bool {
                return empty;
            }
            Completions { replace: start..cursor, items: rank(vec![items], prefix) }
        }
        // After a value: connectives.
        Some(FTok::Str | FTok::Num | FTok::Close) | Some(FTok::Ident(_)) => {
            if prefix.is_empty() && !invoked {
                return empty;
            }
            let between = tokens.len() >= 3
                && matches!(&tokens[tokens.len() - 2], FTok::Ident(w) if w.eq_ignore_ascii_case("between"));
            let list: &[&str] = if between { &["and"] } else { &["and", "or"] };
            Completions { replace: start..cursor, items: rank(vec![words(list, SuggestionKind::Keyword)], prefix) }
        }
        _ => empty,
    }
}

fn is_filter_keyword(w: &str) -> bool {
    let w = w.to_lowercase();
    CONNECTIVES.contains(&w.as_str())
        || VALUE_WORDS.contains(&w.as_str())
        || ["not", "in", "is", "like", "ilike", "matches", "between", "exists"].contains(&w.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn catalog() -> Catalog {
        let col = |n: &str, t: &str| CatalogColumn { name: n.into(), type_name: t.into() };
        Catalog {
            tables: vec![
                CatalogTable {
                    schema: "public".into(),
                    name: "customers".into(),
                    view: false,
                    columns: vec![col("id", "integer"), col("email", "text"), col("country", "character(2)")],
                },
                CatalogTable {
                    schema: "public".into(),
                    name: "orders".into(),
                    view: false,
                    columns: vec![col("id", "bigint"), col("customer_id", "integer"), col("total", "numeric")],
                },
                CatalogTable {
                    schema: "analytics".into(),
                    name: "events".into(),
                    view: false,
                    columns: vec![col("kind", "text")],
                },
                CatalogTable { schema: "public".into(), name: "My Table".into(), view: true, columns: vec![] },
            ],
        }
    }

    fn labels(c: &Completions) -> Vec<&str> {
        c.items.iter().map(|s| s.label.as_str()).collect()
    }

    fn at_end(text: &str) -> usize {
        text.len()
    }

    #[test]
    fn sql_tables_after_from() {
        let t = "select * from cu";
        let c = sql(t, at_end(t), &catalog(), false);
        assert_eq!(labels(&c)[0], "customers");
        assert_eq!(c.replace, 14..16);
        let t = "SELECT * FROM ";
        assert!(sql(t, at_end(t), &catalog(), false).items.is_empty());
        let invoked = sql(t, at_end(t), &catalog(), true);
        assert!(labels(&invoked).contains(&"analytics.events"));
        assert!(labels(&invoked).contains(&"My Table"));
        let q = invoked.items.iter().find(|s| s.label == "My Table").unwrap();
        assert_eq!(q.insert, "\"My Table\"");
    }

    #[test]
    fn sql_columns_from_aliases() {
        let t = "select o.";
        let c = sql(t, at_end(t), &catalog(), false);
        assert!(c.items.is_empty(), "unknown alias yet");
        let t = "select o.to from orders o";
        let c = sql(t, 11, &catalog(), false);
        assert_eq!(labels(&c), vec!["total"]);
        let t = "select c.e from customers as c join orders o on o.customer_id = c.id";
        let c = sql(t, 10, &catalog(), false);
        assert_eq!(labels(&c), vec!["email"]);
        let t = "select * from orders where cust";
        let c = sql(t, at_end(t), &catalog(), false);
        assert_eq!(labels(&c)[0], "customer_id");
        // `id` matches at a word boundary of customer_id too.
        let t = "select * from orders where id";
        let c = sql(t, at_end(t), &catalog(), false);
        assert!(labels(&c).contains(&"customer_id"));
        let t = "select * from analytics.";
        assert_eq!(labels(&sql(t, at_end(t), &catalog(), false)), vec!["events"]);
    }

    #[test]
    fn sql_keywords_keep_case() {
        let t = "sel";
        assert_eq!(labels(&sql(t, 3, &catalog(), false))[0], "select");
        let t = "SEL";
        assert_eq!(labels(&sql(t, 3, &catalog(), false))[0], "SELECT");
        let t = "select * from orders gro";
        assert!(labels(&sql(t, at_end(t), &catalog(), false)).contains(&"group by"));
    }

    #[test]
    fn sql_not_in_strings_or_comments() {
        let t = "select 'cu";
        assert!(sql(t, at_end(t), &catalog(), false).items.is_empty());
        let t = "-- cu";
        assert!(sql(t, at_end(t), &catalog(), false).items.is_empty());
        // Earlier statements don't leak aliases.
        let t = "select * from orders o; select o.";
        assert!(sql(t, at_end(t), &catalog(), false).items.is_empty());
    }

    fn mongo(text: &str, fields: &HashMap<String, Vec<(String, ValueKind)>>) -> (Completions, Option<MongoNeed>) {
        let colls = vec!["users".to_string(), "orders".to_string(), "weird-name".to_string()];
        mongo_shell(text, text.len(), &colls, fields, Some("users"), false)
    }

    #[test]
    fn mongo_completions() {
        let none = HashMap::new();
        assert_eq!(labels(&mongo("db.us", &none).0), vec!["users"]);
        let w = mongo("db.we", &none).0;
        assert_eq!(w.items[0].insert, "getCollection(\"weird-name\")");
        assert_eq!(labels(&mongo("db.users.fi", &none).0), vec!["find", "findOne"]);
        assert_eq!(mongo("db.users.fi", &none).0.items[0].insert, "find({})");
        assert!(labels(&mongo("db.getCollection(\"users\").agg", &none).0).contains(&"aggregate"));
        assert!(labels(&mongo("db.users.find({}).so", &none).0).contains(&"sort"));
        assert!(labels(&mongo("db.users.find({age: {$g", &none).0).contains(&"$gt"));
        // Field names need the collection's fields first…
        let (c, need) = mongo("db.orders.find({sta", &none);
        assert!(c.items.is_empty());
        assert_eq!(need, Some(MongoNeed::Fields("orders".into())));
        // …then complete, quoting dotted paths.
        let mut f = HashMap::new();
        f.insert("orders".to_string(), vec![("status".to_string(), ValueKind::Text), ("items.sku".to_string(), ValueKind::Text)]);
        assert_eq!(labels(&mongo("db.orders.find({sta", &f).0), vec!["status"]);
        let c = mongo("db.orders.find({status: 'paid', ite", &f).0;
        assert_eq!(c.items[0].insert, "\"items.sku\"");
        // Inside strings: nothing.
        assert!(mongo("db.orders.find({status: 'pa", &f).0.items.is_empty());
        // Bare filter uses the default collection.
        let (_, need) = mongo("{na", &none);
        assert_eq!(need, Some(MongoNeed::Fields("users".into())));
    }

    fn fields() -> Vec<FilterField> {
        let f = |n: &str, k: ValueKind| FilterField { name: n.into(), kind: k, type_name: format!("{k:?}") };
        vec![
            f("status", ValueKind::Text),
            f("total", ValueKind::Number),
            f("vip", ValueKind::Bool),
            f("address.city", ValueKind::Text),
            f("odd name", ValueKind::Text),
        ]
    }

    #[test]
    fn filter_fields_operators_connectives() {
        let pg = DbKind::Postgres;
        let c = filter("st", 2, &fields(), pg, false);
        assert_eq!(labels(&c), vec!["status"]);
        assert!(filter("", 0, &fields(), pg, false).items.is_empty());
        assert!(filter("", 0, &fields(), pg, true).items.len() >= 5);
        // Operators after a field (also right after the space).
        let c = filter("total ", 6, &fields(), pg, false);
        assert_eq!(&labels(&c)[..3], &["=", "!=", ">"], "natural order without a prefix");
        assert!(labels(&c).contains(&">="));
        assert!(!labels(&c).contains(&"~"), "no text operators for numbers");
        let c = filter("status ", 7, &fields(), pg, false);
        assert!(labels(&c).contains(&"~"));
        assert!(!labels(&c).contains(&"exists"), "exists is Mongo only");
        assert!(labels(&filter("status ", 7, &fields(), DbKind::Mongo, false)).contains(&"exists"));
        // Value helpers.
        assert_eq!(labels(&filter("vip = ", 6, &fields(), pg, false)), vec!["true", "false"]);
        assert_eq!(labels(&filter("status is ", 10, &fields(), pg, false)), vec!["null", "not null"]);
        // Connectives after a value.
        let t = "status = \"paid\" a";
        assert_eq!(labels(&filter(t, t.len(), &fields(), pg, false)), vec!["and"]);
        let t = "total between 1 a";
        assert_eq!(labels(&filter(t, t.len(), &fields(), pg, false)), vec!["and"]);
        let t = "total > 5 and to";
        assert_eq!(labels(&filter(t, t.len(), &fields(), pg, false)), vec!["total"]);
        // Paths and quoting.
        let t = "address.c";
        assert_eq!(filter(t, t.len(), &fields(), DbKind::Mongo, false).items[0].insert, "address.city");
        let t = "od";
        assert_eq!(filter(t, t.len(), &fields(), pg, false).items[0].insert, "`odd name`");
        // Inside a string: nothing.
        let t = "status = \"pa";
        assert!(filter(t, t.len(), &fields(), pg, false).items.is_empty());
    }
}
