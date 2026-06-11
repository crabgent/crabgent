//! Relation-edge tests for the `SQLite` [`super::super::SqliteMemoryStore`].
//!
//! Mirrors the in-memory relation cases in
//! `crabgent-store/src/memory/memory_store_relation_tests.rs`.

use crabgent_core::{MemoryId, MemoryScope, Owner};
use crabgent_store::error::StoreError;
use crabgent_store::ids::RelationId;
use crabgent_store::records::{MemoryDoc, MemoryRelation};
use crabgent_store::relation_type::RelationType;
use crabgent_store::traits::MemoryStore;

use crate::backend::SqliteStore;

fn scope(owner: &str) -> MemoryScope {
    MemoryScope::for_owner(Owner::new(owner))
}

fn shared_scope(owner: &str, agent: &str) -> MemoryScope {
    MemoryScope::for_owner(Owner::new(owner)).with_agent(agent)
}

async fn store() -> SqliteStore {
    SqliteStore::open_in_memory().await.expect("open store")
}

async fn store_doc(s: &SqliteStore, owner: &str, body: &str) -> MemoryId {
    s.memory()
        .store(&MemoryDoc::new(scope(owner), body))
        .await
        .expect("store doc")
}

async fn relate(
    s: &SqliteStore,
    from: &MemoryId,
    to: &MemoryId,
    kind: RelationType,
    edge_scope: MemoryScope,
) -> Result<RelationId, StoreError> {
    let relation = MemoryRelation::new(from.clone(), to.clone(), kind, edge_scope);
    s.memory().relation_store(&relation).await
}

#[tokio::test]
async fn store_then_neighbors_returns_edge() {
    let s = store().await;
    let a = store_doc(&s, "alice", "a").await;
    let b = store_doc(&s, "alice", "b").await;
    relate(&s, &a, &b, RelationType::supports(), scope("alice"))
        .await
        .expect("store edge");

    let from_a = s
        .memory()
        .relation_neighbors(std::slice::from_ref(&a), &scope("alice"))
        .await
        .expect("neighbors");
    assert_eq!(from_a.len(), 1);
    assert_eq!(from_a[0].from_id, a);
    assert_eq!(from_a[0].to_id, b);
    assert_eq!(from_a[0].relation_type, RelationType::supports());

    // The edge is also reachable from the target node.
    let from_b = s
        .memory()
        .relation_neighbors(std::slice::from_ref(&b), &scope("alice"))
        .await
        .expect("neighbors");
    assert_eq!(from_b.len(), 1);
}

#[tokio::test]
async fn relation_store_is_idempotent_and_returns_existing_id() {
    let s = store().await;
    let a = store_doc(&s, "alice", "a").await;
    let b = store_doc(&s, "alice", "b").await;

    let first = MemoryRelation::new(
        a.clone(),
        b.clone(),
        RelationType::supports(),
        scope("alice"),
    );
    let id1 = s
        .memory()
        .relation_store(&first)
        .await
        .expect("first store");
    assert_eq!(id1, first.id);

    // A fresh MemoryRelation with the same natural key but a different id must
    // collapse to the already-stored edge.
    let second = MemoryRelation::new(
        a.clone(),
        b.clone(),
        RelationType::supports(),
        scope("alice"),
    );
    assert_ne!(second.id, first.id);
    let id2 = s
        .memory()
        .relation_store(&second)
        .await
        .expect("second store");
    assert_eq!(id2, id1, "re-store returns the existing id");

    let edges = s
        .memory()
        .relation_neighbors(std::slice::from_ref(&a), &scope("alice"))
        .await
        .expect("neighbors");
    assert_eq!(edges.len(), 1, "no duplicate edge inserted");
}

#[tokio::test]
async fn global_scope_edge_is_idempotent_on_null_owner() {
    // Regression for the SQLite NULL-owner dedup gap: without the partial
    // unique index over (from_id, to_id, relation_type) WHERE owner IS NULL,
    // two global-scope edges with the same natural key both inserted because
    // SQLite treats NULLs as distinct in the table-level UNIQUE. With the index
    // the re-store must collapse to the first edge's id and leave one row.
    let s = store().await;
    let a = store_doc(&s, "alice", "a").await;
    let b = store_doc(&s, "alice", "b").await;

    let first = MemoryRelation::new(
        a.clone(),
        b.clone(),
        RelationType::supports(),
        MemoryScope::global(),
    );
    assert!(first.scope.owner.is_none(), "global scope has no owner");
    let id1 = s
        .memory()
        .relation_store(&first)
        .await
        .expect("first global store");

    let second = MemoryRelation::new(
        a.clone(),
        b.clone(),
        RelationType::supports(),
        MemoryScope::global(),
    );
    assert_ne!(second.id, first.id);
    let id2 = s
        .memory()
        .relation_store(&second)
        .await
        .expect("second global store");
    assert_eq!(
        id2, id1,
        "re-store of a global edge returns the existing id"
    );

    let edges = s
        .memory()
        .relation_neighbors(std::slice::from_ref(&a), &MemoryScope::global())
        .await
        .expect("neighbors");
    assert_eq!(edges.len(), 1, "no duplicate global edge inserted");
    assert!(edges[0].scope.owner.is_none());
}

#[tokio::test]
async fn relation_store_missing_doc_returns_not_found() {
    let s = store().await;
    let a = store_doc(&s, "alice", "a").await;
    let absent = MemoryId::new();

    let result = relate(&s, &a, &absent, RelationType::supports(), scope("alice")).await;
    assert!(matches!(result, Err(StoreError::NotFound)));

    let result = relate(&s, &absent, &a, RelationType::supports(), scope("alice")).await;
    assert!(matches!(result, Err(StoreError::NotFound)));
}

#[tokio::test]
async fn cross_owner_endpoint_is_rejected_as_not_found() {
    let s = store().await;
    let a = store_doc(&s, "alice", "a").await;
    let b = store_doc(&s, "bob", "b").await;

    // alice tries to link her doc to bob's doc under her own scope. bob's doc
    // is not visible to alice, so the edge is rejected with NotFound rather
    // than confirming that bob's doc exists.
    let result = relate(&s, &a, &b, RelationType::supports(), scope("alice")).await;
    assert!(matches!(result, Err(StoreError::NotFound)));

    // The reverse direction (foreign from_id) is rejected too.
    let result = relate(&s, &b, &a, RelationType::supports(), scope("alice")).await;
    assert!(matches!(result, Err(StoreError::NotFound)));

    // No edge was stored.
    let alice_view = s
        .memory()
        .relation_neighbors(std::slice::from_ref(&a), &scope("alice"))
        .await
        .expect("neighbors");
    assert!(alice_view.is_empty(), "no edge over a non-visible endpoint");
}

#[tokio::test]
async fn shared_agent_endpoint_is_visible_to_owner() {
    let s = store().await;
    let agent = "shared-agent";
    let mine = s
        .memory()
        .store(&MemoryDoc::new(shared_scope("alice", agent), "mine"))
        .await
        .expect("store mine");
    let shared = s
        .memory()
        .store(&MemoryDoc::new(shared_scope(agent, agent), "shared"))
        .await
        .expect("store shared");

    // alice links her doc to the shared doc (owner == agent) under her shared
    // scope: the shared endpoint is visible via owner-with-shared widening.
    relate(
        &s,
        &mine,
        &shared,
        RelationType::supports(),
        shared_scope("alice", agent),
    )
    .await
    .expect("shared endpoint is visible");

    let view = s
        .memory()
        .relation_neighbors(&[mine], &shared_scope("alice", agent))
        .await
        .expect("neighbors");
    assert_eq!(view.len(), 1);
}

#[tokio::test]
async fn neighbors_include_shared_owner_and_agent_excludes_other_user() {
    let s = store().await;
    let agent = "shared-agent";
    // Shared root doc (owner == agent), visible to alice and bob via widening.
    let root = s
        .memory()
        .store(&MemoryDoc::new(shared_scope(agent, agent), "root"))
        .await
        .expect("store root");
    let t1 = s
        .memory()
        .store(&MemoryDoc::new(shared_scope("alice", agent), "t1"))
        .await
        .expect("store t1");
    let t2 = s
        .memory()
        .store(&MemoryDoc::new(shared_scope(agent, agent), "t2"))
        .await
        .expect("store t2");
    let t3 = s
        .memory()
        .store(&MemoryDoc::new(shared_scope("bob", agent), "t3"))
        .await
        .expect("store t3");

    // alice's own edge (owner alice, agent shared-agent): root shared, t1 hers.
    relate(
        &s,
        &root,
        &t1,
        RelationType::supports(),
        shared_scope("alice", agent),
    )
    .await
    .expect("own edge");
    // shared edge (owner == agent): both endpoints shared.
    relate(
        &s,
        &root,
        &t2,
        RelationType::supports(),
        shared_scope(agent, agent),
    )
    .await
    .expect("shared edge");
    // bob's edge (owner bob, agent shared-agent): root shared, t3 bob's. The
    // edge must stay hidden from alice.
    relate(
        &s,
        &root,
        &t3,
        RelationType::supports(),
        shared_scope("bob", agent),
    )
    .await
    .expect("bob edge");

    let view = s
        .memory()
        .relation_neighbors(&[root], &shared_scope("alice", agent))
        .await
        .expect("neighbors");
    assert_eq!(view.len(), 2, "alice private + shared, never bob");
    assert!(view.iter().all(|edge| {
        edge.scope.owner == Some(Owner::new("alice")) || edge.scope.owner == Some(Owner::new(agent))
    }));
}

#[tokio::test]
async fn relation_delete_by_natural_key_respects_owner() {
    let s = store().await;
    let a = store_doc(&s, "alice", "a").await;
    let b = store_doc(&s, "alice", "b").await;
    relate(&s, &a, &b, RelationType::supports(), scope("alice"))
        .await
        .expect("edge");

    // wrong-owner delete is a no-op.
    let removed = s
        .memory()
        .relation_delete(&a, &b, &RelationType::supports(), &scope("bob"))
        .await
        .expect("delete");
    assert!(!removed, "bob cannot delete alice's edge");

    // owner delete removes the edge.
    let removed = s
        .memory()
        .relation_delete(&a, &b, &RelationType::supports(), &scope("alice"))
        .await
        .expect("delete");
    assert!(removed);

    let edges = s
        .memory()
        .relation_neighbors(std::slice::from_ref(&a), &scope("alice"))
        .await
        .expect("neighbors");
    assert!(edges.is_empty());
}

#[tokio::test]
async fn delete_cascades_to_relations() {
    let s = store().await;
    let a = store_doc(&s, "alice", "a").await;
    let b = store_doc(&s, "alice", "b").await;
    relate(&s, &a, &b, RelationType::supports(), scope("alice"))
        .await
        .expect("edge");

    assert!(s.memory().delete(&a).await.expect("delete"));

    let edges = s
        .memory()
        .relation_neighbors(&[b], &scope("alice"))
        .await
        .expect("neighbors");
    assert!(edges.is_empty(), "deleting a node cascades its edges");
}

#[tokio::test]
async fn delete_scoped_cascades_when_it_removes() {
    let s = store().await;
    let a = store_doc(&s, "alice", "a").await;
    let b = store_doc(&s, "alice", "b").await;
    relate(&s, &a, &b, RelationType::supports(), scope("alice"))
        .await
        .expect("edge");

    // wrong-owner scoped delete is a no-op and leaves the edge.
    assert!(
        !s.memory()
            .delete_scoped(&a, &scope("bob"))
            .await
            .expect("delete")
    );
    assert_eq!(
        s.memory()
            .relation_neighbors(std::slice::from_ref(&a), &scope("alice"))
            .await
            .expect("neighbors")
            .len(),
        1
    );

    // owner scoped delete removes the doc and cascades the edge.
    assert!(
        s.memory()
            .delete_scoped(&a, &scope("alice"))
            .await
            .expect("delete")
    );
    assert!(
        s.memory()
            .relation_neighbors(&[b], &scope("alice"))
            .await
            .expect("neighbors")
            .is_empty()
    );
}
