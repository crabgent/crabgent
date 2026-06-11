//! [`SessionPersistHook`] auto-persists conversation state across
//! `Kernel::run()` invocations. It is fail-soft: store errors are logged via
//! `crabgent_log::warn!` and the run continues with the in-memory log unchanged.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;
use crabgent_core::hook::{Decision, Hook, Outcome, RunCtx};
use crabgent_core::message::{ContentBlock, Message};
use crabgent_core::model::ModelId;
use crabgent_core::run_id::RunId;
use crabgent_core::{MemoryScope, Subject};
use crabgent_log::{debug, info, warn};
use crabgent_store::records::Session;
use crabgent_store::traits::SessionStore;
use crabgent_store::{Owner, ThreadId};
use tokio::sync::Mutex;

type OwnerResolver = Arc<dyn Fn(&Subject) -> Owner + Send + Sync>;
type ThreadResolver = Arc<dyn Fn(&RunCtx) -> Option<ThreadId> + Send + Sync>;

struct ForeignOutbound<'a> {
    conv: &'a Owner,
    message_id: &'a str,
    channel: &'a str,
    message: &'a Message,
}

/// Hook that auto-persists conversation messages into a [`SessionStore`].
///
/// Lifecycle:
///
/// 1. `on_session_start`: resolve `(Owner, Option<ThreadId>)` from the
///    [`RunCtx`] using the configured resolvers, then call
///    [`SessionStore::find_or_create`]. The resulting [`Session`] is
///    cached per [`RunId`] for the duration of the run.
/// 2. `on_message`: replace the cached `session.messages` with the kernel's
///    current log slice, bump `updated_at`, and immediately flush the latest
///    safe prefix that ends in system/user input.
/// 3. `on_stop`: for completed or max-turn runs, persist the cached
///    messages and fan out foreign-conv outbounds (see below), then drop
///    the per-run cache entry. Errored and cancelled runs persist only the
///    safe user/input prefix and do not fan out outbound records.
///
/// ## Customisation
///
/// - [`Self::with_owner_resolver`]: derives [`Owner`] from [`Subject`].
/// - [`Self::with_thread_resolver`]: derives optional [`ThreadId`] from [`RunCtx`].
///
/// ## `notify_user` fan-out
///
/// `on_stop` scans the finished log for [`Message::ChannelOutbound`]
/// entries whose `conv` differs from the running session's `owner` and
/// appends each into the session keyed by that conv. This makes a
/// `notify_user` outbound visible in the recipient DM session.
///
/// The discriminator (`conv != session.owner`) is correct only when
/// sessions are keyed by their conv `Owner`, i.e. when the deployment
/// installs a conv-keyed [`Self::with_owner_resolver`]. Under such a
/// resolver a `channel_send` into the running session has
/// `conv == owner` and is skipped (the normal flush already covers it).
/// Under the default subject-id resolver the fan-out cannot match the
/// inbound session key, so `notify_user` coherence requires the
/// conv-keyed resolver; channel deployments are expected to install one.
///
/// ## Failure handling
///
/// Store errors are warned and never fail the kernel run.
pub struct SessionPersistHook<S: SessionStore> {
    store: Arc<S>,
    resolve_owner: OwnerResolver,
    resolve_thread: ThreadResolver,
    // Lock-acquisition discipline for the two per-run maps below: no code
    // path ever holds both guards at the same time. Each site takes one
    // lock inside a narrow block, drops it, then takes the other. The two
    // guards therefore never nest, so the apparent acquisition-order
    // difference between sites cannot deadlock:
    //   * `on_session_start` inserts into `safe_messages` (temporary guard,
    //     dropped at the end of that statement) and then locks `state`.
    //   * `record_messages` reads/writes `state` in a block, drops it, then
    //     locks `safe_messages`.
    //   * `on_stop` locks `state`, drops it, then locks `safe_messages`.
    // Within a single `Kernel::run`, the hook-chain awaits the lifecycle
    // callbacks sequentially (one run never overlaps itself). Across two
    // concurrent runs sharing this `Arc`, each guard is a short critical
    // section over a `HashMap` keyed by the run-specific `RunId`, so there
    // is no nested-lock cycle to invert.
    state: Arc<Mutex<HashMap<RunId, Session>>>,
    safe_messages: Arc<Mutex<HashMap<RunId, Vec<Message>>>>,
}

impl<S: SessionStore> Clone for SessionPersistHook<S> {
    fn clone(&self) -> Self {
        Self {
            store: Arc::clone(&self.store),
            resolve_owner: Arc::clone(&self.resolve_owner),
            resolve_thread: Arc::clone(&self.resolve_thread),
            state: Arc::clone(&self.state),
            safe_messages: Arc::clone(&self.safe_messages),
        }
    }
}

impl<S: SessionStore> SessionPersistHook<S> {
    pub fn new(store: Arc<S>) -> Self {
        Self {
            store,
            resolve_owner: Arc::new(|s: &Subject| Owner::new(s.id())),
            resolve_thread: Arc::new(|_| None),
            state: Arc::new(Mutex::new(HashMap::new())),
            safe_messages: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    #[must_use]
    pub fn with_owner_resolver<F>(mut self, f: F) -> Self
    where
        F: Fn(&Subject) -> Owner + Send + Sync + 'static,
    {
        self.resolve_owner = Arc::new(f);
        self
    }

    #[must_use]
    pub fn with_thread_resolver<F>(mut self, f: F) -> Self
    where
        F: Fn(&RunCtx) -> Option<ThreadId> + Send + Sync + 'static,
    {
        self.resolve_thread = Arc::new(f);
        self
    }

    async fn open_session(&self, ctx: &RunCtx) -> Option<Session> {
        let owner = (self.resolve_owner)(&ctx.subject);
        let thread = (self.resolve_thread)(ctx);
        let mut scope = MemoryScope::from_subject(&ctx.subject);
        scope.owner = Some(owner.clone());
        match self
            .store
            .find_or_create(&owner, thread.as_ref(), &scope)
            .await
        {
            Ok(mut session) => {
                // Stamp the run-level scope onto the in-memory snapshot
                // so search and external consumers see the resolved
                // scope tuple even when the row pre-dated the scoped
                // lookup (legacy rows have NULL scope columns).
                session.scope = scope;
                Some(session)
            }
            Err(e) => {
                warn!(
                    run_id = %ctx.run_id,
                    error_kind = e.kind(),
                    transient = e.is_transient(),
                    "session-persist: find_or_create failed; skipping run"
                );
                None
            }
        }
    }

    async fn record_messages(&self, ctx: &RunCtx, msgs: &[Message]) {
        let safe_session = {
            let mut state = self.state.lock().await;
            let Some(session) = state.get_mut(&ctx.run_id) else {
                return;
            };
            session.messages = msgs.to_vec();
            session.updated_at = Utc::now();
            if is_error_safe_log(msgs) {
                Some(session.clone())
            } else {
                None
            }
        };

        if let Some(session) = safe_session {
            self.safe_messages
                .lock()
                .await
                .insert(ctx.run_id.clone(), session.messages.clone());
            self.save_session_messages(ctx, &session, &session.messages)
                .await;
        }
    }

    async fn save_session_messages(&self, ctx: &RunCtx, session: &Session, messages: &[Message]) {
        // Mutate only the two columns this hook owns. A full-row
        // `store.save(session)` would clobber sibling fields (most
        // critically `model_override`, set by `models.set_session`
        // mid-run) with this hook's pre-tool-call cached snapshot.
        if let Err(e) = self
            .store
            .save_messages(&session.id, messages, session.updated_at)
            .await
        {
            warn!(
                run_id = %ctx.run_id,
                session_id = %session.id,
                error_kind = e.kind(),
                transient = e.is_transient(),
                "session-persist: save failed; in-memory log unchanged"
            );
        }
    }

    /// Fan out `Message::ChannelOutbound` entries addressed at a conv
    /// other than the running session's own conv into that conv's
    /// session. This is how a `notify_user` outbound becomes visible in
    /// the recipient DM session: the agent runs in session X, but the
    /// message lands in session Y keyed by the DM conv.
    ///
    /// Assumes channel DM sessions are keyed by their conv `Owner`
    /// (the conv-keyed owner resolver). The fan-out is idempotent: an
    /// outbound already present (matched by `message_id`) is skipped.
    async fn fan_out_foreign_outbound(&self, ctx: &RunCtx, session: &Session) {
        log_fan_out_scan_start(ctx, session);
        for msg in &session.messages {
            let Some(outbound) = foreign_outbound(session, msg) else {
                continue;
            };
            log_fan_out_trigger(ctx, session, &outbound);
            self.append_outbound_to_conv(ctx, session, &outbound).await;
        }
    }

    async fn append_outbound_to_conv(
        &self,
        ctx: &RunCtx,
        origin: &Session,
        outbound: &ForeignOutbound<'_>,
    ) {
        // Inherit channel/agent/kind scope from the originating
        // session so each agent's fan-out lands in a distinct target
        // session, mirroring the scope separation enforced on the
        // primary `open_session` path. Without this, two cron agents
        // sending `notify_user` to the same user would collide on the
        // same target row. `scope.owner` and `scope.conv` are stamped
        // with the target conv so the recipient's later DM-run, which
        // opens a session with `scope.conv = recipient_conv`, finds
        // this fan-out row via `find_or_create` instead of creating a
        // duplicate. `scope.agent` stays from origin to keep distinct
        // rows per sender (agent_alpha vs agent_beta notifying same user).
        //
        // `scope.channel` is stamped from the outbound `Message::Channel
        // Outbound.channel` field rather than inherited from `origin.scope`.
        // Cron-origin runs carry `origin.scope.channel = None` (no inbound
        // channel context), so an inherited None would fail the recipient
        // DM lookup which queries with `scope.channel = Some("slack")` (or
        // whichever adapter delivers the DM). Stamping the delivery channel
        // here keys the fan-out row to the channel that will look it up.
        // For non-cron origins the outbound channel matches the origin
        // channel in practice; the override is harmless either way because
        // the recipient lookup always uses the delivery channel.
        let scope = fan_out_scope(origin, outbound);
        let Some(mut target) = self.load_fan_out_target(ctx, outbound.conv, &scope).await else {
            return;
        };
        if target_has_outbound(&target, outbound.message_id) {
            return;
        }
        append_fan_out_messages(&mut target, outbound.message);
        self.save_fan_out_target(ctx, &target).await;
    }

    async fn load_fan_out_target(
        &self,
        ctx: &RunCtx,
        conv: &Owner,
        scope: &MemoryScope,
    ) -> Option<Session> {
        match self.store.find_or_create(conv, None, scope).await {
            Ok(target) => Some(target),
            Err(e) => {
                warn!(
                    run_id = %ctx.run_id,
                    conv = %conv,
                    error_kind = e.kind(),
                    transient = e.is_transient(),
                    "session-persist: notify_user fan-out find_or_create failed"
                );
                None
            }
        }
    }

    async fn save_fan_out_target(&self, ctx: &RunCtx, target: &Session) {
        if let Err(e) = self
            .store
            .save_messages(&target.id, &target.messages, Utc::now())
            .await
        {
            warn!(
                run_id = %ctx.run_id,
                session_id = %target.id,
                error_kind = e.kind(),
                transient = e.is_transient(),
                "session-persist: notify_user fan-out save failed"
            );
        }
    }
}

fn log_fan_out_scan_start(ctx: &RunCtx, session: &Session) {
    info!(
        run_id = %ctx.run_id,
        session_id = %session.id,
        owner = %session.owner,
        message_count = session.messages.len(),
        "session-persist: fan-out scan starting"
    );
}

fn foreign_outbound<'a>(session: &Session, msg: &'a Message) -> Option<ForeignOutbound<'a>> {
    let Message::ChannelOutbound {
        conv,
        message_id,
        channel,
        ..
    } = msg
    else {
        return None;
    };
    if conv == &session.owner {
        return None;
    }
    Some(ForeignOutbound {
        conv,
        message_id,
        channel,
        message: msg,
    })
}

fn log_fan_out_trigger(ctx: &RunCtx, session: &Session, outbound: &ForeignOutbound<'_>) {
    info!(
        run_id = %ctx.run_id,
        conv = %outbound.conv,
        owner = %session.owner,
        message_id = %outbound.message_id,
        "session-persist: fan-out triggered for foreign outbound"
    );
}

fn fan_out_scope(origin: &Session, outbound: &ForeignOutbound<'_>) -> MemoryScope {
    let mut scope = origin.scope.clone();
    scope.owner = Some(outbound.conv.clone());
    scope.conv = Some(outbound.conv.as_str().to_owned());
    scope.channel = Some(outbound.channel.to_owned());
    scope
}

fn target_has_outbound(target: &Session, message_id: &str) -> bool {
    target
        .messages
        .iter()
        .any(|m| matches!(m, Message::ChannelOutbound { message_id: id, .. } if id == message_id))
}

fn append_fan_out_messages(target: &mut Session, outbound: &Message) {
    target.messages.push(outbound.clone());
    if let Some(record) = notify_user_record(outbound) {
        target.messages.push(record);
    }
}

// Provider transforms strip bare ChannelOutbound entries as audit metadata.
// The recipient session needs a user-framed record so the body survives.
fn notify_user_record(outbound: &Message) -> Option<Message> {
    let Message::ChannelOutbound { body, .. } = outbound else {
        return None;
    };
    Some(Message::User {
        content: vec![ContentBlock::Text {
            text: format!(
                "[notify_user record] You previously sent the following message into this conversation: {body}"
            ),
        }],
        timestamp: None,
    })
}

#[async_trait]
impl<S> Hook for SessionPersistHook<S>
where
    S: SessionStore + 'static,
{
    async fn on_session_start(&self, ctx: &RunCtx) -> Decision<()> {
        if let Some(session) = self.open_session(ctx).await {
            // Best-effort: surface the resolved session id to tools that operate on the current session.
            if ctx.set_session_id(session.id.to_string()).is_err() {
                debug!("run context already has a session id");
            }
            // Surface persisted per-session model override for the next turn.
            if let Some(model) = session.model_override.as_deref()
                && ctx.set_session_model_override(ModelId::new(model)).is_err()
            {
                debug!("run context already has a session model override");
            }
            if let Some(effort) = session.reasoning_effort_override
                && ctx.set_session_reasoning_effort_override(effort).is_err()
            {
                debug!("run context already has a session reasoning effort override");
            }
            self.safe_messages
                .lock()
                .await
                .insert(ctx.run_id.clone(), session.messages.clone());
            let mut state = self.state.lock().await;
            state.insert(ctx.run_id.clone(), session);
        }
        Decision::Continue
    }

    async fn on_user_prompt_submit(
        &self,
        msgs: &[Message],
        ctx: &RunCtx,
    ) -> Decision<Vec<Message>> {
        let combined = {
            let state = self.state.lock().await;
            let Some(session) = state.get(&ctx.run_id) else {
                return Decision::Continue;
            };
            if session.messages.is_empty() {
                return Decision::Continue;
            }
            let mut combined = session.messages.clone();
            combined.extend_from_slice(msgs);
            combined
        };
        Decision::Replace(combined)
    }

    async fn on_message(&self, msgs: &[Message], ctx: &RunCtx) -> Decision<Vec<Message>> {
        self.record_messages(ctx, msgs).await;
        Decision::Continue
    }

    async fn on_stop(&self, ctx: &RunCtx, outcome: &Outcome) {
        let session = {
            let mut state = self.state.lock().await;
            state.remove(&ctx.run_id)
        };
        let Some(session) = session else {
            return;
        };
        // Paused runs keep every completed turn: the tail IS the resume
        // state for the next run over this session. Dangling tool calls
        // (pause landed between dispatches) get a synthetic interrupted
        // result so resolved side effects stay in history and the model
        // decides what to redo.
        if matches!(outcome, Outcome::Paused) {
            {
                self.safe_messages.lock().await.remove(&ctx.run_id);
            }
            let messages = repaired_paused_log(&session.messages);
            self.save_session_messages(ctx, &session, &messages).await;
            return;
        }
        // Cancelled and errored runs intentionally persist only the safe
        // user/input prefix, never the partial assistant/tool tail. This
        // matches the module-level lifecycle doc ("Errored and cancelled runs
        // persist only the safe user/input prefix and do not fan out outbound
        // records"): a half-finished turn must not leave a dangling tool-call
        // or truncated assistant reply in the persisted history. Both outcomes
        // share this branch by design, not by accident.
        if !matches!(outcome, Outcome::Completed(_) | Outcome::MaxTurnsExceeded) {
            let snapshot = { self.safe_messages.lock().await.remove(&ctx.run_id) };
            // Prefer the longest safe prefix between the snapshot captured by
            // `record_messages` and the trimmed latest log. When the only
            // `on_message` of a brand-new run ended in an assistant/tool tail,
            // `record_messages` never refreshed the snapshot (it still holds
            // the empty `on_session_start` seed), so the snapshot alone would
            // drop this run's accepted user input. Trimming `session.messages`
            // to its last user/system boundary recovers that prefix.
            let messages = longest_safe_prefix(snapshot, &session.messages);
            self.save_session_messages(ctx, &session, &messages).await;
            return;
        }
        // Drop the safe-prefix snapshot before the async store write so the
        // `safe_messages` guard is not held across `.await`.
        {
            self.safe_messages.lock().await.remove(&ctx.run_id);
        }
        self.save_session_messages(ctx, &session, &session.messages)
            .await;
        self.fan_out_foreign_outbound(ctx, &session).await;
    }
}

mod safe_prefix;
use safe_prefix::{is_error_safe_log, longest_safe_prefix, repaired_paused_log};

#[cfg(test)]
mod tests;
