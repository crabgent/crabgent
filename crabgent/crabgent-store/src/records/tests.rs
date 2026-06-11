use super::*;
use crabgent_core::ContentBlock;

fn session_skeleton(owner: &str, msg_count: usize) -> Session {
    let now = Utc::now();
    let messages = (0..msg_count)
        .map(|i| Message::User {
            content: vec![ContentBlock::Text {
                text: format!("turn-{i}"),
            }],
            timestamp: None,
        })
        .collect();
    Session {
        id: SessionId::new(),
        owner: Owner::new(owner),
        scope: MemoryScope::for_owner(Owner::new(owner)),
        thread: None,
        title: Some("test".into()),
        summary: None,
        compaction_summary: None,
        model_override: None,
        reasoning_effort_override: None,
        messages,
        created_at: now,
        updated_at: now,
    }
}

#[test]
fn session_info_projects_count_and_summary() {
    let s = session_skeleton("u", 3);
    let info: SessionInfo = (&s).into();
    assert_eq!(info.message_count, 3);
    assert!(!info.has_summary);
    assert_eq!(info.id, s.id);
    assert_eq!(info.owner, s.owner);
}

#[test]
fn session_info_marks_summary_when_present() {
    let mut s = session_skeleton("u", 0);
    s.summary = Some("done".into());
    let info: SessionInfo = (&s).into();
    assert!(info.has_summary);
    assert_eq!(info.message_count, 0);
}

#[test]
fn session_serde_defaults_missing_compaction_summary() {
    let session = session_skeleton("u", 1);
    let mut json = serde_json::to_value(&session).expect("serialize session");
    let object = json.as_object_mut().expect("session json object");
    object.remove("compaction_summary");

    let decoded: Session = serde_json::from_value(json).expect("deserialize session");

    assert!(decoded.compaction_summary.is_none());
}

#[test]
fn task_status_round_trip_via_string() {
    for status in [
        TaskStatus::Running,
        TaskStatus::Paused,
        TaskStatus::Done,
        TaskStatus::Failed,
    ] {
        let parsed: TaskStatus = status.as_str().parse().expect("round-trip");
        assert_eq!(parsed, status);
        // serde uses the same lowercase wire form as `as_str`.
        assert_eq!(
            serde_json::to_string(&status).expect("serialize"),
            format!("\"{}\"", status.as_str())
        );
    }
}

#[test]
fn task_pause_cause_round_trip_via_string() {
    for cause in [
        TaskPauseCause::Shutdown,
        TaskPauseCause::Forced,
        TaskPauseCause::Crash,
    ] {
        let parsed: TaskPauseCause = cause.as_str().parse().expect("round-trip");
        assert_eq!(parsed, cause);
        assert_eq!(
            serde_json::to_string(&cause).expect("serialize"),
            format!("\"{}\"", cause.as_str())
        );
    }
    "wat"
        .parse::<TaskPauseCause>()
        .expect_err("unknown cause is rejected");
}

#[test]
fn task_resume_spec_serde_round_trip_and_defaults() {
    let spec = TaskResumeSpec {
        subject_id: "alice".into(),
        subject_attrs: [("role".to_owned(), "admin".to_owned())].into(),
        model: ModelTargetDto::Provider {
            provider: "anthropic".into(),
            id: "claude-fable-5".into(),
        },
        explicit_model: None,
        session_model_override: Some("session-model".into()),
        reasoning_effort: None,
        system_prompt: Some("be terse".into()),
        max_turns: Some(40),
        tool_access: crabgent_core::ToolAccess::only(["task"]),
    };
    let json = serde_json::to_string(&spec).expect("serialize");
    let back: TaskResumeSpec = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(back, spec);

    // Optional fields default when absent on the wire.
    let minimal: TaskResumeSpec =
        serde_json::from_str(r#"{"subject_id":"u","model":"m"}"#).expect("minimal spec");
    assert_eq!(minimal.subject_id, "u");
    assert!(minimal.subject_attrs.is_empty());
    assert!(minimal.max_turns.is_none());
    assert_eq!(minimal.tool_access, crabgent_core::ToolAccess::all());
}

#[test]
fn task_status_unknown_fails_to_parse() {
    let r: Result<TaskStatus, _> = "wat".parse();
    assert!(r.is_err());
    let err = r.expect_err("expected error");
    assert!(err.to_string().contains("wat"));
}

#[test]
fn cron_schedule_every_sets_interval() {
    let s = CronSchedule::every(60);
    assert_eq!(s.interval_secs, Some(60));
    assert!(s.cron_expr.is_none());
}

#[test]
fn cron_schedule_cron_sets_expr_and_tz() {
    let s = CronSchedule::cron("0 9 * * *", Some("Europe/Berlin".into()));
    assert!(s.interval_secs.is_none());
    assert_eq!(s.cron_expr.as_deref(), Some("0 9 * * *"));
    assert_eq!(s.cron_tz.as_deref(), Some("Europe/Berlin"));
}

#[test]
fn cron_job_update_default_is_all_none() {
    let u = CronJobUpdate::default();
    assert!(u.name.is_none());
    assert!(u.prompt.is_none());
    assert!(u.schedule.is_none());
    assert!(u.enabled.is_none());
    assert!(u.delivery_ctx.is_none());
}

#[test]
fn model_target_dto_serializes_plain_and_provider_targets() {
    let plain = ModelTargetDto::Id("opus".into());
    let provider = ModelTargetDto::Provider {
        provider: "openai".into(),
        id: "opus".into(),
    };

    assert_eq!(serialize_model_target_dto(&plain), "\"opus\"");
    assert_eq!(deserialize_model_target_dto("openai/opus"), provider);
    assert_eq!(
        deserialize_model_target_dto(r#"{"provider":"openai","id":"opus"}"#),
        ModelTargetDto::Provider {
            provider: "openai".into(),
            id: "opus".into(),
        }
    );
}

#[test]
fn task_serde_round_trip() {
    let now = Utc::now();
    let t = Task {
        id: TaskId::new(),
        owner: Owner::new("u"),
        name: None,
        prompt: "hi".into(),
        status: TaskStatus::Running,
        output: String::new(),
        error: None,
        created_at: now,
        updated_at: now,
        finished_at: None,
        parent_session_id: None,
        parent_task_id: None,
        context_mode: None,
        reasoning_effort_override: None,
        resume_spec: None,
        resume_count: 0,
        pause_cause: None,
        paused_at: None,
    };
    let json = serde_json::to_string(&t).expect("serialize");
    let back: Task = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(back.id, t.id);
    assert_eq!(back.status, TaskStatus::Running);
}

#[test]
fn task_serde_defaults_missing_pause_fields() {
    // Rows serialized before pause support lack the four new fields.
    let json = serde_json::json!({
        "id": TaskId::new(),
        "owner": "u",
        "prompt": "hi",
        "status": "running",
        "output": "",
        "error": null,
        "created_at": Utc::now(),
        "updated_at": Utc::now(),
        "finished_at": null,
        "parent_session_id": null,
        "parent_task_id": null,
        "context_mode": null,
    });
    let decoded: Task = serde_json::from_value(json).expect("legacy task decodes");
    assert!(decoded.resume_spec.is_none());
    assert_eq!(decoded.resume_count, 0);
    assert!(decoded.pause_cause.is_none());
    assert!(decoded.paused_at.is_none());
}

#[test]
fn memory_doc_serde_round_trip() {
    let now = Utc::now();
    let scope = MemoryScope::for_owner(Owner::new("alice")).with_channel("slack");
    let doc = MemoryDoc {
        id: MemoryId::new(),
        scope: scope.clone(),
        body: "remember this".into(),
        class: Some("semantic".into()),
        importance: Some(0.5),
        expires_at: None,
        archived_at: None,
        embedding: Some(vec![1.0, 0.0, 0.0]),
        created_at: now,
        updated_at: now,
    };
    let json = serde_json::to_string(&doc).expect("serialize");
    let back: MemoryDoc = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(back.id, doc.id);
    assert_eq!(back.scope, scope);
    assert_eq!(back.body, doc.body);
    assert_eq!(back.class.as_deref(), Some("semantic"));
    assert_eq!(back.importance, Some(0.5));
    assert_eq!(back.embedding, Some(vec![1.0, 0.0, 0.0]));
}

#[test]
fn memory_doc_new_sets_defaults() {
    let scope = MemoryScope::for_owner(Owner::new("alice"));
    let doc = MemoryDoc::new(scope.clone(), "remember this");
    assert_eq!(doc.scope, scope);
    assert_eq!(doc.body, "remember this");
    assert!(doc.class.is_none());
    assert!(doc.importance.is_none());
    assert!(doc.expires_at.is_none());
    assert!(doc.archived_at.is_none());
    assert!(doc.embedding.is_none());
}

#[test]
fn memory_doc_serde_defaults_missing_embedding() {
    let doc = MemoryDoc::new(MemoryScope::for_owner(Owner::new("alice")), "remember this");
    let mut json = serde_json::to_value(&doc).expect("serialize memory doc");
    let object = json.as_object_mut().expect("memory doc json object");
    object.remove("embedding");

    let decoded: MemoryDoc = serde_json::from_value(json).expect("deserialize memory doc");

    assert!(decoded.embedding.is_none());
}

#[test]
fn memory_hit_serde_round_trip() {
    let now = Utc::now();
    let hit = MemoryHit {
        id: MemoryId::new(),
        body: "snippet".into(),
        score: 0.42,
        cosine_similarity: Some(0.9),
        created_at: now,
    };
    let json = serde_json::to_string(&hit).expect("serialize");
    let back: MemoryHit = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(back.id, hit.id);
    assert!((back.score - hit.score).abs() < f32::EPSILON);
    assert_eq!(back.cosine_similarity, Some(0.9));
}

#[test]
fn memory_hit_serde_defaults_missing_cosine_similarity() {
    let hit = MemoryHit {
        id: MemoryId::new(),
        body: "snippet".into(),
        score: 0.42,
        cosine_similarity: None,
        created_at: Utc::now(),
    };
    let mut json = serde_json::to_value(&hit).expect("serialize memory hit");
    let object = json.as_object_mut().expect("memory hit json object");
    object.remove("cosine_similarity");

    let decoded: MemoryHit = serde_json::from_value(json).expect("deserialize memory hit");

    assert!(decoded.cosine_similarity.is_none());
}

#[test]
fn session_search_hit_serde_round_trip() {
    let now = Utc::now();
    let hit = SessionSearchHit {
        session_id: SessionId::new(),
        excerpt: "...matched text...".into(),
        score: 1.5,
        occurred_at: now,
    };
    let json = serde_json::to_string(&hit).expect("serialize");
    let back: SessionSearchHit = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(back.session_id, hit.session_id);
    assert_eq!(back.excerpt, hit.excerpt);
    assert!((back.score - hit.score).abs() < f64::EPSILON);
}

#[test]
fn goal_status_str_round_trips_every_variant() {
    for (status, label) in [
        (GoalStatus::Active, "active"),
        (GoalStatus::Paused, "paused"),
        (GoalStatus::Suspended, "suspended"),
        (GoalStatus::Blocked, "blocked"),
        (GoalStatus::UsageLimited, "usage_limited"),
        (GoalStatus::BudgetLimited, "budget_limited"),
        (GoalStatus::Complete, "complete"),
    ] {
        assert_eq!(status.as_str(), label);
        assert_eq!(label.parse::<GoalStatus>().expect("parse"), status);
        // serde uses the same snake_case wire form as `as_str`.
        assert_eq!(
            serde_json::to_string(&status).expect("serialize"),
            format!("\"{label}\"")
        );
    }
}

#[test]
fn goal_status_parse_rejects_unknown() {
    let err = "done".parse::<GoalStatus>().expect_err("unknown status");
    assert!(err.to_string().contains("done"));
}

#[test]
fn goal_status_active_and_terminal_predicates() {
    assert!(GoalStatus::Active.is_active());
    assert!(!GoalStatus::Paused.is_active());
    assert!(!GoalStatus::Suspended.is_active());
    assert!(GoalStatus::Complete.is_terminal());
    assert!(GoalStatus::BudgetLimited.is_terminal());
    assert!(!GoalStatus::Blocked.is_terminal());
    assert!(!GoalStatus::Suspended.is_terminal());
    assert!(!GoalStatus::Active.is_terminal());
}

#[test]
fn thread_goal_new_is_active_with_zeroed_accounting() {
    let goal = ThreadGoal::new(Owner::new("u"), SessionId::new(), "objective", Some(500));
    assert_eq!(goal.status, GoalStatus::Active);
    assert_eq!(goal.tokens_used, 0);
    assert_eq!(goal.time_used_seconds, 0);
    assert_eq!(goal.remaining_tokens(), Some(500));
}

#[test]
fn thread_goal_remaining_tokens_floors_at_zero() {
    let mut goal = ThreadGoal::new(Owner::new("u"), SessionId::new(), "objective", Some(100));
    goal.tokens_used = 150;
    assert_eq!(goal.remaining_tokens(), Some(0));
    let unbudgeted = ThreadGoal::new(Owner::new("u"), SessionId::new(), "objective", None);
    assert_eq!(unbudgeted.remaining_tokens(), None);
}

#[test]
fn validate_goal_objective_trims_and_bounds() {
    assert_eq!(
        validate_goal_objective("  ship it  ").expect("valid"),
        "ship it"
    );
    validate_goal_objective("   ").expect_err("blank objective is rejected");
    let too_long = "x".repeat(MAX_GOAL_OBJECTIVE_CHARS + 1);
    validate_goal_objective(&too_long).expect_err("over-cap objective is rejected");
    let at_cap = "y".repeat(MAX_GOAL_OBJECTIVE_CHARS);
    validate_goal_objective(&at_cap).expect("objective at the cap is valid");
}

#[test]
fn validate_goal_budget_requires_positive() {
    validate_goal_budget(None).expect("absent budget is valid");
    validate_goal_budget(Some(1)).expect("positive budget is valid");
    validate_goal_budget(Some(0)).expect_err("zero budget is rejected");
    validate_goal_budget(Some(-5)).expect_err("negative budget is rejected");
}

#[test]
fn thread_goal_update_apply_to_patches_present_fields() {
    let mut goal = ThreadGoal::new(Owner::new("u"), SessionId::new(), "old", None);
    let at = Utc::now();
    ThreadGoalUpdate {
        objective: Some("new".into()),
        status: Some(GoalStatus::Complete),
    }
    .apply_to(&mut goal, at);
    assert_eq!(goal.objective, "new");
    assert_eq!(goal.status, GoalStatus::Complete);
    assert_eq!(goal.updated_at, at);
}
