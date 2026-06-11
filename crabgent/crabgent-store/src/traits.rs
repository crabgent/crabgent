//! Trait surfaces for persistence backends.
//!
//! [`Store`] is the umbrella trait. Each sub-trait covers one concern. A
//! backend (in-memory, `SQLite`, Postgres, ...) implements all five sub-traits
//! and ties them together via [`Store`]'s associated types.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use crabgent_core::{MemoryId, MemoryScope, Message, Owner, SearchQuery, ThreadId};

use crate::error::StoreError;
use crate::ids::{ArchiveId, CronJobId, GoalId, RelationId, SessionId, TaskId};
use crate::page::Page;
use crate::records::{
    CronJob, CronJobUpdate, GoalStatus, MemoryDoc, MemoryHit, MemoryRelation, Session,
    SessionArchiveEntry, SessionInfo, SessionSearchHit, Task, TaskPauseCause, TaskStatus,
    ThreadGoal, ThreadGoalUpdate, ToolCacheEntry,
};
use crate::relation_type::RelationType;

/// Umbrella trait. A backend exposes one concrete instance and six sub-stores.
pub trait Store: Send + Sync {
    type Session: SessionStore;
    type Memory: MemoryStore;
    type Task: TaskStore;
    type Cron: CronStore;
    type ToolCache: ToolCacheStore;
    type Goal: GoalStore;

    fn session(&self) -> &Self::Session;
    fn memory(&self) -> &Self::Memory;
    fn task(&self) -> &Self::Task;
    fn cron(&self) -> &Self::Cron;
    fn tool_cache(&self) -> &Self::ToolCache;
    fn goal(&self) -> &Self::Goal;
}

/// Persistence for [`Session`] records.
#[async_trait]
pub trait SessionStore: Send + Sync {
    /// Look up the most recent session for the full scope tuple
    /// `(owner, thread, scope.channel, scope.conv, scope.agent, scope.kind)`,
    /// or create a fresh empty session if none exists. The lookup is
    /// NULL-safe: a `None` field on either side matches a NULL column on
    /// disk. Two `RunCtx` with the same `(owner, thread)` but different
    /// `scope.agent` resolve to distinct sessions, so cron-context runs
    /// without a thread do not collide across agents on the same channel
    /// owner.
    ///
    /// `scope.owner` is ignored for matching: the separate `owner` argument
    /// is authoritative. The created row stamps the full scope (with
    /// `scope.owner` set to `Some(owner)`).
    async fn find_or_create(
        &self,
        owner: &Owner,
        thread: Option<&ThreadId>,
        scope: &MemoryScope,
    ) -> Result<Session, StoreError>;

    /// Fetch a session by id.
    async fn load(&self, id: &SessionId) -> Result<Option<Session>, StoreError>;

    /// Persist a session (insert or replace).
    async fn save(&self, session: &Session) -> Result<(), StoreError>;

    /// Fetch the continuity summary produced by the compaction hook.
    async fn get_compaction_summary(&self, id: &SessionId) -> Result<Option<String>, StoreError> {
        Ok(self.load(id).await?.and_then(|s| s.compaction_summary))
    }

    /// Replace the continuity summary produced by the compaction hook.
    async fn set_compaction_summary(
        &self,
        id: &SessionId,
        summary: &str,
    ) -> Result<(), StoreError> {
        let Some(mut session) = self.load(id).await? else {
            return Err(StoreError::NotFound);
        };
        session.compaction_summary = Some(summary.to_owned());
        session.updated_at = Utc::now();
        self.save(&session).await
    }

    /// Update only the `messages` and `updated_at` columns of an existing
    /// session. Other columns (notably `model_override`, `title`,
    /// `summary`, `compaction_summary`) remain untouched.
    ///
    /// This is the path used by [`crate::SessionPersistHook`] on every
    /// kernel-message tick: that hook holds an in-memory `Session`
    /// snapshot taken at run start, so a full-row [`Self::save`] would
    /// clobber any column written by a tool mid-run (e.g.
    /// `models.set_session` mutating `model_override`).
    ///
    /// The default implementation reads the current row, patches the two
    /// columns and delegates to [`Self::save`]. Backends with column-level
    /// UPDATE support should override this to issue a targeted statement
    /// and avoid the read.
    async fn save_messages(
        &self,
        id: &SessionId,
        messages: &[Message],
        updated_at: DateTime<Utc>,
    ) -> Result<(), StoreError> {
        let Some(mut session) = self.load(id).await? else {
            return Err(StoreError::NotFound);
        };
        session.messages = messages.to_vec();
        session.updated_at = updated_at;
        self.save(&session).await
    }

    /// Persist the exact messages being compacted before the provider-facing
    /// message window is replaced.
    async fn archive_messages(
        &self,
        session_id: &SessionId,
        messages: &[Message],
        created_at: DateTime<Utc>,
    ) -> Result<ArchiveId, StoreError>;

    /// List archived message slices for a session, newest first.
    async fn list_archives(
        &self,
        session_id: &SessionId,
        page: Page,
    ) -> Result<Vec<SessionArchiveEntry>, StoreError>;

    /// Delete archives older than `days` by creation time.
    async fn cleanup_old_archives(&self, days: i64) -> Result<u64, StoreError>;

    /// List sessions for an owner, ordered by `updated_at` descending.
    async fn list(&self, owner: &Owner, page: Page) -> Result<Vec<SessionInfo>, StoreError>;

    /// Delete sessions older than `days` (by `updated_at`). Returns the count.
    async fn cleanup_old(&self, days: i64) -> Result<u64, StoreError>;

    /// Full-text search across stored sessions, scoped by
    /// [`SearchQuery::scope`]. Returns ranked hits with an excerpt.
    ///
    /// Backends must make an explicit choice for search support. The
    /// `MemoryScope::owner` field is treated as a hard filter; other
    /// fields narrow further.
    async fn search(&self, query: &SearchQuery) -> Result<Vec<SessionSearchHit>, StoreError>;
}

/// Persistence for long-term memory documents (free-text facts the
/// agent should retain across sessions). Backends index `body` for
/// full-text search and filter by [`MemoryScope`].
#[async_trait]
pub trait MemoryStore: Send + Sync {
    /// Run a scoped full-text search. Returns ranked hits.
    async fn search(&self, query: &SearchQuery) -> Result<Vec<MemoryHit>, StoreError>;

    /// Persist a new document. Returns the document id.
    async fn store(&self, doc: &MemoryDoc) -> Result<MemoryId, StoreError>;

    /// Fetch a document by id, returning `None` if absent.
    async fn get(&self, id: &MemoryId) -> Result<Option<MemoryDoc>, StoreError>;

    /// Delete a document by id. Returns `true` if a row was removed.
    async fn delete(&self, id: &MemoryId) -> Result<bool, StoreError>;

    /// Delete a document by id only when its scope matches `scope`.
    async fn delete_scoped(&self, id: &MemoryId, scope: &MemoryScope) -> Result<bool, StoreError>;

    /// Mark a document as archived. Returns `true` when a row was updated.
    async fn archive(&self, _id: &MemoryId, _at: DateTime<Utc>) -> Result<bool, StoreError> {
        Err(StoreError::Unsupported)
    }

    /// Remove a document archive marker. Returns `true` when a row was updated.
    async fn unarchive(&self, _id: &MemoryId) -> Result<bool, StoreError> {
        Err(StoreError::Unsupported)
    }

    /// Replace a document expiry timestamp. `None` clears expiry.
    async fn extend_expiry(
        &self,
        _id: &MemoryId,
        _new_expiry: Option<DateTime<Utc>>,
    ) -> Result<bool, StoreError> {
        Err(StoreError::Unsupported)
    }

    /// Replace the document body, clear its embedding, and bump `updated_at`.
    async fn update_body(&self, id: &MemoryId, new_body: String) -> Result<bool, StoreError>;

    /// Replace the document body and embedding together, then bump `updated_at`.
    async fn update_body_with_embedding(
        &self,
        id: &MemoryId,
        new_body: String,
        embedding: Option<Vec<f32>>,
    ) -> Result<bool, StoreError>;

    /// Store a relation edge between two documents. Idempotent on the natural
    /// key `(from_id, to_id, relation_type, scope.owner)`: re-storing the same
    /// edge returns the existing [`RelationId`] without inserting a duplicate.
    ///
    /// Returns [`StoreError::NotFound`] when either linked document is absent
    /// *or not visible under the edge `scope`*, so a caller can neither build
    /// edges over deleted nodes nor link (or probe the existence of) another
    /// owner's documents. Visibility uses the same owner-with-shared widening as
    /// [`Self::relation_neighbors`]; a wildcard scope field does not constrain
    /// that field.
    ///
    /// The default implementation reports [`StoreError::Unsupported`]; the
    /// in-memory, `SQLite`, and Postgres backends override it.
    async fn relation_store(&self, _relation: &MemoryRelation) -> Result<RelationId, StoreError> {
        Err(StoreError::Unsupported)
    }

    /// Delete the relation edge matching `(from_id, to_id, relation_type)`
    /// owned within `scope`. Returns `true` when a row was removed. The default
    /// implementation reports [`StoreError::Unsupported`].
    async fn relation_delete(
        &self,
        _from_id: &MemoryId,
        _to_id: &MemoryId,
        _relation_type: &RelationType,
        _scope: &MemoryScope,
    ) -> Result<bool, StoreError> {
        Err(StoreError::Unsupported)
    }

    /// Return relation edges incident to any id in `ids` (as `from_id` or
    /// `to_id`) that are visible under `scope`, using owner-with-shared
    /// semantics (the same widening as memory search `include_shared`). This is
    /// a single hop; recursive expansion and depth control live in the caller.
    /// The default implementation reports [`StoreError::Unsupported`].
    async fn relation_neighbors(
        &self,
        _ids: &[MemoryId],
        _scope: &MemoryScope,
    ) -> Result<Vec<MemoryRelation>, StoreError> {
        Err(StoreError::Unsupported)
    }
}

/// Persistence for [`Task`] records.
#[async_trait]
pub trait TaskStore: Send + Sync {
    /// Insert a fresh task. The id must be unique.
    async fn insert(&self, task: &Task) -> Result<(), StoreError>;

    /// Fetch a task by id.
    async fn get(&self, id: &TaskId) -> Result<Option<Task>, StoreError>;

    /// Append `chunk` to the task's `output` field and bump `updated_at`.
    /// No-op if the task does not exist.
    async fn append_output(&self, id: &TaskId, chunk: &str) -> Result<(), StoreError>;

    /// Mark the task as finished. `status` must be `Done` or `Failed`
    /// (`Running` and `Paused` are rejected). The optional `error` is only
    /// meaningful when `status == Failed`. Sets `finished_at` to `now` and
    /// clears `pause_cause`/`paused_at`.
    async fn finish(
        &self,
        id: &TaskId,
        status: TaskStatus,
        error: Option<&str>,
    ) -> Result<(), StoreError>;

    /// CAS `Running -> Paused` with the given cause. Stamps `pause_cause`
    /// and `paused_at`, bumps `updated_at`. Returns `true` only when the
    /// row was `Running` (the transition happened); a missing, finished, or
    /// already-paused task returns `false`. One atomic statement: pausing
    /// never races a concurrent finish into an inconsistent state.
    async fn pause(&self, id: &TaskId, cause: TaskPauseCause) -> Result<bool, StoreError>;

    /// CAS `Paused -> Running` for resume, guarded by the resume cap.
    /// The cap is a poison-task/crash-loop breaker, so only involuntary
    /// pauses count: a row paused with [`TaskPauseCause::Shutdown`]
    /// (clean cooperative pause) is always claimable and does not
    /// increment `resume_count`; `Forced` and `Crash` claims require
    /// `resume_count < max_resumes` and increment it. Clears
    /// `pause_cause`/`paused_at`, bumps `updated_at` (which shields the
    /// fresh claim from [`Self::recover_stuck`]). Returns `true` only for
    /// the single caller that won the claim; `false` means another
    /// claimer won, the task is not paused, or the cap is exhausted
    /// (callers distinguish the cap case via [`Self::get`]).
    async fn claim_for_resume(&self, id: &TaskId, max_resumes: u32) -> Result<bool, StoreError>;

    /// List tasks currently in `Paused` state, ordered oldest first.
    async fn list_paused(&self, page: Page) -> Result<Vec<Task>, StoreError>;

    /// Fold orphaned `Running` rows whose `updated_at` is older than
    /// `stale_secs` into `Paused` with cause
    /// [`TaskPauseCause::Crash`], stamping `paused_at`. Returns the
    /// affected ids. This is the boot-time crash-recovery primitive: a
    /// process that died without writing anything leaves `Running` rows
    /// whose transcript heartbeat stopped; folding them into `Paused`
    /// routes them through the same resume path as a graceful pause.
    /// `stale_secs = 0` adopts every `Running` row (single-process
    /// deployments where any `Running` row at boot is dead).
    async fn pause_orphans(&self, stale_secs: i64) -> Result<Vec<TaskId>, StoreError>;

    /// List direct children of `parent` (any status, via
    /// `parent_task_id`), ordered oldest first.
    async fn list_children(&self, parent: &TaskId, page: Page) -> Result<Vec<Task>, StoreError>;

    /// Atomically replace the persisted conversation transcript and bump
    /// `updated_at` (the bump doubles as the liveness heartbeat that
    /// [`Self::pause_orphans`] and [`Self::recover_stuck`] read). One
    /// full-overwrite write: a crash mid-write leaves the old or the new
    /// value, never a torn one. No-op if the task does not exist.
    async fn save_transcript(&self, id: &TaskId, messages: &[Message]) -> Result<(), StoreError>;

    /// Load the persisted conversation transcript. `None` when the task
    /// does not exist or never had a transcript written.
    async fn load_transcript(&self, id: &TaskId) -> Result<Option<Vec<Message>>, StoreError>;

    /// List tasks currently in `Running` state, ordered oldest first.
    async fn list_running(&self, page: Page) -> Result<Vec<Task>, StoreError>;

    /// Returns running tasks for the given owner; completed/failed tasks must
    /// be fetched via `get`.
    async fn list_by_owner(
        &self,
        owner: Option<&Owner>,
        page: Page,
    ) -> Result<Vec<Task>, StoreError> {
        let Some(owner) = owner else {
            return self.list_running(page).await;
        };
        if page.limit == 0 {
            return Ok(Vec::new());
        }

        let mut matches_seen = 0usize;
        let mut tasks = Vec::with_capacity(page.limit);
        let mut scan_page = Page::first(page.limit.max(50));
        loop {
            let running = self.list_running(scan_page).await?;
            if running.is_empty() {
                return Ok(tasks);
            }
            let batch_len = running.len();
            for task in running.into_iter().filter(|task| &task.owner == owner) {
                if matches_seen < page.offset {
                    matches_seen += 1;
                    continue;
                }
                tasks.push(task);
                if tasks.len() == page.limit {
                    return Ok(tasks);
                }
            }
            if batch_len < scan_page.limit {
                return Ok(tasks);
            }
            scan_page = scan_page.next();
        }
    }

    /// Reset tasks that have been `Running` past `timeout_secs` to a final
    /// `Failed` state. Returns the affected ids. `Paused` rows are never
    /// touched. Hosts that resume paused tasks at startup must call
    /// `resume_paused` (which folds orphans via [`Self::pause_orphans`])
    /// BEFORE scheduling this sweeper, otherwise crash orphans are failed
    /// instead of resumed.
    async fn recover_stuck(&self, timeout_secs: i64) -> Result<Vec<TaskId>, StoreError>;

    /// Delete tasks finished more than `days` ago. Returns the count.
    async fn cleanup_old(&self, days: i64) -> Result<u64, StoreError>;
}

/// Persistence for [`CronJob`] records.
#[async_trait]
pub trait CronStore: Send + Sync {
    /// Insert a fresh cron job. The id must be unique.
    async fn create(&self, job: &CronJob) -> Result<(), StoreError>;

    /// Fetch a cron job by id.
    async fn get(&self, id: &CronJobId) -> Result<Option<CronJob>, StoreError>;

    /// List jobs matching every present field in `scope`.
    async fn list(&self, scope: &MemoryScope, page: Page) -> Result<Vec<CronJob>, StoreError>;

    /// Apply a partial update. Returns `true` if a job with `id` existed.
    async fn update(&self, id: &CronJobId, update: &CronJobUpdate) -> Result<bool, StoreError>;

    /// Delete the job. Returns `true` if it existed.
    async fn delete(&self, id: &CronJobId) -> Result<bool, StoreError>;

    /// Atomically claim up to `limit` jobs whose `next_run <= now` and that
    /// are not already claimed. Each returned job has `claimed_at` set.
    async fn claim_due(&self, now: DateTime<Utc>, limit: usize)
    -> Result<Vec<CronJob>, StoreError>;

    /// Release a claim. Updates `last_run`, `next_run`, and clears `claimed_at`.
    /// If `disable_run_once` is true and the job had `run_once == true`, the
    /// job is also disabled.
    async fn finish_claim(
        &self,
        id: &CronJobId,
        last_run: DateTime<Utc>,
        next_run: DateTime<Utc>,
        disable_run_once: bool,
    ) -> Result<(), StoreError>;

    /// Release a claim without advancing the schedule.
    async fn release_claim_only(&self, id: &CronJobId) -> Result<(), StoreError>;

    /// Reset jobs stuck in `claimed_at` older than `timeout_secs`. Returns
    /// the affected ids.
    async fn recover_stuck(&self, timeout_secs: i64) -> Result<Vec<CronJobId>, StoreError>;
}

/// Persistence for cached tool outputs.
#[async_trait]
pub trait ToolCacheStore: Send + Sync {
    /// Insert a cache entry. Idempotent on `(id, session_id)`.
    async fn insert(&self, entry: &ToolCacheEntry) -> Result<(), StoreError>;

    /// Fetch a non-expired entry by `(id, session_id)`.
    async fn get(
        &self,
        id: &str,
        session_id: &SessionId,
    ) -> Result<Option<ToolCacheEntry>, StoreError>;

    /// Delete every expired entry. Returns the count.
    async fn cleanup_expired(&self) -> Result<u64, StoreError>;
}

/// Persistence for [`ThreadGoal`] records.
///
/// A goal is a per-[`SessionId`] singleton: at most one goal exists per
/// session. [`Self::create`] is atomic against that invariant and reports
/// [`StoreError::Conflict`] when a goal already exists for the session.
/// Clearing the goal ([`Self::delete`]) frees the session to take a new one.
#[async_trait]
pub trait GoalStore: Send + Sync {
    /// Insert a fresh goal. Returns [`StoreError::Conflict`] when the session
    /// already has a goal.
    async fn create(&self, goal: &ThreadGoal) -> Result<(), StoreError>;

    /// Fetch a goal by id.
    async fn get(&self, id: &GoalId) -> Result<Option<ThreadGoal>, StoreError>;

    /// Fetch the goal for `session`, if any.
    async fn get_for_session(&self, session: &SessionId) -> Result<Option<ThreadGoal>, StoreError>;

    /// Apply a partial update. Returns `true` when a goal with `id` existed.
    async fn update(&self, id: &GoalId, update: &ThreadGoalUpdate) -> Result<bool, StoreError>;

    /// Add `token_delta` and `time_delta_seconds` (both clamped at 0) to the
    /// goal's accounting fields and stamp `at` as `updated_at`. When the goal
    /// is `Active`, carries a positive `token_budget`, and `tokens_used`
    /// reaches that budget, the status flips to `budget_limited` in the same
    /// write (see [`GoalStatus`](crate::records::GoalStatus)). Returns the
    /// updated goal, or `None` when no goal with `id` exists.
    async fn account_usage(
        &self,
        id: &GoalId,
        token_delta: i64,
        time_delta_seconds: i64,
        at: DateTime<Utc>,
    ) -> Result<Option<ThreadGoal>, StoreError>;

    /// Delete the goal. Returns `true` when a row was removed.
    async fn delete(&self, id: &GoalId) -> Result<bool, StoreError>;

    /// List goals with the given status, ordered by `updated_at`
    /// descending.
    async fn list_by_status(
        &self,
        status: GoalStatus,
        page: Page,
    ) -> Result<Vec<ThreadGoal>, StoreError>;

    /// Atomically flip every [`GoalStatus::Suspended`] goal back to
    /// `Active`, stamping `at` as `updated_at`, and return the flipped
    /// goals. One atomic statement per backend: a concurrent host pause
    /// landing during the flip is never clobbered (it targets `Paused`,
    /// which this never touches), and a second call returns an empty list
    /// (idempotent). This is the startup primitive for resuming
    /// shutdown-suspended goals.
    async fn resume_suspended(&self, at: DateTime<Utc>) -> Result<Vec<ThreadGoal>, StoreError>;
}
