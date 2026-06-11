#[path = "../src/test_helpers.rs"]
mod test_helpers;

use crabgent_core::{
    GlobalModelOverrideStore, GlobalReasoningEffortOverrideStore, ModelId, ReasoningEffort,
};
use crabgent_store_postgres::PostgresStore;
use test_helpers::postgres_test_ctx;

#[tokio::test]
async fn global_model_override_set_get_round_trip() {
    let ctx = postgres_test_ctx().await;
    let store = PostgresStore::from_pool(ctx.pool.clone());

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
    let ctx = postgres_test_ctx().await;
    let store = PostgresStore::from_pool(ctx.pool.clone());

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
    let ctx = postgres_test_ctx().await;
    let store = PostgresStore::from_pool(ctx.pool.clone());

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
    let ctx = postgres_test_ctx().await;
    let store = PostgresStore::from_pool(ctx.pool.clone());

    assert!(
        store
            .get_global_reasoning_effort_override()
            .await
            .expect("test result")
            .is_none()
    );
    store
        .set_global_reasoning_effort_override(ReasoningEffort::Medium)
        .await
        .expect("test result");
    assert_eq!(
        store
            .get_global_reasoning_effort_override()
            .await
            .expect("test result"),
        Some(ReasoningEffort::Medium)
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
    let ctx = postgres_test_ctx().await;

    let err = sqlx::query(
        "INSERT INTO global_reasoning_effort_overrides (singleton, reasoning_effort) \
         VALUES (0, 'extreme')",
    )
    .execute(&ctx.pool)
    .await
    .expect_err("invalid effort should violate check");

    assert!(err.to_string().contains("violates check constraint"));
}
