//! Holiday data source. The trait abstracts over backends; the
//! [`EmbeddedHolidayProvider`] is the default JSON-backed implementation.

use std::collections::HashMap;
use std::sync::OnceLock;

use chrono::{Datelike, NaiveDate};
use crabgent_log::warn;

/// Embedded combined dataset, shape:
/// `{ "DE": { "NW": { "2025-01-01": "Neujahr" }, ... }, ... }`.
const HOLIDAY_JSON: &str = include_str!("../data/holidays_combined.json");

/// Subdivision marker for entries that apply nation-wide.
const NATIONAL: &str = "National";

/// Look up holidays by date, country, and subdivision.
pub trait HolidayProvider: Send + Sync {
    /// Return the holiday name for the given date if any matches the
    /// requested country and subdivision (or the country's `National`
    /// subdivision).
    fn get_holiday(&self, date: NaiveDate, country: &str, subdivision: &str) -> Option<&str>;

    /// Return up to `count` upcoming holidays starting at `from`, paired with
    /// their dates. Implementations should de-duplicate when a national entry
    /// overlaps with a subdivision entry on the same date.
    fn upcoming_holidays(
        &self,
        from: NaiveDate,
        country: &str,
        subdivision: &str,
        count: usize,
    ) -> Vec<(NaiveDate, &str)>;

    /// Report whether `(country, subdivision)` is backed by data. A country is
    /// known when it has any entries; `National` is accepted for every known
    /// country, otherwise the subdivision must have at least one own entry.
    /// Callers use this to reject unknown subdivisions instead of silently
    /// falling back to the `National` list. Default returns `false` so a
    /// backend that cannot enumerate its data fails closed.
    fn has_subdivision(&self, _country: &str, _subdivision: &str) -> bool {
        false
    }

    /// Country codes that carry at least one subdivision-specific entry (beyond
    /// `National`). Callers turn this into a "subdivision required" hint. Empty
    /// by default; data-backed implementations override it.
    fn subdivision_countries(&self) -> Vec<&str> {
        Vec::new()
    }

    /// Inclusive `(min_year, max_year)` covered by the data, or `None` when the
    /// backend has no entries. Callers reflect these as the valid year range so
    /// an out-of-data-range year is a hard error, not a silent empty result.
    fn year_bounds(&self) -> Option<(i32, i32)> {
        None
    }
}

/// Default JSON-backed implementation. Parses the embedded dataset on first
/// access and answers lookups via binary search.
pub struct EmbeddedHolidayProvider;

impl EmbeddedHolidayProvider {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl Default for EmbeddedHolidayProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl HolidayProvider for EmbeddedHolidayProvider {
    fn get_holiday(&self, date: NaiveDate, country: &str, subdivision: &str) -> Option<&str> {
        let entries = data().get(country)?;
        let start = entries.partition_point(|e| e.date < date);
        for entry in entries.iter().skip(start) {
            if entry.date != date {
                break;
            }
            if entry.subdivision == subdivision || entry.subdivision == NATIONAL {
                return Some(entry.name.as_str());
            }
        }
        None
    }

    fn upcoming_holidays(
        &self,
        from: NaiveDate,
        country: &str,
        subdivision: &str,
        count: usize,
    ) -> Vec<(NaiveDate, &str)> {
        let Some(entries) = data().get(country) else {
            return Vec::new();
        };
        let start = entries.partition_point(|e| e.date < from);
        let mut result = Vec::with_capacity(count);
        let mut last_date: Option<NaiveDate> = None;
        for entry in entries.iter().skip(start) {
            if result.len() >= count {
                break;
            }
            if entry.subdivision != subdivision && entry.subdivision != NATIONAL {
                continue;
            }
            if last_date == Some(entry.date) {
                continue;
            }
            last_date = Some(entry.date);
            result.push((entry.date, entry.name.as_str()));
        }
        result
    }

    fn has_subdivision(&self, country: &str, subdivision: &str) -> bool {
        let Some(entries) = data().get(country) else {
            return false;
        };
        if subdivision == NATIONAL {
            return !entries.is_empty();
        }
        entries.iter().any(|entry| entry.subdivision == subdivision)
    }

    fn subdivision_countries(&self) -> Vec<&str> {
        let mut countries: Vec<&str> = data()
            .iter()
            .filter(|(_, entries)| entries.iter().any(|entry| entry.subdivision != NATIONAL))
            .map(|(country, _)| country.as_str())
            .collect();
        countries.sort_unstable();
        countries
    }

    fn year_bounds(&self) -> Option<(i32, i32)> {
        data()
            .values()
            .flat_map(|entries| entries.iter().map(|entry| entry.date.year()))
            .fold(None, |bounds, year| match bounds {
                None => Some((year, year)),
                Some((min, max)) => Some((min.min(year), max.max(year))),
            })
    }
}

struct HolidayEntry {
    date: NaiveDate,
    subdivision: String,
    name: String,
}

type CountryMap = HashMap<String, Vec<HolidayEntry>>;

fn data() -> &'static CountryMap {
    static DATA: OnceLock<CountryMap> = OnceLock::new();
    DATA.get_or_init(parse)
}

fn parse() -> CountryMap {
    let raw: HashMap<String, HashMap<String, HashMap<String, String>>> =
        match serde_json::from_str(HOLIDAY_JSON) {
            Ok(raw) => raw,
            Err(err) => {
                warn!(
                    error = %err,
                    "embedded holiday JSON parse failed, returning empty map"
                );
                return CountryMap::default();
            }
        };

    let mut map = HashMap::with_capacity(raw.len());
    for (country, subdivisions) in raw {
        let mut entries = Vec::new();
        for (subdiv, dates) in subdivisions {
            for (date_str, name) in dates {
                if let Ok(date) = NaiveDate::parse_from_str(&date_str, "%Y-%m-%d") {
                    entries.push(HolidayEntry {
                        date,
                        subdivision: subdiv.clone(),
                        name,
                    });
                }
            }
        }
        entries.sort_unstable_by(|a, b| {
            a.date
                .cmp(&b.date)
                .then_with(|| a.subdivision.cmp(&b.subdivision))
        });
        map.insert(country, entries);
    }
    map
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).expect("valid date in test")
    }

    #[test]
    fn german_unity_day_is_a_national_holiday() {
        let p = EmbeddedHolidayProvider::new();
        let name = p.get_holiday(d(2025, 10, 3), "DE", "NW");
        assert!(name.is_some(), "Oct 3 should be a holiday");
        let name = name.expect("test result");
        assert!(
            name.contains("Einheit") || name.contains("Unity"),
            "expected Tag der Deutschen Einheit, got {name:?}"
        );
    }

    #[test]
    fn christmas_is_holiday() {
        let p = EmbeddedHolidayProvider::new();
        assert!(p.get_holiday(d(2025, 12, 25), "DE", "NW").is_some());
    }

    #[test]
    fn random_tuesday_is_not_holiday() {
        let p = EmbeddedHolidayProvider::new();
        assert!(p.get_holiday(d(2025, 3, 11), "DE", "NW").is_none());
    }

    #[test]
    fn fronleichnam_is_nrw_only() {
        let p = EmbeddedHolidayProvider::new();
        assert!(
            p.get_holiday(d(2025, 6, 19), "DE", "NW").is_some(),
            "Fronleichnam should be a holiday in NRW"
        );
    }

    #[test]
    fn upcoming_returns_sorted_and_deduped() {
        let p = EmbeddedHolidayProvider::new();
        let holidays = p.upcoming_holidays(d(2025, 1, 1), "DE", "NW", 5);
        assert!(!holidays.is_empty());
        for window in holidays.windows(2) {
            assert!(window[0].0 < window[1].0, "must be strictly sorted");
        }
    }

    #[test]
    fn upcoming_respects_count_cap() {
        let p = EmbeddedHolidayProvider::new();
        let holidays = p.upcoming_holidays(d(2025, 1, 1), "DE", "NW", 2);
        assert_eq!(holidays.len(), 2);
    }

    #[test]
    fn unknown_country_returns_none() {
        let p = EmbeddedHolidayProvider::new();
        assert!(p.get_holiday(d(2025, 1, 1), "XX", "National").is_none());
        assert!(
            p.upcoming_holidays(d(2025, 1, 1), "XX", "National", 3)
                .is_empty()
        );
    }

    #[test]
    fn austrian_national_day() {
        let p = EmbeddedHolidayProvider::new();
        assert!(p.get_holiday(d(2025, 10, 26), "AT", "National").is_some());
    }

    #[test]
    fn french_bastille_day() {
        let p = EmbeddedHolidayProvider::new();
        assert!(p.get_holiday(d(2025, 7, 14), "FR", "National").is_some());
    }

    #[test]
    fn unknown_subdivision_falls_back_to_national() {
        let p = EmbeddedHolidayProvider::new();
        assert!(
            p.get_holiday(d(2025, 12, 25), "DE", "ZZ").is_some(),
            "unknown subdivision should still see National-tagged Christmas"
        );
    }

    #[test]
    fn default_constructor_yields_working_provider() {
        let p = <EmbeddedHolidayProvider as Default>::default();
        assert!(p.get_holiday(d(2025, 12, 25), "DE", "NW").is_some());
    }

    #[test]
    fn has_subdivision_accepts_known_pair_and_national() {
        let p = EmbeddedHolidayProvider::new();
        assert!(p.has_subdivision("DE", "NW"));
        assert!(p.has_subdivision("DE", "National"));
        // National-only country still answers for the National subdivision.
        assert!(p.has_subdivision("BG", "National"));
    }

    #[test]
    fn has_subdivision_rejects_unknown_country_or_subdivision() {
        let p = EmbeddedHolidayProvider::new();
        assert!(!p.has_subdivision("XX", "National"));
        assert!(!p.has_subdivision("DE", "ZZ"));
        // BG carries no own subdivisions, only National.
        assert!(!p.has_subdivision("BG", "Sofia"));
    }

    #[test]
    fn subdivision_countries_match_embedded_data() {
        let p = EmbeddedHolidayProvider::new();
        assert_eq!(
            p.subdivision_countries(),
            ["AT", "DE", "ES", "FI", "FR", "GB", "IT", "NO", "PT"]
        );
    }

    #[test]
    fn year_bounds_match_embedded_data() {
        let p = EmbeddedHolidayProvider::new();
        assert_eq!(p.year_bounds(), Some((2000, 2050)));
    }
}
