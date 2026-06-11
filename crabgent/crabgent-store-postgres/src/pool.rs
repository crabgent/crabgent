//! Pool construction for Postgres.

use std::str::FromStr;

use crabgent_log::error;
use crabgent_store::StoreError;
use secrecy::ExposeSecret;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use sqlx::{ConnectOptions, PgPool};

use crate::config::PostgresStoreConfig;
use crate::pgvector_migrate::apply_pgvector_migration;
use crate::retry::map_sqlx_error;
use crate::retry::retry_transient;

static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

pub async fn build_pool(config: &PostgresStoreConfig) -> Result<PgPool, StoreError> {
    let mut options = PgConnectOptions::from_str(config.dsn().expose_secret())
        .map_err(|err| map_sqlx_error("postgres.config", &err))?
        .log_statements(crabgent_log::LogLevelFilter::Debug);

    if let Some(ssl_mode) = config.ssl_mode() {
        options = options.ssl_mode(ssl_mode);
    }

    let mut pool_options = PgPoolOptions::new();
    if let Some(max_connections) = config.max_connections() {
        pool_options = pool_options.max_connections(max_connections);
    }
    if let Some(acquire_timeout) = config.acquire_timeout() {
        pool_options = pool_options.acquire_timeout(acquire_timeout);
    }

    let pool = retry_transient("postgres.connect", || {
        pool_options.clone().connect_with(options.clone())
    })
    .await
    .map_err(|err| match err {
        StoreError::Transient(_) => StoreError::Backend("postgres connection unavailable".into()),
        other => other,
    })?;

    MIGRATOR.run(&pool).await.map_err(|err| {
        error!(error = %err, "migration failed");
        StoreError::Backend("migration failed".to_owned())
    })?;

    apply_pgvector_migration(&pool, config.embedding_dim()).await?;

    Ok(pool)
}
