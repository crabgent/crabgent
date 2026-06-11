//! [`CalendarTool`] implementation.

use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;
use crabgent_calendar::{Clock, HolidayProvider};
use crabgent_core::tool::{Tool, ToolCtx, gate_tool_action, op_schema, parse_args_with_context};
use crabgent_core::{Action, PolicyHook, ToolError};
use serde_json::{Value, json};

use crate::args::{Args, Op};
use crate::ops::{self, MAX_YEAR, MIN_YEAR};

pub const TOOL_NAME: &str = "calendar";

const DESCRIPTION: &str = "Read-only calendar calculations. Operations: holidays_list, \
    holidays_next, holiday_check, days_between, date_arith, weekday_info. All operations are \
    policy-gated. Coverage is European (EU/EEA) for years 2000 to 2050; other countries and \
    out-of-range years are rejected. Subdivision matters for AT, DE, ES, FI, FR, GB, IT, NO, PT \
    (regional holiday variation); these are the only countries with subdivision-specific \
    entries. DE has subdivision-only holidays such as Reformationstag in some federal states, the \
    national list is usable without subdivision. If the user did not specify a subdivision for \
    those countries, ASK the user instead of guessing.";

/// LLM-facing calendar tool. Holds holiday provider + policy by `Arc`.
pub struct CalendarTool {
    provider: Arc<dyn HolidayProvider>,
    policy: Arc<dyn PolicyHook>,
    clock: Clock,
}

impl CalendarTool {
    pub fn new(provider: Arc<dyn HolidayProvider>, policy: Arc<dyn PolicyHook>) -> Self {
        Self {
            provider,
            policy,
            clock: Arc::new(Utc::now),
        }
    }

    #[must_use]
    pub fn with_clock(mut self, clock: Clock) -> Self {
        self.clock = clock;
        self
    }
}

#[async_trait]
impl Tool for CalendarTool {
    fn name(&self) -> &'static str {
        TOOL_NAME
    }

    fn description(&self) -> &'static str {
        DESCRIPTION
    }

    fn parameters_schema(&self) -> Value {
        let subdivision_countries = self.provider.subdivision_countries().join(", ");
        let subdivision_description = format!(
            "Subdivision code, e.g. NW or BY. Matters for {subdivision_countries} \
             (regional holiday variation); ask the user when missing for those countries."
        );
        let (min_year, max_year) = self.provider.year_bounds().unwrap_or((MIN_YEAR, MAX_YEAR));
        json!({
            "type": "object",
            "required": ["op"],
            "properties": {
                "op": op_schema::<Op>("Calendar operation to perform."),
                "country": {"type": "string", "description": "ISO 3166-1 alpha-2 (EU/EEA only), e.g. DE."},
                "subdivision": {
                    "type": "string",
                    "description": subdivision_description
                },
                "year": {"type": "integer", "minimum": min_year, "maximum": max_year},
                "after": {"type": "string", "description": "YYYY-MM-DD. Default: today from tool clock."},
                "count": {"type": "integer", "minimum": 1, "maximum": 50},
                "date": {"type": "string", "description": "YYYY-MM-DD for holiday_check, date_arith, weekday_info."},
                "start": {"type": "string", "description": "YYYY-MM-DD start for days_between."},
                "end": {"type": "string", "description": "YYYY-MM-DD end for days_between."},
                "business_days": {"type": "boolean", "default": false},
                "date_op": {
                    "type": "string",
                    "enum": ["add", "sub"],
                    "description": "Operation for date_arith. Accepted values are only add or sub. The JSON key may also be provided as operation or arith_op; values have no aliases."
                },
                "amount": {"type": "integer", "minimum": 0},
                "unit": {"type": "string", "enum": ["days", "weeks", "months", "years"]}
            }
        })
    }

    async fn execute(&self, args: Value, context: &ToolCtx) -> Result<Value, ToolError> {
        let parsed: Args = parse_args_with_context(args, "calendar args")?;
        let action = action_for(parsed.op);
        gate_tool_action(self.policy.as_ref(), context, &action).await?;
        match parsed.op {
            Op::HolidaysList => ops::holidays_list::run(&parsed, self.provider.as_ref()),
            Op::HolidaysNext => {
                ops::holidays_next::run(&parsed, self.provider.as_ref(), (self.clock)())
            }
            Op::HolidayCheck => ops::holiday_check::run(&parsed, self.provider.as_ref()),
            Op::DaysBetween => ops::days_between::run(&parsed, self.provider.as_ref()),
            Op::DateArith => ops::date_arith::run(&parsed),
            Op::WeekdayInfo => ops::weekday_info::run(&parsed),
        }
    }
}

const fn action_for(op: Op) -> Action {
    match op {
        Op::HolidaysList => Action::CalendarHolidaysList,
        Op::HolidaysNext => Action::CalendarHolidaysNext,
        Op::HolidayCheck => Action::CalendarHolidayCheck,
        Op::DaysBetween => Action::CalendarDaysBetween,
        Op::DateArith => Action::CalendarDateArith,
        Op::WeekdayInfo => Action::CalendarWeekdayInfo,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crabgent_calendar::{EmbeddedHolidayProvider, HolidayProvider};
    use crabgent_core::{AllowAllPolicy, PolicyHook, Tool};

    use super::*;

    fn calendar_tool() -> CalendarTool {
        let provider: Arc<dyn HolidayProvider> = Arc::new(EmbeddedHolidayProvider::new());
        let policy: Arc<dyn PolicyHook> = Arc::new(AllowAllPolicy);
        CalendarTool::new(provider, policy)
    }

    #[test]
    fn description_only_names_supported_subdivision_countries() {
        // Every country named in the static description must carry
        // subdivision data; advertising an absent country (e.g. US/CA) leads
        // the LLM into InvalidArgs calls.
        for country in EmbeddedHolidayProvider::new().subdivision_countries() {
            assert!(
                DESCRIPTION.contains(country),
                "description should mention data-backed country {country}"
            );
        }
        for absent in ["CA", "US", "MX", "BR", "IN", "AU", "CH"] {
            assert!(
                !DESCRIPTION.contains(absent),
                "description must not advertise unsupported country {absent}"
            );
        }
        assert!(DESCRIPTION.contains("ASK the user"));
    }

    #[test]
    fn parameters_schema_subdivision_hint_is_data_derived() {
        let schema = calendar_tool().parameters_schema();
        let description = schema["properties"]["subdivision"]["description"]
            .as_str()
            .expect("subdivision description");

        for country in EmbeddedHolidayProvider::new().subdivision_countries() {
            assert!(description.contains(country), "missing {country}");
        }
        assert!(!description.contains("US"));
        assert!(description.contains("ask the user"));
    }

    #[test]
    fn parameters_schema_year_bounds_match_data() {
        let provider = EmbeddedHolidayProvider::new();
        let (min, max) = provider.year_bounds().expect("embedded data has years");
        let schema = calendar_tool().parameters_schema();
        assert_eq!(schema["properties"]["year"]["minimum"], json!(min));
        assert_eq!(schema["properties"]["year"]["maximum"], json!(max));
    }

    #[test]
    fn year_constants_track_embedded_data_bounds() {
        // CA2 guard: the validate_year constants must stay aligned with the
        // embedded data so out-of-data years fail loud, not silently empty.
        assert_eq!(
            EmbeddedHolidayProvider::new().year_bounds(),
            Some((MIN_YEAR, MAX_YEAR))
        );
    }
}
