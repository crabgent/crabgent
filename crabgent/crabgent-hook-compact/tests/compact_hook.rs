use std::sync::Arc;

use crabgent_core::{
    Decision, Hook, Message, ModelId, ModelInfo, Outcome, ProviderError, RunCtx, RunId, Subject,
};
use crabgent_hook_compact::{CompactFailureMode, CompactHook};
use crabgent_test_support::{StubProvider, assistant, assistant_with_tools, tool_call, user_msg};
use serde_json::json;

/// Default model catalog advertised by the summary doubles, matching the
/// `"summary-model"` id the hooks resolve their output cap against.
fn summary_models() -> Vec<ModelInfo> {
    vec![ModelInfo::minimal("summary-model", "summary")]
}

/// A canned-summary double returning `text` and advertising [`summary_models`].
fn summary(text: &str) -> Arc<StubProvider> {
    Arc::new(StubProvider::with_text(text).with_models(summary_models()))
}

/// A summary double with an explicit model catalog (alias-cap tests).
fn summary_with_models(text: &str, models: Vec<ModelInfo>) -> Arc<StubProvider> {
    Arc::new(StubProvider::with_text(text).with_models(models))
}

fn error() -> Arc<StubProvider> {
    Arc::new(
        StubProvider::new()
            .with_models(summary_models())
            .fail_with(|| ProviderError::Other("summary failed".into())),
    )
}

fn auth() -> Arc<StubProvider> {
    Arc::new(
        StubProvider::new()
            .with_models(summary_models())
            .fail_with(|| ProviderError::Auth("bad key".into())),
    )
}

fn server_error() -> Arc<StubProvider> {
    Arc::new(
        StubProvider::new()
            .with_models(summary_models())
            .fail_with(|| ProviderError::Api {
                status: 503,
                message: "busy".into(),
                retry_after_secs: None,
            }),
    )
}

fn long_rate_limit() -> Arc<StubProvider> {
    Arc::new(
        StubProvider::new()
            .with_models(summary_models())
            .fail_with(|| ProviderError::RateLimited {
                retry_after_secs: Some(30),
            }),
    )
}

fn ctx() -> RunCtx {
    RunCtx::new(RunId::new(), Subject::new("u"))
}

fn user(text: &str) -> Message {
    user_msg(text)
}

fn assistant_tool_call(id: &str) -> Message {
    assistant_with_tools(
        "",
        vec![tool_call(id, "read_file", json!({"path": "Cargo.toml"}))],
    )
}

fn tool_result(call_id: &str) -> Message {
    Message::ToolResult {
        call_id: call_id.into(),
        output: json!("contents"),
        is_error: false,
    }
}

#[tokio::test]
async fn below_threshold_continues_without_provider_call() {
    let provider = summary("summary");
    let hook = CompactHook::new(Arc::clone(&provider), "summary-model")
        .with_max_messages(10)
        .with_max_tokens(10_000);

    let decision = hook.pre_compact(&[user("small")], &ctx()).await;

    assert!(matches!(decision, Decision::Continue));
    assert_eq!(provider.captured_requests().len(), 0);
}

#[tokio::test]
async fn compacts_old_messages_and_keeps_recent_tail() {
    let provider = summary("old context summary");
    let hook = CompactHook::new(Arc::clone(&provider), "summary-model")
        .with_max_messages(2)
        .with_keep_recent_messages(1);
    let messages = vec![
        Message::System {
            content: "rules".into(),
        },
        user("old request"),
        assistant("old answer"),
        user("latest request"),
    ];

    let decision = hook.pre_compact(&messages, &ctx()).await;

    let Decision::Replace(next) = decision else {
        panic!("expected replacement");
    };
    assert_eq!(next.len(), 3);
    assert!(matches!(next[0], Message::System { .. }));
    assert!(matches!(next[1], Message::User { .. }));
    assert!(matches!(next[2], Message::User { .. }));

    let request_guard = provider.captured_requests();
    assert_eq!(request_guard.len(), 1);
    let summary_input = request_guard[0].messages[0].to_string();
    assert!(summary_input.contains("old request"));
    assert!(summary_input.contains("old answer"));
    assert!(!summary_input.contains("latest request"));
}

#[tokio::test]
async fn compaction_does_not_split_tool_call_group() {
    let provider = summary("old context summary");
    let hook = CompactHook::new(Arc::clone(&provider), "summary-model")
        .with_max_messages(1)
        .with_keep_recent_messages(2);
    let messages = vec![
        user("old request"),
        assistant_tool_call("call-1"),
        tool_result("call-1"),
        user("latest request"),
    ];

    let decision = hook.pre_compact(&messages, &ctx()).await;

    let Decision::Replace(next) = decision else {
        panic!("expected replacement");
    };
    assert_eq!(next.len(), 4);
    assert!(matches!(next[0], Message::User { .. }));
    assert!(matches!(next[1], Message::Assistant { .. }));
    assert!(matches!(next[2], Message::ToolResult { .. }));
    assert!(matches!(next[3], Message::User { .. }));

    let request_guard = provider.captured_requests();
    let summary_input = request_guard[0].messages[0].to_string();
    assert!(summary_input.contains("old request"));
    assert!(!summary_input.contains("read_file"));
}

#[tokio::test]
async fn token_threshold_can_trigger_compaction() {
    let provider = summary("summary");
    let hook = CompactHook::new(provider, "summary-model")
        .with_max_messages(100)
        .with_max_tokens(1)
        .with_keep_recent_messages(1);
    let messages = vec![user("long old text"), user("latest")];

    let decision = hook.pre_compact(&messages, &ctx()).await;

    assert!(matches!(decision, Decision::Replace(_)));
}

#[tokio::test]
async fn provider_error_continues_by_default() {
    let provider = error();
    let hook = CompactHook::new(provider, "summary-model")
        .with_max_messages(1)
        .with_keep_recent_messages(1);
    let messages = vec![user("old"), user("latest")];

    let decision = hook.pre_compact(&messages, &ctx()).await;

    assert!(matches!(decision, Decision::Continue));
}

#[tokio::test]
async fn fallback_provider_is_used_for_retryable_error() {
    let primary = error();
    let fallback = summary("fallback summary");
    let hook = CompactHook::new(Arc::clone(&primary), "primary-summary")
        .with_fallback(Arc::clone(&fallback), "fallback-summary")
        .with_max_messages(1)
        .with_keep_recent_messages(1);
    let messages = vec![user("old"), user("latest")];

    let decision = hook.pre_compact(&messages, &ctx()).await;

    assert!(matches!(decision, Decision::Replace(_)));
    assert_eq!(primary.captured_requests().len(), 1);
    let fallback_guard = fallback.captured_requests();
    assert_eq!(fallback_guard.len(), 1);
    assert_eq!(fallback_guard[0].model, ModelId::new("fallback-summary"));
}

#[tokio::test]
async fn summary_max_tokens_clamps_to_advertised_alias_cap() {
    let mut model = ModelInfo::minimal("canonical-summary", "summary");
    model.aliases.push(ModelId::new("summary-alias"));
    model.caps.max_output_tokens = 4_000;
    let provider = summary_with_models("old context summary", vec![model]);
    let hook = CompactHook::new(Arc::clone(&provider), "summary-alias")
        .with_summary_max_tokens(Some(32_768))
        .with_max_messages(1)
        .with_keep_recent_messages(1);
    let messages = vec![user("old"), user("latest")];

    let decision = hook.pre_compact(&messages, &ctx()).await;

    assert!(matches!(decision, Decision::Replace(_)));
    let request_guard = provider.captured_requests();
    assert_eq!(request_guard[0].max_tokens, Some(4_000));
}

#[tokio::test]
async fn unknown_summary_model_metadata_keeps_configured_max_tokens() {
    let provider = summary_with_models("old context summary", Vec::new());
    let hook = CompactHook::new(Arc::clone(&provider), "summary-model")
        .with_summary_max_tokens(Some(32_768))
        .with_max_messages(1)
        .with_keep_recent_messages(1);
    let messages = vec![user("old"), user("latest")];

    let decision = hook.pre_compact(&messages, &ctx()).await;

    assert!(matches!(decision, Decision::Replace(_)));
    let request_guard = provider.captured_requests();
    assert_eq!(request_guard[0].max_tokens, Some(32_768));
}

#[tokio::test]
async fn server_error_falls_back_to_next_provider() {
    let primary = server_error();
    let fallback = summary("fallback summary");
    let hook = CompactHook::new(Arc::clone(&primary), "primary-summary")
        .with_fallback(Arc::clone(&fallback), "fallback-summary")
        .with_max_messages(1)
        .with_keep_recent_messages(1);
    let messages = vec![user("old"), user("latest")];

    let decision = hook.pre_compact(&messages, &ctx()).await;

    assert!(matches!(decision, Decision::Replace(_)));
    assert_eq!(primary.captured_requests().len(), 1);
    assert_eq!(fallback.captured_requests().len(), 1);
}

#[tokio::test]
async fn long_rate_limit_does_not_fallback() {
    let primary = long_rate_limit();
    let fallback = summary("fallback summary");
    let hook = CompactHook::new(Arc::clone(&primary), "primary-summary")
        .with_fallback(Arc::clone(&fallback), "fallback-summary")
        .with_max_messages(1)
        .with_keep_recent_messages(1);
    let messages = vec![user("old"), user("latest")];

    let decision = hook.pre_compact(&messages, &ctx()).await;

    assert!(matches!(decision, Decision::Continue));
    assert_eq!(primary.captured_requests().len(), 1);
    assert_eq!(fallback.captured_requests().len(), 0);
}

#[tokio::test]
async fn auth_error_does_not_fallback() {
    let primary = auth();
    let fallback = summary("fallback summary");
    let hook = CompactHook::new(Arc::clone(&primary), "primary-summary")
        .with_fallback(Arc::clone(&fallback), "fallback-summary")
        .with_max_messages(1)
        .with_keep_recent_messages(1);
    let messages = vec![user("old"), user("latest")];

    let decision = hook.pre_compact(&messages, &ctx()).await;

    assert!(matches!(decision, Decision::Continue));
    assert_eq!(primary.captured_requests().len(), 1);
    assert_eq!(fallback.captured_requests().len(), 0);
}

#[tokio::test]
async fn empty_summary_falls_back_before_failure_mode() {
    let primary = summary(" ");
    let fallback = summary("fallback summary");
    let hook = CompactHook::new(Arc::clone(&primary), "primary-summary")
        .with_fallback(Arc::clone(&fallback), "fallback-summary")
        .with_max_messages(1)
        .with_keep_recent_messages(1)
        .with_failure_mode(CompactFailureMode::Deny);
    let messages = vec![user("old"), user("latest")];

    let decision = hook.pre_compact(&messages, &ctx()).await;

    assert!(matches!(decision, Decision::Replace(_)));
    assert_eq!(primary.captured_requests().len(), 1);
    assert_eq!(fallback.captured_requests().len(), 1);
}

#[tokio::test]
async fn deny_mode_denies_on_provider_error() {
    let provider = error();
    let hook = CompactHook::new(provider, "summary-model")
        .with_max_messages(1)
        .with_keep_recent_messages(1)
        .with_failure_mode(CompactFailureMode::Deny);
    let messages = vec![user("old"), user("latest")];

    let decision = hook.pre_compact(&messages, &ctx()).await;

    assert!(matches!(decision, Decision::Deny(reason) if reason.contains("provider")));
}

#[tokio::test]
async fn empty_summary_uses_failure_mode() {
    let provider = summary("   ");
    let hook = CompactHook::new(provider, "summary-model")
        .with_max_messages(1)
        .with_keep_recent_messages(1)
        .with_failure_mode(CompactFailureMode::Deny);
    let messages = vec![user("old"), user("latest")];

    let decision = hook.pre_compact(&messages, &ctx()).await;

    assert!(matches!(decision, Decision::Deny(reason) if reason.contains("empty")));
}

#[tokio::test]
async fn permanent_failure_mutes_subsequent_calls_in_same_run() {
    let provider = auth();
    let hook = CompactHook::new(Arc::clone(&provider), "summary-model")
        .with_max_messages(1)
        .with_keep_recent_messages(1);
    let messages = vec![user("old"), user("latest")];
    let run_ctx = ctx();

    let first = hook.pre_compact(&messages, &run_ctx).await;
    let second = hook.pre_compact(&messages, &run_ctx).await;

    assert!(
        matches!(first, Decision::Continue),
        "first call hits Continue via failure_mode default"
    );
    assert!(
        matches!(second, Decision::Continue),
        "second call hits Continue via mute short-circuit"
    );
    assert_eq!(provider.captured_requests().len(), 1);
}

#[tokio::test]
async fn builder_methods_apply_to_config() {
    let provider = summary("ignored");
    let hook = CompactHook::new(provider, "summary-model")
        .with_config(crabgent_hook_compact::CompactConfig::default())
        .with_max_messages(7)
        .with_max_tokens(123)
        .with_keep_recent_messages(3)
        .with_summary_max_tokens(Some(99))
        .with_summary_temperature(Some(0.5))
        .with_system_prompt("system")
        .with_instruction("instruction")
        .with_failure_mode(CompactFailureMode::Deny);
    let config = hook.config();

    assert_eq!(config.max_messages, 7);
    assert_eq!(config.max_tokens, 123);
    assert_eq!(config.keep_recent_messages, 3);
    assert_eq!(config.summary_max_tokens, Some(99));
    assert_eq!(config.summary_temperature, Some(0.5));
    assert_eq!(config.system_prompt, "system");
    assert_eq!(config.instruction, "instruction");
    assert_eq!(config.failure_mode, CompactFailureMode::Deny);
}

#[tokio::test]
async fn default_config_uses_phase_58_limits() {
    let config = crabgent_hook_compact::CompactConfig::default();

    assert_eq!(config.max_messages, 200);
    assert_eq!(config.max_tokens, 64_000);
    assert_eq!(config.keep_recent_messages, 15);
    assert_eq!(config.summary_max_tokens, Some(8_192));
}

#[tokio::test]
async fn on_stop_clears_mute_for_run() {
    let provider = auth();
    let hook = CompactHook::new(Arc::clone(&provider), "summary-model")
        .with_max_messages(1)
        .with_keep_recent_messages(1);
    let messages = vec![user("old"), user("latest")];
    let run_ctx = ctx();

    hook.pre_compact(&messages, &run_ctx).await;
    hook.on_stop(&run_ctx, &Outcome::Completed("ok".into()))
        .await;
    let after_stop = hook.pre_compact(&messages, &run_ctx).await;

    assert!(
        matches!(after_stop, Decision::Continue),
        "post-stop call hits Continue via failure_mode default (provider still fails), not via mute"
    );
    assert_eq!(provider.captured_requests().len(), 2);
}
