//! Parent-task depth validation for nested task creation.

use std::collections::HashSet;
use std::fmt;

use crabgent_store::{StoreError, TaskId, TaskStore};

pub const MAX_DEPTH_WALK_ITERATIONS: usize = 16;

#[derive(Debug, Clone, Copy)]
pub struct DepthLimitExceeded;

impl fmt::Display for DepthLimitExceeded {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("nested task depth limit reached")
    }
}

impl std::error::Error for DepthLimitExceeded {}

pub enum DepthCheckError {
    Limit(DepthLimitExceeded),
    Store(StoreError),
}

impl From<StoreError> for DepthCheckError {
    fn from(value: StoreError) -> Self {
        Self::Store(value)
    }
}

/// Reject creating a task whose parent chain is already too deep.
///
/// `max_depth` counts the total levels in a chain, including the task that is
/// about to be created and counting the root as level 1. The walk only sees
/// the *existing* ancestors (`parent_task_id` and its transitive parents); the
/// new task is not yet stored and is not walked. A new task may therefore have
/// at most `max_depth - 1` existing ancestors.
///
/// With the default `max_depth = 3` (`DEFAULT_MAX_DEPTH`): a root (level 1),
/// its child (level 2), and that child's child (level 3) are all allowed. The
/// `>=` boundary fires when the walk counts `max_depth` ancestors, which is
/// exactly the point where the new task would become level `max_depth + 1`.
/// So a task with three existing ancestors (a fourth nesting level) is denied.
///
/// A `None` `parent_task_id` is a root task and always passes. The walk is
/// bounded by [`MAX_DEPTH_WALK_ITERATIONS`] and a `seen` set, so a cyclic
/// parent chain in the store yields `DepthLimitExceeded` rather than looping.
pub async fn ensure_depth_allowed<S>(
    store: &S,
    parent_task_id: Option<&TaskId>,
    max_depth: usize,
) -> Result<(), DepthCheckError>
where
    S: TaskStore + ?Sized,
{
    let Some(first_parent) = parent_task_id else {
        return Ok(());
    };
    let mut seen = HashSet::new();
    let mut current = Some(first_parent.clone());
    let mut depth = 0usize;
    let mut iterations = 0usize;

    while let Some(id) = current {
        iterations += 1;
        if iterations > MAX_DEPTH_WALK_ITERATIONS || !seen.insert(id.clone()) {
            return Err(DepthCheckError::Limit(DepthLimitExceeded));
        }
        let Some(task) = store.get(&id).await? else {
            return Ok(());
        };
        depth += 1;
        if depth >= max_depth {
            return Err(DepthCheckError::Limit(DepthLimitExceeded));
        }
        current = task.parent_task_id;
    }
    Ok(())
}
