use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use crabgent_core::MemoryScope;
use tokio_util::sync::CancellationToken;

use crabgent_core::{
    AllowAllPolicy, ContentBlock, Kernel, LlmRequest, LlmResponse, Message, ModelInfo, Provider,
    ProviderCapabilities, ProviderError, ReasoningEffort, RunCtx, RunId, RunRequest, StopReason,
    Subject, Usage,
};
use crabgent_session::SessionPersistHook;
use crabgent_store::memory::MemorySessionStore;
use crabgent_store::records::Session;
use crabgent_store::{Owner, SessionStore};
use crabgent_test_support::{done, tool_call, tool_use};

struct ScriptedProvider {
    responses: Mutex<Vec<LlmResponse>>,
    captured_requests: Arc<Mutex<Vec<LlmRequest>>>,
}

impl ScriptedProvider {
    fn with(responses: Vec<LlmResponse>) -> Self {
        Self {
            responses: Mutex::new(responses),
            captured_requests: Arc::new(Mutex::new(Vec::new())),
        }
    }

    #[expect(
        clippy::missing_const_for_fn,
        reason = "test helper returns an Arc field reference and mirrors other provider doubles"
    )]
    fn captured(&self) -> &Arc<Mutex<Vec<LlmRequest>>> {
        &self.captured_requests
    }
}

#[async_trait]
impl Provider for ScriptedProvider {
    async fn complete(
        &self,
        req: &LlmRequest,
        _ctx: &RunCtx,
        _cancel: Option<&CancellationToken>,
    ) -> Result<LlmResponse, ProviderError> {
        self.captured_requests
            .lock()
            .expect("mutex should not be poisoned")
            .push(req.clone());
        let mut responses = self.responses.lock().expect("mutex should not be poisoned");
        if responses.is_empty() {
            return Err(ProviderError::Other("script exhausted".into()));
        }
        Ok(responses.remove(0))
    }

    fn name(&self) -> &'static str {
        "scripted"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            tools: true,
            ..ProviderCapabilities::default()
        }
    }

    fn models(&self) -> Vec<ModelInfo> {
        vec![ModelInfo::minimal("m", "scripted")]
    }
}

fn make_request(model: &str, subject_id: &str, text: &str, max_turns: u32) -> RunRequest {
    RunRequest {
        pause: None,
        run_id: RunId::new(),
        subject: Subject::new(subject_id),
        model: model.into(),
        explicit_model: None,
        session_model_override: None,
        fallbacks: Vec::new(),
        messages: vec![Message::User {
            content: vec![ContentBlock::Text { text: text.into() }],
            timestamp: None,
        }],
        system_prompt: None,
        max_turns: Some(max_turns),
        temperature: None,
        max_tokens: None,
        cancel_reason: None,
        reasoning_effort: None,
        web_search: crabgent_core::types::WebSearchConfig::default(),
    }
}

#[tokio::test]
async fn kernel_run_persists_user_and_assistant_messages() {
    let store = Arc::new(MemorySessionStore::default());
    let kernel = Kernel::builder()
        .provider(ScriptedProvider::with(vec![done("stored reply")]))
        .policy(AllowAllPolicy)
        .add_hook(SessionPersistHook::new(Arc::clone(&store)))
        .build();

    let owner = Owner::new("session-user");
    kernel
        .run(make_request("m", "session-user", "remember this", 5), None)
        .await
        .expect("run succeeds");

    let session = store
        .find_or_create(&owner, None, &MemoryScope::default())
        .await
        .expect("session");
    assert_eq!(session.messages.len(), 2);
    assert!(matches!(
        &session.messages[0],
        Message::User { content, ..} if matches!(
            &content[0],
            ContentBlock::Text { text } if text == "remember this"
        )
    ));
    assert!(matches!(
        &session.messages[1],
        Message::Assistant { text, tool_calls } if text == "stored reply" && tool_calls.is_empty()
    ));
}

#[tokio::test]
async fn test_second_run_sees_persisted_history_in_provider_request() {
    let store = Arc::new(MemorySessionStore::default());
    let provider = ScriptedProvider::with(vec![done("first reply"), done("second reply")]);
    let captured = provider.captured().clone();
    let kernel = Kernel::builder()
        .provider(provider)
        .policy(AllowAllPolicy)
        .add_hook(SessionPersistHook::new(Arc::clone(&store)))
        .build();

    kernel
        .run(make_request("m", "history-user", "first question", 5), None)
        .await
        .expect("run 1 succeeds");

    // Same owner+thread, new run_id, new kernel.
    let provider2 = ScriptedProvider::with(vec![done("second reply")]);
    let captured2 = provider2.captured().clone();
    let kernel2 = Kernel::builder()
        .provider(provider2)
        .policy(AllowAllPolicy)
        .add_hook(SessionPersistHook::new(Arc::clone(&store)))
        .build();
    kernel2
        .run(
            make_request("m", "history-user", "second question", 5),
            None,
        )
        .await
        .expect("run 2 succeeds");

    let all_captured = captured.lock().expect("mutex should not be poisoned");
    let run2_captured = captured2.lock().expect("mutex should not be poisoned");
    assert!(
        !all_captured.is_empty(),
        "run 1 should have captured a request"
    );
    let run2_msgs = &run2_captured[0].messages;
    assert!(
        run2_msgs.len() >= 3,
        "run 2 should see at least 3 messages, got {}",
        run2_msgs.len()
    );
    assert_eq!(run2_msgs[0]["role"], "user");
    assert_eq!(run2_msgs[0]["content"][0]["text"], "first question");
    assert_eq!(run2_msgs[1]["role"], "assistant");
    assert_eq!(run2_msgs[1]["text"], "first reply");
    assert_eq!(run2_msgs[2]["role"], "user");
    assert_eq!(run2_msgs[2]["content"][0]["text"], "second question");
}

/// Tool that emits both a `ToolResult` and a foreign `ChannelOutbound`
/// run-message, mirroring `channel_send`. Shared by the two double-fire
/// persistence tests below.
struct ChannelSendTool;

#[async_trait]
impl crabgent_core::Tool for ChannelSendTool {
    fn name(&self) -> &'static str {
        "channel_send"
    }
    fn description(&self) -> &'static str {
        "send"
    }
    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({"type": "object"})
    }
    async fn execute(
        &self,
        _args: serde_json::Value,
        _ctx: &crabgent_core::ToolCtx,
    ) -> Result<serde_json::Value, crabgent_core::ToolError> {
        Ok(
            serde_json::json!({"channel":"slack","conv":"slack:T1/C1","id":"1234.5678","thread_root":null,"broadcast":false}),
        )
    }

    async fn execute_result(
        &self,
        args: serde_json::Value,
        ctx: &crabgent_core::ToolCtx,
    ) -> Result<crabgent_core::ToolResult, crabgent_core::ToolError> {
        let body = args
            .get("body")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .to_owned();
        let output = self.execute(args, ctx).await?;
        Ok(
            crabgent_core::ToolResult::success(output).with_run_message(Message::ChannelOutbound {
                conv: Owner::new("slack:T1/C1"),
                body,
                channel: "slack".into(),
                message_id: "1234.5678".into(),
                thread_root: None,
                broadcast: false,
            }),
        )
    }
}

/// Run one kernel turn that fires `channel_send` once and return the
/// persisted session messages. Shared setup for the two double-fire tests.
async fn run_double_fire() -> Vec<Message> {
    let store = Arc::new(MemorySessionStore::default());
    let provider = ScriptedProvider::with(vec![
        tool_use(vec![tool_call(
            "call-1",
            "channel_send",
            serde_json::json!({"conv":"slack:T1/C1","body":"hello world","thread_parent":null}),
        )]),
        done("done"),
    ]);
    let kernel = Kernel::builder()
        .provider(provider)
        .policy(AllowAllPolicy)
        .add_tool(ChannelSendTool)
        .add_hook(SessionPersistHook::new(Arc::clone(&store)))
        .build();

    let result = kernel
        .run(make_request("m", "double-fire-user", "send it", 10), None)
        .await;
    assert!(result.is_ok(), "run should succeed: {:?}", result.err());

    let session = store
        .find_or_create(
            &Owner::new("double-fire-user"),
            None,
            &MemoryScope::default(),
        )
        .await
        .expect("session");
    session.messages
}

fn message_roles(msgs: &[Message]) -> Vec<&'static str> {
    msgs.iter()
        .map(|m| match m {
            Message::ToolResult { .. } => "tool_result",
            Message::ChannelOutbound { .. } => "channel_outbound",
            Message::User { .. } => "user",
            Message::Assistant { .. } => "assistant",
            Message::System { .. } => "system",
            _ => "other",
        })
        .collect()
}

#[tokio::test]
async fn test_session_persist_double_fire_orders_tool_result_before_outbound() {
    let msgs = run_double_fire().await;
    let roles = message_roles(&msgs);
    let tr_idx = roles.iter().position(|r| *r == "tool_result");
    let co_idx = roles.iter().position(|r| *r == "channel_outbound");
    assert!(tr_idx.is_some(), "should have tool_result, got {roles:?}");
    assert!(
        co_idx.is_some(),
        "should have channel_outbound, got {roles:?}"
    );
    assert!(
        tr_idx < co_idx,
        "tool_result should come before channel_outbound"
    );
}

#[tokio::test]
async fn test_session_persist_double_fire_persists_both_message_kinds() {
    let msgs = run_double_fire().await;
    let roles = message_roles(&msgs);
    assert!(
        roles.contains(&"tool_result"),
        "tool_result must be persisted, got {roles:?}"
    );
    assert!(
        roles.contains(&"channel_outbound"),
        "channel_outbound must be persisted, got {roles:?}"
    );
}

/// Regression test for the `session.model_override` plumbing bug.
///
/// Persisted `Session.model_override` must take effect on the *next*
/// kernel run, even when the caller (e.g. `KernelChannelInbox`) cannot
/// load the session ahead of `kernel.run()` and therefore passes
/// `RunRequest::session_model_override: None`. `SessionPersistHook`
/// publishes the override into `RunCtx` during `on_session_start`, and
/// `resolve_effective_model` reads from there.
#[tokio::test]
async fn session_persist_hook_publishes_model_override_into_run_ctx() {
    /// Provider that advertises both the kernel-default model and a
    /// session-override model, and captures every request it sees.
    struct MultiModelProvider {
        captured: Arc<Mutex<Vec<LlmRequest>>>,
    }

    #[async_trait]
    impl Provider for MultiModelProvider {
        async fn complete(
            &self,
            req: &LlmRequest,
            _ctx: &RunCtx,
            _cancel: Option<&CancellationToken>,
        ) -> Result<LlmResponse, ProviderError> {
            self.captured
                .lock()
                .expect("mutex should not be poisoned")
                .push(req.clone());
            Ok(LlmResponse {
                text: "ok".into(),
                tool_calls: vec![],
                stop_reason: StopReason::EndTurn,
                usage: Usage::default(),
                model: req.model.clone(),
            })
        }

        fn name(&self) -> &'static str {
            "scripted"
        }

        fn capabilities(&self) -> ProviderCapabilities {
            ProviderCapabilities::default()
        }

        fn models(&self) -> Vec<ModelInfo> {
            vec![
                ModelInfo::minimal("default-model", "scripted"),
                ModelInfo::minimal("session-model", "scripted"),
            ]
        }
    }

    let captured = Arc::new(Mutex::new(Vec::new()));
    let provider = MultiModelProvider {
        captured: Arc::clone(&captured),
    };

    let store = Arc::new(MemorySessionStore::default());

    // Seed: pre-create a session for the owner with `model_override` set.
    // Mirrors what `models.set_session` does in production.
    let owner = Owner::new("override-user");
    {
        let mut seed: Session = store
            .find_or_create(&owner, None, &MemoryScope::default())
            .await
            .expect("seed find_or_create");
        seed.model_override = Some("session-model".into());
        store.save(&seed).await.expect("seed save");
    }

    let kernel = Kernel::builder()
        .provider(provider)
        .policy(AllowAllPolicy)
        .add_hook(SessionPersistHook::new(Arc::clone(&store)))
        .build();

    kernel
        // Caller has no session context (e.g. `KernelChannelInbox`).
        .run(
            make_request("default-model", "override-user", "trigger", 2),
            None,
        )
        .await
        .expect("run with hook-published override");

    let captured = captured.lock().expect("mutex should not be poisoned");
    assert_eq!(captured.len(), 1, "provider should have seen one request");
    assert_eq!(
        captured[0].model.as_str(),
        "session-model",
        "SessionPersistHook must publish `session.model_override` into RunCtx \
         so resolve_effective_model picks it up; got {:?}",
        captured[0].model
    );
}

#[tokio::test]
async fn session_persist_hook_publishes_reasoning_effort_override_into_run_ctx() {
    struct EffortProvider {
        captured: Arc<Mutex<Vec<LlmRequest>>>,
    }

    #[async_trait]
    impl Provider for EffortProvider {
        async fn complete(
            &self,
            req: &LlmRequest,
            _ctx: &RunCtx,
            _cancel: Option<&CancellationToken>,
        ) -> Result<LlmResponse, ProviderError> {
            self.captured
                .lock()
                .expect("mutex should not be poisoned")
                .push(req.clone());
            Ok(LlmResponse {
                text: "ok".into(),
                tool_calls: vec![],
                stop_reason: StopReason::EndTurn,
                usage: Usage::default(),
                model: req.model.clone(),
            })
        }

        fn name(&self) -> &'static str {
            "scripted"
        }

        fn capabilities(&self) -> ProviderCapabilities {
            ProviderCapabilities::default()
        }

        fn models(&self) -> Vec<ModelInfo> {
            let mut model = ModelInfo::minimal("default-model", "scripted");
            model.caps.reasoning_effort = Some(ReasoningEffort::Low);
            vec![model]
        }
    }

    let captured = Arc::new(Mutex::new(Vec::new()));
    let store = Arc::new(MemorySessionStore::default());
    let owner = Owner::new("effort-user");
    {
        let mut seed: Session = store
            .find_or_create(&owner, None, &MemoryScope::default())
            .await
            .expect("seed find_or_create");
        seed.reasoning_effort_override = Some(ReasoningEffort::High);
        store.save(&seed).await.expect("seed save");
    }

    let kernel = Kernel::builder()
        .provider(EffortProvider {
            captured: Arc::clone(&captured),
        })
        .policy(AllowAllPolicy)
        .add_hook(SessionPersistHook::new(Arc::clone(&store)))
        .build();

    kernel
        .run(
            make_request("default-model", "effort-user", "trigger", 2),
            None,
        )
        .await
        .expect("run with hook-published effort override");

    let captured = captured.lock().expect("mutex should not be poisoned");
    assert_eq!(captured.len(), 1);
    assert_eq!(captured[0].reasoning_effort, Some(ReasoningEffort::High));
}
