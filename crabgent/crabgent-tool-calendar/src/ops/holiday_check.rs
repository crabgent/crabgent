use crabgent_calendar::HolidayProvider;
use crabgent_core::ToolError;
use serde_json::{Value, json};

use crate::args::Args;
use crate::ops::parse_date;

pub fn run(args: &Args, provider: &dyn HolidayProvider) -> Result<Value, ToolError> {
    let country = args
        .required_string(args.country.as_ref(), "country")
        .map_err(ToolError::InvalidArgs)?;
    let subdivision = args
        .required_string(args.subdivision.as_ref(), "subdivision")
        .map_err(ToolError::InvalidArgs)?;
    let date = args
        .required_string(args.date.as_ref(), "date")
        .map_err(ToolError::InvalidArgs)?;
    let date = parse_date(&date, "calendar.holiday_check")?;

    let Some(name) = provider.get_holiday(date, &country, &subdivision) else {
        return Ok(json!({ "is_holiday": false }));
    };

    Ok(json!({ "is_holiday": true, "name": name }))
}
