//! `ToolCacheStore` integration tests.
//!
//! Each test uses `postgres_test_ctx()`. Container mode gets a fresh database;
//! `PG_TEST_DSN` mode stays idempotent through unique cache ids.

#[path = "../src/test_helpers.rs"]
mod test_helpers;

use chrono::{DateTime, Duration, Utc};
use crabgent_store::{SessionId, ToolCacheEntry, ToolCacheStore};
use crabgent_store_postgres::PostgresStore;
use test_helpers::postgres_test_ctx;
use uuid::Uuid;

fn cache_id(test_name: &str) -> String {
    format!("pg-tool-cache-{test_name}-{}", Uuid::now_v7())
}

fn make_entry(id: String, session_id: &SessionId, expires_at: DateTime<Utc>) -> ToolCacheEntry {
    ToolCacheEntry {
        id,
        session_id: session_id.clone(),
        tool_name: "bash".into(),
        content: "full command output".into(),
        preview: "full command...".into(),
        created_at: Utc::now(),
        expires_at,
    }
}

#[tokio::test]
async fn tool_cache_insert_get_roundtrip() {
    let ctx = postgres_test_ctx().await;
    let store = PostgresStore::from_pool(ctx.pool.clone());
    let session = SessionId::new();
    let entry = make_entry(
        cache_id("roundtrip"),
        &session,
        Utc::now() + Duration::hours(1),
    );

    store
        .tool_cache_store()
        .insert(&entry)
        .await
        .expect("test result");
    let got = store
        .tool_cache_store()
        .get(&entry.id, &session)
        .await
        .expect("test result")
        .expect("cache entry exists");

    assert_eq!(got.id, entry.id);
    assert_eq!(got.session_id, session);
    assert_eq!(got.content, "full command output");
}

#[tokio::test]
async fn tool_cache_get_returns_none_for_expired() {
    let ctx = postgres_test_ctx().await;
    let store = PostgresStore::from_pool(ctx.pool.clone());
    let session = SessionId::new();
    let entry = make_entry(
        cache_id("expired"),
        &session,
        Utc::now() - Duration::minutes(5),
    );

    store
        .tool_cache_store()
        .insert(&entry)
        .await
        .expect("test result");
    let got = store
        .tool_cache_store()
        .get(&entry.id, &session)
        .await
        .expect("test result");

    assert!(got.is_none());
}

#[tokio::test]
async fn tool_cache_insert_idempotent_on_conflict() {
    let ctx = postgres_test_ctx().await;
    let store = PostgresStore::from_pool(ctx.pool.clone());
    let session = SessionId::new();
    let id = cache_id("idempotent");
    let entry = make_entry(id.clone(), &session, Utc::now() + Duration::hours(1));
    let mut shadow = make_entry(id, &session, Utc::now() + Duration::hours(1));
    shadow.content = "shadow output".into();

    store
        .tool_cache_store()
        .insert(&entry)
        .await
        .expect("test result");
    store
        .tool_cache_store()
        .insert(&shadow)
        .await
        .expect("test result");
    let got = store
        .tool_cache_store()
        .get(&entry.id, &session)
        .await
        .expect("test result")
        .expect("original entry exists");

    assert_eq!(got.content, entry.content);
}

#[tokio::test]
async fn tool_cache_cleanup_expired_removes_only_expired() {
    let ctx = postgres_test_ctx().await;
    let store = PostgresStore::from_pool(ctx.pool.clone());
    let session = SessionId::new();
    let past = Utc::now() - Duration::hours(1);
    let future = Utc::now() + Duration::hours(1);
    let expired_a = make_entry(cache_id("expired-a"), &session, past);
    let expired_b = make_entry(cache_id("expired-b"), &session, past);
    let valid = make_entry(cache_id("valid"), &session, future);
    store
        .tool_cache_store()
        .insert(&expired_a)
        .await
        .expect("test result");
    store
        .tool_cache_store()
        .insert(&expired_b)
        .await
        .expect("test result");
    store
        .tool_cache_store()
        .insert(&valid)
        .await
        .expect("test result");

    let removed = store
        .tool_cache_store()
        .cleanup_expired()
        .await
        .expect("test result");

    assert!(removed >= 2);
    assert!(
        store
            .tool_cache_store()
            .get(&expired_a.id, &session)
            .await
            .expect("test result")
            .is_none()
    );
    assert!(
        store
            .tool_cache_store()
            .get(&expired_b.id, &session)
            .await
            .expect("test result")
            .is_none()
    );
    assert!(
        store
            .tool_cache_store()
            .get(&valid.id, &session)
            .await
            .expect("test result")
            .is_some()
    );
}
