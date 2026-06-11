use crate::action::Action;
use crate::owner::Owner;

use super::target_match::{TargetPredicate, qualifier_matches};

/// Matcher for policy rules.
///
/// Stable enum for matching individual policy-gated action families.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum ActionMatcher {
    Any,
    LlmCall,
    Tool(String),
    Custom(String),
    MemorySearch,
    MemoryStore,
    MemoryGet,
    MemoryDelete,
    MemoryArchive,
    MemoryUnarchive,
    MemoryExtendExpiry,
    MemoryConsolidate,
    /// Matches memory tool variants, excluding consolidation.
    MemoryAny,
    SessionSearch,
    /// Matches every model-registry action; prefer granular matchers unless the
    /// policy is deliberately admin-wide.
    ModelsAny,
    ModelList,
    ModelGet,
    ModelsCurrent {
        /// `None` matches calls without an explicit session target. Use
        /// `ModelsAny` for deliberately admin-wide current-model reads.
        session_id: Option<String>,
    },
    ModelsSetSessionOverride {
        /// `None` matches any target session. Convenience builders only expose
        /// exact session matching; use the raw matcher for admin policies.
        session_id: Option<String>,
    },
    ModelsClearSessionOverride {
        /// `None` matches any target session. Convenience builders only expose
        /// exact session matching; use the raw matcher for admin policies.
        session_id: Option<String>,
    },
    ModelsSetGlobalOverride,
    ModelsClearGlobalOverride,
    /// Matches every reasoning-effort override action; prefer granular
    /// matchers unless the policy is deliberately admin-wide.
    ReasoningEffortAny,
    ReasoningEffortCurrent {
        /// `None` matches calls without an explicit session target. Use
        /// `ReasoningEffortAny` for deliberately admin-wide reads.
        session_id: Option<String>,
    },
    ReasoningEffortSetSessionOverride {
        /// `None` matches any target session. Convenience builders only expose
        /// exact session matching; use the raw matcher for admin policies.
        session_id: Option<String>,
    },
    ReasoningEffortClearSessionOverride {
        /// `None` matches any target session. Convenience builders only expose
        /// exact session matching; use the raw matcher for admin policies.
        session_id: Option<String>,
    },
    ReasoningEffortSetGlobalOverride,
    ReasoningEffortClearGlobalOverride,
    TaskCreate {
        /// `None` matches any task owner. `Some(owner)` requires exact owner.
        owner: Option<Owner>,
    },
    TaskList {
        /// `None` matches any task owner. `Some(owner)` requires exact owner.
        owner: Option<Owner>,
    },
    TaskGet {
        /// `None` matches any task owner. `Some(owner)` requires exact owner.
        owner: Option<Owner>,
    },
    TaskCancel {
        /// `None` matches any task owner. `Some(owner)` requires exact owner.
        owner: Option<Owner>,
    },
    TaskAny {
        /// `None` matches any task owner. `Some(owner)` requires exact owner.
        owner: Option<Owner>,
    },
    CalendarHolidaysList,
    CalendarHolidaysNext,
    CalendarHolidayCheck,
    CalendarDaysBetween,
    CalendarDateArith,
    CalendarWeekdayInfo,
    CalendarAny,
    CronCreate,
    CronGet,
    CronList,
    CronUpdate,
    CronDelete,
    CronAny,
    Targeted {
        name: String,
        qualifier: Option<String>,
        target: TargetPredicate,
    },
    /// Matches `Action::HostedWebSearch`. `None` matches any provider.
    HostedWebSearch {
        provider: Option<String>,
    },
    /// Matches `Action::AudioRetain`. Pair with `requires_scope_from_subject`
    /// so a subject only authorizes retention of their own audio scope.
    AudioRetain,
    RelationStore,
    RelationDelete,
    RelationExpand,
    /// Matches every memory-relation action (store, delete, expand). Relations
    /// are a distinct family: `MemoryAny` does not absorb them.
    RelationAny,
    GoalCreate {
        /// `None` matches any goal owner. `Some(owner)` requires exact owner.
        owner: Option<Owner>,
    },
    GoalGet {
        /// `None` matches any goal owner. `Some(owner)` requires exact owner.
        owner: Option<Owner>,
    },
    GoalUpdate {
        /// `None` matches any goal owner. `Some(owner)` requires exact owner.
        owner: Option<Owner>,
    },
    GoalManage {
        /// `None` matches any goal owner. `Some(owner)` requires exact owner.
        owner: Option<Owner>,
    },
    GoalAny {
        /// `None` matches any goal owner. `Some(owner)` requires exact owner.
        owner: Option<Owner>,
    },
}

impl ActionMatcher {
    pub(super) fn matches(&self, action: &Action) -> bool {
        self.matches_simple_action(action) || self.matches_parameterized(action)
    }

    fn matches_parameterized(&self, action: &Action) -> bool {
        self.matches_session_scoped(action)
            || self.matches_owner_scoped(action)
            || self.matches_targeted(action)
    }

    fn matches_session_scoped(&self, action: &Action) -> bool {
        match (self, action) {
            (
                Self::ModelsCurrent { session_id },
                Action::ModelsCurrent {
                    session_id: action_session_id,
                },
            )
            | (
                Self::ReasoningEffortCurrent { session_id },
                Action::ReasoningEffortCurrent {
                    session_id: action_session_id,
                },
            ) => optional_session_id_matches(session_id.as_deref(), action_session_id.as_deref()),
            (
                Self::ModelsSetSessionOverride { session_id },
                Action::ModelsSetSessionOverride {
                    session_id: action_session_id,
                    ..
                },
            )
            | (
                Self::ModelsClearSessionOverride { session_id },
                Action::ModelsClearSessionOverride {
                    session_id: action_session_id,
                },
            )
            | (
                Self::ReasoningEffortSetSessionOverride { session_id },
                Action::ReasoningEffortSetSessionOverride {
                    session_id: action_session_id,
                    ..
                },
            )
            | (
                Self::ReasoningEffortClearSessionOverride { session_id },
                Action::ReasoningEffortClearSessionOverride {
                    session_id: action_session_id,
                },
            ) => session_id_matches(session_id.as_deref(), action_session_id),
            _ => false,
        }
    }

    fn matches_owner_scoped(&self, action: &Action) -> bool {
        match (self, action) {
            (
                Self::TaskCreate { owner },
                Action::TaskCreate {
                    owner: action_owner,
                },
            )
            | (
                Self::TaskList { owner },
                Action::TaskList {
                    owner: action_owner,
                },
            )
            | (
                Self::TaskGet { owner },
                Action::TaskGet {
                    owner: action_owner,
                    ..
                },
            )
            | (
                Self::TaskCancel { owner },
                Action::TaskCancel {
                    owner: action_owner,
                    ..
                },
            )
            | (
                Self::GoalCreate { owner },
                Action::GoalCreate {
                    owner: action_owner,
                },
            )
            | (
                Self::GoalGet { owner },
                Action::GoalGet {
                    owner: action_owner,
                },
            )
            | (
                Self::GoalUpdate { owner },
                Action::GoalUpdate {
                    owner: action_owner,
                },
            )
            | (
                Self::GoalManage { owner },
                Action::GoalManage {
                    owner: action_owner,
                },
            ) => owner_matches(owner.as_ref(), action_owner.as_ref()),
            _ => false,
        }
    }

    fn matches_targeted(&self, action: &Action) -> bool {
        match (self, action) {
            (
                Self::Targeted {
                    name,
                    qualifier,
                    target,
                },
                Action::Targeted {
                    name: action_name,
                    target: action_target,
                },
            ) => {
                name == action_name
                    && qualifier_matches(qualifier.as_deref(), action_target.qualifier())
                    && target.matches(action_target.owner())
            }
            _ => false,
        }
    }

    fn matches_simple_action(&self, action: &Action) -> bool {
        match (self, action) {
            (Self::Any, _)
            | (Self::LlmCall, Action::LlmCall)
            | (Self::MemorySearch, Action::MemorySearch { .. })
            | (Self::MemoryStore, Action::MemoryStore { .. })
            | (Self::MemoryGet, Action::MemoryGet { .. })
            | (Self::MemoryDelete, Action::MemoryDelete { .. })
            | (Self::MemoryArchive, Action::MemoryArchive { .. })
            | (Self::MemoryUnarchive, Action::MemoryUnarchive { .. })
            | (Self::MemoryExtendExpiry, Action::MemoryExtendExpiry { .. })
            | (Self::MemoryConsolidate, Action::MemoryConsolidate { .. })
            | (Self::SessionSearch, Action::SessionSearch { .. })
            | (Self::ModelList, Action::ModelList)
            | (Self::ModelGet, Action::ModelGet { .. })
            | (Self::ModelsSetGlobalOverride, Action::ModelsSetGlobalOverride { .. })
            | (Self::ModelsClearGlobalOverride, Action::ModelsClearGlobalOverride)
            | (
                Self::ReasoningEffortSetGlobalOverride,
                Action::ReasoningEffortSetGlobalOverride { .. },
            )
            | (
                Self::ReasoningEffortClearGlobalOverride,
                Action::ReasoningEffortClearGlobalOverride,
            )
            | (Self::CalendarHolidaysList, Action::CalendarHolidaysList)
            | (Self::CalendarHolidaysNext, Action::CalendarHolidaysNext)
            | (Self::CalendarHolidayCheck, Action::CalendarHolidayCheck)
            | (Self::CalendarDaysBetween, Action::CalendarDaysBetween)
            | (Self::CalendarDateArith, Action::CalendarDateArith)
            | (Self::CalendarWeekdayInfo, Action::CalendarWeekdayInfo)
            | (Self::CronCreate, Action::CronCreate { .. })
            | (Self::CronGet, Action::CronGet { .. })
            | (Self::CronList, Action::CronList { .. })
            | (Self::CronUpdate, Action::CronUpdate { .. })
            | (Self::CronDelete, Action::CronDelete { .. })
            | (Self::AudioRetain, Action::AudioRetain { .. })
            | (Self::RelationStore, Action::RelationStore { .. })
            | (Self::RelationDelete, Action::RelationDelete { .. })
            | (Self::RelationExpand, Action::RelationExpand { .. }) => true,
            (
                Self::HostedWebSearch { provider },
                Action::HostedWebSearch {
                    provider: action_provider,
                },
            ) => match provider {
                Some(p) => p == action_provider,
                None => true,
            },
            (Self::Tool(name), Action::ToolCall(tool)) => tool == name,
            (Self::Custom(name), Action::Custom(custom)) => custom == name,
            (Self::MemoryAny, action) => is_memory_action(action),
            (Self::RelationAny, action) => is_relation_action(action),
            (Self::ModelsAny, action) => is_models_action(action),
            (Self::ReasoningEffortAny, action) => is_reasoning_effort_action(action),
            (Self::TaskAny { owner }, action) => is_task_action(action, owner.as_ref()),
            (Self::GoalAny { owner }, action) => is_goal_action(action, owner.as_ref()),
            (Self::CalendarAny, action) => is_calendar_action(action),
            (Self::CronAny, action) => is_cron_action(action),
            _ => false,
        }
    }
}

const fn is_memory_action(action: &Action) -> bool {
    matches!(
        action,
        Action::MemorySearch { .. }
            | Action::MemoryStore { .. }
            | Action::MemoryGet { .. }
            | Action::MemoryDelete { .. }
            | Action::MemoryArchive { .. }
            | Action::MemoryUnarchive { .. }
            | Action::MemoryExtendExpiry { .. }
    )
}

const fn is_relation_action(action: &Action) -> bool {
    matches!(
        action,
        Action::RelationStore { .. }
            | Action::RelationDelete { .. }
            | Action::RelationExpand { .. }
    )
}

const fn is_models_action(action: &Action) -> bool {
    matches!(
        action,
        Action::ModelList
            | Action::ModelGet { .. }
            | Action::ModelsCurrent { .. }
            | Action::ModelsSetSessionOverride { .. }
            | Action::ModelsClearSessionOverride { .. }
            | Action::ModelsSetGlobalOverride { .. }
            | Action::ModelsClearGlobalOverride
    )
}

const fn is_reasoning_effort_action(action: &Action) -> bool {
    matches!(
        action,
        Action::ReasoningEffortCurrent { .. }
            | Action::ReasoningEffortSetSessionOverride { .. }
            | Action::ReasoningEffortClearSessionOverride { .. }
            | Action::ReasoningEffortSetGlobalOverride { .. }
            | Action::ReasoningEffortClearGlobalOverride
    )
}

fn owner_matches(expected: Option<&Owner>, actual: Option<&Owner>) -> bool {
    match expected {
        Some(expected) => matches!(actual, Some(actual) if actual == expected),
        None => true,
    }
}

fn session_id_matches(expected: Option<&str>, actual: &str) -> bool {
    match expected {
        Some(expected) => expected == actual,
        None => true,
    }
}

fn optional_session_id_matches(expected: Option<&str>, actual: Option<&str>) -> bool {
    expected == actual
}

fn is_task_action(action: &Action, owner: Option<&Owner>) -> bool {
    match action {
        Action::TaskCreate {
            owner: action_owner,
        }
        | Action::TaskList {
            owner: action_owner,
        }
        | Action::TaskGet {
            owner: action_owner,
            ..
        }
        | Action::TaskCancel {
            owner: action_owner,
            ..
        } => owner_matches(owner, action_owner.as_ref()),
        _ => false,
    }
}

fn is_goal_action(action: &Action, owner: Option<&Owner>) -> bool {
    match action {
        Action::GoalCreate {
            owner: action_owner,
        }
        | Action::GoalGet {
            owner: action_owner,
        }
        | Action::GoalUpdate {
            owner: action_owner,
        }
        | Action::GoalManage {
            owner: action_owner,
        } => owner_matches(owner, action_owner.as_ref()),
        _ => false,
    }
}

const fn is_calendar_action(action: &Action) -> bool {
    matches!(
        action,
        Action::CalendarHolidaysList
            | Action::CalendarHolidaysNext
            | Action::CalendarHolidayCheck
            | Action::CalendarDaysBetween
            | Action::CalendarDateArith
            | Action::CalendarWeekdayInfo
    )
}

const fn is_cron_action(action: &Action) -> bool {
    matches!(
        action,
        Action::CronCreate { .. }
            | Action::CronGet { .. }
            | Action::CronList { .. }
            | Action::CronUpdate { .. }
            | Action::CronDelete { .. }
    )
}
