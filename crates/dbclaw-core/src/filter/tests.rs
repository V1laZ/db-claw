use super::*;
use bson::{Bson, doc};

// ---- helpers ----------------------------------------------------------------

fn p(input: &str) -> Expr {
    parse(input)
        .unwrap_or_else(|e| panic!("parse {input:?} failed: {e} at {:?}", e.span))
        .expect("non-empty")
}

/// Canonical form via Display.
fn canon(input: &str) -> String {
    p(input).to_string()
}

fn perr(input: &str) -> FilterError {
    match parse(input) {
        Ok(e) => panic!("expected error for {input:?}, got {e:?}"),
        Err(e) => e,
    }
}

/// Asserts an error whose message contains `needle` and whose span covers `at`.
fn assert_err(input: &str, needle: &str, at: &str) {
    let e = perr(input);
    assert!(
        e.message.contains(needle),
        "{input:?}: message {:?} doesn't contain {needle:?}",
        e.message
    );
    let start = input
        .find(at)
        .unwrap_or_else(|| panic!("{at:?} not in {input:?}"));
    assert_eq!(
        e.span,
        start..start + at.len(),
        "{input:?}: span of {:?}",
        e.message
    );
}

fn cols() -> Vec<SqlColumn> {
    vec![
        SqlColumn::new("id", "integer"),
        SqlColumn::new("name", "text"),
        SqlColumn::new("email", "character varying"),
        SqlColumn::new("age", "integer"),
        SqlColumn::new("status", "USER-DEFINED"),
        SqlColumn::new("active", "boolean"),
        SqlColumn::new("created", "timestamp with time zone"),
        SqlColumn::new("deleted_at", "timestamp without time zone"),
        SqlColumn::new("data", "jsonb"),
        SqlColumn::new("meta", "json"),
        SqlColumn::new("tags", "ARRAY"),
        SqlColumn::new("scores", "_int4"),
        SqlColumn::new("UserName", "text"),
        SqlColumn::new("weird \"col\"", "text"),
    ]
}

fn sql(input: &str) -> String {
    let e = p(input);
    to_sql(&e, &cols()).unwrap_or_else(|err| panic!("to_sql {input:?}: {err} {:?}", err.span))
}

fn sql_nocols(input: &str) -> String {
    to_sql(&p(input), &[]).unwrap()
}

fn sql_err(input: &str) -> FilterError {
    match to_sql(&p(input), &cols()) {
        Ok(s) => panic!("expected SQL error for {input:?}, got {s}"),
        Err(e) => e,
    }
}

fn mongo(input: &str) -> bson::Document {
    to_mongo(&p(input)).unwrap_or_else(|e| panic!("to_mongo {input:?}: {e}"))
}

fn re(pattern: &str, options: &str) -> Bson {
    Bson::RegularExpression(bson::Regex {
        pattern: pattern.into(),
        options: options.into(),
    })
}

// ---- parsing: basics ----------------------------------------------------------

#[test]
fn empty_filter_is_none() {
    assert_eq!(parse("").unwrap(), None);
    assert_eq!(parse("   \t\n ").unwrap(), None);
    assert_eq!(compile_sql("  ", &cols()).unwrap(), None);
    assert_eq!(compile_mongo("").unwrap(), doc! {});
}

#[test]
fn parses_simple_compare_with_spans() {
    let e = p("age >= 18");
    assert_eq!(
        e,
        Expr::Compare {
            field: Field {
                path: FieldPath(vec!["age".into()]),
                span: 0..3
            },
            op: CompareOp::Ge,
            value: Literal::Int(18),
            span: 0..9,
        }
    );
}

#[test]
fn every_operator_parses() {
    let cases = [
        ("a = 1", "a = 1"),
        ("a == 1", "a = 1"),
        ("a != 1", "a != 1"),
        ("a <> 1", "a != 1"),
        ("a < 1", "a < 1"),
        ("a <= 1", "a <= 1"),
        ("a > 1", "a > 1"),
        ("a >= 1", "a >= 1"),
        ("a ~ \"x\"", "a ~ \"x\""),
        ("a !~ \"x\"", "a !~ \"x\""),
        ("a ^= \"x\"", "a ^= \"x\""),
        ("a $= \"x\"", "a $= \"x\""),
        ("a like \"x%\"", "a like \"x%\""),
        ("a ILIKE \"x%\"", "a ilike \"x%\""),
        ("a matches \"^x\"", "a matches \"^x\""),
        ("a = /^x/i", "a matches /^x/i"),
        ("a ~ /^x/", "a matches /^x/"),
        ("a != /^x/", "not a matches /^x/"),
        ("a in [1, \"b\", true, null]", "a in [1, \"b\", true, null]"),
        ("a in (1, 2)", "a in [1, 2]"),
        ("a in []", "a in []"),
        ("a in [1, 2,]", "a in [1, 2]"),
        ("a not in [1]", "a not in [1]"),
        ("a is null", "a is null"),
        ("a IS NOT NULL", "a is not null"),
        ("a = null", "a is null"),
        ("a != NULL", "a is not null"),
        ("a between 1 and 5", "a between 1 and 5"),
        ("a not between 1 and 5", "not a between 1 and 5"),
        ("a exists", "a exists"),
        ("a not exists", "a not exists"),
        ("a not like \"x\"", "not a like \"x\""),
        ("a not ilike \"x\"", "not a ilike \"x\""),
        ("a not matches \"x\"", "not a matches \"x\""),
    ];
    for (input, expected) in cases {
        assert_eq!(canon(input), expected, "input {input:?}");
    }
}

#[test]
fn keywords_are_case_insensitive() {
    assert_eq!(
        canon("A = TRUE AnD b = False OR NOT c Is NuLl"),
        "(A = true and b = false) or not c is null"
    );
    assert_eq!(canon("x BETWEEN 1 AND 2"), "x between 1 and 2");
    assert_eq!(canon("x NOT IN [1]"), "x not in [1]");
    assert_eq!(canon("x Exists"), "x exists");
}

#[test]
fn symbolic_boolean_operators() {
    assert_eq!(
        canon("a = 1 && b = 2 || !c = 3"),
        "(a = 1 and b = 2) or not c = 3"
    );
    assert_eq!(canon("!!a = 1"), "not not a = 1");
    assert_eq!(
        canon("a=1&&!(b=2)||!c=3"),
        "(a = 1 and not b = 2) or not c = 3"
    );
    assert_eq!(canon("a!=1&&b>=-2"), "a != 1 and b >= -2");
}

#[test]
fn precedence_not_and_or() {
    // and binds tighter than or
    assert_eq!(
        canon("a = 1 or b = 2 and c = 3"),
        "a = 1 or (b = 2 and c = 3)"
    );
    match p("a = 1 or b = 2 and c = 3") {
        Expr::Or(items) => {
            assert_eq!(items.len(), 2);
            assert!(matches!(items[1], Expr::And(_)));
        }
        other => panic!("{other:?}"),
    }
    // not binds tighter than and
    match p("not a = 1 and b = 2") {
        Expr::And(items) => assert!(matches!(items[0], Expr::Not(_))),
        other => panic!("{other:?}"),
    }
    // parentheses override
    assert_eq!(
        canon("(a = 1 or b = 2) and c = 3"),
        "(a = 1 or b = 2) and c = 3"
    );
    assert_eq!(canon("not (a = 1 or b = 2)"), "not (a = 1 or b = 2)");
    assert_eq!(canon("((a = 1))"), "a = 1");
}

#[test]
fn and_or_are_flattened() {
    match p("a = 1 and b = 2 and (c = 3 and d = 4)") {
        Expr::And(items) => assert_eq!(items.len(), 4),
        other => panic!("{other:?}"),
    }
    match p("a = 1 or (b = 2 or c = 3)") {
        Expr::Or(items) => assert_eq!(items.len(), 3),
        other => panic!("{other:?}"),
    }
}

#[test]
fn between_and_does_not_confuse_boolean_and() {
    match p("age between 18 and 65 and active = true") {
        Expr::And(items) => {
            assert_eq!(items.len(), 2);
            assert!(matches!(items[0], Expr::Between { .. }));
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn field_paths() {
    let e = p("address.city = \"Prague\"");
    let Expr::Compare { field, .. } = e else {
        panic!()
    };
    assert_eq!(field.path, FieldPath(vec!["address".into(), "city".into()]));
    assert_eq!(field.span, 0..12);

    let e = p("`weird column`.`a``b`.c = 1");
    let Expr::Compare { field, .. } = e else {
        panic!()
    };
    assert_eq!(
        field.path.0,
        vec!["weird column".to_string(), "a`b".into(), "c".into()]
    );

    // numeric segments and keywords after a dot
    let e = p("items.0.in = 1");
    let Expr::Compare { field, .. } = e else {
        panic!()
    };
    assert_eq!(field.path.0, vec!["items", "0", "in"]);

    // backticks make keywords usable as field names
    assert_eq!(canon("`and` = 1"), "`and` = 1");
    assert_eq!(canon("`weird column` = 1"), "`weird column` = 1");
    // unicode identifiers
    assert_eq!(canon("město = \"Brno\""), "město = \"Brno\"");
}

#[test]
fn string_literals_and_escapes() {
    let lit = |input: &str| match p(input) {
        Expr::Compare { value, .. } => value,
        other => panic!("{other:?}"),
    };
    assert_eq!(lit(r#"a = "x\"y""#), Literal::String("x\"y".into()));
    assert_eq!(lit(r#"a = 'it\'s'"#), Literal::String("it's".into()));
    assert_eq!(
        lit(r#"a = 'say "hi"'"#),
        Literal::String("say \"hi\"".into())
    );
    assert_eq!(lit(r#"a = "a\\b\n\t""#), Literal::String("a\\b\n\t".into()));
    assert_eq!(lit(r#"a = "\u00e9\u010D""#), Literal::String("éč".into()));
    assert_eq!(lit(r#"a = "\ud83d\ude00""#), Literal::String("😀".into()));
    assert_eq!(lit(r#"a = """#), Literal::String(String::new()));
    assert_eq!(
        lit("a = \"žluťoučký kůň\""),
        Literal::String("žluťoučký kůň".into())
    );
}

#[test]
fn number_literals() {
    let lit = |input: &str| match p(input) {
        Expr::Compare { value, .. } => value,
        other => panic!("{other:?}"),
    };
    assert_eq!(lit("a = 42"), Literal::Int(42));
    assert_eq!(lit("a = -42"), Literal::Int(-42));
    assert_eq!(lit("a>-1"), Literal::Int(-1));
    assert_eq!(lit("a = 3.25"), Literal::Float(3.25));
    assert_eq!(lit("a = -0.5"), Literal::Float(-0.5));
    assert_eq!(lit("a = 1e3"), Literal::Float(1000.0));
    assert_eq!(lit("a = 2.5E-1"), Literal::Float(0.25));
    assert_eq!(lit("a = 9223372036854775807"), Literal::Int(i64::MAX));
    assert_eq!(lit("a = -9223372036854775808"), Literal::Int(i64::MIN));
    assert_eq!(lit("a = 1_000"), Literal::Int(1000));
}

#[test]
fn typed_literals() {
    let lit = |input: &str| match p(input) {
        Expr::Compare { value, .. } => value,
        other => panic!("{other:?}"),
    };
    assert_eq!(
        lit("c > date(\"2024-01-01\")"),
        Literal::Date("2024-01-01".into())
    );
    assert_eq!(
        lit("c > DATE('2024-01-01T10:00:00Z')"),
        Literal::Date("2024-01-01T10:00:00Z".into())
    );
    assert_eq!(
        lit("_id = oid(\"65A1F0C2E4B0A1B2C3D4E5F6\")"),
        Literal::ObjectId("65a1f0c2e4b0a1b2c3d4e5f6".into())
    );
    assert_eq!(
        lit("_id = ObjectId(\"65a1f0c2e4b0a1b2c3d4e5f6\")"),
        Literal::ObjectId("65a1f0c2e4b0a1b2c3d4e5f6".into())
    );
    assert_eq!(
        lit("a = /a\\/b\\d/smi"),
        Literal::Regex {
            pattern: "a/b\\d".into(),
            flags: "ims".into()
        }
    );
}

#[test]
fn display_round_trips() {
    let inputs = [
        "a = 1 and (b = 2 or not c ~ \"x\\\"y\")",
        "`we ird`.x in [1, 2.5, \"s\", date(\"2024-01-01\"), oid(\"65a1f0c2e4b0a1b2c3d4e5f6\")]",
        "n matches /a\\/b/i or x between -1 and 1e20",
        "not (a is null or b not exists) and c exists",
        "s = \"tab\\there\\nnew \\u0001\"",
    ];
    for input in inputs {
        let e = p(input);
        let again = p(&e.to_string());
        assert_eq!(e.to_string(), again.to_string(), "{input}");
    }
}

#[test]
fn date_normalization() {
    assert_eq!(
        normalize_date("2024-01-01").as_deref(),
        Some("2024-01-01T00:00:00Z")
    );
    assert_eq!(
        normalize_date("2024-01-01 10:30").as_deref(),
        Some("2024-01-01T10:30:00Z")
    );
    assert_eq!(
        normalize_date("2024-01-01T10:30:00+02:00").as_deref(),
        Some("2024-01-01T10:30:00+02:00")
    );
    assert_eq!(
        normalize_date("2024-01-01T10:30:00.123Z").as_deref(),
        Some("2024-01-01T10:30:00.123Z")
    );
    assert_eq!(normalize_date("2024-13-01"), None);
    assert_eq!(normalize_date("yesterday"), None);
    assert_eq!(normalize_date("2024-01-01X10"), None);
}

// ---- parsing: errors ------------------------------------------------------------

#[test]
fn error_bare_word_value_hint() {
    assert_err("status = active", "did you mean \"active\"?", "active");
}

#[test]
fn error_missing_value() {
    let e = perr("age >");
    assert_eq!(e.message, "expected a value after '>'");
    assert_eq!(e.span, 5..5);
    assert_err(
        "age > and b = 1",
        "expected a value after '>', found 'and'",
        "and",
    );
}

#[test]
fn error_unclosed_string() {
    let e = perr("name = \"abc");
    assert_eq!(e.message, "unclosed string");
    assert_eq!(e.span, 7..11);
    assert_eq!(perr("name = 'abc\\'").message, "unclosed string");
}

#[test]
fn error_unknown_operators() {
    assert_err(
        "age => 5",
        "unknown operator '=>'; did you mean '>='?",
        "=>",
    );
    assert_err("age =< 5", "did you mean '<='?", "=<");
    assert_err("age === 5", "did you mean '='?", "===");
    assert_err("a = 1 & b = 2", "did you mean '&&'", "&");
    assert_err("a = 1 | b = 2", "did you mean '||'", "|");
    assert_err("a =~ \"x\"", "matches", "=~");
    assert_err(
        "name contains \"x\"",
        "unknown operator 'contains'; did you mean '~'?",
        "contains",
    );
}

#[test]
fn error_missing_operator() {
    let e = perr("name");
    assert!(
        e.message.starts_with("expected an operator after 'name'"),
        "{}",
        e.message
    );
    assert_eq!(e.span, 4..4);
    assert_err("name \"jo\"", "expected an operator after 'name'", "\"jo\"");
}

#[test]
fn error_structure() {
    assert_err("a = 1 b = 2", "expected 'and' or 'or' before 'b'", "b");
    assert_err("a = 1)", "unmatched ')'", ")");
    let e = perr("(a = 1");
    assert_eq!(e.message, "missing ')' to close '('");
    assert_eq!(e.span, 0..6);
    let e = perr("a = 1 and");
    assert_eq!(e.message, "expected a condition after 'and'");
    assert_eq!(e.span, 9..9);
    assert_err("a = 1 and or b = 1", "keyword 'or'", "or");
    assert_err("in = 1", "write `in`", "in");
    assert_err(
        "\"weird column\" = 1",
        "backticks: `weird column`",
        "\"weird column\"",
    );
    assert_err("a = 1 and ()", "expected a condition after '('", ")");
    assert_err("a. = 1", "expected a field name after '.'", "=");
}

#[test]
fn error_values() {
    assert_err("a = [1]", "only allowed after 'in'", "[");
    assert_err("a in 1", "expected a list after 'in'", "1");
    assert_err("a in [1 2]", "expected ',' or ']'", "2");
    let e = perr("a in [1, 2");
    assert_eq!(e.message, "missing ']' to close the list");
    assert_eq!(e.span, 5..10);
    assert_err("a = `b`", "backticks are for field names", "`b`");
    assert_err("a > null", "use 'is null'", "null");
    assert_err("a ~ 5", "'~' needs text in quotes", "5");
    assert_err("a like true", "'like' needs text in quotes", "true");
    assert_err("a < /x/", "a regex can't be used with '<'", "/x/");
    assert_err("a is 5", "expected 'null' after 'is'", "5");
    assert_err("a is not true", "expected 'null' after 'is not'", "true");
    assert_err("a between 1 or 2", "expected 'and' in 'between", "or");
    assert_err("a between null and 2", "'between' needs two values", "null");
    assert_err("a not = 1", "did you mean '!='?", "=");
    assert_err("a not foo", "expected 'in', 'like'", "foo");
}

#[test]
fn error_literals() {
    assert_err(
        "a = 99999999999999999999",
        "number is too large",
        "99999999999999999999",
    );
    assert_err("a = 1e999", "out of range", "1e999");
    assert_err("a = 12abc", "invalid number '12abc'", "12abc");
    assert_err("a = \"x\\q\"", "unknown escape '\\q'", "\\q");
    assert_err("a = \"\\u12\"", "expected 4 hex digits", "\\u12");
    assert_err("a = \"\\ud83d\"", "unpaired surrogate", "\\ud83d");
    assert_err("a = /abc", "unclosed regex", "/abc");
    assert_err("a = /abc/g", "unknown regex flag 'g'", "g");
    assert_err("a = date(\"2024-02-30\")", "invalid date", "\"2024-02-30\"");
    assert_err("a = date(2024)", "date() expects a quoted string", "2024");
    let e = perr("a = date(\"2024-01-01\"");
    assert_eq!(e.message, "missing ')' after date(…");
    assert_eq!(e.span, 21..21);
    assert_err("a = oid(\"123\")", "24 hex characters", "\"123\"");
    assert_err("a = foo(\"x\")", "unknown function 'foo'", "foo");
    assert_err("a = 1 # comment", "unexpected character '#'", "#");
    assert_err("a = “x”", "straight", "“");
    assert_err("`abc = 1", "unclosed `quoted name`", "`abc = 1");
    assert_err("`` = 1", "empty field name", "``");
}

#[test]
fn error_display_and_trait() {
    let e = perr("age >");
    assert_eq!(e.to_string(), "expected a value after '>'");
    let boxed: Box<dyn std::error::Error> = Box::new(e);
    assert!(boxed.to_string().contains("value"));
}

// ---- SQL ------------------------------------------------------------------------

#[test]
fn sql_comparisons() {
    assert_eq!(sql("age >= 18"), r#""age" >= '18'"#);
    assert_eq!(sql("age < 18.5"), r#""age" < '18.5'"#);
    assert_eq!(sql("age > -3"), r#""age" > '-3'"#);
    assert_eq!(sql("age = 1"), r#""age" = '1'"#);
    assert_eq!(sql("age != 1"), r#""age" <> '1'"#);
    assert_eq!(sql("age <= 1"), r#""age" <= '1'"#);
    assert_eq!(sql("status = \"active\""), r#""status" = 'active'"#);
    assert_eq!(sql("active = true"), r#""active" = 'true'"#);
    assert_eq!(
        sql("created > date(\"2024-01-01\")"),
        r#""created" > '2024-01-01'"#
    );
    assert_eq!(
        sql("id = oid(\"65a1f0c2e4b0a1b2c3d4e5f6\")"),
        r#""id" = '65a1f0c2e4b0a1b2c3d4e5f6'"#
    );
}

#[test]
fn sql_null_handling() {
    assert_eq!(sql("deleted_at = null"), r#""deleted_at" IS NULL"#);
    assert_eq!(sql("deleted_at != null"), r#""deleted_at" IS NOT NULL"#);
    assert_eq!(sql("deleted_at is null"), r#""deleted_at" IS NULL"#);
    assert_eq!(sql("deleted_at is not null"), r#""deleted_at" IS NOT NULL"#);
    assert_eq!(sql("name exists"), r#""name" IS NOT NULL"#);
    assert_eq!(sql("name not exists"), r#""name" IS NULL"#);
}

#[test]
fn sql_text_operators() {
    assert_eq!(sql("name ~ \"jo\""), r#""name"::text ILIKE '%jo%'"#);
    assert_eq!(
        sql("name !~ \"test\""),
        r#""name"::text NOT ILIKE '%test%'"#
    );
    assert_eq!(sql("email ^= \"admin\""), r#""email"::text ILIKE 'admin%'"#);
    assert_eq!(sql("email $= \".cz\""), r#""email"::text ILIKE '%.cz'"#);
    assert_eq!(
        sql("name like \"foo%bar_\""),
        r#""name"::text LIKE 'foo%bar_'"#
    );
    assert_eq!(sql("name ilike \"foo%\""), r#""name"::text ILIKE 'foo%'"#);
    assert_eq!(sql("name matches \"^J.*n$\""), r#""name"::text ~ '^J.*n$'"#);
    assert_eq!(sql("name matches /^J.*n$/i"), r#""name"::text ~* '^J.*n$'"#);
    assert_eq!(sql("name = /^J/"), r#""name"::text ~ '^J'"#);
    assert_eq!(sql("name != /^J/"), r#"NOT ("name"::text ~ '^J')"#);
    assert_eq!(
        sql("name not like \"a%\""),
        r#"NOT ("name"::text LIKE 'a%')"#
    );
}

#[test]
fn sql_contains_escapes_wildcards() {
    assert_eq!(
        sql(r#"name ~ "50%_off\\x""#),
        r#""name"::text ILIKE '%50\%\_off\\x%'"#
    );
    assert_eq!(sql(r#"name ^= "a_b""#), r#""name"::text ILIKE 'a\_b%'"#);
}

#[test]
fn sql_in_lists() {
    assert_eq!(sql("status in [\"a\", \"b\"]"), r#""status" IN ('a', 'b')"#);
    assert_eq!(sql("id in [1, 2, 3]"), r#""id" IN ('1', '2', '3')"#);
    assert_eq!(sql("id not in [1, 2]"), r#""id" NOT IN ('1', '2')"#);
    assert_eq!(sql("id in []"), "FALSE");
    assert_eq!(sql("id not in []"), "TRUE");
    assert_eq!(sql("id in [1, null]"), r#"("id" IN ('1') OR "id" IS NULL)"#);
    assert_eq!(
        sql("id not in [1, null]"),
        r#"("id" NOT IN ('1') AND "id" IS NOT NULL)"#
    );
    assert_eq!(sql("id in [null]"), r#""id" IS NULL"#);
    assert_eq!(sql("id not in [null]"), r#""id" IS NOT NULL"#);
}

#[test]
fn sql_between() {
    assert_eq!(
        sql("age between 18 and 65"),
        r#""age" BETWEEN '18' AND '65'"#
    );
    assert_eq!(
        sql("created between date(\"2024-01-01\") and date(\"2024-12-31\")"),
        r#""created" BETWEEN '2024-01-01' AND '2024-12-31'"#
    );
    assert_eq!(
        sql("age not between 1 and 2"),
        r#"NOT ("age" BETWEEN '1' AND '2')"#
    );
}

#[test]
fn sql_boolean_structure_is_parenthesized() {
    assert_eq!(
        sql("status = \"active\" and age >= 18"),
        r#"("status" = 'active' AND "age" >= '18')"#
    );
    assert_eq!(
        sql("id = 1 or id = 2 or id = 3"),
        r#"("id" = '1' OR "id" = '2' OR "id" = '3')"#
    );
}

#[test]
fn sql_precedence_preserved() {
    assert_eq!(
        sql("id = 1 or id = 2 and age = 3"),
        r#"("id" = '1' OR ("id" = '2' AND "age" = '3'))"#
    );
    assert_eq!(
        sql("(id = 1 or id = 2) and age = 3"),
        r#"(("id" = '1' OR "id" = '2') AND "age" = '3')"#
    );
    assert_eq!(
        sql("not (id = 1 or id = 2)"),
        r#"NOT (("id" = '1' OR "id" = '2'))"#
    );
    assert_eq!(sql("not id = 1"), r#"NOT ("id" = '1')"#);
}

#[test]
fn sql_injection_attempts_are_quoted() {
    assert_eq!(
        sql(r#"name = "'; drop table x; --""#),
        r#""name" = '''; drop table x; --'"#
    );
    assert_eq!(sql(r#"name = "O'Brien""#), r#""name" = 'O''Brien'"#);
    assert_eq!(sql(r#"name = "a\\'b""#), r#""name" = 'a\''b'"#);
    assert_eq!(sql(r#"name = "\\""#), r#""name" = '\'"#);
    assert_eq!(sql(r#"name ~ "'""#), r#""name"::text ILIKE '%''%'"#);
    assert_eq!(sql(r#"name in ["x'", "y"]"#), r#""name" IN ('x''', 'y')"#);
    assert_eq!(sql(r#"name matches "'|.*""#), r#""name"::text ~ '''|.*'"#);
    assert_eq!(sql("name like \"$$; --\""), r#""name"::text LIKE '$$; --'"#);
    // Identifiers: double quotes are doubled.
    assert_eq!(
        sql_nocols(r#"`a"; drop table x; --` = 1"#),
        r#""a""; drop table x; --" = '1'"#
    );
    assert_eq!(sql(r#"`weird "col"` = 1"#), r#""weird ""col""" = '1'"#);
    // JSON path elements with quotes and braces.
    assert_eq!(
        sql(r#"data.`it's`.`a"b}` = 1"#),
        r#"("data" #>> '{it''s,"a\"b}"}') = '1'"#
    );
}

#[test]
fn sql_rejects_nul_bytes() {
    let e = sql_err(r#"name = "a\u0000b""#);
    assert!(e.message.contains("NUL"), "{}", e.message);
    assert_eq!(e.span, 0..17);
    let e = to_sql(&p("`a\\u0000` = 1"), &[]);
    // Backtick names have no escapes, so this is a literal backslash: fine.
    assert_eq!(e.unwrap(), r#""a\u0000" = '1'"#);
    let e = sql_err(r#"name ~ "\0""#);
    assert!(e.message.contains("NUL"));
}

#[test]
fn sql_column_resolution() {
    // exact, then case-insensitive (output uses the real column name)
    assert_eq!(sql("UserName = \"x\""), r#""UserName" = 'x'"#);
    assert_eq!(sql("username = \"x\""), r#""UserName" = 'x'"#);
    assert_eq!(sql("AGE = 1"), r#""age" = '1'"#);
    // no columns: no validation
    assert_eq!(sql_nocols("Whatever = 1"), r#""Whatever" = '1'"#);
    assert_eq!(sql_nocols("doc.a.b = 1"), r#"("doc" #>> '{a,b}') = '1'"#);
}

#[test]
fn sql_unknown_column_suggestion() {
    let e = sql_err("nmae = \"x\"");
    assert_eq!(e.message, "unknown column 'nmae'; did you mean 'name'?");
    assert_eq!(e.span, 0..4);
    let e = sql_err("age > 1 and emial ~ \"x\"");
    assert_eq!(e.message, "unknown column 'emial'; did you mean 'email'?");
    assert_eq!(e.span, 12..17);
    let e = sql_err("zzzzzzzz = 1");
    assert_eq!(e.message, "unknown column 'zzzzzzzz'");
    let e = sql_err("dta.x = 1");
    assert_eq!(e.message, "unknown column 'dta'; did you mean 'data'?");
    assert_eq!(e.span, 0..5);
}

#[test]
fn sql_dotted_path_on_non_json_column() {
    let e = sql_err("name.first = \"x\"");
    assert!(
        e.message.contains("not a json/jsonb column"),
        "{}",
        e.message
    );
    assert_eq!(e.span, 0..10);
}

#[test]
fn sql_json_paths() {
    assert_eq!(
        sql("data.user.name = \"Jo\""),
        r#"("data" #>> '{user,name}') = 'Jo'"#
    );
    assert_eq!(sql("meta.a = \"x\""), r#"("meta" #>> '{a}') = 'x'"#);
    assert_eq!(
        sql("data.age > 30"),
        r#"("data" #>> '{age}')::numeric > 30"#
    );
    assert_eq!(
        sql("data.age <= -1.5"),
        r#"("data" #>> '{age}')::numeric <= -1.5"#
    );
    assert_eq!(sql("data.age = 30"), r#"("data" #>> '{age}') = '30'"#);
    assert_eq!(sql("data.d > \"2024\""), r#"("data" #>> '{d}') > '2024'"#);
    assert_eq!(sql("data.flag = true"), r#"("data" #>> '{flag}') = 'true'"#);
    assert_eq!(
        sql("data.flag != false"),
        r#"("data" #>> '{flag}') <> 'false'"#
    );
    assert_eq!(
        sql("data.age between 18 and 65"),
        r#"("data" #>> '{age}')::numeric BETWEEN 18 AND 65"#
    );
    assert_eq!(
        sql("data.n between \"a\" and \"b\""),
        r#"("data" #>> '{n}') BETWEEN 'a' AND 'b'"#
    );
    assert_eq!(
        sql("data.user.name exists"),
        r#"("data" #> '{user,name}') IS NOT NULL"#
    );
    assert_eq!(
        sql("data.user not exists"),
        r#"("data" #> '{user}') IS NULL"#
    );
    assert_eq!(sql("data.x is null"), r#"("data" #>> '{x}') IS NULL"#);
    assert_eq!(sql("data.x ~ \"jo\""), r#"("data" #>> '{x}') ILIKE '%jo%'"#);
    assert_eq!(sql("data.x matches /a/i"), r#"("data" #>> '{x}') ~* 'a'"#);
    assert_eq!(
        sql("data.x in [1, \"b\"]"),
        r#"("data" #>> '{x}') IN ('1', 'b')"#
    );
    assert_eq!(
        sql("data.items.0.sku = \"A\""),
        r#"("data" #>> '{items,0,sku}') = 'A'"#
    );
    assert_eq!(
        sql("data.`a b`.`null`.`x,y` = 1"),
        r#"("data" #>> '{"a b","null","x,y"}') = '1'"#
    );
}

#[test]
fn sql_array_columns() {
    assert_eq!(sql("tags = \"a\""), r#"'a' = ANY("tags")"#);
    assert_eq!(sql("tags != \"a\""), r#"NOT ('a' = ANY("tags"))"#);
    assert_eq!(sql("scores = 3"), r#"'3' = ANY("scores")"#);
    assert_eq!(
        sql("tags in [\"a\", \"b\"]"),
        r#"('a' = ANY("tags") OR 'b' = ANY("tags"))"#
    );
    assert_eq!(sql("tags in [\"a\"]"), r#"'a' = ANY("tags")"#);
    assert_eq!(sql("tags not in [\"a\"]"), r#"NOT ('a' = ANY("tags"))"#);
    assert_eq!(sql("tags ~ \"a\""), r#""tags"::text ILIKE '%a%'"#);
    assert_eq!(sql("tags is null"), r#""tags" IS NULL"#);
}

#[test]
fn sql_errors() {
    let e = sql_err("name matches /x/m");
    assert!(e.message.contains("flag 'm'"), "{}", e.message);
    let e = sql_err("name in [/x/]");
    assert!(e.message.contains("regex"), "{}", e.message);
}

#[test]
fn compile_sql_helper() {
    assert_eq!(
        compile_sql("age > 1", &cols()).unwrap().as_deref(),
        Some(r#""age" > '1'"#)
    );
    assert!(compile_sql("age >", &cols()).is_err());
}

// ---- Mongo ------------------------------------------------------------------------

#[test]
fn mongo_comparisons() {
    assert_eq!(mongo("a = 1"), doc! { "a": 1_i32 });
    assert_eq!(mongo("a = 5000000000"), doc! { "a": 5_000_000_000_i64 });
    assert_eq!(mongo("a = -2147483648"), doc! { "a": i32::MIN });
    assert_eq!(mongo("a = 1.5"), doc! { "a": 1.5 });
    assert_eq!(mongo("a = \"x\""), doc! { "a": "x" });
    assert_eq!(mongo("a = true"), doc! { "a": true });
    assert_eq!(mongo("a != 1"), doc! { "a": { "$ne": 1 } });
    assert_eq!(mongo("a < 1"), doc! { "a": { "$lt": 1 } });
    assert_eq!(mongo("a <= 1"), doc! { "a": { "$lte": 1 } });
    assert_eq!(mongo("a > 1"), doc! { "a": { "$gt": 1 } });
    assert_eq!(mongo("a >= 1"), doc! { "a": { "$gte": 1 } });
    assert_eq!(
        mongo("address.city = \"Prague\""),
        doc! { "address.city": "Prague" }
    );
    assert_eq!(mongo("items.0.sku = \"A\""), doc! { "items.0.sku": "A" });
}

#[test]
fn mongo_nulls_and_exists() {
    assert_eq!(mongo("a = null"), doc! { "a": Bson::Null });
    assert_eq!(mongo("a is null"), doc! { "a": Bson::Null });
    assert_eq!(mongo("a is not null"), doc! { "a": { "$ne": Bson::Null } });
    assert_eq!(mongo("a != null"), doc! { "a": { "$ne": Bson::Null } });
    assert_eq!(mongo("a exists"), doc! { "a": { "$exists": true } });
    assert_eq!(mongo("a not exists"), doc! { "a": { "$exists": false } });
}

#[test]
fn mongo_text_operators() {
    assert_eq!(
        mongo("n ~ \"jo\""),
        doc! { "n": { "$regex": "jo", "$options": "i" } }
    );
    assert_eq!(
        mongo("n ~ \"a.b(c)*\""),
        doc! { "n": { "$regex": "a\\.b\\(c\\)\\*", "$options": "i" } }
    );
    assert_eq!(mongo("n !~ \"x\""), doc! { "n": { "$not": re("x", "i") } });
    assert_eq!(
        mongo("e ^= \"ad\""),
        doc! { "e": { "$regex": "^ad", "$options": "i" } }
    );
    assert_eq!(
        mongo("e $= \".cz\""),
        doc! { "e": { "$regex": "\\.cz$", "$options": "i" } }
    );
    assert_eq!(
        mongo("t like \"foo%bar_.\""),
        doc! { "t": { "$regex": "^foo.*bar.\\.$", "$options": "s" } }
    );
    assert_eq!(
        mongo("t ilike \"50\\\\%%\""),
        doc! { "t": { "$regex": "^50%.*$", "$options": "is" } }
    );
    assert_eq!(
        mongo("n matches \"^J.*n$\""),
        doc! { "n": { "$regex": "^J.*n$" } }
    );
    assert_eq!(
        mongo("n matches /^J.*n$/xi"),
        doc! { "n": { "$regex": "^J.*n$", "$options": "ix" } }
    );
    assert_eq!(mongo("n = /^J/"), doc! { "n": { "$regex": "^J" } });
    assert_eq!(
        mongo("n != /^J/"),
        doc! { "$nor": [ { "n": { "$regex": "^J" } } ] }
    );
}

#[test]
fn mongo_in_between() {
    assert_eq!(
        mongo("tags in [\"a\", \"b\", 3]"),
        doc! { "tags": { "$in": ["a", "b", 3] } }
    );
    assert_eq!(mongo("tags not in []"), doc! { "tags": { "$nin": [] } });
    assert_eq!(
        mongo("tags in [null, /x/i]"),
        doc! { "tags": { "$in": [Bson::Null, re("x", "i")] } }
    );
    assert_eq!(
        mongo("age between 18 and 65"),
        doc! { "age": { "$gte": 18, "$lte": 65 } }
    );
}

#[test]
fn mongo_boolean_structure() {
    // disjoint keys are merged
    assert_eq!(
        mongo("status = \"active\" and age >= 18"),
        doc! { "status": "active", "age": { "$gte": 18 } }
    );
    // colliding keys fall back to $and
    assert_eq!(
        mongo("age > 1 and age < 5"),
        doc! { "$and": [ { "age": { "$gt": 1 } }, { "age": { "$lt": 5 } } ] }
    );
    assert_eq!(
        mongo("a = 1 or b = 2"),
        doc! { "$or": [ { "a": 1 }, { "b": 2 } ] }
    );
    assert_eq!(
        mongo("a = 1 or b = 2 and c = 3"),
        doc! { "$or": [ { "a": 1 }, { "b": 2, "c": 3 } ] }
    );
    assert_eq!(
        mongo("not (a = 1 or b = 2)"),
        doc! { "$nor": [ { "$or": [ { "a": 1 }, { "b": 2 } ] } ] }
    );
    assert_eq!(
        mongo("(a = 1 or b = 2) and (c = 3 or d = 4)"),
        doc! { "$and": [
            { "$or": [ { "a": 1 }, { "b": 2 } ] },
            { "$or": [ { "c": 3 }, { "d": 4 } ] },
        ] }
    );
    assert_eq!(
        mongo("x = 1 and (a = 1 or b = 2)"),
        doc! { "x": 1, "$or": [ { "a": 1 }, { "b": 2 } ] }
    );
}

#[test]
fn mongo_typed_literals() {
    let dt = bson::DateTime::parse_rfc3339_str("2024-01-01T00:00:00Z").unwrap();
    assert_eq!(
        mongo("c > date(\"2024-01-01\")"),
        doc! { "c": { "$gt": dt } }
    );
    let dt2 = bson::DateTime::parse_rfc3339_str("2024-01-01T10:00:00Z").unwrap();
    assert_eq!(
        mongo("c = date(\"2024-01-01T10:00:00Z\")"),
        doc! { "c": dt2 }
    );
    assert_eq!(
        mongo("c = date(\"2024-01-01T12:00:00+02:00\")"),
        doc! { "c": dt2 }
    );
    let oid = bson::oid::ObjectId::parse_str("65a1f0c2e4b0a1b2c3d4e5f6").unwrap();
    assert_eq!(
        mongo("_id = oid(\"65a1f0c2e4b0a1b2c3d4e5f6\")"),
        doc! { "_id": oid }
    );
    assert_eq!(
        mongo("_id in [oid(\"65a1f0c2e4b0a1b2c3d4e5f6\")]"),
        doc! { "_id": { "$in": [oid] } }
    );
}

#[test]
fn mongo_rejects_dollar_fields() {
    let e = to_mongo(&p("`$where` = \"sleep(1000)\"")).unwrap_err();
    assert!(e.message.contains("can't start with '$'"), "{}", e.message);
    assert_eq!(e.span, 0..8);
    assert!(to_mongo(&p("a.`$gt` = 1")).is_err());
}

#[test]
fn mongo_injection_like_values_stay_values() {
    // Values are always data, never operators.
    assert_eq!(mongo("a = \"{$gt: ''}\""), doc! { "a": "{$gt: ''}" });
    assert_eq!(
        mongo("a ~ \"$where\""),
        doc! { "a": { "$regex": "\\$where", "$options": "i" } }
    );
}

#[test]
fn compile_mongo_helper() {
    assert_eq!(compile_mongo("a = 1").unwrap(), doc! { "a": 1 });
    assert!(compile_mongo("a =").is_err());
}

#[test]
fn help_is_short() {
    let lines = HELP.lines().count();
    assert!((8..=12).contains(&lines), "{lines} lines");
    // every example line in HELP should parse (sanity check of the docs)
    for ex in [
        "status = \"active\" and age >= 18",
        "name ~ \"jo\"",
        "name !~ \"test\"",
        "email ^= \"admin\"",
        "email $= \".cz\"",
        "title like \"foo%\"",
        "name matches \"^J.*n$\"",
        "name = /^j/i",
        "tags in [\"a\", \"b\", 3]",
        "deleted_at is null",
        "age between 18 and 65",
        "profile exists",
        "profile not exists",
        "not (a = 1 or b = 2)",
        "address.city = \"Prague\"",
        "`odd name` = 1",
        "created > date(\"2024-01-01\")",
    ] {
        p(ex);
    }
}
