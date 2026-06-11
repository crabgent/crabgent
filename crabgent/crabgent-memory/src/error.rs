//! Error type for memory-domain helpers.

use crabgent_store::StoreError;
use thiserror::Error;

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum MemoryError {
    #[error("invalid memory config: {0}")]
    InvalidConfig(String),

    #[error("invalid memory importance: {0}")]
    InvalidImportance(f32),

    #[error("unknown memory class: {0}")]
    ParseClass(String),

    #[error(transparent)]
    Store(#[from] StoreError),
}
