use crate::owner::Owner;

use super::StrictPolicy;
use super::matcher::ActionMatcher;
use super::rule::Rule;

pub struct StrictPolicyBuilder {
    pub(super) rules: Vec<Rule>,
    pub(super) default_allow: bool,
}

impl StrictPolicyBuilder {
    /// Push a fully-specified rule. Use this when the convenience
    /// methods aren't enough.
    pub fn rule(mut self, rule: Rule) -> Self {
        self.rules.push(rule);
        self
    }

    /// Allow `Action::LlmCall` unconditionally.
    pub fn allow_llm_call(self) -> Self {
        self.rule(Rule::allow(ActionMatcher::LlmCall))
    }

    /// Allow `Action::ToolCall(name)` unconditionally.
    pub fn allow_tool(self, name: impl Into<String>) -> Self {
        self.rule(Rule::allow(ActionMatcher::Tool(name.into())))
    }

    /// Allow `Action::ToolCall(name)` only when the subject has
    /// `attr_key = attr_value`.
    pub fn allow_tool_for(
        self,
        name: impl Into<String>,
        attr_key: impl Into<String>,
        attr_value: impl Into<String>,
    ) -> Self {
        self.rule(Rule::allow(ActionMatcher::Tool(name.into())).requires_attr(attr_key, attr_value))
    }

    /// Deny `Action::ToolCall(name)`. Place before any matching allow
    /// rule for short-circuit deny.
    pub fn deny_tool(self, name: impl Into<String>) -> Self {
        self.rule(Rule::deny(ActionMatcher::Tool(name.into())))
    }

    /// Allow `Action::Custom(name)` unconditionally.
    pub fn allow_custom(self, name: impl Into<String>) -> Self {
        self.rule(Rule::allow(ActionMatcher::Custom(name.into())))
    }

    /// Allow `Action::MemorySearch { .. }` when scope is within subject.
    pub fn allow_memory_search(self) -> Self {
        self.rule(Rule::allow(ActionMatcher::MemorySearch).requires_scope_from_subject())
    }

    /// Allow `Action::MemoryStore { .. }` when scope is within subject.
    pub fn allow_memory_store(self) -> Self {
        self.rule(Rule::allow(ActionMatcher::MemoryStore).requires_scope_from_subject())
    }

    /// Allow `Action::MemoryGet { .. }` when scope is within subject.
    pub fn allow_memory_get(self) -> Self {
        self.rule(Rule::allow(ActionMatcher::MemoryGet).requires_scope_from_subject())
    }

    /// Allow `Action::MemoryDelete { .. }` when scope is within subject.
    pub fn allow_memory_delete(self) -> Self {
        self.rule(Rule::allow(ActionMatcher::MemoryDelete).requires_scope_from_subject())
    }

    /// Allow `Action::MemoryArchive { .. }` when scope is within subject.
    pub fn allow_memory_archive(self) -> Self {
        self.rule(Rule::allow(ActionMatcher::MemoryArchive).requires_scope_from_subject())
    }

    /// Allow `Action::MemoryUnarchive { .. }` when scope is within subject.
    pub fn allow_memory_unarchive(self) -> Self {
        self.rule(Rule::allow(ActionMatcher::MemoryUnarchive).requires_scope_from_subject())
    }

    /// Allow `Action::MemoryExtendExpiry { .. }` when scope is within subject.
    pub fn allow_memory_extend_expiry(self) -> Self {
        self.rule(Rule::allow(ActionMatcher::MemoryExtendExpiry).requires_scope_from_subject())
    }

    /// Allow memory tool variants when scope is within subject.
    pub fn allow_memory_any(self) -> Self {
        self.rule(Rule::allow(ActionMatcher::MemoryAny).requires_scope_from_subject())
    }

    /// Allow `Action::MemoryConsolidate { .. }` when scope is within subject.
    pub fn allow_memory_consolidate(self) -> Self {
        self.rule(Rule::allow(ActionMatcher::MemoryConsolidate).requires_scope_from_subject())
    }

    /// Allow `Action::RelationStore { .. }` when the edge scope is within
    /// subject. The linked documents may belong to other owners; only the
    /// edge's own scope is gated, which is how cross-owner relations stay
    /// writable while the writer still owns the edge.
    pub fn allow_relation_store(self) -> Self {
        self.rule(Rule::allow(ActionMatcher::RelationStore).requires_scope_from_subject())
    }

    /// Allow `Action::RelationDelete { .. }` when scope is within subject.
    pub fn allow_relation_delete(self) -> Self {
        self.rule(Rule::allow(ActionMatcher::RelationDelete).requires_scope_from_subject())
    }

    /// Allow `Action::RelationExpand { .. }` when scope is within subject.
    pub fn allow_relation_expand(self) -> Self {
        self.rule(Rule::allow(ActionMatcher::RelationExpand).requires_scope_from_subject())
    }

    /// Allow every memory-relation action when scope is within subject.
    pub fn allow_relation_any(self) -> Self {
        self.rule(Rule::allow(ActionMatcher::RelationAny).requires_scope_from_subject())
    }

    /// Allow `Action::SessionSearch { .. }` when scope is within subject.
    pub fn allow_session_search(self) -> Self {
        self.rule(Rule::allow(ActionMatcher::SessionSearch).requires_scope_from_subject())
    }

    /// Allow `Action::ModelList`.
    pub fn allow_model_list(self) -> Self {
        self.rule(Rule::allow(ActionMatcher::ModelList))
    }

    /// Allow `Action::ModelGet { .. }`.
    pub fn allow_model_get(self) -> Self {
        self.rule(Rule::allow(ActionMatcher::ModelGet))
    }

    /// Allow `Action::ModelsCurrent { session_id: None }`.
    pub fn allow_models_current(self) -> Self {
        self.rule(Rule::allow(ActionMatcher::ModelsCurrent {
            session_id: None,
        }))
    }

    /// Allow `Action::ModelsCurrent { .. }` for one target session.
    pub fn allow_models_current_for_session(self, session_id: impl Into<String>) -> Self {
        self.rule(Rule::allow(ActionMatcher::ModelsCurrent {
            session_id: Some(session_id.into()),
        }))
    }

    /// Allow `Action::ModelsSetSessionOverride { .. }` for one target session.
    pub fn allow_models_set_session_override_for_session(
        self,
        session_id: impl Into<String>,
    ) -> Self {
        self.rule(Rule::allow(ActionMatcher::ModelsSetSessionOverride {
            session_id: Some(session_id.into()),
        }))
    }

    /// Allow `Action::ModelsClearSessionOverride { .. }` for one target session.
    pub fn allow_models_clear_session_override_for_session(
        self,
        session_id: impl Into<String>,
    ) -> Self {
        self.rule(Rule::allow(ActionMatcher::ModelsClearSessionOverride {
            session_id: Some(session_id.into()),
        }))
    }

    /// Allow `Action::ModelsSetGlobalOverride { .. }`.
    pub fn allow_models_set_global_override(self) -> Self {
        self.rule(Rule::allow(ActionMatcher::ModelsSetGlobalOverride))
    }

    /// Allow `Action::ModelsClearGlobalOverride`.
    pub fn allow_models_clear_global_override(self) -> Self {
        self.rule(Rule::allow(ActionMatcher::ModelsClearGlobalOverride))
    }

    /// Allow `Action::ReasoningEffortCurrent { session_id: None }`.
    pub fn allow_reasoning_effort_current(self) -> Self {
        self.rule(Rule::allow(ActionMatcher::ReasoningEffortCurrent {
            session_id: None,
        }))
    }

    /// Allow `Action::ReasoningEffortCurrent { .. }` for one target session.
    pub fn allow_reasoning_effort_current_for_session(self, session_id: impl Into<String>) -> Self {
        self.rule(Rule::allow(ActionMatcher::ReasoningEffortCurrent {
            session_id: Some(session_id.into()),
        }))
    }

    /// Allow `Action::ReasoningEffortSetSessionOverride { .. }` for one
    /// target session.
    pub fn allow_reasoning_effort_set_session_override_for_session(
        self,
        session_id: impl Into<String>,
    ) -> Self {
        self.rule(Rule::allow(
            ActionMatcher::ReasoningEffortSetSessionOverride {
                session_id: Some(session_id.into()),
            },
        ))
    }

    /// Allow `Action::ReasoningEffortClearSessionOverride { .. }` for one
    /// target session.
    pub fn allow_reasoning_effort_clear_session_override_for_session(
        self,
        session_id: impl Into<String>,
    ) -> Self {
        self.rule(Rule::allow(
            ActionMatcher::ReasoningEffortClearSessionOverride {
                session_id: Some(session_id.into()),
            },
        ))
    }

    /// Allow `Action::ReasoningEffortSetGlobalOverride { .. }`.
    pub fn allow_reasoning_effort_set_global_override(self) -> Self {
        self.rule(Rule::allow(ActionMatcher::ReasoningEffortSetGlobalOverride))
    }

    /// Allow `Action::ReasoningEffortClearGlobalOverride`.
    pub fn allow_reasoning_effort_clear_global_override(self) -> Self {
        self.rule(Rule::allow(
            ActionMatcher::ReasoningEffortClearGlobalOverride,
        ))
    }

    /// Allow `Action::TaskCreate { .. }` for any owner.
    pub fn allow_task_create(self) -> Self {
        self.rule(Rule::allow(ActionMatcher::TaskCreate { owner: None }))
    }

    /// Allow `Action::TaskList { .. }` for any owner.
    pub fn allow_task_list(self) -> Self {
        self.rule(Rule::allow(ActionMatcher::TaskList { owner: None }))
    }

    /// Allow `Action::TaskGet { .. }` for any owner.
    pub fn allow_task_get(self) -> Self {
        self.rule(Rule::allow(ActionMatcher::TaskGet { owner: None }))
    }

    /// Allow `Action::TaskCancel { .. }` for any owner.
    pub fn allow_task_cancel(self) -> Self {
        self.rule(Rule::allow(ActionMatcher::TaskCancel { owner: None }))
    }

    /// Allow every task action for any owner.
    pub fn allow_task_any(self) -> Self {
        self.rule(Rule::allow(ActionMatcher::TaskAny { owner: None }))
    }

    /// Allow every task action for one exact owner.
    pub fn allow_task_any_for_owner(self, owner: impl Into<Owner>) -> Self {
        self.rule(Rule::allow(ActionMatcher::TaskAny {
            owner: Some(owner.into()),
        }))
    }

    /// Allow `Action::CalendarHolidaysList`.
    pub fn allow_calendar_holidays_list(self) -> Self {
        self.rule(Rule::allow(ActionMatcher::CalendarHolidaysList))
    }

    /// Allow `Action::CalendarHolidaysNext`.
    pub fn allow_calendar_holidays_next(self) -> Self {
        self.rule(Rule::allow(ActionMatcher::CalendarHolidaysNext))
    }

    /// Allow `Action::CalendarHolidayCheck`.
    pub fn allow_calendar_holiday_check(self) -> Self {
        self.rule(Rule::allow(ActionMatcher::CalendarHolidayCheck))
    }

    /// Allow `Action::CalendarDaysBetween`.
    pub fn allow_calendar_days_between(self) -> Self {
        self.rule(Rule::allow(ActionMatcher::CalendarDaysBetween))
    }

    /// Allow `Action::CalendarDateArith`.
    pub fn allow_calendar_date_arith(self) -> Self {
        self.rule(Rule::allow(ActionMatcher::CalendarDateArith))
    }

    /// Allow `Action::CalendarWeekdayInfo`.
    pub fn allow_calendar_weekday_info(self) -> Self {
        self.rule(Rule::allow(ActionMatcher::CalendarWeekdayInfo))
    }

    /// Allow every calendar action.
    pub fn allow_calendar_any(self) -> Self {
        self.rule(Rule::allow(ActionMatcher::CalendarAny))
    }

    /// Allow `Action::CronCreate { .. }` when scope is within subject.
    pub fn allow_cron_create(self) -> Self {
        self.rule(Rule::allow(ActionMatcher::CronCreate).requires_scope_from_subject())
    }

    /// Allow `Action::CronGet { .. }` when scope is within subject.
    pub fn allow_cron_get(self) -> Self {
        self.rule(Rule::allow(ActionMatcher::CronGet).requires_scope_from_subject())
    }

    /// Allow `Action::CronList { .. }` when scope is within subject.
    pub fn allow_cron_list(self) -> Self {
        self.rule(Rule::allow(ActionMatcher::CronList).requires_scope_from_subject())
    }

    /// Allow `Action::CronUpdate { .. }` when scope is within subject.
    pub fn allow_cron_update(self) -> Self {
        self.rule(Rule::allow(ActionMatcher::CronUpdate).requires_scope_from_subject())
    }

    /// Allow `Action::CronDelete { .. }` when scope is within subject.
    pub fn allow_cron_delete(self) -> Self {
        self.rule(Rule::allow(ActionMatcher::CronDelete).requires_scope_from_subject())
    }

    /// Allow every cron action when scope is within subject.
    pub fn allow_cron_any(self) -> Self {
        self.rule(Rule::allow(ActionMatcher::CronAny).requires_scope_from_subject())
    }

    /// Allow `Action::HostedWebSearch` for an optional provider filter.
    ///
    /// When `provider` is `None` the rule allows any provider. Pass
    /// `Some("anthropic")` to restrict to one specific provider.
    pub fn allow_hosted_web_search(self, provider: Option<&str>) -> Self {
        self.rule(Rule::allow(ActionMatcher::HostedWebSearch {
            provider: provider.map(str::to_owned),
        }))
    }

    /// Allow `Action::AudioRetain { .. }` when scope is within subject.
    ///
    /// Gates fail-closed persistence of a subject's raw voice to disk. Without
    /// this rule (or `allow_by_default`) retention is denied.
    pub fn allow_audio_retain(self) -> Self {
        self.rule(Rule::allow(ActionMatcher::AudioRetain).requires_scope_from_subject())
    }

    /// Allow `Action::GoalCreate { .. }` for any owner, unconditionally.
    ///
    /// Prefer [`Self::allow_goal_create_for`] in fail-closed deployments so
    /// the model cannot create goals on its own; only a trusted path that
    /// stamps the required origin attr may.
    pub fn allow_goal_create(self) -> Self {
        self.rule(Rule::allow(ActionMatcher::GoalCreate { owner: None }))
    }

    /// Allow `Action::GoalCreate { .. }` only when the subject carries
    /// `attr_key = attr_value`. Trusted paths (a `/goal` command, an operator)
    /// stamp that attr; a model-initiated create lacks it and is denied.
    pub fn allow_goal_create_for(
        self,
        attr_key: impl Into<String>,
        attr_value: impl Into<String>,
    ) -> Self {
        self.rule(
            Rule::allow(ActionMatcher::GoalCreate { owner: None })
                .requires_attr(attr_key, attr_value),
        )
    }

    /// Allow `Action::GoalGet { .. }` for any owner.
    pub fn allow_goal_get(self) -> Self {
        self.rule(Rule::allow(ActionMatcher::GoalGet { owner: None }))
    }

    /// Allow `Action::GoalUpdate { .. }` for any owner.
    pub fn allow_goal_update(self) -> Self {
        self.rule(Rule::allow(ActionMatcher::GoalUpdate { owner: None }))
    }

    /// Allow `Action::GoalManage { .. }` for any owner. Host-only goal control
    /// (pause/resume/clear); the model tool surface never raises this action.
    pub fn allow_goal_manage(self) -> Self {
        self.rule(Rule::allow(ActionMatcher::GoalManage { owner: None }))
    }

    /// Allow every goal action for any owner.
    pub fn allow_goal_any(self) -> Self {
        self.rule(Rule::allow(ActionMatcher::GoalAny { owner: None }))
    }

    /// Allow every goal action for one exact owner.
    pub fn allow_goal_any_for_owner(self, owner: impl Into<Owner>) -> Self {
        self.rule(Rule::allow(ActionMatcher::GoalAny {
            owner: Some(owner.into()),
        }))
    }

    /// Default action when no rule matches: `Allow`.
    pub const fn allow_by_default(mut self) -> Self {
        self.default_allow = true;
        self
    }

    /// Default action when no rule matches: `Deny`. (This is the
    /// constructor default; calling it is for documentation only.)
    pub const fn deny_by_default(mut self) -> Self {
        self.default_allow = false;
        self
    }

    pub fn build(self) -> StrictPolicy {
        StrictPolicy {
            rules: self.rules,
            default_allow: self.default_allow,
        }
    }
}
