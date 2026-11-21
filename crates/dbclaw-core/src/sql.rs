//! PostgreSQL text helpers: quoting, statement splitting and generation of
//! the statements behind the data grid (page queries and staged changes).
//!
//! Everything is sent via the simple query protocol, so literals are inlined.
//! Values are always emitted as *untyped* quoted literals and Postgres
//! coerces them to the target column type with the type's input function —
//! the same text representation the grid displays, so edits round-trip.

use std::ops::Range;

use crate::model::{ColumnMeta, NewValue, ObjectRef, RowChange, RowKey, SortDir, SortSpec};

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[error("{0}")]
pub struct SqlError(pub String);

/// Quote an identifier: `my "col"` → `"my ""col"""`.
pub fn quote_ident(ident: &str) -> String {
    let mut out = String::with_capacity(ident.len() + 2);
    out.push('"');
    for ch in ident.chars() {
        if ch == '"' {
            out.push('"');
        }
        out.push(ch);
    }
    out.push('"');
    out
}

/// Quote a string literal (standard_conforming_strings = on).
pub fn quote_literal(value: &str) -> Result<String, SqlError> {
    if value.contains('\0') {
        return Err(SqlError("values cannot contain NUL characters".into()));
    }
    let mut out = String::with_capacity(value.len() + 2);
    out.push('\'');
    for ch in value.chars() {
        if ch == '\'' {
            out.push('\'');
        }
        out.push(ch);
    }
    out.push('\'');
    Ok(out)
}

/// `"schema"."table"`
pub fn qualified_name(object: &ObjectRef) -> String {
    if object.schema.is_empty() {
        quote_ident(&object.name)
    } else {
        format!("{}.{}", quote_ident(&object.schema), quote_ident(&object.name))
    }
}

/// Split a SQL script into statements. Returns byte ranges of the statement
/// text (trimmed, without the terminating `;`), skipping empty statements and
/// statements that consist only of comments.
///
/// Understands single quotes, `E''` strings with backslash escapes, quoted
/// identifiers, dollar quoting (`$tag$ … $tag$`), `--` line comments and
/// nested `/* */` block comments.
pub fn split_statements(script: &str) -> Vec<Range<usize>> {
    let bytes = script.as_bytes();
    let mut out = Vec::new();
    let mut start = 0;
    let mut i = 0;
    let mut has_code = false;

    let push = |out: &mut Vec<Range<usize>>, s: usize, e: usize, has_code: bool| {
        if !has_code {
            return;
        }
        let text = &script[s..e];
        let lead = text.len() - text.trim_start().len();
        let trail = text.len() - text.trim_end().len();
        out.push(s + lead..e - trail);
    };

    while i < bytes.len() {
        let c = bytes[i];
        match c {
            b'-' if bytes.get(i + 1) == Some(&b'-') => {
                // Line comment. If no code was seen yet, move the start past it.
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
                if !has_code {
                    start = i;
                }
                continue;
            }
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                let mut depth = 1;
                i += 2;
                while i < bytes.len() && depth > 0 {
                    if bytes[i] == b'/' && bytes.get(i + 1) == Some(&b'*') {
                        depth += 1;
                        i += 2;
                    } else if bytes[i] == b'*' && bytes.get(i + 1) == Some(&b'/') {
                        depth -= 1;
                        i += 2;
                    } else {
                        i += 1;
                    }
                }
                if !has_code {
                    start = i;
                }
                continue;
            }
            b'\'' => {
                has_code = true;
                let escapes = i > 0
                    && matches!(bytes[i - 1], b'E' | b'e')
                    && (i < 2 || !is_ident_byte(bytes[i - 2]));
                i += 1;
                while i < bytes.len() {
                    if escapes && bytes[i] == b'\\' {
                        i += 2;
                        continue;
                    }
                    if bytes[i] == b'\'' {
                        if bytes.get(i + 1) == Some(&b'\'') {
                            i += 2;
                            continue;
                        }
                        break;
                    }
                    i += 1;
                }
                i += 1;
                continue;
            }
            b'"' => {
                has_code = true;
                i += 1;
                while i < bytes.len() {
                    if bytes[i] == b'"' {
                        if bytes.get(i + 1) == Some(&b'"') {
                            i += 2;
                            continue;
                        }
                        break;
                    }
                    i += 1;
                }
                i += 1;
                continue;
            }
            b'$' => {
                has_code = true;
                // Dollar quote: $tag$ where tag is empty or an identifier not starting with a digit.
                let preceded_by_ident = i > 0 && is_ident_byte(bytes[i - 1]);
                if !preceded_by_ident {
                    let mut j = i + 1;
                    while j < bytes.len() && is_ident_byte(bytes[j]) {
                        j += 1;
                    }
                    let tag_ok = j < bytes.len()
                        && bytes[j] == b'$'
                        && !(j > i + 1 && bytes[i + 1].is_ascii_digit());
                    if tag_ok {
                        let tag = &script[i..=j];
                        if let Some(end) = script[j + 1..].find(tag) {
                            i = j + 1 + end + tag.len();
                        } else {
                            i = bytes.len();
                        }
                        continue;
                    }
                }
                i += 1;
                continue;
            }
            b';' => {
                push(&mut out, start, i, has_code);
                i += 1;
                start = i;
                has_code = false;
                continue;
            }
            c if c.is_ascii_whitespace() => {
                i += 1;
                continue;
            }
            _ => {
                has_code = true;
                i += 1;
            }
        }
    }
    push(&mut out, start, bytes.len(), has_code);
    out
}

fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b >= 0x80
}

/// Find the statement containing the byte offset `cursor` (or the closest one
/// before it), for "run statement at cursor".
pub fn statement_at(script: &str, cursor: usize) -> Option<Range<usize>> {
    let stmts = split_statements(script);
    let mut best = None;
    for r in stmts {
        if r.start <= cursor {
            best = Some(r.clone());
            // Cursor inside the statement or within its trailing `;`/whitespace.
            if cursor <= r.end + 1 {
                return best;
            }
        } else {
            return best.or(Some(r));
        }
    }
    best
}

fn order_by(sort: &[SortSpec], columns: &[ColumnMeta]) -> String {
    let specs: Vec<String> = if sort.is_empty() {
        // Stable paging: order by the primary key when there is one.
        columns
            .iter()
            .filter(|c| c.is_pk)
            .map(|c| quote_ident(&c.name))
            .collect()
    } else {
        sort.iter()
            .map(|s| {
                format!(
                    "{} {}",
                    quote_ident(&s.column),
                    match s.dir {
                        SortDir::Asc => "ASC NULLS FIRST",
                        SortDir::Desc => "DESC NULLS LAST",
                    }
                )
            })
            .collect()
    };
    if specs.is_empty() {
        String::new()
    } else {
        format!(" ORDER BY {}", specs.join(", "))
    }
}

/// Page query for the data grid. When `with_ctid` is set, `ctid` is selected
/// as the *last* column.
pub fn select_page(
    object: &ObjectRef,
    columns: &[ColumnMeta],
    where_sql: Option<&str>,
    sort: &[SortSpec],
    with_ctid: bool,
    offset: u64,
    limit: u64,
) -> String {
    let mut cols: Vec<String> = columns.iter().map(|c| quote_ident(&c.name)).collect();
    if cols.is_empty() {
        cols.push("*".into());
    }
    if with_ctid {
        cols.push("ctid".into());
    }
    let mut sql = format!("SELECT {} FROM {}", cols.join(", "), qualified_name(object));
    if let Some(w) = where_sql {
        sql.push_str(" WHERE ");
        sql.push_str(w);
    }
    sql.push_str(&order_by(sort, columns));
    sql.push_str(&format!(" LIMIT {limit} OFFSET {offset}"));
    sql
}

pub fn count_rows(object: &ObjectRef, where_sql: Option<&str>) -> String {
    let mut sql = format!("SELECT count(*) FROM {}", qualified_name(object));
    if let Some(w) = where_sql {
        sql.push_str(" WHERE ");
        sql.push_str(w);
    }
    sql
}

fn value_sql(v: &NewValue) -> Result<String, SqlError> {
    match v {
        NewValue::Text(Some(s)) => quote_literal(s),
        NewValue::Text(None) => Ok("NULL".into()),
        NewValue::Default => Ok("DEFAULT".into()),
        NewValue::Bson(_) | NewValue::Unset => {
            Err(SqlError("document values are not valid for SQL tables".into()))
        }
    }
}

fn where_key(key: &RowKey) -> Result<String, SqlError> {
    match key {
        RowKey::Pk(cols) => {
            if cols.is_empty() {
                return Err(SqlError("row has no key".into()));
            }
            let parts = cols
                .iter()
                .map(|(c, v)| match v {
                    Some(v) => Ok(format!("{} = {}", quote_ident(c), quote_literal(v)?)),
                    None => Ok(format!("{} IS NULL", quote_ident(c))),
                })
                .collect::<Result<Vec<_>, SqlError>>()?;
            Ok(parts.join(" AND "))
        }
        RowKey::Ctid(ctid) => Ok(format!("ctid = {}", quote_literal(ctid)?)),
        RowKey::Id(_) => Err(SqlError("document ids are not valid for SQL tables".into())),
    }
}

/// SQL for one staged change. Updates and deletes must affect exactly one
/// row; the caller verifies the command tag.
pub fn change_statement(object: &ObjectRef, change: &RowChange) -> Result<String, SqlError> {
    let table = qualified_name(object);
    match change {
        RowChange::Update { key, values } => {
            if values.is_empty() {
                return Err(SqlError("nothing to update".into()));
            }
            let sets = values
                .iter()
                .map(|(c, v)| Ok(format!("{} = {}", quote_ident(c), value_sql(v)?)))
                .collect::<Result<Vec<_>, SqlError>>()?;
            Ok(format!("UPDATE {table} SET {} WHERE {}", sets.join(", "), where_key(key)?))
        }
        RowChange::Insert { values } => {
            let explicit: Vec<&(String, NewValue)> =
                values.iter().filter(|(_, v)| *v != NewValue::Default).collect();
            if explicit.is_empty() {
                return Ok(format!("INSERT INTO {table} DEFAULT VALUES"));
            }
            let cols: Vec<String> = explicit.iter().map(|(c, _)| quote_ident(c)).collect();
            let vals = explicit
                .iter()
                .map(|(_, v)| value_sql(v))
                .collect::<Result<Vec<_>, SqlError>>()?;
            Ok(format!(
                "INSERT INTO {table} ({}) VALUES ({})",
                cols.join(", "),
                vals.join(", ")
            ))
        }
        RowChange::Delete { key } => Ok(format!("DELETE FROM {table} WHERE {}", where_key(key)?)),
        RowChange::Replace { .. } | RowChange::InsertDocument { .. } => {
            Err(SqlError("document changes are not valid for SQL tables".into()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ObjectKind;

    fn obj() -> ObjectRef {
        ObjectRef {
            database: "shop".into(),
            schema: "public".into(),
            name: "my table".into(),
            kind: ObjectKind::Table,
        }
    }

    fn col(name: &str, pk: bool) -> ColumnMeta {
        ColumnMeta { name: name.into(), is_pk: pk, ..Default::default() }
    }

    #[test]
    fn quoting() {
        assert_eq!(quote_ident("a\"b"), "\"a\"\"b\"");
        assert_eq!(quote_literal("it's").unwrap(), "'it''s'");
        assert_eq!(quote_literal("back\\slash").unwrap(), "'back\\slash'");
        assert!(quote_literal("nul\0").is_err());
        assert_eq!(quote_literal("'; drop table x; --").unwrap(), "'''; drop table x; --'");
    }

    fn split(s: &str) -> Vec<&str> {
        split_statements(s).into_iter().map(|r| &s[r]).collect()
    }

    #[test]
    fn splitting() {
        assert_eq!(split("select 1; select 2;"), vec!["select 1", "select 2"]);
        assert_eq!(split("select ';'; select \"a;b\""), vec!["select ';'", "select \"a;b\""]);
        assert_eq!(
            split("create function f() returns int as $$ begin return 1; end $$ language plpgsql; select 1"),
            vec![
                "create function f() returns int as $$ begin return 1; end $$ language plpgsql",
                "select 1"
            ]
        );
        assert_eq!(split("select $tag$ ; $tag$"), vec!["select $tag$ ; $tag$"]);
        assert_eq!(split("-- comment; here\nselect 1"), vec!["select 1"]);
        assert_eq!(split("/* a; /* nested; */ b; */ select 1;;  ;"), vec!["select 1"]);
        assert_eq!(split("select E'it\\'s;'; select 2"), vec!["select E'it\\'s;'", "select 2"]);
        assert_eq!(split("select 'a''b;c'"), vec!["select 'a''b;c'"]);
        assert_eq!(split("select $1; select 2"), vec!["select $1", "select 2"]);
        assert_eq!(split("   \n  "), Vec::<&str>::new());
        assert_eq!(split("-- only a comment"), Vec::<&str>::new());
    }

    #[test]
    fn statement_under_cursor() {
        let s = "select 1;\nselect 2;\n\nselect 3";
        assert_eq!(&s[statement_at(s, 0).unwrap()], "select 1");
        assert_eq!(&s[statement_at(s, 12).unwrap()], "select 2");
        assert_eq!(&s[statement_at(s, 19).unwrap()], "select 2");
        assert_eq!(&s[statement_at(s, s.len()).unwrap()], "select 3");
    }

    #[test]
    fn page_query() {
        let cols = vec![col("id", true), col("name", false)];
        assert_eq!(
            select_page(&obj(), &cols, None, &[], false, 0, 100),
            "SELECT \"id\", \"name\" FROM \"public\".\"my table\" ORDER BY \"id\" LIMIT 100 OFFSET 0"
        );
        let sort = vec![SortSpec { column: "name".into(), dir: SortDir::Desc }];
        assert_eq!(
            select_page(&obj(), &cols, Some("\"id\" > '5'"), &sort, true, 200, 100),
            "SELECT \"id\", \"name\", ctid FROM \"public\".\"my table\" WHERE \"id\" > '5' ORDER BY \"name\" DESC NULLS LAST LIMIT 100 OFFSET 200"
        );
    }

    #[test]
    fn changes() {
        let o = obj();
        let upd = RowChange::Update {
            key: RowKey::Pk(vec![("id".into(), Some("7".into()))]),
            values: vec![
                ("name".into(), NewValue::Text(Some("O'Brien".into()))),
                ("note".into(), NewValue::Text(None)),
            ],
        };
        assert_eq!(
            change_statement(&o, &upd).unwrap(),
            "UPDATE \"public\".\"my table\" SET \"name\" = 'O''Brien', \"note\" = NULL WHERE \"id\" = '7'"
        );
        let ins = RowChange::Insert {
            values: vec![
                ("id".into(), NewValue::Default),
                ("name".into(), NewValue::Text(Some("x".into()))),
            ],
        };
        assert_eq!(
            change_statement(&o, &ins).unwrap(),
            "INSERT INTO \"public\".\"my table\" (\"name\") VALUES ('x')"
        );
        let all_default = RowChange::Insert { values: vec![("id".into(), NewValue::Default)] };
        assert_eq!(
            change_statement(&o, &all_default).unwrap(),
            "INSERT INTO \"public\".\"my table\" DEFAULT VALUES"
        );
        let del = RowChange::Delete { key: RowKey::Ctid("(0,1)".into()) };
        assert_eq!(
            change_statement(&o, &del).unwrap(),
            "DELETE FROM \"public\".\"my table\" WHERE ctid = '(0,1)'"
        );
        let composite = RowChange::Delete {
            key: RowKey::Pk(vec![("a".into(), Some("1".into())), ("b".into(), None)]),
        };
        assert_eq!(
            change_statement(&o, &composite).unwrap(),
            "DELETE FROM \"public\".\"my table\" WHERE \"a\" = '1' AND \"b\" IS NULL"
        );
    }
}
