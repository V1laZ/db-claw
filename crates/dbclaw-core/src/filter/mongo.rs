//! [`Expr`] → MongoDB filter document.

use super::{CompareOp, Expr, Field, FilterError, Literal, normalize_date};
use bson::{Bson, Document, Regex, doc};
use std::ops::Range;

/// Compile to a MongoDB filter document (usable with `find`, `count`, …).
pub fn to_mongo(expr: &Expr) -> Result<Document, FilterError> {
    compile(expr)
}

type CResult<T> = Result<T, FilterError>;

fn compile(e: &Expr) -> CResult<Document> {
    match e {
        Expr::And(items) => {
            let docs = items.iter().map(compile).collect::<CResult<Vec<_>>>()?;
            Ok(merge_and(docs))
        }
        Expr::Or(items) => {
            let docs = items.iter().map(compile).collect::<CResult<Vec<_>>>()?;
            Ok(doc! { "$or": docs })
        }
        Expr::Not(inner) => Ok(doc! { "$nor": [compile(inner)?] }),
        Expr::Compare {
            field,
            op,
            value,
            span,
        } => compare(field, *op, value, span),
        Expr::In {
            field,
            values,
            negated,
            span,
        } => {
            let key = path(field)?;
            let arr = values
                .iter()
                .map(|v| bson(v, span))
                .collect::<CResult<Vec<_>>>()?;
            let op = if *negated { "$nin" } else { "$in" };
            Ok(doc! { key: { op: arr } })
        }
        Expr::IsNull { field, negated, .. } => {
            let key = path(field)?;
            Ok(if *negated {
                doc! { key: { "$ne": Bson::Null } }
            } else {
                doc! { key: Bson::Null }
            })
        }
        Expr::Between {
            field,
            low,
            high,
            span,
        } => {
            let key = path(field)?;
            Ok(doc! { key: { "$gte": bson(low, span)?, "$lte": bson(high, span)? } })
        }
        Expr::Exists { field, negated, .. } => {
            let key = path(field)?;
            Ok(doc! { key: { "$exists": !*negated } })
        }
    }
}

/// Merge the parts of an `and` into one document when their top-level keys
/// don't collide (`{a: 1, b: 2}`); otherwise fall back to `$and`.
fn merge_and(docs: Vec<Document>) -> Document {
    let mut seen = std::collections::HashSet::new();
    let collides = docs
        .iter()
        .flat_map(|d| d.keys())
        .any(|k| !seen.insert(k.clone()));
    if collides {
        return doc! { "$and": docs };
    }
    docs.into_iter().flatten().collect()
}

fn path(field: &Field) -> CResult<String> {
    for seg in &field.path.0 {
        if seg.starts_with('$') {
            return Err(FilterError::new(
                "MongoDB field names can't start with '$'",
                field.span.clone(),
            ));
        }
        if seg.contains('\0') {
            return Err(FilterError::new(
                "field names can't contain NUL (\\u0000) characters",
                field.span.clone(),
            ));
        }
    }
    Ok(field.path.dotted())
}

fn bson(lit: &Literal, span: &Range<usize>) -> CResult<Bson> {
    Ok(match lit {
        Literal::Null => Bson::Null,
        Literal::Bool(b) => Bson::Boolean(*b),
        Literal::Int(i) => match i32::try_from(*i) {
            Ok(small) => Bson::Int32(small),
            Err(_) => Bson::Int64(*i),
        },
        Literal::Float(x) => Bson::Double(*x),
        Literal::String(s) => Bson::String(s.clone()),
        Literal::Date(s) => {
            let dt = normalize_date(s)
                .and_then(|n| bson::DateTime::parse_rfc3339_str(n).ok())
                .ok_or_else(|| FilterError::new(format!("invalid date \"{s}\""), span.clone()))?;
            Bson::DateTime(dt)
        }
        Literal::ObjectId(s) => {
            Bson::ObjectId(bson::oid::ObjectId::parse_str(s).map_err(|_| {
                FilterError::new(
                    "an ObjectId must be exactly 24 hex characters",
                    span.clone(),
                )
            })?)
        }
        Literal::Regex { pattern, flags } => Bson::RegularExpression(regex(pattern, flags)),
    })
}

fn regex(pattern: &str, flags: &str) -> Regex {
    let mut opts: Vec<char> = flags.chars().collect();
    opts.sort_unstable();
    opts.dedup();
    Regex {
        pattern: pattern.to_string(),
        options: opts.into_iter().collect(),
    }
}

/// Escape PCRE metacharacters so `s` matches literally.
fn regex_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if "\\^$.|?*+()[]{}/".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Translate a SQL LIKE pattern to an anchored regex. `\` escapes the next
/// character, as in PostgreSQL.
fn like_to_regex(pattern: &str) -> String {
    let mut out = String::from("^");
    let mut chars = pattern.chars();
    while let Some(c) = chars.next() {
        match c {
            '%' => out.push_str(".*"),
            '_' => out.push('.'),
            '\\' => match chars.next() {
                Some(n) => out.push_str(&regex_escape(&n.to_string())),
                None => out.push_str("\\\\"),
            },
            c => out.push_str(&regex_escape(&c.to_string())),
        }
    }
    out.push('$');
    out
}

fn regex_doc(pattern: String, options: &str) -> Document {
    if options.is_empty() {
        doc! { "$regex": pattern }
    } else {
        doc! { "$regex": pattern, "$options": options }
    }
}

fn compare(
    field: &Field,
    op: CompareOp,
    value: &Literal,
    span: &Range<usize>,
) -> CResult<Document> {
    let key = path(field)?;
    let text = || match value {
        Literal::String(s) => Ok(s.as_str()),
        _ => Err(FilterError::new(
            format!("'{}' needs a text value", op.symbol()),
            span.clone(),
        )),
    };
    let cond: Bson = match op {
        CompareOp::Eq => {
            return Ok(match value {
                Literal::Regex { .. } => doc! { key: { "$regex": bson(value, span)? } },
                _ => doc! { key: bson(value, span)? },
            });
        }
        CompareOp::Ne => doc! { "$ne": bson(value, span)? }.into(),
        CompareOp::Lt => doc! { "$lt": bson(value, span)? }.into(),
        CompareOp::Le => doc! { "$lte": bson(value, span)? }.into(),
        CompareOp::Gt => doc! { "$gt": bson(value, span)? }.into(),
        CompareOp::Ge => doc! { "$gte": bson(value, span)? }.into(),
        CompareOp::Contains => regex_doc(regex_escape(text()?), "i").into(),
        CompareOp::NotContains => {
            doc! { "$not": Bson::RegularExpression(regex(&regex_escape(text()?), "i")) }.into()
        }
        CompareOp::StartsWith => regex_doc(format!("^{}", regex_escape(text()?)), "i").into(),
        CompareOp::EndsWith => regex_doc(format!("{}$", regex_escape(text()?)), "i").into(),
        // `s` so that `%` / `_` also match newlines, like in SQL.
        CompareOp::Like => regex_doc(like_to_regex(text()?), "s").into(),
        CompareOp::ILike => regex_doc(like_to_regex(text()?), "is").into(),
        CompareOp::Matches => match value {
            Literal::String(p) => regex_doc(p.clone(), "").into(),
            Literal::Regex { pattern, flags } => regex_doc(pattern.clone(), flags).into(),
            _ => {
                return Err(FilterError::new(
                    "'matches' needs a text pattern",
                    span.clone(),
                ));
            }
        },
    };
    Ok(doc! { key: cond })
}
