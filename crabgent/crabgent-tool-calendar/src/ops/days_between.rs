use chrono::{Datelike, NaiveDate};
use crabgent_calendar::HolidayProvider;
use crabgent_core::ToolError;
use serde_json::{Value, json};

use crate::args::Args;
use crate::ops::parse_date;

pub fn run(args: &Args, provider: &dyn HolidayProvider) -> Result<Value, ToolError> {
    let start = args
        .required_string(args.start.as_ref(), "start")
        .map_err(ToolError::InvalidArgs)?;
    let end = args
        .required_string(args.end.as_ref(), "end")
        .map_err(ToolError::InvalidArgs)?;
    let start = parse_date(&start, "calendar.days_between")?;
    let end = parse_date(&end, "calendar.days_between")?;
    let calendar_days = (end - start).num_days();
    let business_days = if args.business_days.unwrap_or(false) {
        business_days(start, end, holiday_scope(args)?, provider)?
    } else {
        0
    };

    Ok(json!({
        "calendar_days": calendar_days,
        "business_days": business_days,
        "weeks_full": calendar_days / 7,
    }))
}

fn holiday_scope(args: &Args) -> Result<Option<(&str, &str)>, ToolError> {
    match (args.country.as_deref(), args.subdivision.as_deref()) {
        (Some(country), Some(subdivision)) => Ok(Some((country, subdivision))),
        (None, None) => Ok(None),
        _ => Err(ToolError::InvalidArgs(
            "calendar.days_between: country and subdivision must be provided together".into(),
        )),
    }
}

fn business_days(
    start: NaiveDate,
    end: NaiveDate,
    scope: Option<(&str, &str)>,
    provider: &dyn HolidayProvider,
) -> Result<i64, ToolError> {
    if let Some((country, subdivision)) = scope
        && !provider.has_subdivision(country, subdivision)
    {
        return Err(ToolError::InvalidArgs(
            "calendar.days_between: unknown country/subdivision".into(),
        ));
    }
    if start == end {
        return Ok(0);
    }
    let sign = if end > start { 1 } else { -1 };
    let mut cursor = if sign == 1 { start } else { end };
    let stop = if sign == 1 { end } else { start };
    let mut count = 0i64;

    while cursor < stop {
        let is_weekday = cursor.weekday().number_from_monday() <= 5;
        if is_weekday {
            let is_holiday = scope.is_some_and(|(country, subdivision)| {
                provider.get_holiday(cursor, country, subdivision).is_some()
            });
            if !is_holiday {
                count += 1;
            }
        }
        let Some(next) = cursor.succ_opt() else {
            break;
        };
        cursor = next;
    }

    Ok(count * sign)
}
