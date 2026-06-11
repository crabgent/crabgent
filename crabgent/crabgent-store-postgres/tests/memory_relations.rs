//! `MemoryStore` relation-graph integration tests for the Postgres backend.
//!
//! Mirrors the in-memory relation cases in
//! `crabgent-store/src/memory/memory_store_relation_tests.rs`. Each test uses
//! `postgres_test_ctx()`: container mode gets a fresh database, `PG_TEST_DSN`
//! mode stays idempotent through unique owner prefixes.

#[path = "../src/test_helpers.rs"]
mod test_helpers;

use crabgent_core::{MemoryId, MemoryScope, Owner};
use crabgent_store::MemoryStore;
use crabgent_store::{MemoryDoc, MemoryRelation, RelationType, StoreError};
use crabgent_store_postgres::PostgresStore;
use test_helpers::postgres_test_ctx;
use uuid::Uuid;

/// Owner with a per-test unique suffix so `PG_TEST_DSN` shared-database runs do
/// not collide across tests.
fn owner(test_name: &str) -> String {
    format!("pg-rel-{test_name}-{}", Uuid::now_v7())
}

fn scope(owner: &str) -> MemoryScope {
    MemoryScope::for_owner(Owner::new(owner))
}

fn shared_scope(owner: &str, agent: &str) -> MemoryScope {
    MemoryScope::for_owner(Owner::new(owner)).with_agent(agent)
}

async fn store_doc(store: &PostgresStore, scope: MemoryScope, body: &str) -> MemoryId {
    store
        .memory_store()
        .store(&MemoryDoc::new(scope, body))
        .await
        .expect("store doc")
}

async fn relate(
    store: &PostgresStore,
    from: &MemoryId,
    to: &MemoryId,
    kind: RelationType,
    edge_scope: MemoryScope,
) -> Result<crabgent_store::RelationId, StoreError> {
    let relation = MemoryRelation::new(from.clone(), to.clone(), kind, edge_scope);
    store.memory_store().relation_store(&relation).await
}

#[tokio::test]
async fn store_then_neighbors_returns_edge() {
    let ctx = postgres_test_ctx().await;
    let store = PostgresStore::from_pool(ctx.pool.clone());
    let alice = owner("neighbors");
    let a = store_doc(&store, scope(&alice), "a").await;
    let b = store_doc(&store, scope(&alice), "b").await;
    relate(&store, &a, &b, RelationType::supports(), scope(&alice))
        .await
        .expect("store edge");

    let from_a = store
        .memory_store()
        .relation_neighbors(std::slice::from_ref(&a), &scope(&alice))
        .await
        .expect("neighbors");
    assert_eq!(from_a.len(), 1);
    assert_eq!(from_a[0].from_id, a);
    assert_eq!(from_a[0].to_id, b);

    // The same edge is reachable from the target node too.
    let from_b = store
        .memory_store()
        .relation_neighbors(&[b], &scope(&alice))
        .await
        .expect("neighbors");
    assert_eq!(from_b.len(), 1);
}

#[tokio::test]
async fn relation_store_is_idempotent_and_returns_existing_id() {
    let ctx = postgres_test_ctx().await;
    let store = PostgresStore::from_pool(ctx.pool.clone());
    let alice = owner("idempotent");
    let a = store_doc(&store, scope(&alice), "a").await;
    let b = store_doc(&store, scope(&alice), "b").await;

    let first = MemoryRelation::new(
        a.clone(),
        b.clone(),
        RelationType::supports(),
        scope(&alice),
    );
    let id1 = store
        .memory_store()
        .relation_store(&first)
        .await
        .expect("first store");
    assert_eq!(id1, first.id);

    // Same natural key, freshly generated RelationId.
    let again = MemoryRelation::new(
        a.clone(),
        b.clone(),
        RelationType::supports(),
        scope(&alice),
    );
    let id2 = store
        .memory_store()
        .relation_store(&again)
        .await
        .expect("second store");
    assert_eq!(id2, id1, "re-store returns the existing id");
    assert_ne!(again.id, id2, "the freshly generated id is discarded");

    let neighbors = store
        .memory_store()
        .relation_neighbors(&[a], &scope(&alice))
        .await
        .expect("neighbors");
    assert_eq!(neighbors.len(), 1, "no duplicate edge");
}

#[tokio::test]
async fn relation_store_missing_document_is_not_found() {
    let ctx = postgres_test_ctx().await;
    let store = PostgresStore::from_pool(ctx.pool.clone());
    let alice = owner("missing-doc");
    let a = store_doc(&store, scope(&alice), "a").await;
    let ghost = MemoryId::new();

    let err = relate(&store, &a, &ghost, RelationType::supports(), scope(&alice))
        .await
        .expect_err("missing target rejected");
    assert!(matches!(err, StoreError::NotFound));
}

#[tokio::test]
async fn cross_owner_edge_is_owned_by_writer() {
    let ctx = postgres_test_ctx().await;
    let store = PostgresStore::from_pool(ctx.pool.clone());
    let alice = owner("cross-alice");
    let bob = owner("cross-bob");
    let a = store_doc(&store, scope(&alice), "a").await;
    let b = store_doc(&store, scope(&bob), "b").await;

    // alice draws an edge from her doc to bob's doc; the edge scope is alice's.
    relate(&store, &a, &b, RelationType::supports(), scope(&alice))
        .await
        .expect("cross-owner edge stored");

    // alice sees her edge.
    let alice_view = store
        .memory_store()
        .relation_neighbors(std::slice::from_ref(&a), &scope(&alice))
        .await
        .expect("neighbors");
    assert_eq!(alice_view.len(), 1);

    // bob queries from his own doc but does not own the edge, so it is hidden.
    let bob_view = store
        .memory_store()
        .relation_neighbors(&[b], &scope(&bob))
        .await
        .expect("neighbors");
    assert!(bob_view.is_empty(), "bob cannot see alice's edge");
}

#[tokio::test]
async fn neighbors_include_shared_owner_and_agent_excludes_other_user() {
    let ctx = postgres_test_ctx().await;
    let store = PostgresStore::from_pool(ctx.pool.clone());
    let suffix = Uuid::now_v7();
    let agent = format!("pg-rel-shared-agent-{suffix}");
    let alice = format!("pg-rel-shared-alice-{suffix}");
    let bob = format!("pg-rel-shared-bob-{suffix}");

    let root = store_doc(&store, scope(&alice), "root").await;
    let t1 = store_doc(&store, scope(&alice), "t1").await;
    let t2 = store_doc(&store, scope(&alice), "t2").await;
    let t3 = store_doc(&store, scope(&alice), "t3").await;

    // alice's own edge (owner alice, agent shared-agent).
    relate(
        &store,
        &root,
        &t1,
        RelationType::supports(),
        shared_scope(&alice, &agent),
    )
    .await
    .expect("own edge");
    // shared edge (owner == agent).
    relate(
        &store,
        &root,
        &t2,
        RelationType::supports(),
        shared_scope(&agent, &agent),
    )
    .await
    .expect("shared edge");
    // bob's edge (owner bob, agent shared-agent) must stay hidden.
    relate(
        &store,
        &root,
        &t3,
        RelationType::supports(),
        shared_scope(&bob, &agent),
    )
    .await
    .expect("bob edge");

    let view = store
        .memory_store()
        .relation_neighbors(&[root], &shared_scope(&alice, &agent))
        .await
        .expect("neighbors");
    assert_eq!(view.len(), 2, "alice private + shared, never bob");
    assert!(view.iter().all(|edge| edge.to_id != t3));
}

#[tokio::test]
async fn relation_delete_respects_owner_and_type() {
    let ctx = postgres_test_ctx().await;
    let store = PostgresStore::from_pool(ctx.pool.clone());
    let alice = owner("delete-alice");
    let bob = owner("delete-bob");
    let a = store_doc(&store, scope(&alice), "a").await;
    let b = store_doc(&store, scope(&alice), "b").await;
    relate(&store, &a, &b, RelationType::supports(), scope(&alice))
        .await
        .expect("edge");

    // wrong owner does not delete.
    assert!(
        !store
            .memory_store()
            .relation_delete(&a, &b, &RelationType::supports(), &scope(&bob))
            .await
            .expect("delete")
    );
    // wrong type does not delete.
    assert!(
        !store
            .memory_store()
            .relation_delete(&a, &b, &RelationType::contradicts(), &scope(&alice))
            .await
            .expect("delete")
    );
    // exact natural key deletes.
    assert!(
        store
            .memory_store()
            .relation_delete(&a, &b, &RelationType::supports(), &scope(&alice))
            .await
            .expect("delete")
    );
    assert!(
        store
            .memory_store()
            .relation_neighbors(&[a], &scope(&alice))
            .await
            .expect("neighbors")
            .is_empty()
    );
}

#[tokio::test]
async fn doc_delete_cascades_incident_edges() {
    let ctx = postgres_test_ctx().await;
    let store = PostgresStore::from_pool(ctx.pool.clone());
    let alice = owner("cascade");
    let a = store_doc(&store, scope(&alice), "a").await;
    let b = store_doc(&store, scope(&alice), "b").await;
    let c = store_doc(&store, scope(&alice), "c").await;
    // a -> b and b -> c so deleting b removes both edges (incident as from and to).
    relate(&store, &a, &b, RelationType::supports(), scope(&alice))
        .await
        .expect("a->b");
    relate(&store, &b, &c, RelationType::supports(), scope(&alice))
        .await
        .expect("b->c");

    assert!(
        store.memory_store().delete(&b).await.expect("delete b"),
        "doc b removed"
    );

    let from_a = store
        .memory_store()
        .relation_neighbors(&[a], &scope(&alice))
        .await
        .expect("neighbors a");
    assert!(from_a.is_empty(), "a->b cascaded away");
    let from_c = store
        .memory_store()
        .relation_neighbors(&[c], &scope(&alice))
        .await
        .expect("neighbors c");
    assert!(from_c.is_empty(), "b->c cascaded away");
}
