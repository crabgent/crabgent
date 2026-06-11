//! End-to-end exercise of the channel-side tools (send, react, edit,
//! delete, read, upload) against a real Conduit homeserver spun up via
//! testcontainers. Skips silently when Docker is unavailable.

#![allow(
    clippy::similar_names,
    clippy::too_many_lines,
    clippy::option_if_let_else
)]

mod support;

use std::sync::Arc;
use std::time::Duration;

use base64::Engine;
use crabgent_channel::{
    ChannelDeleteTool, ChannelEditTool, ChannelKind, ChannelReactTool, ChannelReadTool,
    ChannelRouter, ChannelSendTool, ChannelSink, ChannelSubjectExt, ChannelUploadTool, MessageRef,
};
use crabgent_channel_matrix::MatrixChannel;
use crabgent_core::{
    owner::Owner,
    policy::{AllowAllPolicy, PolicyHook},
    subject::Subject,
    tool::{Tool, ToolCtx},
};
use matrix_sdk::Client;
use matrix_sdk::ruma::events::AnySyncTimelineEvent;
use matrix_sdk::ruma::events::room::message::MessageType;
use matrix_sdk::ruma::serde::Raw;
use matrix_sdk::ruma::{OwnedRoomId, OwnedUserId, owned_user_id};

fn allow_all() -> Arc<dyn PolicyHook> {
    Arc::new(AllowAllPolicy)
}
use serde_json::{Value, json};

const MATRIX_CHANNEL: &str = "matrix";

struct Harness {
    _alice: Client,
    bot: Client,
    bot_user_id: OwnedUserId,
    room_id: OwnedRoomId,
    sink: Arc<dyn ChannelSink>,
    conv: Owner,
}

impl Harness {
    async fn new(ctx: &support::MatrixTestCtx) -> support::TestResult<Self> {
        let bot = ctx.register_client("bot").await?;
        let alice = ctx.register_client("alice").await?;
        let bot_user_id = bot.user_id().ok_or("bot is not logged in")?.to_owned();
        let room_id = support::create_public_room(&bot, "channel-tools").await?;
        support::invite_and_join(&bot, &alice, &room_id).await?;
        support::warmup_visibility(&bot, &room_id).await?;
        bot.sync_once(support::short_sync()).await?;

        let channel = Arc::new(MatrixChannel::from_client(
            bot.clone(),
            bot_user_id.clone(),
            Some("Nova".into()),
        ));
        let router = ChannelRouter::new().with_channel(channel as Arc<_>);
        let sink: Arc<dyn ChannelSink> = Arc::new(router);
        let conv = Owner::new(format!("{MATRIX_CHANNEL}:{room_id}"));
        Ok(Self {
            _alice: alice,
            bot,
            bot_user_id,
            room_id,
            sink,
            conv,
        })
    }

    fn subject(&self, inbound: Option<&MessageRef>) -> Subject {
        let mut subject =
            Subject::new("agent").with_channel(MATRIX_CHANNEL, &self.conv, ChannelKind::Group);
        if let Some(parent) = inbound {
            subject = subject.with_inbound_message_ref(parent);
        }
        subject
    }

    async fn run_tool(
        &self,
        tool: &dyn Tool,
        args: Value,
        subject: Subject,
    ) -> Result<Value, crabgent_core::error::ToolError> {
        let ctx = ToolCtx::new(subject);
        tool.execute(args, &ctx).await
    }
}

fn message_ref_from_value(value: &Value, conv: &Owner) -> MessageRef {
    MessageRef {
        channel: value["channel"].as_str().unwrap().to_owned(),
        conv: conv.clone(),
        id: value["id"].as_str().unwrap().to_owned(),
        thread_root: value["thread_root"].as_str().map(str::to_owned),
        broadcast: value["broadcast"].as_bool().unwrap_or(false),
    }
}

async fn timeline_events(
    bot: &Client,
    room_id: &OwnedRoomId,
) -> support::TestResult<Vec<Raw<AnySyncTimelineEvent>>> {
    for _ in 0..10 {
        bot.sync_once(support::short_sync()).await?;
        if let Some(room) = bot.get_room(room_id) {
            let mut options = matrix_sdk::room::MessagesOptions::backward();
            options.limit = matrix_sdk::ruma::UInt::from(50u16);
            let messages = room.messages(options).await?;
            if !messages.chunk.is_empty() {
                return Ok(messages
                    .chunk
                    .into_iter()
                    .map(|ev| ev.raw().clone())
                    .collect());
            }
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    Ok(Vec::new())
}

#[tokio::test]
#[ignore = "spawns a Conduit container; run explicitly with cargo test --ignored channel_tools"]
async fn channel_tools_end_to_end_against_conduit() -> support::TestResult {
    let Some(ctx) = support::MatrixTestCtx::new().await? else {
        return Ok(());
    };
    let h = Harness::new(&ctx).await?;
    let policy = allow_all();

    // 1) channel_send: create a base message.
    let send = ChannelSendTool::new(Arc::clone(&h.sink), Arc::clone(&policy));
    let sent_value = h
        .run_tool(
            &send,
            json!({"conv": h.conv.as_str(), "body": "hello from nova"}),
            h.subject(None),
        )
        .await?;
    let sent_ref = message_ref_from_value(&sent_value, &h.conv);
    assert_eq!(sent_value["channel"], MATRIX_CHANNEL);
    assert!(!sent_ref.id.is_empty());

    // 2) channel_react: react to the user's inbound (here: the bot's own
    //    sent message, supplied via Subject) with a thumbs-up.
    let react = ChannelReactTool::new(Arc::clone(&h.sink), Arc::clone(&policy));
    let react_value = h
        .run_tool(&react, json!({"emoji": "👍"}), h.subject(Some(&sent_ref)))
        .await?;
    assert_eq!(react_value["channel"], MATRIX_CHANNEL);
    assert!(!react_value["id"].as_str().unwrap().is_empty());

    // 3) channel_edit: rewrite the base message.
    let edit = ChannelEditTool::new(Arc::clone(&h.sink), Arc::clone(&policy));
    let _ = h
        .run_tool(
            &edit,
            json!({
                "conv": h.conv.as_str(),
                "id": sent_ref.id,
                "new_text": "hello from nova (edited)",
            }),
            h.subject(None),
        )
        .await?;

    // 4) channel_upload: send a small binary blob.
    let upload = ChannelUploadTool::new(Arc::clone(&h.sink), Arc::clone(&policy));
    let payload = b"hello-bytes".to_vec();
    let b64 = base64::engine::general_purpose::STANDARD.encode(&payload);
    let upload_value = h
        .run_tool(
            &upload,
            json!({
                "conv": h.conv.as_str(),
                "filename": "nova.txt",
                "content_base64": b64,
                "comment": "ctx",
            }),
            h.subject(None),
        )
        .await?;
    let upload_ref = message_ref_from_value(&upload_value, &h.conv);
    assert!(!upload_ref.id.is_empty());

    // 5) channel_read: fetch recent messages and verify the bot's own
    //    edited text + upload show up.
    let read = ChannelReadTool::new(Arc::clone(&h.sink), Arc::clone(&policy));
    let read_value = h
        .run_tool(
            &read,
            json!({"conv": h.conv.as_str(), "limit": 20}),
            h.subject(None),
        )
        .await?;
    let messages = read_value["messages"]
        .as_array()
        .expect("channel_read returns array");
    let bodies: Vec<String> = messages
        .iter()
        .filter_map(|m| m["text"].as_str().map(str::to_owned))
        .collect();
    assert!(
        bodies.iter().any(|b| b.contains("hello from nova")),
        "expected base or edited text in channel_read output, got: {bodies:?}"
    );

    // 6) channel_delete: remove the base message; verify via matrix-sdk
    //    that the room timeline now carries a redaction for it.
    let delete = ChannelDeleteTool::new(Arc::clone(&h.sink), Arc::clone(&policy));
    let _ = h
        .run_tool(
            &delete,
            json!({"conv": h.conv.as_str(), "id": sent_ref.id}),
            h.subject(None),
        )
        .await?;

    let events = timeline_events(&h.bot, &h.room_id).await?;
    let saw_reaction = events
        .iter()
        .any(|raw| raw.get_field::<String>("type").ok().flatten().as_deref() == Some("m.reaction"));
    let saw_redaction = events.iter().any(|raw| {
        raw.get_field::<String>("type").ok().flatten().as_deref() == Some("m.room.redaction")
    });
    let saw_file = events.iter().any(|raw| {
        let Ok(parsed) = raw.deserialize() else {
            return false;
        };
        match parsed {
            AnySyncTimelineEvent::MessageLike(
                matrix_sdk::ruma::events::AnySyncMessageLikeEvent::RoomMessage(ev),
            ) => match ev.as_original() {
                Some(orig) => matches!(orig.content.msgtype, MessageType::File(_)),
                None => false,
            },
            _ => false,
        }
    });
    assert!(saw_reaction, "expected m.reaction event in timeline");
    assert!(
        saw_redaction,
        "expected m.room.redaction event after delete"
    );
    assert!(saw_file, "expected m.room.message file upload in timeline");

    let _ = h.bot_user_id; // kept for future identity assertions
    let _ = owned_user_id!("@unused:example.org");
    Ok(())
}
