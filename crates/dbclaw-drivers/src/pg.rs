//! PostgreSQL driver built on tokio-postgres.
//!
//! Everything goes through the *simple query* protocol: values come back in
//! Postgres' own text format, which supports every type (enums, arrays,
//! ranges, composites, extensions) without per-type decoders and round-trips
//! exactly when the user edits a cell.

use std::collections::HashMap;
use std::str::FromStr;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, anyhow, bail};
use dbclaw_core::filter::{self, SqlColumn};
use dbclaw_core::model::{
    ApplySummary, Cell, ColumnMeta, ConnectionConfig, ForeignKey, NewValue, ObjectInfo, ObjectKind, ObjectRef,
    PageRequest, QueryOutput, ResultSet, RowChange, ValueKind,
};
use dbclaw_core::sql::{self, quote_literal};
use futures_util::{StreamExt as _, TryStreamExt as _};
use postgres_native_tls::MakeTlsConnector;
use tokio_postgres::{AsyncMessage, CancelToken, Client, SimpleQueryMessage, SimpleQueryRow};

/// Max number of pooled "browse" connections per database.
const BROWSE_POOL: usize = 3;

pub struct PgConnection {
    base: tokio_postgres::Config,
    tls: MakeTlsConnector,
    default_db: String,
    version: String,
    databases: StdMutex<HashMap<String, Arc<PgDatabase>>>,
}

struct PgDatabase {
    browse: tokio::sync::Mutex<Vec<Arc<PgClient>>>,
    /// Dedicated connection for applying staged changes in a transaction.
    writer: tokio::sync::Mutex<Option<PgClient>>,
}

pub(crate) struct PgClient {
    client: Client,
    cancel: CancelToken,
    notices: Arc<StdMutex<Vec<String>>>,
}

impl PgConnection {
    pub async fn open(config: ConnectionConfig) -> Result<PgConnection> {
        let (base, verify) = parse_config(&config)?;
        let tls = make_tls(verify)?;
        let default_db = base
            .get_dbname()
            .map(str::to_string)
            .or_else(|| base.get_user().map(str::to_string))
            .unwrap_or_else(|| "postgres".to_string());
        let conn = PgConnection {
            base,
            tls,
            default_db: default_db.clone(),
            version: String::new(),
            databases: StdMutex::new(HashMap::new()),
        };
        let client = conn.browse_client(&default_db).await?;
        let version = simple_scalar(&client.client, "SHOW server_version")
            .await
            .unwrap_or_default();
        Ok(PgConnection { version: format!("PostgreSQL {version}"), ..conn })
    }

    pub fn default_database(&self) -> &str {
        &self.default_db
    }

    pub fn server_version(&self) -> String {
        self.version.clone()
    }

    fn database(&self, name: &str) -> Arc<PgDatabase> {
        let mut dbs = self.databases.lock().unwrap();
        dbs.entry(name.to_string())
            .or_insert_with(|| {
                Arc::new(PgDatabase {
                    browse: tokio::sync::Mutex::new(Vec::new()),
                    writer: tokio::sync::Mutex::new(None),
                })
            })
            .clone()
    }

    async fn connect(&self, database: &str) -> Result<PgClient> {
        let mut cfg = self.base.clone();
        cfg.dbname(database);
        let (client, mut connection) = cfg
            .connect(self.tls.clone())
            .await
            .map_err(|e| anyhow!(format_pg_error(&e)))?;
        let notices = Arc::new(StdMutex::new(Vec::new()));
        let sink = notices.clone();
        tokio::spawn(async move {
            let mut messages = futures_util::stream::poll_fn(move |cx| connection.poll_message(cx));
            while let Some(message) = messages.next().await {
                match message {
                    Ok(AsyncMessage::Notice(n)) => {
                        sink.lock().unwrap().push(format!("{}: {}", n.severity(), n.message()))
                    }
                    Ok(_) => {}
                    Err(_) => break,
                }
            }
        });
        let cancel = client.cancel_token();
        Ok(PgClient { client, cancel, notices })
    }

    /// Pick the least busy pooled connection, opening a new one when all are
    /// busy and the pool isn't full. "Busy" = handed out (Arc strong count).
    async fn browse_client(&self, database: &str) -> Result<Arc<PgClient>> {
        let db = self.database(database);
        let mut pool = db.browse.lock().await;
        pool.retain(|c| !c.client.is_closed());
        let best = pool.iter().min_by_key(|c| Arc::strong_count(c)).cloned();
        match best {
            Some(c) if Arc::strong_count(&c) <= 2 || pool.len() >= BROWSE_POOL => Ok(c),
            _ => {
                let client = Arc::new(self.connect(database).await?);
                pool.push(client.clone());
                Ok(client)
            }
        }
    }

    pub async fn list_databases(&self) -> Result<Vec<String>> {
        let c = self.browse_client(&self.default_db).await?;
        let rows = simple_rows(
            &c.client,
            "SELECT datname FROM pg_database WHERE NOT datistemplate AND datallowconn ORDER BY datname",
        )
        .await?;
        Ok(rows.iter().filter_map(|r| r.get(0).map(str::to_string)).collect())
    }

    pub async fn list_objects(&self, database: &str) -> Result<Vec<ObjectInfo>> {
        let c = self.browse_client(database).await?;
        let rows = simple_rows(
            &c.client,
            "SELECT n.nspname, c.relname, c.relkind, c.reltuples::bigint \
             FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
             WHERE c.relkind IN ('r','p','v','m','f') AND NOT c.relispartition \
               AND n.nspname <> 'information_schema' AND n.nspname NOT LIKE 'pg\\_%' \
             ORDER BY n.nspname = 'public' DESC, n.nspname, c.relname",
        )
        .await?;
        Ok(rows
            .iter()
            .map(|r| {
                let kind = match r.get(2).unwrap_or("r") {
                    "p" => ObjectKind::PartitionedTable,
                    "v" => ObjectKind::View,
                    "m" => ObjectKind::MaterializedView,
                    "f" => ObjectKind::ForeignTable,
                    _ => ObjectKind::Table,
                };
                let estimated_rows =
                    r.get(3).and_then(|s| s.parse::<i64>().ok()).filter(|n| *n >= 0);
                ObjectInfo {
                    object: ObjectRef {
                        database: database.to_string(),
                        schema: r.get(0).unwrap_or_default().to_string(),
                        name: r.get(1).unwrap_or_default().to_string(),
                        kind,
                    },
                    estimated_rows,
                }
            })
            .collect())
    }

    pub async fn columns(&self, object: &ObjectRef) -> Result<Vec<ColumnMeta>> {
        let c = self.browse_client(&object.database).await?;
        let regclass = quote_literal(&sql::qualified_name(object))?;
        let query = format!(
            "SELECT a.attname, format_type(a.atttypid, a.atttypmod), t.typcategory, t.typname, \
                    a.attnotnull, pg_get_expr(d.adbin, d.adrelid), \
                    (a.attidentity = 'a' OR a.attgenerated <> ''), \
                    EXISTS (SELECT 1 FROM pg_constraint p WHERE p.conrelid = a.attrelid \
                            AND p.contype = 'p' AND a.attnum = ANY (p.conkey)) \
             FROM pg_attribute a \
             JOIN pg_type t ON t.oid = a.atttypid \
             LEFT JOIN pg_attrdef d ON d.adrelid = a.attrelid AND d.adnum = a.attnum \
             WHERE a.attrelid = {regclass}::regclass AND a.attnum > 0 AND NOT a.attisdropped \
             ORDER BY a.attnum"
        );
        let rows = simple_rows(&c.client, &query).await?;
        Ok(rows
            .iter()
            .map(|r| {
                let type_name = r.get(1).unwrap_or_default().to_string();
                let kind = ValueKind::from_pg_category(
                    r.get(2).unwrap_or_default(),
                    r.get(3).unwrap_or_default(),
                );
                ColumnMeta {
                    name: r.get(0).unwrap_or_default().to_string(),
                    type_name,
                    kind,
                    nullable: r.get(4) != Some("t"),
                    default: r.get(5).map(str::to_string),
                    generated: r.get(6) == Some("t"),
                    is_pk: r.get(7) == Some("t"),
                    references: None,
                }
            })
            .collect())
    }

    /// Foreign keys from this table (outgoing) and to it (incoming).
    pub async fn foreign_keys(&self, object: &ObjectRef) -> Result<Vec<ForeignKey>> {
        let c = self.browse_client(&object.database).await?;
        let regclass = quote_literal(&sql::qualified_name(object))?;
        // Column lists as JSON arrays, in key order (composite keys).
        let cols = |rel: &str, keys: &str| {
            format!(
                "(SELECT json_agg(a.attname ORDER BY k.ord) FROM unnest(c.{keys}) WITH ORDINALITY k(attnum, ord) \
                 JOIN pg_attribute a ON a.attrelid = c.{rel} AND a.attnum = k.attnum)"
            )
        };
        let query = format!(
            "SELECT c.conname, sn.nspname, s.relname, s.relkind, {}, tn.nspname, t.relname, t.relkind, {} \
             FROM pg_constraint c \
             JOIN pg_class s ON s.oid = c.conrelid JOIN pg_namespace sn ON sn.oid = s.relnamespace \
             JOIN pg_class t ON t.oid = c.confrelid JOIN pg_namespace tn ON tn.oid = t.relnamespace \
             WHERE c.contype = 'f' AND (c.conrelid = {regclass}::regclass OR c.confrelid = {regclass}::regclass) \
             ORDER BY s.relname, c.conname",
            cols("conrelid", "conkey"),
            cols("confrelid", "confkey"),
        );
        let rows = simple_rows(&c.client, &query).await?;
        let obj = |schema: Option<&str>, name: Option<&str>, kind: Option<&str>| ObjectRef {
            database: object.database.clone(),
            schema: schema.unwrap_or_default().to_string(),
            name: name.unwrap_or_default().to_string(),
            kind: match kind {
                Some("p") => ObjectKind::PartitionedTable,
                Some("f") => ObjectKind::ForeignTable,
                _ => ObjectKind::Table,
            },
        };
        let names = |json: Option<&str>| -> Vec<String> {
            json.and_then(|j| serde_json::from_str(j).ok()).unwrap_or_default()
        };
        Ok(rows
            .iter()
            .map(|r| ForeignKey {
                name: r.get(0).unwrap_or_default().to_string(),
                from: obj(r.get(1), r.get(2), r.get(3)),
                from_columns: names(r.get(4)),
                to: obj(r.get(5), r.get(6), r.get(7)),
                to_columns: names(r.get(8)),
            })
            .filter(|fk| !fk.from_columns.is_empty() && fk.from_columns.len() == fk.to_columns.len())
            .collect())
    }

    fn where_sql(req: &PageRequest, columns: &[ColumnMeta]) -> Result<Option<String>> {
        match &req.filter {
            None => Ok(None),
            Some(expr) => {
                let cols: Vec<SqlColumn> = columns
                    .iter()
                    .map(|c| SqlColumn { name: c.name.clone(), data_type: c.type_name.clone() })
                    .collect();
                Ok(Some(filter::to_sql(expr, &cols).map_err(|e| anyhow!(e.message))?))
            }
        }
    }

    pub async fn fetch_page(&self, req: &PageRequest, columns: &[ColumnMeta]) -> Result<ResultSet> {
        let c = self.browse_client(&req.object.database).await?;
        let where_sql = Self::where_sql(req, columns)?;
        let has_pk = columns.iter().any(|c| c.is_pk);
        let with_ctid = !has_pk
            && matches!(req.object.kind, ObjectKind::Table | ObjectKind::MaterializedView);
        let query = sql::select_page(
            &req.object,
            columns,
            where_sql.as_deref(),
            &req.sort,
            with_ctid,
            req.offset,
            req.limit,
        );
        let messages = c
            .client
            .simple_query(&query)
            .await
            .map_err(|e| anyhow!(format_pg_error(&e)))?;
        let kinds: Vec<ValueKind> = columns.iter().map(|c| c.kind).collect();
        let mut rows = Vec::with_capacity(req.limit as usize);
        let mut ctids = with_ctid.then(Vec::new);
        for m in messages {
            if let SimpleQueryMessage::Row(r) = m {
                rows.push(decode_row(&r, &kinds, columns.len()));
                if let Some(ctids) = ctids.as_mut() {
                    ctids.push(r.get(columns.len()).unwrap_or_default().to_string());
                }
            }
        }
        Ok(ResultSet { columns: columns.to_vec(), rows, ctids, ..Default::default() })
    }

    pub async fn count(&self, req: &PageRequest, columns: &[ColumnMeta]) -> Result<u64> {
        let c = self.browse_client(&req.object.database).await?;
        let where_sql = Self::where_sql(req, columns)?;
        let query = sql::count_rows(&req.object, where_sql.as_deref());
        let v = simple_scalar(&c.client, &query).await?;
        Ok(v.parse().unwrap_or(0))
    }

    pub async fn apply(&self, object: &ObjectRef, changes: Vec<RowChange>) -> Result<ApplySummary> {
        let statements = changes
            .iter()
            .map(|ch| sql::change_statement(object, ch).map(|s| (s, ch)))
            .collect::<Result<Vec<_>, _>>()?;

        let db = self.database(&object.database);
        let mut writer = db.writer.lock().await;
        if writer.as_ref().is_none_or(|w| w.client.is_closed()) {
            *writer = Some(self.connect(&object.database).await?);
        }
        let w = writer.as_ref().unwrap();
        w.client.simple_query("BEGIN").await.map_err(|e| anyhow!(format_pg_error(&e)))?;
        let mut summary = ApplySummary::default();
        let result: Result<()> = async {
            for (stmt, change) in &statements {
                let msgs = w
                    .client
                    .simple_query(stmt)
                    .await
                    .map_err(|e| anyhow!("{}\n\nStatement: {stmt}", format_pg_error(&e)))?;
                let affected = msgs
                    .iter()
                    .find_map(|m| match m {
                        SimpleQueryMessage::CommandComplete(n) => Some(*n),
                        _ => None,
                    })
                    .unwrap_or(0);
                match change {
                    RowChange::Insert { .. } => summary.inserted += affected,
                    RowChange::Update { .. } | RowChange::Delete { .. } => {
                        if affected != 1 {
                            bail!(
                                "expected to change exactly 1 row but {affected} matched — the row \
                                 may have been modified or deleted by someone else. Nothing was \
                                 saved.\n\nStatement: {stmt}"
                            );
                        }
                        if matches!(change, RowChange::Delete { .. }) {
                            summary.deleted += 1;
                        } else {
                            summary.updated += 1;
                        }
                    }
                    _ => {}
                }
            }
            Ok(())
        }
        .await;
        match result {
            Ok(()) => {
                w.client.simple_query("COMMIT").await.map_err(|e| anyhow!(format_pg_error(&e)))?;
                Ok(summary)
            }
            Err(e) => {
                let _ = w.client.simple_query("ROLLBACK").await;
                Err(e)
            }
        }
    }

    pub async fn open_session(&self, database: &str) -> Result<PgSession> {
        let client = self.connect(database).await?;
        Ok(PgSession { client, tls: self.tls.clone() })
    }
}

pub struct PgSession {
    client: PgClient,
    tls: MakeTlsConnector,
}

impl PgSession {
    pub fn is_closed(&self) -> bool {
        self.client.client.is_closed()
    }

    pub async fn cancel(&self) -> Result<()> {
        self.client.cancel.cancel_query(self.tls.clone()).await?;
        Ok(())
    }

    pub async fn execute(&self, text: &str, row_limit: usize) -> Result<QueryOutput> {
        let start = Instant::now();
        let client = &self.client.client;
        let statements = sql::split_statements(text);
        if statements.is_empty() {
            bail!("nothing to run");
        }

        // For a single statement, ask the server for the result column types
        // (Parse/Describe only, nothing is executed) so the grid can align
        // numbers and label columns with their types.
        let mut described: Option<Vec<(String, String)>> = None;
        if statements.len() == 1
            && let Ok(stmt) = client.prepare(&text[statements[0].clone()]).await {
                described = Some(
                    stmt.columns()
                        .iter()
                        .map(|c| (c.name().to_string(), c.type_().name().to_string()))
                        .collect(),
                );
            }

        let stream = client
            .simple_query_raw(text)
            .await
            .map_err(|e| anyhow!(format_pg_error(&e)))?;
        futures_util::pin_mut!(stream);

        let mut out = QueryOutput::default();
        let mut current: Option<ResultSet> = None;
        let mut kinds: Vec<ValueKind> = Vec::new();
        let mut statement_no = 0usize;
        loop {
            let msg = match stream.try_next().await {
                Ok(Some(m)) => m,
                Ok(None) => break,
                Err(e) => {
                    let mut msg = format_pg_error(&e);
                    if statements.len() > 1 {
                        msg = format!("Statement {}: {msg}", statement_no + 1);
                    }
                    // Keep what already completed, then report the error.
                    out.notices.extend(self.client.notices.lock().unwrap().drain(..));
                    out.elapsed = start.elapsed();
                    return Err(anyhow!(PgExecError { message: msg, partial: out }));
                }
            };
            match msg {
                SimpleQueryMessage::RowDescription(cols) => {
                    let columns: Vec<ColumnMeta> = cols
                        .iter()
                        .enumerate()
                        .map(|(i, c)| {
                            let type_name = described
                                .as_ref()
                                .filter(|d| d.len() == cols.len())
                                .map(|d| d[i].1.clone())
                                .unwrap_or_default();
                            ColumnMeta {
                                name: c.name().to_string(),
                                kind: ValueKind::from_pg_type_name(&type_name),
                                type_name,
                                nullable: true,
                                ..Default::default()
                            }
                        })
                        .collect();
                    kinds = columns.iter().map(|c| c.kind).collect();
                    current = Some(ResultSet { columns, ..Default::default() });
                }
                SimpleQueryMessage::Row(row) => {
                    if let Some(set) = current.as_mut() {
                        if set.rows.len() < row_limit {
                            set.rows.push(decode_row(&row, &kinds, kinds.len()));
                        } else {
                            set.truncated = true;
                        }
                    }
                }
                SimpleQueryMessage::CommandComplete(n) => {
                    statement_no += 1;
                    let mut set = current.take().unwrap_or_default();
                    set.affected = Some(n);
                    if set.columns.is_empty() {
                        set.status = Some(format!("{n} row(s) affected"));
                    } else {
                        set.status = Some(if set.truncated {
                            format!("{n} rows (showing first {row_limit})")
                        } else {
                            format!("{n} rows")
                        });
                    }
                    out.sets.push(set);
                }
                _ => {}
            }
        }
        out.notices.extend(self.client.notices.lock().unwrap().drain(..));
        out.elapsed = start.elapsed();
        Ok(out)
    }
}

/// Error from a console run that still carries the results of the
/// statements that completed before the failure.
#[derive(Debug)]
pub struct PgExecError {
    pub message: String,
    pub partial: QueryOutput,
}

impl std::fmt::Display for PgExecError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for PgExecError {}

fn decode_row(row: &SimpleQueryRow, kinds: &[ValueKind], n: usize) -> Vec<Cell> {
    (0..n)
        .map(|i| match row.get(i) {
            None => Cell::Null,
            Some(v) => {
                let kind = kinds.get(i).copied().unwrap_or_default();
                if kind == ValueKind::Bool {
                    match v {
                        "t" => return Cell::Value(Arc::from("true"), kind),
                        "f" => return Cell::Value(Arc::from("false"), kind),
                        _ => {}
                    }
                }
                Cell::Value(Arc::from(v), kind)
            }
        })
        .collect()
}

async fn simple_rows(client: &Client, query: &str) -> Result<Vec<SimpleQueryRow>> {
    let msgs = client.simple_query(query).await.map_err(|e| anyhow!(format_pg_error(&e)))?;
    Ok(msgs
        .into_iter()
        .filter_map(|m| match m {
            SimpleQueryMessage::Row(r) => Some(r),
            _ => None,
        })
        .collect())
}

async fn simple_scalar(client: &Client, query: &str) -> Result<String> {
    let rows = simple_rows(client, query).await?;
    rows.first()
        .and_then(|r| r.get(0).map(str::to_string))
        .context("query returned no rows")
}

/// Parse the connection URL. `sslmode=verify-ca|verify-full` (which
/// tokio-postgres doesn't know) become `require` with certificate checks on;
/// plain `prefer`/`require` don't verify certificates, like libpq.
fn parse_config(config: &ConnectionConfig) -> Result<(tokio_postgres::Config, bool)> {
    let mut url = config.url.trim().to_string();
    let mut verify = false;
    for mode in ["verify-full", "verify-ca"] {
        let needle = format!("sslmode={mode}");
        if url.contains(&needle) {
            url = url.replace(&needle, "sslmode=require");
            verify = true;
        }
    }
    let mut cfg = tokio_postgres::Config::from_str(&url)
        .map_err(|e| anyhow!("invalid connection URL: {e}"))?;
    if let Some(pw) = config.password.as_deref().filter(|p| !p.is_empty()) {
        cfg.password(pw);
    }
    cfg.application_name("DB Claw");
    if cfg.get_connect_timeout().is_none() {
        cfg.connect_timeout(Duration::from_secs(10));
    }
    if cfg.get_user().is_none()
        && let Ok(user) = std::env::var("USER") {
            cfg.user(&user);
        }
    Ok((cfg, verify))
}

fn make_tls(verify: bool) -> Result<MakeTlsConnector> {
    let connector = native_tls::TlsConnector::builder()
        .danger_accept_invalid_certs(!verify)
        .danger_accept_invalid_hostnames(!verify)
        .build()?;
    Ok(MakeTlsConnector::new(connector))
}

/// Render a Postgres error like psql does: severity, message, detail, hint.
pub(crate) fn format_pg_error(err: &tokio_postgres::Error) -> String {
    if let Some(db) = err.as_db_error() {
        let mut s = format!("{}: {}", db.severity(), db.message());
        if let Some(d) = db.detail() {
            s.push_str(&format!("\nDETAIL: {d}"));
        }
        if let Some(h) = db.hint() {
            s.push_str(&format!("\nHINT: {h}"));
        }
        if let Some(tokio_postgres::error::ErrorPosition::Original(pos)) = db.position() {
            s.push_str(&format!("\nPOSITION: {pos}"));
        }
        s
    } else {
        let mut s = err.to_string();
        let mut source = std::error::Error::source(err);
        while let Some(cause) = source {
            let c = cause.to_string();
            if !s.contains(&c) {
                s.push_str(": ");
                s.push_str(&c);
            }
            source = cause.source();
        }
        s
    }
}

#[allow(dead_code)]
fn _assert_send() {
    fn is_send<T: Send + Sync>() {}
    is_send::<PgConnection>();
    is_send::<PgSession>();
    let _ = NewValue::Default;
}
