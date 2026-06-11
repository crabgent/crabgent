//! Configuration for opening a Postgres store.

use std::fmt;
use std::time::Duration;

use secrecy::SecretString;
use sqlx::postgres::PgSslMode;
use url::Url;

/// Default vector embedding dimension used by store constructors.
pub const DEFAULT_EMBEDDING_DIM: usize = 1024;

/// Connection configuration for [`crate::PostgresStore`].
#[derive(Clone)]
pub struct PostgresStoreConfig {
    dsn: SecretString,
    redacted_dsn: String,
    max_connections: Option<u32>,
    acquire_timeout: Option<Duration>,
    ssl_mode: Option<PgSslMode>,
    embedding_dim: usize,
}

impl PostgresStoreConfig {
    /// Build a config from a raw DSN string supplied by the caller.
    pub fn new(dsn: impl Into<String>) -> Self {
        let dsn = dsn.into();
        let redacted_dsn = redact_dsn(&dsn);
        Self {
            dsn: SecretString::from(dsn),
            redacted_dsn,
            max_connections: None,
            acquire_timeout: None,
            ssl_mode: None,
            embedding_dim: DEFAULT_EMBEDDING_DIM,
        }
    }

    /// Build a config from an already-secret DSN.
    pub fn from_secret(dsn: SecretString) -> Self {
        Self {
            dsn,
            redacted_dsn: "postgres://****".to_owned(),
            max_connections: None,
            acquire_timeout: None,
            ssl_mode: None,
            embedding_dim: DEFAULT_EMBEDDING_DIM,
        }
    }

    /// Start a builder for optional pool settings.
    pub fn builder(dsn: impl Into<String>) -> PostgresStoreConfigBuilder {
        PostgresStoreConfigBuilder {
            config: Self::new(dsn),
        }
    }

    pub(crate) const fn dsn(&self) -> &SecretString {
        &self.dsn
    }

    pub(crate) const fn max_connections(&self) -> Option<u32> {
        self.max_connections
    }

    pub(crate) const fn acquire_timeout(&self) -> Option<Duration> {
        self.acquire_timeout
    }

    pub(crate) const fn ssl_mode(&self) -> Option<PgSslMode> {
        self.ssl_mode
    }

    /// Vector embedding dimension used for the pgvector migration.
    #[must_use]
    pub const fn embedding_dim(&self) -> usize {
        self.embedding_dim
    }

    /// Override the default vector embedding dimension.
    #[must_use]
    pub const fn with_embedding_dim(mut self, embedding_dim: usize) -> Self {
        self.embedding_dim = embedding_dim;
        self
    }
}

impl fmt::Debug for PostgresStoreConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PostgresStoreConfig")
            .field("dsn", &self.redacted_dsn)
            .field("max_connections", &self.max_connections)
            .field("acquire_timeout", &self.acquire_timeout)
            .field("ssl_mode", &self.ssl_mode)
            .field("embedding_dim", &self.embedding_dim)
            .finish()
    }
}

/// Builder for optional pool settings.
#[derive(Debug)]
pub struct PostgresStoreConfigBuilder {
    config: PostgresStoreConfig,
}

impl PostgresStoreConfigBuilder {
    /// Override the sqlx pool maximum.
    #[must_use]
    pub const fn max_connections(mut self, max_connections: u32) -> Self {
        self.config.max_connections = Some(max_connections);
        self
    }

    /// Override the sqlx acquire timeout.
    #[must_use]
    pub const fn acquire_timeout(mut self, acquire_timeout: Duration) -> Self {
        self.config.acquire_timeout = Some(acquire_timeout);
        self
    }

    /// Override SSL mode parsed from the DSN.
    #[must_use]
    pub const fn ssl_mode(mut self, ssl_mode: PgSslMode) -> Self {
        self.config.ssl_mode = Some(ssl_mode);
        self
    }

    /// Override the default vector embedding dimension.
    #[must_use]
    pub const fn embedding_dim(mut self, embedding_dim: usize) -> Self {
        self.config.embedding_dim = embedding_dim;
        self
    }

    /// Return the final config.
    #[must_use]
    pub fn build(self) -> PostgresStoreConfig {
        self.config
    }
}

fn redact_dsn(raw: &str) -> String {
    Url::parse(raw).map_or_else(
        |_| "postgres://****".to_owned(),
        |mut url| {
            if url.set_password(Some("****")).is_err() {
                return "postgres://****".to_owned();
            }
            if !url.username().is_empty() && url.set_username("****").is_err() {
                return "postgres://****".to_owned();
            }
            url.to_string()
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_masks_credentials() {
        let cfg = PostgresStoreConfig::new(
            "postgres://user:secret-test-password-99999@localhost:5432/crabgent",
        );

        let rendered = format!("{cfg:?}");

        assert!(!rendered.contains("secret-test-password-99999"));
        assert!(!rendered.contains("user:"));
        assert!(rendered.contains("****"));
    }

    #[test]
    fn secret_constructor_uses_generic_redaction() {
        let cfg = PostgresStoreConfig::from_secret(SecretString::from(
            "postgres://user:secret@localhost:5432/crabgent".to_owned(),
        ));

        assert!(format!("{cfg:?}").contains("postgres://****"));
    }

    #[test]
    fn embedding_dim_defaults_to_compat_value() {
        let cfg = PostgresStoreConfig::new("postgres://localhost/crabgent");

        assert_eq!(cfg.embedding_dim(), DEFAULT_EMBEDDING_DIM);
    }

    #[test]
    fn embedding_dim_can_be_overridden_by_builder_or_config() {
        let from_builder = PostgresStoreConfig::builder("postgres://localhost/crabgent")
            .embedding_dim(8)
            .build();
        let from_config =
            PostgresStoreConfig::new("postgres://localhost/crabgent").with_embedding_dim(16);

        assert_eq!(from_builder.embedding_dim(), 8);
        assert_eq!(from_config.embedding_dim(), 16);
    }
}
