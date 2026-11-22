//! Database-agnostic core of DB Claw: connection model, result sets,
//! the universal filter language and the Mongo shell-syntax parser.
//! Nothing in here performs I/O, so all of it is unit-testable.

pub mod complete;
pub mod conn_url;
pub mod filter;
pub mod model;
pub mod mongo_refs;
pub mod mongo_shell;
pub mod sql;
pub mod update;
