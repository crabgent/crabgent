//! Agent-shared memory recall over the `SQLite` backend.
//!
//! PRIVATE rows have `owner = user-id`; SHARED rows have `owner = agent-id`.
//! With `include_shared`, recall for a user talking to an agent returns the
//! user's private rows plus the agent's shared rows, never another user's
//! private rows.

use crabgent_core::{MemoryId, MemoryScope, Owner, SearchQuery};
use crabgent_store::{MemoryDoc, MemoryStore};
use crabgent_store_sqlite::SqliteStore;

async fn store() -> SqliteStore {
    SqliteStore::open_in_memory()
        .await
        .expect("open sqlite store")
}

fn shared_agent_scope(owner: &str) -> MemoryScope {
    MemoryScope::for_owner(Owner::new(owner)).with_agent("shared-agent")
}

fn doc(owner: &str, class: &str, body: &str) -> MemoryDoc {
    let mut doc = MemoryDoc::new(shared_agent_scope(owner), body);
    doc.class = Some(class.to_owned());
    doc
}

async fn store_doc(store: &SqliteStore, owner: &str, class: &str, body: &str) -> MemoryId {
    store
        .memory()
        .store(&doc(owner, class, body))
        .await
        .expect("store memory")
}

#[tokio::test]
async fn shared_rows_recall_for_any_user_with_include_shared() {
    let store = store().await;
    let alice_id = store_doc(&store, "alice", "semantic", "shared knowledge entry").await;
    let bob_id = store_doc(&store, "bob", "semantic", "shared knowledge entry").await;
    let shared_agent_id =
        store_doc(&store, "shared-agent", "semantic", "shared knowledge entry").await;

    let query = SearchQuery::new("knowledge")
        .scope(shared_agent_scope("alice"))
        .include_shared(true);
    let hits = store.memory().search(&query).await.expect("search memory");
    let ids: Vec<&MemoryId> = hits.iter().map(|hit| &hit.id).collect();

    assert_eq!(
        hits.len(),
        2,
        "alice private + shared-agent shared, never bob"
    );
    assert!(ids.contains(&&alice_id), "alice private row recalled");
    assert!(
        ids.contains(&&shared_agent_id),
        "shared-agent shared row recalled"
    );
    assert!(!ids.contains(&&bob_id), "bob private row must never leak");
}

#[tokio::test]
async fn without_include_shared_returns_owner_only() {
    let store = store().await;
    let alice_id = store_doc(&store, "alice", "semantic", "shared knowledge entry").await;
    store_doc(&store, "shared-agent", "semantic", "shared knowledge entry").await;

    let query = SearchQuery::new("knowledge").scope(shared_agent_scope("alice"));
    let hits = store.memory().search(&query).await.expect("search memory");

    assert_eq!(hits.len(), 1, "no flag: alice only");
    assert_eq!(hits[0].id, alice_id);
}

#[tokio::test]
async fn skill_and_tool_rows_are_shared_and_recall_for_any_user() {
    let store = store().await;
    let skill_id = store_doc(&store, "shared-agent", "skill", "deployment runbook").await;
    let tool_id = store_doc(&store, "shared-agent", "tools", "deployment runbook").await;
    // A different human's private row must not surface for an unrelated user.
    store_doc(&store, "alice", "semantic", "deployment runbook").await;

    // A user who has never stored anything, talking to agent shared-agent.
    let query = SearchQuery::new("deployment")
        .scope(shared_agent_scope("bob"))
        .include_shared(true);
    let hits = store.memory().search(&query).await.expect("search memory");
    let ids: Vec<&MemoryId> = hits.iter().map(|hit| &hit.id).collect();

    assert_eq!(
        hits.len(),
        2,
        "bob sees shared-agent's shared rows, not alice private"
    );
    assert!(ids.contains(&&skill_id), "skill row is shared");
    assert!(ids.contains(&&tool_id), "tool row is shared");
}
