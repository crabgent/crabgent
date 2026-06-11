use super::*;
use crate::envelope::MessageRef;
use crate::inbox_lifecycle::ClaimResult;
use crate::participant::{Participant, ParticipantRole};
use crate::subject::{ChannelSubjectExt, attr_keys};
use chrono::Utc;
use crabgent_core::error::ProviderError;
use crabgent_core::owner::Owner;
use crabgent_core::policy::{AllowAllPolicy, DenyAllPolicy};
use crabgent_core::provider::{Provider, ProviderCapabilities};
use crabgent_core::types::{LlmRequest, LlmResponse, StopReason, Usage};
use crabgent_core::{ImagePayload, Kernel, RunCtx};
use crabgent_hook_inject::InjectionRegistry;
use std::sync::Mutex;
use tokio_util::sync::CancellationToken;

struct StubProvider {
    seen: Arc<Mutex<Vec<String>>>,
}

mod kind_tests;

#[async_trait]
impl Provider for StubProvider {
    async fn complete(
        &self,
        req: &LlmRequest,
        _ctx: &RunCtx,
        _cancel: Option<&CancellationToken>,
    ) -> Result<LlmResponse, ProviderError> {
        self.seen
            .lock()
            .expect("test result")
            .push(req.model.as_str().to_owned());
        Ok(LlmResponse {
            text: "ok".into(),
            tool_calls: vec![],
            stop_reason: StopReason::EndTurn,
            usage: Usage::default(),
            model: req.model.clone(),
        })
    }
    fn name(&self) -> &'static str {
        "stub"
    }
    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities::default()
    }
    fn models(&self) -> Vec<crabgent_core::ModelInfo> {
        vec![crabgent_core::ModelInfo::minimal(
            "claude-haiku-4-5",
            "stub",
        )]
    }
}

fn build_event(channel: &str, conv: &str, role: ParticipantRole, body: &str) -> InboundEvent {
    InboundEvent {
        channel: channel.to_owned(),
        conv: Owner::new(conv),
        kind: None,
        from: Participant::new("U1", role),
        message: MessageRef::top_level(channel, Owner::new(conv), "ts:1"),
        body: body.to_owned(),
        attachments: vec![],
        timestamp: Utc::now(),
    }
}

pub(super) fn build_kernel(seen: Arc<Mutex<Vec<String>>>) -> Arc<Kernel> {
    Arc::new(
        Kernel::builder()
            .provider(StubProvider { seen })
            .policy(AllowAllPolicy)
            .build(),
    )
}

fn allow_inbox(model: impl Into<ModelId>) -> KernelChannelInbox {
    KernelChannelInbox::new(
        build_kernel(Arc::new(Mutex::new(Vec::new()))),
        model,
        Arc::new(AllowAllPolicy),
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn receive_spawns_kernel_run() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let kernel = build_kernel(Arc::clone(&seen));
    let inbox = KernelChannelInbox::new(kernel, "claude-haiku-4-5", Arc::new(AllowAllPolicy));
    let ev = build_event("slack", "slack:T1/D1", ParticipantRole::Human, "hi");
    inbox.receive(ev).await.expect("receive ok");
    // The run is spawned; wait briefly so the stub provider records.
    tokio::time::sleep(Duration::from_millis(120)).await;
    let entries = seen.lock().expect("mutex should not be poisoned");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0], "claude-haiku-4-5");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn receive_denies_before_kernel_run_spawn() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let kernel = build_kernel(Arc::clone(&seen));
    let inbox = KernelChannelInbox::new(kernel, "claude-haiku-4-5", Arc::new(DenyAllPolicy));
    let ev = build_event("slack", "slack:T1/D1", ParticipantRole::Human, "hi");
    let err = inbox.receive(ev).await.expect_err("policy deny");
    assert!(matches!(err, ChannelError::PolicyDenied { .. }));
    tokio::time::sleep(Duration::from_millis(120)).await;
    assert!(
        seen.lock()
            .expect("mutex should not be poisoned")
            .is_empty()
    );
}

#[test]
fn build_request_sets_subject_id_from_event() {
    let inbox = allow_inbox("model");
    let ev = build_event("slack", "slack:T1/D1", ParticipantRole::Human, "hi");
    let req = inbox.build_request(&ev).expect("valid subject");
    assert_eq!(req.subject.id(), "slack:U1");
    assert_eq!(req.subject.attr("channel"), Some("slack"));
    assert_eq!(req.subject.attr("conv"), Some("slack:T1/D1"));
    assert_eq!(req.subject.attr("participant_role"), Some("human"));
    assert!(req.subject.attr("channel_kind").is_none());
    assert_eq!(req.model.as_str(), "model");
    assert_eq!(req.messages.len(), 1);
    match &req.messages[0] {
        Message::User { content, .. } => match &content[0] {
            ContentBlock::Text { text } => {
                assert_eq!(
                    text,
                    "<inbound source=\"unknown\" channel=\"slack\">hi</inbound>"
                );
            }
            other => panic!("unexpected: {other:?}"),
        },
        other => panic!("unexpected: {other:?}"),
    }
}

#[test]
fn default_subject_resolver_stamps_inbound_message_ref() {
    let mut ev = build_event("slack", "slack:T1/D1", ParticipantRole::Human, "hi");
    ev.message = MessageRef::thread_reply_broadcast(
        "slack",
        Owner::new("slack:T1/D1"),
        "ts:2",
        "ts:1",
        true,
    );
    let resolver = default_subject_resolver(Some(ChannelKind::Group));
    let subject = resolver(&ev).expect("valid subject");
    assert_eq!(subject.attr(attr_keys::INBOUND_MSG_ID), Some("ts:2"));
    assert_eq!(
        subject.attr(attr_keys::INBOUND_MSG_THREAD_ROOT),
        Some("ts:1")
    );
    assert_eq!(subject.attr(attr_keys::INBOUND_MSG_BROADCAST), Some("true"));
    assert_eq!(subject.inbound_message_ref(), Some(ev.message));
}

#[test]
fn with_fallbacks_propagates_to_run_request() {
    let inbox = allow_inbox("primary").with_fallbacks(vec![
        ModelTarget::id(ModelId::new("backup-a")),
        ModelTarget::id(ModelId::new("backup-b")),
    ]);
    let ev = build_event("slack", "slack:T1/D1", ParticipantRole::Human, "hi");
    let req = inbox.build_request(&ev).expect("valid subject");
    assert_eq!(req.fallbacks.len(), 2);
    assert_eq!(req.fallbacks[0].as_str(), "backup-a");
    assert_eq!(req.fallbacks[1].as_str(), "backup-b");
}

#[test]
fn default_run_request_has_no_fallbacks() {
    let inbox = allow_inbox("primary");
    let ev = build_event("slack", "slack:T1/D1", ParticipantRole::Human, "hi");
    let req = inbox.build_request(&ev).expect("valid subject");
    assert!(req.fallbacks.is_empty());
}

#[test]
fn custom_subject_resolver_replaces_default() {
    let inbox = allow_inbox("m")
        .with_subject_resolver(|_ev| Subject::new("custom-id").with_attr("custom", "yes"));
    let ev = build_event("slack", "slack:T1/D1", ParticipantRole::Bot, "hi");
    let req = inbox.build_request(&ev).expect("valid subject");
    assert_eq!(req.subject.id(), "custom-id");
    assert_eq!(req.subject.attr("custom"), Some("yes"));
    assert!(req.subject.attr("channel").is_none());
}

#[test]
fn fallible_subject_resolver_rejects_invalid_subject() {
    let inbox = allow_inbox("m").with_fallible_subject_resolver(|_ev| Subject::try_new(""));
    let ev = build_event("slack", "slack:T1/D1", ParticipantRole::Human, "hi");
    assert!(matches!(
        inbox.build_request(&ev),
        Err(ChannelError::InvalidSubject(_))
    ));
}

#[test]
fn default_subject_resolver_rejects_empty_channel_before_formatting() {
    let inbox = allow_inbox("m");
    let ev = build_event("", "slack:T1/D1", ParticipantRole::Human, "hi");
    assert!(matches!(
        inbox.build_request(&ev),
        Err(ChannelError::InvalidSubject(_))
    ));
}

#[test]
fn with_system_prompt_passes_through() {
    let inbox = allow_inbox("m").with_system_prompt("be terse");
    let ev = build_event("slack", "c", ParticipantRole::Human, "hi");
    let req = inbox.build_request(&ev).expect("valid subject");
    let p = req.system_prompt.as_deref().expect("system prompt present");
    let after = p
        .strip_prefix(PERSONA_BOUNDARY_PREFIX)
        .expect("persona prefix at head");
    assert!(after.starts_with("be terse"), "{after:?}");
    assert!(p.contains("Conversation context"), "{p:?}");
}

#[test]
fn with_max_turns_passes_through() {
    let inbox = allow_inbox("m").with_max_turns(3);
    let ev = build_event("slack", "c", ParticipantRole::Human, "hi");
    let req = inbox.build_request(&ev).expect("valid subject");
    assert_eq!(req.max_turns, Some(3));
}

#[test]
fn build_request_appends_conversation_hint_by_default() {
    let inbox = allow_inbox("m");
    let ev = build_event("slack", "slack:T1/D1", ParticipantRole::Human, "hi");
    let req = inbox.build_request(&ev).expect("ok");
    let p = req.system_prompt.as_deref().expect("hint default-on");
    let after = p
        .strip_prefix(PERSONA_BOUNDARY_PREFIX)
        .expect("persona prefix at head");
    assert!(after.starts_with("Conversation context"), "{after:?}");
    assert!(p.contains("\"slack\""), "{p:?}");
    assert!(p.contains("conv=\"slack:T1/D1\""), "{p:?}");
}

#[test]
fn without_conversation_hint_disables_append() {
    let inbox = allow_inbox("m")
        .with_system_prompt("base")
        .without_conversation_hint();
    let ev = build_event("slack", "slack:T1/D1", ParticipantRole::Human, "hi");
    let req = inbox.build_request(&ev).expect("ok");
    let p = req.system_prompt.as_deref().expect("base prompt present");
    assert_eq!(p, format!("{PERSONA_BOUNDARY_PREFIX}base"));
    assert!(!p.contains("Conversation context"), "{p:?}");
}

#[test]
fn empty_base_with_disabled_hint_keeps_persona_prefix() {
    // `Some("")` counts as no base in both branches. The composed
    // prompt still carries the prompt-injection persona boundary.
    let inbox = allow_inbox("m")
        .with_system_prompt("")
        .without_conversation_hint();
    let ev = build_event("slack", "slack:T1/D1", ParticipantRole::Human, "hi");
    let req = inbox.build_request(&ev).expect("ok");
    assert_eq!(req.system_prompt.as_deref(), Some(PERSONA_BOUNDARY_PREFIX));
}

#[test]
fn conversation_hint_composes_with_existing_system_prompt() {
    let inbox = allow_inbox("m").with_system_prompt("base prompt");
    let ev = build_event("slack", "slack:T1/D1", ParticipantRole::Human, "hi");
    let req = inbox.build_request(&ev).expect("ok");
    let p = req.system_prompt.as_deref().expect("present");
    let after = p
        .strip_prefix(PERSONA_BOUNDARY_PREFIX)
        .expect("persona prefix at head");
    assert!(
        after.starts_with("base prompt\n\nConversation context"),
        "{after:?}"
    );
}

#[test]
fn build_request_merges_attachments_into_user_message() {
    let inbox = allow_inbox("m");
    let mut ev = build_event("slack", "slack:T1/D1", ParticipantRole::Human, "describe");
    ev.attachments.push(ContentBlock::Image(
        ImagePayload::new(b"iVBOR".to_vec(), "image/png").expect("valid image payload"),
    ));
    let req = inbox.build_request(&ev).expect("valid subject");
    assert_eq!(req.messages.len(), 1);
    match &req.messages[0] {
        Message::User { content, .. } => {
            assert_eq!(content.len(), 2);
            assert!(matches!(
                &content[0],
                ContentBlock::Text { text } if text == "<inbound source=\"unknown\" channel=\"slack\">describe</inbound>"
            ));
            assert!(matches!(&content[1], ContentBlock::Image(_)));
        }
        other => panic!("unexpected: {other:?}"),
    }
}

#[test]
fn build_request_without_attachments_unmodified() {
    let inbox = allow_inbox("m");
    let ev = build_event("slack", "slack:T1/D1", ParticipantRole::Human, "hi");
    let req = inbox.build_request(&ev).expect("valid subject");
    match &req.messages[0] {
        Message::User { content, .. } => {
            assert_eq!(content.len(), 1);
            assert!(matches!(
                &content[0],
                ContentBlock::Text { text } if text == "<inbound source=\"unknown\" channel=\"slack\">hi</inbound>"
            ));
        }
        other => panic!("unexpected: {other:?}"),
    }
}

// ----- Mid-turn injection tests -----

/// M1 in-flight, M2 arrives for the same (channel, conv): M2 must inject
/// into the existing run's registry, not spawn a second run.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn second_event_same_conv_injects_not_spawns() {
    use crate::inbox_lifecycle::ConvKey;

    let seen = Arc::new(Mutex::new(Vec::new()));
    let kernel = build_kernel(Arc::clone(&seen));
    let registry = InjectionRegistry::new();
    let inbox = KernelChannelInbox::new(kernel, "claude-haiku-4-5", Arc::new(AllowAllPolicy))
        .with_inject_registry(registry.clone());

    // Manually claim the conv slot as if M1 is running, using a known RunId.
    let m1_run_id = RunId::new();
    let conv_key = ConvKey("slack".to_owned(), "slack:T1/D1".to_owned());
    let claim = inbox
        .lifecycle
        .try_claim_conv(conv_key.clone(), m1_run_id.clone())
        .await;
    assert!(matches!(claim, ClaimResult::Spawned { .. }));

    // M2 arrives for the same conv.
    let ev2 = build_event("slack", "slack:T1/D1", ParticipantRole::Human, "follow-up");
    inbox.receive(ev2).await.expect("receive ok");

    // Registry for M1's run_id should have exactly one pending entry.
    assert_eq!(registry.pending(&m1_run_id).await, 1);

    // Only M1's slot was claimed, the provider has not been called by M2.
    assert!(
        seen.lock()
            .expect("mutex should not be poisoned")
            .is_empty()
    );

    // Cleanup: release so subsequent tests don't bleed state.
    inbox.lifecycle.release_conv(&conv_key, &m1_run_id).await;
}

/// After M1 finishes and releases the conv, M3 arrives and must spawn a
/// new run rather than injecting.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn after_release_next_event_spawns_new_run() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let kernel = build_kernel(Arc::clone(&seen));
    let inbox = KernelChannelInbox::new(
        Arc::clone(&kernel),
        "claude-haiku-4-5",
        Arc::new(AllowAllPolicy),
    );

    // M1 goes through normally.
    let ev1 = build_event("slack", "slack:T1/D1", ParticipantRole::Human, "first");
    inbox.receive(ev1).await.expect("receive ok");

    // Wait long enough for the stub provider to run and release_conv to fire.
    tokio::time::sleep(Duration::from_millis(200)).await;

    // M3 arrives after M1 has finished. The conv slot is free.
    let ev3 = build_event("slack", "slack:T1/D1", ParticipantRole::Human, "third");
    inbox.receive(ev3).await.expect("receive ok");

    tokio::time::sleep(Duration::from_millis(200)).await;

    // Both M1 and M3 caused provider calls (two separate runs).
    let calls = seen.lock().expect("mutex should not be poisoned").len();
    assert_eq!(
        calls, 2,
        "expected two separate provider calls, got {calls}"
    );
}

/// Two distinct conv keys must both spawn independent runs without
/// cross-conv blocking.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn distinct_conv_keys_spawn_parallel_runs() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let kernel = build_kernel(Arc::clone(&seen));
    let inbox = KernelChannelInbox::new(kernel, "claude-haiku-4-5", Arc::new(AllowAllPolicy));

    let ev_a = build_event("slack", "slack:T1/Da", ParticipantRole::Human, "conv-a");
    let ev_b = build_event("slack", "slack:T1/Db", ParticipantRole::Human, "conv-b");

    inbox.receive(ev_a).await.expect("receive conv-a");
    inbox.receive(ev_b).await.expect("receive conv-b");

    tokio::time::sleep(Duration::from_millis(200)).await;

    let calls = seen.lock().expect("mutex should not be poisoned").len();
    assert_eq!(
        calls, 2,
        "both conv-a and conv-b should spawn runs, got {calls}"
    );
}

/// A policy-denied event must NOT claim a conv slot: the `active_runs` map
/// must remain empty after a deny.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn policy_denied_does_not_claim_conv_slot() {
    use crate::inbox_lifecycle::ConvKey;

    let kernel = build_kernel(Arc::new(Mutex::new(Vec::new())));
    let inbox = KernelChannelInbox::new(kernel, "claude-haiku-4-5", Arc::new(DenyAllPolicy));

    let ev = build_event("slack", "slack:T1/D1", ParticipantRole::Human, "blocked");
    let err = inbox.receive(ev).await.expect_err("policy deny");
    assert!(matches!(err, ChannelError::PolicyDenied { .. }));

    // The conv slot must be unclaimed: a subsequent claim on the same key must
    // return Spawned (not Existing).
    let key = ConvKey("slack".to_owned(), "slack:T1/D1".to_owned());
    let run_id = RunId::new();
    let claim = inbox
        .lifecycle
        .try_claim_conv(key.clone(), run_id.clone())
        .await;
    assert!(
        matches!(claim, ClaimResult::Spawned { .. }),
        "policy deny must not pollute active_runs map"
    );
    inbox.lifecycle.release_conv(&key, &run_id).await;
}

/// `with_inject_registry` and `inject_registry()` round-trip: the getter
/// returns the same shared state as the registry passed to the builder.
#[tokio::test]
async fn inject_registry_getter_returns_shared_registry() {
    let reg = InjectionRegistry::new();
    let run_id = RunId::new();
    reg.submit_user_text(&run_id, "seed").await;

    let inbox = allow_inbox("m").with_inject_registry(reg);
    assert_eq!(
        inbox.inject_registry().pending(&run_id).await,
        1,
        "getter must return the registry supplied via with_inject_registry"
    );
}

pub(super) fn build_reaction(
    channel: &str,
    conv: &str,
    emoji: &str,
    added: bool,
) -> crate::envelope::InboundReaction {
    crate::envelope::InboundReaction {
        channel: channel.to_owned(),
        conv: Owner::new(conv),
        from: Participant::new("U1", ParticipantRole::Human),
        parent: MessageRef::top_level(channel, Owner::new(conv), "ts:42"),
        emoji: emoji.to_owned(),
        added,
        timestamp: Utc::now(),
    }
}

mod conv_display;
mod inbound_wrap;
mod inject_parity;
mod live_turn;
mod live_turn_error;
mod live_turn_progress;
mod live_turn_support;
mod persona;
mod reaction;
