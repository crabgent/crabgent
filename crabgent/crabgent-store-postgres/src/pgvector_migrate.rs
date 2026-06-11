//! Runtime-dimension migration for `pgvector` memory embeddings.

use crabgent_store::StoreError;
use sqlx::{PgPool, Postgres, Transaction};

const MIGRATION_NAME: &str = "add_pgvector";

/// Stable advisory-lock key for the pgvector bootstrap. Serializes concurrent
/// startups so the table-create + dim-check + apply run as one critical section.
/// `pg_advisory_xact_lock` releases automatically on transaction end.
const PGVECTOR_MIGRATION_LOCK: i64 = 0x6e_65_6f_63_5f_76_65_63; // "neoc_vec"

/// Apply the pgvector migration, guarding against dimension drift.
///
/// The bootstrap table-create, the dimension check, and the schema apply run
/// inside one transaction holding a Postgres advisory lock. Without the lock,
/// two startups could both observe an empty `crabgent_vector_migrations` between
/// their independent checks and both try to apply the schema (TOCTOU).
pub async fn apply_pgvector_migration(pool: &PgPool, dim: usize) -> Result<(), StoreError> {
    if dim == 0 {
        return Err(StoreError::invalid("embedding dimension must be positive"));
    }
    let recorded_dim = i32::try_from(dim).map_err(StoreError::invalid)?;

    let mut tx = pool.begin().await.map_err(StoreError::backend)?;
    // Serialize concurrent bootstraps: blocks until any other startup's
    // transaction commits or rolls back, then releases at this tx's end.
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(PGVECTOR_MIGRATION_LOCK)
        .execute(&mut *tx)
        .await
        .map_err(StoreError::backend)?;

    bootstrap_migration_table(&mut tx).await?;

    if let Some(existing) = current_dim(&mut tx).await? {
        if existing == dim {
            return Ok(());
        }
        return Err(StoreError::backend(format!(
            "embedding dim drift: existing {existing}, requested {dim}"
        )));
    }

    let sql = include_str!("../migrations-post/add_pgvector.sql.tmpl")
        .replace("${EMBEDDING_DIM}", &dim.to_string());
    let sql = strip_comment_lines(&sql);
    apply_migration_sql(&mut tx, recorded_dim, &sql).await?;
    tx.commit().await.map_err(StoreError::backend)?;
    Ok(())
}

/// Apply `sql` (semicolon-separated statements) and record the migration within
/// the caller's transaction. The caller owns commit/rollback, so a failure here
/// leaves no partial schema or migration row once it rolls back.
async fn apply_migration_sql(
    tx: &mut Transaction<'_, Postgres>,
    recorded_dim: i32,
    sql: &str,
) -> Result<(), StoreError> {
    for statement in sql.split(';').map(str::trim).filter(|s| !s.is_empty()) {
        sqlx::query(sqlx::AssertSqlSafe(statement))
            .execute(&mut **tx)
            .await
            .map_err(StoreError::backend)?;
    }
    // ON CONFLICT keeps the insert idempotent even under the advisory lock: a
    // prior committed sentinel (e.g. a re-run after the lock is released) is a
    // no-op instead of a hard primary-key error.
    sqlx::query(
        "INSERT INTO crabgent_vector_migrations (name, dim) VALUES ($1, $2) \
         ON CONFLICT (name) DO NOTHING",
    )
    .bind(MIGRATION_NAME)
    .bind(recorded_dim)
    .execute(&mut **tx)
    .await
    .map_err(StoreError::backend)?;
    Ok(())
}

async fn bootstrap_migration_table(tx: &mut Transaction<'_, Postgres>) -> Result<(), StoreError> {
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS crabgent_vector_migrations (\
         name TEXT PRIMARY KEY, \
         dim INT NOT NULL, \
         applied_at TIMESTAMPTZ NOT NULL DEFAULT NOW())",
    )
    .execute(&mut **tx)
    .await
    .map_err(StoreError::backend)?;
    Ok(())
}

async fn current_dim(tx: &mut Transaction<'_, Postgres>) -> Result<Option<usize>, StoreError> {
    let row: Option<(i32,)> =
        sqlx::query_as("SELECT dim FROM crabgent_vector_migrations WHERE name = $1")
            .bind(MIGRATION_NAME)
            .fetch_optional(&mut **tx)
            .await
            .map_err(StoreError::backend)?;
    row.map(|(dim,)| usize::try_from(dim).map_err(StoreError::backend))
        .transpose()
}

fn strip_comment_lines(sql: &str) -> String {
    sql.lines()
        .filter(|line| !line.trim_start().starts_with("--"))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_helpers::postgres_test_ctx;

    #[tokio::test]
    async fn rollback_leaves_no_partial_state_when_second_statement_invalid() {
        // PostgresStore::open in postgres_test_ctx already ran the real
        // pgvector migration, so crabgent_vector_migrations contains a row for
        // MIGRATION_NAME. The rollback signal we observe is therefore the
        // absence of the probe table created by the first statement: if any
        // statement in apply_migration_sql commits without rolling back, the
        // probe table would survive. apply_migration_sql now runs inside a
        // caller-owned transaction; dropping the tx without commit on error
        // rolls back everything it wrote.
        let ctx = postgres_test_ctx().await;

        let bad_sql = "CREATE TABLE crabgent_rollback_probe (id INT); \
                       NOT VALID SQL HERE;";
        {
            let mut tx = ctx.pool.begin().await.expect("begin tx");
            let result = apply_migration_sql(&mut tx, 8, bad_sql).await;
            assert!(
                result.is_err(),
                "expected error from invalid statement, got {result:?}"
            );
            // Drop tx without commit -> rollback.
        }

        let probe: Option<(String,)> = sqlx::query_as(
            "SELECT table_name FROM information_schema.tables \
             WHERE table_name = 'crabgent_rollback_probe'",
        )
        .fetch_optional(&ctx.pool)
        .await
        .expect("query probe table");
        assert!(
            probe.is_none(),
            "probe table must not exist after transaction rollback"
        );
    }
}
