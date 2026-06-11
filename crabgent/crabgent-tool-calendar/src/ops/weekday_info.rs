use crabgent_core::ToolError;
use serde_json::Value;

use crate::args::Args;
use crate::ops::parse_date;
use crate::output::weekday_info;

pub fn run(args: &Args) -> Result<Value, ToolError> {
    let date = args
        .required_string(args.date.as_ref(), "date")
        .map_err(ToolError::InvalidArgs)?;
    let date = parse_date(&date, "calendar.weekday_info")?;
    Ok(weekday_info(date))
}
