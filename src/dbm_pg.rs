//! [pg] Postgres's errors, said in one place.
//!
//! An error of the server's is its own message - in the language it was asked for
//! (lc_messages: connect() asks, schema_guard_tokio::set_lc_messages) - with its
//! severity, SQLSTATE, DETAIL, HINT and WHERE; one of the client's says what caused it.
//! tokio_postgres's own Display says neither: "db error", "error connecting to server".
//! A deploy's comes from schema_guard as it came from Postgres (DeployError::Pg), with
//! the statement it refused, and is said here too, by sge's messages - with the line of
//! the statement the server points at, and a ^ under it, as psql says it.

use schema_guard_tokio::{DeployError, DeployStep};

/// A Postgres error in full - see the module
pub(crate) fn pg(e: &tokio_postgres::Error) -> String {
    schema_guard_tokio::pg_error(e)
}

/// What a schema deploy failed on: the statement Postgres refused, and why - or
/// schema_guard's own message
pub(crate) fn deploy(e: DeployError) -> String {
    match e {
        DeployError::Pg { step, sql, file, err } => {
            let source = if file.is_empty() { String::new() } else { tr!("pg.source", file = file) };
            tr!(match step {
                DeployStep::Table => "pg.table",
                DeployStep::Comments => "pg.comments",
                DeployStep::Indexes => "pg.indexes",
                DeployStep::Grants => "pg.grants",
                DeployStep::Data => "pg.data",
                DeployStep::ForeignKeys => "pg.foreign_keys",
            }, sql = sql, source = source, err = schema_guard_tokio::pg_error_in(&err, &sql))
        }
        DeployError::Message(m) => m,
    }
}
