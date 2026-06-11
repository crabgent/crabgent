//! In-memory backend tests for [`super::MemoryMemoryStore`].
//!
//! Moved out of `memory_store.rs` to keep that file under the 500-line cap
//! when the relation methods landed; behavior is unchanged.

use super::*;
use chrono::Duration;
use crabgent_core::Owner;

fn alice_scope() -> MemoryScope {
    MemoryScope::for_owner(Owner::new("alice"))
}

fn doc(scope: MemoryScope, body: impl Into<String>) -> MemoryDoc {
    MemoryDoc::new(scope, body)
}

async fn store_doc(
    store: &MemoryMemoryStore,
    scope: MemoryScope,
    body: impl Into<String>,
) -> MemoryId {
    store.store(&doc(scope, body)).await.expect("test result")
}

fn ranked_doc(importance: Option<f32>, created_at: DateTime<Utc>) -> MemoryDoc {
    let mut doc = doc(alice_scope(), "expert knowledge");
    doc.importance = importance;
    doc.created_at = created_at;
    doc.updated_at = created_at;
    doc
}

async fn assert_order(first: (Option<f32>, i64), second: (Option<f32>, i64)) {
    let store = MemoryMemoryStore::default();
    let now = Utc::now();
    let first = ranked_doc(first.0, now - Duration::hours(first.1));
    let second = ranked_doc(second.0, now - Duration::hours(second.1));
    let first_id = store.store(&first).await.expect("store first doc");
    let second_id = store.store(&second).await.expect("store second doc");
    let q = SearchQuery::new("expert").scope(alice_scope());
    let hits = store.search(&q).await.expect("test result");
    assert_eq!(hits.len(), 2);
    assert_eq!(hits[0].id, first_id);
    assert_eq!(hits[1].id, second_id);
}

#[tokio::test]
async fn store_then_get_returns_doc() {
    let store = MemoryMemoryStore::default();
    let id = store_doc(&store, alice_scope(), "remember me").await;
    let doc = store
        .get(&id)
        .await
        .expect("test result")
        .expect("doc exists");
    assert_eq!(doc.body, "remember me");
    assert_eq!(doc.scope.owner, Some(Owner::new("alice")));
}

#[tokio::test]
async fn store_doc_roundtrip_preserves_class() {
    let store = MemoryMemoryStore::default();
    let mut doc = doc(alice_scope(), "semantic note");
    doc.class = Some("semantic".to_owned());
    let id = store.store(&doc).await.expect("test result");
    let stored = store
        .get(&id)
        .await
        .expect("test result")
        .expect("doc exists");
    assert_eq!(stored.class.as_deref(), Some("semantic"));
}

#[tokio::test]
async fn store_preserves_importance() {
    let store = MemoryMemoryStore::default();
    let mut doc = doc(alice_scope(), "important note");
    doc.importance = Some(0.75);
    let id = store.store(&doc).await.expect("test result");
    let stored = store
        .get(&id)
        .await
        .expect("test result")
        .expect("doc exists");
    assert_eq!(stored.importance, Some(0.75));
}

#[tokio::test]
async fn store_preserves_expires_at() {
    let store = MemoryMemoryStore::default();
    let expires_at = Utc::now() + Duration::hours(1);
    let mut doc = doc(alice_scope(), "temporary note");
    doc.expires_at = Some(expires_at);
    let id = store.store(&doc).await.expect("test result");
    let stored = store
        .get(&id)
        .await
        .expect("test result")
        .expect("doc exists");
    assert_eq!(stored.expires_at, Some(expires_at));
}

#[tokio::test]
async fn get_unknown_returns_none() {
    let store = MemoryMemoryStore::default();
    let unknown = MemoryId::new();
    assert!(store.get(&unknown).await.expect("test result").is_none());
}

#[tokio::test]
async fn delete_returns_true_when_existed() {
    let store = MemoryMemoryStore::default();
    let id = store_doc(&store, alice_scope(), "x").await;
    assert!(store.delete(&id).await.expect("test result"));
    assert!(store.get(&id).await.expect("test result").is_none());
}

#[tokio::test]
async fn delete_returns_false_when_absent() {
    let store = MemoryMemoryStore::default();
    let unknown = MemoryId::new();
    assert!(!store.delete(&unknown).await.expect("test result"));
}

#[tokio::test]
async fn delete_scoped_respects_scope() {
    let store = MemoryMemoryStore::default();
    let id = store_doc(&store, alice_scope(), "x").await;
    let bob = MemoryScope::for_owner(Owner::new("bob"));
    assert!(!store.delete_scoped(&id, &bob).await.expect("test result"));
    assert!(store.get(&id).await.expect("test result").is_some());
    assert!(
        store
            .delete_scoped(&id, &alice_scope())
            .await
            .expect("test result")
    );
    assert!(store.get(&id).await.expect("test result").is_none());
}

#[tokio::test]
async fn search_filters_by_owner_scope() {
    let store = MemoryMemoryStore::default();
    store_doc(
        &store,
        MemoryScope::for_owner(Owner::new("alice")),
        "secret",
    )
    .await;
    store_doc(&store, MemoryScope::for_owner(Owner::new("bob")), "secret").await;
    let q = SearchQuery::new("secret").scope(alice_scope());
    let hits = store.search(&q).await.expect("test result");
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].body, "secret");
}

#[tokio::test]
async fn include_shared_returns_user_and_agent_rows_excludes_other_user() {
    let store = MemoryMemoryStore::default();
    let shared_agent =
        |owner: &str| MemoryScope::for_owner(Owner::new(owner)).with_agent("shared-agent");
    store_doc(&store, shared_agent("alice"), "shared knowledge").await;
    store_doc(&store, shared_agent("bob"), "shared knowledge").await;
    let shared_agent_id = store_doc(&store, shared_agent("shared-agent"), "shared knowledge").await;

    let query = SearchQuery::new("shared")
        .scope(shared_agent("alice"))
        .include_shared(true);
    let hits = store.search(&query).await.expect("test result");

    assert_eq!(
        hits.len(),
        2,
        "alice private + shared-agent shared, never bob"
    );
    assert!(hits.iter().any(|hit| hit.id == shared_agent_id));
}

#[tokio::test]
async fn exact_owner_match_returns_only_owner_rows() {
    let store = MemoryMemoryStore::default();
    let shared_agent =
        |owner: &str| MemoryScope::for_owner(Owner::new(owner)).with_agent("shared-agent");
    store_doc(&store, shared_agent("alice"), "shared knowledge").await;
    store_doc(&store, shared_agent("shared-agent"), "shared knowledge").await;

    let query = SearchQuery::new("shared").scope(shared_agent("alice"));
    let hits = store.search(&query).await.expect("test result");

    assert_eq!(hits.len(), 1, "no flag: alice only");
}

#[tokio::test]
async fn search_substring_match_case_insensitive() {
    let store = MemoryMemoryStore::default();
    store_doc(&store, alice_scope(), "Alice prefers local-first tools").await;
    let q = SearchQuery::new("LOCAL-FIRST").scope(alice_scope());
    let hits = store.search(&q).await.expect("test result");
    assert_eq!(hits.len(), 1);
}

#[tokio::test]
async fn search_in_memory_returns_no_cosine_similarity() {
    let store = MemoryMemoryStore::default();
    let mut doc = doc(alice_scope(), "vector note");
    doc.embedding = Some(vec![1.0, 0.0, 0.0]);
    store.store(&doc).await.expect("test result");
    let mut q = SearchQuery::new("vector").scope(alice_scope());
    q.embedding = Some(vec![1.0, 0.0, 0.0]);

    let hits = store.search(&q).await.expect("test result");

    assert_eq!(hits.len(), 1);
    assert!(hits[0].cosine_similarity.is_none());
}

#[tokio::test]
async fn search_global_filter_matches_any_scope() {
    let store = MemoryMemoryStore::default();
    store_doc(&store, MemoryScope::for_owner(Owner::new("u1")), "a").await;
    store_doc(&store, MemoryScope::for_owner(Owner::new("u2")), "a").await;
    let q = SearchQuery::new("a").scope(MemoryScope::global());
    let hits = store.search(&q).await.expect("test result");
    assert_eq!(hits.len(), 2);
}

#[tokio::test]
async fn search_respects_limit() {
    let store = MemoryMemoryStore::default();
    for i in 0..5 {
        store_doc(&store, alice_scope(), format!("note {i}")).await;
    }
    let q = SearchQuery::new("note").scope(alice_scope()).limit(2);
    let hits = store.search(&q).await.expect("test result");
    assert_eq!(hits.len(), 2);
}

#[tokio::test]
async fn search_empty_query_returns_all_in_scope() {
    let store = MemoryMemoryStore::default();
    store_doc(&store, alice_scope(), "first").await;
    store_doc(&store, alice_scope(), "second").await;
    let q = SearchQuery::new(String::new()).scope(alice_scope());
    let hits = store.search(&q).await.expect("test result");
    assert_eq!(hits.len(), 2);
}

#[tokio::test]
async fn search_in_memory_orders_by_importance_when_relevance_tied() {
    assert_order((Some(0.9), 1), (Some(0.3), 0)).await;
}
#[tokio::test]
async fn search_in_memory_orders_by_recency_when_importance_tied() {
    assert_order((Some(0.5), 0), (Some(0.5), 1)).await;
}

#[tokio::test]
async fn search_in_memory_none_importance_uses_default() {
    assert_order((None, 1), (Some(0.3), 0)).await;
}

#[tokio::test]
async fn expired_record_filtered_default() {
    let store = MemoryMemoryStore::default();
    let mut doc = doc(alice_scope(), "expired note");
    doc.expires_at = Some(Utc::now() - Duration::minutes(1));
    store.store(&doc).await.expect("test result");
    let q = SearchQuery::new("expired").scope(alice_scope());
    assert!(store.search(&q).await.expect("test result").is_empty());
}

#[tokio::test]
async fn expired_record_visible_with_flag() {
    let store = MemoryMemoryStore::default();
    let mut doc = doc(alice_scope(), "expired note");
    doc.expires_at = Some(Utc::now() - Duration::minutes(1));
    store.store(&doc).await.expect("test result");
    let q = SearchQuery::new("expired")
        .scope(alice_scope())
        .include_expired();
    assert_eq!(store.search(&q).await.expect("test result").len(), 1);
}

#[tokio::test]
async fn archive_unarchive_roundtrip() {
    let store = MemoryMemoryStore::default();
    let id = store_doc(&store, alice_scope(), "archived note").await;
    assert!(store.archive(&id, Utc::now()).await.expect("test result"));
    let default_q = SearchQuery::new("archived").scope(alice_scope());
    assert!(
        store
            .search(&default_q)
            .await
            .expect("test result")
            .is_empty()
    );

    let archived_q = SearchQuery::new("archived")
        .scope(alice_scope())
        .include_archived();
    assert_eq!(
        store.search(&archived_q).await.expect("test result").len(),
        1
    );
    assert!(store.unarchive(&id).await.expect("test result"));
    assert_eq!(
        store.search(&default_q).await.expect("test result").len(),
        1
    );
}

#[tokio::test]
async fn extend_expiry_updates_doc() {
    let store = MemoryMemoryStore::default();
    let id = store_doc(&store, alice_scope(), "expiry note").await;
    let expires_at = Utc::now() + Duration::hours(2);
    assert!(
        store
            .extend_expiry(&id, Some(expires_at))
            .await
            .expect("test result")
    );
    let doc = store
        .get(&id)
        .await
        .expect("test result")
        .expect("doc exists");
    assert_eq!(doc.expires_at, Some(expires_at));
    assert!(store.extend_expiry(&id, None).await.expect("test result"));
    let doc = store
        .get(&id)
        .await
        .expect("test result")
        .expect("doc exists");
    assert!(doc.expires_at.is_none());
}

#[tokio::test]
async fn class_filter_in_search() {
    let store = MemoryMemoryStore::default();
    let mut semantic = doc(alice_scope(), "shared memory");
    semantic.class = Some("semantic".to_owned());
    let mut episodic = doc(alice_scope(), "shared memory");
    episodic.class = Some("episodic".to_owned());
    store.store(&semantic).await.expect("test result");
    store.store(&episodic).await.expect("test result");

    let q = SearchQuery::new("shared")
        .scope(alice_scope())
        .class("episodic");
    let hits = store.search(&q).await.expect("test result");
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].id, episodic.id);
}
