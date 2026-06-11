use super::*;
use crabgent_core::{AllowAllPolicy, ContentBlock, Kernel, ProviderError, RunRequest};
use crabgent_store::Page;
use crabgent_store::memory::MemorySessionStore;
use crabgent_test_support::{StubProvider, user_msg as user};

mod fan_out;

fn ctx_for(subject: &str) -> RunCtx {
    RunCtx::new(RunId::new(), Subject::new(subject))
}

fn channel_outbound(conv: &str, message_id: &str, body: &str) -> Message {
    Message::ChannelOutbound {
        conv: Owner::new(conv),
        body: body.to_owned(),
        channel: "slack".to_owned(),
        message_id: message_id.to_owned(),
        thread_root: None,
        broadcast: false,
    }
}

async fn load_session(store: &MemorySessionStore, id: &crabgent_store::SessionId) -> Session {
    store
        .load(id)
        .await
        .expect("load session")
        .expect("session exists")
}

#[tokio::test]
async fn on_session_start_caches_session_for_run() {
    let store = Arc::new(MemorySessionStore::default());
    let hook = SessionPersistHook::new(Arc::clone(&store));
    let ctx = ctx_for("u1");
    let dec = hook.on_session_start(&ctx).await;
    assert!(matches!(dec, Decision::Continue));
    let state = hook.state.lock().await;
    assert!(state.contains_key(&ctx.run_id));
    assert_eq!(state[&ctx.run_id].owner, Owner::new("u1"));
}

#[tokio::test]
async fn provider_error_clears_session_state() {
    let store = Arc::new(MemorySessionStore::default());
    let hook = SessionPersistHook::new(Arc::clone(&store));
    let observer = hook.clone();
    let kernel = Kernel::builder()
        .provider(
            StubProvider::new().fail_with(|| ProviderError::Other("provider exploded".into())),
        )
        .policy(AllowAllPolicy)
        .add_hook(hook)
        .build();

    let err = kernel
        .run(
            RunRequest {
                pause: None,
                run_id: RunId::new(),
                subject: Subject::new("u-error"),
                model: "m".into(),
                explicit_model: None,
                session_model_override: None,
                fallbacks: Vec::new(),
                messages: vec![user("trigger failure")],
                system_prompt: None,
                max_turns: Some(5),
                temperature: None,
                max_tokens: None,
                cancel_reason: None,
                reasoning_effort: None,
                web_search: crabgent_core::types::WebSearchConfig::default(),
            },
            None,
        )
        .await
        .expect_err("provider must fail");

    assert!(matches!(err, crabgent_core::KernelError::Provider(_)));
    assert!(observer.state.lock().await.is_empty());

    let sessions = store
        .list(&Owner::new("u-error"), Page::first(1))
        .await
        .expect("list sessions");
    assert_eq!(sessions.len(), 1);
    let stored = load_session(&store, &sessions[0].id).await;
    assert_eq!(stored.messages.len(), 1);
    assert!(
        matches!(stored.messages[0], Message::User { .. }),
        "errored runs persist accepted user input but no provider-side mutations"
    );
}

#[tokio::test]
async fn on_stop_persists_messages_to_store_for_completed_run() {
    let store = Arc::new(MemorySessionStore::default());
    let hook = SessionPersistHook::new(Arc::clone(&store));
    let ctx = ctx_for("u2");
    hook.on_session_start(&ctx).await;
    let msgs = vec![
        user("hi"),
        Message::Assistant {
            text: "done".to_owned(),
            tool_calls: Vec::new(),
        },
    ];
    hook.on_message(&msgs, &ctx).await;
    let session_id = {
        let state = hook.state.lock().await;
        state[&ctx.run_id].id.clone()
    };
    let stored_before_stop = load_session(&store, &session_id).await;
    assert!(stored_before_stop.messages.is_empty());
    hook.on_stop(&ctx, &Outcome::Completed("done".into())).await;
    let stored = load_session(&store, &session_id).await;
    assert_eq!(stored.messages.len(), 2);
}

#[tokio::test]
async fn errored_run_persists_only_safe_user_prefix() {
    let store = Arc::new(MemorySessionStore::default());
    let hook = SessionPersistHook::new(Arc::clone(&store));
    let ctx = ctx_for("u-error-prefix");
    hook.on_session_start(&ctx).await;
    let session_id = {
        let state = hook.state.lock().await;
        state[&ctx.run_id].id.clone()
    };
    hook.on_message(&[user("accepted")], &ctx).await;
    hook.on_message(
        &[
            user("accepted"),
            Message::Assistant {
                text: "unsafe partial".to_owned(),
                tool_calls: Vec::new(),
            },
        ],
        &ctx,
    )
    .await;

    hook.on_stop(&ctx, &Outcome::Errored("provider failed".into()))
        .await;

    let stored = load_session(&store, &session_id).await;
    assert_eq!(stored.messages.len(), 1);
    assert!(matches!(stored.messages[0], Message::User { .. }));
}

#[tokio::test]
async fn errored_run_persists_user_input_when_only_snapshot_ends_unsafe() {
    // Regression for the brand-new-session drop: when the only `on_message`
    // of a fresh run already carries the assistant tail, `record_messages`
    // never refreshes the safe snapshot (it still holds the empty
    // `on_session_start` seed). The errored `on_stop` must still persist the
    // accepted user prefix recovered from the latest log, not the empty seed.
    let store = Arc::new(MemorySessionStore::default());
    let hook = SessionPersistHook::new(Arc::clone(&store));
    let ctx = ctx_for("u-drop");
    hook.on_session_start(&ctx).await;
    let session_id = {
        let state = hook.state.lock().await;
        state[&ctx.run_id].id.clone()
    };
    // Single on_message snapshot ending in an assistant message: never safe,
    // so the snapshot map keeps the empty seed from on_session_start.
    hook.on_message(
        &[
            user("first input"),
            Message::Assistant {
                text: "partial".to_owned(),
                tool_calls: Vec::new(),
            },
        ],
        &ctx,
    )
    .await;

    hook.on_stop(&ctx, &Outcome::Errored("provider failed".into()))
        .await;

    let stored = load_session(&store, &session_id).await;
    assert_eq!(stored.messages.len(), 1, "user input must survive");
    assert!(matches!(stored.messages[0], Message::User { .. }));
}

#[tokio::test]
async fn cancelled_run_persists_only_safe_user_prefix_no_fan_out() {
    // Mirror of `errored_run_persists_only_safe_user_prefix` for the
    // `Outcome::Cancelled` branch: the safe user prefix persists, the partial
    // assistant tail is dropped, and no foreign-conv fan-out happens.
    let store = Arc::new(MemorySessionStore::default());
    let hook = SessionPersistHook::new(Arc::clone(&store));
    let ctx = ctx_for("u-cancelled-prefix");
    hook.on_session_start(&ctx).await;
    let session_id = {
        let state = hook.state.lock().await;
        state[&ctx.run_id].id.clone()
    };
    hook.on_message(&[user("accepted")], &ctx).await;
    hook.on_message(
        &[
            user("accepted"),
            // A foreign-conv outbound would fan out on a completed run; the
            // cancelled branch must not append it to any recipient session.
            channel_outbound("other-conv", "m-cancel", "leaked?"),
            Message::Assistant {
                text: "unsafe partial".to_owned(),
                tool_calls: Vec::new(),
            },
        ],
        &ctx,
    )
    .await;

    hook.on_stop(&ctx, &Outcome::Cancelled).await;

    let stored = load_session(&store, &session_id).await;
    assert_eq!(stored.messages.len(), 1);
    assert!(matches!(stored.messages[0], Message::User { .. }));

    // No fan-out: the foreign conv never gets a session row from this run.
    let foreign = store
        .list(&Owner::new("other-conv"), Page::first(4))
        .await
        .expect("list foreign sessions");
    assert!(
        foreign.is_empty(),
        "cancelled runs must not fan out foreign outbounds"
    );
}

#[tokio::test]
async fn on_message_bumps_updated_at() {
    let store = Arc::new(MemorySessionStore::default());
    let hook = SessionPersistHook::new(Arc::clone(&store));
    let ctx = ctx_for("u3");
    hook.on_session_start(&ctx).await;
    let before = {
        let state = hook.state.lock().await;
        state[&ctx.run_id].updated_at
    };
    tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    hook.on_message(&[user("x")], &ctx).await;
    let after = {
        let state = hook.state.lock().await;
        state[&ctx.run_id].updated_at
    };
    assert!(after >= before);
}

#[tokio::test]
async fn on_message_without_prior_start_is_noop() {
    let store = Arc::new(MemorySessionStore::default());
    let hook = SessionPersistHook::new(Arc::clone(&store));
    let ctx = ctx_for("u4");
    hook.on_message(&[user("orphan")], &ctx).await;
    let state = hook.state.lock().await;
    assert!(state.is_empty());
}

#[tokio::test]
async fn on_stop_drops_cached_session() {
    let store = Arc::new(MemorySessionStore::default());
    let hook = SessionPersistHook::new(Arc::clone(&store));
    let ctx = ctx_for("u5");
    hook.on_session_start(&ctx).await;
    hook.on_stop(&ctx, &Outcome::Completed("done".into())).await;
    let state = hook.state.lock().await;
    assert!(!state.contains_key(&ctx.run_id));
}

#[tokio::test]
async fn custom_owner_resolver_overrides_default() {
    let store = Arc::new(MemorySessionStore::default());
    let hook = SessionPersistHook::new(Arc::clone(&store))
        .with_owner_resolver(|_| Owner::new("constant-owner"));
    let ctx = ctx_for("subject-id-ignored");
    hook.on_session_start(&ctx).await;
    let state = hook.state.lock().await;
    assert_eq!(state[&ctx.run_id].owner, Owner::new("constant-owner"));
}

#[tokio::test]
async fn custom_thread_resolver_routes_to_distinct_session() {
    let store = Arc::new(MemorySessionStore::default());
    let thread = ThreadId::new("topic-7");
    let hook = SessionPersistHook::new(Arc::clone(&store)).with_thread_resolver({
        let t = thread.clone();
        move |_| Some(t.clone())
    });
    let ctx = ctx_for("u6");
    hook.on_session_start(&ctx).await;
    let session_id = {
        let state = hook.state.lock().await;
        state[&ctx.run_id].id.clone()
    };
    let stored = load_session(&store, &session_id).await;
    assert_eq!(stored.thread.as_ref(), Some(&thread));
}

#[tokio::test]
async fn distinct_runs_resolve_to_same_session_for_same_owner() {
    let store = Arc::new(MemorySessionStore::default());
    let hook = SessionPersistHook::new(Arc::clone(&store));
    let ctx_a = ctx_for("u7");
    let ctx_b = ctx_for("u7");
    hook.on_session_start(&ctx_a).await;
    hook.on_session_start(&ctx_b).await;
    let state = hook.state.lock().await;
    assert_eq!(state[&ctx_a.run_id].id, state[&ctx_b.run_id].id);
}

#[tokio::test]
async fn on_user_prompt_submit_no_cached_session_no_op() {
    let store = Arc::new(MemorySessionStore::default());
    let hook = SessionPersistHook::new(Arc::clone(&store));
    let ctx = ctx_for("u11");
    let dec = hook.on_user_prompt_submit(&[user("hi")], &ctx).await;
    assert!(matches!(dec, Decision::Continue));
}

#[tokio::test]
async fn on_user_prompt_submit_empty_persisted_history_no_op() {
    let store = Arc::new(MemorySessionStore::default());
    let hook = SessionPersistHook::new(Arc::clone(&store));
    let ctx = ctx_for("u12");
    hook.on_session_start(&ctx).await;
    // session exists but messages is empty (no on_message called yet)
    {
        let mut state = hook.state.lock().await;
        state
            .get_mut(&ctx.run_id)
            .expect("test result")
            .messages
            .clear();
    }
    let dec = hook.on_user_prompt_submit(&[user("hi")], &ctx).await;
    assert!(matches!(dec, Decision::Continue));
}

#[tokio::test]
async fn on_user_prompt_submit_prepends_persisted_history() {
    let store = Arc::new(MemorySessionStore::default());
    let hook = SessionPersistHook::new(Arc::clone(&store));
    let ctx = ctx_for("u13");
    hook.on_session_start(&ctx).await;
    hook.on_message(&[user("old1"), user("old2")], &ctx).await;
    let new_msgs = vec![user("new")];
    let dec = hook.on_user_prompt_submit(&new_msgs, &ctx).await;
    match dec {
        Decision::Replace(combined) => {
            assert_eq!(combined.len(), 3);
            assert!(
                matches!(&combined[0], Message::User { content, ..} if content[0] == ContentBlock::Text { text: "old1".into() })
            );
            assert!(
                matches!(&combined[1], Message::User { content, ..} if content[0] == ContentBlock::Text { text: "old2".into() })
            );
            assert!(
                matches!(&combined[2], Message::User { content, ..} if content[0] == ContentBlock::Text { text: "new".into() })
            );
        }
        other => panic!("expected Replace, got {other:?}"),
    }
}

#[tokio::test]
async fn second_run_appends_to_existing_session() {
    let store = Arc::new(MemorySessionStore::default());
    let hook = SessionPersistHook::new(Arc::clone(&store));
    let ctx_a = ctx_for("u8");
    hook.on_session_start(&ctx_a).await;
    hook.on_message(&[user("first")], &ctx_a).await;
    hook.on_stop(&ctx_a, &Outcome::Completed("done".into()))
        .await;

    let ctx_b = ctx_for("u8");
    hook.on_session_start(&ctx_b).await;
    let session_id = {
        let state = hook.state.lock().await;
        state[&ctx_b.run_id].id.clone()
    };
    let stored = load_session(&store, &session_id).await;
    assert_eq!(stored.messages.len(), 1);
    hook.on_message(&[user("first"), user("second")], &ctx_b)
        .await;
    let stored = load_session(&store, &session_id).await;
    assert_eq!(stored.messages.len(), 2);
}

#[tokio::test]
async fn store_error_warns_but_does_not_block_run() {
    // Failing-store path is exercised via the noop branch above.
    let store = Arc::new(MemorySessionStore::default());
    let hook = SessionPersistHook::new(Arc::clone(&store));
    let ctx = ctx_for("u9");
    let dec = hook.on_session_start(&ctx).await;
    assert!(matches!(dec, Decision::Continue));
    let dec = hook.on_message(&[user("x")], &ctx).await;
    assert!(matches!(dec, Decision::Continue));
}

#[tokio::test]
async fn distinct_owners_get_distinct_sessions() {
    let store = Arc::new(MemorySessionStore::default());
    let hook = SessionPersistHook::new(Arc::clone(&store));
    let ctx_a = ctx_for("alice");
    let ctx_b = ctx_for("bob");
    hook.on_session_start(&ctx_a).await;
    hook.on_session_start(&ctx_b).await;
    let state = hook.state.lock().await;
    assert_ne!(state[&ctx_a.run_id].id, state[&ctx_b.run_id].id);
}

#[tokio::test]
async fn session_scoping_separates_agents() {
    // Regression for the cron-context collision bug: four agents running
    // on the same matrix bot owner with `thread = NULL` previously
    // resolved to the same session row (lookup keyed only by
    // `(owner, thread)`). Distinct `scope_agent` attrs must now produce
    // distinct sessions, and re-running the same agent against the same
    // owner/thread/scope tuple must reuse the existing one.
    let store = Arc::new(MemorySessionStore::default());
    let hook = SessionPersistHook::new(Arc::clone(&store))
        .with_owner_resolver(|_| Owner::new("matrix:@alice%3Amatrix"));

    let make_ctx = |agent: &str| {
        let subject = Subject::new("matrix:@alice%3Amatrix")
            .with_attr("channel", "matrix")
            .with_attr("channel_kind", "direct")
            .with_attr("agent", agent);
        RunCtx::new(RunId::new(), subject)
    };

    let ctx_agent_alpha = make_ctx("agent_alpha");
    let ctx_agent_beta = make_ctx("agent_beta");
    let ctx_worker = make_ctx("worker");
    hook.on_session_start(&ctx_agent_alpha).await;
    hook.on_session_start(&ctx_agent_beta).await;
    hook.on_session_start(&ctx_worker).await;

    let (sid_agent_alpha, sid_agent_beta, sid_worker) = {
        let state = hook.state.lock().await;
        (
            state[&ctx_agent_alpha.run_id].id.clone(),
            state[&ctx_agent_beta.run_id].id.clone(),
            state[&ctx_worker.run_id].id.clone(),
        )
    };
    assert_ne!(sid_agent_alpha, sid_agent_beta);
    assert_ne!(sid_agent_alpha, sid_worker);
    assert_ne!(sid_agent_beta, sid_worker);

    // Re-running the agent_alpha cron on the same owner/thread/scope
    // resolves back to the original agent_alpha session.
    let ctx_agent_alpha_again = make_ctx("agent_alpha");
    hook.on_session_start(&ctx_agent_alpha_again).await;
    let sid_agent_alpha_again = {
        let state = hook.state.lock().await;
        state[&ctx_agent_alpha_again.run_id].id.clone()
    };
    assert_eq!(sid_agent_alpha, sid_agent_alpha_again);
}

#[tokio::test]
async fn on_message_replaces_full_log_each_call() {
    let store = Arc::new(MemorySessionStore::default());
    let hook = SessionPersistHook::new(Arc::clone(&store));
    let ctx = ctx_for("u10");
    hook.on_session_start(&ctx).await;
    hook.on_message(&[user("a")], &ctx).await;
    hook.on_message(&[user("a"), user("b")], &ctx).await;
    hook.on_message(&[user("c")], &ctx).await;
    let session_id = {
        let state = hook.state.lock().await;
        state[&ctx.run_id].id.clone()
    };
    hook.on_stop(&ctx, &Outcome::Completed("done".into())).await;
    let stored = load_session(&store, &session_id).await;
    assert_eq!(stored.messages.len(), 1);
}
