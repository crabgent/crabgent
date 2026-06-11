//! Unit tests for `RunCtx`, `Outcome`, and `CancelReason`.

use super::*;

#[test]
fn run_ctx_new_carries_fields() {
    let id = RunId::new();
    let ctx = RunCtx::new(id.clone(), Subject::new("u"));
    assert_eq!(ctx.run_id, id);
    assert_eq!(ctx.subject.id(), "u");
    assert!(ctx.session_id().is_none());
}

#[test]
fn run_ctx_session_id_is_clone_shared() {
    let original = RunCtx::new(RunId::new(), Subject::new("u"));
    let clone = original.clone();
    original
        .set_session_id("sess-1")
        .expect("first set succeeds");

    assert_eq!(clone.session_id(), Some("sess-1"));
    assert_eq!(original.session_id(), Some("sess-1"));
}

#[test]
fn run_ctx_set_session_id_is_write_once() {
    let ctx = RunCtx::new(RunId::new(), Subject::new("u"));
    ctx.set_session_id("first").expect("first set succeeds");

    let err = ctx
        .set_session_id("second")
        .expect_err("second set is rejected");
    assert_eq!(err, "second");
    assert_eq!(ctx.session_id(), Some("first"));
}

#[test]
fn run_ctx_session_model_override_is_clone_shared_and_write_once() {
    let original = RunCtx::new(RunId::new(), Subject::new("u"));
    let clone = original.clone();
    original
        .set_session_model_override(ModelId::new("session-model"))
        .expect("first set succeeds");

    assert_eq!(
        clone.session_model_override().map(ModelId::as_str),
        Some("session-model")
    );
    assert_eq!(
        original.session_model_override().map(ModelId::as_str),
        Some("session-model")
    );

    let rejected = original
        .set_session_model_override(ModelId::new("other"))
        .expect_err("second set is rejected");
    assert_eq!(rejected.as_str(), "other");
    assert_eq!(
        original.session_model_override().map(ModelId::as_str),
        Some("session-model")
    );
}

#[test]
fn run_ctx_session_reasoning_effort_override_is_clone_shared_and_write_once() {
    let original = RunCtx::new(RunId::new(), Subject::new("u"));
    let clone = original.clone();
    original
        .set_session_reasoning_effort_override(ReasoningEffort::High)
        .expect("first set succeeds");

    assert_eq!(
        clone.session_reasoning_effort_override(),
        Some(ReasoningEffort::High)
    );
    assert_eq!(
        original.session_reasoning_effort_override(),
        Some(ReasoningEffort::High)
    );

    let rejected = original
        .set_session_reasoning_effort_override(ReasoningEffort::Low)
        .expect_err("second set is rejected");
    assert_eq!(rejected, ReasoningEffort::Low);
    assert_eq!(
        original.session_reasoning_effort_override(),
        Some(ReasoningEffort::High)
    );
}

#[test]
fn outcome_round_trips_via_json() {
    let cases = [
        Outcome::Completed("done".into()),
        Outcome::MaxTurnsExceeded,
        Outcome::Cancelled,
        Outcome::Paused,
        Outcome::Errored("boom".into()),
    ];
    for o in cases {
        let s = serde_json::to_string(&o).expect("ser");
        let back: Outcome = serde_json::from_str(&s).expect("de");
        // Outcome doesn't impl PartialEq; check via shape match.
        match (&o, &back) {
            (Outcome::Completed(a), Outcome::Completed(b))
            | (Outcome::Errored(a), Outcome::Errored(b)) => assert_eq!(a, b),
            (Outcome::MaxTurnsExceeded, Outcome::MaxTurnsExceeded)
            | (Outcome::Cancelled, Outcome::Cancelled)
            | (Outcome::Paused, Outcome::Paused) => {}
            other => panic!("mismatched roundtrip: {other:?}"),
        }
    }
    assert_eq!(
        serde_json::to_string(&Outcome::Paused).expect("ser"),
        "{\"kind\":\"paused\"}"
    );
}

#[test]
fn run_ctx_new_carries_default_cancel_token() {
    let ctx = RunCtx::new(RunId::new(), Subject::new("u"));
    assert!(!ctx.cancel.is_cancelled());
    assert!(ctx.cancel_reason().is_none());
}

#[test]
fn run_ctx_with_cancel_replaces_token() {
    let installed = CancellationToken::new();
    let ctx = RunCtx::new(RunId::new(), Subject::new("u")).with_cancel(installed.clone());
    installed.cancel();
    assert!(ctx.cancel.is_cancelled());
}

#[test]
fn run_ctx_cancel_clone_shared() {
    let original = RunCtx::new(RunId::new(), Subject::new("u"));
    let clone = original.clone();
    original.cancel.cancel();
    assert!(clone.cancel.is_cancelled());
}

#[test]
fn run_ctx_cancel_reason_write_once() {
    let ctx = RunCtx::new(RunId::new(), Subject::new("u"));
    ctx.set_cancel_reason(CancelReason::Hook)
        .expect("first set succeeds");
    let rejected = ctx
        .set_cancel_reason(CancelReason::StopPattern)
        .expect_err("second set is rejected");
    assert_eq!(rejected, CancelReason::StopPattern);
    assert_eq!(ctx.cancel_reason(), Some(CancelReason::Hook));
}

#[test]
fn run_ctx_cancel_reason_clone_shared() {
    let installed: Arc<OnceLock<CancelReason>> = Arc::new(OnceLock::new());
    let original =
        RunCtx::new(RunId::new(), Subject::new("u")).with_cancel_reason(Arc::clone(&installed));
    let clone = original.clone();
    installed
        .set(CancelReason::StopPattern)
        .expect("first set succeeds");
    assert_eq!(original.cancel_reason(), Some(CancelReason::StopPattern));
    assert_eq!(clone.cancel_reason(), Some(CancelReason::StopPattern));
}

#[test]
fn cancel_reason_round_trips_via_json() {
    for r in [
        CancelReason::StopPattern,
        CancelReason::Hook,
        CancelReason::External,
        CancelReason::Shutdown,
    ] {
        let s = serde_json::to_string(&r).expect("ser");
        let back: CancelReason = serde_json::from_str(&s).expect("de");
        assert_eq!(r, back);
    }
    assert_eq!(
        serde_json::to_string(&CancelReason::StopPattern).expect("ser"),
        "\"stop_pattern\""
    );
}
