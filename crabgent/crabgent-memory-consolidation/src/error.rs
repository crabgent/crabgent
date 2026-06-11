//! Errors emitted by memory consolidation.

use crabgent_core::{MemoryScope, ProviderError};
use crabgent_memory::MemoryError;
use crabgent_store::StoreError;
use thiserror::Error;

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ConsolidationError {
    #[error("cancelled")]
    Cancelled,

    #[error("denied: {0}")]
    Denied(String),

    #[error("consolidation already running for scope: {0:?}")]
    AlreadyRunning(MemoryScope),

    #[error("provider error: {0}")]
    Provider(#[from] ProviderError),

    #[error("store error: {0}")]
    Store(#[from] StoreError),

    #[error("memory recall error: {0}")]
    MemoryRecall(#[from] MemoryError),

    #[error("subject resolver error: {0}")]
    SubjectResolver(String),
}
