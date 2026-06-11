use std::sync::Arc;

use chrono::{DateTime, Utc};
use crabgent_calendar::{Clock, EmbeddedHolidayProvider, HolidayProvider};
use crabgent_core::{AllowAllPolicy, DenyAllPolicy, PolicyHook, Subject, Tool, ToolCtx, ToolError};
use crabgent_tool_calendar::CalendarTool;
use serde_json::{Value, json};

fn fixed_clock(datetime: &str) -> Clock {
    let now = DateTime::parse_from_rfc3339(datetime)
        .expect("valid RFC3339 datetime in test")
        .with_timezone(&Utc);
    Arc::new(move || now)
}

fn tool_with_policy(policy: Arc<dyn PolicyHook>) -> CalendarTool {
    let provider: Arc<dyn HolidayProvider> = Arc::new(EmbeddedHolidayProvider::new());
    CalendarTool::new(provider, policy).with_clock(fixed_clock("2026-01-01T00:00:00Z"))
}

fn allow_tool() -> CalendarTool {
    tool_with_policy(Arc::new(AllowAllPolicy))
}

fn deny_tool() -> CalendarTool {
    tool_with_policy(Arc::new(DenyAllPolicy))
}

fn ctx() -> ToolCtx {
    ToolCtx::new(Subject::new("u"))
}

async fn execute(tool: &CalendarTool, args: Value) -> Result<Value, ToolError> {
    tool.execute(args, &ctx()).await
}

#[test]
fn tool_metadata_and_schema_are_calendar_shaped() {
    let tool = allow_tool();

    assert_eq!(tool.name(), "calendar");
    assert!(tool.description().contains("date_arith"));
    assert_eq!(tool.parameters_schema()["required"], json!(["op"]));
    assert!(
        tool.parameters_schema()["properties"]["op"]["enum"]
            .as_array()
            .expect("op enum")
            .contains(&json!("weekday_info"))
    );
}

#[tokio::test]
async fn holidays_list_returns_array_for_valid_year() {
    let result = execute(
        &allow_tool(),
        json!({"op": "holidays_list", "country": "DE", "subdivision": "NW", "year": 2026}),
    )
    .await
    .expect("valid holidays_list");

    let holidays = result["holidays"].as_array().expect("holidays array");
    assert!(!holidays.is_empty());
    assert!(holidays.iter().any(|item| item["date"] == "2026-10-03"));
}

#[tokio::test]
async fn holidays_list_rejects_out_of_range_year() {
    let err = execute(
        &allow_tool(),
        json!({"op": "holidays_list", "country": "DE", "subdivision": "NW", "year": 1800}),
    )
    .await
    .expect_err("year out of range");

    assert!(
        matches!(err, ToolError::InvalidArgs(message) if message.contains("year out of range"))
    );
}

#[tokio::test]
async fn holidays_list_rejects_years_just_outside_data_bounds() {
    // CA2: 1999 and 2051 are inside the old 1970..=2100 window but outside the
    // embedded 2000..=2050 data, so they used to return an empty success.
    for year in [1999, 2051] {
        let err = execute(
            &allow_tool(),
            json!({"op": "holidays_list", "country": "DE", "subdivision": "NW", "year": year}),
        )
        .await
        .expect_err("year outside data bounds should be rejected");
        assert!(
            matches!(err, ToolError::InvalidArgs(message) if message.contains("year out of range")),
            "year {year} should report year out of range"
        );
    }
}

#[tokio::test]
async fn holidays_list_accepts_data_boundary_years() {
    // CA2: the inclusive bounds themselves stay valid.
    for year in [2000, 2050] {
        let result = execute(
            &allow_tool(),
            json!({"op": "holidays_list", "country": "DE", "subdivision": "NW", "year": year}),
        )
        .await
        .unwrap_or_else(|err| panic!("year {year} should be valid, got {err:?}"));
        assert!(
            !result["holidays"]
                .as_array()
                .expect("holidays array")
                .is_empty(),
            "year {year} should list holidays"
        );
    }
}

#[tokio::test]
async fn holidays_list_rejects_unknown_subdivision_for_known_country() {
    // CA3 (consistency): holidays_list also rejects a bogus subdivision rather
    // than answering from the National fallback.
    let err = execute(
        &allow_tool(),
        json!({"op": "holidays_list", "country": "DE", "subdivision": "ZZ", "year": 2026}),
    )
    .await
    .expect_err("unknown subdivision invalid");

    assert!(
        matches!(err, ToolError::InvalidArgs(message) if message.contains("unknown country/subdivision"))
    );
}

#[tokio::test]
async fn holidays_next_returns_upcoming_with_days_from_today() {
    let tool =
        tool_with_policy(Arc::new(AllowAllPolicy)).with_clock(fixed_clock("2026-10-01T00:00:00Z"));
    let result = execute(
        &tool,
        json!({"op": "holidays_next", "country": "DE", "subdivision": "NW", "count": 3}),
    )
    .await
    .expect("valid holidays_next");

    let holidays = result["holidays"].as_array().expect("holidays array");
    assert_eq!(holidays.len(), 3);
    assert_eq!(holidays[0]["date"], "2026-10-03");
    assert_eq!(holidays[0]["days_from_today"], 2);
}

#[tokio::test]
async fn holidays_next_honors_after_arg() {
    let result = execute(
        &allow_tool(),
        json!({
            "op": "holidays_next",
            "country": "DE",
            "subdivision": "NW",
            "after": "2026-12-24",
            "count": 1
        }),
    )
    .await
    .expect("valid holidays_next");

    assert_eq!(result["holidays"][0]["date"], "2026-12-25");
    assert_eq!(result["holidays"][0]["days_from_today"], 1);
}

#[tokio::test]
async fn holiday_check_returns_true_for_known_holiday() {
    let result = execute(
        &allow_tool(),
        json!({"op": "holiday_check", "country": "DE", "subdivision": "NW", "date": "2026-10-03"}),
    )
    .await
    .expect("valid holiday_check");

    assert_eq!(result["is_holiday"], true);
    let name = result["name"].as_str().expect("holiday name");
    assert!(name.contains("Einheit") || name.contains("Unity"));
}

#[tokio::test]
async fn holiday_check_returns_false_for_workday() {
    let result = execute(
        &allow_tool(),
        json!({"op": "holiday_check", "country": "DE", "subdivision": "NW", "date": "2026-01-07"}),
    )
    .await
    .expect("valid holiday_check");

    assert_eq!(result, json!({"is_holiday": false}));
}

#[tokio::test]
async fn days_between_positive() {
    let result = execute(
        &allow_tool(),
        json!({"op": "days_between", "start": "2026-01-01", "end": "2026-01-08"}),
    )
    .await
    .expect("valid days_between");

    assert_eq!(result["calendar_days"], 7);
    assert_eq!(result["business_days"], 0);
    assert_eq!(result["weeks_full"], 1);
}

#[tokio::test]
async fn days_between_negative() {
    let result = execute(
        &allow_tool(),
        json!({"op": "days_between", "start": "2026-01-08", "end": "2026-01-01"}),
    )
    .await
    .expect("valid days_between");

    assert_eq!(result["calendar_days"], -7);
    assert_eq!(result["weeks_full"], -1);
}

#[tokio::test]
async fn days_between_business_flag() {
    let result = execute(
        &allow_tool(),
        json!({
            "op": "days_between",
            "start": "2026-01-02",
            "end": "2026-01-05",
            "business_days": true
        }),
    )
    .await
    .expect("valid days_between");

    assert_eq!(result["calendar_days"], 3);
    assert_eq!(result["business_days"], 1);
}

#[tokio::test]
async fn days_between_with_holidays_deducted() {
    let baseline = execute(
        &allow_tool(),
        json!({
            "op": "days_between",
            "start": "2026-04-30",
            "end": "2026-05-02",
            "business_days": true
        }),
    )
    .await
    .expect("baseline days_between");
    let with_holidays = execute(
        &allow_tool(),
        json!({
            "op": "days_between",
            "start": "2026-04-30",
            "end": "2026-05-02",
            "business_days": true,
            "country": "DE",
            "subdivision": "NW"
        }),
    )
    .await
    .expect("holiday-aware days_between");

    assert_eq!(baseline["business_days"], 2);
    assert_eq!(with_holidays["business_days"], 1);
}

#[tokio::test]
async fn days_between_rejects_partial_holiday_scope() {
    let err = execute(
        &allow_tool(),
        json!({
            "op": "days_between",
            "start": "2026-01-01",
            "end": "2026-01-08",
            "business_days": true,
            "country": "DE"
        }),
    )
    .await
    .expect_err("partial scope invalid");

    assert!(
        matches!(err, ToolError::InvalidArgs(message) if message.contains("provided together"))
    );
}

#[tokio::test]
async fn days_between_rejects_unknown_holiday_scope() {
    let err = execute(
        &allow_tool(),
        json!({
            "op": "days_between",
            "start": "2026-01-01",
            "end": "2026-01-08",
            "business_days": true,
            "country": "XX",
            "subdivision": "National"
        }),
    )
    .await
    .expect_err("unknown country invalid");

    assert!(
        matches!(err, ToolError::InvalidArgs(message) if message.contains("unknown country/subdivision"))
    );
}

#[tokio::test]
async fn days_between_rejects_unknown_subdivision_for_known_country() {
    // CA3: a bogus subdivision for a real country must not slip past via the
    // National fallback and silently return a business-day count.
    let err = execute(
        &allow_tool(),
        json!({
            "op": "days_between",
            "start": "2026-01-01",
            "end": "2026-01-08",
            "business_days": true,
            "country": "DE",
            "subdivision": "ZZ"
        }),
    )
    .await
    .expect_err("unknown subdivision invalid");

    assert!(
        matches!(err, ToolError::InvalidArgs(message) if message.contains("unknown country/subdivision"))
    );
}

#[tokio::test]
async fn weekday_info_wednesday() {
    let result = execute(
        &allow_tool(),
        json!({"op": "weekday_info", "date": "2026-01-07"}),
    )
    .await
    .expect("valid weekday_info");

    assert_eq!(result["weekday_number_iso"], 3);
    assert_eq!(result["weekday_name"], "Wednesday");
    assert_eq!(result["is_weekend"], false);
}

#[tokio::test]
async fn weekday_info_sunday() {
    let result = execute(
        &allow_tool(),
        json!({"op": "weekday_info", "date": "2026-01-04"}),
    )
    .await
    .expect("valid weekday_info");

    assert_eq!(result["weekday_number_iso"], 7);
    assert_eq!(result["is_weekend"], true);
}

#[tokio::test]
async fn policy_deny_holidays_list_returns_tool_error_permission() {
    assert_permission_denied(
        json!({"op": "holidays_list", "country": "DE", "subdivision": "NW", "year": 2026}),
    )
    .await;
}

#[tokio::test]
async fn policy_deny_remaining_ops_return_tool_error_permission() {
    let cases = [
        json!({"op": "holidays_next", "country": "DE", "subdivision": "NW"}),
        json!({"op": "holiday_check", "country": "DE", "subdivision": "NW", "date": "2026-10-03"}),
        json!({"op": "days_between", "start": "2026-01-01", "end": "2026-01-08"}),
        json!({"op": "date_arith", "date": "2026-01-01", "date_op": "add", "amount": 1, "unit": "days"}),
        json!({"op": "weekday_info", "date": "2026-01-07"}),
    ];

    for args in cases {
        assert_permission_denied(args).await;
    }
}

#[tokio::test]
async fn invalid_date_string_returns_invalid_args() {
    let err = execute(
        &allow_tool(),
        json!({"op": "weekday_info", "date": "2026-02-30"}),
    )
    .await
    .expect_err("invalid date");

    assert!(matches!(err, ToolError::InvalidArgs(message) if message.contains("invalid date")));
}

#[tokio::test]
async fn country_unknown_returns_invalid_args() {
    let err = execute(
        &allow_tool(),
        json!({"op": "holidays_list", "country": "XX", "subdivision": "National", "year": 2026}),
    )
    .await
    .expect_err("unknown country");

    assert!(
        matches!(err, ToolError::InvalidArgs(message) if message.contains("unknown country/subdivision"))
    );
}

#[tokio::test]
async fn missing_required_fields_report_op_context() {
    let cases = [
        (
            json!({"op": "holidays_list"}),
            "calendar.holidays_list: country required",
        ),
        (
            json!({"op": "holidays_next"}),
            "calendar.holidays_next: country required",
        ),
        (
            json!({"op": "holiday_check", "country": "DE", "subdivision": "NW"}),
            "calendar.holiday_check: date required",
        ),
        (
            json!({"op": "days_between", "start": "2026-01-01"}),
            "calendar.days_between: end required",
        ),
        (
            json!({"op": "date_arith", "date": "2026-01-01"}),
            "calendar.date_arith: date_op required",
        ),
        (
            json!({"op": "weekday_info"}),
            "calendar.weekday_info: date required",
        ),
    ];

    for (args, expected) in cases {
        let err = execute(&allow_tool(), args)
            .await
            .expect_err("required field should fail");
        assert!(matches!(err, ToolError::InvalidArgs(message) if message == expected));
    }
}

async fn assert_permission_denied(args: Value) {
    let err = execute(&deny_tool(), args)
        .await
        .expect_err("policy should deny");
    assert!(matches!(err, ToolError::Permission(message) if message.contains("DenyAllPolicy")));
}
