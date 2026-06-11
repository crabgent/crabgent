//! `MemoryStore` integration tests.
//!
//! Each test uses `postgres_test_ctx()`. Container mode gets a fresh database;
//! `PG_TEST_DSN` mode stays idempotent through unique owner prefixes.

#[path = "../src/test_helpers.rs"]
mod test_helpers;

use crabgent_core::{MemoryScope, Owner, SearchQuery};
use crabgent_store::{MemoryDoc, MemoryStore};
use crabgent_store_postgres::PostgresStore;
use test_helpers::postgres_test_ctx;
use uuid::Uuid;

fn owner(test_name: &str) -> Owner {
    Owner::new(format!("pg-memory-{test_name}-{}", Uuid::now_v7()))
}

async fn store_doc(
    store: &PostgresStore,
    scope: MemoryScope,
    body: impl Into<String>,
) -> crabgent_core::MemoryId {
    store
        .memory_store()
        .store(&MemoryDoc::new(scope, body))
        .await
        .expect("test result")
}

fn scoped(owner: &str, agent: &str) -> MemoryScope {
    MemoryScope::for_owner(Owner::new(owner)).with_agent(agent)
}

/// Verifies the `owner IN ($n, $n+1)` widening keeps positional-param
/// alignment: the agent predicate and the user/agent owner binds must land on
/// their own placeholders, otherwise the query returns wrong rows.
#[tokio::test]
async fn memory_shared_recall_returns_user_and_agent_rows_never_other_user() {
    let ctx = postgres_test_ctx().await;
    let store = PostgresStore::from_pool(ctx.pool.clone());
    let suffix = Uuid::now_v7();
    // SHARED rows have owner = agent id, so agent and the shared owner match.
    let agent = format!("pg-shared-agent-{suffix}");
    let alice = format!("pg-shared-alice-{suffix}");
    let bob = format!("pg-shared-bob-{suffix}");

    let alice_id = store_doc(&store, scoped(&alice, &agent), "shared knowledge entry").await;
    let bob_id = store_doc(&store, scoped(&bob, &agent), "shared knowledge entry").await;
    let agent_id = store_doc(&store, scoped(&agent, &agent), "shared knowledge entry").await;

    let query = SearchQuery::new("knowledge")
        .scope(scoped(&alice, &agent))
        .include_shared(true);
    let hits = store.memory_store().search(&query).await.expect("search");
    let ids: Vec<&crabgent_core::MemoryId> = hits.iter().map(|hit| &hit.id).collect();

    assert_eq!(hits.len(), 2, "alice private + agent shared, never bob");
    assert!(ids.contains(&&alice_id), "alice private row recalled");
    assert!(ids.contains(&&agent_id), "agent shared row recalled");
    assert!(!ids.contains(&&bob_id), "bob private row must never leak");
}

#[tokio::test]
async fn memory_store_get_delete() {
    let ctx = postgres_test_ctx().await;
    let store = PostgresStore::from_pool(ctx.pool.clone());
    let scope = MemoryScope::for_owner(owner("get-delete"));

    let id = store_doc(&store, scope, "remember invoices").await;
    let doc = store
        .memory_store()
        .get(&id)
        .await
        .expect("test result")
        .expect("doc exists");
    assert_eq!(doc.body, "remember invoices");

    assert!(store.memory_store().delete(&id).await.expect("test result"));
    assert!(
        store
            .memory_store()
            .get(&id)
            .await
            .expect("test result")
            .is_none()
    );
}

#[tokio::test]
async fn memory_delete_scoped_mismatch_returns_false() {
    let ctx = postgres_test_ctx().await;
    let store = PostgresStore::from_pool(ctx.pool.clone());
    let alice = MemoryScope::for_owner(owner("delete-scoped-alice"));
    let bob = MemoryScope::for_owner(owner("delete-scoped-bob"));
    let id = store_doc(&store, alice.clone(), "private note").await;

    assert!(
        !store
            .memory_store()
            .delete_scoped(&id, &bob)
            .await
            .expect("test result")
    );
    assert!(
        store
            .memory_store()
            .get(&id)
            .await
            .expect("test result")
            .is_some()
    );
    assert!(
        store
            .memory_store()
            .delete_scoped(&id, &alice)
            .await
            .expect("test result")
    );
}

#[tokio::test]
async fn fts_memory_search_scoped_hit() {
    // If this flakes under parallel container load, validation on 2026-05-22
    // pointed at container or pool saturation rather than a long-held FTS
    // read. `PostgresMemoryStore::search` builds a self-contained query inside
    // `retry_transient("memory.search", ...)`.
    let ctx = postgres_test_ctx().await;
    let store = PostgresStore::from_pool(ctx.pool.clone());
    let scope = MemoryScope::for_owner(owner("fts-hit"))
        .with_channel("slack")
        .with_kind("direct");
    store_doc(
        &store,
        scope.clone(),
        "The deployment keyword is heliotrope",
    )
    .await;

    let query = SearchQuery::new("heliotrope").scope(scope);
    let hits = store
        .memory_store()
        .search(&query)
        .await
        .expect("test result");

    assert_eq!(hits.len(), 1);
    assert!(hits[0].body.contains("heliotrope"));
}

#[tokio::test]
async fn fts_memory_search_zero_hits() {
    let ctx = postgres_test_ctx().await;
    let store = PostgresStore::from_pool(ctx.pool.clone());
    let scope = MemoryScope::for_owner(owner("fts-zero"));
    store_doc(&store, scope.clone(), "The searchable term is apricot").await;

    let hits = store
        .memory_store()
        .search(&SearchQuery::new("banana").scope(scope))
        .await
        .expect("test result");

    assert!(hits.is_empty());
}

#[tokio::test]
async fn fts_memory_search_filters_by_owner() {
    let ctx = postgres_test_ctx().await;
    let store = PostgresStore::from_pool(ctx.pool.clone());
    let alice = MemoryScope::for_owner(owner("fts-owner-alice"));
    let bob = MemoryScope::for_owner(owner("fts-owner-bob"));
    store_doc(&store, alice.clone(), "shared keyword").await;
    store_doc(&store, bob, "shared keyword").await;

    let hits = store
        .memory_store()
        .search(&SearchQuery::new("shared").scope(alice))
        .await
        .expect("test result");

    assert_eq!(hits.len(), 1);
}
