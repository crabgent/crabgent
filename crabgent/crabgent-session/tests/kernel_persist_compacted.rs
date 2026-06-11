use std::sync::Arc;

use async_trait::async_trait;
use crabgent_core::MemoryScope;
use tokio_util::sync::CancellationToken;

use crabgent_core::{
    AllowAllPolicy, ContentBlock, Decision, Hook, Kernel, LlmRequest, LlmResponse, Message,
    ModelInfo, ModelTarget, Provider, ProviderCapabilities, ProviderError, RunCtx, RunId,
    RunRequest, StopReason, Subject, Usage,
};
use crabgent_session::SessionPersistHook;
use crabgent_store::memory::MemorySessionStore;
use crabgent_store::records::Session;
use crabgent_store::{Owner, SessionId, SessionStore};
use crabgent_test_support::user_msg as user;

const SUMMARY_TEXT: &str = "compacted summary text";

struct TestCompactHook {
    replacement: Vec<Message>,
}

#[async_trait]
impl Hook for TestCompactHook {
    async fn pre_compact(&self, _msgs: &[Message], _ctx: &RunCtx) -> Decision<Vec<Message>> {
        Decision::Replace(self.replacement.clone())
    }
}

struct MockProvider;

#[async_trait]
impl Provider for MockProvider {
    async fn complete(
        &self,
        req: &LlmRequest,
        _ctx: &RunCtx,
        _cancel: Option<&CancellationToken>,
    ) -> Result<LlmResponse, ProviderError> {
        Ok(LlmResponse {
            text: "stored reply".into(),
            tool_calls: Vec::new(),
            stop_reason: StopReason::EndTurn,
            usage: Usage::default(),
            model: req.model.clone(),
        })
    }

    fn name(&self) -> &'static str {
        "mock"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities::default()
    }

    fn models(&self) -> Vec<ModelInfo> {
        vec![
            ModelInfo::minimal("m", "mock"),
            ModelInfo::minimal("preserved-model", "mock"),
        ]
    }
}

fn message_text(message: &Message) -> String {
    match message {
        Message::User { content, .. } => content
            .iter()
            .filter_map(|block| match block {
                ContentBlock::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n"),
        Message::Assistant { text, .. } | Message::System { content: text } => text.clone(),
        Message::ToolResult { output, .. } => output.to_string(),
        _ => String::new(),
    }
}

fn persisted_initial(count: usize) -> Vec<Message> {
    (1..=count)
        .map(|idx| user(format!("persisted user {idx}")))
        .collect()
}

fn request(messages: Vec<Message>) -> RunRequest {
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
        web_search: crabgent_core::types::WebSearchConfig::default(),
    }
}

async fn seed_session(
    store: &Arc<MemorySessionStore>,
    messages: Vec<Message>,
    model_override: Option<&str>,
) -> (SessionId, Vec<Message>) {
    let mut session: Session = store
        .find_or_create(&Owner::new("u"), None, &MemoryScope::default())
        .await
        .expect("session created");
    session.messages = messages.clone();
    session.model_override = model_override.map(str::to_owned);
    store.save(&session).await.expect("session saved");
    (session.id, messages)
}

fn kernel(store: Arc<MemorySessionStore>) -> Kernel {
    Kernel::builder()
        .provider(MockProvider)
        .policy(AllowAllPolicy)
        .add_hook(SessionPersistHook::new(store))
        .add_hook(TestCompactHook {
            replacement: vec![user(SUMMARY_TEXT)],
        })
        .build()
}

#[tokio::test]
async fn kernel_persist_compacted_view_after_pre_compact() {
    let store = Arc::new(MemorySessionStore::default());
    let (session_id, persisted) = seed_session(&store, persisted_initial(15), None).await;
    let initial_count = persisted.len();
    let mut request_messages = persisted;
    request_messages.push(user("new user"));

    kernel(Arc::clone(&store))
        .run(request(request_messages), None)
        .await
        .expect("kernel run succeeds");

    let loaded = store
        .load(&session_id)
        .await
        .expect("session load succeeds")
        .expect("session exists");
    assert!(loaded.messages.len() < initial_count + 2);
    assert_eq!(loaded.messages.len(), 2);
    assert!(
        message_text(
            loaded
                .messages
                .first()
                .expect("session should persist compacted summary")
        )
        .contains(SUMMARY_TEXT)
    );
}

#[tokio::test]
async fn kernel_persist_compacted_view_preserves_model_override() {
    let store = Arc::new(MemorySessionStore::default());
    let (session_id, persisted) =
        seed_session(&store, persisted_initial(15), Some("preserved-model")).await;
    let mut request_messages = persisted;
    request_messages.push(user("new user"));

    kernel(Arc::clone(&store))
        .run(request(request_messages), None)
        .await
        .expect("kernel run succeeds");

    let loaded = store
        .load(&session_id)
        .await
        .expect("session load succeeds")
        .expect("session exists");
    assert_eq!(loaded.model_override.as_deref(), Some("preserved-model"));
    assert!(
        message_text(
            loaded
                .messages
                .first()
                .expect("session should persist compacted summary")
        )
        .contains(SUMMARY_TEXT)
    );
}
