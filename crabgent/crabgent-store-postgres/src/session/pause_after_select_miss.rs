use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use tokio::sync::Barrier;

use crabgent_core::Owner;

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

pub(super) async fn pause_after_select_miss_if_configured(owner: &Owner) {
    if let Some(hook) = hook_for(owner) {
        hook.barrier.wait().await;
        hook.release.wait().await;
    }
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
