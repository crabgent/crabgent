//! [`TaskTranscriptHook`]: continuous, crash-safe transcript persistence
//! for executor-spawned task runs.
//!
//! The hook observes the kernel's canonical message log via the existing
//! [`Hook::on_message`] seam (no new trait method, no decorator sweep)
//! and full-overwrites the task's persisted transcript through
//! [`TaskStore::save_transcript`]. Each write is one atomic statement
//! whose `updated_at` bump doubles as the liveness heartbeat for boot
//! time orphan adoption. Runs without the task-id subject attribute
//! (channel, cron, goal-continuation runs) are ignored.
//!
//! Register it on the kernel the executor drives:
//! `Kernel::builder().add_hook(TaskTranscriptHook::new(store))`.

use std::str::FromStr;
use std::sync::Arc;

use async_trait::async_trait;
use crabgent_core::message::Message;
use crabgent_core::{Decision, Hook, RunCtx};
use crabgent_log::warn;
use crabgent_store::TaskId;
use crabgent_store::traits::TaskStore;

use crate::TASK_ID_ATTR;

/// Persists the in-flight conversation transcript of task runs so a
/// paused or crashed task can resume where it stopped.
pub struct TaskTranscriptHook<S: TaskStore> {
    store: Arc<S>,
}

impl<S: TaskStore> TaskTranscriptHook<S> {
    #[must_use]
    pub const fn new(store: Arc<S>) -> Self {
        Self { store }
    }
}

/// Flush cadence: every completed turn leg (assistant message or tool
/// result) is durable, plus the initial context burst before the first
/// assistant message (so a crash in turn one still resumes with the
/// caller-assembled context instead of just the prompt). Token deltas
/// never hit the store; the hook fires once per appended message.
fn should_persist(msgs: &[Message]) -> bool {
    match msgs.last() {
        Some(Message::Assistant { .. } | Message::ToolResult { .. }) => true,
        Some(_) => !msgs
            .iter()
            .any(|message| matches!(message, Message::Assistant { .. })),
        None => false,
    }
}

#[async_trait]
impl<S> Hook for TaskTranscriptHook<S>
where
    S: TaskStore + 'static,
{
    async fn on_message(&self, msgs: &[Message], ctx: &RunCtx) -> Decision<Vec<Message>> {
        let Some(raw_id) = ctx.subject.attr(TASK_ID_ATTR) else {
            return Decision::Continue;
        };
        if !should_persist(msgs) {
            return Decision::Continue;
        }
        let Ok(task_id) = TaskId::from_str(raw_id) else {
            log_invalid_task_id(raw_id);
            return Decision::Continue;
        };
        if let Err(error) = self.store.save_transcript(&task_id, msgs).await {
            log_save_failed(&task_id, &error);
        }
        Decision::Continue
    }
}

fn log_invalid_task_id(raw_id: &str) {
    warn!(
        attr = raw_id,
        "task transcript hook: subject carries an unparsable task id; skipping persistence"
    );
}

fn log_save_failed(task_id: &TaskId, error: &crabgent_store::StoreError) {
    // Fail-soft: a down store degrades resume granularity but never
    // blocks the run.
    warn!(
        task_id = %task_id,
        error = %error,
        "task transcript hook: save_transcript failed; transcript is stale until the next flush"
    );
}

#[cfg(test)]
#[path = "hook_tests.rs"]
mod tests;
