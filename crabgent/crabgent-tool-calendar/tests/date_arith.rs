use std::sync::Arc;

use chrono::{DateTime, Utc};
use crabgent_calendar::{Clock, EmbeddedHolidayProvider, HolidayProvider};
use crabgent_core::{AllowAllPolicy, PolicyHook, Subject, Tool, ToolCtx, ToolError};
use crabgent_tool_calendar::CalendarTool;
use serde_json::{Value, json};

fn fixed_clock(datetime: &str) -> Clock {
    let now = DateTime::parse_from_rfc3339(datetime)
        .expect("valid RFC3339 datetime in test")
        .with_timezone(&Utc);
    Arc::new(move || now)
}

fn allow_tool() -> CalendarTool {
    let provider: Arc<dyn HolidayProvider> = Arc::new(EmbeddedHolidayProvider::new());
    let policy: Arc<dyn PolicyHook> = Arc::new(AllowAllPolicy);
    CalendarTool::new(provider, policy).with_clock(fixed_clock("2026-01-01T00:00:00Z"))
}

fn ctx() -> ToolCtx {
    ToolCtx::new(Subject::new("u"))
}

async fn execute(args: Value) -> Result<Value, ToolError> {
    allow_tool().execute(args, &ctx()).await
}

#[tokio::test]
async fn add_days() {
    let result = execute(json!({
        "op": "date_arith",
        "date": "2026-01-01",
        "date_op": "add",
        "amount": 7,
        "unit": "days"
    }))
    .await
    .expect("valid date_arith");

    assert_eq!(result["result_date"], "2026-01-08");
    assert_eq!(result["weekday"], "Thursday");
}

#[tokio::test]
async fn supports_weeks_months_years_and_subtraction() {
    let weeks = execute(json!({
        "op": "date_arith",
        "date": "2026-01-15",
        "date_op": "sub",
        "amount": 2,
        "unit": "weeks"
    }))
    .await
    .expect("valid week subtraction");
    let months = execute(json!({
        "op": "date_arith",
        "date": "2026-01-31",
        "date_op": "add",
        "amount": 1,
        "unit": "months"
    }))
    .await
    .expect("valid month addition");
    let years = execute(json!({
        "op": "date_arith",
        "date": "2026-02-28",
        "date_op": "add",
        "amount": 1,
        "unit": "years"
    }))
    .await
    .expect("valid year addition");

    assert_eq!(weeks["result_date"], "2026-01-01");
    assert_eq!(months["result_date"], "2026-02-28");
    assert_eq!(years["result_date"], "2027-02-28");
}

#[tokio::test]
async fn invalid_op() {
    let err = execute(json!({
        "op": "date_arith",
        "date": "2026-01-01",
        "date_op": "multiply",
        "amount": 7,
        "unit": "days"
    }))
    .await
    .expect_err("invalid date_op");

    assert!(matches!(err, ToolError::InvalidArgs(message) if message.contains("date_op")));

    let err = execute(json!({
        "op": "date_arith",
        "date": "2026-01-01",
        "date_op": "multiply",
        "amount": -1,
        "unit": "days"
    }))
    .await
    .expect_err("invalid date_op wins over invalid amount");

    assert!(matches!(err, ToolError::InvalidArgs(message) if message.contains("date_op")));
}

#[tokio::test]
async fn rejects_invalid_unit_and_negative_amount() {
    let invalid_unit = execute(json!({
        "op": "date_arith",
        "date": "2026-01-01",
        "date_op": "add",
        "amount": 1,
        "unit": "fortnights"
    }))
    .await
    .expect_err("invalid unit");
    let negative_amount = execute(json!({
        "op": "date_arith",
        "date": "2026-01-01",
        "date_op": "add",
        "amount": -1,
        "unit": "days"
    }))
    .await
    .expect_err("negative amount");

    assert!(matches!(invalid_unit, ToolError::InvalidArgs(message) if message.contains("unit")));
    assert!(
        matches!(negative_amount, ToolError::InvalidArgs(message) if message.contains("non-negative"))
    );
}
