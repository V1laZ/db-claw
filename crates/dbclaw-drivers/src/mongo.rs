//! MongoDB driver built on the official `mongodb` crate.

use std::collections::HashMap;
use std::sync::Mutex as StdMutex;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow, bail};
use bson::{Bson, Document, doc};
use dbclaw_core::filter;
use dbclaw_core::model::{
    ApplySummary, Cell, ColumnMeta, ConnectionConfig, ForeignKey, NewValue, ObjectInfo, ObjectKind, ObjectRef,
    PageRequest, QueryOutput, ResultSet, RowChange, RowCount, RowKey, SortDir, ValueKind,
};
use dbclaw_core::complete::{Catalog, CatalogTable};
use dbclaw_core::mongo_refs;
use dbclaw_core::mongo_shell::{self, MongoCommand, UpdateSpec};
use futures_util::{StreamExt as _, TryStreamExt as _};
use mongodb::options::{ClientOptions, Tls};
use mongodb::results::CollectionType;
use mongodb::{Client, IndexModel};

/// Documents sampled per collection when inferring references.
const REF_SAMPLE: i64 = 20;
/// Most queries one reference inference may issue.
const REF_QUERY_BUDGET: usize = 60;
/// Queries in flight at once.
const REF_CONCURRENCY: usize = 4;
/// Candidate fields examined per collection.
const REF_MAX_FIELDS: usize = 16;
/// Look an id up in every collection only when there are at most this many…
const REF_PROBE_MAX_COLLECTIONS: usize = 20;
/// …and only for this many fields per call.
const REF_PROBE_MAX_FIELDS: usize = 4;
/// How long inferred references and samples are reused.
const REF_CACHE_TTL: Duration = Duration::from_secs(600);

type Cache<T> = StdMutex<HashMap<(String, String), (std::time::Instant, T)>>;

/// The driver's rustls trusts only its bundled Mozilla roots, so servers with
/// a company CA (installed in the OS trust store) fail with `UnknownIssuer`.
/// Unless the URL names its own `tlsCAFile`, point it at a PEM bundle of the
/// OS trust store instead (which includes the public CAs too).
fn trust_system_roots(options: &mut ClientOptions) {
    let Some(Tls::Enabled(tls)) = options.tls.as_mut() else { return };
    if tls.ca_file_path.is_some() {
        return;
    }
    if let Some(path) = system_ca_bundle() {
        tls.ca_file_path = Some(path.clone());
    }
}

/// Writes the OS trust store as PEM once per process; `None` (keep the driver's
/// bundled roots) when the store is empty or the file can't be written.
fn system_ca_bundle() -> Option<&'static PathBuf> {
    static BUNDLE: OnceLock<Option<PathBuf>> = OnceLock::new();
    BUNDLE
        .get_or_init(|| {
            let certs = rustls_native_certs::load_native_certs().certs;
            if certs.is_empty() {
                return None;
            }
            let mut pem = String::new();
            for cert in &certs {
                pem_encode(&mut pem, cert.as_ref());
            }
            let dir = dirs::cache_dir()?.join("DB Claw");
            std::fs::create_dir_all(&dir).ok()?;
            let path = dir.join("system-ca.pem");
            // Write-then-rename so a concurrent instance never reads half a file.
            let tmp = dir.join(format!("system-ca.{}.tmp", std::process::id()));
            std::fs::write(&tmp, pem).ok()?;
            std::fs::rename(&tmp, &path).ok()?;
            Some(path)
        })
        .as_ref()
}

fn pem_encode(out: &mut String, der: &[u8]) {
    use base64::Engine as _;
    let b64 = base64::engine::general_purpose::STANDARD.encode(der);
    out.push_str("-----BEGIN CERTIFICATE-----\n");
    for line in b64.as_bytes().chunks(64) {
        out.push_str(std::str::from_utf8(line).unwrap_or_default());
        out.push('\n');
    }
    out.push_str("-----END CERTIFICATE-----\n");
}

pub struct MongoConnection {
    client: Client,
    default_db: Option<String>,
    /// Set when the URL names a database (path, not `authSource`) and the
    /// connection shouldn't list the others.
    only_db: Option<String>,
    version: String,
    ref_cache: Cache<Vec<ForeignKey>>,
    sample_cache: Cache<Vec<Document>>,
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
        trust_system_roots(&mut options);
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
        let only_db = if config.show_all_databases { None } else { default_db.clone() };
        Ok(MongoConnection {
            client,
            default_db,
            only_db,
            version: format!("MongoDB {version}"),
            ref_cache: StdMutex::new(HashMap::new()),
            sample_cache: StdMutex::new(HashMap::new()),
        })
    }

    pub fn default_database(&self) -> Option<String> {
        self.default_db.clone()
    }

    pub fn server_version(&self) -> String {
        self.version.clone()
    }

    pub async fn list_databases(&self) -> Result<Vec<String>> {
        if let Some(db) = &self.only_db {
            return Ok(vec![db.clone()]);
        }
        // Only databases this user has privileges on: admin-like users would otherwise
        // see every database on the server (MongoDB 4.0.5+).
        match self.client.list_database_names().authorized_databases(true).await {
            Ok(mut names) => {
                // Roles like clusterMonitor count as "privileges" on every database
                // without allowing to read any data; keep the ones the user can read.
                if let Some(readable) = self.readable_databases().await {
                    names.retain(|n| readable.can_read(n));
                }
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

    /// Which databases the logged-in user may read (`find`), from `connectionStatus`.
    /// `None` when the server doesn't say (e.g. authentication is off): don't filter.
    async fn readable_databases(&self) -> Option<ReadableDatabases> {
        let status = self
            .client
            .database("admin")
            .run_command(doc! { "connectionStatus": 1, "showPrivileges": true })
            .await
            .ok()?;
        ReadableDatabases::from_connection_status(&status)
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

    pub async fn count(&self, req: &PageRequest) -> Result<RowCount> {
        let coll = self.collection(&req.object);
        if req.filter.is_none() && req.object.kind == ObjectKind::Collection {
            // Collection metadata: free, and exact on unsharded collections.
            return Ok(RowCount::Exact(coll.estimated_document_count().await?));
        }
        match coll
            .count_documents(Self::filter_doc(req)?)
            .max_time(Duration::from_secs(3))
            .await
        {
            Ok(n) => Ok(RowCount::Exact(n)),
            // MaxTimeMSExpired: too expensive to count.
            Err(e) if matches!(*e.kind, mongodb::error::ErrorKind::Command(ref c) if c.code == 50) => {
                Ok(RowCount::Unknown)
            }
            Err(e) => Err(e.into()),
        }
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

    /// Infer references from and to a collection (MongoDB has no foreign
    /// keys). A guess counts only once a sampled id is found as an `_id` in
    /// the target collection — ObjectIds are globally unique.
    ///
    /// Kept deliberately light for production servers: real collections
    /// only (views would run their pipelines), at most [`REF_QUERY_BUDGET`]
    /// small queries per call, [`REF_CONCURRENCY`] in flight, a 1 s server
    /// time limit each, and results cached for [`REF_CACHE_TTL`].
    pub async fn guess_references(&self, object: &ObjectRef) -> Result<Vec<ForeignKey>> {
        let key = (object.database.clone(), object.name.clone());
        if let Some((at, fks)) = self.ref_cache.lock().unwrap().get(&key)
            && at.elapsed() < REF_CACHE_TTL
        {
            return Ok(fks.clone());
        }

        let db = self.client.database(&object.database);
        let budget = std::sync::atomic::AtomicUsize::new(REF_QUERY_BUDGET);
        let spend = || {
            budget
                .try_update(std::sync::atomic::Ordering::Relaxed, std::sync::atomic::Ordering::Relaxed, |b| b.checked_sub(1))
                .is_ok()
        };
        let limit = Duration::from_millis(1000);

        // Real collections only: sampling a view runs its whole pipeline.
        let specs: Vec<_> = db
            .list_collections()
            .filter(doc! { "type": "collection" })
            .await?
            .try_collect()
            .await?;
        let mut names: Vec<String> =
            specs.into_iter().map(|s| s.name).filter(|n| !n.starts_with("system.")).collect();
        names.sort();

        let sample = |coll: String, n: i64| {
            let c = db.collection::<Document>(&coll);
            let cache = &self.sample_cache;
            let key = (object.database.clone(), coll);
            let allowed = spend;
            async move {
                if let Some((at, docs)) = cache.lock().unwrap().get(&key)
                    && at.elapsed() < REF_CACHE_TTL
                {
                    return docs.clone();
                }
                if !allowed() {
                    return Vec::new();
                }
                let docs: Vec<Document> = match c.find(doc! {}).limit(n).max_time(limit).await {
                    Ok(cursor) => cursor.try_collect().await.unwrap_or_default(),
                    Err(_) => Vec::new(),
                };
                cache.lock().unwrap().insert(key, (std::time::Instant::now(), docs.clone()));
                docs
            }
        };
        let exists = |coll: &str, id: &Bson| {
            let c = db.collection::<Document>(coll);
            let filter = doc! { "_id": id.clone() };
            let allowed = spend();
            async move {
                allowed
                    && c.find_one(filter)
                        .projection(doc! { "_id": 1 })
                        .max_time(limit)
                        .await
                        .is_ok_and(|d| d.is_some())
            }
        };
        let coll_ref = |name: &str| ObjectRef {
            database: object.database.clone(),
            schema: String::new(),
            name: name.to_string(),
            kind: ObjectKind::Collection,
        };
        let fk = |from: &str, field: &str, to: &str| ForeignKey {
            name: format!("{from}.{field} → {to}"),
            from: coll_ref(from),
            from_columns: vec![field.to_string()],
            to: coll_ref(to),
            to_columns: vec!["_id".to_string()],
            inferred: true,
        };

        let mut out = Vec::new();

        // Outgoing: fields of this collection pointing elsewhere.
        let docs = sample(object.name.clone(), REF_SAMPLE).await;
        let mut fallbacks = 0;
        for cand in mongo_refs::candidate_fields(&docs).into_iter().take(REF_MAX_FIELDS) {
            if let Some(target) = &cand.dbref {
                if names.contains(target) {
                    out.push(fk(&object.name, &cand.field, target));
                }
                continue;
            }
            let base = mongo_refs::reference_base(&cand.field);
            let mut target = None;
            for c in names.iter().filter(|c| mongo_refs::collection_matches(&base, c)) {
                if exists(c, &cand.sample).await {
                    target = Some(c.clone());
                    break;
                }
            }
            // Unusual names (createdBy, owner…): look the id up everywhere,
            // but only on small databases and for a few fields.
            if target.is_none() && names.len() <= REF_PROBE_MAX_COLLECTIONS && fallbacks < REF_PROBE_MAX_FIELDS {
                fallbacks += 1;
                let lookups: Vec<_> = names
                    .clone()
                    .into_iter()
                    .map(|c: String| {
                        let found = exists(&c, &cand.sample);
                        async move { (c, found.await) }
                    })
                    .collect();
                let probes = futures_util::stream::iter(lookups).buffer_unordered(REF_CONCURRENCY);
                futures_util::pin_mut!(probes);
                while let Some((c, found)) = probes.next().await {
                    if found {
                        target = Some(c);
                        break;
                    }
                }
            }
            if let Some(t) = target {
                out.push(fk(&object.name, &cand.field, &t));
            }
        }

        // Incoming: fields of other collections whose name points here.
        let others: Vec<String> = names
            .iter()
            .filter(|c| **c != object.name)
            .take(REF_PROBE_MAX_COLLECTIONS)
            .cloned()
            .collect();
        let sampling: Vec<_> = others
            .into_iter()
            .map(|c: String| {
                let s = sample(c.clone(), REF_SAMPLE);
                async move { (c, s.await) }
            })
            .collect();
        let samples: Vec<(String, Vec<Document>)> =
            futures_util::stream::iter(sampling).buffer_unordered(REF_CONCURRENCY).collect().await;
        for (coll, docs) in samples {
            for cand in mongo_refs::candidate_fields(&docs) {
                let points_here = match &cand.dbref {
                    Some(target) => *target == object.name,
                    None => mongo_refs::collection_matches(&mongo_refs::reference_base(&cand.field), &object.name),
                };
                if points_here && exists(&object.name, &cand.sample).await {
                    out.push(fk(&coll, &cand.field, &object.name));
                }
            }
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        self.ref_cache.lock().unwrap().insert(key, (std::time::Instant::now(), out.clone()));
        Ok(out)
    }

    /// Collection names, for autocompletion.
    pub async fn catalog(&self, database: &str) -> Result<Catalog> {
        let mut names = self.client.database(database).list_collection_names().await?;
        names.retain(|n| !n.starts_with("system."));
        names.sort();
        Ok(Catalog {
            tables: names
                .into_iter()
                .map(|name| CatalogTable { name, ..Default::default() })
                .collect(),
        })
    }

    /// Field paths seen in a sample of the collection (`address.city`,
    /// `items.sku`, …), for autocompletion. Uses the shared sample cache.
    pub async fn field_paths(&self, database: &str, collection: &str) -> Result<Vec<(String, ValueKind)>> {
        let key = (database.to_string(), collection.to_string());
        let cached = self
            .sample_cache
            .lock()
            .unwrap()
            .get(&key)
            .filter(|(at, _)| at.elapsed() < REF_CACHE_TTL)
            .map(|(_, d)| d.clone());
        let docs = match cached {
            Some(d) => d,
            None => {
                let c = self.client.database(database).collection::<Document>(collection);
                let docs: Vec<Document> = c
                    .find(doc! {})
                    .limit(REF_SAMPLE)
                    .max_time(Duration::from_millis(1000))
                    .await?
                    .try_collect()
                    .await?;
                self.sample_cache.lock().unwrap().insert(key, (std::time::Instant::now(), docs.clone()));
                docs
            }
        };
        let mut out: Vec<(String, ValueKind)> = Vec::new();
        fn walk(doc: &Document, prefix: &str, depth: usize, out: &mut Vec<(String, ValueKind)>) {
            for (k, v) in doc {
                let path = if prefix.is_empty() { k.clone() } else { format!("{prefix}.{k}") };
                if !out.iter().any(|(p, _)| *p == path) {
                    out.push((path.clone(), bson_kind(v)));
                }
                if depth < 3 {
                    match v {
                        Bson::Document(d) => walk(d, &path, depth + 1, out),
                        Bson::Array(items) => {
                            for d in items.iter().filter_map(Bson::as_document).take(5) {
                                walk(d, &path, depth + 1, out);
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
        for d in &docs {
            walk(d, "", 1, &mut out);
        }
        Ok(out)
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
                    "inserted 1 document".to_string(),
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

pub fn bson_kind(v: &Bson) -> ValueKind {
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

/// Databases a user may read, from `connectionStatus` with `showPrivileges`.
#[derive(Debug, PartialEq)]
pub(crate) enum ReadableDatabases {
    All,
    Some(std::collections::BTreeSet<String>),
}

impl ReadableDatabases {
    pub(crate) fn from_connection_status(status: &Document) -> Option<Self> {
        let auth = status.get_document("authInfo").ok()?;
        // No authenticated user: authentication is off, everything is readable.
        if auth.get_array("authenticatedUsers").map_or(true, |u| u.is_empty()) {
            return None;
        }
        let privileges = auth.get_array("authenticatedUserPrivileges").ok()?;
        let mut dbs = std::collections::BTreeSet::new();
        for p in privileges.iter().filter_map(Bson::as_document) {
            let can_find = p
                .get_array("actions")
                .is_ok_and(|a| a.iter().any(|x| x.as_str() == Some("find")));
            if !can_find {
                continue;
            }
            let Ok(resource) = p.get_document("resource") else { continue };
            if resource.get_bool("anyResource").unwrap_or(false) {
                return Some(Self::All);
            }
            // `find` on a system collection (e.g. clusterMonitor's `system.profile` in
            // every database) doesn't make a database's data readable.
            let collection = resource.get_str("collection").unwrap_or("");
            if collection.starts_with("system.") {
                continue;
            }
            match resource.get_str("db") {
                // `db: ""` = any (non-system) database.
                Ok("") if collection.is_empty() => return Some(Self::All),
                Ok("") => {}
                Ok(db) => {
                    dbs.insert(db.to_string());
                }
                Err(_) => {}
            }
        }
        Some(Self::Some(dbs))
    }

    pub(crate) fn can_read(&self, db: &str) -> bool {
        match self {
            Self::All => true,
            Self::Some(dbs) => dbs.contains(db),
        }
    }
}

#[cfg(test)]
mod readable_tests {
    use super::*;

    fn status(privileges: Bson) -> Document {
        doc! { "authInfo": {
            "authenticatedUsers": [{ "user": "u", "db": "admin" }],
            "authenticatedUserPrivileges": privileges,
        } }
    }

    #[test]
    fn read_role_on_one_database() {
        // read@shop + clusterMonitor (stats on every database, but no `find`)
        let s = status(bson::bson!([
            { "resource": { "db": "shop", "collection": "" }, "actions": ["find", "listCollections"] },
            { "resource": { "db": "", "collection": "" }, "actions": ["dbStats", "collStats"] },
            { "resource": { "db": "", "collection": "system.profile" }, "actions": ["find"] },
            { "resource": { "db": "local", "collection": "" }, "actions": ["find"] },
            { "resource": { "cluster": true }, "actions": ["listDatabases"] },
        ]));
        let r = ReadableDatabases::from_connection_status(&s).unwrap();
        assert!(r.can_read("shop"));
        assert!(r.can_read("local"));
        assert!(!r.can_read("secret"));
        assert!(!r.can_read("admin"));
    }

    #[test]
    fn read_any_database_and_root() {
        let any = status(bson::bson!([{ "resource": { "db": "", "collection": "" }, "actions": ["find"] }]));
        assert_eq!(ReadableDatabases::from_connection_status(&any), Some(ReadableDatabases::All));
        let root = status(bson::bson!([{ "resource": { "anyResource": true }, "actions": ["find", "insert"] }]));
        assert_eq!(ReadableDatabases::from_connection_status(&root), Some(ReadableDatabases::All));
    }

    #[test]
    fn no_authentication_means_no_filter() {
        let s = doc! { "authInfo": { "authenticatedUsers": [], "authenticatedUserPrivileges": [] } };
        assert_eq!(ReadableDatabases::from_connection_status(&s), None);
    }
}
