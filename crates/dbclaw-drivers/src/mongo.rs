//! MongoDB driver built on the official `mongodb` crate.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow, bail};
use bson::{Bson, Document, doc};
use dbclaw_core::filter;
use dbclaw_core::model::{
    ApplySummary, Cell, ColumnMeta, ConnectionConfig, NewValue, ObjectInfo, ObjectKind, ObjectRef,
    PageRequest, QueryOutput, ResultSet, RowChange, RowKey, SortDir, ValueKind,
};
use dbclaw_core::mongo_shell::{self, MongoCommand, UpdateSpec};
use futures_util::TryStreamExt as _;
use mongodb::options::ClientOptions;
use mongodb::results::CollectionType;
use mongodb::{Client, IndexModel};

pub struct MongoConnection {
    client: Client,
    default_db: Option<String>,
    version: String,
}

impl MongoConnection {
    pub async fn open(config: ConnectionConfig) -> Result<MongoConnection> {
        let mut options = ClientOptions::parse(config.url.trim())
            .await
            .map_err(|e| anyhow!("invalid connection URL: {e}"))?;
        if let Some(pw) = config.password.as_deref().filter(|p| !p.is_empty()) {
            match options.credential.as_mut() {
                Some(cred) => cred.password = Some(pw.to_string()),
                None => bail!("a password was given but the URL has no user name"),
            }
        }
        options.app_name = Some("DB Claw".into());
        if options.server_selection_timeout.is_none() {
            options.server_selection_timeout = Some(Duration::from_secs(8));
        }
        if options.connect_timeout.is_none() {
            options.connect_timeout = Some(Duration::from_secs(8));
        }
        let default_db = options.default_database.clone();
        let client = Client::with_options(options)?;
        // Verify connectivity (and credentials) up front.
        let auth_db = default_db.clone().unwrap_or_else(|| "admin".into());
        let info = client.database(&auth_db).run_command(doc! { "buildInfo": 1 }).await?;
        let version = info.get_str("version").unwrap_or("?").to_string();
        Ok(MongoConnection { client, default_db, version: format!("MongoDB {version}") })
    }

    pub fn default_database(&self) -> Option<String> {
        self.default_db.clone()
    }

    pub fn server_version(&self) -> String {
        self.version.clone()
    }

    pub async fn list_databases(&self) -> Result<Vec<String>> {
        match self.client.list_database_names().await {
            Ok(mut names) => {
                names.sort();
                Ok(names)
            }
            // Users without listDatabases privilege still see their own db.
            Err(e) => match &self.default_db {
                Some(db) => Ok(vec![db.clone()]),
                None => Err(e.into()),
            },
        }
    }

    pub async fn list_objects(&self, database: &str) -> Result<Vec<ObjectInfo>> {
        let db = self.client.database(database);
        let mut specs: Vec<_> = db.list_collections().await?.try_collect().await?;
        specs.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(specs
            .into_iter()
            .filter(|s| !s.name.starts_with("system."))
            .map(|s| ObjectInfo {
                object: ObjectRef {
                    database: database.to_string(),
                    schema: String::new(),
                    name: s.name,
                    kind: match s.collection_type {
                        CollectionType::View => ObjectKind::MongoView,
                        _ => ObjectKind::Collection,
                    },
                },
                estimated_rows: None,
            })
            .collect())
    }

    fn collection(&self, object: &ObjectRef) -> mongodb::Collection<Document> {
        self.client.database(&object.database).collection(&object.name)
    }

    fn filter_doc(req: &PageRequest) -> Result<Document> {
        match &req.filter {
            None => Ok(Document::new()),
            Some(expr) => filter::to_mongo(expr).map_err(|e| anyhow!(e.message)),
        }
    }

    pub async fn fetch_page(&self, req: &PageRequest) -> Result<ResultSet> {
        let coll = self.collection(&req.object);
        let filter = Self::filter_doc(req)?;
        let mut sort = Document::new();
        for s in &req.sort {
            sort.insert(s.column.clone(), if s.dir == SortDir::Asc { 1 } else { -1 });
        }
        let mut find = coll.find(filter).skip(req.offset).limit(req.limit as i64);
        if !sort.is_empty() {
            find = find.sort(sort);
        }
        let docs: Vec<Document> = find.await?.try_collect().await?;
        Ok(docs_to_result_set(docs))
    }

    pub async fn count(&self, req: &PageRequest) -> Result<u64> {
        let coll = self.collection(&req.object);
        if req.filter.is_none() && req.object.kind == ObjectKind::Collection {
            return Ok(coll.estimated_document_count().await?);
        }
        Ok(coll
            .count_documents(Self::filter_doc(req)?)
            .max_time(Duration::from_secs(10))
            .await?)
    }

    pub async fn apply(&self, object: &ObjectRef, changes: Vec<RowChange>) -> Result<ApplySummary> {
        let coll = self.collection(object);
        let mut summary = ApplySummary::default();
        let total = changes.len();
        for (i, change) in changes.into_iter().enumerate() {
            let step = async {
                match change {
                    RowChange::Update { key, values } => {
                        let id = id_of(&key)?;
                        let mut set = Document::new();
                        let mut unset = Document::new();
                        for (field, v) in values {
                            match v {
                                NewValue::Bson(b) => {
                                    set.insert(field, b);
                                }
                                NewValue::Unset => {
                                    unset.insert(field, "");
                                }
                                NewValue::Text(Some(s)) => {
                                    set.insert(field, s);
                                }
                                NewValue::Text(None) => {
                                    set.insert(field, Bson::Null);
                                }
                                NewValue::Default => {}
                            }
                        }
                        let mut update = Document::new();
                        if !set.is_empty() {
                            update.insert("$set", set);
                        }
                        if !unset.is_empty() {
                            update.insert("$unset", unset);
                        }
                        let r = coll.update_one(doc! { "_id": id }, update).await?;
                        if r.matched_count != 1 {
                            bail!("document no longer exists");
                        }
                        summary.updated += 1;
                    }
                    RowChange::Replace { key, document } => {
                        let id = id_of(&key)?;
                        let r = coll.replace_one(doc! { "_id": id }, document).await?;
                        if r.matched_count != 1 {
                            bail!("document no longer exists");
                        }
                        summary.updated += 1;
                    }
                    RowChange::InsertDocument { document } => {
                        coll.insert_one(document).await?;
                        summary.inserted += 1;
                    }
                    RowChange::Insert { values } => {
                        let mut d = Document::new();
                        for (field, v) in values {
                            match v {
                                NewValue::Bson(b) => {
                                    d.insert(field, b);
                                }
                                NewValue::Text(Some(s)) => {
                                    d.insert(field, s);
                                }
                                NewValue::Text(None) => {
                                    d.insert(field, Bson::Null);
                                }
                                NewValue::Default | NewValue::Unset => {}
                            }
                        }
                        coll.insert_one(d).await?;
                        summary.inserted += 1;
                    }
                    RowChange::Delete { key } => {
                        let id = id_of(&key)?;
                        let r = coll.delete_one(doc! { "_id": id }).await?;
                        if r.deleted_count != 1 {
                            bail!("document no longer exists");
                        }
                        summary.deleted += 1;
                    }
                }
                Ok(())
            };
            if let Err(e) = step.await {
                let done = summary.to_string();
                bail!(
                    "change {} of {total} failed: {e}\n\nAlready applied: {done}. MongoDB \
                     standalone servers have no multi-document transactions, so earlier \
                     changes were kept.",
                    i + 1
                );
            }
        }
        Ok(summary)
    }

    pub fn open_session(&self, database: &str) -> MongoSession {
        MongoSession { client: self.client.clone(), database: database.to_string() }
    }
}

fn id_of(key: &RowKey) -> Result<Bson> {
    match key {
        RowKey::Id(id) => Ok(id.clone()),
        _ => bail!("documents are identified by _id"),
    }
}

pub struct MongoSession {
    client: Client,
    database: String,
}

impl MongoSession {
    pub async fn execute(
        &self,
        text: &str,
        row_limit: usize,
        default_collection: Option<&str>,
    ) -> Result<QueryOutput> {
        let start = Instant::now();
        let command = mongo_shell::parse_filter_or_command(text, default_collection)
            .map_err(|e| anyhow!("{} (at {}..{})", e.message, e.span.start, e.span.end))?;
        let db = self.client.database(&self.database);
        let coll = |name: &str| db.collection::<Document>(name);
        let limit = row_limit as i64;

        let set = match command {
            MongoCommand::Find { collection, filter, projection, sort, skip, limit: user_limit } => {
                let effective = user_limit.filter(|l| *l > 0).map_or(limit, |l| l.min(limit));
                let c = coll(&collection);
                let mut find = c.find(filter).limit(effective + 1);
                if let Some(p) = projection {
                    find = find.projection(p);
                }
                if let Some(s) = sort {
                    find = find.sort(s);
                }
                if let Some(s) = skip {
                    find = find.skip(s);
                }
                let mut docs: Vec<Document> = find.await?.try_collect().await?;
                let truncated = docs.len() as i64 > effective;
                docs.truncate(effective as usize);
                let mut set = docs_to_result_set(docs);
                set.truncated = truncated && user_limit.is_none_or(|l| l > limit);
                set.status = Some(doc_status(set.rows.len(), set.truncated));
                set
            }
            MongoCommand::FindOne { collection, filter, projection } => {
                let c = coll(&collection);
                let mut find = c.find_one(filter);
                if let Some(p) = projection {
                    find = find.projection(p);
                }
                let docs: Vec<Document> = find.await?.into_iter().collect();
                let mut set = docs_to_result_set(docs);
                set.status = Some(doc_status(set.rows.len(), false));
                set
            }
            MongoCommand::Aggregate { collection, pipeline } => {
                let cursor = coll(&collection).aggregate(pipeline).await?;
                let mut docs = Vec::new();
                let mut truncated = false;
                futures_util::pin_mut!(cursor);
                while let Some(d) = cursor.try_next().await? {
                    if docs.len() >= row_limit {
                        truncated = true;
                        break;
                    }
                    docs.push(d);
                }
                let mut set = docs_to_result_set(docs);
                set.truncated = truncated;
                set.status = Some(doc_status(set.rows.len(), truncated));
                set
            }
            MongoCommand::Count { collection, filter } => {
                let n = coll(&collection).count_documents(filter).await?;
                scalar_set("count", Bson::Int64(n as i64))
            }
            MongoCommand::EstimatedCount { collection } => {
                let n = coll(&collection).estimated_document_count().await?;
                scalar_set("count", Bson::Int64(n as i64))
            }
            MongoCommand::Distinct { collection, field, filter } => {
                let values = coll(&collection).distinct(&field, filter).await?;
                let docs = values.into_iter().map(|v| doc! { field.clone(): v }).collect();
                let mut set = docs_to_result_set(docs);
                set.status = Some(format!("{} distinct values", set.rows.len()));
                set
            }
            MongoCommand::InsertOne { collection, document } => {
                let r = coll(&collection).insert_one(document).await?;
                status_set(
                    format!("inserted 1 document"),
                    vec![doc! { "insertedId": r.inserted_id }],
                )
            }
            MongoCommand::InsertMany { collection, documents } => {
                let r = coll(&collection).insert_many(documents).await?;
                let mut ids: Vec<(usize, Bson)> = r.inserted_ids.into_iter().collect();
                ids.sort_by_key(|(i, _)| *i);
                let n = ids.len();
                status_set(
                    format!("inserted {n} documents"),
                    ids.into_iter().map(|(_, id)| doc! { "insertedId": id }).collect(),
                )
            }
            cmd @ (MongoCommand::UpdateOne { .. } | MongoCommand::UpdateMany { .. }) => {
                let (collection, filter, update, upsert, many) = match cmd {
                    MongoCommand::UpdateOne { collection, filter, update, upsert } => {
                        (collection, filter, update, upsert, false)
                    }
                    MongoCommand::UpdateMany { collection, filter, update, upsert } => {
                        (collection, filter, update, upsert, true)
                    }
                    _ => unreachable!(),
                };
                let c = coll(&collection);
                let r = match (update, many) {
                    (UpdateSpec::Document(u), false) => c.update_one(filter, u).upsert(upsert).await?,
                    (UpdateSpec::Pipeline(p), false) => c.update_one(filter, p).upsert(upsert).await?,
                    (UpdateSpec::Document(u), true) => c.update_many(filter, u).upsert(upsert).await?,
                    (UpdateSpec::Pipeline(p), true) => c.update_many(filter, p).upsert(upsert).await?,
                };
                let mut d = doc! {
                    "matched": r.matched_count as i64,
                    "modified": r.modified_count as i64,
                };
                if let Some(id) = r.upserted_id {
                    d.insert("upsertedId", id);
                }
                status_set(
                    format!("matched {}, modified {}", r.matched_count, r.modified_count),
                    vec![d],
                )
            }
            MongoCommand::ReplaceOne { collection, filter, replacement, upsert } => {
                let r = coll(&collection).replace_one(filter, replacement).upsert(upsert).await?;
                status_set(
                    format!("matched {}, modified {}", r.matched_count, r.modified_count),
                    vec![doc! { "matched": r.matched_count as i64, "modified": r.modified_count as i64 }],
                )
            }
            MongoCommand::DeleteOne { collection, filter } => {
                let r = coll(&collection).delete_one(filter).await?;
                status_set(format!("deleted {}", r.deleted_count), vec![])
            }
            MongoCommand::DeleteMany { collection, filter } => {
                let r = coll(&collection).delete_many(filter).await?;
                status_set(format!("deleted {}", r.deleted_count), vec![])
            }
            MongoCommand::CreateIndex { collection, keys, options } => {
                let opts = match options {
                    Some(o) => Some(bson::from_document(o).map_err(|e| anyhow!("index options: {e}"))?),
                    None => None,
                };
                let model = IndexModel::builder().keys(keys).options(opts).build();
                let r = coll(&collection).create_index(model).await?;
                status_set(format!("created index {}", r.index_name), vec![])
            }
            MongoCommand::ListIndexes { collection } => {
                let r = db.run_command(doc! { "listIndexes": collection }).await?;
                let docs = r
                    .get_document("cursor")
                    .ok()
                    .and_then(|c| c.get_array("firstBatch").ok())
                    .map(|a| a.iter().filter_map(|b| b.as_document().cloned()).collect())
                    .unwrap_or_default();
                let mut set = docs_to_result_set(docs);
                set.status = Some(format!("{} indexes", set.rows.len()));
                set
            }
            MongoCommand::DropIndex { collection, name } => {
                coll(&collection).drop_index(&name).await?;
                status_set(format!("dropped index {name}"), vec![])
            }
            MongoCommand::DropCollection { collection } => {
                coll(&collection).drop().await?;
                status_set(format!("dropped collection {collection}"), vec![])
            }
            MongoCommand::CreateCollection { name } => {
                db.create_collection(&name).await?;
                status_set(format!("created collection {name}"), vec![])
            }
            MongoCommand::RunCommand { command, admin } => {
                let target = if admin { self.client.database("admin") } else { db.clone() };
                let r = target.run_command(command).await?;
                let mut set = docs_to_result_set(vec![r]);
                set.status = Some("ok".into());
                set
            }
            MongoCommand::ShowCollections => {
                let mut names = db.list_collection_names().await?;
                names.sort();
                let docs = names.into_iter().map(|n| doc! { "collection": n }).collect();
                let mut set = docs_to_result_set(docs);
                set.status = Some(format!("{} collections", set.rows.len()));
                set
            }
            MongoCommand::ShowDatabases => {
                let dbs = self.client.list_databases().await?;
                let docs = dbs
                    .into_iter()
                    .map(|d| doc! { "name": d.name, "sizeOnDisk": d.size_on_disk as i64, "empty": d.empty })
                    .collect();
                let mut set = docs_to_result_set(docs);
                set.status = Some(format!("{} databases", set.rows.len()));
                set
            }
            #[allow(unreachable_patterns)]
            other => bail!("unsupported command: {other:?}"),
        };
        Ok(QueryOutput { sets: vec![set], notices: vec![], elapsed: start.elapsed() })
    }
}

fn doc_status(n: usize, truncated: bool) -> String {
    if truncated {
        format!("{n} documents (limit reached)")
    } else if n == 1 {
        "1 document".into()
    } else {
        format!("{n} documents")
    }
}

fn scalar_set(name: &str, v: Bson) -> ResultSet {
    let mut set = docs_to_result_set(vec![doc! { name: v.clone() }]);
    set.status = Some(format!("{name}: {}", mongo_shell::format_cell(&v)));
    set
}

fn status_set(status: String, docs: Vec<Document>) -> ResultSet {
    let mut set = docs_to_result_set(docs);
    set.status = Some(status);
    set
}

pub(crate) fn bson_kind(v: &Bson) -> ValueKind {
    match v {
        Bson::Double(_) | Bson::Int32(_) | Bson::Int64(_) | Bson::Decimal128(_) => {
            ValueKind::Number
        }
        Bson::Boolean(_) => ValueKind::Bool,
        Bson::DateTime(_) | Bson::Timestamp(_) => ValueKind::DateTime,
        Bson::Document(_) => ValueKind::Json,
        Bson::Array(_) => ValueKind::Array,
        Bson::ObjectId(_) => ValueKind::Uuid,
        Bson::Binary(_) => ValueKind::Binary,
        Bson::String(_) | Bson::Symbol(_) => ValueKind::Text,
        _ => ValueKind::Other,
    }
}

fn bson_type_name(v: &Bson) -> &'static str {
    match v {
        Bson::Double(_) => "double",
        Bson::String(_) => "string",
        Bson::Array(_) => "array",
        Bson::Document(_) => "object",
        Bson::Boolean(_) => "bool",
        Bson::Null => "null",
        Bson::RegularExpression(_) => "regex",
        Bson::JavaScriptCode(_) | Bson::JavaScriptCodeWithScope(_) => "javascript",
        Bson::Int32(_) => "int",
        Bson::Int64(_) => "long",
        Bson::Timestamp(_) => "timestamp",
        Bson::Binary(_) => "binData",
        Bson::ObjectId(_) => "objectId",
        Bson::DateTime(_) => "date",
        Bson::Symbol(_) => "symbol",
        Bson::Decimal128(_) => "decimal",
        Bson::Undefined => "undefined",
        Bson::MaxKey => "maxKey",
        Bson::MinKey => "minKey",
        Bson::DbPointer(_) => "dbPointer",
    }
}

/// Flatten documents into grid rows: columns are the union of top-level
/// fields in first-seen order (`_id` first); absent fields are `Missing`.
pub fn docs_to_result_set(docs: Vec<Document>) -> ResultSet {
    let mut index: HashMap<String, usize> = HashMap::new();
    let mut columns: Vec<ColumnMeta> = Vec::new();
    let mut types: Vec<Vec<&'static str>> = Vec::new();
    let mut add_col = |name: &str, columns: &mut Vec<ColumnMeta>, types: &mut Vec<Vec<&'static str>>| {
        if let Some(i) = index.get(name) {
            return *i;
        }
        let i = columns.len();
        index.insert(name.to_string(), i);
        columns.push(ColumnMeta {
            name: name.to_string(),
            is_pk: name == "_id",
            nullable: true,
            ..Default::default()
        });
        types.push(Vec::new());
        i
    };
    if docs.iter().any(|d| d.contains_key("_id")) {
        add_col("_id", &mut columns, &mut types);
    }
    let mut positions: Vec<Vec<(usize, &Bson)>> = Vec::with_capacity(docs.len());
    for d in &docs {
        let mut row = Vec::with_capacity(d.len());
        for (k, v) in d {
            let i = add_col(k, &mut columns, &mut types);
            let t = bson_type_name(v);
            if !types[i].contains(&t) {
                types[i].push(t);
            }
            row.push((i, v));
        }
        positions.push(row);
    }
    let rows: Vec<Vec<Cell>> = positions
        .into_iter()
        .map(|fields| {
            let mut row = vec![Cell::Missing; columns.len()];
            for (i, v) in fields {
                row[i] = match v {
                    Bson::Null | Bson::Undefined => Cell::Null,
                    v => Cell::Value(Arc::from(mongo_shell::format_cell(v)), bson_kind(v)),
                };
            }
            row
        })
        .collect();
    for (c, t) in columns.iter_mut().zip(types) {
        let non_null: Vec<&str> = t.iter().copied().filter(|t| *t != "null").collect();
        c.kind = match non_null.as_slice() {
            ["double" | "int" | "long" | "decimal", ..]
                if non_null.iter().all(|t| matches!(*t, "double" | "int" | "long" | "decimal")) =>
            {
                ValueKind::Number
            }
            ["bool"] => ValueKind::Bool,
            ["date"] => ValueKind::DateTime,
            ["objectId"] => ValueKind::Uuid,
            _ => ValueKind::Text,
        };
        c.type_name = t.join(" | ");
    }
    ResultSet { columns, rows, documents: Some(docs), ..Default::default() }
}
