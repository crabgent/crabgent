use crabgent_core::{
    GlobalModelOverrideStore, GlobalReasoningEffortOverrideStore, ModelId, ReasoningEffort,
};
use crabgent_store_sqlite::SqliteStore;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

#[tokio::test]
async fn global_model_override_set_get_round_trip() {
    let store = SqliteStore::open_in_memory().await.expect("open sqlite");

    assert!(
        store
            .get_global_model_override()
            .await
            .expect("test result")
            .is_none()
    );
    store
        .set_global_model_override(&ModelId::new("claude"))
        .await
        .expect("test result");
    assert_eq!(
        store
            .get_global_model_override()
            .await
            .expect("test result"),
        Some(ModelId::new("claude"))
    );
}

#[tokio::test]
async fn global_model_override_set_replaces_existing() {
    let store = SqliteStore::open_in_memory().await.expect("open sqlite");

    store
        .set_global_model_override(&ModelId::new("claude"))
        .await
        .expect("test result");
    store
        .set_global_model_override(&ModelId::new("gpt"))
        .await
        .expect("test result");

    assert_eq!(
        store
            .get_global_model_override()
            .await
            .expect("test result"),
        Some(ModelId::new("gpt"))
    );
}

#[tokio::test]
async fn global_model_override_clear_removes_override() {
    let store = SqliteStore::open_in_memory().await.expect("open sqlite");

    store
        .set_global_model_override(&ModelId::new("claude"))
        .await
        .expect("test result");
    store
        .clear_global_model_override()
        .await
        .expect("test result");

    assert!(
        store
            .get_global_model_override()
            .await
            .expect("test result")
            .is_none()
    );
}

#[tokio::test]
async fn global_reasoning_effort_override_set_get_clear_round_trip() {
    let store = SqliteStore::open_in_memory().await.expect("open sqlite");

    assert!(
        store
            .get_global_reasoning_effort_override()
            .await
            .expect("test result")
            .is_none()
    );
    store
        .set_global_reasoning_effort_override(ReasoningEffort::High)
        .await
        .expect("test result");
    assert_eq!(
        store
            .get_global_reasoning_effort_override()
            .await
            .expect("test result"),
        Some(ReasoningEffort::High)
    );
    store
        .clear_global_reasoning_effort_override()
        .await
        .expect("test result");
    assert!(
        store
            .get_global_reasoning_effort_override()
            .await
            .expect("test result")
            .is_none()
    );
}

#[tokio::test]
async fn global_reasoning_effort_override_rejects_invalid_value() {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            SqliteConnectOptions::new()
                .in_memory(true)
                .shared_cache(true),
        )
        .await
        .expect("open test pool");
    let _store = SqliteStore::from_pool(pool.clone()).await.expect("migrate");

    let err = sqlx::query(
        "INSERT INTO global_reasoning_effort_overrides (singleton, reasoning_effort) \
         VALUES (0, 'extreme')",
    )
    .execute(&pool)
    .await
    .expect_err("invalid effort should violate check");

    assert!(err.to_string().contains("CHECK"));
}
