use super::*;
use crate::backend::SqliteStore;
use crabgent_core::{ContentBlock, MemoryScope, ReasoningEffort};
use crabgent_store::{SessionStore, Store, session_support::new_empty_session};
use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

async fn store() -> SqliteStore {
    SqliteStore::open_in_memory().await.expect("open store")
}

async fn store_with_pool(max_connections: u32) -> (SqliteStore, SqlitePool) {
    let opts = SqliteConnectOptions::new()
        .in_memory(true)
        .shared_cache(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(max_connections)
        .connect_with(opts)
        .await
        .expect("open test pool");
    let store = SqliteStore::from_pool(pool.clone()).await.expect("migrate");
    (store, pool)
}

fn session_with_text(owner: &str, body: &str) -> Session {
    let owner = Owner::new(owner);
    let mut session = new_empty_session(&owner, None, Utc::now());
    session.messages = vec![Message::User {
        content: vec![ContentBlock::Text {
            text: body.to_owned(),
        }],
        timestamp: None,
    }];
    session
}

async fn session_count(pool: &SqlitePool, id: &SessionId) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM sessions WHERE id = ?")
        .bind(id.to_string())
        .fetch_one(pool)
        .await
        .expect("count sessions")
}

async fn search_body(pool: &SqlitePool, id: &SessionId) -> String {
    sqlx::query_scalar("SELECT body FROM session_search WHERE session_id = ?")
        .bind(id.to_string())
        .fetch_one(pool)
        .await
        .expect("read session search body")
}

#[tokio::test]
async fn find_or_create_round_trips_through_save() {
    let s = store().await;
    let owner = Owner::new("u1");
    let a = s
        .session()
        .find_or_create(&owner, None, &MemoryScope::default())
        .await
        .expect("create");
    let b = s
        .session()
        .find_or_create(&owner, None, &MemoryScope::default())
        .await
        .expect("find");
    assert_eq!(a.id, b.id);
}

#[tokio::test]
async fn save_persists_messages_via_json_column() {
    let s = store().await;
    let owner = Owner::new("u");
    let mut session = s
        .session()
        .find_or_create(&owner, None, &MemoryScope::default())
        .await
        .expect("test result");
    session.messages.push(Message::User {
        content: vec![ContentBlock::Text { text: "hi".into() }],
        timestamp: None,
    });
    session.title = Some("greeting".into());
    s.session().save(&session).await.expect("test result");
    let loaded = s
        .session()
        .load(&session.id)
        .await
        .expect("test result")
        .expect("test result");
    assert_eq!(loaded.messages.len(), 1);
    assert_eq!(loaded.title.as_deref(), Some("greeting"));
}

#[tokio::test]
async fn session_save_happy_path_updates_search_index() {
    let (s, pool) = store_with_pool(1).await;
    let session = session_with_text("u", "searchable transaction body");

    s.session().save(&session).await.expect("test result");

    assert_eq!(session_count(&pool, &session.id).await, 1);
    assert!(
        search_body(&pool, &session.id)
            .await
            .contains("searchable transaction body")
    );
}

#[tokio::test]
async fn save_messages_preserves_other_columns() {
    let s = store().await;
    let owner = Owner::new("u-preserve");
    let mut session = s
        .session()
        .find_or_create(&owner, None, &MemoryScope::default())
        .await
        .expect("test result");
    session.title = Some("greeting".into());
    session.summary = Some("done".into());
    session.compaction_summary = Some("prior compacted state".into());
    session.model_override = Some("claude-opus-4-6".into());
    session.reasoning_effort_override = Some(ReasoningEffort::High);
    s.session().save(&session).await.expect("test result");

    // Simulate the hook flushing a new message list.
    let new_msgs = vec![Message::User {
        content: vec![ContentBlock::Text {
            text: "post-tool body".into(),
        }],
        timestamp: None,
    }];
    let now = Utc::now();
    s.session()
        .save_messages(&session.id, &new_msgs, now)
        .await
        .expect("save_messages");

    let loaded = s
        .session()
        .load(&session.id)
        .await
        .expect("test result")
        .expect("test result");
    assert_eq!(loaded.messages.len(), 1);
    assert_eq!(loaded.title.as_deref(), Some("greeting"));
    assert_eq!(loaded.summary.as_deref(), Some("done"));
    assert_eq!(
        loaded.compaction_summary.as_deref(),
        Some("prior compacted state")
    );
    assert_eq!(loaded.model_override.as_deref(), Some("claude-opus-4-6"));
    assert_eq!(
        loaded.reasoning_effort_override,
        Some(ReasoningEffort::High)
    );
    assert!(loaded.updated_at >= now - Duration::seconds(1));
}

#[tokio::test]
async fn compaction_summary_round_trips() {
    let s = store().await;
    let owner = Owner::new("u-summary");
    let session = s
        .session()
        .find_or_create(&owner, None, &MemoryScope::default())
        .await
        .expect("test result");

    s.session()
        .set_compaction_summary(&session.id, "prior compacted state")
        .await
        .expect("set compaction summary");

    let summary = s
        .session()
        .get_compaction_summary(&session.id)
        .await
        .expect("get compaction summary");
    assert_eq!(summary.as_deref(), Some("prior compacted state"));
}

#[tokio::test]
async fn save_messages_updates_search_body() {
    let (s, pool) = store_with_pool(1).await;
    let session = session_with_text("u-fts", "stale body");
    s.session().save(&session).await.expect("test result");
    let new_msgs = vec![Message::User {
        content: vec![ContentBlock::Text {
            text: "fresh searchable body".into(),
        }],
        timestamp: None,
    }];
    s.session()
        .save_messages(&session.id, &new_msgs, Utc::now())
        .await
        .expect("save_messages");
    let body = search_body(&pool, &session.id).await;
    assert!(
        body.contains("fresh searchable body"),
        "FTS body should reflect the new messages, got {body:?}"
    );
}

#[tokio::test]
async fn save_messages_returns_not_found_for_unknown_id() {
    let s = store().await;
    let id = SessionId::new();
    let err = s
        .session()
        .save_messages(&id, &[], Utc::now())
        .await
        .expect_err("missing session");
    assert!(matches!(err, StoreError::NotFound), "got {err:?}");
}

#[tokio::test]
async fn session_save_atomic_rollback_on_search_failure() {
    let (s, pool) = store_with_pool(1).await;
    sqlx::query(
        "CREATE TRIGGER fail_session_search_insert \
         BEFORE INSERT ON session_search \
         BEGIN \
         SELECT RAISE(ABORT, 'forced session_search failure'); \
         END",
    )
    .execute(&pool)
    .await
    .expect("create failure trigger");
    let session = session_with_text("u", "rollback body");

    let err = s.session().save(&session).await.expect_err("trigger fails");

    assert!(
        matches!(err, StoreError::Backend(ref msg) if msg.contains("forced session_search failure"))
    );
    assert_eq!(session_count(&pool, &session.id).await, 0);
    assert!(
        s.session()
            .load(&session.id)
            .await
            .expect("test result")
            .is_none()
    );
}

#[tokio::test]
async fn session_save_concurrent_same_session_id() {
    let (s, _pool) = store_with_pool(1).await;
    let mut left = session_with_text("u", "concurrent left");
    left.title = Some("left".into());
    let mut right = left.clone();
    right.messages = vec![Message::User {
        content: vec![ContentBlock::Text {
            text: "concurrent right".into(),
        }],
        timestamp: None,
    }];
    right.title = Some("right".into());
    let left_store = s.session().clone();
    let right_store = s.session().clone();

    let (left_result, right_result) =
        tokio::join!(left_store.save(&left), right_store.save(&right));

    left_result.expect("test result");
    right_result.expect("test result");
    let loaded = s
        .session()
        .load(&left.id)
        .await
        .expect("test result")
        .expect("test result");
    assert!(matches!(loaded.title.as_deref(), Some("left" | "right")));
}

#[tokio::test]
async fn list_returns_message_count_and_summary_flag() {
    let s = store().await;
    let owner = Owner::new("u");
    let mut session = s
        .session()
        .find_or_create(&owner, None, &MemoryScope::default())
        .await
        .expect("test result");
    session.messages.push(Message::User {
        content: vec![ContentBlock::Text { text: "x".into() }],
        timestamp: None,
    });
    session.summary = Some("done".into());
    s.session().save(&session).await.expect("test result");
    let listed = s
        .session()
        .list(&owner, Page::first(10))
        .await
        .expect("test result");
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].message_count, 1);
    assert!(listed[0].has_summary);
}

#[tokio::test]
async fn distinct_threads_yield_distinct_sessions() {
    let s = store().await;
    let owner = Owner::new("u");
    let a = s
        .session()
        .find_or_create(&owner, None, &MemoryScope::default())
        .await
        .expect("test result");
    let t = ThreadId::new("t1");
    let b = s
        .session()
        .find_or_create(&owner, Some(&t), &MemoryScope::default())
        .await
        .expect("test result");
    assert_ne!(a.id, b.id);
}

#[tokio::test]
async fn cleanup_old_deletes_stale_sessions() {
    let s = store().await;
    let owner = Owner::new("u");
    let mut session = s
        .session()
        .find_or_create(&owner, None, &MemoryScope::default())
        .await
        .expect("test result");
    session.updated_at = Utc::now() - Duration::days(30);
    s.session().save(&session).await.expect("test result");
    let removed = s.session().cleanup_old(7).await.expect("test result");
    assert_eq!(removed, 1);
}

#[tokio::test]
async fn load_unknown_returns_none() {
    let s = store().await;
    let unknown = SessionId::new();
    assert!(
        s.session()
            .load(&unknown)
            .await
            .expect("test result")
            .is_none()
    );
}

async fn save_user_msg(s: &SqliteStore, who: &str, body: &str) -> SessionId {
    let owner = Owner::new(who);
    let mut session = s
        .session()
        .find_or_create(&owner, None, &MemoryScope::default())
        .await
        .expect("test result");
    session.messages.push(Message::User {
        content: vec![ContentBlock::Text {
            text: body.to_owned(),
        }],
        timestamp: None,
    });
    s.session().save(&session).await.expect("test result");
    session.id
}

async fn save_scoped_user_msg(s: &SqliteStore, scope: MemoryScope, body: &str) -> SessionId {
    let owner = scope.owner.clone().expect("test scope owner");
    let thread = scope.conv.as_ref().map(ThreadId::new);
    let mut session = new_empty_session(&owner, thread.as_ref(), Utc::now()).with_scope(scope);
    session.messages = vec![Message::User {
        content: vec![ContentBlock::Text {
            text: body.to_owned(),
        }],
        timestamp: None,
    }];
    s.session().save(&session).await.expect("test result");
    session.id
}

#[tokio::test]
async fn search_finds_owner_scoped_text_via_fts5() {
    let s = store().await;
    save_user_msg(&s, "alice", "the cat is named whiskers").await;
    save_user_msg(&s, "bob", "the cat is named whiskers").await;
    let q = SearchQuery::new("whiskers")
        .scope(crabgent_core::MemoryScope::for_owner(Owner::new("alice")));
    let hits = s.session().search(&q).await.expect("test result");
    assert_eq!(hits.len(), 1);
    assert!(hits[0].excerpt.to_lowercase().contains("whiskers"));
}

#[tokio::test]
async fn search_filters_by_channel_scope() {
    let s = store().await;
    let slack = MemoryScope::for_owner(Owner::new("alice")).with_channel("slack");
    let telegram = MemoryScope::for_owner(Owner::new("alice")).with_channel("telegram");
    let slack_id = save_scoped_user_msg(&s, slack.clone(), "shared channel note").await;
    save_scoped_user_msg(&s, telegram, "shared channel note").await;

    let q = SearchQuery::new("shared").scope(slack);
    let hits = s.session().search(&q).await.expect("test result");

    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].session_id, slack_id);
}

#[tokio::test]
async fn search_filters_by_conv_scope() {
    let s = store().await;
    let conv_a = MemoryScope::for_owner(Owner::new("alice")).with_conv("conv-a");
    let conv_b = MemoryScope::for_owner(Owner::new("alice")).with_conv("conv-b");
    let conv_a_id = save_scoped_user_msg(&s, conv_a.clone(), "shared conv note").await;
    save_scoped_user_msg(&s, conv_b, "shared conv note").await;

    let q = SearchQuery::new("shared").scope(conv_a);
    let hits = s.session().search(&q).await.expect("test result");

    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].session_id, conv_a_id);
}

#[tokio::test]
async fn fts_operator_text_is_quoted_as_phrase() {
    let s = store().await;
    let scope = MemoryScope::for_owner(Owner::new("alice"));
    let literal_id = save_scoped_user_msg(&s, scope.clone(), "literal X OR foo:1 phrase").await;
    save_scoped_user_msg(
        &s,
        scope.clone().with_conv("other"),
        "X appears separately from foo",
    )
    .await;

    let q = SearchQuery::new("X OR foo:1").scope(scope);
    let hits = s.session().search(&q).await.expect("test result");

    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].session_id, literal_id);
}

#[tokio::test]
async fn fts_special_chars_do_not_crash() {
    let s = store().await;
    save_user_msg(&s, "alice", "safe body").await;
    let q = SearchQuery::new("\"unterminated NEAR/5 foo:bar \\")
        .scope(MemoryScope::for_owner(Owner::new("alice")));
    let hits = s.session().search(&q).await.expect("test result");

    assert!(hits.is_empty());
}

#[tokio::test]
async fn search_global_scope_matches_any_owner() {
    let s = store().await;
    save_user_msg(&s, "alice", "shared phrase here").await;
    save_user_msg(&s, "bob", "shared phrase here").await;
    let q = SearchQuery::new("shared").scope(crabgent_core::MemoryScope::global());
    let hits = s.session().search(&q).await.expect("test result");
    assert_eq!(hits.len(), 2);
}

#[tokio::test]
async fn search_empty_query_lists_recent_in_owner_scope() {
    let s = store().await;
    save_user_msg(&s, "alice", "first").await;
    save_user_msg(&s, "alice", "second").await;
    save_user_msg(&s, "bob", "ignored").await;
    let q = SearchQuery::new(String::new())
        .scope(crabgent_core::MemoryScope::for_owner(Owner::new("alice")));
    let hits = s.session().search(&q).await.expect("test result");
    assert_eq!(hits.len(), 1);
}

#[tokio::test]
async fn search_no_match_returns_empty() {
    let s = store().await;
    save_user_msg(&s, "alice", "yes no maybe").await;
    let q = SearchQuery::new("absent")
        .scope(crabgent_core::MemoryScope::for_owner(Owner::new("alice")));
    let hits = s.session().search(&q).await.expect("test result");
    assert!(hits.is_empty());
}

#[tokio::test]
async fn list_rejects_offset_out_of_range() {
    let s = store().await;
    let owner = Owner::new("u");
    let err = s
        .session()
        .list(&owner, Page::new(10, usize::MAX))
        .await
        .expect_err("offset overflow should be rejected");

    assert!(matches!(err, StoreError::Invalid(ref msg) if msg == "page.offset out of range"));
}
