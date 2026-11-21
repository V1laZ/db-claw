use std::str::FromStr;

use bson::spec::BinarySubtype;
use bson::{Binary, Bson, DateTime, Decimal128, Document, Regex, Timestamp, doc, oid::ObjectId};

use super::*;

fn v(s: &str) -> Bson {
    parse_value(s).unwrap_or_else(|e| panic!("parse_value({s:?}) failed: {e} at {:?}", e.span))
}

fn err(s: &str) -> ShellError {
    match parse_value(s) {
        Ok(v) => panic!("expected error for {s:?}, got {v:?}"),
        Err(e) => e,
    }
}

fn cmd(s: &str) -> MongoCommand {
    parse_command(s).unwrap_or_else(|e| panic!("parse_command({s:?}) failed: {e} at {:?}", e.span))
}

fn cmd_err(s: &str) -> ShellError {
    match parse_command(s) {
        Ok(c) => panic!("expected error for {s:?}, got {c:?}"),
        Err(e) => e,
    }
}

fn oid(hex: &str) -> ObjectId {
    ObjectId::parse_str(hex).unwrap()
}

fn date(s: &str) -> Bson {
    Bson::DateTime(DateTime::parse_rfc3339_str(s).unwrap())
}

fn dec(s: &str) -> Bson {
    Bson::Decimal128(Decimal128::from_str(s).unwrap())
}

fn regex(p: &str, o: &str) -> Bson {
    Bson::RegularExpression(Regex {
        pattern: p.into(),
        options: o.into(),
    })
}

const HEX: &str = "507f1f77bcf86cd799439011";

// ---------------------------------------------------------------------------
// Literal parser: scalars
// ---------------------------------------------------------------------------

#[test]
fn numbers() {
    assert_eq!(v("0"), Bson::Int32(0));
    assert_eq!(v("42"), Bson::Int32(42));
    assert_eq!(v("-42"), Bson::Int32(-42));
    assert_eq!(v("+7"), Bson::Int32(7));
    assert_eq!(v("2147483647"), Bson::Int32(i32::MAX));
    assert_eq!(v("-2147483648"), Bson::Int32(i32::MIN));
    assert_eq!(v("2147483648"), Bson::Int64(2147483648));
    assert_eq!(v("-2147483649"), Bson::Int64(-2147483649));
    assert_eq!(v("9223372036854775807"), Bson::Int64(i64::MAX));
    assert_eq!(v("-9223372036854775808"), Bson::Int64(i64::MIN));
    assert_eq!(
        v("9223372036854775808"),
        Bson::Double(9223372036854775808.0)
    );
    assert_eq!(v("1.5"), Bson::Double(1.5));
    assert_eq!(v("-0.25"), Bson::Double(-0.25));
    assert_eq!(v(".5"), Bson::Double(0.5));
    assert_eq!(v("5."), Bson::Double(5.0));
    assert_eq!(v("5.0"), Bson::Double(5.0));
    assert_eq!(v("1e3"), Bson::Double(1000.0));
    assert_eq!(v("1E-3"), Bson::Double(0.001));
    assert_eq!(v("-2.5e+2"), Bson::Double(-250.0));
    assert_eq!(v("0x1F"), Bson::Int32(31));
    assert_eq!(v("- 3"), Bson::Int32(-3));
    assert_eq!(v("Infinity"), Bson::Double(f64::INFINITY));
    assert_eq!(v("-Infinity"), Bson::Double(f64::NEG_INFINITY));
    assert_eq!(v("+Infinity"), Bson::Double(f64::INFINITY));
    assert!(matches!(v("NaN"), Bson::Double(f) if f.is_nan()));
    match v("-0.0") {
        Bson::Double(f) => assert!(f == 0.0 && f.is_sign_negative()),
        other => panic!("{other:?}"),
    }
}

#[test]
fn number_errors() {
    let e = err("12abc");
    assert!(e.message.contains("invalid number"), "{}", e.message);
    assert_eq!(e.span, 0..5);
    let e = err("1.2.3");
    assert_eq!(e.span, 0..5);
    let e = err("-");
    assert!(
        e.message.contains("expected a number after `-`"),
        "{}",
        e.message
    );
}

#[test]
fn keywords() {
    assert_eq!(v("true"), Bson::Boolean(true));
    assert_eq!(v("false"), Bson::Boolean(false));
    assert_eq!(v("null"), Bson::Null);
    assert_eq!(v("undefined"), Bson::Null);
}

#[test]
fn strings_and_escapes() {
    assert_eq!(v(r#""hello""#), Bson::String("hello".into()));
    assert_eq!(v("'single'"), Bson::String("single".into()));
    assert_eq!(v(r#"'it\'s'"#), Bson::String("it's".into()));
    assert_eq!(v(r#""say \"hi\"""#), Bson::String("say \"hi\"".into()));
    assert_eq!(v(r#""a'b""#), Bson::String("a'b".into()));
    assert_eq!(
        v(r#""\n\r\t\b\f\v\0\\\/""#),
        Bson::String("\n\r\t\u{8}\u{c}\u{b}\0\\/".into())
    );
    assert_eq!(v(r#""\x41B\u{1F600}""#), Bson::String("AB😀".into()));
    assert_eq!(v(r#""😀""#), Bson::String("😀".into()));
    assert_eq!(v(r#""\q""#), Bson::String("q".into()));
    assert_eq!(v("\"line\\\ncont\""), Bson::String("linecont".into()));
    assert_eq!(v("\"héllo 世界\""), Bson::String("héllo 世界".into()));
    assert_eq!(v("\"\""), Bson::String(String::new()));
}

#[test]
fn string_errors() {
    let e = err("\"abc");
    assert!(e.message.contains("unterminated string"), "{}", e.message);
    assert_eq!(e.span, 0..4);
    let e = err("\"ab\ncd\"");
    assert!(e.message.contains("unterminated string"));
    assert_eq!(e.span.start, 0);
    let e = err(r#""\u12""#);
    assert!(e.message.contains("hex digits"), "{}", e.message);
}

// ---------------------------------------------------------------------------
// Literal parser: objects and arrays
// ---------------------------------------------------------------------------

#[test]
fn objects_keys_and_nesting() {
    assert_eq!(
        v(r#"{name: "x", 'quoted key': 1, "address.city": "Paris", $gt: 5, _id: 2, $$ROOT: 3}"#),
        Bson::Document(doc! {
            "name": "x", "quoted key": 1, "address.city": "Paris", "$gt": 5, "_id": 2, "$$ROOT": 3
        })
    );
    assert_eq!(
        v("{age: {$gt: 30, $lte: 40}, tags: {$in: ['a', \"b\"]}}"),
        Bson::Document(doc! { "age": {"$gt": 30, "$lte": 40}, "tags": {"$in": ["a", "b"]} })
    );
    assert_eq!(v("{}"), Bson::Document(Document::new()));
    assert_eq!(v("[]"), Bson::Array(vec![]));
    assert_eq!(
        v("{1: 'a', true: 2, null: 3}"),
        Bson::Document(doc! {"1": "a", "true": 2, "null": 3})
    );
    assert_eq!(
        v("[1, [2, [3]], {a: []}]"),
        Bson::Array(vec![
            Bson::Int32(1),
            Bson::Array(vec![Bson::Int32(2), Bson::Array(vec![Bson::Int32(3)])]),
            Bson::Document(doc! {"a": []}),
        ])
    );
    // duplicate keys: last one wins (JS semantics)
    assert_eq!(v("{a: 1, a: 2}"), Bson::Document(doc! {"a": 2}));
}

#[test]
fn comments_trailing_commas_whitespace() {
    let input = r#"
        // leading comment
        {
            name: "x", // trailing comment
            /* block
               comment */ tags: ["a", "b",],
            n: 1,
        } ;
    "#;
    assert_eq!(
        v(input),
        Bson::Document(doc! {"name": "x", "tags": ["a", "b"], "n": 1})
    );
    assert_eq!(
        v("[1,2,]"),
        Bson::Array(vec![Bson::Int32(1), Bson::Int32(2)])
    );
    assert_eq!(v("\u{feff}{a:1}"), Bson::Document(doc! {"a": 1}));
}

#[test]
fn object_errors_and_spans() {
    let e = err("{a: 1");
    assert!(e.message.contains("unterminated object"), "{}", e.message);
    assert_eq!(e.span, 0..5);

    let e = err("{a 1}");
    assert!(
        e.message.contains("expected `:` after field name `a`"),
        "{}",
        e.message
    );
    assert_eq!(e.span, 3..4);

    let e = err("{a: 1 b: 2}");
    assert!(e.message.contains("expected `,` or `}`"), "{}", e.message);
    assert_eq!(e.span, 6..7);

    let e = err("{name: foo}");
    assert!(
        e.message.contains("unknown identifier `foo`"),
        "{}",
        e.message
    );
    assert_eq!(e.span, 7..10);

    let e = err("[1, 2");
    assert!(e.message.contains("unterminated array"), "{}", e.message);

    let e = err("{a: 1} extra");
    assert!(e.message.contains("unexpected"), "{}", e.message);
    assert_eq!(e.span, 7..12);

    let e = err("{a: 1, /* never closed");
    assert!(
        e.message.contains("unterminated /* comment"),
        "{}",
        e.message
    );

    let e = err("");
    assert!(e.message.contains("expected a value"), "{}", e.message);

    let e = err("{,}");
    assert!(e.message.contains("expected a field name"), "{}", e.message);
    assert_eq!(e.span, 1..2);

    let e = err("@");
    assert_eq!(e.span, 0..1);
}

#[test]
fn deep_nesting_is_rejected_not_overflowing() {
    let input = "[".repeat(10_000);
    let e = err(&input);
    assert!(e.message.contains("nested too deeply"), "{}", e.message);
}

#[test]
fn parse_document_requires_object() {
    assert_eq!(parse_document("{a: 1}").unwrap(), doc! {"a": 1});
    let e = parse_document("[1]").unwrap_err();
    assert!(e.message.contains("expected a document"), "{}", e.message);
    assert_eq!(e.span, 0..1);
    let e = parse_document("  {$oid: '507f1f77bcf86cd799439011'}").unwrap_err();
    assert!(e.message.contains("an ObjectId"), "{}", e.message);
    assert_eq!(e.span, 2..36);
}

// ---------------------------------------------------------------------------
// Literal parser: constructors
// ---------------------------------------------------------------------------

#[test]
fn object_id() {
    let want = Bson::ObjectId(oid(HEX));
    assert_eq!(v(&format!("ObjectId(\"{HEX}\")")), want);
    assert_eq!(v(&format!("new ObjectId('{HEX}')")), want);
    assert_eq!(v(&format!("ObjectID(\"{HEX}\")")), want);
    assert!(matches!(v("ObjectId()"), Bson::ObjectId(_)));
    let e = err("ObjectId(\"xyz\")");
    assert!(e.message.contains("invalid ObjectId"), "{}", e.message);
    assert_eq!(e.span, 9..14);
    let e = err("ObjectId(1)");
    assert!(e.message.contains("expects a string"), "{}", e.message);
}

#[test]
fn dates() {
    assert_eq!(
        v("ISODate(\"2024-01-01T00:00:00Z\")"),
        date("2024-01-01T00:00:00Z")
    );
    assert_eq!(v("ISODate(\"2024-01-01\")"), date("2024-01-01T00:00:00Z"));
    assert_eq!(
        v("new Date(\"2024-03-05T10:20:30.123Z\")"),
        date("2024-03-05T10:20:30.123Z")
    );
    assert_eq!(
        v("Date('2024-03-05T10:20:30.123456Z')"),
        date("2024-03-05T10:20:30.123Z")
    );
    assert_eq!(
        v("new ISODate(\"2024-03-05T10:20:30+02:00\")"),
        date("2024-03-05T08:20:30Z")
    );
    assert_eq!(
        v("ISODate(\"2024-03-05T10:20:30-0130\")"),
        date("2024-03-05T11:50:30Z")
    );
    assert_eq!(
        v("ISODate(\"2024-03-05 10:20\")"),
        date("2024-03-05T10:20:00Z")
    );
    assert_eq!(
        v("ISODate(\"2024-03-05T10:20:30\")"),
        date("2024-03-05T10:20:30Z")
    );
    assert_eq!(v("ISODate(\"2024-03\")"), date("2024-03-01T00:00:00Z"));
    assert_eq!(
        v("ISODate(\"1969-12-31T23:59:59.999Z\")"),
        Bson::DateTime(DateTime::from_millis(-1))
    );
    assert_eq!(v("ISODate(\"2024-02-29\")"), date("2024-02-29T00:00:00Z"));
    assert_eq!(
        v("new Date(1700000000000)"),
        Bson::DateTime(DateTime::from_millis(1_700_000_000_000))
    );
    assert_eq!(v("new Date(-5)"), Bson::DateTime(DateTime::from_millis(-5)));
    assert_eq!(v("new Date(2024, 0, 15)"), date("2024-01-15T00:00:00Z"));
    assert_eq!(
        v("new Date(2024, 13, 1, 2, 3, 4, 5)"),
        date("2025-02-01T02:03:04.005Z")
    );
    assert!(matches!(v("new Date()"), Bson::DateTime(_)));
    assert!(matches!(v("ISODate()"), Bson::DateTime(_)));
    assert_eq!(
        v("ISODate(\"+012345-01-01T00:00:00.000Z\")"),
        Bson::DateTime(DateTime::from_millis(327_403_382_400_000))
    );

    for bad in [
        "2024-02-30",
        "2023-02-29",
        "2024-13-01",
        "2024-01-01T25:00",
        "yesterday",
        "2024-01-01T10",
        "",
    ] {
        let e = err(&format!("ISODate(\"{bad}\")"));
        assert!(e.message.contains("invalid date"), "{bad}: {}", e.message);
        assert_eq!(e.span.start, 8);
    }
}

#[test]
fn integer_and_decimal_constructors() {
    assert_eq!(v("NumberInt(5)"), Bson::Int32(5));
    assert_eq!(v("NumberInt(\"5\")"), Bson::Int32(5));
    assert_eq!(v("new NumberInt('-7')"), Bson::Int32(-7));
    assert_eq!(v("Int32(3)"), Bson::Int32(3));
    assert!(err("NumberInt(3000000000)").message.contains("32-bit"));
    assert!(err("NumberInt(1.5)").message.contains("expects an integer"));

    assert_eq!(v("NumberLong(5)"), Bson::Int64(5));
    assert_eq!(v("NumberLong(\"5\")"), Bson::Int64(5));
    assert_eq!(
        v("NumberLong(\"9223372036854775807\")"),
        Bson::Int64(i64::MAX)
    );
    assert_eq!(
        v("new NumberLong(-9223372036854775808)"),
        Bson::Int64(i64::MIN)
    );
    assert_eq!(v("Long(2)"), Bson::Int64(2));
    assert!(
        err("NumberLong(\"abc\")")
            .message
            .contains("expects an integer")
    );

    assert_eq!(v("Double(5)"), Bson::Double(5.0));
    assert_eq!(v("Double('2.5')"), Bson::Double(2.5));

    assert_eq!(v("NumberDecimal(\"1.5\")"), dec("1.5"));
    assert_eq!(v("Decimal128(\"-0.001\")"), dec("-0.001"));
    assert_eq!(v("new NumberDecimal('1E+10')"), dec("1E+10"));
    assert_eq!(v("NumberDecimal(\"NaN\")"), dec("NaN"));
    assert_eq!(v("NumberDecimal(\"-Infinity\")"), dec("-Infinity"));
    assert_eq!(v("NumberDecimal(42)"), dec("42"));
    assert_eq!(
        v("NumberDecimal(\"1234567890123456789012345678901234\")"),
        dec("1234567890123456789012345678901234")
    );
    let e = err("NumberDecimal(\"1.2.3\")");
    assert!(e.message.contains("invalid decimal"), "{}", e.message);
    assert_eq!(e.span, 14..21);
}

#[test]
fn binary_uuid() {
    let uuid_bytes = vec![
        0x12, 0x3e, 0x45, 0x67, 0xe8, 0x9b, 0x12, 0xd3, 0xa4, 0x56, 0x42, 0x66, 0x14, 0x17, 0x40,
        0x00,
    ];
    let want = Bson::Binary(Binary {
        subtype: BinarySubtype::Uuid,
        bytes: uuid_bytes.clone(),
    });
    assert_eq!(v("UUID(\"123e4567-e89b-12d3-a456-426614174000\")"), want);
    assert_eq!(v("UUID('123e4567e89b12d3a456426614174000')"), want);
    assert_eq!(
        v("new UUID(\"{123E4567-E89B-12D3-A456-426614174000}\")"),
        want
    );
    assert!(matches!(
        v("UUID()"),
        Bson::Binary(Binary {
            subtype: BinarySubtype::Uuid,
            ..
        })
    ));
    assert!(err("UUID(\"1234\")").message.contains("invalid UUID"));

    assert_eq!(
        v("BinData(0, \"aGVsbG8=\")"),
        Bson::Binary(Binary {
            subtype: BinarySubtype::Generic,
            bytes: b"hello".to_vec()
        })
    );
    assert_eq!(
        v("BinData(128, 'aGVsbG8')"),
        Bson::Binary(Binary {
            subtype: BinarySubtype::UserDefined(128),
            bytes: b"hello".to_vec()
        })
    );
    assert_eq!(v("BinData(4, \"Ej5FZ+ibEtOkVkJmFBdAAA==\")"), want);
    assert_eq!(
        v("HexData(0, \"68656c6c6f\")"),
        Bson::Binary(Binary {
            subtype: BinarySubtype::Generic,
            bytes: b"hello".to_vec()
        })
    );
    assert!(err("BinData(0, \"!!\")").message.contains("invalid base64"));
    assert!(err("BinData(300, \"\")").message.contains("subtype"));
    assert!(err("BinData(0)").message.contains("takes 2 arguments"));
}

#[test]
fn timestamp_minmax_code() {
    let want = Bson::Timestamp(Timestamp {
        time: 1_700_000_000,
        increment: 3,
    });
    assert_eq!(v("Timestamp(1700000000, 3)"), want);
    assert_eq!(v("new Timestamp({t: 1700000000, i: 3})"), want);
    assert_eq!(v("Timestamp({i: 3, t: 1700000000})"), want);
    assert!(err("Timestamp(-1, 0)").message.contains("out of range"));
    assert!(
        err("Timestamp({t: 1})")
            .message
            .contains("expects integer fields")
    );

    assert_eq!(v("MinKey()"), Bson::MinKey);
    assert_eq!(v("MaxKey()"), Bson::MaxKey);
    assert_eq!(v("MinKey"), Bson::MinKey);
    assert_eq!(v("{a: MaxKey}"), Bson::Document(doc! {"a": Bson::MaxKey}));

    assert_eq!(
        v("Code(\"function() {}\")"),
        Bson::JavaScriptCode("function() {}".into())
    );
    assert_eq!(
        v("Code(\"x\", {a: 1})"),
        Bson::JavaScriptCodeWithScope(bson::JavaScriptCodeWithScope {
            code: "x".into(),
            scope: doc! {"a": 1}
        })
    );
}

#[test]
fn constructor_errors() {
    let e = err("new Foo(1)");
    assert!(
        e.message.contains("unknown constructor `Foo`"),
        "{}",
        e.message
    );
    assert_eq!(e.span, 4..7);
    let e = err("ObjectId");
    assert!(
        e.message.contains("expected `(` after `ObjectId`"),
        "{}",
        e.message
    );
    let e = err("ObjectId(\"a\", \"b\")");
    assert!(e.message.contains("takes 0 to 1 argument"), "{}", e.message);
    assert_eq!(e.span, 0..18);
    let e = err("ObjectId(\"a\"");
    assert!(
        e.message.contains("unterminated argument list"),
        "{}",
        e.message
    );
}

#[test]
fn regexes() {
    assert_eq!(v("/abc/"), regex("abc", ""));
    assert_eq!(v("/^a.*z$/im"), regex("^a.*z$", "im"));
    assert_eq!(v("/x/mi"), regex("x", "im"));
    assert_eq!(v("/x/gi"), regex("x", "i"));
    assert_eq!(v("/a\\/b/"), regex("a/b", ""));
    assert_eq!(v("/[/]x/"), regex("[/]x", ""));
    assert_eq!(v("/\\d+\\.\\d*/"), regex("\\d+\\.\\d*", ""));
    assert_eq!(
        v("{name: /^jo/i}"),
        Bson::Document(doc! {"name": regex("^jo", "i")})
    );
    assert_eq!(v("RegExp(\"^a\", \"i\")"), regex("^a", "i"));
    assert_eq!(v("new RegExp('a/b')"), regex("a/b", ""));
    assert_eq!(v("RegExp(/x/i, 'm')"), regex("x", "m"));

    let e = err("/abc");
    assert!(
        e.message.contains("unterminated regular expression"),
        "{}",
        e.message
    );
    let e = err("/abc/q");
    assert!(
        e.message.contains("invalid regular expression flag `q`"),
        "{}",
        e.message
    );
    assert_eq!(e.span, 5..6);
}

// ---------------------------------------------------------------------------
// Extended JSON
// ---------------------------------------------------------------------------

#[test]
fn extended_json() {
    assert_eq!(
        v(&format!(r#"{{"$oid": "{HEX}"}}"#)),
        Bson::ObjectId(oid(HEX))
    );
    assert_eq!(
        v(r#"{"$date": "2024-01-01T00:00:00Z"}"#),
        date("2024-01-01T00:00:00Z")
    );
    assert_eq!(
        v(r#"{"$date": "2024-01-01T00:00:00.5+01:00"}"#),
        date("2023-12-31T23:00:00.500Z")
    );
    assert_eq!(
        v(r#"{"$date": {"$numberLong": "1700000000000"}}"#),
        Bson::DateTime(DateTime::from_millis(1_700_000_000_000))
    );
    assert_eq!(
        v(r#"{"$date": 1700000000000}"#),
        Bson::DateTime(DateTime::from_millis(1_700_000_000_000))
    );
    assert_eq!(v(r#"{"$numberLong": "12"}"#), Bson::Int64(12));
    assert_eq!(v(r#"{"$numberInt": "12"}"#), Bson::Int32(12));
    assert_eq!(v(r#"{"$numberInt": 12}"#), Bson::Int32(12));
    assert_eq!(v(r#"{"$numberDouble": "1.5"}"#), Bson::Double(1.5));
    assert_eq!(
        v(r#"{"$numberDouble": "-Infinity"}"#),
        Bson::Double(f64::NEG_INFINITY)
    );
    assert!(matches!(v(r#"{"$numberDouble": "NaN"}"#), Bson::Double(f) if f.is_nan()));
    assert_eq!(v(r#"{"$numberDecimal": "1.50"}"#), dec("1.50"));
    assert_eq!(
        v(r#"{"$binary": {"base64": "aGVsbG8=", "subType": "00"}}"#),
        Bson::Binary(Binary {
            subtype: BinarySubtype::Generic,
            bytes: b"hello".to_vec()
        })
    );
    assert_eq!(
        v(r#"{"$binary": "aGVsbG8=", "$type": "05"}"#),
        Bson::Binary(Binary {
            subtype: BinarySubtype::Md5,
            bytes: b"hello".to_vec()
        })
    );
    assert!(matches!(
        v(r#"{"$uuid": "123e4567-e89b-12d3-a456-426614174000"}"#),
        Bson::Binary(Binary {
            subtype: BinarySubtype::Uuid,
            ..
        })
    ));
    assert_eq!(
        v(r#"{"$timestamp": {"t": 5, "i": 6}}"#),
        Bson::Timestamp(Timestamp {
            time: 5,
            increment: 6
        })
    );
    assert_eq!(
        v(r#"{"$regularExpression": {"pattern": "^a", "options": "mi"}}"#),
        regex("^a", "im")
    );
    assert_eq!(v(r#"{"$minKey": 1}"#), Bson::MinKey);
    assert_eq!(v(r#"{"$maxKey": 1}"#), Bson::MaxKey);
    assert_eq!(v(r#"{"$symbol": "s"}"#), Bson::Symbol("s".into()));
    assert_eq!(v(r#"{"$code": "x"}"#), Bson::JavaScriptCode("x".into()));
    assert!(matches!(
        v(&format!(
            r#"{{"$dbPointer": {{"$ref": "db.c", "$id": {{"$oid": "{HEX}"}}}}}}"#
        )),
        Bson::DbPointer(_)
    ));

    // A pasted document from another tool.
    let pasted = format!(
        r#"{{
          "_id": {{"$oid": "{HEX}"}},
          "createdAt": {{"$date": {{"$numberLong": "0"}}}},
          "count": {{"$numberLong": "3"}},
          "price": {{"$numberDecimal": "9.99"}}
        }}"#
    );
    assert_eq!(
        parse_document(&pasted).unwrap(),
        doc! {
            "_id": oid(HEX),
            "createdAt": DateTime::from_millis(0),
            "count": 3i64,
            "price": Decimal128::from_str("9.99").unwrap(),
        }
    );

    // Query operators are left alone.
    assert_eq!(
        v("{name: {$regex: 'a', $options: 'i'}, $type: 'string'}"),
        Bson::Document(doc! {"name": {"$regex": "a", "$options": "i"}, "$type": "string"})
    );
    assert_eq!(v("{$gt: 5}"), Bson::Document(doc! {"$gt": 5}));

    let e = err(r#"{"$oid": "nope"}"#);
    assert!(
        e.message.contains("invalid Extended JSON $oid"),
        "{}",
        e.message
    );
    assert_eq!(e.span, 0..16);
    assert!(
        err(r#"{"$numberLong": "x"}"#)
            .message
            .contains("$numberLong")
    );
    assert!(err(r#"{"$date": true}"#).message.contains("$date"));
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

#[test]
fn find_with_chain() {
    assert_eq!(
        cmd("db.users.find({age: {$gt: 30}}, {name: 1}).sort({name: 1}).skip(10).limit(20)"),
        MongoCommand::Find {
            collection: "users".into(),
            filter: doc! {"age": {"$gt": 30}},
            projection: Some(doc! {"name": 1}),
            sort: Some(doc! {"name": 1}),
            skip: Some(10),
            limit: Some(20),
        }
    );
    assert_eq!(
        cmd("db.users.find()"),
        MongoCommand::Find {
            collection: "users".into(),
            filter: doc! {},
            projection: None,
            sort: None,
            skip: None,
            limit: None,
        }
    );
    assert_eq!(
        cmd("  db.users\n  .find({})\n  .project({a: 1})\n  .limit(-5)\n  .toArray();  // done"),
        MongoCommand::Find {
            collection: "users".into(),
            filter: doc! {},
            projection: Some(doc! {"a": 1}),
            sort: None,
            skip: None,
            limit: Some(-5),
        }
    );
    assert_eq!(
        cmd("db.users.find(null).projection({b: 0}).pretty()"),
        MongoCommand::Find {
            collection: "users".into(),
            filter: doc! {},
            projection: Some(doc! {"b": 0}),
            sort: None,
            skip: None,
            limit: None,
        }
    );
    assert_eq!(
        cmd("db.users.find({}, null, {sort: {a: -1}, limit: 5, skip: 2})"),
        MongoCommand::Find {
            collection: "users".into(),
            filter: doc! {},
            projection: None,
            sort: Some(doc! {"a": -1}),
            skip: Some(2),
            limit: Some(5),
        }
    );
}

#[test]
fn find_count_chain() {
    assert_eq!(
        cmd("db.users.find({a: 1}).sort({a: 1}).count()"),
        MongoCommand::Count {
            collection: "users".into(),
            filter: doc! {"a": 1}
        }
    );
    let e = cmd_err("db.users.find().count().limit(1)");
    assert!(
        e.message.contains("cannot be chained after count()"),
        "{}",
        e.message
    );
}

#[test]
fn find_one_and_collection_names() {
    assert_eq!(
        cmd(&format!("db.users.findOne({{_id: ObjectId(\"{HEX}\")}})")),
        MongoCommand::FindOne {
            collection: "users".into(),
            filter: doc! {"_id": oid(HEX)},
            projection: None,
        }
    );
    let find_all = |c: &str| MongoCommand::Find {
        collection: c.into(),
        filter: doc! {},
        projection: None,
        sort: None,
        skip: None,
        limit: None,
    };
    assert_eq!(
        cmd("db.getCollection(\"weird name\").find({})"),
        find_all("weird name")
    );
    assert_eq!(cmd("db[\"weird-name\"].find({})"), find_all("weird-name"));
    assert_eq!(cmd("db['a'].find()"), find_all("a"));
    assert_eq!(cmd("db.system.users.find()"), find_all("system.users"));
    assert_eq!(cmd("db . users . find ( )"), find_all("users"));
    assert_eq!(cmd("db.find.find()"), find_all("find"));
    assert_eq!(
        cmd("db.getCollection('x').find().limit(1)"),
        MongoCommand::Find {
            collection: "x".into(),
            filter: doc! {},
            projection: None,
            sort: None,
            skip: None,
            limit: Some(1),
        }
    );
}

#[test]
fn aggregate_count_distinct() {
    assert_eq!(
        cmd("db.users.aggregate([{$match: {a: 1}}, {$group: {_id: '$b', n: {$sum: 1}}}])"),
        MongoCommand::Aggregate {
            collection: "users".into(),
            pipeline: vec![
                doc! {"$match": {"a": 1}},
                doc! {"$group": {"_id": "$b", "n": {"$sum": 1}}}
            ],
        }
    );
    assert_eq!(
        cmd("db.users.aggregate([]).toArray()"),
        MongoCommand::Aggregate {
            collection: "users".into(),
            pipeline: vec![]
        }
    );
    assert_eq!(
        cmd("db.users.countDocuments({a: 1})"),
        MongoCommand::Count {
            collection: "users".into(),
            filter: doc! {"a": 1}
        }
    );
    assert_eq!(
        cmd("db.users.count()"),
        MongoCommand::Count {
            collection: "users".into(),
            filter: doc! {}
        }
    );
    assert_eq!(
        cmd("db.users.estimatedDocumentCount()"),
        MongoCommand::EstimatedCount {
            collection: "users".into()
        }
    );
    assert_eq!(
        cmd("db.users.distinct(\"city\", {active: true})"),
        MongoCommand::Distinct {
            collection: "users".into(),
            field: "city".into(),
            filter: doc! {"active": true}
        }
    );
    assert_eq!(
        cmd("db.users.distinct('city')"),
        MongoCommand::Distinct {
            collection: "users".into(),
            field: "city".into(),
            filter: doc! {}
        }
    );
    let e = cmd_err("db.users.aggregate([{$match: {}}, 5])");
    assert!(
        e.message.contains("pipeline stage #2 must be a document"),
        "{}",
        e.message
    );
}

#[test]
fn writes() {
    assert_eq!(
        cmd("db.users.insertOne({name: 'x'})"),
        MongoCommand::InsertOne {
            collection: "users".into(),
            document: doc! {"name": "x"}
        }
    );
    assert_eq!(
        cmd("db.users.insertMany([{a: 1}, {a: 2}])"),
        MongoCommand::InsertMany {
            collection: "users".into(),
            documents: vec![doc! {"a": 1}, doc! {"a": 2}]
        }
    );
    assert_eq!(
        cmd("db.users.insert({a: 1})"),
        MongoCommand::InsertOne {
            collection: "users".into(),
            document: doc! {"a": 1}
        }
    );
    assert_eq!(
        cmd("db.users.updateOne({_id: 1}, {$set: {a: 2}}, {upsert: true})"),
        MongoCommand::UpdateOne {
            collection: "users".into(),
            filter: doc! {"_id": 1},
            update: UpdateSpec::Document(doc! {"$set": {"a": 2}}),
            upsert: true,
        }
    );
    assert_eq!(
        cmd("db.users.updateMany({}, [{$set: {b: '$a'}}])"),
        MongoCommand::UpdateMany {
            collection: "users".into(),
            filter: doc! {},
            update: UpdateSpec::Pipeline(vec![doc! {"$set": {"b": "$a"}}]),
            upsert: false,
        }
    );
    assert_eq!(
        cmd("db.users.replaceOne({_id: 1}, {name: 'y'}, {upsert: false})"),
        MongoCommand::ReplaceOne {
            collection: "users".into(),
            filter: doc! {"_id": 1},
            replacement: doc! {"name": "y"},
            upsert: false,
        }
    );
    assert_eq!(
        cmd("db.users.deleteOne({_id: 1})"),
        MongoCommand::DeleteOne {
            collection: "users".into(),
            filter: doc! {"_id": 1}
        }
    );
    assert_eq!(
        cmd("db.users.deleteMany({})"),
        MongoCommand::DeleteMany {
            collection: "users".into(),
            filter: doc! {}
        }
    );

    let e = cmd_err("db.users.updateOne({}, {name: 'x'})");
    assert!(e.message.contains("only update operators"), "{}", e.message);
    assert_eq!(e.span, 23..34);
    let e = cmd_err("db.users.replaceOne({}, {$set: {a: 1}})");
    assert!(
        e.message.contains("must not contain update operators"),
        "{}",
        e.message
    );
    let e = cmd_err("db.users.updateOne({}, {$set: {a: 1}}, {multi: true})");
    assert!(
        e.message.contains("unsupported option `multi`"),
        "{}",
        e.message
    );
    let e = cmd_err("db.users.deleteMany()");
    assert!(
        e.message.contains("deleteMany() takes 1 argument, got 0"),
        "{}",
        e.message
    );
    assert_eq!(e.span, 9..21);
    let e = cmd_err("db.users.insertMany({a: 1})");
    assert!(
        e.message.contains("expects an array of documents"),
        "{}",
        e.message
    );
    let e = cmd_err("db.users.insertOne([{a: 1}])");
    assert!(
        e.message.contains("document must be a document"),
        "{}",
        e.message
    );
}

#[test]
fn indexes_and_admin() {
    assert_eq!(
        cmd("db.users.createIndex({a: 1}, {unique: true})"),
        MongoCommand::CreateIndex {
            collection: "users".into(),
            keys: doc! {"a": 1},
            options: Some(doc! {"unique": true}),
        }
    );
    assert_eq!(
        cmd("db.users.createIndex({a: -1})"),
        MongoCommand::CreateIndex {
            collection: "users".into(),
            keys: doc! {"a": -1},
            options: None
        }
    );
    assert_eq!(
        cmd("db.users.getIndexes()"),
        MongoCommand::ListIndexes {
            collection: "users".into()
        }
    );
    assert_eq!(
        cmd("db.users.dropIndex(\"a_1\")"),
        MongoCommand::DropIndex {
            collection: "users".into(),
            name: "a_1".into()
        }
    );
    assert_eq!(
        cmd("db.users.drop()"),
        MongoCommand::DropCollection {
            collection: "users".into()
        }
    );
    assert_eq!(
        cmd("db.createCollection(\"logs\")"),
        MongoCommand::CreateCollection {
            name: "logs".into()
        }
    );
    assert_eq!(
        cmd("db.runCommand({ping: 1})"),
        MongoCommand::RunCommand {
            command: doc! {"ping": 1},
            admin: false
        }
    );
    assert_eq!(
        cmd("db.adminCommand({listDatabases: 1})"),
        MongoCommand::RunCommand {
            command: doc! {"listDatabases": 1},
            admin: true
        }
    );
    assert_eq!(
        cmd("db.runCommand('ping')"),
        MongoCommand::RunCommand {
            command: doc! {"ping": 1},
            admin: false
        }
    );
    assert_eq!(
        cmd("db.getCollectionNames()"),
        MongoCommand::ShowCollections
    );
    assert_eq!(cmd("show collections"), MongoCommand::ShowCollections);
    assert_eq!(cmd("show tables;"), MongoCommand::ShowCollections);
    assert_eq!(cmd("show dbs"), MongoCommand::ShowDatabases);
    assert_eq!(cmd("  show   databases ; "), MongoCommand::ShowDatabases);
    let e = cmd_err("db.users.dropIndex({a: 1})");
    assert!(
        e.message.contains("index name must be a string"),
        "{}",
        e.message
    );
}

#[test]
fn command_errors() {
    let e = cmd_err("{age: {$gt: 30}}");
    assert!(
        e.message.contains("expected db.<collection>.<method>(…)"),
        "{}",
        e.message
    );
    assert_eq!(e.span, 0..16);

    let e = cmd_err("db.users.fnd({})");
    assert!(e.message.contains("unknown method `fnd`"), "{}", e.message);
    assert!(e.message.contains("find, findOne"), "{}", e.message);
    assert_eq!(e.span, 9..12);

    let e = cmd_err("db.users.find().batchSize(5)");
    assert!(
        e.message.contains("unsupported cursor method `batchSize`"),
        "{}",
        e.message
    );
    assert_eq!(e.span, 16..25);

    let e = cmd_err("db.users");
    assert!(
        e.message
            .contains("expected a method call after `db.users`"),
        "{}",
        e.message
    );

    let e = cmd_err("db.users.find");
    assert!(
        e.message.contains("expected a method call"),
        "{}",
        e.message
    );

    let e = cmd_err("db.dropDatabase()");
    assert!(
        e.message.contains("unknown database method `dropDatabase`"),
        "{}",
        e.message
    );
    assert_eq!(e.span, 3..15);

    let e = cmd_err("db.users.find({a: 1}");
    assert!(
        e.message.contains("unterminated call to find"),
        "{}",
        e.message
    );

    let e = cmd_err("db.users.find({a: })");
    assert!(e.message.contains("unexpected `}`"), "{}", e.message);
    assert_eq!(e.span, 18..19);

    let e = cmd_err("db.users.find(); db.users.find()");
    assert!(e.message.contains("only one statement"), "{}", e.message);
    assert_eq!(e.span, 17..32);

    let e = cmd_err("show users");
    assert!(e.message.contains("unknown `show users`"), "{}", e.message);
    assert_eq!(e.span, 5..10);

    let e = cmd_err("use test");
    assert!(e.message.contains("not supported"), "{}", e.message);

    let e = cmd_err("foo.bar()");
    assert!(e.message.contains("unknown command `foo`"), "{}", e.message);
    assert_eq!(e.span, 0..3);

    let e = cmd_err("   ");
    assert!(e.message.contains("empty command"), "{}", e.message);

    let e = cmd_err("db.users.find().skip(-1)");
    assert!(e.message.contains("must not be negative"), "{}", e.message);

    let e = cmd_err("db.users.find().limit('a')");
    assert!(e.message.contains("expects an integer"), "{}", e.message);

    let e = cmd_err("db.users.find({}, {}, {}, {})");
    assert!(
        e.message.contains("at most 3 arguments, got 4"),
        "{}",
        e.message
    );
    assert_eq!(e.span, 26..28);

    let e = cmd_err("db.users.insertOne({}).limit(1)");
    assert!(
        e.message.contains("cannot be chained after insertOne()"),
        "{}",
        e.message
    );
}

#[test]
fn error_line_col_and_display() {
    let input = "db.users.find({\n  a: foo\n})";
    let e = parse_command(input).unwrap_err();
    assert_eq!(e.line_col(input), (2, 6));
    assert_eq!(e.to_string(), e.message);
    let _: &dyn std::error::Error = &e;
}

#[test]
fn filter_or_command() {
    let find = |filter: Document| MongoCommand::Find {
        collection: "users".into(),
        filter,
        projection: None,
        sort: None,
        skip: None,
        limit: None,
    };
    assert_eq!(
        parse_filter_or_command("{age: {$gt: 30}}", Some("users")).unwrap(),
        find(doc! {"age": {"$gt": 30}})
    );
    assert_eq!(
        parse_filter_or_command("  ", Some("users")).unwrap(),
        find(doc! {})
    );
    assert_eq!(
        parse_filter_or_command("// c\n{a: 1};", Some("users")).unwrap(),
        find(doc! {"a": 1})
    );
    assert_eq!(
        parse_filter_or_command("[{$match: {a: 1}}]", Some("users")).unwrap(),
        MongoCommand::Aggregate {
            collection: "users".into(),
            pipeline: vec![doc! {"$match": {"a": 1}}]
        }
    );
    assert_eq!(
        parse_filter_or_command("db.other.drop()", Some("users")).unwrap(),
        MongoCommand::DropCollection {
            collection: "other".into()
        }
    );
    assert_eq!(
        parse_filter_or_command("show dbs", None).unwrap(),
        MongoCommand::ShowDatabases
    );
    let e = parse_filter_or_command("{a: 1}", None).unwrap_err();
    assert!(
        e.message.contains("no collection selected"),
        "{}",
        e.message
    );
    let e = parse_filter_or_command("{a: 1", Some("users")).unwrap_err();
    assert!(e.message.contains("unterminated object"), "{}", e.message);
    let e = parse_filter_or_command("[1]", Some("users")).unwrap_err();
    assert!(e.message.contains("pipeline stage #1"), "{}", e.message);
    let e = parse_filter_or_command(&format!("{{$oid: '{HEX}'}}"), Some("users")).unwrap_err();
    assert!(
        e.message.contains("expected a filter document"),
        "{}",
        e.message
    );
}

#[test]
fn is_write_and_collection() {
    assert!(!cmd("db.a.find()").is_write());
    assert!(!cmd("db.a.findOne()").is_write());
    assert!(!cmd("db.a.aggregate([{$match: {}}])").is_write());
    assert!(cmd("db.a.aggregate([{$match: {}}, {$out: 'b'}])").is_write());
    assert!(cmd("db.a.aggregate([{$merge: {into: 'b'}}])").is_write());
    assert!(!cmd("db.a.count()").is_write());
    assert!(!cmd("db.a.distinct('x')").is_write());
    assert!(!cmd("db.a.getIndexes()").is_write());
    assert!(!cmd("show dbs").is_write());
    assert!(cmd("db.a.insertOne({})").is_write());
    assert!(cmd("db.a.updateMany({}, {$set: {a: 1}})").is_write());
    assert!(cmd("db.a.deleteOne({})").is_write());
    assert!(cmd("db.a.drop()").is_write());
    assert!(cmd("db.a.createIndex({a: 1})").is_write());
    assert!(cmd("db.createCollection('x')").is_write());
    assert!(cmd("db.runCommand({ping: 1})").is_write());

    assert_eq!(cmd("db.a.find()").collection(), Some("a"));
    assert_eq!(cmd("db['a b'].drop()").collection(), Some("a b"));
    assert_eq!(cmd("db.createCollection('x')").collection(), Some("x"));
    assert_eq!(cmd("db.runCommand({ping: 1})").collection(), None);
    assert_eq!(cmd("show collections").collection(), None);
}

// ---------------------------------------------------------------------------
// Formatting
// ---------------------------------------------------------------------------

fn all_types_doc() -> Document {
    let mut d = Document::new();
    d.insert("_id", oid(HEX));
    d.insert("int32", 5);
    d.insert("int32neg", i32::MIN);
    d.insert("int64", 5i64);
    d.insert("int64big", i64::MAX);
    d.insert("int64min", i64::MIN);
    d.insert("double", 1.5);
    d.insert("doubleIntegral", 5.0);
    d.insert("doubleBig", 1e300);
    d.insert("doubleTiny", -1e-300);
    d.insert("negZero", -0.0);
    d.insert("inf", f64::INFINITY);
    d.insert("ninf", f64::NEG_INFINITY);
    d.insert("dec", Decimal128::from_str("1.50").unwrap());
    d.insert("decNeg", Decimal128::from_str("-1E+10").unwrap());
    d.insert("decInf", Decimal128::from_str("Infinity").unwrap());
    d.insert(
        "str",
        "she said \"hi\"\nand left\t\\ 'quoted' 世界 😀 \u{1} \u{2028}",
    );
    d.insert("emptyStr", "");
    d.insert("date", DateTime::from_millis(1_704_103_200_123));
    d.insert("datePre1970", DateTime::from_millis(-86_400_001));
    d.insert("dateFar", DateTime::from_millis(327_403_382_400_000));
    d.insert("dateMax", DateTime::MAX);
    d.insert(
        "bin",
        Binary {
            subtype: BinarySubtype::Generic,
            bytes: vec![0, 1, 2, 254, 255],
        },
    );
    d.insert(
        "binEmpty",
        Binary {
            subtype: BinarySubtype::UserDefined(0x80),
            bytes: vec![],
        },
    );
    d.insert(
        "uuid",
        Binary {
            subtype: BinarySubtype::Uuid,
            bytes: (0u8..16).collect(),
        },
    );
    d.insert(
        "uuidOdd",
        Binary {
            subtype: BinarySubtype::Uuid,
            bytes: vec![1, 2, 3],
        },
    );
    d.insert(
        "regex",
        Regex {
            pattern: "^a/b\\d+$".into(),
            options: "im".into(),
        },
    );
    d.insert(
        "regexEscSlash",
        Regex {
            pattern: "a\\/b".into(),
            options: "".into(),
        },
    );
    d.insert(
        "regexEmpty",
        Regex {
            pattern: "".into(),
            options: "".into(),
        },
    );
    d.insert(
        "regexNewline",
        Regex {
            pattern: "a\nb".into(),
            options: "s".into(),
        },
    );
    d.insert(
        "regexClass",
        Regex {
            pattern: "[".into(),
            options: "".into(),
        },
    );
    d.insert(
        "ts",
        Timestamp {
            time: 1_700_000_000,
            increment: 7,
        },
    );
    d.insert(
        "nested",
        doc! { "a": { "b": [1, { "c": null }] }, "empty": {}, "arr": [] },
    );
    d.insert("array", bson::bson!([1, "two", 3.0, [4], { "five": 5 }]));
    d.insert("null", Bson::Null);
    d.insert("t", true);
    d.insert("f", false);
    d.insert("min", Bson::MinKey);
    d.insert("max", Bson::MaxKey);
    d.insert(
        "code",
        Bson::JavaScriptCode("function() { return \"x\"; }".into()),
    );
    d.insert(
        "codeScope",
        Bson::JavaScriptCodeWithScope(bson::JavaScriptCodeWithScope {
            code: "x + y".into(),
            scope: doc! {"y": 1},
        }),
    );
    d.insert("symbol", Bson::Symbol("sym".into()));
    d.insert("weird key", 1);
    d.insert("dotted.key", 2);
    d.insert("$op", 3);
    d.insert("_under", 4);
    d.insert("1digit", 5);
    d.insert("", 6);
    d.insert("true", 7);
    d.insert("ключ", 8);
    d
}

/// Bitwise-aware comparison (NaN == NaN, -0.0 != 0.0).
fn assert_same(a: &Bson, b: &Bson, path: &str) {
    match (a, b) {
        (Bson::Double(x), Bson::Double(y)) => {
            assert!(
                x.to_bits() == y.to_bits() || (x.is_nan() && y.is_nan()),
                "{path}: {x:?} != {y:?}"
            )
        }
        (Bson::Document(x), Bson::Document(y)) => {
            assert_eq!(
                x.keys().collect::<Vec<_>>(),
                y.keys().collect::<Vec<_>>(),
                "{path}: keys differ"
            );
            for (k, xv) in x {
                assert_same(xv, y.get(k).unwrap(), &format!("{path}.{k}"));
            }
        }
        (Bson::Array(x), Bson::Array(y)) => {
            assert_eq!(x.len(), y.len(), "{path}: len differs");
            for (i, (xv, yv)) in x.iter().zip(y).enumerate() {
                assert_same(xv, yv, &format!("{path}[{i}]"));
            }
        }
        _ => assert_eq!(a, b, "{path}"),
    }
}

#[test]
fn round_trip_every_type() {
    let d = all_types_doc();
    for pretty in [false, true] {
        let text = format_document(&d, pretty);
        let back =
            parse_document(&text).unwrap_or_else(|e| panic!("{e} at {:?} in:\n{text}", e.span));
        assert_same(&Bson::Document(d.clone()), &Bson::Document(back), "doc");
        // each value on its own as well
        for (k, val) in &d {
            let t = format_literal(val, pretty);
            let back = parse_value(&t).unwrap_or_else(|e| panic!("{k}: {e} in {t}"));
            assert_same(val, &back, k);
        }
    }
    // NaN separately (doc! with NaN would make the equality-based check above awkward)
    let t = format_literal(&Bson::Double(f64::NAN), false);
    assert_eq!(t, "NaN");
    assert!(matches!(parse_value(&t).unwrap(), Bson::Double(f) if f.is_nan()));
}

#[test]
#[ignore = "prints the formatted sample document; run with --ignored --nocapture"]
fn show_pretty_sample() {
    println!("{}", format_document(&all_types_doc(), true));
}

#[test]
fn round_trip_dbpointer() {
    let p = parse_value(&format!(
        r#"{{"$dbPointer": {{"$ref": "db.c", "$id": {{"$oid": "{HEX}"}}}}}}"#
    ))
    .unwrap();
    let t = format_literal(&p, false);
    assert_eq!(parse_value(&t).unwrap(), p, "{t}");
}

#[test]
fn literal_formats() {
    let f = |b: Bson| format_literal(&b, false);
    assert_eq!(f(Bson::Int32(5)), "5");
    assert_eq!(f(Bson::Int64(5)), "NumberLong(5)");
    assert_eq!(
        f(Bson::Int64(i64::MAX)),
        "NumberLong(\"9223372036854775807\")"
    );
    assert_eq!(f(Bson::Double(5.0)), "5.0");
    assert_eq!(f(Bson::Double(1.5)), "1.5");
    assert_eq!(f(Bson::Double(1e300)), "1e300");
    assert_eq!(f(Bson::Double(f64::INFINITY)), "Infinity");
    assert_eq!(f(Bson::Double(f64::NEG_INFINITY)), "-Infinity");
    assert_eq!(f(dec("1.50")), "NumberDecimal(\"1.50\")");
    assert_eq!(f(Bson::String("a\"b\n".into())), r#""a\"b\n""#);
    assert_eq!(f(Bson::ObjectId(oid(HEX))), format!("ObjectId(\"{HEX}\")"));
    assert_eq!(
        f(Bson::DateTime(DateTime::from_millis(1_704_103_200_000))),
        "ISODate(\"2024-01-01T10:00:00.000Z\")"
    );
    assert_eq!(
        f(Bson::DateTime(DateTime::from_millis(-1))),
        "ISODate(\"1969-12-31T23:59:59.999Z\")"
    );
    assert_eq!(
        f(Bson::DateTime(DateTime::MAX)),
        format!("new Date({})", i64::MAX)
    );
    assert_eq!(
        f(Bson::Binary(Binary {
            subtype: BinarySubtype::Generic,
            bytes: b"hello".to_vec()
        })),
        "BinData(0, \"aGVsbG8=\")"
    );
    assert_eq!(
        f(v("UUID('123e4567-e89b-12d3-a456-426614174000')")),
        "UUID(\"123e4567-e89b-12d3-a456-426614174000\")"
    );
    assert_eq!(f(regex("a/b", "i")), "/a\\/b/i");
    assert_eq!(f(regex("", "")), "RegExp(\"\")");
    assert_eq!(f(regex("a\\/b", "")), "RegExp(\"a\\\\/b\")");
    assert_eq!(
        f(Bson::Timestamp(Timestamp {
            time: 1,
            increment: 2
        })),
        "Timestamp({t: 1, i: 2})"
    );
    assert_eq!(f(Bson::MinKey), "MinKey()");
    assert_eq!(f(Bson::MaxKey), "MaxKey()");
    assert_eq!(f(Bson::Null), "null");
    assert_eq!(f(Bson::Undefined), "undefined");
    assert_eq!(f(Bson::Boolean(true)), "true");
    assert_eq!(
        format_document(
            &doc! {"name": "x", "tags": ["a", "b"], "a b": 1, "$set": {}},
            false
        ),
        r#"{name: "x", tags: ["a", "b"], "a b": 1, $set: {}}"#
    );
}

#[test]
fn pretty_format_layout() {
    let d = doc! {
        "_id": oid(HEX),
        "name": "Ada",
        "tags": ["a", "b"],
        "address": { "city": "London", "zip": "N1" },
        "orders": [ { "id": 1 }, { "id": 2 } ],
        "deep": { "a": { "b": [1, { "c": null }] } },
        "empty": {},
    };
    let expected = format!(
        r#"{{
  _id: ObjectId("{HEX}"),
  name: "Ada",
  tags: ["a", "b"],
  address: {{city: "London", zip: "N1"}},
  orders: [{{id: 1}}, {{id: 2}}],
  deep: {{
    a: {{
      b: [1, {{c: null}}]
    }}
  }},
  empty: {{}}
}}"#
    );
    assert_eq!(format_document(&d, true), expected);
    assert_eq!(format_document(&Document::new(), true), "{}");

    // Long scalar arrays are broken over multiple lines.
    let long: Vec<String> = (0..30).map(|i| format!("item-{i}")).collect();
    let text = format_document(&doc! {"list": long.clone()}, true);
    assert!(text.contains("list: [\n    \"item-0\",\n"), "{text}");
    assert_eq!(parse_document(&text).unwrap(), doc! {"list": long});
}

#[test]
fn cell_formats() {
    assert_eq!(
        format_cell(&Bson::String("plain text".into())),
        "plain text"
    );
    assert_eq!(
        format_cell(&Bson::ObjectId(oid(HEX))),
        format!("ObjectId(\"{HEX}\")")
    );
    assert_eq!(
        format_cell(&Bson::DateTime(DateTime::from_millis(1_704_103_200_000))),
        "2024-01-01T10:00:00.000Z"
    );
    assert_eq!(format_cell(&Bson::Int32(42)), "42");
    assert_eq!(format_cell(&Bson::Int64(42)), "42");
    assert_eq!(format_cell(&Bson::Double(3.0)), "3.0");
    assert_eq!(format_cell(&Bson::Double(2.5)), "2.5");
    assert_eq!(format_cell(&dec("1.50")), "1.50");
    assert_eq!(format_cell(&Bson::Boolean(false)), "false");
    assert_eq!(format_cell(&Bson::Null), "null");
    assert_eq!(
        format_cell(&Bson::Document(doc! {"name": "x", "tags": ["a", "b"]})),
        r#"{name: "x", tags: ["a", "b"]}"#
    );
    assert_eq!(
        format_cell(&Bson::Array(vec![Bson::Int32(1), Bson::String("a".into())])),
        r#"[1, "a"]"#
    );
    assert_eq!(format_cell(&regex("^a", "i")), "/^a/i");

    let big: Vec<i32> = (0..500).collect();
    let cell = format_cell(&Bson::Array(big.into_iter().map(Bson::Int32).collect()));
    assert_eq!(cell.chars().count(), 300);
    assert!(cell.ends_with('…'));
    assert!(cell.starts_with("[0, 1, 2"));

    // Truncation never splits a multi-byte char.
    let wide = Bson::Document(doc! {"k": "世".repeat(400)});
    let cell = format_cell(&wide);
    assert_eq!(cell.chars().count(), 300);
}

// ---------------------------------------------------------------------------
// Cell editing
// ---------------------------------------------------------------------------

#[test]
fn cell_edit_strings() {
    let s = Bson::String("old".into());
    assert_eq!(
        parse_cell_edit("new value", &s).unwrap(),
        Bson::String("new value".into())
    );
    assert_eq!(
        parse_cell_edit("\"quoted\"", &s).unwrap(),
        Bson::String("\"quoted\"".into())
    );
    assert_eq!(
        parse_cell_edit("42", &s).unwrap(),
        Bson::String("42".into())
    );
    assert_eq!(
        parse_cell_edit("", &s).unwrap(),
        Bson::String(String::new())
    );
    assert_eq!(parse_cell_edit("null", &s).unwrap(), Bson::Null);
    assert_eq!(
        parse_cell_edit(" null", &s).unwrap(),
        Bson::String(" null".into())
    );
}

#[test]
fn cell_edit_numbers() {
    let i32v = Bson::Int32(1);
    assert_eq!(parse_cell_edit("5", &i32v).unwrap(), Bson::Int32(5));
    assert_eq!(parse_cell_edit(" -5 ", &i32v).unwrap(), Bson::Int32(-5));
    assert_eq!(
        parse_cell_edit("3000000000", &i32v).unwrap(),
        Bson::Int64(3_000_000_000)
    );
    assert_eq!(parse_cell_edit("2.5", &i32v).unwrap(), Bson::Double(2.5));
    assert_eq!(parse_cell_edit("null", &i32v).unwrap(), Bson::Null);
    assert_eq!(
        parse_cell_edit("abc", &i32v).unwrap(),
        Bson::String("abc".into())
    );
    assert_eq!(
        parse_cell_edit("NumberLong(5)", &i32v).unwrap(),
        Bson::Int64(5)
    );

    let i64v = Bson::Int64(1);
    assert_eq!(parse_cell_edit("5", &i64v).unwrap(), Bson::Int64(5));
    assert_eq!(
        parse_cell_edit("NumberLong(7)", &i64v).unwrap(),
        Bson::Int64(7)
    );

    let dv = Bson::Double(1.0);
    assert_eq!(parse_cell_edit("5", &dv).unwrap(), Bson::Double(5.0));
    assert_eq!(parse_cell_edit("5.25", &dv).unwrap(), Bson::Double(5.25));
    assert_eq!(
        parse_cell_edit("-Infinity", &dv).unwrap(),
        Bson::Double(f64::NEG_INFINITY)
    );
    assert_eq!(
        parse_cell_edit("9999999999", &dv).unwrap(),
        Bson::Double(9_999_999_999.0)
    );

    let decv = dec("1");
    assert_eq!(parse_cell_edit("1.10", &decv).unwrap(), dec("1.10"));
    assert_eq!(parse_cell_edit("7", &decv).unwrap(), dec("7"));
    assert_eq!(
        parse_cell_edit("NumberDecimal(\"2.5\")", &decv).unwrap(),
        dec("2.5")
    );

    let e = parse_cell_edit("{a: ", &i32v).unwrap_err();
    assert!(
        e.message.contains("expected a value, found end of input"),
        "{}",
        e.message
    );
}

#[test]
fn cell_edit_other_types() {
    let n = Bson::Null;
    assert_eq!(parse_cell_edit("5", &n).unwrap(), Bson::Int32(5));
    assert_eq!(parse_cell_edit("true", &n).unwrap(), Bson::Boolean(true));
    assert_eq!(
        parse_cell_edit("hello world", &n).unwrap(),
        Bson::String("hello world".into())
    );
    assert_eq!(
        parse_cell_edit("5 apples", &n).unwrap(),
        Bson::String("5 apples".into())
    );
    assert_eq!(
        parse_cell_edit("'quoted'", &n).unwrap(),
        Bson::String("quoted".into())
    );
    assert_eq!(
        parse_cell_edit("{a: 1}", &n).unwrap(),
        Bson::Document(doc! {"a": 1})
    );
    assert!(parse_cell_edit("{a: ", &n).is_err());
    assert!(parse_cell_edit("\"unterminated", &n).is_err());

    let o = Bson::ObjectId(oid(HEX));
    assert_eq!(
        parse_cell_edit("ObjectId(\"507f191e810c19729de860ea\")", &o).unwrap(),
        Bson::ObjectId(oid("507f191e810c19729de860ea"))
    );
    let e = parse_cell_edit("ObjectId(\"zz\")", &o).unwrap_err();
    assert!(e.message.contains("invalid ObjectId"), "{}", e.message);
    let e = parse_cell_edit("new ISODate(\"bad\")", &o).unwrap_err();
    assert!(e.message.contains("invalid date"), "{}", e.message);

    let dt = Bson::DateTime(DateTime::from_millis(0));
    assert_eq!(
        parse_cell_edit("ISODate(\"2024-01-01\")", &dt).unwrap(),
        date("2024-01-01T00:00:00Z")
    );
    let b = Bson::Boolean(true);
    assert_eq!(parse_cell_edit("false", &b).unwrap(), Bson::Boolean(false));
    let arr = Bson::Array(vec![]);
    assert_eq!(
        parse_cell_edit("[1, 2]", &arr).unwrap(),
        Bson::Array(vec![Bson::Int32(1), Bson::Int32(2)])
    );
}

#[test]
fn format_cell_edit_round_trip() {
    // What the grid shows for non-string scalars can be fed back unchanged.
    for val in [
        Bson::Int32(5),
        Bson::Int64(1 << 40),
        Bson::Double(3.0),
        Bson::Boolean(true),
        Bson::Null,
        Bson::ObjectId(oid(HEX)),
        dec("1.25"),
    ] {
        let shown = format_cell(&val);
        assert_eq!(parse_cell_edit(&shown, &val).unwrap(), val, "{shown}");
    }
}
