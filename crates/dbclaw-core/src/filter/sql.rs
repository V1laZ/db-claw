//! [`Expr`] → PostgreSQL boolean expression with every literal inlined.
//!
//! The output is meant for the simple query protocol (no bind parameters),
//! so all quoting happens here: literals become `'…'` with `'` doubled
//! (assuming `standard_conforming_strings = on`, the default since 9.1) and
//! identifiers become `"…"` with `"` doubled. NUL bytes are rejected.

use super::{CompareOp, Expr, Field, FilterError, Literal, SqlColumn};
use std::ops::Range;

/// Compile to a PostgreSQL boolean expression (no leading `WHERE`).
///
/// `columns` lists the table's columns with their PostgreSQL type names; it
/// is used to validate field names and to detect `json`/`jsonb` and array
/// columns. Pass an empty slice to skip validation.
pub fn to_sql(expr: &Expr, columns: &[SqlColumn]) -> Result<String, FilterError> {
    Compiler { columns }.expr(expr)
}

type CResult<T> = Result<T, FilterError>;

struct Compiler<'a> {
    columns: &'a [SqlColumn],
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Scalar,
    Array,
    Json,
}

/// What a field resolved to.
enum Target {
    /// A plain column: `"col"`.
    Column { ident: String, kind: Kind },
    /// A path inside a json/jsonb column. `text` is `("col" #>> '{a,b}')`,
    /// `json` is `("col" #> '{a,b}')`.
    JsonPath { text: String, json: String },
}

impl Target {
    /// Expression to compare directly against a literal.
    fn value_expr(&self) -> &str {
        match self {
            Target::Column { ident, .. } => ident,
            Target::JsonPath { text, .. } => text,
        }
    }

    /// Expression usable with LIKE / regex operators.
    fn text_expr(&self) -> String {
        match self {
            Target::Column { ident, .. } => format!("{ident}::text"),
            Target::JsonPath { text, .. } => text.clone(),
        }
    }
}

fn column_kind(data_type: &str) -> Kind {
    let t = data_type.trim().to_ascii_lowercase();
    if t == "json" || t == "jsonb" {
        Kind::Json
    } else if t == "array" || t.starts_with('_') || t.ends_with("[]") {
        Kind::Array
    } else {
        Kind::Scalar
    }
}

/// `'…'` with quotes doubled. Errors on NUL, which PostgreSQL can't store.
pub(crate) fn quote_literal(s: &str, span: &Range<usize>) -> CResult<String> {
    if s.contains('\0') {
        return Err(FilterError::new(
            "text values can't contain NUL (\\u0000) characters",
            span.clone(),
        ));
    }
    Ok(format!("'{}'", s.replace('\'', "''")))
}

/// `"…"` with quotes doubled.
pub(crate) fn quote_ident(s: &str, span: &Range<usize>) -> CResult<String> {
    if s.contains('\0') {
        return Err(FilterError::new(
            "names can't contain NUL (\\u0000) characters",
            span.clone(),
        ));
    }
    Ok(format!("\"{}\"", s.replace('"', "\"\"")))
}

/// One element of a PostgreSQL text-array literal (`{a,"b c"}`).
fn array_elem(s: &str) -> String {
    let needs_quotes = s.is_empty()
        || s.eq_ignore_ascii_case("null")
        || s.chars()
            .any(|c| matches!(c, '{' | '}' | ',' | '"' | '\\') || c.is_whitespace());
    if needs_quotes {
        let mut out = String::with_capacity(s.len() + 2);
        out.push('"');
        for c in s.chars() {
            if c == '"' || c == '\\' {
                out.push('\\');
            }
            out.push(c);
        }
        out.push('"');
        out
    } else {
        s.to_string()
    }
}

/// Escape LIKE wildcards so the value matches literally (escape char `\`).
fn like_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if matches!(c, '%' | '_' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0; b.len() + 1];
    for i in 1..=a.len() {
        cur[0] = i;
        for j in 1..=b.len() {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            cur[j] = (prev[j] + 1).min(cur[j - 1] + 1).min(prev[j - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

fn is_numeric(lit: &Literal) -> bool {
    matches!(lit, Literal::Int(_) | Literal::Float(_))
}

/// Unquoted numeric text (only for numbers we formatted ourselves).
fn number_text(lit: &Literal) -> String {
    match lit {
        Literal::Int(i) => i.to_string(),
        Literal::Float(x) => x.to_string(),
        _ => unreachable!("checked by is_numeric"),
    }
}

impl Compiler<'_> {
    fn expr(&self, e: &Expr) -> CResult<String> {
        match e {
            Expr::And(items) | Expr::Or(items) => {
                let sep = if matches!(e, Expr::And(_)) {
                    " AND "
                } else {
                    " OR "
                };
                let parts = items
                    .iter()
                    .map(|i| self.expr(i))
                    .collect::<CResult<Vec<_>>>()?;
                Ok(format!("({})", parts.join(sep)))
            }
            Expr::Not(inner) => Ok(format!("NOT ({})", self.expr(inner)?)),
            Expr::Compare {
                field,
                op,
                value,
                span,
            } => self.compare(field, *op, value, span),
            Expr::In {
                field,
                values,
                negated,
                span,
            } => self.in_list(field, values, *negated, span),
            Expr::IsNull { field, negated, .. } => {
                let t = self.resolve(field)?;
                let not = if *negated { "NOT " } else { "" };
                Ok(format!("{} IS {not}NULL", t.value_expr()))
            }
            Expr::Between {
                field,
                low,
                high,
                span,
            } => {
                let t = self.resolve(field)?;
                if let Target::JsonPath { text, .. } = &t
                    && is_numeric(low)
                    && is_numeric(high)
                {
                    return Ok(format!(
                        "{text}::numeric BETWEEN {} AND {}",
                        number_text(low),
                        number_text(high)
                    ));
                }
                Ok(format!(
                    "{} BETWEEN {} AND {}",
                    t.value_expr(),
                    self.literal(low, span)?,
                    self.literal(high, span)?
                ))
            }
            Expr::Exists { field, negated, .. } => {
                let t = self.resolve(field)?;
                let not = if *negated { "" } else { "NOT " };
                let subject = match &t {
                    Target::Column { ident, .. } => ident.as_str(),
                    Target::JsonPath { json, .. } => json.as_str(),
                };
                Ok(format!("{subject} IS {not}NULL"))
            }
        }
    }

    fn resolve(&self, field: &Field) -> CResult<Target> {
        let segs = &field.path.0;
        let first = &segs[0];
        let (name, kind) = if self.columns.is_empty() {
            (first.as_str(), None)
        } else {
            let col = self.columns.iter().find(|c| c.name == *first).or_else(|| {
                self.columns
                    .iter()
                    .find(|c| c.name.eq_ignore_ascii_case(first))
            });
            match col {
                Some(c) => (c.name.as_str(), Some(column_kind(&c.data_type))),
                None => return Err(self.unknown_column(first, field)),
            }
        };
        let ident = quote_ident(name, &field.span)?;
        if segs.len() == 1 {
            return Ok(Target::Column {
                ident,
                kind: kind.unwrap_or(Kind::Scalar),
            });
        }
        if kind.is_some_and(|k| k != Kind::Json) {
            return Err(FilterError::new(
                format!(
                    "column '{name}' is not a json/jsonb column, so '{}' can't look inside it",
                    field.path.dotted()
                ),
                field.span.clone(),
            ));
        }
        let elems: Vec<String> = segs[1..].iter().map(|s| array_elem(s)).collect();
        let path = quote_literal(&format!("{{{}}}", elems.join(",")), &field.span)?;
        Ok(Target::JsonPath {
            text: format!("({ident} #>> {path})"),
            json: format!("({ident} #> {path})"),
        })
    }

    fn unknown_column(&self, name: &str, field: &Field) -> FilterError {
        let lname = name.to_lowercase();
        let best = self
            .columns
            .iter()
            .map(|c| (edit_distance(&lname, &c.name.to_lowercase()), &c.name))
            .min_by_key(|(d, _)| *d);
        let threshold = (name.chars().count() / 3).max(2);
        let span = field.span.clone();
        match best {
            Some((d, col)) if d <= threshold => FilterError::new(
                format!("unknown column '{name}'; did you mean '{col}'?"),
                span,
            ),
            _ => FilterError::new(format!("unknown column '{name}'"), span),
        }
    }

    /// Untyped quoted literal (`'30'`, `'true'`, `'text'`).
    fn literal(&self, lit: &Literal, span: &Range<usize>) -> CResult<String> {
        match lit {
            Literal::Null => Ok("NULL".to_string()),
            Literal::Bool(b) => Ok(format!("'{b}'")),
            Literal::Int(i) => Ok(format!("'{i}'")),
            Literal::Float(x) => Ok(format!("'{x}'")),
            Literal::String(s) | Literal::Date(s) | Literal::ObjectId(s) => quote_literal(s, span),
            Literal::Regex { .. } => Err(FilterError::new(
                "a regex can only be used with '=', '!=' or 'matches'",
                span.clone(),
            )),
        }
    }

    fn compare(
        &self,
        field: &Field,
        op: CompareOp,
        value: &Literal,
        span: &Range<usize>,
    ) -> CResult<String> {
        let t = self.resolve(field)?;
        let text_value = || match value {
            Literal::String(s) => Ok(s.as_str()),
            _ => Err(FilterError::new(
                format!("'{}' needs a text value", op.symbol()),
                span.clone(),
            )),
        };
        let like = |pattern: String, kw: &str| -> CResult<String> {
            Ok(format!(
                "{} {kw} {}",
                t.text_expr(),
                quote_literal(&pattern, span)?
            ))
        };
        match op {
            CompareOp::Eq
            | CompareOp::Ne
            | CompareOp::Lt
            | CompareOp::Le
            | CompareOp::Gt
            | CompareOp::Ge => {
                if matches!(value, Literal::Null) {
                    let not = if op == CompareOp::Ne { "NOT " } else { "" };
                    return Ok(format!("{} IS {not}NULL", t.value_expr()));
                }
                let sym = match op {
                    CompareOp::Eq => "=",
                    CompareOp::Ne => "<>",
                    CompareOp::Lt => "<",
                    CompareOp::Le => "<=",
                    CompareOp::Gt => ">",
                    _ => ">=",
                };
                match &t {
                    Target::Column {
                        ident,
                        kind: Kind::Array,
                    } if matches!(op, CompareOp::Eq | CompareOp::Ne) => {
                        let any = format!("{} = ANY({ident})", self.literal(value, span)?);
                        Ok(if op == CompareOp::Eq {
                            any
                        } else {
                            format!("NOT ({any})")
                        })
                    }
                    Target::JsonPath { text, .. }
                        if is_numeric(value) && !matches!(op, CompareOp::Eq | CompareOp::Ne) =>
                    {
                        Ok(format!("{text}::numeric {sym} {}", number_text(value)))
                    }
                    _ => Ok(format!(
                        "{} {sym} {}",
                        t.value_expr(),
                        self.literal(value, span)?
                    )),
                }
            }
            CompareOp::Contains => like(format!("%{}%", like_escape(text_value()?)), "ILIKE"),
            CompareOp::NotContains => {
                like(format!("%{}%", like_escape(text_value()?)), "NOT ILIKE")
            }
            CompareOp::StartsWith => like(format!("{}%", like_escape(text_value()?)), "ILIKE"),
            CompareOp::EndsWith => like(format!("%{}", like_escape(text_value()?)), "ILIKE"),
            CompareOp::Like => like(text_value()?.to_string(), "LIKE"),
            CompareOp::ILike => like(text_value()?.to_string(), "ILIKE"),
            CompareOp::Matches => match value {
                Literal::String(p) => like(p.clone(), "~"),
                Literal::Regex { pattern, flags } => {
                    if let Some(f) = flags.chars().find(|&f| f != 'i') {
                        return Err(FilterError::new(
                            format!("regex flag '{f}' isn't supported on PostgreSQL (only 'i')"),
                            span.clone(),
                        ));
                    }
                    like(
                        pattern.clone(),
                        if flags.contains('i') { "~*" } else { "~" },
                    )
                }
                _ => Err(FilterError::new(
                    "'matches' needs a text pattern",
                    span.clone(),
                )),
            },
        }
    }

    fn in_list(
        &self,
        field: &Field,
        values: &[Literal],
        negated: bool,
        span: &Range<usize>,
    ) -> CResult<String> {
        let t = self.resolve(field)?;
        let has_null = values.iter().any(|v| matches!(v, Literal::Null));
        let items = values
            .iter()
            .filter(|v| !matches!(v, Literal::Null))
            .map(|v| self.literal(v, span))
            .collect::<CResult<Vec<_>>>()?;
        let subject = t.value_expr();

        let core = if items.is_empty() {
            None
        } else if let Target::Column {
            ident,
            kind: Kind::Array,
        } = &t
        {
            let any: Vec<String> = items
                .iter()
                .map(|v| format!("{v} = ANY({ident})"))
                .collect();
            let joined = any.join(" OR ");
            Some(if negated {
                format!("NOT ({joined})")
            } else if any.len() == 1 {
                joined
            } else {
                format!("({joined})")
            })
        } else {
            let not = if negated { "NOT " } else { "" };
            Some(format!("{subject} {not}IN ({})", items.join(", ")))
        };

        Ok(match (core, has_null, negated) {
            (None, false, false) => "FALSE".to_string(),
            (None, false, true) => "TRUE".to_string(),
            (None, true, false) => format!("{subject} IS NULL"),
            (None, true, true) => format!("{subject} IS NOT NULL"),
            (Some(c), false, _) => c,
            (Some(c), true, false) => format!("({c} OR {subject} IS NULL)"),
            (Some(c), true, true) => format!("({c} AND {subject} IS NOT NULL)"),
        })
    }
}
