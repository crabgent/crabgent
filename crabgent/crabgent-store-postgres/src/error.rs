//! Error type for opening and migrating the Postgres store.

use crabgent_store::StoreError;
use thiserror::Error;

/// Errors emitted by Postgres pool setup before they are mapped to
/// [`StoreError`].
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum PostgresStoreError {
    /// Pool failed to initialise.
    #[error("connect failed: {0}")]
    Connect(#[source] sqlx::Error),

    /// Schema migration failed mid-run.
    #[error("migration failed: {0}")]
    Migration(String),
}

impl PostgresStoreError {
    /// Convert to the public store error surface.
    pub fn into_store_error(self) -> StoreError {
        match self {
            Self::Connect(err) => crate::retry::map_sqlx_error("postgres.open", &err),
            Self::Migration(err) => StoreError::Backend(err),
        }
    }
}
