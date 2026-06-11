//! Retry and error-mapping helpers for Postgres-backed stores.

use std::future::Future;
use std::time::Duration;

use crabgent_log::warn;
use crabgent_store::StoreError;

const MAX_ATTEMPTS: usize = 5;
const BASE_DELAY_MS: u64 = 50;
const TRANSIENT_CODES: &[&str] = &["40001", "40P01", "53300", "57P03"];
const AUTH_CODES: &[&str] = &["28P01", "28000"];

/// True if the underlying error is worth retrying.
#[must_use]
pub fn is_transient(err: &sqlx::Error) -> bool {
    match err {
        sqlx::Error::Database(db) => is_transient_code(db.code().as_deref()),
        sqlx::Error::Io(_) | sqlx::Error::PoolTimedOut => true,
        _ => false,
    }
}

/// Map an `sqlx::Error` to a [`StoreError`], redacting auth failures.
#[must_use]
pub fn map_sqlx_error(name: &str, err: &sqlx::Error) -> StoreError {
    match err {
        sqlx::Error::Database(db) => map_database_error(name, db.as_ref(), err),
        sqlx::Error::Io(_) | sqlx::Error::PoolTimedOut => transient_error(name),
        sqlx::Error::PoolClosed => pool_closed_error(),
        _ => generic_backend_error(name, err),
    }
}

fn map_database_error(
    name: &str,
    db: &dyn sqlx::error::DatabaseError,
    err: &sqlx::Error,
) -> StoreError {
    let code = db.code();
    let sqlstate = code.as_deref();
    if is_auth_database_error(sqlstate, db.message()) {
        warn!(operation = name, "postgres authentication failed");
        return postgres_unavailable_error();
    }
    map_database_status(name, sqlstate, err)
}

fn map_database_status(name: &str, sqlstate: Option<&str>, err: &sqlx::Error) -> StoreError {
    if sqlstate == Some("23505") {
        return StoreError::Conflict(format!("{name}: unique constraint violated"));
    }
    if is_transient_code(sqlstate) {
        return transient_error(name);
    }
    // Log only the SQLSTATE code, never the full `sqlx::Error` Display: the
    // server error body can echo DSN/connection fragments into the logs.
    warn!(
        operation = name,
        sqlstate,
        error_kind = error_kind(err),
        "postgres database error"
    );
    postgres_backend_error()
}

/// A coarse, secret-free discriminant for an `sqlx::Error`. The full `Display`
/// can echo server error bodies (DSN, connection fragments), so logging paths
/// use this instead.
const fn error_kind(err: &sqlx::Error) -> &'static str {
    match err {
        sqlx::Error::Database(_) => "database",
        sqlx::Error::Io(_) => "io",
        sqlx::Error::PoolTimedOut => "pool_timed_out",
        sqlx::Error::PoolClosed => "pool_closed",
        sqlx::Error::RowNotFound => "row_not_found",
        sqlx::Error::ColumnNotFound(_) => "column_not_found",
        sqlx::Error::ColumnDecode { .. } => "column_decode",
        sqlx::Error::Decode(_) => "decode",
        sqlx::Error::Protocol(_) => "protocol",
        sqlx::Error::Tls(_) => "tls",
        sqlx::Error::Configuration(_) => "configuration",
        _ => "other",
    }
}

fn is_transient_code(sqlstate: Option<&str>) -> bool {
    sqlstate.is_some_and(|code| TRANSIENT_CODES.contains(&code))
}

fn is_auth_database_error(sqlstate: Option<&str>, message: &str) -> bool {
    sqlstate.is_some_and(|code| AUTH_CODES.contains(&code))
        || message.contains("password authentication failed")
}

fn transient_error(name: &str) -> StoreError {
    StoreError::Transient(format!("{name}: postgres transient error"))
}

fn postgres_unavailable_error() -> StoreError {
    StoreError::Backend("postgres connection unavailable".to_owned())
}

fn pool_closed_error() -> StoreError {
    StoreError::Backend("pool closed".to_owned())
}

fn generic_backend_error(name: &str, err: &sqlx::Error) -> StoreError {
    // Same redaction as `map_database_status`: log the kind, not the Display.
    warn!(
        operation = name,
        error_kind = error_kind(err),
        "postgres backend error"
    );
    postgres_backend_error()
}

fn postgres_backend_error() -> StoreError {
    StoreError::Backend("postgres backend error".to_owned())
}

/// Run `op` up to [`MAX_ATTEMPTS`] times.
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
                    "transient postgres error, retrying"
                );
                last = Some(err);
                tokio::time::sleep(Duration::from_millis(BASE_DELAY_MS * attempt as u64)).await;
            }
            Err(err) => return Err(map_sqlx_error(name, &err)),
        }
    }
    let Some(last) = last.as_ref() else {
        return Err(postgres_backend_error());
    };
    Err(map_sqlx_error(name, last))
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;
    use std::error::Error;
    use std::fmt;

    use sqlx::error::{DatabaseError, ErrorKind};

    use super::*;

    #[derive(Debug)]
    struct FakeDatabaseError {
        message: &'static str,
        code: &'static str,
    }

    impl fmt::Display for FakeDatabaseError {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str(self.message)
        }
    }

    impl Error for FakeDatabaseError {}

    impl DatabaseError for FakeDatabaseError {
        fn message(&self) -> &str {
            self.message
        }

        fn code(&self) -> Option<Cow<'_, str>> {
            Some(Cow::Borrowed(self.code))
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
            if self.code == "23505" {
                ErrorKind::UniqueViolation
            } else {
                ErrorKind::Other
            }
        }
    }

    fn db_error(code: &'static str) -> sqlx::Error {
        sqlx::Error::Database(Box::new(FakeDatabaseError {
            message: "postgres test error",
            code,
        }))
    }

    #[test]
    fn retry_unit_pool_timeout_is_transient() {
        assert!(matches!(
            map_sqlx_error("test.pool", &sqlx::Error::PoolTimedOut),
            StoreError::Transient(_)
        ));
    }

    #[test]
    fn retry_unit_io_error_is_transient() {
        let err = sqlx::Error::Io(std::io::Error::new(
            std::io::ErrorKind::ConnectionReset,
            "socket reset",
        ));

        assert!(matches!(
            map_sqlx_error("test.io", &err),
            StoreError::Transient(_)
        ));
    }

    #[test]
    fn retry_unit_auth_error_redacts_dsn() {
        let err = sqlx::Error::Database(Box::new(FakeDatabaseError {
            message: "password authentication failed for user postgres",
            code: "28P01",
        }));

        let mapped = map_sqlx_error("test.auth", &err);

        assert!(matches!(
            mapped,
            StoreError::Backend(ref msg) if msg == "postgres connection unavailable"
        ));
        assert!(!mapped.to_string().contains("28P01"));
        assert!(!mapped.to_string().contains("password"));
    }

    #[test]
    fn retry_unit_conflict_maps_to_conflict() {
        assert!(matches!(
            map_sqlx_error("test.conflict", &db_error("23505")),
            StoreError::Conflict(_)
        ));
    }
}
