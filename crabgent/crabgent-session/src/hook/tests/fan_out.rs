use std::sync::Arc;

use crabgent_core::{ContentBlock, MemoryScope, Outcome, Owner};
use crabgent_store::memory::MemorySessionStore;

use super::super::*;
use super::{channel_outbound, ctx_for, user};

#[tokio::test]
async fn on_stop_fans_out_foreign_channel_outbound_to_conv_session() {
    let (store, hook, ctx) = started_hook("agent-runner").await;
    let msgs = vec![
        user("cron trigger"),
        channel_outbound("slack:T1/D456", "1700.0001", "your report is ready"),
    ];
    hook.on_message(&msgs, &ctx).await;
    hook.on_stop(&ctx, &Outcome::Completed("done".into())).await;

    let dm = find_dm_session(&store, "slack:T1/D456").await;
    assert_eq!(
        dm.messages.len(),
        2,
        "fan-out lands the audit ChannelOutbound plus an LLM-visible user-framed note"
    );
    assert!(matches!(
        &dm.messages[0],
        Message::ChannelOutbound { message_id, .. } if message_id == "1700.0001"
    ));
    let user_note = match &dm.messages[1] {
        Message::User { content, .. } => content,
        other => panic!("expected user-framed fan-out note, got {other:?}"),
    };
    let text = match &user_note[0] {
        ContentBlock::Text { text } => text,
        other => panic!("expected text block, got {other:?}"),
    };
    assert!(
        text.contains("your report is ready") && text.contains("[notify_user record]"),
        "user-framed note must carry the body and the marker: {text}"
    );
}

#[tokio::test]
async fn errored_run_does_not_fan_out_foreign_channel_outbound() {
    let (store, hook, ctx) = started_hook("agent-runner").await;
    hook.on_message(
        &[channel_outbound(
            "slack:T1/D456",
            "1700.0099",
            "should not fan out",
        )],
        &ctx,
    )
    .await;
    hook.on_stop(&ctx, &Outcome::Errored("provider failed".into()))
        .await;

    let dm = find_dm_session(&store, "slack:T1/D456").await;
    assert!(
        dm.messages.is_empty(),
        "errored runs must drop cached outbound state without notify_user fan-out"
    );
}

#[tokio::test]
async fn fan_out_scope_conv_matches_recipient_conv() {
    // Origin is a cron run: subject carries no channel attr, so
    // open_session's scope has channel = None. The fan-out must still
    // produce a row that the recipient's later DM-run can find when it
    // looks up with the delivery channel stamped in scope.channel.
    let (store, hook, ctx) = started_hook("agent-runner").await;
    let msgs = vec![
        user("cron trigger"),
        channel_outbound("slack:T1/D456", "1700.0010", "scope-stamp-check"),
    ];
    hook.on_message(&msgs, &ctx).await;
    hook.on_stop(&ctx, &Outcome::Completed("done".into())).await;

    // The fan-out row must be found (not created anew) when looked up with the
    // recipient conv AND channel stamped in scope, matching what a DM-run on
    // the slack adapter would use.
    let dm = find_dm_session(&store, "slack:T1/D456").await;
    assert_eq!(
        dm.messages.len(),
        2,
        "find_or_create must return the existing fan-out row, not a new empty session"
    );
    assert_eq!(
        dm.scope.conv,
        Some("slack:T1/D456".to_owned()),
        "fan-out row scope.conv must equal the recipient conv"
    );
    assert_eq!(
        dm.scope.channel,
        Some("slack".to_owned()),
        "fan-out row scope.channel must equal the delivery channel \
         so the recipient DM lookup hits even when the origin run is \
         cron-context (origin.scope.channel = None)"
    );
}

#[tokio::test]
async fn fan_out_cron_origin_lookup_without_channel_misses_fan_out_row() {
    // Belt-and-braces guard for the recipient-DM-lookup contract: when a
    // caller queries with `scope.channel = None` (the cron-context lookup
    // shape that existed before the stamp fix), it must NOT find the
    // fan-out row that targets a real channel. This pins the asymmetry:
    // the fan-out row is keyed to the delivery channel; a no-channel
    // lookup creates a fresh empty session instead of accidentally
    // matching the channel-stamped row.
    let (store, hook, ctx) = started_hook("agent-runner").await;
    let msgs = vec![
        user("cron trigger"),
        channel_outbound("slack:T1/D999", "1700.0030", "channel-stamp-guard"),
    ];
    hook.on_message(&msgs, &ctx).await;
    hook.on_stop(&ctx, &Outcome::Completed("done".into())).await;

    let no_channel = store
        .find_or_create(
            &Owner::new("slack:T1/D999"),
            None,
            &MemoryScope {
                conv: Some("slack:T1/D999".to_owned()),
                ..MemoryScope::default()
            },
        )
        .await
        .expect("lookup resolves");
    assert!(
        no_channel.messages.is_empty(),
        "channel=None lookup must NOT alias to the channel-stamped fan-out row"
    );
}

#[tokio::test]
async fn on_stop_skips_outbound_for_own_session_conv() {
    let store = Arc::new(MemorySessionStore::default());
    let hook = SessionPersistHook::new(Arc::clone(&store));
    let ctx = ctx_for("slack:T1/C1");
    hook.on_session_start(&ctx).await;
    hook.on_message(
        &[channel_outbound(
            "slack:T1/C1",
            "1700.0002",
            "reply in place",
        )],
        &ctx,
    )
    .await;
    hook.on_stop(&ctx, &Outcome::Completed("done".into())).await;

    let own = store
        .find_or_create(&Owner::new("slack:T1/C1"), None, &MemoryScope::default())
        .await
        .expect("own session resolves");
    assert_eq!(
        own.messages.len(),
        1,
        "same-conv outbound stays single, no fan-out duplicate"
    );
}

#[tokio::test]
async fn fan_out_is_idempotent_across_runs() {
    let store = Arc::new(MemorySessionStore::default());
    let hook = SessionPersistHook::new(Arc::clone(&store));
    let outbound = channel_outbound("slack:T1/D789", "1700.0003", "ping");

    let ctx1 = ctx_for("agent-runner");
    hook.on_session_start(&ctx1).await;
    hook.on_message(std::slice::from_ref(&outbound), &ctx1)
        .await;
    hook.on_stop(&ctx1, &Outcome::Completed("done".into()))
        .await;

    let ctx2 = ctx_for("agent-runner");
    hook.on_session_start(&ctx2).await;
    hook.on_message(std::slice::from_ref(&outbound), &ctx2)
        .await;
    hook.on_stop(&ctx2, &Outcome::Completed("done".into()))
        .await;

    let dm = find_dm_session(&store, "slack:T1/D789").await;
    assert_eq!(
        dm.messages.len(),
        2,
        "repeated fan-out must not duplicate: one ChannelOutbound + one user-framed note"
    );
}

#[tokio::test]
async fn two_crons_same_user_distinct_rows() {
    // Two cron runs with the same target conv but DISTINCT scope.agent
    // must produce two distinct fan-out session rows in the recipient DM,
    // not collapse into one shared row. A regression test flagged this gap.
    let store = Arc::new(MemorySessionStore::default());
    let hook = SessionPersistHook::new(Arc::clone(&store));
    let target_conv = "slack:T1/D100";

    for agent_name in ["agent_alpha", "agent_beta"] {
        let ctx = RunCtx::new(
            RunId::new(),
            Subject::new("cron-sender").with_attr("agent", agent_name),
        );
        hook.on_session_start(&ctx).await;
        let msg_id = format!("1700.{agent_name}");
        let msgs = vec![
            user("cron trigger"),
            channel_outbound(target_conv, &msg_id, "your report is ready"),
        ];
        hook.on_message(&msgs, &ctx).await;
        hook.on_stop(&ctx, &Outcome::Completed("done".into())).await;
    }

    let row_a = store
        .find_or_create(
            &Owner::new(target_conv),
            None,
            &MemoryScope {
                conv: Some(target_conv.to_owned()),
                channel: Some("slack".to_owned()),
                agent: Some("agent_alpha".to_owned()),
                ..MemoryScope::default()
            },
        )
        .await
        .expect("agent_alpha fan-out row resolves");
    let row_b = store
        .find_or_create(
            &Owner::new(target_conv),
            None,
            &MemoryScope {
                conv: Some(target_conv.to_owned()),
                channel: Some("slack".to_owned()),
                agent: Some("agent_beta".to_owned()),
                ..MemoryScope::default()
            },
        )
        .await
        .expect("agent_beta fan-out row resolves");

    assert_ne!(
        row_a.id, row_b.id,
        "distinct scope.agent must produce distinct fan-out session rows"
    );
    assert_channel_outbound_message(&row_a.messages, "1700.agent_alpha");
    assert_no_channel_outbound_message(&row_a.messages, "1700.agent_beta");
    assert_channel_outbound_message(&row_b.messages, "1700.agent_beta");
    assert_no_channel_outbound_message(&row_b.messages, "1700.agent_alpha");
}

fn assert_channel_outbound_message(messages: &[Message], expected_message_id: &str) {
    assert!(
        has_channel_outbound_message(messages, expected_message_id),
        "expected fan-out row to contain ChannelOutbound message_id={expected_message_id}"
    );
}

fn assert_no_channel_outbound_message(messages: &[Message], unexpected_message_id: &str) {
    assert!(
        !has_channel_outbound_message(messages, unexpected_message_id),
        "fan-out row must not contain ChannelOutbound message_id={unexpected_message_id}"
    );
}

fn has_channel_outbound_message(messages: &[Message], expected_message_id: &str) -> bool {
    messages.iter().any(|message| match message {
        Message::ChannelOutbound { message_id, .. } => message_id == expected_message_id,
        _ => false,
    })
}

async fn started_hook(
    subject: &str,
) -> (
    Arc<MemorySessionStore>,
    SessionPersistHook<MemorySessionStore>,
    RunCtx,
) {
    let store = Arc::new(MemorySessionStore::default());
    let hook = SessionPersistHook::new(Arc::clone(&store));
    let ctx = ctx_for(subject);
    hook.on_session_start(&ctx).await;
    (store, hook, ctx)
}

async fn find_dm_session(store: &MemorySessionStore, owner: &str) -> Session {
    store
        .find_or_create(
            &Owner::new(owner),
            None,
            &MemoryScope {
                conv: Some(owner.to_owned()),
                channel: Some("slack".to_owned()),
                ..MemoryScope::default()
            },
        )
        .await
        .expect("dm session resolves")
}
