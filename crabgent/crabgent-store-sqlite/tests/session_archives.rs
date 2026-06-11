use chrono::{Duration, Utc};
use crabgent_core::MemoryScope;
use crabgent_core::{ContentBlock, Message, Owner};
use crabgent_store::{Page, SessionId, SessionStore, Store, StoreError};
use crabgent_store_sqlite::SqliteStore;

async fn store() -> SqliteStore {
    SqliteStore::open_in_memory().await.expect("open store")
}

fn user_message(text: &str) -> Message {
    Message::User {
        content: vec![ContentBlock::Text { text: text.into() }],
        timestamp: None,
    }
}

fn first_user_text(messages: &[Message]) -> &str {
    first_user_text_maybe(messages).expect("archive should contain user text message")
}

fn first_user_text_maybe(messages: &[Message]) -> Option<&str> {
    let Some(Message::User { content, .. }) = messages.first() else {
        return None;
    };
    let Some(ContentBlock::Text { text }) = content.first() else {
        return None;
    };
    Some(text)
}

#[tokio::test]
async fn sqlite_archives_and_lists_messages() {
    let store = store().await;
    let owner = Owner::new("sqlite-archive-owner");
    let session = store
        .session()
        .find_or_create(&owner, None, &MemoryScope::default())
        .await
        .expect("create session");
    let old = Utc::now() - Duration::minutes(5);
    let new = Utc::now();

    let old_id = store
        .session()
        .archive_messages(&session.id, &[user_message("old")], old)
        .await
        .expect("archive old messages");
    let new_id = store
        .session()
        .archive_messages(&session.id, &[user_message("new")], new)
        .await
        .expect("archive new messages");

    let archives = store
        .session()
        .list_archives(&session.id, Page::first(10))
        .await
        .expect("list archives");

    assert_eq!(archives.len(), 2);
    let newest = archives
        .first()
        .expect("archive list should contain newest entry");
    let oldest = archives
        .get(1)
        .expect("archive list should contain oldest entry");
    assert_eq!(newest.id, new_id);
    assert_eq!(oldest.id, old_id);
    assert_eq!(first_user_text(&newest.messages), "new");
}

#[tokio::test]
async fn sqlite_archive_missing_session_returns_not_found() {
    let store = store().await;
    let err = store
        .session()
        .archive_messages(&SessionId::new(), &[user_message("missing")], Utc::now())
        .await
        .expect_err("missing session");

    assert!(matches!(err, StoreError::NotFound));
}

#[tokio::test]
async fn sqlite_cleanup_old_archives_removes_stale_entries() {
    let store = store().await;
    let owner = Owner::new("sqlite-archive-cleanup-owner");
    let session = store
        .session()
        .find_or_create(&owner, None, &MemoryScope::default())
        .await
        .expect("create session");

    store
        .session()
        .archive_messages(
            &session.id,
            &[user_message("stale")],
            Utc::now() - Duration::days(40),
        )
        .await
        .expect("archive stale messages");
    store
        .session()
        .archive_messages(&session.id, &[user_message("fresh")], Utc::now())
        .await
        .expect("archive fresh messages");

    let removed = store
        .session()
        .cleanup_old_archives(30)
        .await
        .expect("cleanup old archives");
    let archives = store
        .session()
        .list_archives(&session.id, Page::first(10))
        .await
        .expect("list archives");

    assert_eq!(removed, 1);
    assert_eq!(archives.len(), 1);
    let remaining = archives
        .first()
        .expect("archive list should contain remaining fresh entry");
    assert_eq!(first_user_text(&remaining.messages), "fresh");
}
