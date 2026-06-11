//! End-to-end check of the time-hint hook: system block carries
//! `Last user message:` + pause-marker, and the last
//! [`INLINE_ANNOTATE_LIMIT`] user messages get an inline `[ts, ago]`
//! prefix on the wire-side request.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use crabgent_calendar::{
    Clock, EmbeddedHolidayProvider, INLINE_ANNOTATE_LIMIT, TIME_HINT_CLOSE, TIME_HINT_OPEN,
    TimeHintConfig, TimeHintHook,
};
use crabgent_core::{Decision, Hook, LlmRequest, RunCtx, RunId, Subject};
use serde_json::{Value, json};

fn fixed_clock(s: &str) -> Clock {
    let now = DateTime::parse_from_rfc3339(s)
        .expect("rfc3339")
        .with_timezone(&Utc);
    Arc::new(move || now)
}

fn ctx() -> RunCtx {
    RunCtx::new(RunId::new(), Subject::new("u"))
}

fn user_msg(text: &str, ts: &str) -> Value {
    json!({
        "role": "user",
        "content": [{"type": "text", "text": text}],
        "timestamp": ts,
    })
}

fn assistant_msg(text: &str) -> Value {
    json!({
        "role": "assistant",
        "text": text,
        "tool_calls": [],
    })
}

fn empty_request(messages: Vec<Value>) -> LlmRequest {
    LlmRequest {
        model: "test-model".into(),
        system_prompt: Some("base prompt".into()),
        messages,
        tools: Vec::new(),
        max_tokens: None,
        temperature: None,
        stop_sequences: Vec::new(),
        reasoning_effort: None,
        web_search: crabgent_core::types::WebSearchConfig::default(),
        tool_choice: None,
    }
}

#[tokio::test]
async fn e2e_system_block_and_inline_prefix_with_seven_user_msgs() {
    let provider = Arc::new(EmbeddedHolidayProvider::new());
    let hook = TimeHintHook::new(provider).with_config(
        TimeHintConfig::default()
            .with_upcoming_count(0)
            .with_clock(fixed_clock("2026-05-21T14:32:11Z")),
    );

    let messages = vec![
        user_msg("3 days ago", "2026-05-18T14:32:11Z"),
        assistant_msg("ack 1"),
        user_msg("26h ago", "2026-05-20T12:32:11Z"),
        user_msg("10h ago", "2026-05-21T04:32:11Z"),
        assistant_msg("ack 2"),
        user_msg("5h ago", "2026-05-21T09:32:11Z"),
        user_msg("3h ago", "2026-05-21T11:32:11Z"),
        user_msg("30m ago", "2026-05-21T14:02:11Z"),
        user_msg("2m ago", "2026-05-21T14:30:11Z"),
    ];
    let req = empty_request(messages);

    let next = match hook.before_llm(&req, &ctx()).await {
        Decision::Replace(next) => next,
        other => panic!("expected Replace, got {other:?}"),
    };

    let prompt = next.system_prompt.expect("prompt should be set");
    // System block carries the markers and the new pause line. The
    // latest user message is 2m ago, which classifies as `active`.
    assert_eq!(prompt.matches(TIME_HINT_OPEN).count(), 1);
    assert_eq!(prompt.matches(TIME_HINT_CLOSE).count(), 1);
    assert!(prompt.starts_with("base prompt"));
    assert!(prompt.contains("Last user message: 2026-05-21 16:30"));
    assert!(prompt.contains("Pause: active."));

    // Inline annotation: exactly the last INLINE_ANNOTATE_LIMIT user
    // messages get a prefix. Walk messages and verify which ones got
    // touched.
    let prefixed_count = next
        .messages
        .iter()
        .filter(|m| m.get("role").and_then(Value::as_str) == Some("user"))
        .filter(|m| {
            m["content"][0]["text"]
                .as_str()
                .is_some_and(|s| s.starts_with("[2026-"))
        })
        .count();
    assert_eq!(prefixed_count, INLINE_ANNOTATE_LIMIT);

    // Oldest user message (3 days ago) keeps the original bare text.
    let oldest = next.messages[0].clone();
    assert_eq!(oldest["content"][0]["text"].as_str(), Some("3 days ago"));

    // Newest user message carries the inline prefix.
    let newest = next.messages.last().expect("non-empty messages");
    assert!(
        newest["content"][0]["text"]
            .as_str()
            .expect("text")
            .starts_with("[2026-05-21 16:30 Europe/Berlin"),
        "newest text starts with prefix: {:?}",
        newest["content"][0]["text"]
    );
}

#[tokio::test]
async fn e2e_idempotent_when_pristine_messages_replayed() {
    let provider = Arc::new(EmbeddedHolidayProvider::new());
    let hook = TimeHintHook::new(provider).with_config(
        TimeHintConfig::default()
            .with_upcoming_count(0)
            .with_clock(fixed_clock("2026-05-21T14:32:11Z")),
    );

    let pristine = vec![
        user_msg("first", "2026-05-21T13:00:00Z"),
        user_msg("second", "2026-05-21T14:00:00Z"),
    ];

    let first = match hook
        .before_llm(&empty_request(pristine.clone()), &ctx())
        .await
    {
        Decision::Replace(next) => next,
        other => panic!("expected Replace, got {other:?}"),
    };

    let second = match hook.before_llm(&empty_request(pristine), &ctx()).await {
        Decision::Replace(next) => next,
        other => panic!("expected Replace, got {other:?}"),
    };

    // Same pristine input + fixed clock => identical wire output.
    assert_eq!(first.system_prompt, second.system_prompt);
    assert_eq!(first.messages, second.messages);
}

#[tokio::test]
async fn e2e_replaces_stale_hint_block_in_place() {
    let provider = Arc::new(EmbeddedHolidayProvider::new());
    let hook = TimeHintHook::new(provider).with_config(
        TimeHintConfig::default()
            .with_upcoming_count(0)
            .with_clock(fixed_clock("2026-05-21T14:32:11Z")),
    );

    let mut req = empty_request(vec![user_msg("only", "2026-05-21T14:00:00Z")]);
    req.system_prompt = Some(format!(
        "base prompt\n\n{TIME_HINT_OPEN}\nStale content from 6 hours ago.\n{TIME_HINT_CLOSE}"
    ));

    let next = match hook.before_llm(&req, &ctx()).await {
        Decision::Replace(next) => next,
        other => panic!("expected Replace, got {other:?}"),
    };

    let prompt = next.system_prompt.expect("prompt should be set");
    assert_eq!(prompt.matches(TIME_HINT_OPEN).count(), 1);
    assert_eq!(prompt.matches(TIME_HINT_CLOSE).count(), 1);
    assert!(!prompt.contains("Stale content from 6 hours ago."));
    assert!(prompt.starts_with("base prompt"));
}
