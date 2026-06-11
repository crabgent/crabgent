//! Strict policy coverage for typed action families outside memory/session.

use super::strict::StrictPolicy;
use super::{PolicyDecision, PolicyHook};
use crate::{Action, MemoryScope, ModelId, Owner, ReasoningEffort, Subject};

fn cron_scope() -> MemoryScope {
    MemoryScope::for_owner(Owner::new("u"))
}

fn task_owner() -> Owner {
    Owner::new("u")
}

#[tokio::test]
async fn exact_session_model_override_matchers_only_cover_target_session() {
    let policy = StrictPolicy::builder()
        .allow_models_set_session_override_for_session("session-1")
        .allow_models_clear_session_override_for_session("session-1")
        .build();
    let subject = Subject::new("u");

    for action in [
        Action::ModelsSetSessionOverride {
            session_id: "session-1".into(),
            model: ModelId::new("m"),
        },
        Action::ModelsClearSessionOverride {
            session_id: "session-1".into(),
        },
    ] {
        assert!(matches!(
            policy.allow(&subject, &action).await,
            PolicyDecision::Allow
        ));
    }

    for action in [
        Action::ModelsSetSessionOverride {
            session_id: "session-2".into(),
            model: ModelId::new("m"),
        },
        Action::ModelsClearSessionOverride {
            session_id: "session-2".into(),
        },
    ] {
        assert!(matches!(
            policy.allow(&subject, &action).await,
            PolicyDecision::Deny(_)
        ));
    }
}

#[tokio::test]
async fn granular_model_matchers_cover_read_and_global_ops() {
    let policy = StrictPolicy::builder()
        .allow_model_list()
        .allow_model_get()
        .allow_models_current()
        .allow_models_current_for_session("session-1")
        .allow_models_set_global_override()
        .allow_models_clear_global_override()
        .build();
    let subject = Subject::new("u");
    for action in [
        Action::ModelList,
        Action::ModelGet {
            id: ModelId::new("m"),
        },
        Action::ModelsCurrent { session_id: None },
        Action::ModelsCurrent {
            session_id: Some("session-1".into()),
        },
        Action::ModelsSetGlobalOverride {
            model: ModelId::new("m"),
        },
        Action::ModelsClearGlobalOverride,
    ] {
        assert!(matches!(
            policy.allow(&subject, &action).await,
            PolicyDecision::Allow
        ));
    }
}

#[tokio::test]
async fn exact_session_model_current_matcher_only_covers_target_session() {
    let policy = StrictPolicy::builder()
        .allow_models_current_for_session("session-1")
        .build();
    let subject = Subject::new("u");

    assert!(matches!(
        policy
            .allow(
                &subject,
                &Action::ModelsCurrent {
                    session_id: Some("session-1".into()),
                },
            )
            .await,
        PolicyDecision::Allow
    ));
    assert!(matches!(
        policy
            .allow(
                &subject,
                &Action::ModelsCurrent {
                    session_id: Some("session-2".into()),
                },
            )
            .await,
        PolicyDecision::Deny(_)
    ));
    assert!(matches!(
        policy
            .allow(&subject, &Action::ModelsCurrent { session_id: None })
            .await,
        PolicyDecision::Deny(_)
    ));
}

#[tokio::test]
async fn granular_model_matchers_do_not_grant_other_model_ops() {
    let policy = StrictPolicy::builder().allow_model_list().build();
    let subject = Subject::new("u");

    assert!(matches!(
        policy.allow(&subject, &Action::ModelList).await,
        PolicyDecision::Allow
    ));
    assert!(matches!(
        policy
            .allow(
                &subject,
                &Action::ModelGet {
                    id: ModelId::new("m"),
                },
            )
            .await,
        PolicyDecision::Deny(_)
    ));
}

#[tokio::test]
async fn exact_session_reasoning_effort_matchers_only_cover_target_session() {
    let policy = StrictPolicy::builder()
        .allow_reasoning_effort_set_session_override_for_session("session-1")
        .allow_reasoning_effort_clear_session_override_for_session("session-1")
        .build();
    let subject = Subject::new("u");

    for action in [
        Action::ReasoningEffortSetSessionOverride {
            session_id: "session-1".into(),
            effort: ReasoningEffort::High,
        },
        Action::ReasoningEffortClearSessionOverride {
            session_id: "session-1".into(),
        },
    ] {
        assert!(matches!(
            policy.allow(&subject, &action).await,
            PolicyDecision::Allow
        ));
    }

    for action in [
        Action::ReasoningEffortSetSessionOverride {
            session_id: "session-2".into(),
            effort: ReasoningEffort::High,
        },
        Action::ReasoningEffortClearSessionOverride {
            session_id: "session-2".into(),
        },
    ] {
        assert!(matches!(
            policy.allow(&subject, &action).await,
            PolicyDecision::Deny(_)
        ));
    }
}

#[tokio::test]
async fn granular_reasoning_effort_matchers_cover_read_and_global_ops() {
    let policy = StrictPolicy::builder()
        .allow_reasoning_effort_current()
        .allow_reasoning_effort_current_for_session("session-1")
        .allow_reasoning_effort_set_global_override()
        .allow_reasoning_effort_clear_global_override()
        .build();
    let subject = Subject::new("u");
    for action in [
        Action::ReasoningEffortCurrent { session_id: None },
        Action::ReasoningEffortCurrent {
            session_id: Some("session-1".into()),
        },
        Action::ReasoningEffortSetGlobalOverride {
            effort: ReasoningEffort::Medium,
        },
        Action::ReasoningEffortClearGlobalOverride,
    ] {
        assert!(matches!(
            policy.allow(&subject, &action).await,
            PolicyDecision::Allow
        ));
    }
}

#[tokio::test]
async fn allow_task_any_can_be_scoped_to_owner() {
    let policy = StrictPolicy::builder()
        .allow_task_any_for_owner(task_owner())
        .build();
    let subject = Subject::new("u");

    assert!(matches!(
        policy
            .allow(
                &subject,
                &Action::TaskCreate {
                    owner: Some(task_owner()),
                },
            )
            .await,
        PolicyDecision::Allow
    ));
    assert!(matches!(
        policy
            .allow(
                &subject,
                &Action::TaskList {
                    owner: Some(Owner::new("other")),
                },
            )
            .await,
        PolicyDecision::Deny(_)
    ));
}

#[tokio::test]
async fn granular_task_matchers_do_not_grant_other_task_ops() {
    let policy = StrictPolicy::builder().allow_task_create().build();
    let subject = Subject::new("u");

    assert!(matches!(
        policy
            .allow(
                &subject,
                &Action::TaskCreate {
                    owner: Some(task_owner()),
                },
            )
            .await,
        PolicyDecision::Allow
    ));
    assert!(matches!(
        policy
            .allow(
                &subject,
                &Action::TaskCancel {
                    id: "task-1".into(),
                    owner: Some(task_owner()),
                },
            )
            .await,
        PolicyDecision::Deny(_)
    ));
}

#[tokio::test]
async fn allow_calendar_any_covers_calendar_variants() {
    let policy = StrictPolicy::builder().allow_calendar_any().build();
    let subject = Subject::new("u");
    for action in [
        Action::CalendarHolidaysList,
        Action::CalendarHolidaysNext,
        Action::CalendarHolidayCheck,
        Action::CalendarDaysBetween,
        Action::CalendarDateArith,
        Action::CalendarWeekdayInfo,
    ] {
        assert!(matches!(
            policy.allow(&subject, &action).await,
            PolicyDecision::Allow
        ));
    }
}

#[tokio::test]
async fn granular_calendar_matchers_do_not_grant_other_calendar_ops() {
    let policy = StrictPolicy::builder()
        .allow_calendar_holidays_list()
        .build();
    let subject = Subject::new("u");

    assert!(matches!(
        policy.allow(&subject, &Action::CalendarHolidaysList).await,
        PolicyDecision::Allow
    ));
    assert!(matches!(
        policy.allow(&subject, &Action::CalendarDaysBetween).await,
        PolicyDecision::Deny(_)
    ));
}

#[tokio::test]
async fn allow_cron_any_covers_cron_variants_with_subject_scope() {
    let policy = StrictPolicy::builder().allow_cron_any().build();
    let subject = Subject::new("u");
    for action in [
        Action::CronCreate {
            scope: cron_scope(),
        },
        Action::CronGet {
            id: "job-1".into(),
            scope: cron_scope(),
        },
        Action::CronList {
            scope: cron_scope(),
        },
        Action::CronUpdate {
            id: "job-1".into(),
            scope: cron_scope(),
        },
        Action::CronDelete {
            id: "job-1".into(),
            scope: cron_scope(),
        },
    ] {
        assert!(matches!(
            policy.allow(&subject, &action).await,
            PolicyDecision::Allow
        ));
    }
}

#[tokio::test]
async fn cron_matchers_require_subject_scope() {
    let policy = StrictPolicy::builder().allow_cron_list().build();
    let subject = Subject::new("alice");

    assert!(matches!(
        policy
            .allow(
                &subject,
                &Action::CronList {
                    scope: MemoryScope::for_owner(Owner::new("alice")),
                },
            )
            .await,
        PolicyDecision::Allow
    ));
    assert!(matches!(
        policy
            .allow(
                &subject,
                &Action::CronList {
                    scope: MemoryScope::for_owner(Owner::new("bob")),
                },
            )
            .await,
        PolicyDecision::Deny(_)
    ));
}

#[tokio::test]
async fn allow_goal_any_covers_every_goal_variant() {
    let policy = StrictPolicy::builder().allow_goal_any().build();
    let subject = Subject::new("u");
    for action in [
        Action::GoalCreate { owner: None },
        Action::GoalGet { owner: None },
        Action::GoalUpdate { owner: None },
        Action::GoalManage { owner: None },
    ] {
        assert!(matches!(
            policy.allow(&subject, &action).await,
            PolicyDecision::Allow
        ));
    }
}

#[tokio::test]
async fn goal_create_origin_gate_denies_model_allows_stamped_subject() {
    // Fail-closed: GoalCreate is only allowed when the subject carries the
    // trusted origin attr. A model-initiated create lacks it; the host path
    // (e.g. a /goal command) stamps it.
    let policy = StrictPolicy::builder()
        .allow_goal_create_for("goal_origin", "user")
        .allow_goal_get()
        .allow_goal_update()
        .build();

    let model_subject = Subject::new("agent");
    assert!(matches!(
        policy
            .allow(&model_subject, &Action::GoalCreate { owner: None })
            .await,
        PolicyDecision::Deny(_)
    ));

    let host_subject = Subject::new("alice").with_attr("goal_origin", "user");
    assert!(matches!(
        policy
            .allow(&host_subject, &Action::GoalCreate { owner: None })
            .await,
        PolicyDecision::Allow
    ));

    // get/update are allowed for the model; manage (host-only) is not granted.
    assert!(matches!(
        policy
            .allow(&model_subject, &Action::GoalGet { owner: None })
            .await,
        PolicyDecision::Allow
    ));
    assert!(matches!(
        policy
            .allow(&model_subject, &Action::GoalManage { owner: None })
            .await,
        PolicyDecision::Deny(_)
    ));
}

#[tokio::test]
async fn allow_goal_any_can_be_scoped_to_owner() {
    let policy = StrictPolicy::builder()
        .allow_goal_any_for_owner(Owner::new("alice"))
        .build();
    let subject = Subject::new("alice");

    assert!(matches!(
        policy
            .allow(
                &subject,
                &Action::GoalUpdate {
                    owner: Some(Owner::new("alice")),
                },
            )
            .await,
        PolicyDecision::Allow
    ));
    assert!(matches!(
        policy
            .allow(
                &subject,
                &Action::GoalUpdate {
                    owner: Some(Owner::new("bob")),
                },
            )
            .await,
        PolicyDecision::Deny(_)
    ));
}
