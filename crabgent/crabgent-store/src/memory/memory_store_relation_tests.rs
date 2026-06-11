//! In-memory relation-edge tests for [`super::MemoryMemoryStore`].

use crabgent_core::Owner;

use super::*;

fn scope(owner: &str) -> MemoryScope {
    MemoryScope::for_owner(Owner::new(owner))
}

fn shared_scope(owner: &str, agent: &str) -> MemoryScope {
    MemoryScope::for_owner(Owner::new(owner)).with_agent(agent)
}

async fn store_doc(store: &MemoryMemoryStore, owner: &str, body: &str) -> MemoryId {
    store
        .store(&MemoryDoc::new(scope(owner), body))
        .await
        .expect("store doc")
}

async fn relate(
    store: &MemoryMemoryStore,
    from: &MemoryId,
    to: &MemoryId,
    kind: RelationType,
    edge_scope: MemoryScope,
) -> Result<RelationId, StoreError> {
    let relation = MemoryRelation::new(from.clone(), to.clone(), kind, edge_scope);
    store.relation_store(&relation).await
}

#[tokio::test]
async fn store_then_neighbors_returns_edge() {
    let store = MemoryMemoryStore::default();
    let a = store_doc(&store, "alice", "a").await;
    let b = store_doc(&store, "alice", "b").await;
    relate(&store, &a, &b, RelationType::supports(), scope("alice"))
        .await
        .expect("store edge");

    let from_a = store
        .relation_neighbors(std::slice::from_ref(&a), &scope("alice"))
        .await
        .expect("neighbors");
    assert_eq!(from_a.len(), 1);
    assert_eq!(from_a[0].from_id, a);
    assert_eq!(from_a[0].to_id, b);

    // The same edge is reachable from the target node too.
    let from_b = store
        .relation_neighbors(&[b], &scope("alice"))
        .await
        .expect("neighbors");
    assert_eq!(from_b.len(), 1);
}

#[tokio::test]
async fn relation_store_is_idempotent_and_returns_existing_id() {
    let store = MemoryMemoryStore::default();
    let a = store_doc(&store, "alice", "a").await;
    let b = store_doc(&store, "alice", "b").await;

    let first = MemoryRelation::new(
        a.clone(),
        b.clone(),
        RelationType::supports(),
        scope("alice"),
    );
    let id1 = store.relation_store(&first).await.expect("first store");
    assert_eq!(id1, first.id);

    // Same natural key, freshly generated RelationId.
    let again = MemoryRelation::new(
        a.clone(),
        b.clone(),
        RelationType::supports(),
        scope("alice"),
    );
    let id2 = store.relation_store(&again).await.expect("second store");
    assert_eq!(id2, id1, "re-store returns the existing id");
    assert_ne!(again.id, id2, "the freshly generated id is discarded");

    let neighbors = store
        .relation_neighbors(&[a], &scope("alice"))
        .await
        .expect("neighbors");
    assert_eq!(neighbors.len(), 1, "no duplicate edge");
}

#[tokio::test]
async fn different_relation_type_is_a_distinct_edge() {
    let store = MemoryMemoryStore::default();
    let a = store_doc(&store, "alice", "a").await;
    let b = store_doc(&store, "alice", "b").await;
    relate(&store, &a, &b, RelationType::supports(), scope("alice"))
        .await
        .expect("supports");
    relate(&store, &a, &b, RelationType::contradicts(), scope("alice"))
        .await
        .expect("contradicts");

    let neighbors = store
        .relation_neighbors(&[a], &scope("alice"))
        .await
        .expect("neighbors");
    assert_eq!(neighbors.len(), 2);
}

#[tokio::test]
async fn relation_store_missing_document_is_not_found() {
    let store = MemoryMemoryStore::default();
    let a = store_doc(&store, "alice", "a").await;
    let ghost = MemoryId::new();

    let err = relate(&store, &a, &ghost, RelationType::supports(), scope("alice"))
        .await
        .expect_err("missing target rejected");
    assert!(matches!(err, StoreError::NotFound));
}

#[tokio::test]
async fn cross_owner_endpoint_is_rejected_as_not_found() {
    let store = MemoryMemoryStore::default();
    let a = store_doc(&store, "alice", "a").await;
    let b = store_doc(&store, "bob", "b").await;

    // alice tries to link her doc to bob's doc under her own scope. The target
    // is not visible to alice, so the edge is rejected with NotFound rather
    // than confirming that bob's doc exists.
    let err = relate(&store, &a, &b, RelationType::supports(), scope("alice"))
        .await
        .expect_err("cross-owner endpoint rejected");
    assert!(matches!(err, StoreError::NotFound));

    // No edge was stored; alice's own node has no neighbors.
    let alice_view = store
        .relation_neighbors(std::slice::from_ref(&a), &scope("alice"))
        .await
        .expect("neighbors");
    assert!(alice_view.is_empty(), "no edge over a non-visible endpoint");
}

#[tokio::test]
async fn shared_agent_endpoint_is_visible_to_owner() {
    let store = MemoryMemoryStore::default();
    let agent = "shared-agent";
    // alice's own doc and a shared doc (owner == agent), both under the agent.
    let mine = store
        .store(&MemoryDoc::new(shared_scope("alice", agent), "mine"))
        .await
        .expect("store mine");
    let shared = store
        .store(&MemoryDoc::new(shared_scope(agent, agent), "shared"))
        .await
        .expect("store shared");

    // alice links her doc to the shared doc under her shared scope: the shared
    // endpoint is visible via owner-with-shared widening, so the edge stores.
    relate(
        &store,
        &mine,
        &shared,
        RelationType::supports(),
        shared_scope("alice", agent),
    )
    .await
    .expect("shared endpoint is visible");

    let view = store
        .relation_neighbors(&[mine], &shared_scope("alice", agent))
        .await
        .expect("neighbors");
    assert_eq!(view.len(), 1);
}

#[tokio::test]
async fn neighbors_include_shared_owner_and_agent_excludes_other_user() {
    let store = MemoryMemoryStore::default();
    let agent = "shared-agent";
    // Shared root doc (owner == agent), visible to alice and bob via widening.
    let root = store
        .store(&MemoryDoc::new(shared_scope(agent, agent), "root"))
        .await
        .expect("store root");
    let t1 = store
        .store(&MemoryDoc::new(shared_scope("alice", agent), "t1"))
        .await
        .expect("store t1");
    let t2 = store
        .store(&MemoryDoc::new(shared_scope(agent, agent), "t2"))
        .await
        .expect("store t2");
    let t3 = store
        .store(&MemoryDoc::new(shared_scope("bob", agent), "t3"))
        .await
        .expect("store t3");

    // alice's own edge (owner alice, agent shared-agent): root is shared, t1 is
    // alice's, both visible to alice.
    relate(
        &store,
        &root,
        &t1,
        RelationType::supports(),
        shared_scope("alice", agent),
    )
    .await
    .expect("own edge");
    // shared edge (owner == agent): both endpoints shared.
    relate(
        &store,
        &root,
        &t2,
        RelationType::supports(),
        shared_scope(agent, agent),
    )
    .await
    .expect("shared edge");
    // bob's edge (owner bob, agent shared-agent): root is shared, t3 is bob's,
    // both visible to bob. The edge must stay hidden from alice.
    relate(
        &store,
        &root,
        &t3,
        RelationType::supports(),
        shared_scope("bob", agent),
    )
    .await
    .expect("bob edge");

    let view = store
        .relation_neighbors(&[root], &shared_scope("alice", agent))
        .await
        .expect("neighbors");
    assert_eq!(view.len(), 2, "alice private + shared, never bob");
    assert!(view.iter().all(|edge| edge.to_id != t3));
}

#[tokio::test]
async fn relation_delete_removes_only_matching_edge() {
    let store = MemoryMemoryStore::default();
    let a = store_doc(&store, "alice", "a").await;
    let b = store_doc(&store, "alice", "b").await;
    relate(&store, &a, &b, RelationType::supports(), scope("alice"))
        .await
        .expect("edge");

    // wrong owner does not delete.
    assert!(
        !store
            .relation_delete(&a, &b, &RelationType::supports(), &scope("bob"))
            .await
            .expect("delete")
    );
    // wrong type does not delete.
    assert!(
        !store
            .relation_delete(&a, &b, &RelationType::contradicts(), &scope("alice"))
            .await
            .expect("delete")
    );
    // exact natural key deletes.
    assert!(
        store
            .relation_delete(&a, &b, &RelationType::supports(), &scope("alice"))
            .await
            .expect("delete")
    );
    assert!(
        store
            .relation_neighbors(&[a], &scope("alice"))
            .await
            .expect("neighbors")
            .is_empty()
    );
}

#[tokio::test]
async fn deleting_a_document_cascades_its_edges() {
    let store = MemoryMemoryStore::default();
    let a = store_doc(&store, "alice", "a").await;
    let b = store_doc(&store, "alice", "b").await;
    let c = store_doc(&store, "alice", "c").await;
    relate(&store, &a, &b, RelationType::supports(), scope("alice"))
        .await
        .expect("a-b");
    relate(&store, &b, &c, RelationType::supports(), scope("alice"))
        .await
        .expect("b-c");

    // delete b: both incident edges (a-b, b-c) vanish.
    assert!(store.delete(&b).await.expect("delete b"));
    assert!(
        store
            .relation_neighbors(&[a, c], &scope("alice"))
            .await
            .expect("neighbors")
            .is_empty()
    );
}

#[tokio::test]
async fn delete_scoped_cascades_when_it_removes() {
    let store = MemoryMemoryStore::default();
    let a = store_doc(&store, "alice", "a").await;
    let b = store_doc(&store, "alice", "b").await;
    relate(&store, &a, &b, RelationType::supports(), scope("alice"))
        .await
        .expect("edge");

    // wrong-owner scoped delete is a no-op and leaves the edge.
    assert!(
        !store
            .delete_scoped(&a, &scope("bob"))
            .await
            .expect("delete")
    );
    assert_eq!(
        store
            .relation_neighbors(std::slice::from_ref(&a), &scope("alice"))
            .await
            .expect("neighbors")
            .len(),
        1
    );

    // owner scoped delete removes the doc and cascades the edge.
    assert!(
        store
            .delete_scoped(&a, &scope("alice"))
            .await
            .expect("delete")
    );
    assert!(
        store
            .relation_neighbors(&[b], &scope("alice"))
            .await
            .expect("neighbors")
            .is_empty()
    );
}
