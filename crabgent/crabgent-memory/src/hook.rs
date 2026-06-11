//! Optional hook for persisting classified memories after runs.

use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use crabgent_core::{Hook, MemoryId, MemoryScope, Outcome, RunCtx};
use crabgent_log::warn;
use crabgent_store::{MemoryDoc, MemoryStore};

use crate::{MemoryClass, MemoryError, MemoryImportance};

pub type PersistClassifier = Arc<dyn Fn(&RunCtx, &Outcome) -> Option<PersistRequest> + Send + Sync>;

#[derive(Clone)]
pub struct MemoryPersistHook {
    classifier: Option<PersistClassifier>,
    store: Arc<dyn MemoryStore>,
}

#[derive(Debug, Clone)]
pub struct PersistRequest {
    pub class: MemoryClass,
    pub scope: MemoryScope,
    pub body: String,
    pub importance: Option<MemoryImportance>,
    pub expires_at: Option<DateTime<Utc>>,
}

impl MemoryPersistHook {
    pub fn new(store: Arc<dyn MemoryStore>) -> Self {
        Self {
            classifier: None,
            store,
        }
    }

    #[must_use]
    pub fn with_classifier<F>(mut self, classifier: F) -> Self
    where
        F: Fn(&RunCtx, &Outcome) -> Option<PersistRequest> + Send + Sync + 'static,
    {
        self.classifier = Some(Arc::new(classifier));
        self
    }

    pub async fn persist_on_stop(
        &self,
        ctx: &RunCtx,
        outcome: &Outcome,
    ) -> Result<Option<MemoryId>, MemoryError> {
        if !matches!(outcome, Outcome::Completed(_) | Outcome::MaxTurnsExceeded) {
            return Ok(None);
        }
        let Some(classifier) = &self.classifier else {
            return Ok(None);
        };
        let Some(request) = classifier(ctx, outcome) else {
            return Ok(None);
        };

        let mut doc = MemoryDoc::new(request.scope, request.body);
        doc.class = Some(request.class.as_str().to_owned());
        doc.importance = request.importance.map(MemoryImportance::into_inner);
        doc.expires_at = request.expires_at;

        self.store.store(&doc).await.map(Some).map_err(Into::into)
    }
}

#[async_trait]
impl Hook for MemoryPersistHook {
    async fn on_stop(&self, ctx: &RunCtx, outcome: &Outcome) {
        if let Err(err) = self.persist_on_stop(ctx, outcome).await {
            warn!(error = %err, "memory persist hook failed");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crabgent_core::{Owner, RunId, SearchQuery, Subject};
    use crabgent_store::{MemoryMemoryStore, MemoryStore};

    fn ctx() -> RunCtx {
        RunCtx::new(RunId::new(), Subject::new("u"))
    }

    async fn stored_docs(store: &MemoryMemoryStore, scope: MemoryScope) -> Vec<MemoryDoc> {
        let hits = store
            .search(&SearchQuery::new("").scope(scope))
            .await
            .expect("test result");
        let mut docs = Vec::with_capacity(hits.len());
        for hit in hits {
            docs.push(
                store
                    .get(&hit.id)
                    .await
                    .expect("test result")
                    .expect("test result"),
            );
        }
        docs
    }

    #[tokio::test]
    async fn default_hook_noop() {
        let store = Arc::new(MemoryMemoryStore::default());
        let hook = MemoryPersistHook::new(store.clone());
        let scope = MemoryScope::for_owner(Owner::new("u"));

        hook.on_stop(&ctx(), &Outcome::Completed("done".into()))
            .await;

        assert!(stored_docs(&store, scope).await.is_empty());
    }

    #[tokio::test]
    async fn classifier_some_invokes_store_store() {
        let store = Arc::new(MemoryMemoryStore::default());
        let scope = MemoryScope::for_owner(Owner::new("u"));
        let request_scope = scope.clone();
        let hook = MemoryPersistHook::new(store.clone()).with_classifier(move |_, _| {
            Some(PersistRequest {
                class: MemoryClass::Episodic,
                scope: request_scope.clone(),
                body: "remember this event".to_owned(),
                importance: Some(MemoryImportance::new(0.8).expect("test result")),
                expires_at: None,
            })
        });

        hook.on_stop(&ctx(), &Outcome::Completed("done".into()))
            .await;

        let docs = stored_docs(&store, scope).await;
        assert_eq!(docs.len(), 1);
        assert_eq!(docs[0].class.as_deref(), Some("episodic"));
        assert_eq!(docs[0].importance, Some(0.8));
        assert_eq!(docs[0].body, "remember this event");
    }

    #[tokio::test]
    async fn classifier_none_skips() {
        let store = Arc::new(MemoryMemoryStore::default());
        let scope = MemoryScope::for_owner(Owner::new("u"));
        let hook = MemoryPersistHook::new(store.clone()).with_classifier(|_, _| None);

        hook.on_stop(&ctx(), &Outcome::Completed("done".into()))
            .await;

        assert!(stored_docs(&store, scope).await.is_empty());
    }
}
