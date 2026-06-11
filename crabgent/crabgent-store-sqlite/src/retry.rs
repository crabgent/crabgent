//! Retry helper for transient `SQLite` errors (mostly `SQLITE_BUSY`).
//!
//! `SQLite` returns `SQLITE_BUSY` when a writer collides with another writer
//! and the busy-timeout elapsed without a slot opening. With WAL+`busy_timeout`
//! configured the error is rare, but cron-claim contention or large
//! cleanup runs can still hit it. The helper retries up to 5 times with
//! linear backoff.

use std::future::Future;
use std::time::Duration;

use crabgent_log::warn;
use crabgent_store::StoreError;
use sqlx::error::ErrorKind;

const MAX_ATTEMPTS: usize = 5;
const BASE_DELAY_MS: u64 = 50;

/// True if the underlying error is worth retrying.
pub fn is_transient(err: &sqlx::Error) -> bool {
    matches!(
        err,
        sqlx::Error::Database(db) if matches!(db.code().as_deref(), Some("5" | "6"))
    )
}

/// Map an `sqlx::Error` to a [`StoreError`].
///
/// Transient busy/locked codes (5/6) promote to [`StoreError::Transient`].
/// Unique/primary-key constraint violations map to [`StoreError::Conflict`]
/// with an opaque message (no table or column name), mirroring the Postgres
/// SQLSTATE 23505 mapping in `crabgent-store-postgres`. Everything else falls
/// through to [`StoreError::Backend`].
pub fn map_sqlx_error(name: &str, err: &sqlx::Error) -> StoreError {
    if is_transient(err) {
        return StoreError::Transient(format!("{name}: {err}"));
    }
    if let sqlx::Error::Database(db) = err
        && db.kind() == ErrorKind::UniqueViolation
    {
        return StoreError::Conflict(format!("{name}: unique constraint violated"));
    }
    StoreError::Backend(format!("{name}: {err}"))
}

/// Run `op` up to [`MAX_ATTEMPTS`] times. Returns on first success or after
/// the last attempt's error if every retry was transient.
pub async fn retry_transient<T, F, Fut>(name: &str, mut op: F) -> Result<T, StoreError>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, sqlx::Error>>,
{
    let mut last: Option<sqlx::Error> = None;
    for attempt in 1..=MAX_ATTEMPTS {
        match op().await {
            Ok(value) => return Ok(value),
            Err(err) if is_transient(&err) => {
                warn!(
                    operation = name,
                    attempt,
                    max = MAX_ATTEMPTS,
                    error = %err,
                    "transient sqlite error, retrying"
                );
                last = Some(err);
                tokio::time::sleep(Duration::from_millis(BASE_DELAY_MS * attempt as u64)).await;
            }
            Err(err) => return Err(map_sqlx_error(name, &err)),
        }
    }
    Err(map_sqlx_error(
        name,
        &last.expect("retry loop must have observed at least one error"),
    ))
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;
    use std::error::Error;
    use std::fmt;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use sqlx::error::DatabaseError;

    use super::*;

    /// Fake `DatabaseError` whose `Display` carries a schema-revealing message
    /// (table.column) so the test can prove the opaque mapping strips it.
    #[derive(Debug)]
    struct FakeUniqueViolation {
        message: &'static str,
    }

    impl fmt::Display for FakeUniqueViolation {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str(self.message)
        }
    }

    impl Error for FakeUniqueViolation {}

    impl DatabaseError for FakeUniqueViolation {
        fn message(&self) -> &str {
            self.message
        }

        fn code(&self) -> Option<Cow<'_, str>> {
            // SQLITE_CONSTRAINT_UNIQUE extended code.
            Some(Cow::Borrowed("2067"))
        }

        fn as_error(&self) -> &(dyn Error + Send + Sync + 'static) {
            self
        }

        fn as_error_mut(&mut self) -> &mut (dyn Error + Send + Sync + 'static) {
            self
        }

        fn into_error(self: Box<Self>) -> Box<dyn Error + Send + Sync + 'static> {
            self
        }

        fn kind(&self) -> ErrorKind {
            ErrorKind::UniqueViolation
        }
    }

    #[tokio::test]
    async fn retry_transient_returns_first_success() {
        let calls = AtomicUsize::new(0);
        let result: Result<i32, StoreError> = retry_transient("test.success", || async {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok::<i32, sqlx::Error>(42)
        })
        .await;
        assert_eq!(result.expect("test result"), 42);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn retry_transient_propagates_non_transient() {
        let result: Result<i32, StoreError> = retry_transient("test.row", || async {
            Err::<i32, sqlx::Error>(sqlx::Error::RowNotFound)
        })
        .await;
        assert!(matches!(
            result.expect_err("expected error"),
            StoreError::Backend(_)
        ));
    }

    #[test]
    fn is_transient_only_for_busy_or_locked_codes() {
        assert!(!is_transient(&sqlx::Error::RowNotFound));
        assert!(!is_transient(&sqlx::Error::WorkerCrashed));
    }

    #[test]
    fn map_unique_violation_to_opaque_conflict() {
        let err = sqlx::Error::Database(Box::new(FakeUniqueViolation {
            message: "UNIQUE constraint failed: sessions.owner",
        }));

        let mapped = map_sqlx_error("session_insert", &err);

        assert!(
            matches!(mapped, StoreError::Conflict(_)),
            "unique violation must map to Conflict, got {mapped:?}"
        );
        let msg = mapped.to_string();
        // The opaque message must not leak the table or column name from the
        // raw sqlx Display ("UNIQUE constraint failed: sessions.owner").
        assert!(!msg.contains("sessions"), "leaked table name: {msg}");
        assert!(!msg.contains("owner"), "leaked column name: {msg}");
        assert!(!msg.contains('.'), "leaked qualified identifier: {msg}");
    }
}
