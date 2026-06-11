use chrono::{Days, Months};
use crabgent_core::ToolError;
use serde_json::{Value, json};

use crate::args::Args;
use crate::ops::parse_date;
use crate::output::{date_string, weekday_name};

#[derive(Clone, Copy)]
enum DateOperation {
    Add,
    Sub,
}

pub fn run(args: &Args) -> Result<Value, ToolError> {
    let date = args
        .required_string(args.date.as_ref(), "date")
        .map_err(ToolError::InvalidArgs)?;
    let operation = args
        .required_string(args.date_op.as_ref(), "date_op")
        .map_err(ToolError::InvalidArgs)?;
    let amount = args
        .required_i64(args.amount, "amount")
        .map_err(ToolError::InvalidArgs)?;
    let unit = args
        .required_string(args.unit.as_ref(), "unit")
        .map_err(ToolError::InvalidArgs)?;
    let date = parse_date(&date, "calendar.date_arith")?;
    let result = apply(date, &operation, amount, &unit)?;

    Ok(json!({
        "result_date": date_string(result),
        "weekday": weekday_name(result),
    }))
}

fn apply(
    date: chrono::NaiveDate,
    operation: &str,
    amount: i64,
    unit: &str,
) -> Result<chrono::NaiveDate, ToolError> {
    let operation = checked_operation(operation)?;
    let normalized = checked_amount(amount)?;
    match (operation, unit) {
        (DateOperation::Add, "days") => date.checked_add_days(Days::new(normalized)),
        (DateOperation::Sub, "days") => date.checked_sub_days(Days::new(normalized)),
        (DateOperation::Add, "weeks") => {
            date.checked_add_days(Days::new(normalized.saturating_mul(7)))
        }
        (DateOperation::Sub, "weeks") => {
            date.checked_sub_days(Days::new(normalized.saturating_mul(7)))
        }
        (DateOperation::Add, "months") => date.checked_add_months(Months::new(to_u32(normalized)?)),
        (DateOperation::Sub, "months") => date.checked_sub_months(Months::new(to_u32(normalized)?)),
        (DateOperation::Add, "years") => {
            date.checked_add_months(Months::new(year_months(normalized)?))
        }
        (DateOperation::Sub, "years") => {
            date.checked_sub_months(Months::new(year_months(normalized)?))
        }
        (_, _) => {
            return Err(ToolError::InvalidArgs(
                "calendar.date_arith: unit must be days, weeks, months, or years".into(),
            ));
        }
    }
    .ok_or_else(|| ToolError::InvalidArgs("calendar.date_arith: date out of range".into()))
}

fn checked_operation(operation: &str) -> Result<DateOperation, ToolError> {
    match operation {
        "add" => Ok(DateOperation::Add),
        "sub" => Ok(DateOperation::Sub),
        _ => Err(ToolError::InvalidArgs(
            "calendar.date_arith: date_op must be add or sub".into(),
        )),
    }
}

fn checked_amount(amount: i64) -> Result<u64, ToolError> {
    if amount < 0 {
        return Err(ToolError::InvalidArgs(
            "calendar.date_arith: amount must be non-negative".into(),
        ));
    }
    u64::try_from(amount)
        .map_err(|err| ToolError::InvalidArgs(format!("calendar.date_arith: amount: {err}")))
}

fn to_u32(amount: u64) -> Result<u32, ToolError> {
    u32::try_from(amount)
        .map_err(|err| ToolError::InvalidArgs(format!("calendar.date_arith: amount: {err}")))
}

fn year_months(years: u64) -> Result<u32, ToolError> {
    let months = years
        .checked_mul(12)
        .ok_or_else(|| ToolError::InvalidArgs("calendar.date_arith: amount out of range".into()))?;
    to_u32(months)
}
