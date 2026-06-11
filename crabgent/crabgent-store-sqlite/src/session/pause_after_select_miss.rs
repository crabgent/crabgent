use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use sqlx::sqlite::SqlitePool;
use tokio::sync::Barrier;

use crabgent_core::{MemoryScope, Owner, ThreadId};
use crabgent_store::StoreError;

use crate::retry::map_sqlx_error;

#[derive(Clone)]
struct PauseAfterSelectMissHook {
    barrier: Arc<Barrier>,
    release: Arc<Barrier>,
}

pub struct PauseAfterSelectMissGuard {
    owner: Owner,
}

pub fn arm_pause_after_select_miss(
    owner: &Owner,
    barrier: Arc<Barrier>,
    release: Arc<Barrier>,
) -> PauseAfterSelectMissGuard {
    pause_hooks()
        .lock()
        .expect("pause-after-select-miss hooks mutex should not be poisoned")
        .insert(owner.clone(), PauseAfterSelectMissHook { barrier, release });
    PauseAfterSelectMissGuard {
        owner: owner.clone(),
    }
}

impl Drop for PauseAfterSelectMissGuard {
    fn drop(&mut self) {
        pause_hooks()
            .lock()
            .expect("pause-after-select-miss hooks mutex should not be poisoned")
            .remove(&self.owner);
    }
}

pub(super) async fn pause_after_select_miss_if_configured(
    pool: &SqlitePool,
    owner: &Owner,
    thread: Option<&ThreadId>,
    scope: &MemoryScope,
) -> Result<(), StoreError> {
    let Some(hook) = hook_for(owner) else {
        return Ok(());
    };

    let thread_s = thread.map(ThreadId::as_str);
    let row = sqlx::query_scalar::<_, i64>(
        "SELECT 1 FROM sessions \
         WHERE owner = ? \
         AND ((? IS NULL AND thread IS NULL) OR thread = ?) \
         AND ((? IS NULL AND scope_channel IS NULL) OR scope_channel = ?) \
         AND ((? IS NULL AND scope_conv IS NULL) OR scope_conv = ?) \
         AND ((? IS NULL AND scope_agent IS NULL) OR scope_agent = ?) \
         AND ((? IS NULL AND scope_kind IS NULL) OR scope_kind = ?) \
         LIMIT 1",
    )
    .bind(owner.as_str())
    .bind(thread_s)
    .bind(thread_s)
    .bind(scope.channel.as_deref())
    .bind(scope.channel.as_deref())
    .bind(scope.conv.as_deref())
    .bind(scope.conv.as_deref())
    .bind(scope.agent.as_deref())
    .bind(scope.agent.as_deref())
    .bind(scope.kind.as_deref())
    .bind(scope.kind.as_deref())
    .fetch_optional(pool)
    .await
    .map_err(|e| map_sqlx_error("session.find_or_create.preselect", &e))?;

    if row.is_none() {
        hook.barrier.wait().await;
        hook.release.wait().await;
    }
    Ok(())
}

fn hook_for(owner: &Owner) -> Option<PauseAfterSelectMissHook> {
    pause_hooks()
        .lock()
        .expect("pause-after-select-miss hooks mutex should not be poisoned")
        .get(owner)
        .cloned()
}

fn pause_hooks() -> &'static Mutex<HashMap<Owner, PauseAfterSelectMissHook>> {
    static PAUSE_HOOKS: OnceLock<Mutex<HashMap<Owner, PauseAfterSelectMissHook>>> = OnceLock::new();
    PAUSE_HOOKS.get_or_init(|| Mutex::new(HashMap::new()))
}
