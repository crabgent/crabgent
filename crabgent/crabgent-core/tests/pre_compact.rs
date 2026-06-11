use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use crabgent_core::{
    AllowAllPolicy, ContentBlock, Decision, Hook, Kernel, LlmRequest, LlmResponse, Message,
    ModelInfo, ModelTarget, Provider, ProviderCapabilities, ProviderError, RunCtx, RunId,
    RunRequest, StopReason, Subject, Usage,
};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

struct CompactHook;

#[async_trait]
impl Hook for CompactHook {
    async fn pre_compact(&self, _msgs: &[Message], _ctx: &RunCtx) -> Decision<Vec<Message>> {
        Decision::Replace(vec![Message::User {
            content: vec![ContentBlock::Text {
                text: "compact".into(),
            }],
            timestamp: None,
        }])
    }
}

#[derive(Clone)]
enum CompactMode {
    Replace(Vec<Message>),
    Continue,
}

struct ConfigurableCompactHook {
    mode: CompactMode,
}

#[async_trait]
impl Hook for ConfigurableCompactHook {
    async fn pre_compact(&self, _msgs: &[Message], _ctx: &RunCtx) -> Decision<Vec<Message>> {
        match &self.mode {
            CompactMode::Replace(messages) => Decision::Replace(messages.clone()),
            CompactMode::Continue => Decision::Continue,
        }
    }
}

struct ProbeHook {
    captured: Arc<Mutex<Vec<Vec<Message>>>>,
}

#[async_trait]
impl Hook for ProbeHook {
    async fn on_message(&self, msgs: &[Message], _ctx: &RunCtx) -> Decision<Vec<Message>> {
        self.captured
            .lock()
            .expect("captured lock")
            .push(msgs.to_vec());
        Decision::Continue
    }
}

struct CapturingProvider {
    seen: Arc<Mutex<Vec<Value>>>,
}

#[async_trait]
impl Provider for CapturingProvider {
    async fn complete(
        &self,
        req: &LlmRequest,
        _ctx: &RunCtx,
        _cancel: Option<&CancellationToken>,
    ) -> Result<LlmResponse, ProviderError> {
        (*self.seen.lock().expect("seen lock")).clone_from(&req.messages);
        Ok(LlmResponse {
            text: "done".into(),
            tool_calls: Vec::new(),
            stop_reason: StopReason::EndTurn,
            usage: Usage::default(),
            model: req.model.clone(),
        })
    }

    fn name(&self) -> &'static str {
        "test"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities::default()
    }

    fn models(&self) -> Vec<ModelInfo> {
        vec![ModelInfo::minimal("m", "test")]
    }
}

#[tokio::test]
async fn pre_compact_replace_is_used_for_provider_request() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let kernel = Kernel::builder()
        .provider(CapturingProvider {
            seen: Arc::clone(&seen),
        })
        .add_hook(CompactHook)
        .policy(AllowAllPolicy)
        .build();
    let text = kernel
        .run(
            RunRequest {
                pause: None,
                run_id: RunId::new(),
                subject: Subject::new("u"),
                model: ModelTarget::id("m"),
                explicit_model: None,
                session_model_override: None,
                fallbacks: Vec::new(),
                messages: vec![Message::User {
                    content: vec![ContentBlock::Text {
                        text: "full context".into(),
                    }],
                    timestamp: None,
                }],
                system_prompt: None,
                max_turns: Some(1),
                temperature: None,
                max_tokens: None,
                cancel_reason: None,
                reasoning_effort: None,
                web_search: crabgent_core::WebSearchConfig::default(),
            },
            None,
        )
        .await
        .expect("run succeeds");

    assert_eq!(text, "done");
    let provider_messages = seen.lock().expect("seen lock");
    assert_eq!(provider_messages.len(), 1);
    assert_eq!(provider_messages[0]["content"][0]["text"], "compact");
}

fn user(text: impl Into<String>) -> Message {
    Message::User {
        content: vec![ContentBlock::Text { text: text.into() }],
        timestamp: None,
    }
}

fn system(text: impl Into<String>) -> Message {
    Message::System {
        content: text.into(),
    }
}

fn user_text(message: &Message) -> &str {
    assert!(
        matches!(message, Message::User { .. }),
        "message should be user"
    );
    let Message::User { content, .. } = message else {
        return "";
    };
    assert!(
        matches!(content.first(), Some(ContentBlock::Text { .. })),
        "user message should contain text"
    );
    let Some(ContentBlock::Text { text }) = content.first() else {
        return "";
    };
    text
}

fn system_text(message: &Message) -> &str {
    assert!(
        matches!(message, Message::System { .. }),
        "message should be system"
    );
    let Message::System { content } = message else {
        return "";
    };
    content
}

fn assistant_text(message: &Message) -> &str {
    assert!(
        matches!(message, Message::Assistant { .. }),
        "message should be assistant"
    );
    let Message::Assistant { text, .. } = message else {
        return "";
    };
    text
}

fn run_request(messages: Vec<Message>) -> RunRequest {
    RunRequest {
        pause: None,
        run_id: RunId::new(),
        subject: Subject::new("u"),
        model: ModelTarget::id("m"),
        explicit_model: None,
        session_model_override: None,
        fallbacks: Vec::new(),
        messages,
        system_prompt: None,
        max_turns: Some(1),
        temperature: None,
        max_tokens: None,
        cancel_reason: None,
        reasoning_effort: None,
        web_search: crabgent_core::WebSearchConfig::default(),
    }
}

fn assistant_capture(captured: &[Vec<Message>]) -> &[Message] {
    captured
        .iter()
        .rev()
        .find(|messages| matches!(messages.last(), Some(Message::Assistant { .. })))
        .map(Vec::as_slice)
        .expect("assistant append captured")
}

fn final_capture(captured: &[Vec<Message>]) -> &[Message] {
    captured
        .last()
        .map(Vec::as_slice)
        .expect("at least one on_message call captured")
}

async fn run_with_probe(mode: CompactMode, messages: Vec<Message>) -> Vec<Vec<Message>> {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let captured = Arc::new(Mutex::new(Vec::new()));
    let kernel = Kernel::builder()
        .provider(CapturingProvider { seen })
        .add_hook(ConfigurableCompactHook { mode })
        .add_hook(ProbeHook {
            captured: Arc::clone(&captured),
        })
        .policy(AllowAllPolicy)
        .build();

    kernel
        .run(run_request(messages), None)
        .await
        .expect("run succeeds");

    captured.lock().expect("captured lock").clone()
}

async fn run_turn_with_replace(initial: Vec<Message>, compacted: Vec<Message>) -> Vec<Message> {
    let captured = run_with_probe(CompactMode::Replace(compacted), initial).await;
    assistant_capture(&captured).to_vec()
}

#[tokio::test]
async fn pre_compact_replace_mutates_canonical_observed_by_on_message() {
    let compacted = vec![user("compact summary")];
    let captured =
        run_with_probe(CompactMode::Replace(compacted.clone()), vec![user("full")]).await;

    assert!(!captured.is_empty());
    let observed = assistant_capture(&captured);
    assert_eq!(observed.len(), compacted.len() + 1);
    assert_eq!(user_text(&observed[0]), "compact summary");
    assert_eq!(assistant_text(&observed[1]), "done");
}

#[tokio::test]
async fn pre_compact_continue_leaves_canonical_unchanged() {
    let captured = run_with_probe(CompactMode::Continue, vec![user("full context")]).await;

    let observed = final_capture(&captured);
    assert_eq!(observed.len(), 2);
    assert_eq!(user_text(&observed[0]), "full context");
    assert_eq!(assistant_text(&observed[1]), "done");
}

#[tokio::test]
async fn multi_turn_message_log_stays_bounded() {
    let short_replace = vec![system("policy"), user("summary")];
    let first_initial: Vec<Message> = (1..=20).map(|idx| user(format!("user {idx}"))).collect();

    let persisted_one = run_turn_with_replace(first_initial, short_replace.clone()).await;
    let mut second_initial = persisted_one.clone();
    second_initial.push(user("user 21"));

    let persisted_two = run_turn_with_replace(second_initial, short_replace.clone()).await;
    let mut third_initial = persisted_two.clone();
    third_initial.push(user("user 22"));

    let persisted_three = run_turn_with_replace(third_initial, short_replace.clone()).await;

    for persisted in [&persisted_one, &persisted_two, &persisted_three] {
        assert!(persisted.len() <= short_replace.len() + 4);
        assert_eq!(system_text(&persisted[0]), "policy");
        assert_eq!(user_text(&persisted[1]), "summary");
        assert_eq!(
            assistant_text(
                persisted
                    .last()
                    .expect("persisted turn should include assistant reply")
            ),
            "done"
        );
    }
}
