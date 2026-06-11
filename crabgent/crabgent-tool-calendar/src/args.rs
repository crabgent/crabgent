//! Deserializable argument surface for [`crate::CalendarTool`].

use serde::Deserialize;

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Op {
    HolidaysList,
    HolidaysNext,
    HolidayCheck,
    DaysBetween,
    DateArith,
    WeekdayInfo,
}

impl crabgent_core::tool::ToolOp for Op {
    const JSON_VALUES: &'static [&'static str] = &[
        "holidays_list",
        "holidays_next",
        "holiday_check",
        "days_between",
        "date_arith",
        "weekday_info",
    ];

    fn as_str(self) -> &'static str {
        match self {
            Self::HolidaysList => "holidays_list",
            Self::HolidaysNext => "holidays_next",
            Self::HolidayCheck => "holiday_check",
            Self::DaysBetween => "days_between",
            Self::DateArith => "date_arith",
            Self::WeekdayInfo => "weekday_info",
        }
    }
}

impl Op {
    pub const fn action_name(self) -> &'static str {
        match self {
            Self::HolidaysList => "calendar.holidays_list",
            Self::HolidaysNext => "calendar.holidays_next",
            Self::HolidayCheck => "calendar.holiday_check",
            Self::DaysBetween => "calendar.days_between",
            Self::DateArith => "calendar.date_arith",
            Self::WeekdayInfo => "calendar.weekday_info",
        }
    }
}

#[derive(Deserialize)]
pub struct Args {
    pub op: Op,
    #[serde(default)]
    pub country: Option<String>,
    #[serde(default)]
    pub subdivision: Option<String>,
    #[serde(default)]
    pub year: Option<i32>,
    #[serde(default)]
    pub after: Option<String>,
    #[serde(default)]
    pub count: Option<usize>,
    #[serde(default)]
    pub date: Option<String>,
    #[serde(default)]
    pub start: Option<String>,
    #[serde(default)]
    pub end: Option<String>,
    #[serde(default)]
    pub business_days: Option<bool>,
    #[serde(default, alias = "operation", alias = "arith_op")]
    pub date_op: Option<String>,
    #[serde(default)]
    pub amount: Option<i64>,
    #[serde(default)]
    pub unit: Option<String>,
}

impl Args {
    pub fn required_string(&self, value: Option<&String>, field: &str) -> Result<String, String> {
        value
            .filter(|value| !value.trim().is_empty())
            .cloned()
            .ok_or_else(|| format!("{}: {field} required", self.op.action_name()))
    }

    pub fn required_i32(&self, value: Option<i32>, field: &str) -> Result<i32, String> {
        value.ok_or_else(|| format!("{}: {field} required", self.op.action_name()))
    }

    pub fn required_i64(&self, value: Option<i64>, field: &str) -> Result<i64, String> {
        value.ok_or_else(|| format!("{}: {field} required", self.op.action_name()))
    }
}
