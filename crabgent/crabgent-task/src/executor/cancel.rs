use crabgent_core::CancelReason;
use crabgent_store::TaskId;
use crabgent_store::traits::TaskStore;

use super::TaskExecutor;

pub(super) async fn do_cancel<S>(exec: &TaskExecutor<S>, id: &TaskId) -> bool
where
    S: TaskStore,
{
    let handles = exec.cancel_handles.lock().await.remove(id);
    if let Some(handles) = handles {
        // Per-task cancel is caller intent: stamp it (first-write-wins)
        // before firing the token so a concurrent shutdown classifies the
        // task as user-cancelled (Failed), never as pausable.
        let _rejected = handles.reason.set(CancelReason::StopPattern);
        handles.cancel.cancel();
        return true;
    }
    false
}
