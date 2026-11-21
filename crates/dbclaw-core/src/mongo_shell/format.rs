//! Formatting BSON values as relaxed (mongosh-style) literals.

use bson::spec::BinarySubtype;
use bson::{Bson, DateTime, Document};

use super::ShellError;
use super::lexer::{Cursor, is_identifier};
use super::literal::{base64_encode, civil_from_days, parse_value, scan_regex_literal};

/// Maximum length (in characters) of a document/array shown in a grid cell.
const CELL_MAX_CHARS: usize = 300;
/// Pretty mode keeps a container on one line if it fits in this width.
const PRETTY_LINE_WIDTH: usize = 72;

/// Formats a value for display in a result grid cell (single line).
///
/// Top-level strings are shown raw (no quotes); dates as bare ISO-8601;
/// documents and arrays as compact relaxed literals truncated to ~300 chars.
pub fn format_cell(v: &Bson) -> String {
    match v {
        Bson::String(s) => s.clone(),
        Bson::DateTime(d) => format_iso(*d),
        Bson::Decimal128(d) => d.to_string(),
        Bson::Int64(i) => i.to_string(),
        Bson::Document(_) | Bson::Array(_) => {
            let s = format_literal(v, false);
            truncate_chars(&s, CELL_MAX_CHARS)
        }
        other => format_literal(other, false),
    }
}

fn truncate_chars(s: &str, max: usize) -> String {
    match s.char_indices().nth(max) {
        None => s.to_string(),
        Some(_) => {
            let cut = s.char_indices().nth(max - 1).map_or(s.len(), |(i, _)| i);
            format!("{}…", &s[..cut])
        }
    }
}

/// Formats a value as a relaxed literal that parses back to exactly the
/// same value with [`parse_value`](super::parse_value).
pub fn format_literal(v: &Bson, pretty: bool) -> String {
    let mut out = String::new();
    write_value(&mut out, v, pretty, 0, true);
    out
}

/// Formats a document as a relaxed literal (see [`format_literal`]).
pub fn format_document(d: &Document, pretty: bool) -> String {
    let mut out = String::new();
    write_document(&mut out, d, pretty, 0, true);
    out
}

/// Nesting depth of containers: scalars are 0, `[1]` is 1, `[{a: 1}]` is 2.
fn depth(v: &Bson) -> usize {
    match v {
        Bson::Document(d) => 1 + d.values().map(depth).max().unwrap_or(0),
        Bson::Array(a) => 1 + a.iter().map(depth).max().unwrap_or(0),
        _ => 0,
    }
}

/// Pretty mode keeps a container on one line when it is shallow (at most
/// one level of nested containers) and its compact form fits the line.
fn fits_inline(compact_len: usize, child_depth: usize, indent: usize) -> bool {
    child_depth <= 1 && compact_len + indent <= PRETTY_LINE_WIDTH
}

fn write_value(out: &mut String, v: &Bson, pretty: bool, indent: usize, top: bool) {
    match v {
        Bson::Document(d) => write_document(out, d, pretty, indent, top),
        Bson::Array(a) => write_array(out, a, pretty, indent),
        _ => write_scalar(out, v),
    }
}

fn write_document(out: &mut String, d: &Document, pretty: bool, indent: usize, top: bool) {
    if d.is_empty() {
        out.push_str("{}");
        return;
    }
    let inline = || {
        let mut s = String::new();
        write_document(&mut s, d, false, 0, false);
        fits_inline(s.len(), d.values().map(depth).max().unwrap_or(0), indent)
    };
    if !pretty || (!top && inline()) {
        out.push('{');
        for (i, (k, v)) in d.iter().enumerate() {
            if i > 0 {
                out.push_str(", ");
            }
            write_key(out, k);
            out.push_str(": ");
            write_value(out, v, false, 0, false);
        }
        out.push('}');
        return;
    }
    out.push_str("{\n");
    let inner = indent + 2;
    let n = d.len();
    for (i, (k, v)) in d.iter().enumerate() {
        push_indent(out, inner);
        write_key(out, k);
        out.push_str(": ");
        write_value(out, v, true, inner, false);
        if i + 1 < n {
            out.push(',');
        }
        out.push('\n');
    }
    push_indent(out, indent);
    out.push('}');
}

fn write_array(out: &mut String, a: &[Bson], pretty: bool, indent: usize) {
    if a.is_empty() {
        out.push_str("[]");
        return;
    }
    let inline = || {
        let mut s = String::new();
        write_array(&mut s, a, false, 0);
        fits_inline(s.len(), a.iter().map(depth).max().unwrap_or(0), indent)
    };
    if !pretty || inline() {
        out.push('[');
        for (i, v) in a.iter().enumerate() {
            if i > 0 {
                out.push_str(", ");
            }
            write_value(out, v, false, 0, false);
        }
        out.push(']');
        return;
    }
    out.push_str("[\n");
    let inner = indent + 2;
    for (i, v) in a.iter().enumerate() {
        push_indent(out, inner);
        write_value(out, v, true, inner, false);
        if i + 1 < a.len() {
            out.push(',');
        }
        out.push('\n');
    }
    push_indent(out, indent);
    out.push(']');
}

fn push_indent(out: &mut String, n: usize) {
    out.extend(std::iter::repeat_n(' ', n));
}

fn write_key(out: &mut String, k: &str) {
    if is_identifier(k) {
        out.push_str(k);
    } else {
        write_string(out, k);
    }
}

/// Writes a double-quoted JavaScript string literal.
pub(crate) fn write_string(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            '\u{2028}' => out.push_str("\\u2028"),
            '\u{2029}' => out.push_str("\\u2029"),
            c if (c as u32) < 0x20 || c == '\u{7f}' => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

fn quoted(s: &str) -> String {
    let mut out = String::new();
    write_string(&mut out, s);
    out
}

pub(crate) fn format_double(f: f64) -> String {
    if f.is_nan() {
        "NaN".into()
    } else if f == f64::INFINITY {
        "Infinity".into()
    } else if f == f64::NEG_INFINITY {
        "-Infinity".into()
    } else {
        // Debug gives the shortest round-trippable form and always includes
        // a decimal point or exponent (`5.0`, `1e300`, `-0.0`).
        format!("{f:?}")
    }
}

/// ISO-8601 with millisecond precision, e.g. `2024-01-01T10:00:00.000Z`.
/// Returns `None` if the year can't be written with at most 6 digits.
fn try_format_iso(d: DateTime) -> Option<String> {
    let ms = d.timestamp_millis();
    let days = ms.div_euclid(86_400_000);
    let rem = ms.rem_euclid(86_400_000);
    let (y, m, day) = civil_from_days(days);
    let (h, mi, s, milli) = (
        rem / 3_600_000,
        rem / 60_000 % 60,
        rem / 1000 % 60,
        rem % 1000,
    );
    let year = if (0..=9999).contains(&y) {
        format!("{y:04}")
    } else if y.abs() <= 999_999 {
        format!("{}{:06}", if y < 0 { '-' } else { '+' }, y.abs())
    } else {
        return None;
    };
    Some(format!(
        "{year}-{m:02}-{day:02}T{h:02}:{mi:02}:{s:02}.{milli:03}Z"
    ))
}

fn format_iso(d: DateTime) -> String {
    try_format_iso(d).unwrap_or_else(|| d.timestamp_millis().to_string())
}

fn format_uuid(bytes: &[u8]) -> String {
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

fn format_regex(pattern: &str, options: &str) -> String {
    // Escape unescaped `/`; keep every other escape verbatim.
    let mut lit = String::from("/");
    let mut chars = pattern.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                lit.push('\\');
                if let Some(n) = chars.next() {
                    lit.push(n);
                }
            }
            '/' => lit.push_str("\\/"),
            c => lit.push(c),
        }
    }
    lit.push('/');
    lit.push_str(options);
    // Verify the literal form scans back to the same pattern; otherwise
    // (empty pattern, newlines, `\/` in the source, unbalanced classes …)
    // fall back to the constructor form which preserves the text exactly.
    let mut c = Cursor::new(&lit);
    let ok = matches!(scan_regex_literal(&mut c), Ok((p, f)) if p == pattern && f == options && c.at_end())
        && !pattern.is_empty();
    if ok {
        lit
    } else if options.is_empty() {
        format!("RegExp({})", quoted(pattern))
    } else {
        format!("RegExp({}, {})", quoted(pattern), quoted(options))
    }
}

fn write_scalar(out: &mut String, v: &Bson) {
    let s = match v {
        Bson::Double(f) => format_double(*f),
        Bson::String(s) => quoted(s),
        Bson::Boolean(b) => b.to_string(),
        Bson::Null => "null".into(),
        Bson::Undefined => "undefined".into(),
        Bson::Int32(i) => i.to_string(),
        Bson::Int64(i) => {
            if i.unsigned_abs() <= (1u64 << 53) {
                format!("NumberLong({i})")
            } else {
                format!("NumberLong(\"{i}\")")
            }
        }
        Bson::Decimal128(d) => format!("NumberDecimal(\"{d}\")"),
        Bson::ObjectId(o) => format!("ObjectId(\"{}\")", o.to_hex()),
        Bson::DateTime(d) => match try_format_iso(*d) {
            Some(s) => format!("ISODate(\"{s}\")"),
            None => format!("new Date({})", d.timestamp_millis()),
        },
        Bson::Binary(b) => {
            if b.subtype == BinarySubtype::Uuid && b.bytes.len() == 16 {
                format!("UUID(\"{}\")", format_uuid(&b.bytes))
            } else {
                format!(
                    "BinData({}, \"{}\")",
                    u8::from(b.subtype),
                    base64_encode(&b.bytes)
                )
            }
        }
        Bson::RegularExpression(r) => format_regex(&r.pattern, &r.options),
        Bson::Timestamp(t) => format!("Timestamp({{t: {}, i: {}}})", t.time, t.increment),
        Bson::MinKey => "MinKey()".into(),
        Bson::MaxKey => "MaxKey()".into(),
        Bson::Symbol(s) => format!("{{$symbol: {}}}", quoted(s)),
        Bson::JavaScriptCode(code) => format!("Code({})", quoted(code)),
        Bson::JavaScriptCodeWithScope(cws) => {
            let mut scope = String::new();
            write_document(&mut scope, &cws.scope, false, 0, false);
            format!("Code({}, {})", quoted(&cws.code), scope)
        }
        Bson::DbPointer(_) => {
            // Fields are private in bson 2; go through canonical Extended JSON.
            let json = v.clone().into_canonical_extjson();
            json_to_literal(&json)
        }
        Bson::Document(_) | Bson::Array(_) => unreachable!("containers handled by write_value"),
    };
    out.push_str(&s);
}

/// Renders a serde_json value as a relaxed literal (used for DBPointer).
fn json_to_literal(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::Object(m) => {
            let parts: Vec<String> = m
                .iter()
                .map(|(k, v)| {
                    let mut key = String::new();
                    write_key(&mut key, k);
                    format!("{key}: {}", json_to_literal(v))
                })
                .collect();
            format!("{{{}}}", parts.join(", "))
        }
        serde_json::Value::String(s) => quoted(s),
        other => other.to_string(),
    }
}

/// Parses the text of an inline grid-cell edit, using the cell's original
/// value to decide how to interpret it.
///
/// * original String → the raw text becomes the string (no quotes needed),
///   unless the input is exactly `null`;
/// * original number → parsed as a number keeping the numeric type when
///   possible (Int32 grows to Int64 if needed; Double stays Double;
///   Decimal128 stays Decimal128); other literals are accepted too;
/// * anything else → [`parse_value`]; if that fails and the input does not
///   look like a literal, the raw text becomes a string.
pub fn parse_cell_edit(input: &str, original: &Bson) -> Result<Bson, ShellError> {
    match original {
        Bson::String(_) => {
            if input == "null" {
                Ok(Bson::Null)
            } else {
                Ok(Bson::String(input.to_string()))
            }
        }
        Bson::Int32(_) | Bson::Int64(_) | Bson::Double(_) | Bson::Decimal128(_) => {
            if let Bson::Decimal128(_) = original
                && let Some(d) = super::literal::parse_decimal(input)
            {
                return Ok(Bson::Decimal128(d));
            }
            let parsed = parse_or_string(input)?;
            Ok(match (original, parsed) {
                (Bson::Int32(_), Bson::Int64(i)) => Bson::Int64(i),
                (Bson::Int64(_), Bson::Int32(i)) => Bson::Int64(i as i64),
                (Bson::Double(_), Bson::Int32(i)) => Bson::Double(i as f64),
                (Bson::Double(_), Bson::Int64(i)) => Bson::Double(i as f64),
                (Bson::Decimal128(_), Bson::Int32(i)) => decimal_or(i.to_string(), Bson::Int32(i)),
                (Bson::Decimal128(_), Bson::Int64(i)) => decimal_or(i.to_string(), Bson::Int64(i)),
                (_, other) => other,
            })
        }
        _ => parse_or_string(input),
    }
}

fn decimal_or(text: String, fallback: Bson) -> Bson {
    super::literal::parse_decimal(&text).map_or(fallback, Bson::Decimal128)
}

fn parse_or_string(input: &str) -> Result<Bson, ShellError> {
    match parse_value(input) {
        Ok(v) => Ok(v),
        Err(e) if looks_like_literal(input) => Err(e),
        Err(_) => Ok(Bson::String(input.to_string())),
    }
}

/// True when the text was clearly meant as a literal (object, array, quoted
/// string or constructor call), so a parse error should be reported rather
/// than silently storing the text as a string.
fn looks_like_literal(input: &str) -> bool {
    let t = input.trim_start();
    if t.starts_with(['{', '[', '"', '\'']) {
        return true;
    }
    let mut c = Cursor::new(t);
    let Some((mut word, _)) = c.ident() else {
        return false;
    };
    if word == "new" {
        if c.skip_ws().is_err() {
            return false;
        }
        match c.ident() {
            Some((w, _)) => word = w,
            None => return false,
        }
    }
    if !super::literal::is_constructor(word) {
        return false;
    }
    c.skip_ws().is_ok() && c.peek() == Some('(')
}
