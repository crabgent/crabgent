//! Shared test fixtures for the crabgent workspace.
//!
//! This crate consolidates the `Provider`/channel/command test doubles and the
//! message/response builders that were hand-rolled per crate and per test file.
//! It is a test-support crate: depend on it only under `[dev-dependencies]`.
//! Dev-dependency cycles (for example `crabgent-channel` -> `crabgent-test-support`
//! -> `crabgent-channel`) are permitted by cargo because dev-deps do not
//! participate in the normal build graph.
//!
//! # Surface
//!
//! - [`StubProvider`]: configurable [`Provider`](crabgent_core::provider::Provider)
//!   double (canned/scripted/failure, capabilities, models, call-count and
//!   captured-request introspection).
//! - Builders: [`done`], [`done_for_model`], [`tool_use`], [`user_msg`],
//!   [`assistant`], [`assistant_with_tools`], [`text_block`], [`tool_call`].
//! - Channel doubles: [`RecordingSink`], [`RecordingChannel`], [`CountingInbox`],
//!   [`RecordingInbox`], plus [`inbound_event`], [`inbound_reaction`], and
//!   [`human_participant`].
//! - Command double: [`StubCommand`].
//! - Media-byte builders: [`minimal_ogg_bytes`].

mod builders;
mod channel;
mod command;
mod media;
mod provider;

pub use builders::{
    assistant, assistant_with_tools, done, done_for_model, text_block, tool_call, tool_use,
    user_msg,
};
pub use channel::{
    CountingInbox, RecordingChannel, RecordingInbox, RecordingSink, human_participant,
    inbound_event, inbound_reaction,
};
pub use command::StubCommand;
pub use media::minimal_ogg_bytes;
pub use provider::StubProvider;

#[cfg(test)]
mod tests {
    use super::*;
    use crabgent_channel::{Channel, ChannelInbox, ChannelKind, ChannelSink, MessageRef};
    use crabgent_command::{Command, CommandName};
    use crabgent_core::provider::{Provider, ProviderCapabilities};
    use crabgent_core::{
        Action, LlmRequest, Owner, ProviderError, RunCtx, RunId, StopReason, Subject,
    };
    use serde_json::json;
    use std::sync::Arc;
    use tokio_util::sync::CancellationToken;

    fn ctx() -> RunCtx {
        RunCtx::new(RunId::new(), Subject::new("test-subject"))
    }

    fn request() -> LlmRequest {
        // `LlmRequest.messages` is a loose `Vec<Value>`; the stub ignores their
        // shape, so an empty list keeps the fixture minimal.
        LlmRequest {
            model: "m".into(),
            system_prompt: None,
            messages: Vec::new(),
            tools: Vec::new(),
            max_tokens: None,
            temperature: None,
            stop_sequences: Vec::new(),
            reasoning_effort: None,
            web_search: crabgent_core::WebSearchConfig::default(),
            tool_choice: None,
        }
    }

    // --- builders ---------------------------------------------------------

    #[test]
    fn done_builds_end_turn_text() {
        let r = done("hello");
        assert_eq!(r.text, "hello");
        assert_eq!(r.stop_reason, StopReason::EndTurn);
        assert!(r.tool_calls.is_empty());
        assert_eq!(r.model.as_str(), "m");
    }

    #[test]
    fn done_for_model_sets_model_id() {
        assert_eq!(done_for_model("x", "claude").model.as_str(), "claude");
    }

    #[test]
    fn tool_use_builds_tool_stop() {
        let r = tool_use(vec![tool_call("c1", "search", json!({"q": 1}))]);
        assert_eq!(r.stop_reason, StopReason::ToolUse);
        assert_eq!(r.tool_calls.len(), 1);
        assert_eq!(r.tool_calls[0].name, "search");
    }

    #[test]
    fn message_builders_shape() {
        assert!(matches!(user_msg("u"), crabgent_core::Message::User { .. }));
        assert!(matches!(
            assistant("a"),
            crabgent_core::Message::Assistant { tool_calls, .. } if tool_calls.is_empty()
        ));
        assert!(matches!(
            assistant_with_tools("a", vec![tool_call("c", "n", json!({}))]),
            crabgent_core::Message::Assistant { tool_calls, .. } if tool_calls.len() == 1
        ));
        assert!(matches!(
            text_block("t"),
            crabgent_core::ContentBlock::Text { text } if text == "t"
        ));
    }

    // --- StubProvider -----------------------------------------------------

    #[tokio::test]
    async fn stub_provider_canned_repeats() {
        let p = StubProvider::with_text("canned");
        let a = p.complete(&request(), &ctx(), None).await.expect("ok");
        let b = p.complete(&request(), &ctx(), None).await.expect("ok");
        assert_eq!(a.text, "canned");
        assert_eq!(b.text, "canned");
        assert_eq!(p.call_count(), 2);
    }

    #[tokio::test]
    async fn stub_provider_scripted_sequence_then_exhausts() {
        let p = StubProvider::new().responses(vec![done("first"), done("second")]);
        assert_eq!(
            p.complete(&request(), &ctx(), None).await.expect("ok").text,
            "first"
        );
        assert_eq!(
            p.complete(&request(), &ctx(), None).await.expect("ok").text,
            "second"
        );
        let exhausted = p.complete(&request(), &ctx(), None).await;
        assert!(matches!(exhausted, Err(ProviderError::Other(_))));
    }

    #[tokio::test]
    async fn stub_provider_fail_with_always_fails() {
        let p = StubProvider::new().fail_with(|| ProviderError::Other("boom".into()));
        p.complete(&request(), &ctx(), None)
            .await
            .expect_err("first call fails");
        p.complete(&request(), &ctx(), None)
            .await
            .expect_err("second call fails");
    }

    #[tokio::test]
    async fn stub_provider_fail_on_nth_only() {
        let p = StubProvider::with_text("ok").fail_on(2, || ProviderError::Api {
            status: 503,
            message: "unavailable".into(),
            retry_after_secs: None,
        });
        p.complete(&request(), &ctx(), None)
            .await
            .expect("first call succeeds");
        assert!(matches!(
            p.complete(&request(), &ctx(), None).await,
            Err(ProviderError::Api { status: 503, .. })
        ));
        p.complete(&request(), &ctx(), None)
            .await
            .expect("third call succeeds");
    }

    #[test]
    fn stub_provider_capabilities_and_models_configurable() {
        let caps = ProviderCapabilities {
            tools: true,
            vision: true,
            ..ProviderCapabilities::default()
        };
        let p = StubProvider::new()
            .with_capabilities(caps)
            .with_name("custom")
            .with_models(vec![
                crabgent_core::ModelInfo::minimal("a", "custom"),
                crabgent_core::ModelInfo::minimal("b", "custom"),
            ]);
        assert!(p.capabilities().tools);
        assert!(p.capabilities().vision);
        assert_eq!(p.name(), "custom");
        assert_eq!(p.models().len(), 2);
    }

    #[test]
    fn stub_provider_with_tools_toggles_flag() {
        assert!(StubProvider::new().with_tools(true).capabilities().tools);
        assert!(!StubProvider::new().capabilities().tools);
    }

    #[tokio::test]
    async fn stub_provider_captures_requests() {
        let p = StubProvider::new();
        let mut req = request();
        req.model = "probe-model".into();
        p.complete(&req, &ctx(), None).await.expect("ok");
        let captured = p.captured_requests();
        assert_eq!(captured.len(), 1);
        assert_eq!(captured[0].model.as_str(), "probe-model");
    }

    #[tokio::test]
    async fn stub_provider_default_stream_wraps_complete() {
        use futures::StreamExt as _;
        let p = StubProvider::with_text("streamed");
        let cancel = CancellationToken::new();
        let mut stream = p
            .stream(&request(), &ctx(), Some(&cancel))
            .await
            .expect("stream");
        let mut saw_text = false;
        while let Some(ev) = stream.next().await {
            if let Ok(crabgent_core::provider::ProviderEvent::TextDelta(t)) = ev {
                saw_text = t == "streamed";
            }
        }
        assert!(saw_text);
    }

    // --- channel fixtures -------------------------------------------------

    #[tokio::test]
    async fn recording_sink_records_send_and_react() {
        let sink = RecordingSink::new();
        let subject = Subject::new("agent");
        let conv = Owner::new("test:conv");
        sink.send(
            &subject,
            &conv,
            &crabgent_channel::OutboundMessage::new("body"),
        )
        .await
        .expect("send");
        let parent = MessageRef::top_level("test", conv.clone(), "p1");
        sink.react(&subject, &conv, &parent, "+1")
            .await
            .expect("react");
        assert_eq!(sink.sent(), vec!["body".to_owned()]);
        assert_eq!(sink.sent_count(), 1);
        assert_eq!(sink.thread_parents(), vec![None]);
        assert_eq!(sink.reactions(), vec![("p1".to_owned(), "+1".to_owned())]);
    }

    #[tokio::test]
    async fn recording_channel_records_every_op() {
        let channel = RecordingChannel::new("test", ChannelKind::Direct, "stub-id");
        let subject = Subject::new("agent");
        let conv = Owner::new("test:1");
        assert_eq!(
            channel.kind(&conv).await.expect("kind"),
            ChannelKind::Direct
        );
        assert_eq!(
            channel
                .participants(&subject, &conv)
                .await
                .expect("parts")
                .len(),
            1
        );
        let r = channel
            .send(
                &subject,
                &conv,
                &crabgent_channel::OutboundMessage::new("hi"),
            )
            .await
            .expect("send");
        assert_eq!(r.id, "stub-id");
        let parent = MessageRef::top_level("test", conv.clone(), "p");
        channel
            .react(&subject, &conv, &parent, "x")
            .await
            .expect("react");
        channel
            .edit(&subject, &conv, &parent, "new")
            .await
            .expect("edit");
        channel
            .delete(&subject, &conv, &parent)
            .await
            .expect("delete");
        let read = channel.read(&subject, &conv, None, 5).await.expect("read");
        assert_eq!(read.len(), 1);
        assert_eq!(channel.sent_count(), 1);
        assert_eq!(channel.last_sent().expect("last").body, "hi");
        assert_eq!(channel.react_count(), 1);
        assert_eq!(channel.edit_count(), 1);
        assert_eq!(channel.delete_count(), 1);
        assert_eq!(channel.read_count(), 1);
    }

    #[tokio::test]
    async fn recording_channel_with_participants_override() {
        let channel = RecordingChannel::default()
            .with_participants(vec![human_participant(), human_participant()]);
        let parts = channel
            .participants(&Subject::new("a"), &Owner::new("test:1"))
            .await
            .expect("parts");
        assert_eq!(parts.len(), 2);
    }

    #[tokio::test]
    async fn counting_inbox_counts_events_and_reactions() {
        let inbox = CountingInbox::new();
        inbox
            .receive(inbound_event("hello"))
            .await
            .expect("receive");
        inbox
            .receive_reaction(inbound_reaction("+1"))
            .await
            .expect("reaction");
        assert_eq!(inbox.received_count(), 1);
        assert_eq!(inbox.reaction_count(), 1);
    }

    #[tokio::test]
    async fn recording_inbox_records_bodies_and_emojis() {
        let inbox = RecordingInbox::new();
        inbox
            .receive(inbound_event("first"))
            .await
            .expect("receive");
        inbox
            .receive(inbound_event("second"))
            .await
            .expect("receive");
        inbox
            .receive_reaction(inbound_reaction("eyes"))
            .await
            .expect("reaction");
        assert_eq!(
            inbox.events(),
            vec!["first".to_owned(), "second".to_owned()]
        );
        assert_eq!(inbox.received_count(), 2);
        assert_eq!(inbox.reactions(), vec!["eyes".to_owned()]);
    }

    #[test]
    fn inbound_event_has_expected_shape() {
        let ev = inbound_event("hi");
        assert_eq!(ev.body, "hi");
        assert_eq!(ev.channel, "test");
        assert_eq!(ev.kind, Some(ChannelKind::Group));
    }

    // --- StubCommand ------------------------------------------------------

    #[test]
    fn stub_command_name_and_description() {
        let cmd = StubCommand::new("stub").with_description("custom desc");
        assert_eq!(cmd.name(), &CommandName::parse("stub").expect("name"));
        assert_eq!(cmd.description(), "custom desc");
    }

    #[tokio::test]
    async fn stub_command_policy_action_configurable() {
        let cmd = StubCommand::new("stub").with_policy_action(Action::custom("custom.act"));
        let action = cmd
            .policy_action("input", &command_ctx(Arc::new(RecordingSink::new())))
            .await
            .expect("policy action");
        assert_eq!(action, Action::custom("custom.act"));
    }

    #[tokio::test]
    async fn stub_command_execute_sends_reply() {
        let cmd = StubCommand::new("stub");
        let sink = Arc::new(RecordingSink::new());
        let out = cmd
            .execute("ping", &command_ctx(Arc::clone(&sink)))
            .await
            .expect("execute");
        assert_eq!(out.reply, "stub: ping");
        assert_eq!(sink.sent(), vec!["stub: ping".to_owned()]);
        assert_eq!(cmd.calls(), 1);
    }

    #[tokio::test]
    async fn stub_command_without_sink_reply_counts_but_skips_send() {
        let cmd = StubCommand::new("stub").without_sink_reply();
        let sink = Arc::new(RecordingSink::new());
        let out = cmd
            .execute("ping", &command_ctx(Arc::clone(&sink)))
            .await
            .expect("execute");
        cmd.execute("pong", &command_ctx(Arc::clone(&sink)))
            .await
            .expect("execute");
        assert_eq!(out.reply, "stub: ping");
        assert_eq!(cmd.calls(), 2);
        assert_eq!(sink.sent_count(), 0);
    }

    fn command_ctx(sink: Arc<RecordingSink>) -> crabgent_command::CommandCtx {
        let event = inbound_event("/stub ping");
        let sink: Arc<dyn ChannelSink> = sink;
        crabgent_command::CommandCtx::new(
            Subject::new("agent"),
            crabgent_store::SessionId::new(),
            event,
            sink,
        )
    }

    // --- media builders ---------------------------------------------------

    #[test]
    fn minimal_ogg_bytes_has_ogg_capture_pattern() {
        let bytes = minimal_ogg_bytes();
        assert_eq!(bytes.len(), 64);
        assert_eq!(&bytes[..4], b"OggS");
    }
}
