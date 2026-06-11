//! Pure operation implementations for [`crate::CalendarTool`].

pub mod date_arith;
pub mod days_between;
pub mod holiday_check;
pub mod holidays_list;
pub mod holidays_next;
pub mod weekday_info;

use chrono::NaiveDate;
use crabgent_core::ToolError;

/// Inclusive year range covered by the embedded holiday data. Mirrors
/// `EmbeddedHolidayProvider::year_bounds`; a guard test pins the two together
/// so a data refresh that widens the range surfaces here instead of silently
/// returning empty holiday lists for years outside the data.
pub const MIN_YEAR: i32 = 2000;
pub const MAX_YEAR: i32 = 2050;

pub fn parse_date(value: &str, context: &str) -> Result<NaiveDate, ToolError> {
    NaiveDate::parse_from_str(value, "%Y-%m-%d")
        .map_err(|err| ToolError::InvalidArgs(format!("{context}: invalid date '{value}': {err}")))
}

pub fn validate_year(year: i32, context: &str) -> Result<(), ToolError> {
    if (MIN_YEAR..=MAX_YEAR).contains(&year) {
        Ok(())
    } else {
        Err(ToolError::InvalidArgs(format!(
            "{context}: year out of range (supported {MIN_YEAR}..={MAX_YEAR})"
        )))
    }
}
