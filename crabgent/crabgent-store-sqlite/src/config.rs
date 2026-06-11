//! Configuration for opening a `SQLite` store.

/// Default vector embedding dimension used by store constructors.
pub const DEFAULT_EMBEDDING_DIM: usize = 1024;

/// Configuration for [`crate::SqliteStore`] construction.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SqliteStoreConfig {
    embedding_dim: usize,
}

impl Default for SqliteStoreConfig {
    fn default() -> Self {
        Self {
            embedding_dim: DEFAULT_EMBEDDING_DIM,
        }
    }
}

impl SqliteStoreConfig {
    /// Vector embedding dimension used for the sqlite-vec virtual table.
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

#[cfg(test)]
mod tests {
    use super::{DEFAULT_EMBEDDING_DIM, SqliteStoreConfig};

    #[test]
    fn embedding_dim_defaults_to_compat_value() {
        assert_eq!(
            SqliteStoreConfig::default().embedding_dim(),
            DEFAULT_EMBEDDING_DIM
        );
    }

    #[test]
    fn embedding_dim_can_be_overridden() {
        let cfg = SqliteStoreConfig::default().with_embedding_dim(8);

        assert_eq!(cfg.embedding_dim(), 8);
    }
}
