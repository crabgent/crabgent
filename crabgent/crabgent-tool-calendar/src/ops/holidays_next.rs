use chrono::{DateTime, Utc};
use crabgent_calendar::HolidayProvider;
use crabgent_core::ToolError;
use serde_json::{Value, json};

use crate::args::Args;
use crate::ops::parse_date;
use crate::output::date_string;

const DEFAULT_COUNT: usize = 3;
const MAX_COUNT: usize = 50;

pub fn run(
    args: &Args,
    provider: &dyn HolidayProvider,
    now: DateTime<Utc>,
) -> Result<Value, ToolError> {
    let country = args
        .required_string(args.country.as_ref(), "country")
        .map_err(ToolError::InvalidArgs)?;
    let subdivision = args
        .required_string(args.subdivision.as_ref(), "subdivision")
        .map_err(ToolError::InvalidArgs)?;
    let after = match &args.after {
        Some(after) => parse_date(after, "calendar.holidays_next")?,
        None => now.date_naive(),
    };
    let count = args.count.unwrap_or(DEFAULT_COUNT).min(MAX_COUNT);

    let holidays = provider
        .upcoming_holidays(after, &country, &subdivision, count)
        .into_iter()
        .map(|(date, name)| {
            json!({
                "date": date_string(date),
                "name": name,
                "days_from_today": (date - after).num_days(),
            })
        })
        .collect::<Vec<_>>();

    Ok(json!({ "holidays": holidays }))
}
