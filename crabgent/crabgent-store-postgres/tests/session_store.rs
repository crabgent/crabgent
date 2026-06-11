//! `SessionStore` integration tests.
//!
//! Each test uses `postgres_test_ctx()`. Container mode gets a fresh database;
//! `PG_TEST_DSN` mode stays idempotent through unique owner prefixes.

#[path = "../src/test_helpers.rs"]
mod test_helpers;

use chrono::{Duration, Utc};
use crabgent_core::{
    ContentBlock, MemoryScope, Message, Owner, ReasoningEffort, SearchQuery, ThreadId,
};
use crabgent_store::{Page, SessionStore};
use crabgent_store_postgres::PostgresStore;
use test_helpers::postgres_test_ctx;
use uuid::Uuid;

fn owner(test_name: &str) -> Owner {
    Owner::new(format!("pg-session-{test_name}-{}", Uuid::now_v7()))
}

fn user_message(text: &str) -> Message {
    Message::User {
        content: vec![ContentBlock::Text { text: text.into() }],
        timestamp: None,
    }
}

#[tokio::test]
async fn session_find_or_create() {
    let ctx = postgres_test_ctx().await;
    let store = PostgresStore::from_pool(ctx.pool.clone());
    let owner = owner("find-or-create");
    let thread = ThreadId::new("thread-a");

    let first = store
        .session_store()
        .find_or_create(&owner, Some(&thread), &MemoryScope::default())
        .await
        .expect("test result");
    let second = store
        .session_store()
        .find_or_create(&owner, Some(&thread), &MemoryScope::default())
        .await
        .expect("test result");

    assert_eq!(first.id, second.id);
    assert_eq!(second.owner, owner);
    assert_eq!(second.thread.as_ref(), Some(&thread));
}

#[tokio::test]
async fn session_save_load_roundtrip() {
    let ctx = postgres_test_ctx().await;
    let store = PostgresStore::from_pool(ctx.pool.clone());
    let owner = owner("save-load");
    let mut session = store
        .session_store()
        .find_or_create(&owner, None, &MemoryScope::default())
        .await
        .expect("test result");
    session.title = Some("Support thread".into());
    session.summary = Some("Resolved".into());
    session.model_override = Some("claude-opus".into());
    session.messages = vec![user_message("alpha support transcript")];
    session.updated_at = Utc::now();

    store
        .session_store()
        .save(&session)
        .await
        .expect("test result");
    let loaded = store
        .session_store()
        .load(&session.id)
        .await
        .expect("test result")
        .expect("session exists");

    assert_eq!(loaded.title.as_deref(), Some("Support thread"));
    assert_eq!(loaded.summary.as_deref(), Some("Resolved"));
    assert_eq!(loaded.messages.len(), 1);
    assert_eq!(loaded.owner, owner);
}

#[tokio::test]
async fn save_messages_preserves_other_columns() {
    let ctx = postgres_test_ctx().await;
    let store = PostgresStore::from_pool(ctx.pool.clone());
    let owner = owner("save-messages-preserve");
    let mut session = store
        .session_store()
        .find_or_create(&owner, None, &MemoryScope::default())
        .await
        .expect("test result");
    session.title = Some("Support thread".into());
    session.summary = Some("Resolved".into());
    session.compaction_summary = Some("prior compacted state".into());
    session.model_override = Some("claude-opus-4-6".into());
    session.reasoning_effort_override = Some(ReasoningEffort::Medium);
    session.messages = vec![user_message("original transcript body")];
    store
        .session_store()
        .save(&session)
        .await
        .expect("test result");

    let new_messages = vec![user_message("post tool body")];
    let updated_at = Utc::now();
    store
        .session_store()
        .save_messages(&session.id, &new_messages, updated_at)
        .await
        .expect("test result");
    let loaded = store
        .session_store()
        .load(&session.id)
        .await
        .expect("test result")
        .expect("session exists");

    assert_eq!(loaded.messages.len(), 1);
    match loaded.messages.as_slice() {
        [Message::User { content, .. }] => match content.as_slice() {
            [ContentBlock::Text { text }] => assert_eq!(text, "post tool body"),
            _ => panic!("expected one text content block"),
        },
        _ => panic!("expected one user message"),
    }
    assert_eq!(loaded.title.as_deref(), Some("Support thread"));
    assert_eq!(loaded.summary.as_deref(), Some("Resolved"));
    assert_eq!(
        loaded.compaction_summary.as_deref(),
        Some("prior compacted state")
    );
    assert_eq!(loaded.model_override.as_deref(), Some("claude-opus-4-6"));
    assert_eq!(
        loaded.reasoning_effort_override,
        Some(ReasoningEffort::Medium)
    );
    assert_eq!(loaded.created_at, session.created_at);
    assert_eq!(loaded.updated_at, updated_at);

    let hits = store
        .session_store()
        .search(&SearchQuery::new("post tool").scope(MemoryScope::for_owner(owner)))
        .await
        .expect("test result");
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].session_id, session.id);
}

#[tokio::test]
async fn session_list_paginated() {
    let ctx = postgres_test_ctx().await;
    let store = PostgresStore::from_pool(ctx.pool.clone());
    let owner = owner("list");
    for i in 0..3 {
        let thread = ThreadId::new(format!("thread-{i}"));
        let mut session = store
            .session_store()
            .find_or_create(&owner, Some(&thread), &MemoryScope::default())
            .await
            .expect("test result");
        session.title = Some(format!("session-{i}"));
        session.updated_at = Utc::now() + Duration::seconds(i);
        store
            .session_store()
            .save(&session)
            .await
            .expect("test result");
    }

    let first_page = store
        .session_store()
        .list(&owner, Page::first(2))
        .await
        .expect("test result");
    let second_page = store
        .session_store()
        .list(&owner, Page::new(2, 2))
        .await
        .expect("test result");

    assert_eq!(first_page.len(), 2);
    assert_eq!(second_page.len(), 1);
    assert_ne!(first_page[0].id, first_page[1].id);
}

#[tokio::test]
async fn session_cleanup_old() {
    let ctx = postgres_test_ctx().await;
    let store = PostgresStore::from_pool(ctx.pool.clone());
    let owner = owner("cleanup");
    let mut stale = store
        .session_store()
        .find_or_create(&owner, None, &MemoryScope::default())
        .await
        .expect("test result");
    stale.updated_at = Utc::now() - Duration::days(40);
    store
        .session_store()
        .save(&stale)
        .await
        .expect("test result");
    let mut fresh = store
        .session_store()
        .find_or_create(
            &owner,
            Some(&ThreadId::new("fresh")),
            &MemoryScope::default(),
        )
        .await
        .expect("test result");
    fresh.updated_at = Utc::now();
    store
        .session_store()
        .save(&fresh)
        .await
        .expect("test result");

    let removed = store
        .session_store()
        .cleanup_old(30)
        .await
        .expect("test result");

    assert!(removed >= 1);
    assert!(
        store
            .session_store()
            .load(&stale.id)
            .await
            .expect("test result")
            .is_none()
    );
    assert!(
        store
            .session_store()
            .load(&fresh.id)
            .await
            .expect("test result")
            .is_some()
    );
}

#[tokio::test]
async fn fts_session_search_returns_hit() {
    let ctx = postgres_test_ctx().await;
    let store = PostgresStore::from_pool(ctx.pool.clone());
    let owner = owner("fts-hit");
    let mut session = store
        .session_store()
        .find_or_create(&owner, None, &MemoryScope::default())
        .await
        .expect("test result");
    session.scope = MemoryScope::for_owner(owner.clone()).with_channel("slack");
    session.messages = vec![user_message("Customer mentioned evening delivery windows")];
    session.updated_at = Utc::now();
    store
        .session_store()
        .save(&session)
        .await
        .expect("test result");

    let query =
        SearchQuery::new("evening").scope(MemoryScope::for_owner(owner).with_channel("slack"));
    let hits = store
        .session_store()
        .search(&query)
        .await
        .expect("test result");

    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].session_id, session.id);
}

#[tokio::test]
async fn fts_session_search_zero_hits_for_unrelated() {
    let ctx = postgres_test_ctx().await;
    let store = PostgresStore::from_pool(ctx.pool.clone());
    let owner = owner("fts-zero");
    let mut session = store
        .session_store()
        .find_or_create(&owner, None, &MemoryScope::default())
        .await
        .expect("test result");
    session.messages = vec![user_message("Only calendar planning appears here")];
    store
        .session_store()
        .save(&session)
        .await
        .expect("test result");

    let query = SearchQuery::new("invoice").scope(MemoryScope::for_owner(owner));
    let hits = store
        .session_store()
        .search(&query)
        .await
        .expect("test result");

    assert!(hits.is_empty());
}
