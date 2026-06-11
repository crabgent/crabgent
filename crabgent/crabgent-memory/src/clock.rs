//! Clock abstraction for deterministic memory tests.

#[cfg(any(test, feature = "test-helpers"))]
use std::sync::{Arc, Mutex};

#[cfg(any(test, feature = "test-helpers"))]
use chrono::Duration;
use chrono::{DateTime, Utc};

pub trait Clock: Send + Sync {
    fn now(&self) -> DateTime<Utc>;
}

#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }
}

#[cfg(any(test, feature = "test-helpers"))]
#[derive(Debug, Clone)]
pub struct MockClock {
    current: Arc<Mutex<DateTime<Utc>>>,
}

#[cfg(any(test, feature = "test-helpers"))]
impl MockClock {
    pub fn new(now: DateTime<Utc>) -> Self {
        Self {
            current: Arc::new(Mutex::new(now)),
        }
    }

    pub fn set_now(&self, now: DateTime<Utc>) {
        let mut current = self.current.lock().expect("mock clock mutex not poisoned");
        *current = now;
    }

    pub fn advance(&self, duration: Duration) {
        let mut current = self.current.lock().expect("mock clock mutex not poisoned");
        *current += duration;
    }
}

#[cfg(any(test, feature = "test-helpers"))]
impl Clock for MockClock {
    fn now(&self) -> DateTime<Utc> {
        *self.current.lock().expect("mock clock mutex not poisoned")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn mock_clock_advance_progresses() {
        let start = Utc
            .with_ymd_and_hms(2026, 5, 12, 8, 0, 0)
            .single()
            .expect("valid test datetime");
        let clock = MockClock::new(start);

        clock.advance(Duration::hours(2));

        assert_eq!(clock.now(), start + Duration::hours(2));
    }

    #[test]
    fn mock_clock_set_now_overwrites() {
        let start = Utc
            .with_ymd_and_hms(2026, 5, 12, 8, 0, 0)
            .single()
            .expect("valid test datetime");
        let next = Utc
            .with_ymd_and_hms(2026, 5, 13, 9, 30, 0)
            .single()
            .expect("valid test datetime");
        let clock = MockClock::new(start);

        clock.set_now(next);

        assert_eq!(clock.now(), next);
    }

    #[test]
    fn system_clock_increasing() {
        let clock = SystemClock;
        let first = clock.now();
        let second = clock.now();

        assert!(second >= first);
    }
}
