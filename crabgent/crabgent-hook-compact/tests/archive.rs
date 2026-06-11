use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use crabgent_core::{
    ContentBlock, Decision, Hook, LlmRequest, LlmResponse, MemoryScope, Message, ModelInfo, Owner,
    Provider, ProviderCapabilities, ProviderError, RunCtx, RunId, SearchQuery, StopReason, Subject,
    Usage,
};
use crabgent_hook_compact::CompactHook;
use crabgent_store::{
    ArchiveId, DateTime, Page, Session, SessionArchiveEntry, SessionId, SessionInfo,
    SessionSearchHit, SessionStore, StoreError, ThreadId, Utc,
};
use tokio_util::sync::CancellationToken;

struct SummaryProvider {
    requests: Arc<Mutex<Vec<LlmRequest>>>,
    order: Arc<Mutex<Vec<&'static str>>>,
}

impl SummaryProvider {
    fn new(order: Arc<Mutex<Vec<&'static str>>>) -> (Arc<Self>, Arc<Mutex<Vec<LlmRequest>>>) {
        let requests = Arc::new(Mutex::new(Vec::new()));
        (
            Arc::new(Self {
                requests: Arc::clone(&requests),
                order,
            }),
            requests,
        )
    }
}

#[async_trait]
impl Provider for SummaryProvider {
    async fn complete(
        &self,
        req: &LlmRequest,
        _ctx: &RunCtx,
        _cancel: Option<&CancellationToken>,
    ) -> Result<LlmResponse, ProviderError> {
        self.order.lock().expect("order lock").push("provider");
        self.requests
            .lock()
            .expect("requests lock")
            .push(req.clone());
        Ok(LlmResponse {
            text: "new summary".into(),
            tool_calls: Vec::new(),
            stop_reason: StopReason::EndTurn,
            usage: Usage::default(),
            model: req.model.clone(),
        })
    }

    fn name(&self) -> &'static str {
        "summary"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities::default()
    }

    fn models(&self) -> Vec<ModelInfo> {
        vec![ModelInfo::minimal("summary-model", "summary")]
    }
}

struct ArchiveStore {
    fail_archive: bool,
    order: Arc<Mutex<Vec<&'static str>>>,
    archives: ArchiveLog,
}

type ArchiveLog = Arc<Mutex<Vec<Vec<Message>>>>;

impl ArchiveStore {
    fn new(fail_archive: bool, order: Arc<Mutex<Vec<&'static str>>>) -> (Arc<Self>, ArchiveLog) {
        let archives = Arc::new(Mutex::new(Vec::new()));
        (
            Arc::new(Self {
                fail_archive,
                order,
                archives: Arc::clone(&archives),
            }),
            archives,
        )
    }
}

#[async_trait]
impl SessionStore for ArchiveStore {
    async fn find_or_create(
        &self,
        _owner: &Owner,
        _thread: Option<&ThreadId>,
        _scope: &MemoryScope,
    ) -> Result<Session, StoreError> {
        Err(StoreError::Unsupported)
    }

    async fn load(&self, _id: &SessionId) -> Result<Option<Session>, StoreError> {
        Ok(None)
    }

    async fn save(&self, _session: &Session) -> Result<(), StoreError> {
        Ok(())
    }

    async fn get_compaction_summary(&self, _id: &SessionId) -> Result<Option<String>, StoreError> {
        Ok(None)
    }

    async fn set_compaction_summary(
        &self,
        _id: &SessionId,
        _summary: &str,
    ) -> Result<(), StoreError> {
        Ok(())
    }

    async fn archive_messages(
        &self,
        _session_id: &SessionId,
        messages: &[Message],
        _created_at: DateTime<Utc>,
    ) -> Result<ArchiveId, StoreError> {
        self.order.lock().expect("order lock").push("archive");
        if self.fail_archive {
            return Err(StoreError::backend("archive failed"));
        }
        self.archives
            .lock()
            .expect("archives lock")
            .push(messages.to_vec());
        Ok(ArchiveId::new())
    }

    async fn list_archives(
        &self,
        _session_id: &SessionId,
        _page: Page,
    ) -> Result<Vec<SessionArchiveEntry>, StoreError> {
        Ok(Vec::new())
    }

    async fn cleanup_old_archives(&self, _days: i64) -> Result<u64, StoreError> {
        Ok(0)
    }

    async fn list(&self, _owner: &Owner, _page: Page) -> Result<Vec<SessionInfo>, StoreError> {
        Ok(Vec::new())
    }

    async fn cleanup_old(&self, _days: i64) -> Result<u64, StoreError> {
        Ok(0)
    }

    async fn search(&self, _query: &SearchQuery) -> Result<Vec<SessionSearchHit>, StoreError> {
        Ok(Vec::new())
    }
}

fn ctx_with_session(session_id: &SessionId) -> RunCtx {
    let ctx = RunCtx::new(RunId::new(), Subject::new("u"));
    ctx.set_session_id(session_id.to_string())
        .expect("session id should be set once");
    ctx
}

fn user(text: &str) -> Message {
    Message::User {
        content: vec![ContentBlock::Text { text: text.into() }],
        timestamp: None,
    }
}

fn first_archived_user_text(batches: &[Vec<Message>]) -> Option<&str> {
    let messages = batches.first()?;
    let Some(Message::User { content, .. }) = messages.first() else {
        return None;
    };
    match content.first() {
        Some(ContentBlock::Text { text }) => Some(text),
        _ => None,
    }
}

#[tokio::test]
async fn archive_called_before_summary() {
    let order = Arc::new(Mutex::new(Vec::new()));
    let (store, archive_log) = ArchiveStore::new(false, Arc::clone(&order));
    let (provider, _requests) = SummaryProvider::new(Arc::clone(&order));
    let hook = CompactHook::new(provider, "summary-model")
        .with_session_store(store)
        .with_max_messages(1)
        .with_keep_recent_messages(1);

    let decision = hook
        .pre_compact(
            &[user("old request"), user("latest request")],
            &ctx_with_session(&SessionId::new()),
        )
        .await;

    assert!(matches!(decision, Decision::Replace(_)));
    assert_eq!(
        order.lock().expect("order lock").as_slice(),
        ["archive", "provider"]
    );
    let batches = archive_log.lock().expect("archives lock");
    assert_eq!(batches.len(), 1);
    assert_eq!(
        first_archived_user_text(&batches).expect("archive should contain first user text"),
        "old request"
    );
}

#[tokio::test]
async fn archive_failure_continues_and_mutes_run() {
    let order = Arc::new(Mutex::new(Vec::new()));
    let (store, _archives) = ArchiveStore::new(true, Arc::clone(&order));
    let (provider, requests) = SummaryProvider::new(Arc::clone(&order));
    let hook = CompactHook::new(provider, "summary-model")
        .with_session_store(store)
        .with_max_messages(1)
        .with_keep_recent_messages(1);
    let ctx = ctx_with_session(&SessionId::new());
    let messages = [user("old request"), user("latest request")];

    let first = hook.pre_compact(&messages, &ctx).await;
    let second = hook.pre_compact(&messages, &ctx).await;

    assert!(matches!(first, Decision::Continue));
    assert!(matches!(second, Decision::Continue));
    assert_eq!(order.lock().expect("order lock").as_slice(), ["archive"]);
    assert_eq!(requests.lock().expect("requests lock").len(), 0);
}
