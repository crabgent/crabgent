//! Command wrapper for `ModelRegistryTool`.

use std::sync::Arc;

use async_trait::async_trait;
use crabgent_command::{
    Command, CommandCtx, CommandError, CommandName, CommandOutput, ToolCommand,
};
use crabgent_core::{Action, Tool};
use crabgent_tool_models::ModelRegistryTool;
use serde_json::{Value, json};

use crate::format::format_reply;
use crate::parser::CliArgs;

const COMMAND_NAME: &str = "model";
const DESCRIPTION: &str = "Inspect and set the current session model.";

/// Command adapter for [`ModelRegistryTool`].
pub struct ModelCommand {
    name: CommandName,
    tool: ToolCommand,
}

impl ModelCommand {
    /// Build a model command around the existing model registry tool.
    #[must_use]
    pub fn new(tool: Arc<ModelRegistryTool>) -> Self {
        let tool: Arc<dyn Tool> = tool;
        Self {
            name: COMMAND_NAME
                .parse()
                .expect("static model command name is valid"),
            tool: ToolCommand::new(tool),
        }
    }
}

#[async_trait]
impl Command for ModelCommand {
    fn name(&self) -> &CommandName {
        &self.name
    }

    fn description(&self) -> &'static str {
        DESCRIPTION
    }

    async fn policy_action(&self, input: &str, ctx: &CommandCtx) -> Result<Action, CommandError> {
        Ok(match CliArgs::parse(input)? {
            CliArgs::List => Action::ModelList,
            CliArgs::Get { id } => Action::ModelGet { id },
            CliArgs::Set { id } => Action::ModelsSetSessionOverride {
                session_id: ctx.session_id().to_string(),
                model: id,
            },
        })
    }

    async fn execute(&self, input: &str, ctx: &CommandCtx) -> Result<CommandOutput, CommandError> {
        let parsed = CliArgs::parse(input)?;
        let args = tool_args(&parsed);
        let result = self.tool.execute(args, ctx).await?;
        if result.is_error {
            return Err(CommandError::Execution(format_reply(
                &parsed,
                &result.output,
            )));
        }
        let reply = format_reply(&parsed, &result.output);
        ctx.send_reply(reply.clone())
            .await
            .map_err(|err| CommandError::Execution(format!("model reply send failed: {err}")))?;
        Ok(CommandOutput::new(reply))
    }
}

fn tool_args(args: &CliArgs) -> Value {
    match args {
        CliArgs::List => json!({ "op": "list" }),
        CliArgs::Get { id } => json!({ "op": "get", "id": id.as_str() }),
        CliArgs::Set { id } => json!({ "op": "set_session", "model": id.as_str() }),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;

    use crabgent_channel::{ChannelSink, InboundEvent, MessageRef, Participant, ParticipantRole};
    use crabgent_core::{
        AllowAllPolicy, ContentBlock, GlobalModelOverrideStore, GlobalReasoningEffortOverrideStore,
        Kernel, KernelBuilder, MemoryScope, Message, ModelCapabilities, ModelId, ModelInfo, Owner,
        Subject,
    };
    use crabgent_store::memory::{MemoryGlobalOverrideStore, MemorySessionStore};
    use crabgent_store::{SessionId, SessionStore};
    use crabgent_test_support::{RecordingSink, StubProvider};

    use super::*;

    fn caps() -> ModelCapabilities {
        ModelCapabilities {
            max_input_tokens: 200_000,
            max_output_tokens: 8_192,
            default_max_output_tokens: 4_096,
            default_temperature_milli: 1_000,
            supports_tools: true,
            supports_vision: true,
            supports_audio: false,
            supports_thinking: true,
            supports_prompt_cache: true,
            supports_web_search: false,
            supports_temperature: true,
            reasoning_effort: None,
        }
    }

    fn model(provider: &'static str, id: &str, aliases: Vec<ModelId>) -> ModelInfo {
        ModelInfo {
            id: ModelId::new(id),
            provider: provider.to_owned(),
            display_name: id.to_owned(),
            aliases,
            caps: caps(),
            pricing: None,
            extensions: HashMap::new(),
        }
    }

    fn provider(name: &'static str, models: Vec<ModelInfo>) -> StubProvider {
        StubProvider::new().with_name(name).with_models(models)
    }

    fn kernel() -> Arc<Kernel> {
        Arc::new(
            KernelBuilder::new()
                .provider(provider(
                    "anthropic",
                    vec![
                        model(
                            "anthropic",
                            "claude-sonnet-4-6",
                            vec![ModelId::new("sonnet")],
                        ),
                        model("anthropic", "claude-haiku-4-5", Vec::new()),
                    ],
                ))
                .provider(provider(
                    "openai",
                    vec![model("openai", "gpt-5.5", Vec::new())],
                ))
                .policy(AllowAllPolicy)
                .build(),
        )
    }

    fn command(store: Arc<MemorySessionStore>) -> ModelCommand {
        let global: Arc<MemoryGlobalOverrideStore> = Arc::new(MemoryGlobalOverrideStore::default());
        let store_dyn: Arc<dyn SessionStore> = store;
        let global_dyn: Arc<dyn GlobalModelOverrideStore> = global.clone();
        let effort_dyn: Arc<dyn GlobalReasoningEffortOverrideStore> = global;
        ModelCommand::new(Arc::new(ModelRegistryTool::new(
            kernel(),
            Arc::new(AllowAllPolicy),
            store_dyn,
            global_dyn,
            effort_dyn,
        )))
    }

    async fn save_session(store: &MemorySessionStore) -> SessionId {
        let mut session = store
            .find_or_create(&Owner::new("alice"), None, &MemoryScope::default())
            .await
            .expect("session created");
        session.messages = vec![Message::User {
            content: vec![ContentBlock::Text {
                text: "hello".to_owned(),
            }],
            timestamp: None,
        }];
        let id = session.id.clone();
        store.save(&session).await.expect("session saved");
        id
    }

    fn inbound_event() -> InboundEvent {
        let conv = Owner::new("alice");
        InboundEvent {
            channel: "test".into(),
            conv: conv.clone(),
            kind: None,
            from: Participant::new("alice", ParticipantRole::Human),
            message: MessageRef::top_level("test", conv, "msg-1"),
            body: "/model list".into(),
            attachments: Vec::new(),
            timestamp: crabgent_store::Utc::now(),
        }
    }

    fn ctx(session_id: SessionId, sink: Arc<dyn ChannelSink>) -> CommandCtx {
        CommandCtx::new(Subject::new("alice"), session_id, inbound_event(), sink)
    }

    #[tokio::test]
    async fn model_list_returns_formatted_text() {
        let store = Arc::new(MemorySessionStore::default());
        let session_id = save_session(&store).await;
        let sink = Arc::new(RecordingSink::default());
        let ctx = ctx(session_id, sink.clone() as Arc<dyn ChannelSink>);

        let output = command(store).execute("list", &ctx).await.expect("list");

        assert!(output.reply.contains("Models (showing 3 of 3)"));
        assert!(output.reply.contains("claude-sonnet-4-6"));
        assert_eq!(sink.sent_count(), 1);
    }

    #[tokio::test]
    async fn model_set_persists_model_override_via_session_store_save() {
        let store = Arc::new(MemorySessionStore::default());
        let session_id = save_session(&store).await;
        let sink = Arc::new(RecordingSink::default());
        let ctx = ctx(session_id.clone(), sink as Arc<dyn ChannelSink>);

        command(Arc::clone(&store))
            .execute("set sonnet", &ctx)
            .await
            .expect("set session model");

        let loaded = store
            .load(&session_id)
            .await
            .expect("session load succeeds")
            .expect("session exists");
        assert_eq!(loaded.model_override.as_deref(), Some("sonnet"));
    }

    #[tokio::test]
    async fn model_set_invalid_model_id_returns_safe_error_reply() {
        let store = Arc::new(MemorySessionStore::default());
        let session_id = save_session(&store).await;
        let sink = Arc::new(RecordingSink::default());
        let ctx = ctx(session_id, sink as Arc<dyn ChannelSink>);

        let err = command(store)
            .execute("set unknown-model", &ctx)
            .await
            .expect_err("unknown model errors");

        assert_eq!(err.safe_reply(), "command failed");
    }

    #[tokio::test]
    async fn model_set_does_not_trigger_session_persist_hook() {
        let store = Arc::new(MemorySessionStore::default());
        let session_id = save_session(&store).await;
        let sink = Arc::new(RecordingSink::default());
        let ctx = ctx(session_id.clone(), sink as Arc<dyn ChannelSink>);

        command(Arc::clone(&store))
            .execute("set sonnet", &ctx)
            .await
            .expect("set session model");

        let loaded = store
            .load(&session_id)
            .await
            .expect("session load succeeds")
            .expect("session exists");
        assert_eq!(loaded.messages.len(), 1);
        assert_eq!(loaded.model_override.as_deref(), Some("sonnet"));
    }
}
