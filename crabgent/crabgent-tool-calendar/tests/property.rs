use std::sync::Arc;

use chrono::{Duration, NaiveDate};
use crabgent_calendar::{EmbeddedHolidayProvider, HolidayProvider};
use crabgent_core::{AllowAllPolicy, PolicyHook, Subject, Tool, ToolCtx};
use crabgent_tool_calendar::CalendarTool;
use proptest::prelude::*;
use serde_json::{Value, json};

fn tool() -> CalendarTool {
    let provider: Arc<dyn HolidayProvider> = Arc::new(EmbeddedHolidayProvider::new());
    let policy: Arc<dyn PolicyHook> = Arc::new(AllowAllPolicy);
    CalendarTool::new(provider, policy)
}

fn ctx() -> ToolCtx {
    ToolCtx::new(Subject::new("u"))
}

async fn execute(args: Value) -> Value {
    tool()
        .execute(args, &ctx())
        .await
        .expect("property input should be valid")
}

const fn date_from_offset(offset: i64) -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 1, 1)
        .expect("valid base date")
        .checked_add_signed(Duration::days(offset))
        .expect("valid generated date")
}

proptest! {
    #[test]
    fn date_arith_roundtrip(offset in 0i64..365, amount in 1i64..365) {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("test runtime");
        let date = date_from_offset(offset).format("%Y-%m-%d").to_string();

        runtime.block_on(async {
            let added = execute(json!({
                "op": "date_arith",
                "date": date,
                "date_op": "add",
                "amount": amount,
                "unit": "days"
            })).await;
            let roundtrip = execute(json!({
                "op": "date_arith",
                "date": added["result_date"],
                "date_op": "sub",
                "amount": amount,
                "unit": "days"
            })).await;

            prop_assert_eq!(roundtrip["result_date"].as_str(), Some(date.as_str()));
            Ok(())
        })?;
    }

    #[test]
    fn days_between_symmetry(a in 0i64..365, b in 0i64..365) {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("test runtime");
        let date_a = date_from_offset(a).format("%Y-%m-%d").to_string();
        let date_b = date_from_offset(b).format("%Y-%m-%d").to_string();

        runtime.block_on(async {
            let forward = execute(json!({
                "op": "days_between",
                "start": date_a,
                "end": date_b
            })).await;
            let reverse = execute(json!({
                "op": "days_between",
                "start": date_b,
                "end": date_a
            })).await;

            let forward_days = forward["calendar_days"].as_i64().expect("calendar_days");
            let reverse_days = reverse["calendar_days"].as_i64().expect("calendar_days");
            prop_assert_eq!(forward_days, -reverse_days);
            Ok(())
        })?;
    }

    #[test]
    fn weekday_cycle(offset in 0i64..365) {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("test runtime");
        let start = date_from_offset(offset);

        runtime.block_on(async {
            let mut weekdays = Vec::new();
            for day in 0..7 {
                let date = start
                    .checked_add_signed(Duration::days(day))
                    .expect("valid generated date")
                    .format("%Y-%m-%d")
                    .to_string();
                let result = execute(json!({"op": "weekday_info", "date": date})).await;
                weekdays.push(result["weekday_number_iso"].as_u64().expect("weekday number"));
            }
            weekdays.sort_unstable();
            prop_assert_eq!(weekdays, vec![1, 2, 3, 4, 5, 6, 7]);
            Ok(())
        })?;
    }
}
