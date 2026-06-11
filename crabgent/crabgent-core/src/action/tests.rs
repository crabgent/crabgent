use super::*;

#[test]
fn name_for_llm_call_is_constant() {
    assert_eq!(Action::LlmCall.name(), "llm.call");
}

#[test]
fn name_for_tool_call_borrowed() {
    let a = Action::tool("bash");
    assert_eq!(a.name(), "bash");
}

#[test]
fn name_for_tool_call_owned() {
    let owned = String::from("read_file");
    let a = Action::tool(owned);
    assert_eq!(a.name(), "read_file");
}

#[test]
fn name_for_custom() {
    let a = Action::custom("memory.read");
    assert_eq!(a.name(), "memory.read");
}

#[test]
fn name_for_goal_actions() {
    use crate::Owner;
    let owner = Some(Owner::new("u"));
    assert_eq!(
        Action::GoalCreate {
            owner: owner.clone()
        }
        .name(),
        "goal.create"
    );
    assert_eq!(
        Action::GoalGet {
            owner: owner.clone()
        }
        .name(),
        "goal.get"
    );
    assert_eq!(
        Action::GoalUpdate {
            owner: owner.clone()
        }
        .name(),
        "goal.update"
    );
    assert_eq!(Action::GoalManage { owner }.name(), "goal.manage");
}

#[test]
fn goal_actions_carry_no_memory_scope() {
    use crate::Owner;
    assert!(
        Action::GoalCreate {
            owner: Some(Owner::new("u"))
        }
        .scope()
        .is_none()
    );
    assert!(Action::GoalManage { owner: None }.scope().is_none());
}

#[test]
fn equality_holds() {
    assert_eq!(Action::tool("x"), Action::tool("x"));
    assert_ne!(Action::tool("x"), Action::tool("y"));
    assert_ne!(Action::tool("x"), Action::custom("x"));
}

#[test]
fn clone_preserves_name() {
    let a = Action::tool("write_file");
    let b = a.clone();
    assert_eq!(a.name(), b.name());
}

#[test]
fn memory_search_action_has_dotted_name() {
    let a = Action::MemorySearch {
        query: "hi".into(),
        scope: MemoryScope::global(),
    };
    assert_eq!(a.name(), "memory.search");
}

#[test]
fn memory_store_action_has_dotted_name() {
    let a = Action::MemoryStore {
        scope: MemoryScope::global(),
    };
    assert_eq!(a.name(), "memory.store");
}

#[test]
fn memory_get_and_delete_actions_have_dotted_names() {
    let id = MemoryId::new();
    let scope = MemoryScope::global();
    assert_eq!(
        Action::MemoryGet {
            id: id.clone(),
            scope: scope.clone(),
        }
        .name(),
        "memory.get"
    );
    assert_eq!(Action::MemoryDelete { id, scope }.name(), "memory.delete");
}

#[test]
fn memory_lifecycle_actions_have_dotted_names() {
    let id = MemoryId::new();
    let scope = MemoryScope::global();
    assert_eq!(
        Action::MemoryArchive {
            id: id.clone(),
            scope: scope.clone(),
        }
        .name(),
        "memory.archive"
    );
    assert_eq!(
        Action::MemoryUnarchive {
            id: id.clone(),
            scope: scope.clone(),
        }
        .name(),
        "memory.unarchive"
    );
    assert_eq!(
        Action::MemoryExtendExpiry { id, scope }.name(),
        "memory.extend_expiry"
    );
}

#[test]
fn action_memory_consolidate_name() {
    let action = Action::MemoryConsolidate {
        scope: MemoryScope::global(),
    };
    assert_eq!(action.name(), "memory.consolidate");
}

#[test]
fn action_memory_consolidate_scope_returns_some() {
    let scope = MemoryScope::for_owner(Owner::new("alice")).with_kind("direct");
    let action = Action::MemoryConsolidate {
        scope: scope.clone(),
    };
    assert_eq!(action.scope(), Some(&scope));
}

#[test]
fn relation_action_names_are_dotted() {
    let scope = MemoryScope::global();
    assert_eq!(
        Action::RelationStore {
            scope: scope.clone()
        }
        .name(),
        "memory.relation_store"
    );
    assert_eq!(
        Action::RelationDelete {
            scope: scope.clone()
        }
        .name(),
        "memory.relation_delete"
    );
    assert_eq!(
        Action::RelationExpand { scope }.name(),
        "memory.relation_expand"
    );
}

#[test]
fn relation_actions_borrow_scope_returns_some() {
    let scope = MemoryScope::for_owner(Owner::new("alice"));
    for action in [
        Action::RelationStore {
            scope: scope.clone(),
        },
        Action::RelationDelete {
            scope: scope.clone(),
        },
        Action::RelationExpand {
            scope: scope.clone(),
        },
    ] {
        assert_eq!(action.scope(), Some(&scope));
    }
}

#[test]
fn session_search_action_has_dotted_name() {
    let a = Action::SessionSearch {
        query: "hi".into(),
        scope: MemoryScope::global(),
    };
    assert_eq!(a.name(), "session.search");
}

#[test]
fn cron_actions_have_dotted_names() {
    let scope = MemoryScope::for_owner(Owner::new("alice"));
    assert_eq!(
        Action::CronCreate {
            scope: scope.clone()
        }
        .name(),
        "cron.create"
    );
    assert_eq!(
        Action::CronGet {
            id: "job-1".into(),
            scope: scope.clone(),
        }
        .name(),
        "cron.get"
    );
    assert_eq!(
        Action::CronList {
            scope: scope.clone()
        }
        .name(),
        "cron.list"
    );
    assert_eq!(
        Action::CronUpdate {
            id: "job-1".into(),
            scope: scope.clone(),
        }
        .name(),
        "cron.update"
    );
    assert_eq!(
        Action::CronDelete {
            id: "job-1".into(),
            scope,
        }
        .name(),
        "cron.delete"
    );
}

#[test]
fn task_actions_have_dotted_names() {
    let owner = Some(Owner::new("alice"));
    assert_eq!(
        Action::TaskCreate {
            owner: owner.clone()
        }
        .name(),
        "task.create"
    );
    assert_eq!(
        Action::TaskList {
            owner: owner.clone()
        }
        .name(),
        "task.list"
    );
    assert_eq!(
        Action::TaskGet {
            id: "task-1".into(),
            owner: owner.clone(),
        }
        .name(),
        "task.get"
    );
    let action = Action::TaskCancel {
        id: "task-1".into(),
        owner,
    };
    assert_eq!(action.name(), "task.cancel");
    assert_eq!(action.scope(), None);
}

#[test]
fn model_list_action_has_dotted_name() {
    assert_eq!(Action::ModelList.name(), "models.list");
}

#[test]
fn model_get_action_has_dotted_name() {
    let action = Action::ModelGet {
        id: ModelId::new("claude"),
    };

    assert_eq!(action.name(), "models.get");
}

#[test]
fn model_override_actions_have_dotted_names() {
    let cases = [
        (
            Action::ModelsCurrent {
                session_id: Some("session-1".into()),
            },
            "models.current",
        ),
        (
            Action::ModelsSetSessionOverride {
                session_id: "session-1".into(),
                model: ModelId::new("claude"),
            },
            "models.set_session_override",
        ),
        (
            Action::ModelsClearSessionOverride {
                session_id: "session-1".into(),
            },
            "models.clear_session_override",
        ),
        (
            Action::ModelsSetGlobalOverride {
                model: ModelId::new("claude"),
            },
            "models.set_global_override",
        ),
        (
            Action::ModelsClearGlobalOverride,
            "models.clear_global_override",
        ),
        (
            Action::ReasoningEffortCurrent {
                session_id: Some("session-1".into()),
            },
            "models.current_effort",
        ),
        (
            Action::ReasoningEffortSetSessionOverride {
                session_id: "session-1".into(),
                effort: ReasoningEffort::High,
            },
            "models.set_session_effort_override",
        ),
        (
            Action::ReasoningEffortClearSessionOverride {
                session_id: "session-1".into(),
            },
            "models.clear_session_effort_override",
        ),
        (
            Action::ReasoningEffortSetGlobalOverride {
                effort: ReasoningEffort::Low,
            },
            "models.set_global_effort_override",
        ),
        (
            Action::ReasoningEffortClearGlobalOverride,
            "models.clear_global_effort_override",
        ),
    ];

    for (action, name) in cases {
        assert_eq!(action.name(), name);
        assert_eq!(action.scope(), None);
    }
}

#[test]
fn model_actions_borrow_scope_returns_none() {
    let get = Action::ModelGet {
        id: ModelId::new("claude"),
    };
    let current = Action::ModelsCurrent { session_id: None };

    assert_eq!(Action::ModelList.scope(), None);
    assert_eq!(get.scope(), None);
    assert_eq!(current.scope(), None);
}

#[test]
fn action_name_for_calendar_variants() {
    let cases = [
        (Action::CalendarHolidaysList, "calendar.holidays_list"),
        (Action::CalendarHolidaysNext, "calendar.holidays_next"),
        (Action::CalendarHolidayCheck, "calendar.holiday_check"),
        (Action::CalendarDaysBetween, "calendar.days_between"),
        (Action::CalendarDateArith, "calendar.date_arith"),
        (Action::CalendarWeekdayInfo, "calendar.weekday_info"),
    ];

    for (action, name) in cases {
        assert_eq!(action.name(), name);
    }
}

#[test]
fn action_scope_for_calendar_variants_is_none() {
    let cases = [
        Action::CalendarHolidaysList,
        Action::CalendarHolidaysNext,
        Action::CalendarHolidayCheck,
        Action::CalendarDaysBetween,
        Action::CalendarDateArith,
        Action::CalendarWeekdayInfo,
    ];

    for action in cases {
        assert_eq!(action.scope(), None);
    }
}

#[test]
fn targeted_action_has_name_and_no_scope() {
    let conv = Owner::new("slack:T1/C1");
    let action = Action::targeted(
        "channel.send",
        ActionTarget::new(conv.clone()).with_qualifier("slack"),
    );

    assert_eq!(action.name(), "channel.send");
    assert_eq!(action.scope(), None);
    let Action::Targeted { target, .. } = action else {
        panic!("expected targeted action");
    };
    assert_eq!(target.owner(), &conv);
    assert_eq!(target.qualifier(), Some("slack"));
}

#[test]
fn memory_search_actions_compare_by_query_and_scope() {
    let a = Action::MemorySearch {
        query: "foo".into(),
        scope: MemoryScope::global(),
    };
    let b = Action::MemorySearch {
        query: "foo".into(),
        scope: MemoryScope::global(),
    };
    let c = Action::MemorySearch {
        query: "bar".into(),
        scope: MemoryScope::global(),
    };
    assert_eq!(a, b);
    assert_ne!(a, c);
}

#[test]
fn scope_borrows_memory_and_session_scopes() {
    let scope = MemoryScope::global();
    let action = Action::MemoryGet {
        id: MemoryId::new(),
        scope: scope.clone(),
    };
    assert_eq!(action.scope(), Some(&scope));
    assert_eq!(Action::tool("x").scope(), None);
}

#[test]
fn action_scope_returns_some_for_cron_variants() {
    let scope = MemoryScope::for_owner(Owner::new("alice")).with_kind("direct");
    let cases = [
        Action::CronCreate {
            scope: scope.clone(),
        },
        Action::CronGet {
            id: "job-1".into(),
            scope: scope.clone(),
        },
        Action::CronList {
            scope: scope.clone(),
        },
        Action::CronUpdate {
            id: "job-1".into(),
            scope: scope.clone(),
        },
        Action::CronDelete {
            id: "job-1".into(),
            scope: scope.clone(),
        },
    ];

    for action in cases {
        assert_eq!(action.scope(), Some(&scope));
    }
}

#[test]
fn name_for_audio_retain_is_constant() {
    let a = Action::AudioRetain {
        scope: MemoryScope::global(),
    };
    assert_eq!(a.name(), "audio.retain");
}

#[test]
fn audio_retain_exposes_scope() {
    let scope = MemoryScope::for_owner(Owner::new("alice"));
    let a = Action::AudioRetain {
        scope: scope.clone(),
    };
    assert_eq!(a.scope(), Some(&scope));
}
