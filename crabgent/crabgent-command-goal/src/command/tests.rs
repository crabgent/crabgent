use std::sync::Arc;

use crabgent_channel::{ChannelSink, InboundEvent, MessageRef, Participant, ParticipantRole};
use crabgent_core::{Action, Owner, Subject};
use crabgent_hook_goal::GoalRuntime;
use crabgent_store::{GoalStatus, MemoryGoalStore, SessionId};
use crabgent_test_support::RecordingSink;

use super::*;

fn command() -> GoalCommand {
    GoalCommand::new(GoalRuntime::new(Arc::new(MemoryGoalStore::default())))
}

fn inbound_event() -> InboundEvent {
    let conv = Owner::new("alice");
    InboundEvent {
        channel: "test".into(),
        conv: conv.clone(),
        kind: None,
        from: Participant::new("alice", ParticipantRole::Human),
        message: MessageRef::top_level("test", conv, "msg-1"),
        body: "/goal".into(),
        attachments: Vec::new(),
        timestamp: crabgent_store::Utc::now(),
    }
}

fn ctx(session: SessionId, sink: Arc<dyn ChannelSink>) -> CommandCtx {
    CommandCtx::new(Subject::new("alice"), session, inbound_event(), sink)
}

#[tokio::test]
async fn set_then_show_roundtrip() {
    let cmd = command();
    let session = SessionId::new();
    let sink = Arc::new(RecordingSink::default());
    let ctx = ctx(session, sink.clone() as Arc<dyn ChannelSink>);

    let set = cmd.execute("ship the release", &ctx).await.expect("set");
    assert_eq!(set.reply, "Goal set: ship the release");

    let show = cmd.execute("", &ctx).await.expect("show");
    assert!(show.reply.contains("Goal: ship the release"));
    assert!(show.reply.contains("Status: active"));
    assert!(show.reply.contains("Tokens used: 0 / unbounded"));
    // Each command sends exactly one channel reply.
    assert_eq!(sink.sent_count(), 2);
}

#[tokio::test]
async fn show_without_goal_reports_none() {
    let cmd = command();
    let sink = Arc::new(RecordingSink::default());
    let ctx = ctx(SessionId::new(), sink as Arc<dyn ChannelSink>);
    let show = cmd.execute("", &ctx).await.expect("show");
    assert_eq!(show.reply, "No goal set for this thread.");
}

#[tokio::test]
async fn pause_resume_clear_drive_host_state() {
    let cmd = command();
    let session = SessionId::new();
    let sink = Arc::new(RecordingSink::default());
    let ctx = ctx(session.clone(), sink as Arc<dyn ChannelSink>);

    cmd.execute("obj", &ctx).await.expect("set");

    assert_eq!(
        cmd.execute("pause", &ctx).await.expect("pause").reply,
        "Goal paused."
    );
    let show = cmd.execute("", &ctx).await.expect("show");
    assert!(show.reply.contains("Status: paused"));

    assert_eq!(
        cmd.execute("resume", &ctx).await.expect("resume").reply,
        "Goal resumed."
    );
    assert_eq!(
        cmd.execute("clear", &ctx).await.expect("clear").reply,
        "Goal cleared."
    );
    let show = cmd.execute("", &ctx).await.expect("show");
    assert_eq!(show.reply, "No goal set for this thread.");
}

#[tokio::test]
async fn control_verbs_without_goal_report_nothing_to_do() {
    let cmd = command();
    let sink = Arc::new(RecordingSink::default());
    let ctx = ctx(SessionId::new(), sink as Arc<dyn ChannelSink>);
    assert_eq!(
        cmd.execute("pause", &ctx).await.expect("pause").reply,
        "No goal to pause."
    );
    assert_eq!(
        cmd.execute("resume", &ctx).await.expect("resume").reply,
        "No goal to resume."
    );
    assert_eq!(
        cmd.execute("clear", &ctx).await.expect("clear").reply,
        "No goal to clear."
    );
}

#[tokio::test]
async fn policy_action_maps_show_to_get_and_mutations_to_manage() {
    let cmd = command();
    let sink = Arc::new(RecordingSink::default());
    let ctx = ctx(SessionId::new(), sink as Arc<dyn ChannelSink>);
    let owner = Some(Owner::new("alice"));

    assert_eq!(
        cmd.policy_action("", &ctx).await.expect("show action"),
        Action::GoalGet {
            owner: owner.clone()
        }
    );
    for input in ["set the objective", "pause", "resume", "clear"] {
        assert_eq!(
            cmd.policy_action(input, &ctx).await.expect("manage action"),
            Action::GoalManage {
                owner: owner.clone()
            }
        );
    }
}

#[tokio::test]
async fn invalid_objective_is_rejected_safely() {
    let cmd = command();
    let sink = Arc::new(RecordingSink::default());
    let ctx = ctx(SessionId::new(), sink as Arc<dyn ChannelSink>);
    let too_long = "x".repeat(5000);
    let err = cmd.execute(&too_long, &ctx).await.expect_err("too long");
    assert!(matches!(err, CommandError::InvalidArgs(_)));
    assert_eq!(err.safe_reply(), "command rejected");
}

#[tokio::test]
async fn set_replaces_objective_and_reactivates() {
    let cmd = command();
    let session = SessionId::new();
    let sink = Arc::new(RecordingSink::default());
    let ctx = ctx(session.clone(), sink as Arc<dyn ChannelSink>);
    cmd.execute("first", &ctx).await.expect("set first");
    cmd.execute("pause", &ctx).await.expect("pause");
    cmd.execute("second", &ctx).await.expect("set second");
    let goal = cmd.runtime.get(&session).await.expect("get").expect("goal");
    assert_eq!(goal.objective, "second");
    assert_eq!(goal.status, GoalStatus::Active);
}
