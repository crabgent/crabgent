#[path = "../src/test_helpers.rs"]
mod test_helpers;

use sqlx::Row;
use test_helpers::postgres_test_ctx;

/// Each test owns a fresh container unless `PG_TEST_DSN` is set; real-server
/// reuse is safe because migrations are idempotent and assertions are schema
/// metadata reads.
#[tokio::test]
async fn migration_run_creates_all_tables() {
    let ctx = postgres_test_ctx().await;

    let tables: Vec<String> = sqlx::query(
        "SELECT table_name \
         FROM information_schema.tables \
         WHERE table_schema = 'public' \
         ORDER BY table_name",
    )
    .fetch_all(&ctx.pool)
    .await
    .expect("list postgres tables")
    .into_iter()
    .map(|row| row.get::<String, _>("table_name"))
    .collect();

    for expected in [
        "sessions",
        "tasks",
        "cron_jobs",
        "tool_cache",
        "memory_docs",
        "session_archives",
    ] {
        assert!(tables.iter().any(|table| table == expected), "{expected}");
    }
}
