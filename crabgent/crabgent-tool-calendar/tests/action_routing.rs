//! Per-op `Action` routing: a `StrictPolicy` that denies exactly one op's
//! `Action` (allow-by-default) must deny only that op and allow the others. This
//! catches a future misrouting in `action_for` that `AllowAllPolicy`/
//! `DenyAllPolicy` cannot, because those ignore the `Action` entirely.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use crabgent_calendar::{Clock, EmbeddedHolidayProvider, HolidayProvider};
use crabgent_core::{
    ActionMatcher, PolicyHook, Rule, StrictPolicy, Subject, Tool, ToolCtx, ToolError,
};
use crabgent_tool_calendar::CalendarTool;
use serde_json::{Value, json};

fn fixed_clock() -> Clock {
    let now = DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
        .expect("valid RFC3339 datetime in test")
        .with_timezone(&Utc);
    Arc::new(move || now)
}

/// A policy that denies exactly the `date_arith` action and allows everything
/// else. A correct `action_for` routes only `date_arith` to that denied action.
fn deny_only_date_arith() -> CalendarTool {
    let policy: Arc<dyn PolicyHook> = Arc::new(
        StrictPolicy::builder()
            .rule(Rule::deny(ActionMatcher::CalendarDateArith))
            .allow_by_default()
            .build(),
    );
    let provider: Arc<dyn HolidayProvider> = Arc::new(EmbeddedHolidayProvider::new());
    CalendarTool::new(provider, policy).with_clock(fixed_clock())
}

async fn execute(tool: &CalendarTool, args: Value) -> Result<Value, ToolError> {
    tool.execute(args, &ToolCtx::new(Subject::new("u"))).await
}

#[tokio::test]
async fn deny_one_op_action_denies_only_that_op() {
    let tool = deny_only_date_arith();

    let err = execute(
        &tool,
        json!({"op": "date_arith", "date": "2026-01-01", "date_op": "add", "amount": 1, "unit": "days"}),
    )
    .await
    .expect_err("date_arith is the denied action");
    assert!(matches!(err, ToolError::Permission(_)), "date_arith denied");

    // Every other op routes to a different (allowed) action and succeeds.
    let allowed = [
        json!({"op": "weekday_info", "date": "2026-01-07"}),
        json!({"op": "holidays_list", "country": "DE", "subdivision": "NW", "year": 2026}),
        json!({"op": "holidays_next", "country": "DE", "subdivision": "NW"}),
        json!({"op": "holiday_check", "country": "DE", "subdivision": "NW", "date": "2026-10-03"}),
        json!({"op": "days_between", "start": "2026-01-01", "end": "2026-01-08"}),
    ];
    for args in allowed {
        let result = execute(&tool, args.clone()).await;
        assert!(
            !matches!(result, Err(ToolError::Permission(_))),
            "op {} must not be denied: {result:?}",
            args["op"]
        );
    }
}
