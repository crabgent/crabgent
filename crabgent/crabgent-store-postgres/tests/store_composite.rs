//! Composite `Store` integration tests.

#[path = "../src/test_helpers.rs"]
mod test_helpers;

use crabgent_store::{CronStore, MemoryStore, SessionStore, Store, TaskStore, ToolCacheStore};
use crabgent_store_postgres::PostgresStore;
use test_helpers::postgres_test_ctx;

fn assert_session_store(_: &dyn SessionStore) {}
fn assert_memory_store(_: &dyn MemoryStore) {}
fn assert_task_store(_: &dyn TaskStore) {}
fn assert_cron_store(_: &dyn CronStore) {}
fn assert_tool_cache_store(_: &dyn ToolCacheStore) {}

#[tokio::test]
async fn store_composite_provides_all_substores() {
    let ctx = postgres_test_ctx().await;
    let store = PostgresStore::from_pool(ctx.pool.clone());

    assert_session_store(store.session());
    assert_memory_store(store.memory());
    assert_task_store(store.task());
    assert_cron_store(store.cron());
    assert_tool_cache_store(store.tool_cache());
}
