//! Actions submitted to the policy hook for evaluation.

use crate::memory::{MemoryId, MemoryScope};
use crate::model::{ModelId, ReasoningEffort};
use crate::owner::Owner;

/// Structured target for a named policy action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionTarget {
    owner: Owner,
    qualifier: Option<String>,
}

impl ActionTarget {
    /// Build an unqualified target.
    #[must_use]
    pub fn new(owner: impl Into<Owner>) -> Self {
        Self {
            owner: owner.into(),
            qualifier: None,
        }
    }

    /// Attach a qualifier to the target.
    #[must_use]
    pub fn with_qualifier(mut self, qualifier: impl Into<String>) -> Self {
        self.qualifier = Some(qualifier.into());
        self
    }

    /// Borrow the target owner.
    #[must_use]
    pub const fn owner(&self) -> &Owner {
        &self.owner
    }

    /// Borrow the optional target qualifier.
    #[must_use]
    pub fn qualifier(&self) -> Option<&str> {
        self.qualifier.as_deref()
    }
}

/// An action subject to policy evaluation.
///
/// `PolicyHook::allow` receives a borrowed `Action` to decide whether the
/// subject may perform the action.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Action {
    /// The kernel is about to call the LLM.
    LlmCall,
    /// The kernel is about to dispatch a tool by name.
    ToolCall(String),
    /// A hook-defined action (e.g. memory access, custom side effect).
    Custom(String),
    /// A named action with a structured target owned by another crate.
    Targeted { name: String, target: ActionTarget },
    /// A memory tool is about to run a full-text search.
    MemorySearch { query: String, scope: MemoryScope },
    /// A memory tool is about to persist a new document.
    MemoryStore { scope: MemoryScope },
    /// A memory tool is about to fetch a document by id.
    MemoryGet { id: MemoryId, scope: MemoryScope },
    /// A memory tool is about to delete a document by id.
    MemoryDelete { id: MemoryId, scope: MemoryScope },
    /// A memory tool is about to archive a document by id.
    MemoryArchive { id: MemoryId, scope: MemoryScope },
    /// A memory tool is about to unarchive a document by id.
    MemoryUnarchive { id: MemoryId, scope: MemoryScope },
    /// A memory tool is about to replace a document expiry timestamp.
    MemoryExtendExpiry { id: MemoryId, scope: MemoryScope },
    /// A memory consolidation run is about to read and update memory docs.
    MemoryConsolidate { scope: MemoryScope },
    /// A memory tool is about to store a relation edge between two documents.
    ///
    /// The edge's own scope is the writer's; the linked documents may belong
    /// to other owners. Gated by `StrictPolicyBuilder::allow_relation_store`.
    RelationStore { scope: MemoryScope },
    /// A memory tool is about to delete a relation edge by natural key.
    /// Gated by `StrictPolicyBuilder::allow_relation_delete`.
    RelationDelete { scope: MemoryScope },
    /// A memory tool is about to expand the relation graph from a root
    /// document. Gated by `StrictPolicyBuilder::allow_relation_expand`.
    RelationExpand { scope: MemoryScope },
    /// A session-search tool is about to run a full-text search across
    /// stored conversations.
    SessionSearch { query: String, scope: MemoryScope },
    /// A task tool is about to create a background task.
    TaskCreate { owner: Option<Owner> },
    /// A task tool is about to list background tasks.
    TaskList { owner: Option<Owner> },
    /// A task tool is about to fetch a background task.
    TaskGet {
        /// String, not `TaskId`: `crabgent-core` must not depend on
        /// `crabgent-store` (dep-cycle). Callers convert via `TaskId::from_str`.
        id: String,
        owner: Option<Owner>,
    },
    /// A task tool is about to cancel a background task.
    TaskCancel {
        /// String, not `TaskId`: `crabgent-core` must not depend on
        /// `crabgent-store` (dep-cycle). Callers convert via `TaskId::from_str`.
        id: String,
        owner: Option<Owner>,
    },
    /// A model registry tool is about to list registered models.
    ModelList,
    /// A model registry tool is about to fetch a registered model.
    ModelGet {
        // ModelId lives in crabgent-core; same-crate, no dep-cycle so we use the typed
        // newtype here, unlike CronGet/TaskGet which use String to avoid crabgent-store.
        id: ModelId,
    },
    /// A model registry tool is about to inspect the current resolved model.
    ModelsCurrent { session_id: Option<String> },
    /// A model registry tool is about to set a session-scoped model override.
    ModelsSetSessionOverride {
        /// String, not `SessionId`: `crabgent-core` must not depend on
        /// `crabgent-store` (dep-cycle). Callers convert via `SessionId::from_str`.
        session_id: String,
        model: ModelId,
    },
    /// A model registry tool is about to clear a session-scoped model override.
    ModelsClearSessionOverride {
        /// String, not `SessionId`: `crabgent-core` must not depend on
        /// `crabgent-store` (dep-cycle). Callers convert via `SessionId::from_str`.
        session_id: String,
    },
    /// A model registry tool is about to set the global model override.
    ModelsSetGlobalOverride { model: ModelId },
    /// A model registry tool is about to clear the global model override.
    ModelsClearGlobalOverride,
    /// A model registry tool is about to inspect the current resolved
    /// reasoning effort.
    ReasoningEffortCurrent { session_id: Option<String> },
    /// A model registry tool is about to set a session-scoped reasoning
    /// effort override.
    ReasoningEffortSetSessionOverride {
        /// String, not `SessionId`: `crabgent-core` must not depend on
        /// `crabgent-store` (dep-cycle). Callers convert via `SessionId::from_str`.
        session_id: String,
        effort: ReasoningEffort,
    },
    /// A model registry tool is about to clear a session-scoped reasoning
    /// effort override.
    ReasoningEffortClearSessionOverride {
        /// String, not `SessionId`: `crabgent-core` must not depend on
        /// `crabgent-store` (dep-cycle). Callers convert via `SessionId::from_str`.
        session_id: String,
    },
    /// A model registry tool is about to set the global reasoning-effort override.
    ReasoningEffortSetGlobalOverride { effort: ReasoningEffort },
    /// A model registry tool is about to clear the global reasoning-effort override.
    ReasoningEffortClearGlobalOverride,
    /// A calendar tool is about to list holidays for a locale and year.
    CalendarHolidaysList,
    /// A calendar tool is about to list upcoming holidays.
    CalendarHolidaysNext,
    /// A calendar tool is about to check whether a date is a holiday.
    CalendarHolidayCheck,
    /// A calendar tool is about to calculate day counts between dates.
    CalendarDaysBetween,
    /// A calendar tool is about to add or subtract calendar units from a date.
    CalendarDateArith,
    /// A calendar tool is about to return weekday metadata for a date.
    CalendarWeekdayInfo,
    /// A cron tool is about to create a persisted cron job.
    CronCreate { scope: MemoryScope },
    /// A cron tool is about to fetch a persisted cron job.
    CronGet {
        /// String, not `CronJobId`: `crabgent-core` must not depend on
        /// `crabgent-store` (dep-cycle). Callers convert via `CronJobId::from_str`.
        id: String,
        scope: MemoryScope,
    },
    /// A cron tool is about to list persisted cron jobs.
    CronList { scope: MemoryScope },
    /// A cron tool is about to update a persisted cron job.
    CronUpdate {
        /// String, not `CronJobId`: `crabgent-core` must not depend on
        /// `crabgent-store` (dep-cycle). Callers convert via `CronJobId::from_str`.
        id: String,
        scope: MemoryScope,
    },
    /// A cron tool is about to delete a persisted cron job.
    CronDelete {
        /// String, not `CronJobId`: `crabgent-core` must not depend on
        /// `crabgent-store` (dep-cycle). Callers convert via `CronJobId::from_str`.
        id: String,
        scope: MemoryScope,
    },
    /// The kernel is about to forward a hosted web-search request to the
    /// provider. Gated by `StrictPolicyBuilder::allow_hosted_web_search`.
    ///
    /// `provider` is the provider name (`"anthropic"`, `"openai"`, `"google"`).
    HostedWebSearch { provider: String },
    /// A channel inbox is about to persist a subject's raw audio bytes to disk
    /// for later re-hearing. Gated by `StrictPolicyBuilder::allow_audio_retain`.
    ///
    /// Privacy-sensitive: persistence of user voice is fail-closed and
    /// per-subject auditable. The decision is driven by subject + scope; the
    /// store assigns the `AudioRef` only after a granted retention.
    AudioRetain { scope: MemoryScope },
    /// A goal tool is about to create a thread goal.
    ///
    /// Creation is meant to happen only on an explicit user/system request.
    /// Trusted paths (a `/goal` command, an operator) stamp an origin attr on
    /// the subject; a `StrictPolicy` rule can gate this action on that attr
    /// (`Rule::allow(ActionMatcher::GoalCreate { .. }).requires_attr(...)`),
    /// so a model-initiated create is denied unless the operator opts in.
    GoalCreate { owner: Option<Owner> },
    /// A goal tool is about to read the current thread goal.
    GoalGet { owner: Option<Owner> },
    /// A goal tool is about to mark the thread goal complete or blocked.
    GoalUpdate { owner: Option<Owner> },
    /// A host surface (e.g. a `/goal pause|resume|clear` command) is about to
    /// drive a host-controlled goal status change or clear the goal. The model
    /// tool surface never raises this action.
    GoalManage { owner: Option<Owner> },
}

impl Action {
    /// Stable identifier for this action, suitable for logging or matching.
    #[must_use]
    pub const fn name(&self) -> &str {
        match self {
            Self::LlmCall => "llm.call",
            Self::ToolCall(name) | Self::Custom(name) | Self::Targeted { name, .. } => {
                name.as_str()
            }
            Self::MemorySearch { .. } => "memory.search",
            Self::MemoryStore { .. } => "memory.store",
            Self::MemoryGet { .. } => "memory.get",
            Self::MemoryDelete { .. } => "memory.delete",
            Self::MemoryArchive { .. } => "memory.archive",
            Self::MemoryUnarchive { .. } => "memory.unarchive",
            Self::MemoryExtendExpiry { .. } => "memory.extend_expiry",
            Self::MemoryConsolidate { .. } => "memory.consolidate",
            Self::RelationStore { .. } => "memory.relation_store",
            Self::RelationDelete { .. } => "memory.relation_delete",
            Self::RelationExpand { .. } => "memory.relation_expand",
            Self::SessionSearch { .. } => "session.search",
            Self::TaskCreate { .. } => "task.create",
            Self::TaskList { .. } => "task.list",
            Self::TaskGet { .. } => "task.get",
            Self::TaskCancel { .. } => "task.cancel",
            Self::ModelList => "models.list",
            Self::ModelGet { .. } => "models.get",
            Self::ModelsCurrent { .. } => "models.current",
            Self::ModelsSetSessionOverride { .. } => "models.set_session_override",
            Self::ModelsClearSessionOverride { .. } => "models.clear_session_override",
            Self::ModelsSetGlobalOverride { .. } => "models.set_global_override",
            Self::ModelsClearGlobalOverride => "models.clear_global_override",
            Self::ReasoningEffortCurrent { .. } => "models.current_effort",
            Self::ReasoningEffortSetSessionOverride { .. } => "models.set_session_effort_override",
            Self::ReasoningEffortClearSessionOverride { .. } => {
                "models.clear_session_effort_override"
            }
            Self::ReasoningEffortSetGlobalOverride { .. } => "models.set_global_effort_override",
            Self::ReasoningEffortClearGlobalOverride => "models.clear_global_effort_override",
            Self::CalendarHolidaysList => "calendar.holidays_list",
            Self::CalendarHolidaysNext => "calendar.holidays_next",
            Self::CalendarHolidayCheck => "calendar.holiday_check",
            Self::CalendarDaysBetween => "calendar.days_between",
            Self::CalendarDateArith => "calendar.date_arith",
            Self::CalendarWeekdayInfo => "calendar.weekday_info",
            Self::CronCreate { .. } => "cron.create",
            Self::CronGet { .. } => "cron.get",
            Self::CronList { .. } => "cron.list",
            Self::CronUpdate { .. } => "cron.update",
            Self::CronDelete { .. } => "cron.delete",
            Self::HostedWebSearch { .. } => "hosted.web_search",
            Self::AudioRetain { .. } => "audio.retain",
            Self::GoalCreate { .. } => "goal.create",
            Self::GoalGet { .. } => "goal.get",
            Self::GoalUpdate { .. } => "goal.update",
            Self::GoalManage { .. } => "goal.manage",
        }
    }

    /// Construct a tool-call action by name.
    #[must_use]
    pub fn tool(name: impl Into<String>) -> Self {
        Self::ToolCall(name.into())
    }

    /// Construct a custom action by name.
    #[must_use]
    pub fn custom(name: impl Into<String>) -> Self {
        Self::Custom(name.into())
    }

    /// Construct a named action with a structured target.
    #[must_use]
    pub fn targeted(name: impl Into<String>, target: ActionTarget) -> Self {
        Self::Targeted {
            name: name.into(),
            target,
        }
    }

    /// Borrow the scope attached to memory, session, or cron actions.
    #[must_use]
    pub const fn scope(&self) -> Option<&MemoryScope> {
        match self {
            Self::MemorySearch { scope, .. }
            | Self::MemoryStore { scope }
            | Self::MemoryGet { scope, .. }
            | Self::MemoryDelete { scope, .. }
            | Self::MemoryArchive { scope, .. }
            | Self::MemoryUnarchive { scope, .. }
            | Self::MemoryExtendExpiry { scope, .. }
            | Self::MemoryConsolidate { scope }
            | Self::RelationStore { scope }
            | Self::RelationDelete { scope }
            | Self::RelationExpand { scope }
            | Self::SessionSearch { scope, .. }
            | Self::CronCreate { scope }
            | Self::CronGet { scope, .. }
            | Self::CronList { scope }
            | Self::CronUpdate { scope, .. }
            | Self::CronDelete { scope, .. }
            | Self::AudioRetain { scope } => Some(scope),
            Self::LlmCall
            | Self::ToolCall(_)
            | Self::Custom(_)
            | Self::Targeted { .. }
            | Self::TaskCreate { .. }
            | Self::TaskList { .. }
            | Self::TaskGet { .. }
            | Self::TaskCancel { .. }
            | Self::ModelList
            | Self::ModelGet { .. }
            | Self::ModelsCurrent { .. }
            | Self::ModelsSetSessionOverride { .. }
            | Self::ModelsClearSessionOverride { .. }
            | Self::ModelsSetGlobalOverride { .. }
            | Self::ModelsClearGlobalOverride
            | Self::ReasoningEffortCurrent { .. }
            | Self::ReasoningEffortSetSessionOverride { .. }
            | Self::ReasoningEffortClearSessionOverride { .. }
            | Self::ReasoningEffortSetGlobalOverride { .. }
            | Self::ReasoningEffortClearGlobalOverride
            | Self::CalendarHolidaysList
            | Self::CalendarHolidaysNext
            | Self::CalendarHolidayCheck
            | Self::CalendarDaysBetween
            | Self::CalendarDateArith
            | Self::CalendarWeekdayInfo
            | Self::HostedWebSearch { .. }
            | Self::GoalCreate { .. }
            | Self::GoalGet { .. }
            | Self::GoalUpdate { .. }
            | Self::GoalManage { .. } => None,
        }
    }
}

#[cfg(test)]
mod tests;
