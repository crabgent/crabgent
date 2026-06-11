use std::sync::Arc;

use chrono::{DateTime, Utc};
use crabgent_core::{Decision, LlmRequest, RunCtx, RunId, Subject};

use super::*;
use crate::provider::EmbeddedHolidayProvider;

fn fixed_clock(datetime: &str) -> Clock {
    let now = DateTime::parse_from_rfc3339(datetime)
        .expect("valid RFC3339 datetime in test")
        .with_timezone(&Utc);
    Arc::new(move || now)
}

fn empty_request() -> LlmRequest {
    LlmRequest {
        model: "test-model".into(),
        system_prompt: None,
        messages: Vec::new(),
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
async fn before_llm_strips_accumulated_time_hints_before_rebuild() {
    let hook = TimeHintHook::new(Arc::new(EmbeddedHolidayProvider::new()))
        .with_upcoming_count(0)
        .with_clock(fixed_clock("2026-05-06T10:30:00Z"));
    let mut req = empty_request();
    req.system_prompt = Some(format!(
        "Base prompt\n\n{TIME_HINT_OPEN}\nOld one.\n{LEGACY_TIME_HINT_CLOSE_MARKER}\n\n{LEGACY_TIME_HINT_MARKER}\nOld two.\n{TIME_HINT_CLOSE}"
    ));

    let next = match hook
        .before_llm(&req, &RunCtx::new(RunId::new(), Subject::new("u")))
        .await
    {
        Decision::Replace(next) => next,
        other => panic!("expected Replace, got {other:?}"),
    };
    let prompt = next.system_prompt.expect("prompt should be set");
    assert_eq!(prompt.matches(TIME_HINT_OPEN).count(), 1);
    assert_eq!(prompt.matches(TIME_HINT_CLOSE).count(), 1);
    assert!(prompt.starts_with(&format!("Base prompt\n\n{TIME_HINT_OPEN}")));
    assert!(!prompt.contains("Old one."));
    assert!(!prompt.contains("Old two."));
    assert!(!prompt.contains(LEGACY_TIME_HINT_MARKER));
    assert!(!prompt.contains(LEGACY_TIME_HINT_CLOSE_MARKER));
}
