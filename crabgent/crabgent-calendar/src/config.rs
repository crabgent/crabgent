//! Configuration for [`TimeHintHook`](crate::TimeHintHook).

use std::sync::Arc;

use chrono::{DateTime, Utc};
use chrono_tz::Tz;

/// Clock used by the time hint hook and calendar tool.
pub type Clock = Arc<dyn Fn() -> DateTime<Utc> + Send + Sync>;

/// Locale, timezone, and clock configuration for [`TimeHintHook`](crate::TimeHintHook).
#[derive(Clone)]
pub struct TimeHintConfig {
    pub country: String,
    pub subdivision: String,
    pub upcoming_count: usize,
    pub timezone: Tz,
    pub clock: Clock,
}

impl Default for TimeHintConfig {
    fn default() -> Self {
        Self {
            country: "DE".into(),
            subdivision: "NW".into(),
            upcoming_count: 3,
            timezone: chrono_tz::Europe::Berlin,
            clock: Arc::new(Utc::now),
        }
    }
}

impl TimeHintConfig {
    #[must_use]
    pub fn with_country(mut self, country: impl Into<String>) -> Self {
        self.country = country.into();
        self
    }

    #[must_use]
    pub fn with_subdivision(mut self, subdivision: impl Into<String>) -> Self {
        self.subdivision = subdivision.into();
        self
    }

    #[must_use]
    pub const fn with_upcoming_count(mut self, upcoming_count: usize) -> Self {
        self.upcoming_count = upcoming_count;
        self
    }

    #[must_use]
    pub const fn with_timezone(mut self, timezone: Tz) -> Self {
        self.timezone = timezone;
        self
    }

    #[must_use]
    pub fn with_clock(mut self, clock: Clock) -> Self {
        self.clock = clock;
        self
    }
}
