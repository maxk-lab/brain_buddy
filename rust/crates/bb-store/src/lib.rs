//! sqlx repositories and object storage.
//!
//! One public module per owning application module (research R3, data-model.md
//! "Rules"). A module's queries touch only its own schemas; the `tasks` module owns
//! the `tasks`, `sync` and `jobs` schemas as one Tasks owner group. The single
//! cross-module store call is [`control::mark_applied`]. `tests/schema_ownership.rs`
//! enforces both rules.

pub mod agents;
pub mod control;
pub mod crt;
pub mod flags;
pub mod identity;
pub mod migrate;
pub mod tasks;
pub mod voice;

use sqlx::postgres::PgPoolOptions;
use std::time::Duration;

pub use sqlx::PgPool;

/// How long a request waits for a pooled connection before failing.
const ACQUIRE_TIMEOUT: Duration = Duration::from_secs(10);

/// Builds the shared connection pool. Connections are opened lazily on first use, so
/// this never touches the database; schema changes are applied only by
/// [`migrate::run`] (`bb-server migrate`), never at request time.
///
/// # Errors
/// Returns an error when `database_url` is not a valid PostgreSQL URL.
pub fn connect(database_url: &str, max_connections: u32) -> Result<PgPool, sqlx::Error> {
    PgPoolOptions::new()
        .max_connections(max_connections)
        .acquire_timeout(ACQUIRE_TIMEOUT)
        .connect_lazy(database_url)
}
