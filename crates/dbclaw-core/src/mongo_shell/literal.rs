//! Relaxed JSON / JavaScript object-literal parser producing BSON values.

use std::ops::Range;
use std::str::FromStr;

use bson::spec::BinarySubtype;
use bson::{Binary, Bson, DateTime, Decimal128, Document, Regex, Timestamp, oid::ObjectId};

use super::ShellError;
use super::lexer::{Cursor, is_ident_continue};

/// Maximum nesting depth of objects/arrays/constructor calls.
const MAX_DEPTH: usize = 256;

/// Parses a single relaxed literal (object, array, string, number,
/// constructor call, regex, …). The whole input must be consumed, except
/// for whitespace, comments and an optional trailing `;`.
pub fn parse_value(input: &str) -> Result<Bson, ShellError> {
    let mut c = Cursor::new(input);
    c.skip_ws()?;
    if c.at_end() {
        return c.err("expected a value, found end of input", 0..0);
    }
    let v = parse_value_at(&mut c, 0)?;
    finish(&mut c)?;
    Ok(v)
}

/// Parses a relaxed object literal into a document.
pub fn parse_document(input: &str) -> Result<Document, ShellError> {
    let mut c = Cursor::new(input);
    c.skip_ws()?;
    let start = c.pos;
    if c.peek() != Some('{') {
        return c.err(
            format!("expected a document `{{ … }}`, found {}", c.describe_here()),
            c.here(),
        );
    }
    let v = parse_value_at(&mut c, 0)?;
    finish(&mut c)?;
    match v {
        Bson::Document(d) => Ok(d),
        // An Extended JSON wrapper such as {"$oid": "…"} turned into a scalar.
        other => c.err(
            format!("expected a document, but this is {}", type_name(&other)),
            start..c.pos,
        ),
    }
}

/// Skips trailing whitespace/comments and an optional `;`, then requires EOF.
pub(crate) fn finish(c: &mut Cursor) -> Result<(), ShellError> {
    c.skip_ws()?;
    if c.eat(';') {
        c.skip_ws()?;
    }
    if !c.at_end() {
        let span = c.pos..c.src.len();
        return c.err(
            format!(
                "unexpected {} after the end of the value",
                c.describe_here()
            ),
            span,
        );
    }
    Ok(())
}

pub(crate) fn type_name(v: &Bson) -> &'static str {
    match v {
        Bson::Double(_) => "a double",
        Bson::String(_) => "a string",
        Bson::Array(_) => "an array",
        Bson::Document(_) => "a document",
        Bson::Boolean(_) => "a boolean",
        Bson::Null => "null",
        Bson::RegularExpression(_) => "a regular expression",
        Bson::JavaScriptCode(_) | Bson::JavaScriptCodeWithScope(_) => "JavaScript code",
        Bson::Int32(_) => "an int32",
        Bson::Int64(_) => "an int64",
        Bson::Timestamp(_) => "a timestamp",
        Bson::Binary(_) => "binary data",
        Bson::ObjectId(_) => "an ObjectId",
        Bson::DateTime(_) => "a date",
        Bson::Symbol(_) => "a symbol",
        Bson::Decimal128(_) => "a decimal",
        Bson::Undefined => "undefined",
        Bson::MaxKey => "MaxKey",
        Bson::MinKey => "MinKey",
        Bson::DbPointer(_) => "a DBPointer",
    }
}

/// Parses one value at the cursor (leading whitespace is skipped).
pub(crate) fn parse_value_at(c: &mut Cursor, depth: usize) -> Result<Bson, ShellError> {
    c.skip_ws()?;
    if depth > MAX_DEPTH {
        return c.err("value is nested too deeply", c.here());
    }
    let start = c.pos;
    match c.peek() {
        None => c.err("expected a value, found end of input", c.here()),
        Some('{') => parse_object(c, depth),
        Some('[') => parse_array(c, depth).map(Bson::Array),
        Some('"' | '\'') => c.string().map(Bson::String),
        Some('/') => parse_regex_literal(c),
        Some('0'..='9' | '.') => c.number(false, start),
        Some(sign @ ('-' | '+')) => {
            c.bump();
            c.skip_ws()?;
            let negative = sign == '-';
            match c.peek() {
                Some('0'..='9' | '.') => c.number(negative, start),
                _ => match c.peek_ident() {
                    Some("Infinity") => {
                        c.ident();
                        Ok(Bson::Double(if negative {
                            f64::NEG_INFINITY
                        } else {
                            f64::INFINITY
                        }))
                    }
                    Some("NaN") => {
                        c.ident();
                        Ok(Bson::Double(f64::NAN))
                    }
                    _ => c.err(
                        format!(
                            "expected a number after `{sign}`, found {}",
                            c.describe_here()
                        ),
                        start..c.here().end,
                    ),
                },
            }
        }
        Some(ch) if super::lexer::is_ident_start(ch) => parse_ident_value(c, depth),
        Some(ch) => c.err(format!("unexpected `{ch}`, expected a value"), c.here()),
    }
}

fn parse_object(c: &mut Cursor, depth: usize) -> Result<Bson, ShellError> {
    let start = c.pos;
    c.bump(); // {
    let mut doc = Document::new();
    loop {
        c.skip_ws()?;
        if c.eat('}') {
            break;
        }
        let key_start = c.pos;
        let key = match c.peek() {
            Some('"' | '\'') => c.string()?,
            Some('0'..='9') => {
                c.number(false, key_start)?;
                c.src[key_start..c.pos].to_string()
            }
            Some(ch) if super::lexer::is_ident_start(ch) => c.ident().expect("ident").0.to_string(),
            None => {
                return c.err("unterminated object: expected `}`", start..c.src.len());
            }
            Some(_) => {
                return c.err(
                    format!("expected a field name or `}}`, found {}", c.describe_here()),
                    c.here(),
                );
            }
        };
        let key_span = key_start..c.pos;
        c.skip_ws()?;
        if !c.eat(':') {
            return c.err(
                format!(
                    "expected `:` after field name `{key}`, found {}",
                    c.describe_here()
                ),
                if c.at_end() { key_span } else { c.here() },
            );
        }
        let value = parse_value_at(c, depth + 1)?;
        doc.insert(key, value);
        c.skip_ws()?;
        if c.eat(',') {
            continue;
        }
        if c.eat('}') {
            break;
        }
        if c.at_end() {
            return c.err("unterminated object: expected `}`", start..c.src.len());
        }
        return c.err(
            format!(
                "expected `,` or `}}` in object, found {}",
                c.describe_here()
            ),
            c.here(),
        );
    }
    convert_extjson(doc, start..c.pos)
}

fn parse_array(c: &mut Cursor, depth: usize) -> Result<Vec<Bson>, ShellError> {
    let start = c.pos;
    c.bump(); // [
    let mut items = Vec::new();
    loop {
        c.skip_ws()?;
        if c.eat(']') {
            break;
        }
        if c.at_end() {
            return c.err("unterminated array: expected `]`", start..c.src.len());
        }
        items.push(parse_value_at(c, depth + 1)?);
        c.skip_ws()?;
        if c.eat(',') {
            continue;
        }
        if c.eat(']') {
            break;
        }
        if c.at_end() {
            return c.err("unterminated array: expected `]`", start..c.src.len());
        }
        return c.err(
            format!("expected `,` or `]` in array, found {}", c.describe_here()),
            c.here(),
        );
    }
    Ok(items)
}

// ---------------------------------------------------------------------------
// Regular expressions
// ---------------------------------------------------------------------------

const REGEX_FLAGS: &str = "ilmsux";

/// Scans a `/pattern/flags` literal. Returns the (unescaped) pattern and the
/// raw flags. `\/` is turned into `/` (equivalent in PCRE); every other
/// escape is kept verbatim.
pub(crate) fn scan_regex_literal(c: &mut Cursor) -> Result<(String, String), ShellError> {
    let start = c.pos;
    c.bump(); // /
    let mut pattern = String::new();
    let mut in_class = false;
    loop {
        match c.bump() {
            None | Some('\n' | '\r') => {
                return c.err("unterminated regular expression literal", start..c.pos);
            }
            Some('\\') => match c.bump() {
                None | Some('\n' | '\r') => {
                    return c.err("unterminated regular expression literal", start..c.pos);
                }
                Some('/') => pattern.push('/'),
                Some(other) => {
                    pattern.push('\\');
                    pattern.push(other);
                }
            },
            Some('[') => {
                in_class = true;
                pattern.push('[');
            }
            Some(']') => {
                in_class = false;
                pattern.push(']');
            }
            Some('/') if !in_class => break,
            Some(other) => pattern.push(other),
        }
    }
    let flags_start = c.pos;
    while matches!(c.peek(), Some(ch) if is_ident_continue(ch)) {
        c.bump();
    }
    Ok((pattern, c.src[flags_start..c.pos].to_string()))
}

fn parse_regex_literal(c: &mut Cursor) -> Result<Bson, ShellError> {
    let start = c.pos;
    let (pattern, flags) = scan_regex_literal(c)?;
    let flags_span = c.pos - flags.len()..c.pos;
    make_regex(c, pattern, &flags, flags_span).map_err(|mut e| {
        if e.span.is_empty() {
            e.span = start..c.pos;
        }
        e
    })
}

fn make_regex(
    c: &Cursor,
    pattern: String,
    flags: &str,
    flags_span: Range<usize>,
) -> Result<Bson, ShellError> {
    let mut opts: Vec<char> = Vec::new();
    for f in flags.chars() {
        if f == 'g' {
            // Global matching is meaningless for MongoDB; accept and drop it.
            continue;
        }
        if !REGEX_FLAGS.contains(f) {
            return c.err(
                format!(
                    "invalid regular expression flag `{f}` (MongoDB supports i, m, s, x, u, l)"
                ),
                flags_span,
            );
        }
        if !opts.contains(&f) {
            opts.push(f);
        }
    }
    opts.sort_unstable();
    Ok(Bson::RegularExpression(Regex {
        pattern,
        options: opts.into_iter().collect(),
    }))
}

// ---------------------------------------------------------------------------
// Identifiers, keywords and constructors
// ---------------------------------------------------------------------------

struct Arg {
    value: Bson,
    span: Range<usize>,
}

const CONSTRUCTORS: &[&str] = &[
    "ObjectId",
    "ObjectID",
    "ISODate",
    "Date",
    "NumberInt",
    "Int32",
    "NumberLong",
    "Long",
    "Double",
    "NumberDecimal",
    "Decimal128",
    "UUID",
    "BinData",
    "HexData",
    "Timestamp",
    "MinKey",
    "MaxKey",
    "RegExp",
    "Code",
];

/// True when `name` is one of the supported constructor functions.
pub(crate) fn is_constructor(name: &str) -> bool {
    CONSTRUCTORS.contains(&name)
}

fn parse_ident_value(c: &mut Cursor, depth: usize) -> Result<Bson, ShellError> {
    let (word, span) = c.ident().expect("ident start");
    match word {
        "true" => return Ok(Bson::Boolean(true)),
        "false" => return Ok(Bson::Boolean(false)),
        "null" | "undefined" => return Ok(Bson::Null),
        "NaN" => return Ok(Bson::Double(f64::NAN)),
        "Infinity" => return Ok(Bson::Double(f64::INFINITY)),
        _ => {}
    }
    let (name, name_span, has_new) = if word == "new" {
        c.skip_ws()?;
        match c.ident() {
            Some((n, s)) => (n, s, true),
            None => {
                return c.err(
                    format!(
                        "expected a constructor name after `new`, found {}",
                        c.describe_here()
                    ),
                    c.here(),
                );
            }
        }
    } else {
        (word, span.clone(), false)
    };
    if !is_constructor(name) {
        let hint = if has_new {
            format!("unknown constructor `{name}`")
        } else {
            format!("unknown identifier `{name}` (strings must be quoted, e.g. \"{name}\")")
        };
        return c.err(hint, name_span);
    }
    c.skip_ws()?;
    let call_start = span.start;
    if c.peek() != Some('(') {
        if matches!(name, "MinKey" | "MaxKey") {
            return Ok(if name == "MinKey" {
                Bson::MinKey
            } else {
                Bson::MaxKey
            });
        }
        return c.err(
            format!("expected `(` after `{name}`, found {}", c.describe_here()),
            c.here(),
        );
    }
    let args = parse_args(c, depth)?;
    let call_span = call_start..c.pos;
    build_constructor(c, name, args, call_span)
}

fn parse_args(c: &mut Cursor, depth: usize) -> Result<Vec<Arg>, ShellError> {
    let open = c.pos;
    c.bump(); // (
    let mut args = Vec::new();
    loop {
        c.skip_ws()?;
        if c.eat(')') {
            break;
        }
        if c.at_end() {
            return c.err(
                "unterminated argument list: expected `)`",
                open..c.src.len(),
            );
        }
        let s = c.pos;
        let value = parse_value_at(c, depth + 1)?;
        args.push(Arg {
            value,
            span: s..c.pos,
        });
        c.skip_ws()?;
        if c.eat(',') {
            continue;
        }
        if c.eat(')') {
            break;
        }
        if c.at_end() {
            return c.err(
                "unterminated argument list: expected `)`",
                open..c.src.len(),
            );
        }
        return c.err(
            format!(
                "expected `,` or `)` in argument list, found {}",
                c.describe_here()
            ),
            c.here(),
        );
    }
    Ok(args)
}

fn arity(
    c: &Cursor,
    name: &str,
    args: &[Arg],
    min: usize,
    max: usize,
    span: &Range<usize>,
) -> Result<(), ShellError> {
    if args.len() < min || args.len() > max {
        let expected = if min == max {
            format!("{min}")
        } else {
            format!("{min} to {max}")
        };
        let s = if max == 1 { "" } else { "s" };
        return c.err(
            format!("{name}() takes {expected} argument{s}, got {}", args.len()),
            span.clone(),
        );
    }
    Ok(())
}

fn str_arg<'b>(c: &Cursor, name: &str, arg: &'b Arg) -> Result<&'b str, ShellError> {
    match &arg.value {
        Bson::String(s) => Ok(s),
        other => c.err(
            format!(
                "{name}() expects a string argument, got {}",
                type_name(other)
            ),
            arg.span.clone(),
        ),
    }
}

/// Converts a numeric BSON value to an integer if it is integral.
pub(crate) fn as_integer(v: &Bson) -> Option<i64> {
    match v {
        Bson::Int32(i) => Some(*i as i64),
        Bson::Int64(i) => Some(*i),
        Bson::Double(f) if f.fract() == 0.0 && f.abs() < 9.2e18 => Some(*f as i64),
        _ => None,
    }
}

fn int_arg(c: &Cursor, name: &str, arg: &Arg) -> Result<i64, ShellError> {
    match &arg.value {
        Bson::String(s) => s.trim().parse::<i64>().or_else(|_| {
            c.err(
                format!("{name}() expects an integer, got \"{s}\""),
                arg.span.clone(),
            )
        }),
        v => as_integer(v).map_or_else(
            || {
                c.err(
                    format!("{name}() expects an integer, got {}", type_name(v)),
                    arg.span.clone(),
                )
            },
            Ok,
        ),
    }
}

fn u32_arg(c: &Cursor, name: &str, arg: &Arg) -> Result<u32, ShellError> {
    let v = int_arg(c, name, arg)?;
    u32::try_from(v).or_else(|_| {
        c.err(
            format!("{name}() value {v} is out of range (0 to 4294967295)"),
            arg.span.clone(),
        )
    })
}

fn build_constructor(
    c: &Cursor,
    name: &str,
    args: Vec<Arg>,
    span: Range<usize>,
) -> Result<Bson, ShellError> {
    match name {
        "ObjectId" | "ObjectID" => {
            arity(c, name, &args, 0, 1, &span)?;
            match args.first() {
                None => Ok(Bson::ObjectId(ObjectId::new())),
                Some(a) => {
                    let s = str_arg(c, name, a)?;
                    ObjectId::parse_str(s).map(Bson::ObjectId).or_else(|_| {
                        c.err(
                            format!("invalid ObjectId \"{s}\": expected 24 hexadecimal characters"),
                            a.span.clone(),
                        )
                    })
                }
            }
        }
        "ISODate" | "Date" => {
            if args.is_empty() {
                return Ok(Bson::DateTime(DateTime::now()));
            }
            if args.len() == 1 {
                let a = &args[0];
                return match &a.value {
                    Bson::String(s) => parse_date_str(s).map(Bson::DateTime).ok_or_else(|| {
                        ShellError::new(
                            format!("invalid date \"{s}\": expected ISO-8601 like \"2024-01-31T12:00:00Z\""),
                            a.span.clone(),
                        )
                    }),
                    Bson::DateTime(d) => Ok(Bson::DateTime(*d)),
                    v => match as_integer(v) {
                        Some(ms) => Ok(Bson::DateTime(DateTime::from_millis(ms))),
                        None => c.err(
                            format!("{name}() expects a date string or milliseconds, got {}", type_name(v)),
                            a.span.clone(),
                        ),
                    },
                };
            }
            // new Date(year, monthIndex, day?, h?, m?, s?, ms?) — interpreted as UTC.
            arity(c, name, &args, 1, 7, &span)?;
            let mut parts = [0i64, 0, 1, 0, 0, 0, 0];
            for (i, a) in args.iter().enumerate() {
                parts[i] = int_arg(c, name, a)?;
            }
            let month0 = parts[1];
            let year = parts[0] + month0.div_euclid(12);
            let month = month0.rem_euclid(12) as u32 + 1;
            let days = days_from_civil(year, month, 1) + parts[2] - 1;
            let ms = days as i128 * 86_400_000
                + parts[3] as i128 * 3_600_000
                + parts[4] as i128 * 60_000
                + parts[5] as i128 * 1000
                + parts[6] as i128;
            i64::try_from(ms)
                .map(|ms| Bson::DateTime(DateTime::from_millis(ms)))
                .or_else(|_| c.err("date is out of range", span))
        }
        "NumberInt" | "Int32" => {
            arity(c, name, &args, 0, 1, &span)?;
            let Some(a) = args.first() else {
                return Ok(Bson::Int32(0));
            };
            let v = int_arg(c, name, a)?;
            i32::try_from(v).map(Bson::Int32).or_else(|_| {
                c.err(
                    format!("{v} does not fit in a 32-bit integer"),
                    a.span.clone(),
                )
            })
        }
        "NumberLong" | "Long" => {
            arity(c, name, &args, 0, 1, &span)?;
            let Some(a) = args.first() else {
                return Ok(Bson::Int64(0));
            };
            int_arg(c, name, a).map(Bson::Int64)
        }
        "Double" => {
            arity(c, name, &args, 1, 1, &span)?;
            let a = &args[0];
            match &a.value {
                Bson::Double(f) => Ok(Bson::Double(*f)),
                Bson::Int32(i) => Ok(Bson::Double(*i as f64)),
                Bson::Int64(i) => Ok(Bson::Double(*i as f64)),
                Bson::String(s) => parse_double_str(s).map(Bson::Double).ok_or_else(|| {
                    ShellError::new(format!("invalid number \"{s}\""), a.span.clone())
                }),
                v => c.err(
                    format!("Double() expects a number, got {}", type_name(v)),
                    a.span.clone(),
                ),
            }
        }
        "NumberDecimal" | "Decimal128" => {
            arity(c, name, &args, 0, 1, &span)?;
            let Some(a) = args.first() else {
                return Ok(Bson::Decimal128(Decimal128::from_str("0").expect("zero")));
            };
            let text = match &a.value {
                Bson::String(s) => s.clone(),
                Bson::Int32(i) => i.to_string(),
                Bson::Int64(i) => i.to_string(),
                Bson::Double(f) => format_f64_plain(*f),
                v => {
                    return c.err(
                        format!("{name}() expects a numeric string, got {}", type_name(v)),
                        a.span.clone(),
                    );
                }
            };
            parse_decimal(&text).map(Bson::Decimal128).ok_or_else(|| {
                ShellError::new(format!("invalid decimal \"{text}\""), a.span.clone())
            })
        }
        "UUID" => {
            arity(c, name, &args, 0, 1, &span)?;
            match args.first() {
                None => Ok(Bson::Binary(Binary::from(bson::Uuid::new()))),
                Some(a) => {
                    let s = str_arg(c, name, a)?;
                    parse_uuid_str(s).map(Bson::Binary).ok_or_else(|| {
                        ShellError::new(
                            format!(
                                "invalid UUID \"{s}\": expected 32 hex digits (dashes optional)"
                            ),
                            a.span.clone(),
                        )
                    })
                }
            }
        }
        "BinData" | "HexData" => {
            arity(c, name, &args, 2, 2, &span)?;
            let sub = int_arg(c, name, &args[0])?;
            let sub = u8::try_from(sub).or_else(|_| {
                c.err(
                    format!("binary subtype {sub} is out of range (0-255)"),
                    args[0].span.clone(),
                )
            })?;
            let s = str_arg(c, name, &args[1])?;
            let bytes = if name == "BinData" {
                base64_decode(s)
                    .ok_or_else(|| ShellError::new("invalid base64 data", args[1].span.clone()))?
            } else {
                hex_decode(s)
                    .ok_or_else(|| ShellError::new("invalid hex data", args[1].span.clone()))?
            };
            Ok(Bson::Binary(Binary {
                subtype: BinarySubtype::from(sub),
                bytes,
            }))
        }
        "Timestamp" => {
            arity(c, name, &args, 0, 2, &span)?;
            match args.as_slice() {
                [] => Ok(Bson::Timestamp(Timestamp { time: 0, increment: 0 })),
                [a] => match &a.value {
                    Bson::Document(d) => timestamp_from_doc(d)
                        .map(Bson::Timestamp)
                        .ok_or_else(|| {
                            ShellError::new(
                                "Timestamp({t: <seconds>, i: <increment>}) expects integer fields t and i",
                                a.span.clone(),
                            )
                        }),
                    Bson::Timestamp(t) => Ok(Bson::Timestamp(*t)),
                    v => c.err(
                        format!(
                            "Timestamp() expects (t, i) or {{t, i}}, got {}",
                            type_name(v)
                        ),
                        a.span.clone(),
                    ),
                },
                [t, i] => Ok(Bson::Timestamp(Timestamp {
                    time: u32_arg(c, name, t)?,
                    increment: u32_arg(c, name, i)?,
                })),
                _ => unreachable!("arity checked"),
            }
        }
        "MinKey" | "MaxKey" => {
            arity(c, name, &args, 0, 0, &span)?;
            Ok(if name == "MinKey" {
                Bson::MinKey
            } else {
                Bson::MaxKey
            })
        }
        "RegExp" => {
            arity(c, name, &args, 1, 2, &span)?;
            let (pattern, flags) = match &args[0].value {
                Bson::RegularExpression(r) => (r.pattern.clone(), r.options.clone()),
                Bson::String(s) => (s.clone(), String::new()),
                v => {
                    return c.err(
                        format!("RegExp() expects a pattern string, got {}", type_name(v)),
                        args[0].span.clone(),
                    );
                }
            };
            let (flags, flags_span) = match args.get(1) {
                Some(a) => (str_arg(c, name, a)?.to_string(), a.span.clone()),
                None => (flags, args[0].span.clone()),
            };
            make_regex(c, pattern, &flags, flags_span)
        }
        "Code" => {
            arity(c, name, &args, 1, 2, &span)?;
            let code = str_arg(c, name, &args[0])?.to_string();
            match args.get(1) {
                None => Ok(Bson::JavaScriptCode(code)),
                Some(a) => match &a.value {
                    Bson::Document(scope) => Ok(Bson::JavaScriptCodeWithScope(
                        bson::JavaScriptCodeWithScope {
                            code,
                            scope: scope.clone(),
                        },
                    )),
                    v => c.err(
                        format!("Code() scope must be a document, got {}", type_name(v)),
                        a.span.clone(),
                    ),
                },
            }
        }
        _ => unreachable!("is_constructor checked"),
    }
}

fn timestamp_from_doc(d: &Document) -> Option<Timestamp> {
    if d.len() != 2 {
        return None;
    }
    let t = u32::try_from(as_integer(d.get("t")?)?).ok()?;
    let i = u32::try_from(as_integer(d.get("i")?)?).ok()?;
    Some(Timestamp {
        time: t,
        increment: i,
    })
}

pub(crate) fn parse_decimal(s: &str) -> Option<Decimal128> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    Decimal128::from_str(s).ok()
}

/// Formats a finite f64 without exponent loss for Decimal128 conversion.
fn format_f64_plain(f: f64) -> String {
    if f.is_nan() {
        "NaN".into()
    } else if f.is_infinite() {
        if f > 0.0 {
            "Infinity".into()
        } else {
            "-Infinity".into()
        }
    } else {
        format!("{f:?}")
    }
}

fn parse_double_str(s: &str) -> Option<f64> {
    match s.trim() {
        "NaN" => Some(f64::NAN),
        "Infinity" | "+Infinity" => Some(f64::INFINITY),
        "-Infinity" => Some(f64::NEG_INFINITY),
        t if t.is_empty()
            || t.contains(|c: char| c.is_ascii_alphabetic() && c != 'e' && c != 'E') =>
        {
            None
        }
        t => t.parse().ok(),
    }
}

// ---------------------------------------------------------------------------
// Extended JSON
// ---------------------------------------------------------------------------

/// Converts Extended JSON wrapper documents (`{"$oid": …}` etc.) into the
/// BSON values they represent. Any other document is returned unchanged.
fn convert_extjson(doc: Document, span: Range<usize>) -> Result<Bson, ShellError> {
    let Some(first) = doc.keys().next() else {
        return Ok(Bson::Document(doc));
    };
    if !first.starts_with('$') {
        return Ok(Bson::Document(doc));
    }
    let bad = |what: &str| -> Result<Bson, ShellError> {
        Err(ShellError::new(
            format!("invalid Extended JSON {what}"),
            span.clone(),
        ))
    };
    let single = doc.len() == 1;
    let (key, value) = doc.iter().next().expect("non-empty");
    match (key.as_str(), single) {
        ("$oid", true) => match value {
            Bson::String(s) => match ObjectId::parse_str(s) {
                Ok(o) => Ok(Bson::ObjectId(o)),
                Err(_) => bad("$oid: expected 24 hexadecimal characters"),
            },
            Bson::ObjectId(o) => Ok(Bson::ObjectId(*o)),
            _ => bad("$oid: expected a hex string"),
        },
        ("$date", true) => match value {
            Bson::String(s) => match parse_date_str(s) {
                Some(d) => Ok(Bson::DateTime(d)),
                None => bad("$date: expected an ISO-8601 date string"),
            },
            Bson::DateTime(d) => Ok(Bson::DateTime(*d)),
            v => match as_integer(v) {
                Some(ms) => Ok(Bson::DateTime(DateTime::from_millis(ms))),
                None => bad("$date: expected a date string or milliseconds"),
            },
        },
        ("$numberLong", true) => match value {
            Bson::String(s) => match s.trim().parse::<i64>() {
                Ok(v) => Ok(Bson::Int64(v)),
                Err(_) => bad("$numberLong: expected an integer string"),
            },
            v => match as_integer(v) {
                Some(i) => Ok(Bson::Int64(i)),
                None => bad("$numberLong: expected an integer string"),
            },
        },
        ("$numberInt", true) => {
            let v = match value {
                Bson::String(s) => s.trim().parse::<i64>().ok(),
                v => as_integer(v),
            };
            match v.and_then(|v| i32::try_from(v).ok()) {
                Some(v) => Ok(Bson::Int32(v)),
                None => bad("$numberInt: expected a 32-bit integer"),
            }
        }
        ("$numberDouble", true) => match value {
            Bson::String(s) => match parse_double_str(s) {
                Some(f) => Ok(Bson::Double(f)),
                None => bad("$numberDouble: expected a numeric string"),
            },
            Bson::Double(f) => Ok(Bson::Double(*f)),
            Bson::Int32(i) => Ok(Bson::Double(*i as f64)),
            Bson::Int64(i) => Ok(Bson::Double(*i as f64)),
            _ => bad("$numberDouble: expected a numeric string"),
        },
        ("$numberDecimal", true) => {
            let text = match value {
                Bson::String(s) => s.clone(),
                Bson::Int32(i) => i.to_string(),
                Bson::Int64(i) => i.to_string(),
                Bson::Double(f) => format_f64_plain(*f),
                Bson::Decimal128(d) => return Ok(Bson::Decimal128(*d)),
                _ => return bad("$numberDecimal: expected a numeric string"),
            };
            match parse_decimal(&text) {
                Some(d) => Ok(Bson::Decimal128(d)),
                None => bad("$numberDecimal: expected a numeric string"),
            }
        }
        ("$binary", true) => match value {
            Bson::Document(b) => {
                let b64 = b.get_str("base64").ok();
                let sub = b.get("subType");
                match (b64, sub, b.len()) {
                    (Some(b64), Some(sub), 2) => {
                        let sub = match sub {
                            Bson::String(s) => u8::from_str_radix(s, 16).ok(),
                            v => as_integer(v).and_then(|i| u8::try_from(i).ok()),
                        };
                        match (base64_decode(b64), sub) {
                            (Some(bytes), Some(sub)) => Ok(Bson::Binary(Binary {
                                subtype: BinarySubtype::from(sub),
                                bytes,
                            })),
                            _ => bad("$binary: invalid base64 or subType"),
                        }
                    }
                    _ => bad("$binary: expected {base64: \"…\", subType: \"hex\"}"),
                }
            }
            _ => bad("$binary: expected {base64: \"…\", subType: \"hex\"}"),
        },
        ("$binary", false) if doc.len() == 2 && doc.contains_key("$type") => {
            // legacy {"$binary": "<base64>", "$type": "<hex>"}
            let b64 = doc.get_str("$binary").ok();
            let sub = doc
                .get_str("$type")
                .ok()
                .and_then(|s| u8::from_str_radix(s, 16).ok());
            match (b64.and_then(base64_decode), sub) {
                (Some(bytes), Some(sub)) => Ok(Bson::Binary(Binary {
                    subtype: BinarySubtype::from(sub),
                    bytes,
                })),
                _ => bad("$binary: invalid base64 or $type"),
            }
        }
        ("$uuid", true) => match value {
            Bson::String(s) => match parse_uuid_str(s) {
                Some(b) => Ok(Bson::Binary(b)),
                None => bad("$uuid: expected a UUID string"),
            },
            _ => bad("$uuid: expected a UUID string"),
        },
        ("$timestamp", true) => match value {
            Bson::Document(d) => match timestamp_from_doc(d) {
                Some(t) => Ok(Bson::Timestamp(t)),
                None => bad("$timestamp: expected {t: <int>, i: <int>}"),
            },
            _ => bad("$timestamp: expected {t: <int>, i: <int>}"),
        },
        ("$regularExpression", true) => match value {
            Bson::Document(d) => match (d.get_str("pattern"), d.get_str("options")) {
                (Ok(p), Ok(o)) if d.len() == 2 => {
                    let mut opts: Vec<char> = o.chars().collect();
                    opts.sort_unstable();
                    Ok(Bson::RegularExpression(Regex {
                        pattern: p.to_string(),
                        options: opts.into_iter().collect(),
                    }))
                }
                _ => bad("$regularExpression: expected {pattern: \"…\", options: \"…\"}"),
            },
            _ => bad("$regularExpression: expected {pattern: \"…\", options: \"…\"}"),
        },
        ("$minKey", true) => Ok(Bson::MinKey),
        ("$maxKey", true) => Ok(Bson::MaxKey),
        ("$undefined", true) => Ok(Bson::Null),
        ("$symbol", true) => match value {
            Bson::String(s) => Ok(Bson::Symbol(s.clone())),
            _ => bad("$symbol: expected a string"),
        },
        ("$code", true) => match value {
            Bson::String(s) => Ok(Bson::JavaScriptCode(s.clone())),
            _ => bad("$code: expected a string"),
        },
        ("$code", false) if doc.len() == 2 => {
            match (doc.get_str("$code"), doc.get_document("$scope")) {
                (Ok(code), Ok(scope)) => Ok(Bson::JavaScriptCodeWithScope(
                    bson::JavaScriptCodeWithScope {
                        code: code.to_string(),
                        scope: scope.clone(),
                    },
                )),
                _ => Ok(Bson::Document(doc)),
            }
        }
        ("$dbPointer", true) => {
            let json = Bson::Document(doc.clone()).into_relaxed_extjson();
            match Bson::try_from(json) {
                Ok(v @ Bson::DbPointer(_)) => Ok(v),
                _ => bad("$dbPointer: expected {$ref: \"ns\", $id: ObjectId(…)}"),
            }
        }
        _ => Ok(Bson::Document(doc)),
    }
}

// ---------------------------------------------------------------------------
// Dates
// ---------------------------------------------------------------------------

/// Days since 1970-01-01 for a proleptic Gregorian date.
pub(crate) fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m as i64 + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Inverse of [`days_from_civil`].
pub(crate) fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn days_in_month(y: i64, m: u32) -> u32 {
    match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        _ => {
            if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 {
                29
            } else {
                28
            }
        }
    }
}

/// Parses ISO-8601 dates: `YYYY`, `YYYY-MM`, `YYYY-MM-DD`, optionally
/// followed by `T` (or a space) `HH:MM[:SS[.fff]]` and `Z` / `±HH:MM` /
/// `±HHMM` / `±HH`. Expanded years (`+YYYYYY` / `-YYYYYY`) are accepted.
/// A missing offset means UTC (as in mongosh's `ISODate`).
pub(crate) fn parse_date_str(s: &str) -> Option<DateTime> {
    let b = s.trim().as_bytes();
    let mut i = 0usize;
    let num = |i: &mut usize, n: usize| -> Option<i64> {
        let slice = b.get(*i..*i + n)?;
        if !slice.iter().all(u8::is_ascii_digit) {
            return None;
        }
        *i += n;
        std::str::from_utf8(slice).ok()?.parse().ok()
    };
    let year = match b.first()? {
        b'+' | b'-' => {
            let neg = b[0] == b'-';
            i = 1;
            let y = num(&mut i, 6)?;
            if neg { -y } else { y }
        }
        _ => num(&mut i, 4)?,
    };
    let (mut month, mut day) = (1u32, 1u32);
    if b.get(i) == Some(&b'-') {
        i += 1;
        month = num(&mut i, 2)? as u32;
        if b.get(i) == Some(&b'-') {
            i += 1;
            day = num(&mut i, 2)? as u32;
        }
    }
    if !(1..=12).contains(&month) || day < 1 || day > days_in_month(year, month) {
        return None;
    }
    let (mut h, mut mi, mut sec, mut ms, mut offset_min) = (0i64, 0i64, 0i64, 0i64, 0i64);
    if matches!(b.get(i), Some(b'T' | b't' | b' ')) {
        i += 1;
        h = num(&mut i, 2)?;
        if b.get(i) != Some(&b':') {
            return None;
        }
        i += 1;
        mi = num(&mut i, 2)?;
        if b.get(i) == Some(&b':') {
            i += 1;
            sec = num(&mut i, 2)?;
            if matches!(b.get(i), Some(b'.' | b',')) {
                i += 1;
                let s0 = i;
                while b.get(i).is_some_and(u8::is_ascii_digit) {
                    i += 1;
                }
                if i == s0 {
                    return None;
                }
                let digits = std::str::from_utf8(&b[s0..i.min(s0 + 3)]).ok()?;
                let mut v: i64 = digits.parse().ok()?;
                for _ in digits.len()..3 {
                    v *= 10;
                }
                ms = v;
            }
        }
        if h > 24 || mi > 59 || sec > 60 || (h == 24 && (mi, sec, ms) != (0, 0, 0)) {
            return None;
        }
        match b.get(i) {
            Some(b'Z' | b'z') => i += 1,
            Some(sign @ (b'+' | b'-')) => {
                let neg = *sign == b'-';
                i += 1;
                let oh = num(&mut i, 2)?;
                let mut om = 0;
                if b.get(i) == Some(&b':') {
                    i += 1;
                    om = num(&mut i, 2)?;
                } else if b.get(i).is_some_and(u8::is_ascii_digit) {
                    om = num(&mut i, 2)?;
                }
                if oh > 23 || om > 59 {
                    return None;
                }
                offset_min = (oh * 60 + om) * if neg { -1 } else { 1 };
            }
            _ => {}
        }
    } else if matches!(b.get(i), Some(b'Z' | b'z')) {
        i += 1;
    }
    if i != b.len() {
        return None;
    }
    let days = days_from_civil(year, month, day) as i128;
    let ms_total = days * 86_400_000 + (h * 3_600_000 + mi * 60_000 + sec * 1000 + ms) as i128
        - offset_min as i128 * 60_000;
    i64::try_from(ms_total).ok().map(DateTime::from_millis)
}

// ---------------------------------------------------------------------------
// Binary helpers
// ---------------------------------------------------------------------------

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

pub(crate) fn base64_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = match chunk.len() {
            3 => (chunk[0] as u32) << 16 | (chunk[1] as u32) << 8 | chunk[2] as u32,
            2 => (chunk[0] as u32) << 16 | (chunk[1] as u32) << 8,
            _ => (chunk[0] as u32) << 16,
        };
        let idx = [(n >> 18) & 63, (n >> 12) & 63, (n >> 6) & 63, n & 63];
        for (k, ix) in idx.iter().enumerate() {
            if k <= chunk.len() {
                out.push(B64[*ix as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// Decodes standard or URL-safe base64; padding optional, whitespace ignored.
pub(crate) fn base64_decode(s: &str) -> Option<Vec<u8>> {
    let mut vals = Vec::with_capacity(s.len());
    let mut padding = 0;
    for ch in s.bytes() {
        let v = match ch {
            b'A'..=b'Z' => ch - b'A',
            b'a'..=b'z' => ch - b'a' + 26,
            b'0'..=b'9' => ch - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b'=' => {
                padding += 1;
                continue;
            }
            c if c.is_ascii_whitespace() => continue,
            _ => return None,
        };
        if padding > 0 {
            return None; // data after padding
        }
        vals.push(v as u32);
    }
    if vals.len() % 4 == 1 || padding > 2 {
        return None;
    }
    let mut out = Vec::with_capacity(vals.len() * 3 / 4);
    for chunk in vals.chunks(4) {
        let mut n = 0u32;
        for (k, v) in chunk.iter().enumerate() {
            n |= v << (18 - 6 * k);
        }
        out.push((n >> 16) as u8);
        if chunk.len() > 2 {
            out.push((n >> 8) as u8);
        }
        if chunk.len() > 3 {
            out.push(n as u8);
        }
    }
    Some(out)
}

pub(crate) fn hex_decode(s: &str) -> Option<Vec<u8>> {
    let s = s.trim();
    if !s.len().is_multiple_of(2) || !s.is_ascii() {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok())
        .collect()
}

/// Parses a UUID string (dashes optional, braces optional) into a subtype-4 binary.
pub(crate) fn parse_uuid_str(s: &str) -> Option<Binary> {
    let hex: String = s
        .trim()
        .trim_start_matches('{')
        .trim_end_matches('}')
        .chars()
        .filter(|c| *c != '-')
        .collect();
    if hex.len() != 32 {
        return None;
    }
    let bytes = hex_decode(&hex)?;
    Some(Binary {
        subtype: BinarySubtype::Uuid,
        bytes,
    })
}
