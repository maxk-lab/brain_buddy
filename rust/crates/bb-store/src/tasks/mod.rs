//! The Tasks owner group: owns the `tasks`, `sync` and `jobs` schemas and the one unit of
//! work that commits them together (ADR-0031 "task + review atomicity"). Application
//! modules `tasks`, `review`, `sync` and `jobs` all use this module.

pub mod devices;
pub mod items;
pub mod jobs;
pub mod organize;
pub mod review;
pub mod snapshots;
pub mod sync;
pub mod transfers;

use sqlx::{PgPool, PgTransaction};

/// One PostgreSQL transaction spanning the Tasks group's schemas. Dropping it without
/// [`UnitOfWork::commit`] rolls everything back.
pub struct UnitOfWork {
    tx: PgTransaction<'static>,
}

impl UnitOfWork {
    /// Opens the transaction.
    ///
    /// # Errors
    /// Returns an error when no connection can be acquired.
    pub async fn begin(pool: &PgPool) -> Result<Self, sqlx::Error> {
        Ok(Self {
            tx: pool.begin().await?,
        })
    }

    /// The open transaction, for the group's repositories (and `control::mark_applied`).
    pub fn tx(&mut self) -> &mut PgTransaction<'static> {
        &mut self.tx
    }

    /// Commits everything written through [`UnitOfWork::tx`] atomically.
    ///
    /// # Errors
    /// Returns an error when the commit fails.
    pub async fn commit(self) -> Result<(), sqlx::Error> {
        self.tx.commit().await
    }
}
