use super::*;
use chrono::TimeZone;
use crabgent_core::{ContentBlock, MemoryScope, Message};

fn owner(s: &str) -> Owner {
    Owner::new(s)
}

#[tokio::test]
async fn find_or_create_creates_when_missing() {
    let store = MemorySessionStore::default();
    let s = store
        .find_or_create(&owner("u1"), None, &MemoryScope::default())
        .await
        .expect("test result");
    assert_eq!(s.owner, owner("u1"));
    assert!(s.messages.is_empty());
}

#[tokio::test]
async fn find_or_create_returns_existing() {
    let store = MemorySessionStore::default();
    let a = store
        .find_or_create(&owner("u1"), None, &MemoryScope::default())
        .await
        .expect("test result");
    let b = store
        .find_or_create(&owner("u1"), None, &MemoryScope::default())
        .await
        .expect("test result");
    assert_eq!(a.id, b.id);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn find_or_create_concurrent_resolves_to_one_row() {
    use std::sync::Arc;
    let store = Arc::new(MemorySessionStore::default());
    let owner = owner("u-race");
    let store_a = Arc::clone(&store);
    let owner_a = owner.clone();
    let store_b = Arc::clone(&store);
    let owner_b = owner.clone();
    let handle_a = tokio::spawn(async move {
        store_a
            .find_or_create(&owner_a, None, &MemoryScope::default())
            .await
    });
    let handle_b = tokio::spawn(async move {
        store_b
            .find_or_create(&owner_b, None, &MemoryScope::default())
            .await
    });
    let a = handle_a.await.expect("task a").expect("find_or_create a");
    let b = handle_b.await.expect("task b").expect("find_or_create b");
    assert_eq!(
        a.id, b.id,
        "concurrent find_or_create on Mutex-guarded map must converge",
    );
    let infos = store
        .list(&owner, Page::first(10))
        .await
        .expect("list result");
    assert_eq!(
        infos.len(),
        1,
        "exactly one persisted row expected for shared scope tuple",
    );
}

#[tokio::test]
async fn find_or_create_separate_for_distinct_threads() {
    let store = MemorySessionStore::default();
    let a = store
        .find_or_create(&owner("u"), None, &MemoryScope::default())
        .await
        .expect("test result");
    let t = ThreadId::new("t1");
    let b = store
        .find_or_create(&owner("u"), Some(&t), &MemoryScope::default())
        .await
        .expect("test result");
    assert_ne!(a.id, b.id);
}

#[tokio::test]
async fn save_messages_preserves_sibling_columns() {
    let store = MemorySessionStore::default();
    let mut s = store
        .find_or_create(&owner("u"), None, &MemoryScope::default())
        .await
        .expect("test result");
    s.title = Some("greeting".into());
    s.compaction_summary = Some("prior compacted state".into());
    s.model_override = Some("claude-opus-4-6".into());
    store.save(&s).await.expect("test result");

    let new_msgs = vec![Message::User {
        content: vec![ContentBlock::Text {
            text: "post-tool".into(),
        }],
        timestamp: None,
    }];
    let now = Utc::now();
    store
        .save_messages(&s.id, &new_msgs, now)
        .await
        .expect("test result");

    let loaded = store
        .load(&s.id)
        .await
        .expect("test result")
        .expect("test result");
    assert_eq!(loaded.messages.len(), 1);
    assert_eq!(loaded.title.as_deref(), Some("greeting"));
    assert_eq!(
        loaded.compaction_summary.as_deref(),
        Some("prior compacted state")
    );
    assert_eq!(loaded.model_override.as_deref(), Some("claude-opus-4-6"));
    assert_eq!(loaded.updated_at, now);
}

#[tokio::test]
async fn save_messages_returns_not_found_for_unknown_id() {
    let store = MemorySessionStore::default();
    let id = SessionId::new();
    let err = store
        .save_messages(&id, &[], Utc::now())
        .await
        .expect_err("missing session");
    assert!(matches!(err, StoreError::NotFound));
}

#[tokio::test]
async fn save_and_load_round_trip() {
    let store = MemorySessionStore::default();
    let mut s = store
        .find_or_create(&owner("u"), None, &MemoryScope::default())
        .await
        .expect("test result");
    s.title = Some("hi".into());
    store.save(&s).await.expect("test result");
    let loaded = store
        .load(&s.id)
        .await
        .expect("test result")
        .expect("test result");
    assert_eq!(loaded.title.as_deref(), Some("hi"));
}

#[tokio::test]
async fn list_orders_by_updated_at_desc() {
    let store = MemorySessionStore::default();
    let s1 = store
        .find_or_create(&owner("u"), None, &MemoryScope::default())
        .await
        .expect("test result");
    let mut s2 = Session {
        updated_at: Utc::now() + Duration::seconds(1),
        ..s1.clone()
    };
    s2.id = SessionId::new();
    store.save(&s2).await.expect("test result");
    let listed = store
        .list(&owner("u"), Page::first(10))
        .await
        .expect("test result");
    assert_eq!(listed.len(), 2);
    assert_eq!(listed[0].id, s2.id);
}

#[tokio::test]
async fn list_paginates_via_offset_and_limit() {
    let store = MemorySessionStore::default();
    for i in 0..5 {
        let s = Session {
            id: SessionId::new(),
            owner: owner("u"),
            scope: MemoryScope::for_owner(owner("u")).with_conv(format!("t{i}")),
            thread: Some(ThreadId::new(format!("t{i}"))),
            title: None,
            summary: None,
            compaction_summary: None,
            model_override: None,
            reasoning_effort_override: None,
            messages: Vec::new(),
            created_at: Utc::now(),
            updated_at: Utc::now() + Duration::seconds(i),
        };
        store.save(&s).await.expect("test result");
    }
    let first = store
        .list(&owner("u"), Page::new(2, 0))
        .await
        .expect("test result");
    let next = store
        .list(&owner("u"), Page::new(2, 2))
        .await
        .expect("test result");
    assert_eq!(first.len(), 2);
    assert_eq!(next.len(), 2);
    assert_ne!(first[0].id, next[0].id);
}

#[tokio::test]
async fn cleanup_old_removes_stale_sessions() {
    let store = MemorySessionStore::default();
    let old = Session {
        id: SessionId::new(),
        owner: owner("u"),
        scope: MemoryScope::for_owner(owner("u")),
        thread: None,
        title: None,
        summary: None,
        compaction_summary: None,
        model_override: None,
        reasoning_effort_override: None,
        messages: Vec::new(),
        created_at: Utc
            .with_ymd_and_hms(2020, 1, 1, 0, 0, 0)
            .single()
            .expect("valid test datetime"),
        updated_at: Utc
            .with_ymd_and_hms(2020, 1, 1, 0, 0, 0)
            .single()
            .expect("valid test datetime"),
    };
    store.save(&old).await.expect("test result");
    let _fresh = store
        .find_or_create(&owner("u"), None, &MemoryScope::default())
        .await
        .expect("test result");
    let removed = store.cleanup_old(7).await.expect("test result");
    assert_eq!(removed, 1);
}

fn session_with_text(o: &str, body: &str) -> Session {
    let now = Utc::now();
    Session {
        id: SessionId::new(),
        owner: owner(o),
        scope: MemoryScope::for_owner(owner(o)),
        thread: None,
        title: None,
        summary: None,
        compaction_summary: None,
        model_override: None,
        reasoning_effort_override: None,
        messages: vec![Message::User {
            content: vec![ContentBlock::Text {
                text: body.to_owned(),
            }],
            timestamp: None,
        }],
        created_at: now,
        updated_at: now,
    }
}

#[tokio::test]
async fn search_filters_by_channel_scope() {
    let store = MemorySessionStore::default();
    let mut slack = session_with_text("alice", "shared session note");
    slack.scope = MemoryScope::for_owner(owner("alice")).with_channel("slack");
    let mut telegram = session_with_text("alice", "shared session note");
    telegram.scope = MemoryScope::for_owner(owner("alice")).with_channel("telegram");
    store.save(&slack).await.expect("test result");
    store.save(&telegram).await.expect("test result");

    let q = SearchQuery::new("shared")
        .scope(MemoryScope::for_owner(owner("alice")).with_channel("slack"));
    let hits = store.search(&q).await.expect("test result");

    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].session_id, slack.id);
}

#[tokio::test]
async fn search_filters_by_conv_scope() {
    let store = MemorySessionStore::default();
    let mut first = session_with_text("alice", "shared session note");
    first.scope = MemoryScope::for_owner(owner("alice")).with_conv("conv-a");
    let mut second = session_with_text("alice", "shared session note");
    second.scope = MemoryScope::for_owner(owner("alice")).with_conv("conv-b");
    store.save(&first).await.expect("test result");
    store.save(&second).await.expect("test result");

    let q = SearchQuery::new("shared")
        .scope(MemoryScope::for_owner(owner("alice")).with_conv("conv-a"));
    let hits = store.search(&q).await.expect("test result");

    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].session_id, first.id);
}

#[tokio::test]
async fn search_finds_text_in_user_message() {
    let store = MemorySessionStore::default();
    store
        .save(&session_with_text(
            "alice",
            "remember the cat is named whiskers",
        ))
        .await
        .expect("test result");
    let q =
        SearchQuery::new("whiskers").scope(crabgent_core::MemoryScope::for_owner(owner("alice")));
    let hits = store.search(&q).await.expect("test result");
    assert_eq!(hits.len(), 1);
    assert!(hits[0].excerpt.to_lowercase().contains("whiskers"));
}

#[tokio::test]
async fn search_skips_other_owners_when_owner_set() {
    let store = MemorySessionStore::default();
    store
        .save(&session_with_text("alice", "secret token abc"))
        .await
        .expect("test result");
    store
        .save(&session_with_text("bob", "secret token abc"))
        .await
        .expect("test result");
    let q = SearchQuery::new("secret").scope(crabgent_core::MemoryScope::for_owner(owner("alice")));
    let hits = store.search(&q).await.expect("test result");
    assert_eq!(hits.len(), 1);
}

#[tokio::test]
async fn search_global_scope_matches_any_owner() {
    let store = MemorySessionStore::default();
    store
        .save(&session_with_text("alice", "shared phrase"))
        .await
        .expect("test result");
    store
        .save(&session_with_text("bob", "shared phrase"))
        .await
        .expect("test result");
    let q = SearchQuery::new("shared").scope(crabgent_core::MemoryScope::global());
    let hits = store.search(&q).await.expect("test result");
    assert_eq!(hits.len(), 2);
}

#[tokio::test]
async fn search_no_match_returns_empty() {
    let store = MemorySessionStore::default();
    store
        .save(&session_with_text("alice", "yes no maybe"))
        .await
        .expect("test result");
    let q = SearchQuery::new("absent").scope(crabgent_core::MemoryScope::for_owner(owner("alice")));
    let hits = store.search(&q).await.expect("test result");
    assert!(hits.is_empty());
}

#[tokio::test]
async fn search_respects_limit() {
    let store = MemorySessionStore::default();
    for i in 0..5 {
        store
            .save(&session_with_text("alice", &format!("note {i}")))
            .await
            .expect("test result");
    }
    let q = SearchQuery::new("note")
        .scope(crabgent_core::MemoryScope::for_owner(owner("alice")))
        .limit(2);
    let hits = store.search(&q).await.expect("test result");
    assert_eq!(hits.len(), 2);
}
