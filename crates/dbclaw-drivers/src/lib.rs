//! Database drivers for DB Claw.
//!
//! All I/O runs on a dedicated multi-threaded tokio runtime ([`runtime`]).
//! The UI never blocks: it spawns work with [`spawn`] and awaits the returned
//! `JoinHandle` from its own executor (tokio join handles are executor
//! agnostic futures).

mod mongo;
mod pg;

use std::future::Future;
use std::sync::{Arc, OnceLock};

use anyhow::Result;
use dbclaw_core::model::{
    ApplySummary, ColumnMeta, ConnectionConfig, DbKind, ObjectInfo, ObjectRef, PageRequest,
    QueryOutput, ResultSet, RowChange,
};

pub use mongo::{bson_kind, docs_to_result_set};
pub use pg::PgExecError;

/// The shared I/O runtime.
pub fn runtime() -> &'static tokio::runtime::Runtime {
    static RT: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RT.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(4)
            .thread_name("dbclaw-io")
            .enable_all()
            .build()
            .expect("failed to start the I/O runtime")
    })
}

/// Spawn a future on the I/O runtime.
pub fn spawn<F>(future: F) -> tokio::task::JoinHandle<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    runtime().spawn(future)
}

/// An open connection to a server. Cheap to clone.
#[derive(Clone)]
pub enum Connection {
    Pg(Arc<pg::PgConnection>),
    Mongo(Arc<mongo::MongoConnection>),
}

impl Connection {
    /// Connect and verify the server is reachable.
    pub async fn open(config: ConnectionConfig) -> Result<Connection> {
        Ok(match config.kind {
            DbKind::Postgres => Connection::Pg(Arc::new(pg::PgConnection::open(config).await?)),
            DbKind::Mongo => {
                Connection::Mongo(Arc::new(mongo::MongoConnection::open(config).await?))
            }
        })
    }

    pub fn kind(&self) -> DbKind {
        match self {
            Connection::Pg(_) => DbKind::Postgres,
            Connection::Mongo(_) => DbKind::Mongo,
        }
    }

    /// Database named in the connection URL, if any.
    pub fn default_database(&self) -> Option<String> {
        match self {
            Connection::Pg(c) => Some(c.default_database().to_string()),
            Connection::Mongo(c) => c.default_database(),
        }
    }

    /// One-line server description, e.g. `PostgreSQL 17.2`.
    pub fn server_version(&self) -> String {
        match self {
            Connection::Pg(c) => c.server_version(),
            Connection::Mongo(c) => c.server_version(),
        }
    }

    pub async fn list_databases(&self) -> Result<Vec<String>> {
        match self {
            Connection::Pg(c) => c.list_databases().await,
            Connection::Mongo(c) => c.list_databases().await,
        }
    }

    pub async fn list_objects(&self, database: &str) -> Result<Vec<ObjectInfo>> {
        match self {
            Connection::Pg(c) => c.list_objects(database).await,
            Connection::Mongo(c) => c.list_objects(database).await,
        }
    }

    /// Column metadata for a table. Mongo collections are schemaless, so this
    /// returns an empty list for them (columns are inferred per page).
    pub async fn columns(&self, object: &ObjectRef) -> Result<Vec<ColumnMeta>> {
        match self {
            Connection::Pg(c) => c.columns(object).await,
            Connection::Mongo(_) => Ok(Vec::new()),
        }
    }

    pub async fn fetch_page(&self, req: &PageRequest, columns: &[ColumnMeta]) -> Result<ResultSet> {
        match self {
            Connection::Pg(c) => c.fetch_page(req, columns).await,
            Connection::Mongo(c) => c.fetch_page(req).await,
        }
    }

    pub async fn count(&self, req: &PageRequest, columns: &[ColumnMeta]) -> Result<u64> {
        match self {
            Connection::Pg(c) => c.count(req, columns).await,
            Connection::Mongo(c) => c.count(req).await,
        }
    }

    /// Apply staged grid changes. Postgres applies them in one transaction;
    /// Mongo applies them in order and stops at the first failure.
    pub async fn apply(&self, object: &ObjectRef, changes: Vec<RowChange>) -> Result<ApplySummary> {
        match self {
            Connection::Pg(c) => c.apply(object, changes).await,
            Connection::Mongo(c) => c.apply(object, changes).await,
        }
    }

    /// Open a console session. Postgres sessions get their own server
    /// connection so `BEGIN`/`SET` behave as expected.
    pub async fn open_session(&self, database: &str) -> Result<Session> {
        match self {
            Connection::Pg(c) => Ok(Session::Pg(Arc::new(c.open_session(database).await?))),
            Connection::Mongo(c) => Ok(Session::Mongo(Arc::new(c.open_session(database)))),
        }
    }
}

/// A query console session.
#[derive(Clone)]
pub enum Session {
    Pg(Arc<pg::PgSession>),
    Mongo(Arc<mongo::MongoSession>),
}

impl Session {
    /// Run console input. At most `row_limit` rows are kept per result set.
    /// `default_collection` lets Mongo users type a bare filter like `{a: 1}`.
    pub async fn execute(
        &self,
        text: &str,
        row_limit: usize,
        default_collection: Option<&str>,
    ) -> Result<QueryOutput> {
        match self {
            Session::Pg(s) => s.execute(text, row_limit).await,
            Session::Mongo(s) => s.execute(text, row_limit, default_collection).await,
        }
    }

    /// Ask the server to cancel the running statement (Postgres). For Mongo
    /// the caller simply drops the running task.
    pub async fn cancel(&self) -> Result<()> {
        match self {
            Session::Pg(s) => s.cancel().await,
            Session::Mongo(_) => Ok(()),
        }
    }

    pub fn is_closed(&self) -> bool {
        match self {
            Session::Pg(s) => s.is_closed(),
            Session::Mongo(_) => false,
        }
    }
}

/// Format an error chain into a readable multi-line message.
pub fn error_message(err: &anyhow::Error) -> String {
    let mut msg = err.to_string();
    for cause in err.chain().skip(1) {
        let c = cause.to_string();
        if !msg.contains(&c) {
            msg.push_str(": ");
            msg.push_str(&c);
        }
    }
    msg
}
