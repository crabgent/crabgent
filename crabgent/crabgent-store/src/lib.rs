//! # crabgent-store
//!
//! Trait surfaces and records for crabgent persistence (sessions,
//! memory, tasks, cron jobs, tool cache) plus an in-memory backend for
//! tests and solo runs.
//!
//! Concrete persistent backends (`SQLite`, Postgres) live in companion
//! crates that depend on this one.

pub mod error;
pub mod ids;
pub mod memory_search;
pub mod page;
pub mod records;
pub mod relation_type;
pub mod scope_query;
pub mod session_support;
pub mod traits;

#[cfg(feature = "memory")]
pub mod memory;

pub use chrono::{DateTime, Utc};
pub use crabgent_core::{Owner, ThreadId};
pub use error::StoreError;
pub use ids::{ArchiveId, CronJobId, GoalId, ParseIdError, RelationId, SessionId, TaskId};
pub use page::Page;
pub use records::{
    CronJob, CronJobUpdate, CronSchedule, GoalStatus, MAX_GOAL_OBJECTIVE_CHARS, MemoryDoc,
    MemoryHit, MemoryRelation, ModelTargetDto, ParseGoalStatusError, ParseTaskPauseCauseError,
    ParseTaskStatusError, Session, SessionArchiveEntry, SessionInfo, SessionSearchHit, Task,
    TaskPauseCause, TaskResumeSpec, TaskStatus, ThreadGoal, ThreadGoalUpdate, ToolCacheEntry,
    deserialize_model_target_dto, serialize_model_target_dto, validate_goal_budget,
    validate_goal_objective,
};
pub use relation_type::{MAX_RELATION_TYPE_LEN, RelationType, RelationTypeError};
pub use traits::{
    CronStore, GoalStore, MemoryStore, SessionStore, Store, TaskStore, ToolCacheStore,
};

#[cfg(feature = "memory")]
pub use memory::MemoryTaskStore;
#[cfg(feature = "memory")]
pub use memory::{InMemoryStore, MemoryGlobalOverrideStore, MemoryGoalStore, MemoryMemoryStore};
