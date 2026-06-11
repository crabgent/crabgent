use chrono::{Datelike, NaiveDate};
use crabgent_calendar::HolidayProvider;
use crabgent_core::ToolError;
use serde_json::{Value, json};

use crate::args::Args;
use crate::ops::validate_year;
use crate::output::date_string;

pub fn run(args: &Args, provider: &dyn HolidayProvider) -> Result<Value, ToolError> {
    let country = args
        .required_string(args.country.as_ref(), "country")
        .map_err(ToolError::InvalidArgs)?;
    let subdivision = args
        .required_string(args.subdivision.as_ref(), "subdivision")
        .map_err(ToolError::InvalidArgs)?;
    let year = args
        .required_i32(args.year, "year")
        .map_err(ToolError::InvalidArgs)?;
    validate_year(year, "calendar.holidays_list")?;
    ensure_known_country(provider, &country, &subdivision)?;

    let mut holidays = Vec::new();
    let mut cursor = date(year, 1, 1)?;
    while cursor.year() == year {
        if let Some(name) = provider.get_holiday(cursor, &country, &subdivision) {
            holidays.push(json!({"date": date_string(cursor), "name": name}));
        }
        let Some(next) = cursor.succ_opt() else {
            break;
        };
        cursor = next;
    }

    Ok(json!({ "holidays": holidays }))
}

pub fn ensure_known_country(
    provider: &dyn HolidayProvider,
    country: &str,
    subdivision: &str,
) -> Result<(), ToolError> {
    if provider.has_subdivision(country, subdivision) {
        Ok(())
    } else {
        Err(ToolError::InvalidArgs(
            "calendar.holidays_list: unknown country/subdivision".into(),
        ))
    }
}

fn date(year: i32, month: u32, day: u32) -> Result<NaiveDate, ToolError> {
    NaiveDate::from_ymd_opt(year, month, day)
        .ok_or_else(|| ToolError::InvalidArgs("calendar.holidays_list: invalid year".into()))
}
