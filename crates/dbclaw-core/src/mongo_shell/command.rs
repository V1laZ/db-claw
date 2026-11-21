//! mongosh-style command parser (`db.users.find({...}).sort({...})`).

use std::ops::Range;

use bson::{Bson, Document};

use super::ShellError;
use super::lexer::Cursor;
use super::literal::{as_integer, finish, parse_value_at, type_name};

/// A parsed console statement.
#[derive(Debug, Clone, PartialEq)]
pub enum MongoCommand {
    Find {
        collection: String,
        filter: Document,
        projection: Option<Document>,
        sort: Option<Document>,
        skip: Option<u64>,
        limit: Option<i64>,
    },
    FindOne {
        collection: String,
        filter: Document,
        projection: Option<Document>,
    },
    Aggregate {
        collection: String,
        pipeline: Vec<Document>,
    },
    Count {
        collection: String,
        filter: Document,
    },
    EstimatedCount {
        collection: String,
    },
    Distinct {
        collection: String,
        field: String,
        filter: Document,
    },
    InsertOne {
        collection: String,
        document: Document,
    },
    InsertMany {
        collection: String,
        documents: Vec<Document>,
    },
    UpdateOne {
        collection: String,
        filter: Document,
        update: UpdateSpec,
        upsert: bool,
    },
    UpdateMany {
        collection: String,
        filter: Document,
        update: UpdateSpec,
        upsert: bool,
    },
    ReplaceOne {
        collection: String,
        filter: Document,
        replacement: Document,
        upsert: bool,
    },
    DeleteOne {
        collection: String,
        filter: Document,
    },
    DeleteMany {
        collection: String,
        filter: Document,
    },
    CreateIndex {
        collection: String,
        keys: Document,
        options: Option<Document>,
    },
    ListIndexes {
        collection: String,
    },
    DropIndex {
        collection: String,
        name: String,
    },
    DropCollection {
        collection: String,
    },
    CreateCollection {
        name: String,
    },
    RunCommand {
        command: Document,
        admin: bool,
    },
    ShowCollections,
    ShowDatabases,
}

/// The update argument of `updateOne` / `updateMany`.
#[derive(Debug, Clone, PartialEq)]
pub enum UpdateSpec {
    /// An update document with operators (`{$set: {...}}`).
    Document(Document),
    /// An aggregation-pipeline update (`[{$set: {...}}, ...]`).
    Pipeline(Vec<Document>),
}

impl MongoCommand {
    /// True when the command may modify data or schema.
    ///
    /// `runCommand`/`adminCommand` are conservatively treated as writes, and
    /// so is an aggregation containing a `$out` or `$merge` stage.
    pub fn is_write(&self) -> bool {
        match self {
            MongoCommand::Find { .. }
            | MongoCommand::FindOne { .. }
            | MongoCommand::Count { .. }
            | MongoCommand::EstimatedCount { .. }
            | MongoCommand::Distinct { .. }
            | MongoCommand::ListIndexes { .. }
            | MongoCommand::ShowCollections
            | MongoCommand::ShowDatabases => false,
            MongoCommand::Aggregate { pipeline, .. } => pipeline
                .iter()
                .any(|s| s.contains_key("$out") || s.contains_key("$merge")),
            MongoCommand::InsertOne { .. }
            | MongoCommand::InsertMany { .. }
            | MongoCommand::UpdateOne { .. }
            | MongoCommand::UpdateMany { .. }
            | MongoCommand::ReplaceOne { .. }
            | MongoCommand::DeleteOne { .. }
            | MongoCommand::DeleteMany { .. }
            | MongoCommand::CreateIndex { .. }
            | MongoCommand::DropIndex { .. }
            | MongoCommand::DropCollection { .. }
            | MongoCommand::CreateCollection { .. }
            | MongoCommand::RunCommand { .. } => true,
        }
    }

    /// The collection the command targets (for `CreateCollection`, the new
    /// collection's name). `None` for database-level commands.
    pub fn collection(&self) -> Option<&str> {
        match self {
            MongoCommand::Find { collection, .. }
            | MongoCommand::FindOne { collection, .. }
            | MongoCommand::Aggregate { collection, .. }
            | MongoCommand::Count { collection, .. }
            | MongoCommand::EstimatedCount { collection }
            | MongoCommand::Distinct { collection, .. }
            | MongoCommand::InsertOne { collection, .. }
            | MongoCommand::InsertMany { collection, .. }
            | MongoCommand::UpdateOne { collection, .. }
            | MongoCommand::UpdateMany { collection, .. }
            | MongoCommand::ReplaceOne { collection, .. }
            | MongoCommand::DeleteOne { collection, .. }
            | MongoCommand::DeleteMany { collection, .. }
            | MongoCommand::CreateIndex { collection, .. }
            | MongoCommand::ListIndexes { collection }
            | MongoCommand::DropIndex { collection, .. }
            | MongoCommand::DropCollection { collection } => Some(collection),
            MongoCommand::CreateCollection { name } => Some(name),
            MongoCommand::RunCommand { .. }
            | MongoCommand::ShowCollections
            | MongoCommand::ShowDatabases => None,
        }
    }
}

const COLLECTION_METHODS: &str = "find, findOne, aggregate, countDocuments, count, \
estimatedDocumentCount, distinct, insertOne, insertMany, updateOne, updateMany, replaceOne, \
deleteOne, deleteMany, createIndex, getIndexes, dropIndex, drop";

const DB_METHODS: &str =
    "getCollection, createCollection, runCommand, adminCommand, getCollectionNames";

const FIND_CHAIN_METHODS: &str = "sort, skip, limit, project, projection, count, toArray, pretty";

const EXPECTED_COMMAND: &str =
    "expected db.<collection>.<method>(…) or show collections / show dbs";

struct Arg {
    value: Bson,
    span: Range<usize>,
}

struct Call<'a> {
    name: &'a str,
    name_span: Range<usize>,
    args: Vec<Arg>,
    /// Span of the whole call including the name and parentheses.
    span: Range<usize>,
}

/// Parses a single mongosh statement (optionally terminated by `;`).
pub fn parse_command(input: &str) -> Result<MongoCommand, ShellError> {
    let mut c = Cursor::new(input);
    c.skip_ws()?;
    if c.at_end() {
        return c.err(format!("empty command: {EXPECTED_COMMAND}"), 0..0);
    }
    let start = c.pos;
    let cmd = match c.peek_ident() {
        Some("show") => parse_show(&mut c)?,
        Some("db") => parse_db(&mut c)?,
        Some("use") => {
            let (_, span) = c.ident().expect("ident");
            return c.err(
                "`use <db>` is not supported here; pick the database in the sidebar",
                span,
            );
        }
        Some(word) => {
            let span = start..start + word.len();
            return c.err(
                format!("unknown command `{word}`: {EXPECTED_COMMAND}"),
                span,
            );
        }
        None => {
            let span = start..input.len();
            return c.err(EXPECTED_COMMAND, span);
        }
    };
    c.skip_ws()?;
    if c.eat(';') {
        c.skip_ws()?;
    }
    if !c.at_end() {
        let span = c.pos..input.len();
        return c.err(
            format!(
                "unexpected {} after the command (only one statement can be run at a time)",
                c.describe_here()
            ),
            span,
        );
    }
    Ok(cmd)
}

/// Like [`parse_command`], but also accepts a bare filter object (run as
/// `find(filter)` on `default_collection`) or a bare pipeline array (run as
/// `aggregate(pipeline)`). Empty input means `find({})`.
pub fn parse_filter_or_command(
    input: &str,
    default_collection: Option<&str>,
) -> Result<MongoCommand, ShellError> {
    let mut c = Cursor::new(input);
    c.skip_ws()?;
    let start = c.pos;
    let bare = match c.peek() {
        None => Some(Bson::Document(Document::new())),
        Some('{' | '[') => {
            let v = parse_value_at(&mut c, 0)?;
            finish(&mut c)?;
            Some(v)
        }
        _ => None,
    };
    let Some(value) = bare else {
        return parse_command(input);
    };
    let span = start..input.len();
    let Some(collection) = default_collection else {
        return c.err(
            "no collection selected: select a collection or write db.<collection>.find(…)",
            span,
        );
    };
    let collection = collection.to_string();
    match value {
        Bson::Document(filter) => Ok(MongoCommand::Find {
            collection,
            filter,
            projection: None,
            sort: None,
            skip: None,
            limit: None,
        }),
        Bson::Array(items) => {
            let pipeline = docs_from_array(&c, items, &span, "pipeline stage")?;
            Ok(MongoCommand::Aggregate {
                collection,
                pipeline,
            })
        }
        other => c.err(
            format!(
                "expected a filter document or a pipeline array, got {}",
                type_name(&other)
            ),
            span,
        ),
    }
}

fn parse_show(c: &mut Cursor) -> Result<MongoCommand, ShellError> {
    let (_, show_span) = c.ident().expect("show");
    c.skip_ws()?;
    match c.ident() {
        Some(("collections" | "tables", _)) => Ok(MongoCommand::ShowCollections),
        Some(("dbs" | "databases", _)) => Ok(MongoCommand::ShowDatabases),
        Some((other, span)) => c.err(
            format!("unknown `show {other}`; supported: show collections, show dbs"),
            span,
        ),
        None => c.err(
            "expected `collections` or `dbs` after `show`",
            show_span.start..c.here().end,
        ),
    }
}

fn parse_call<'a>(
    c: &mut Cursor<'a>,
    name: &'a str,
    name_span: Range<usize>,
) -> Result<Call<'a>, ShellError> {
    // cursor is on `(`
    let open = c.pos;
    c.bump();
    let mut args = Vec::new();
    loop {
        c.skip_ws()?;
        if c.eat(')') {
            break;
        }
        if c.at_end() {
            return c.err(
                format!("unterminated call to {name}(…): expected `)`"),
                open..c.src.len(),
            );
        }
        let s = c.pos;
        let value = parse_value_at(c, 0)?;
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
                format!("unterminated call to {name}(…): expected `)`"),
                open..c.src.len(),
            );
        }
        return c.err(
            format!(
                "expected `,` or `)` in arguments of {name}(…), found {}",
                c.describe_here()
            ),
            c.here(),
        );
    }
    let span = name_span.start..c.pos;
    Ok(Call {
        name,
        name_span,
        args,
        span,
    })
}

/// Reads `.name(` … `)` — returns None if the next token is not `.`.
fn parse_chained_call<'a>(c: &mut Cursor<'a>) -> Result<Option<Call<'a>>, ShellError> {
    c.skip_ws()?;
    if c.peek() != Some('.') {
        return Ok(None);
    }
    c.bump();
    c.skip_ws()?;
    let Some((name, span)) = c.ident() else {
        return c.err(
            format!(
                "expected a method name after `.`, found {}",
                c.describe_here()
            ),
            c.here(),
        );
    };
    c.skip_ws()?;
    if c.peek() != Some('(') {
        return c.err(
            format!("expected `(` after `.{name}` (methods must be called, e.g. .{name}(…))"),
            c.here(),
        );
    }
    parse_call(c, name, span).map(Some)
}

fn parse_db(c: &mut Cursor) -> Result<MongoCommand, ShellError> {
    let (_, db_span) = c.ident().expect("db");
    // Collection path segments (`db.a.b` or `db["a"]`) until a method call.
    let mut segments: Vec<String> = Vec::new();
    let mut path_span = db_span.end..db_span.end;
    loop {
        c.skip_ws()?;
        match c.peek() {
            Some('.') => {
                c.bump();
                c.skip_ws()?;
                let Some((name, span)) = c.ident() else {
                    return c.err(
                        format!(
                            "expected a collection or method name after `.`, found {}",
                            c.describe_here()
                        ),
                        c.here(),
                    );
                };
                c.skip_ws()?;
                if c.peek() == Some('(') {
                    let call = parse_call(c, name, span)?;
                    if segments.is_empty() {
                        return db_method(c, call);
                    }
                    let collection = segments.join(".");
                    return collection_method(c, collection, path_span, call);
                }
                segments.push(name.to_string());
                path_span.end = span.end;
            }
            Some('[') => {
                c.bump();
                c.skip_ws()?;
                let s = c.pos;
                let name = match c.peek() {
                    Some('"' | '\'') => c.string()?,
                    _ => {
                        return c.err(
                            format!(
                                "expected a quoted collection name inside `[…]`, found {}",
                                c.describe_here()
                            ),
                            c.here(),
                        );
                    }
                };
                if name.is_empty() {
                    return c.err("collection name cannot be empty", s..c.pos);
                }
                c.expect(']', "after collection name")?;
                segments.push(name);
                path_span.end = c.pos;
            }
            _ => {
                if segments.is_empty() {
                    return c.err(EXPECTED_COMMAND, db_span.start..c.here().end);
                }
                return c.err(
                    format!(
                        "expected a method call after `db.{}`, e.g. .find({{}})",
                        segments.join(".")
                    ),
                    db_span.start..c.pos,
                );
            }
        }
    }
}

fn db_method(c: &mut Cursor, call: Call) -> Result<MongoCommand, ShellError> {
    match call.name {
        "getCollection" => {
            arity(c, &call, 1, 1)?;
            let name = string_arg(c, &call, 0, "collection name")?;
            if name.is_empty() {
                return c.err("collection name cannot be empty", call.args[0].span.clone());
            }
            let Some(next) = parse_chained_call(c)? else {
                return c.err(
                    format!("expected a method call after getCollection(\"{name}\"), e.g. .find({{}})"),
                    call.span,
                );
            };
            collection_method(c, name, call.span.clone(), next)
        }
        "createCollection" => {
            arity(c, &call, 1, 1)?;
            let name = string_arg(c, &call, 0, "collection name")?;
            no_chain(c, &call)?;
            Ok(MongoCommand::CreateCollection { name })
        }
        "runCommand" | "adminCommand" => {
            arity(c, &call, 1, 1)?;
            let a = &call.args[0];
            let command = match &a.value {
                Bson::Document(d) => d.clone(),
                Bson::String(s) => {
                    let mut d = Document::new();
                    d.insert(s.clone(), 1);
                    d
                }
                v => {
                    return c.err(
                        format!("{}() expects a command document, got {}", call.name, type_name(v)),
                        a.span.clone(),
                    )
                }
            };
            no_chain(c, &call)?;
            Ok(MongoCommand::RunCommand {
                command,
                admin: call.name == "adminCommand",
            })
        }
        "getCollectionNames" => {
            arity(c, &call, 0, 0)?;
            no_chain(c, &call)?;
            Ok(MongoCommand::ShowCollections)
        }
        other => c.err(
            format!("unknown database method `{other}`; supported: {DB_METHODS} (or db.<collection>.<method>(…))"),
            call.name_span,
        ),
    }
}

fn collection_method(
    c: &mut Cursor,
    collection: String,
    _path_span: Range<usize>,
    call: Call,
) -> Result<MongoCommand, ShellError> {
    let coll = collection;
    let cmd = match call.name {
        "find" => {
            arity(c, &call, 0, 3)?;
            let filter = opt_doc_arg(c, &call, 0, "filter")?.unwrap_or_default();
            let projection = opt_doc_arg(c, &call, 1, "projection")?;
            let mut cmd = MongoCommand::Find {
                collection: coll,
                filter,
                projection,
                sort: None,
                skip: None,
                limit: None,
            };
            if let Some(opts) = opt_doc_arg(c, &call, 2, "options")? {
                apply_find_options(c, &mut cmd, &opts, call.args[2].span.clone())?;
            }
            return find_chain(c, cmd);
        }
        "findOne" => {
            arity(c, &call, 0, 2)?;
            MongoCommand::FindOne {
                collection: coll,
                filter: opt_doc_arg(c, &call, 0, "filter")?.unwrap_or_default(),
                projection: opt_doc_arg(c, &call, 1, "projection")?,
            }
        }
        "aggregate" => {
            arity(c, &call, 0, 1)?;
            let pipeline = match call.args.first() {
                None => Vec::new(),
                Some(a) => match &a.value {
                    Bson::Array(items) => {
                        docs_from_array(c, items.clone(), &a.span, "pipeline stage")?
                    }
                    Bson::Document(d) => vec![d.clone()],
                    v => {
                        return c.err(
                            format!("aggregate() expects a pipeline array, got {}", type_name(v)),
                            a.span.clone(),
                        );
                    }
                },
            };
            let cmd = MongoCommand::Aggregate {
                collection: coll,
                pipeline,
            };
            return ignore_chain(c, cmd, "aggregate");
        }
        "countDocuments" | "count" => {
            arity(c, &call, 0, 1)?;
            MongoCommand::Count {
                collection: coll,
                filter: opt_doc_arg(c, &call, 0, "filter")?.unwrap_or_default(),
            }
        }
        "estimatedDocumentCount" => {
            arity(c, &call, 0, 0)?;
            MongoCommand::EstimatedCount { collection: coll }
        }
        "distinct" => {
            arity(c, &call, 1, 2)?;
            MongoCommand::Distinct {
                collection: coll,
                field: string_arg(c, &call, 0, "field name")?,
                filter: opt_doc_arg(c, &call, 1, "filter")?.unwrap_or_default(),
            }
        }
        "insertOne" => {
            arity(c, &call, 1, 1)?;
            MongoCommand::InsertOne {
                collection: coll,
                document: doc_arg(c, &call, 0, "document")?,
            }
        }
        "insertMany" | "insert" => {
            arity(c, &call, 1, 1)?;
            let a = &call.args[0];
            match &a.value {
                // legacy insert(doc)
                Bson::Document(d) if call.name == "insert" => MongoCommand::InsertOne {
                    collection: coll,
                    document: d.clone(),
                },
                Bson::Array(items) => MongoCommand::InsertMany {
                    collection: coll,
                    documents: docs_from_array(c, items.clone(), &a.span, "document")?,
                },
                v => {
                    return c.err(
                        format!(
                            "{}() expects an array of documents, got {}",
                            call.name,
                            type_name(v)
                        ),
                        a.span.clone(),
                    );
                }
            }
        }
        "updateOne" | "updateMany" => {
            arity(c, &call, 2, 3)?;
            let filter = doc_arg(c, &call, 0, "filter")?;
            let a = &call.args[1];
            let update = match &a.value {
                Bson::Document(d) => {
                    if !d.is_empty() && !d.keys().all(|k| k.starts_with('$')) {
                        return c.err(
                            "update document must contain only update operators like $set; use replaceOne() to replace a whole document",
                            a.span.clone(),
                        );
                    }
                    UpdateSpec::Document(d.clone())
                }
                Bson::Array(items) => UpdateSpec::Pipeline(docs_from_array(
                    c,
                    items.clone(),
                    &a.span,
                    "pipeline stage",
                )?),
                v => {
                    return c.err(
                        format!(
                            "update must be a document or a pipeline array, got {}",
                            type_name(v)
                        ),
                        a.span.clone(),
                    );
                }
            };
            let upsert = upsert_option(c, &call, 2)?;
            if call.name == "updateOne" {
                MongoCommand::UpdateOne {
                    collection: coll,
                    filter,
                    update,
                    upsert,
                }
            } else {
                MongoCommand::UpdateMany {
                    collection: coll,
                    filter,
                    update,
                    upsert,
                }
            }
        }
        "replaceOne" => {
            arity(c, &call, 2, 3)?;
            let filter = doc_arg(c, &call, 0, "filter")?;
            let replacement = doc_arg(c, &call, 1, "replacement")?;
            if replacement.keys().any(|k| k.starts_with('$')) {
                return c.err(
                    "replacement document must not contain update operators; use updateOne() instead",
                    call.args[1].span.clone(),
                );
            }
            let upsert = upsert_option(c, &call, 2)?;
            MongoCommand::ReplaceOne {
                collection: coll,
                filter,
                replacement,
                upsert,
            }
        }
        "deleteOne" | "deleteMany" => {
            arity(c, &call, 1, 1)?;
            let filter = doc_arg(c, &call, 0, "filter")?;
            if call.name == "deleteOne" {
                MongoCommand::DeleteOne {
                    collection: coll,
                    filter,
                }
            } else {
                MongoCommand::DeleteMany {
                    collection: coll,
                    filter,
                }
            }
        }
        "createIndex" => {
            arity(c, &call, 1, 2)?;
            MongoCommand::CreateIndex {
                collection: coll,
                keys: doc_arg(c, &call, 0, "index keys")?,
                options: opt_doc_arg(c, &call, 1, "options")?,
            }
        }
        "getIndexes" | "listIndexes" => {
            arity(c, &call, 0, 0)?;
            MongoCommand::ListIndexes { collection: coll }
        }
        "dropIndex" => {
            arity(c, &call, 1, 1)?;
            MongoCommand::DropIndex {
                collection: coll,
                name: string_arg(c, &call, 0, "index name")?,
            }
        }
        "drop" => {
            arity(c, &call, 0, 0)?;
            MongoCommand::DropCollection { collection: coll }
        }
        other => {
            return c.err(
                format!("unknown method `{other}`; supported: {COLLECTION_METHODS}"),
                call.name_span,
            );
        }
    };
    no_chain(c, &call)?;
    Ok(cmd)
}

fn find_chain(c: &mut Cursor, mut cmd: MongoCommand) -> Result<MongoCommand, ShellError> {
    while let Some(call) = parse_chained_call(c)? {
        let MongoCommand::Find {
            collection,
            filter,
            projection,
            sort,
            skip,
            limit,
        } = &mut cmd
        else {
            unreachable!("find_chain only handles Find");
        };
        match call.name {
            "sort" => {
                arity(c, &call, 1, 1)?;
                *sort = Some(doc_arg(c, &call, 0, "sort specification")?);
            }
            "project" | "projection" => {
                arity(c, &call, 1, 1)?;
                *projection = Some(doc_arg(c, &call, 0, "projection")?);
            }
            "skip" => {
                arity(c, &call, 1, 1)?;
                let n = int_arg(c, &call, 0)?;
                *skip = Some(u64::try_from(n).or_else(|_| {
                    c.err("skip() must not be negative", call.args[0].span.clone())
                })?);
            }
            "limit" => {
                arity(c, &call, 1, 1)?;
                *limit = Some(int_arg(c, &call, 0)?);
            }
            "toArray" | "pretty" => arity(c, &call, 0, 0)?,
            "count" | "countDocuments" | "size" | "itcount" => {
                arity(c, &call, 0, 1)?;
                let cmd = MongoCommand::Count {
                    collection: std::mem::take(collection),
                    filter: std::mem::take(filter),
                };
                return ignore_chain(c, cmd, call.name);
            }
            other => {
                return c.err(
                    format!("unsupported cursor method `{other}`; supported: {FIND_CHAIN_METHODS}"),
                    call.name_span,
                );
            }
        }
    }
    Ok(cmd)
}

fn apply_find_options(
    c: &Cursor,
    cmd: &mut MongoCommand,
    opts: &Document,
    span: Range<usize>,
) -> Result<(), ShellError> {
    let MongoCommand::Find {
        projection,
        sort,
        skip,
        limit,
        ..
    } = cmd
    else {
        unreachable!("find options only apply to Find");
    };
    for (k, v) in opts {
        match (k.as_str(), v) {
            ("sort", Bson::Document(d)) => *sort = Some(d.clone()),
            ("projection", Bson::Document(d)) => *projection = Some(d.clone()),
            ("skip", v) if as_integer(v).is_some_and(|n| n >= 0) => {
                *skip = Some(as_integer(v).expect("checked") as u64)
            }
            ("limit", v) if as_integer(v).is_some() => *limit = as_integer(v),
            (k @ ("sort" | "projection" | "skip" | "limit"), v) => {
                return c.err(format!("invalid find option `{k}`: {}", type_name(v)), span);
            }
            (k, _) => {
                return c.err(
                    format!(
                        "unsupported find option `{k}`; supported: sort, projection, skip, limit"
                    ),
                    span,
                );
            }
        }
    }
    Ok(())
}

/// Accepts (and ignores) `.toArray()` / `.pretty()` chained after a command.
fn ignore_chain(
    c: &mut Cursor,
    cmd: MongoCommand,
    after: &str,
) -> Result<MongoCommand, ShellError> {
    while let Some(call) = parse_chained_call(c)? {
        match call.name {
            "toArray" | "pretty" => arity(c, &call, 0, 0)?,
            other => {
                return c.err(
                    format!("`.{other}()` cannot be chained after {after}()"),
                    call.name_span,
                );
            }
        }
    }
    Ok(cmd)
}

fn no_chain(c: &mut Cursor, prev: &Call) -> Result<(), ShellError> {
    if let Some(call) = parse_chained_call(c)? {
        return c.err(
            format!("`.{}()` cannot be chained after {}()", call.name, prev.name),
            call.name_span,
        );
    }
    Ok(())
}

fn arity(c: &Cursor, call: &Call, min: usize, max: usize) -> Result<(), ShellError> {
    let n = call.args.len();
    if n >= min && n <= max {
        return Ok(());
    }
    let expected = match (min, max) {
        (0, 0) => "no arguments".to_string(),
        (a, b) if a == b => format!("{a} argument{}", if a == 1 { "" } else { "s" }),
        (0, b) => format!("at most {b} argument{}", if b == 1 { "" } else { "s" }),
        (a, b) => format!("{a} to {b} arguments"),
    };
    let span = if n > max {
        call.args[max].span.start..call.args[n - 1].span.end
    } else {
        call.span.clone()
    };
    c.err(format!("{}() takes {expected}, got {n}", call.name), span)
}

fn doc_arg(c: &Cursor, call: &Call, i: usize, what: &str) -> Result<Document, ShellError> {
    match opt_doc_arg(c, call, i, what)? {
        Some(d) => Ok(d),
        None => c.err(
            format!("{}() requires a {what} document", call.name),
            call.span.clone(),
        ),
    }
}

/// Optional document argument; `null`/`undefined` count as absent.
fn opt_doc_arg(
    c: &Cursor,
    call: &Call,
    i: usize,
    what: &str,
) -> Result<Option<Document>, ShellError> {
    match call.args.get(i) {
        None => Ok(None),
        Some(Arg {
            value: Bson::Null, ..
        }) => Ok(None),
        Some(Arg {
            value: Bson::Document(d),
            ..
        }) => Ok(Some(d.clone())),
        Some(a) => c.err(
            format!(
                "{}(): {what} must be a document, got {}",
                call.name,
                type_name(&a.value)
            ),
            a.span.clone(),
        ),
    }
}

fn string_arg(c: &Cursor, call: &Call, i: usize, what: &str) -> Result<String, ShellError> {
    match call.args.get(i) {
        Some(Arg {
            value: Bson::String(s),
            ..
        }) => Ok(s.clone()),
        Some(a) => c.err(
            format!(
                "{}(): {what} must be a string, got {}",
                call.name,
                type_name(&a.value)
            ),
            a.span.clone(),
        ),
        None => c.err(
            format!("{}() requires a {what}", call.name),
            call.span.clone(),
        ),
    }
}

fn int_arg(c: &Cursor, call: &Call, i: usize) -> Result<i64, ShellError> {
    let a = &call.args[i];
    as_integer(&a.value).map_or_else(
        || {
            c.err(
                format!(
                    "{}() expects an integer, got {}",
                    call.name,
                    type_name(&a.value)
                ),
                a.span.clone(),
            )
        },
        Ok,
    )
}

fn upsert_option(c: &Cursor, call: &Call, i: usize) -> Result<bool, ShellError> {
    let Some(opts) = opt_doc_arg(c, call, i, "options")? else {
        return Ok(false);
    };
    let span = call.args[i].span.clone();
    let mut upsert = false;
    for (k, v) in &opts {
        match (k.as_str(), v) {
            ("upsert", Bson::Boolean(b)) => upsert = *b,
            ("upsert", v) => {
                return c.err(
                    format!(
                        "option `upsert` must be true or false, got {}",
                        type_name(v)
                    ),
                    span,
                );
            }
            (k, _) => {
                return c.err(
                    format!(
                        "unsupported option `{k}` for {}(); only `upsert` is supported",
                        call.name
                    ),
                    span,
                );
            }
        }
    }
    Ok(upsert)
}

fn docs_from_array(
    c: &Cursor,
    items: Vec<Bson>,
    span: &Range<usize>,
    what: &str,
) -> Result<Vec<Document>, ShellError> {
    items
        .into_iter()
        .enumerate()
        .map(|(i, v)| match v {
            Bson::Document(d) => Ok(d),
            other => c.err(
                format!(
                    "{what} #{} must be a document, got {}",
                    i + 1,
                    type_name(&other)
                ),
                span.clone(),
            ),
        })
        .collect()
}
