//! Embedded forward-only migrations (`migrations/`), applied only by `bb-server migrate`.

use sqlx::PgPool;
use sqlx::migrate::{MigrateError, Migrator};

/// The migrations embedded at compile time.
pub static MIGRATOR: Migrator = sqlx::migrate!("./migrations");

/// Applies every pending migration. Called by the `migrate` operator subcommand (the Fly
/// `release_command`); never at request time.
///
/// # Errors
/// Returns an error when a migration fails or an applied one was edited.
pub async fn run(pool: &PgPool) -> Result<(), MigrateError> {
    MIGRATOR.run(pool).await
}
