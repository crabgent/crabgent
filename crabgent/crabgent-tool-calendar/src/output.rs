//! JSON output helpers.

use chrono::{Datelike, NaiveDate};
use serde_json::{Value, json};

#[must_use]
pub fn date_string(date: NaiveDate) -> String {
    date.format("%Y-%m-%d").to_string()
}

#[must_use]
pub fn weekday_name(date: NaiveDate) -> String {
    date.format("%A").to_string()
}

#[must_use]
pub fn weekday_info(date: NaiveDate) -> Value {
    json!({
        "weekday_name": weekday_name(date),
        "weekday_number_iso": date.weekday().number_from_monday(),
        "iso_week": date.iso_week().week(),
        "iso_year": date.iso_week().year(),
        "is_weekend": date.weekday().number_from_monday() >= 6,
    })
}
