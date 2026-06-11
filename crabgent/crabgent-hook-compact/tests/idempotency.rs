use std::sync::Arc;

use crabgent_core::{ContentBlock, Decision, Hook, Message, RunCtx, RunId, Subject};
use crabgent_hook_compact::CompactHook;
use crabgent_test_support::{StubProvider, assistant, user_msg as user};

fn ctx() -> RunCtx {
    RunCtx::new(RunId::new(), Subject::new("u"))
}

fn user_text(message: &Message) -> Option<&str> {
    let Message::User { content, .. } = message else {
        return None;
    };
    match content.first() {
        Some(ContentBlock::Text { text }) => Some(text),
        _ => None,
    }
}

async fn compact_once(summary: &str, messages: Vec<Message>) -> Option<Vec<Message>> {
    let provider = Arc::new(StubProvider::with_text(summary));
    let hook = CompactHook::new(provider, "summary-model")
        .with_max_messages(1)
        .with_keep_recent_messages(1);
    replace_value(hook.pre_compact(&messages, &ctx()).await)
}

fn replace_value<T>(decision: Decision<T>) -> Option<T> {
    match decision {
        Decision::Continue | Decision::Deny(_) => None,
        Decision::Replace(next) => Some(next),
    }
}

#[tokio::test]
async fn summary_preserved_across_compactions() {
    let first_next = compact_once(
        "prior summary",
        vec![user("old request"), user("first latest")],
    )
    .await
    .expect("compaction should replace");
    let prior = first_next
        .first()
        .expect("first compaction should include summary")
        .clone();
    let prior_text = user_text(&prior)
        .expect("prior summary message should be user text")
        .to_owned();

    let second_provider = Arc::new(StubProvider::with_text("new summary"));
    let second_hook = CompactHook::new(Arc::clone(&second_provider), "summary-model")
        .with_max_messages(2)
        .with_keep_recent_messages(1);
    let second_messages = vec![
        prior,
        user("new old request"),
        assistant("new old answer"),
        user("second latest"),
    ];

    let second_next = replace_value(second_hook.pre_compact(&second_messages, &ctx()).await)
        .expect("second compaction should replace");

    assert_eq!(second_next.len(), 3);
    assert_eq!(
        user_text(
            second_next
                .first()
                .expect("first compacted message should exist")
        )
        .expect("first compacted message should be user text"),
        prior_text
    );
    assert!(
        user_text(
            second_next
                .get(1)
                .expect("second compacted message should exist")
        )
        .expect("second compacted message should be user text")
        .contains("new summary")
    );
    assert!(
        user_text(
            second_next
                .get(2)
                .expect("third compacted message should exist")
        )
        .expect("third compacted message should be user text")
        .contains("second latest")
    );

    let requests = second_provider.captured_requests();
    let first_request = requests
        .first()
        .expect("summary provider request should exist");
    let summary_input = first_request
        .messages
        .first()
        .expect("summary provider request message should exist")
        .to_string();
    assert!(!summary_input.contains("prior summary"));
    assert!(summary_input.contains("new old request"));
    assert!(summary_input.contains("new old answer"));
}

#[tokio::test]
async fn prior_summary_excluded_from_resummarize() {
    let first_next = compact_once(
        "prior summary",
        vec![user("old request"), user("latest request")],
    )
    .await
    .expect("compaction should replace");

    let second_provider = Arc::new(StubProvider::with_text("unused"));
    let second_hook = CompactHook::new(Arc::clone(&second_provider), "summary-model")
        .with_max_messages(1)
        .with_keep_recent_messages(1);

    let decision = second_hook.pre_compact(&first_next, &ctx()).await;

    assert!(matches!(decision, Decision::Continue));
    assert_eq!(second_provider.captured_requests().len(), 0);
}

#[tokio::test]
async fn pre_compact_after_canonical_replace_returns_continue_or_cached() {
    let provider = Arc::new(StubProvider::with_text("fixed summary"));
    let hook = CompactHook::new(Arc::clone(&provider), "summary-model")
        .with_max_messages(100)
        .with_max_tokens(100)
        .with_keep_recent_messages(0);
    let ctx = ctx();
    let old_one = format!("old-1 {}", "context ".repeat(200));
    let old_two = format!("old-2 {}", "context ".repeat(200));
    let old_three = format!("old-3 {}", "context ".repeat(200));

    let compacted = replace_value(
        hook.pre_compact(&[user(&old_one), user(&old_two), user(&old_three)], &ctx)
            .await,
    )
    .expect("first compaction should replace long history");

    assert_eq!(provider.captured_requests().len(), 1);

    let mut post_replace = compacted;
    post_replace.push(user("new user turn"));
    post_replace.push(assistant("assistant response"));
    let first_text = user_text(
        post_replace
            .first()
            .expect("post-replace state should include summary"),
    )
    .expect("summary message should be user text");
    assert!(first_text.starts_with("<crabgent-compact-summary>"));

    let second = hook.pre_compact(&post_replace, &ctx).await;

    assert!(matches!(second, Decision::Continue));
    assert_eq!(provider.captured_requests().len(), 1);
}

#[tokio::test]
async fn pre_compact_cache_hit_on_same_fingerprint_same_runid() {
    let provider = Arc::new(StubProvider::with_text("cached summary"));
    let hook = CompactHook::new(Arc::clone(&provider), "summary-model")
        .with_max_messages(1)
        .with_keep_recent_messages(1);
    let ctx = ctx();
    let messages = vec![user("old request"), user("latest request")];

    let first = replace_value(hook.pre_compact(&messages, &ctx).await)
        .expect("first compaction should replace");
    let second = replace_value(hook.pre_compact(&messages, &ctx).await)
        .expect("second compaction should reuse cached replacement");

    assert_eq!(provider.captured_requests().len(), 1);
    assert_eq!(first.len(), second.len());
    for (first_message, second_message) in first.iter().zip(second.iter()) {
        assert_eq!(user_text(first_message), user_text(second_message));
    }
}
