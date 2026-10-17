//! The `control` schema repositories (module `control`); queries here touch only that schema.

use sqlx::PgTransaction;

/// Records that ledger entry `seq` has been applied, inside the caller's own transaction so
/// the entry's effect and its applied row commit together. Idempotent. This is the only
/// store call another `bb_store` module may make (data-model.md "Control exception").
///
/// # Errors
/// Returns an error when the insert fails.
pub async fn mark_applied(tx: &mut PgTransaction<'_>, seq: i64) -> Result<(), sqlx::Error> {
    sqlx::query!(
        "INSERT INTO control.control_applied (seq) VALUES ($1) ON CONFLICT (seq) DO NOTHING",
        seq
    )
    .execute(&mut **tx)
    .await?;
    Ok(())
}
