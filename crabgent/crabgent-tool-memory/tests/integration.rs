use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use crabgent_core::{Action, MemoryId, PolicyDecision, PolicyHook, Subject, Tool, ToolCtx};
use crabgent_store::{MemoryMemoryStore, MemoryStore};
use crabgent_tool_memory::{ArchiveTool, ForgetTool, MemoryTool};
use serde_json::json;

struct RecordingPolicy {
    seen: Mutex<Vec<String>>,
}

#[async_trait]
impl PolicyHook for RecordingPolicy {
    async fn allow(&self, _subject: &Subject, action: &Action) -> PolicyDecision {
        self.seen
            .lock()
            .expect("mutex should not be poisoned")
            .push(action.name().to_owned());
        PolicyDecision::Allow
    }
}

fn dyn_store(store: Arc<MemoryMemoryStore>) -> Arc<dyn MemoryStore> {
    store
}

fn dyn_policy(policy: Arc<RecordingPolicy>) -> Arc<dyn PolicyHook> {
    policy
}

fn ctx() -> ToolCtx {
    ToolCtx::new(Subject::new("alice"))
}

#[tokio::test]
async fn policy_gating_chain_covers_memory_tool_flow() {
    let store = Arc::new(MemoryMemoryStore::default());
    let policy = Arc::new(RecordingPolicy {
        seen: Mutex::new(Vec::new()),
    });
    let memory = MemoryTool::new(dyn_store(store.clone()), dyn_policy(policy.clone()), None);
    let archive = ArchiveTool::new(dyn_store(store.clone()), dyn_policy(policy.clone()));
    let forget = ForgetTool::new(dyn_store(store.clone()), dyn_policy(policy.clone()));

    let stored = memory
        .execute(
            json!({
                "op": "store",
                "scope": {"owner": "alice"},
                "body": "semantic policy flow",
                "class": "semantic",
                "importance": 0.6
            }),
            &ctx(),
        )
        .await
        .expect("test result");
    let id: MemoryId = stored["id"]
        .as_str()
        .expect("value should be a string")
        .parse()
        .expect("value should parse");

    archive
        .execute(
            json!({
                "scope": {"owner": "alice"},
                "doc_id": id.to_string()
            }),
            &ctx(),
        )
        .await
        .expect("test result");

    let archived = memory
        .execute(
            json!({
                "op": "search",
                "scope": {"owner": "alice"},
                "query": "semantic",
                "include_archived": true
            }),
            &ctx(),
        )
        .await
        .expect("test result");
    assert_eq!(archived["count"], 1);

    forget
        .execute(
            json!({
                "scope": {"owner": "alice"},
                "doc_id": id.to_string()
            }),
            &ctx(),
        )
        .await
        .expect("test result");
    assert!(store.get(&id).await.expect("test result").is_none());

    assert_eq!(
        *policy.seen.lock().expect("mutex should not be poisoned"),
        vec![
            "memory.store",
            "memory.archive",
            "memory.search",
            "memory.delete"
        ]
    );
}
