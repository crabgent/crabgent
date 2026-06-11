//! `SQLite`-backed [`MemoryStore`] using FTS5 for search.

mod insert;
mod lifecycle;
mod relations;
mod search;
mod update;
mod vec;

use std::str::FromStr;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use crabgent_core::{MemoryId, MemoryScope, Owner, SearchQuery};
use crabgent_store::error::StoreError;
use crabgent_store::ids::RelationId;
use crabgent_store::records::{MemoryDoc, MemoryHit, MemoryRelation};
use crabgent_store::relation_type::RelationType;
use crabgent_store::traits::MemoryStore;
use sqlx::Row;
use sqlx::sqlite::{SqlitePool, SqliteRow};

pub const COLS: &str = "id, owner, channel, conv, agent, kind, body, class, importance, expires_at, archived_at, embedding, created_at, updated_at";

#[derive(Clone)]
pub struct SqliteMemoryStore {
    pool: SqlitePool,
}

impl SqliteMemoryStore {
    pub(crate) const fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }
}

pub fn row_to_doc(row: &SqliteRow) -> Result<MemoryDoc, StoreError> {
    let id_s: String = row.try_get("id").map_err(StoreError::backend)?;
    let owner: Option<String> = row.try_get("owner").map_err(StoreError::backend)?;
    let channel: Option<String> = row.try_get("channel").map_err(StoreError::backend)?;
    let conv: Option<String> = row.try_get("conv").map_err(StoreError::backend)?;
    let agent: Option<String> = row.try_get("agent").map_err(StoreError::backend)?;
    let kind: Option<String> = row.try_get("kind").map_err(StoreError::backend)?;
    let body: String = row.try_get("body").map_err(StoreError::backend)?;
    let class: Option<String> = row.try_get("class").map_err(StoreError::backend)?;
    let importance: Option<f32> = row.try_get("importance").map_err(StoreError::backend)?;
    let expires_at: Option<DateTime<Utc>> =
        row.try_get("expires_at").map_err(StoreError::backend)?;
    let archived_at: Option<DateTime<Utc>> =
        row.try_get("archived_at").map_err(StoreError::backend)?;
    let embedding_blob: Option<Vec<u8>> = row.try_get("embedding").map_err(StoreError::backend)?;
    let embedding = vec::decode_embedding(embedding_blob)?;
    let created_at: DateTime<Utc> = row.try_get("created_at").map_err(StoreError::backend)?;
    let updated_at: DateTime<Utc> = row.try_get("updated_at").map_err(StoreError::backend)?;
    let id = MemoryId::from_str(&id_s).map_err(StoreError::invalid)?;
    Ok(MemoryDoc {
        id,
        scope: MemoryScope {
            owner: owner.map(Owner::new),
            channel,
            conv,
            agent,
            kind,
        },
        body,
        class,
        importance,
        expires_at,
        archived_at,
        embedding,
        created_at,
        updated_at,
    })
}

pub fn row_to_hit(row: &SqliteRow) -> Result<MemoryHit, StoreError> {
    let id_s: String = row.try_get("id").map_err(StoreError::backend)?;
    let body: String = row.try_get("body").map_err(StoreError::backend)?;
    let score: f32 = row.try_get("score").map_err(StoreError::backend)?;
    let cosine_similarity: Option<f32> = row
        .try_get("cosine_similarity")
        .map_err(StoreError::backend)?;
    let created_at: DateTime<Utc> = row.try_get("created_at").map_err(StoreError::backend)?;
    let id = MemoryId::from_str(&id_s).map_err(StoreError::invalid)?;
    Ok(MemoryHit {
        id,
        body,
        score,
        cosine_similarity,
        created_at,
    })
}

#[async_trait]
impl MemoryStore for SqliteMemoryStore {
    async fn search(&self, query: &SearchQuery) -> Result<Vec<MemoryHit>, StoreError> {
        search::search_query(&self.pool, query).await
    }

    async fn store(&self, doc: &MemoryDoc) -> Result<MemoryId, StoreError> {
        insert::store_doc(&self.pool, doc).await
    }

    async fn get(&self, id: &MemoryId) -> Result<Option<MemoryDoc>, StoreError> {
        insert::get_doc(&self.pool, id).await
    }

    async fn delete(&self, id: &MemoryId) -> Result<bool, StoreError> {
        let removed = insert::delete_doc(&self.pool, id).await?;
        if removed {
            relations::cascade_delete(&self.pool, id).await?;
        }
        Ok(removed)
    }

    async fn delete_scoped(&self, id: &MemoryId, scope: &MemoryScope) -> Result<bool, StoreError> {
        let removed = insert::delete_scoped_doc(&self.pool, id, scope).await?;
        if removed {
            relations::cascade_delete(&self.pool, id).await?;
        }
        Ok(removed)
    }

    async fn archive(&self, id: &MemoryId, at: DateTime<Utc>) -> Result<bool, StoreError> {
        self.archive_doc(id, at).await
    }

    async fn unarchive(&self, id: &MemoryId) -> Result<bool, StoreError> {
        self.unarchive_doc(id).await
    }

    async fn extend_expiry(
        &self,
        id: &MemoryId,
        new_expiry: Option<DateTime<Utc>>,
    ) -> Result<bool, StoreError> {
        self.extend_doc_expiry(id, new_expiry).await
    }

    async fn update_body(&self, id: &MemoryId, new_body: String) -> Result<bool, StoreError> {
        update::update_body_query(&self.pool, id, new_body).await
    }

    async fn update_body_with_embedding(
        &self,
        id: &MemoryId,
        new_body: String,
        embedding: Option<Vec<f32>>,
    ) -> Result<bool, StoreError> {
        update::update_body_with_embedding_query(&self.pool, id, new_body, embedding).await
    }

    async fn relation_store(&self, relation: &MemoryRelation) -> Result<RelationId, StoreError> {
        relations::store_relation(&self.pool, relation).await
    }

    async fn relation_delete(
        &self,
        from_id: &MemoryId,
        to_id: &MemoryId,
        relation_type: &RelationType,
        scope: &MemoryScope,
    ) -> Result<bool, StoreError> {
        relations::delete_relation(&self.pool, from_id, to_id, relation_type, scope).await
    }

    async fn relation_neighbors(
        &self,
        ids: &[MemoryId],
        scope: &MemoryScope,
    ) -> Result<Vec<MemoryRelation>, StoreError> {
        relations::neighbors(&self.pool, ids, scope).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::SqliteStore;

    async fn store() -> SqliteStore {
        SqliteStore::open_in_memory().await.expect("open store")
    }

    fn alice_scope() -> MemoryScope {
        MemoryScope::for_owner(Owner::new("alice"))
    }

    async fn store_doc(s: &SqliteStore, scope: MemoryScope, body: impl Into<String>) -> MemoryId {
        s.memory()
            .store(&MemoryDoc::new(scope, body))
            .await
            .expect("test result")
    }

    #[tokio::test]
    async fn store_then_get_returns_doc() {
        let s = store().await;
        let id = store_doc(&s, alice_scope(), "remember me").await;
        let doc = s
            .memory()
            .get(&id)
            .await
            .expect("test result")
            .expect("doc exists");
        assert_eq!(doc.body, "remember me");
        assert_eq!(doc.scope.owner, Some(Owner::new("alice")));
    }

    #[tokio::test]
    async fn delete_removes_doc() {
        let s = store().await;
        let id = store_doc(&s, alice_scope(), "x").await;
        assert!(s.memory().delete(&id).await.expect("test result"));
        assert!(s.memory().get(&id).await.expect("test result").is_none());
    }

    #[tokio::test]
    async fn delete_unknown_returns_false() {
        let s = store().await;
        let unknown = MemoryId::new();
        assert!(!s.memory().delete(&unknown).await.expect("test result"));
    }

    #[tokio::test]
    async fn delete_scoped_respects_scope() {
        let s = store().await;
        let id = store_doc(&s, alice_scope(), "x").await;
        let bob = MemoryScope::for_owner(Owner::new("bob"));
        assert!(
            !s.memory()
                .delete_scoped(&id, &bob)
                .await
                .expect("test result")
        );
        assert!(s.memory().get(&id).await.expect("test result").is_some());
        assert!(
            s.memory()
                .delete_scoped(&id, &alice_scope())
                .await
                .expect("test result")
        );
        assert!(s.memory().get(&id).await.expect("test result").is_none());
    }

    #[tokio::test]
    async fn search_finds_owner_scoped_match_via_fts5() {
        let s = store().await;
        store_doc(
            &s,
            MemoryScope::for_owner(Owner::new("alice")),
            "Alice prefers local-first tools",
        )
        .await;
        store_doc(
            &s,
            MemoryScope::for_owner(Owner::new("bob")),
            "Alice prefers local-first tools",
        )
        .await;
        let q = SearchQuery::new("local-first").scope(alice_scope());
        let hits = s.memory().search(&q).await.expect("test result");
        assert_eq!(hits.len(), 1);
        assert!(hits[0].body.contains("local-first"));
    }

    #[tokio::test]
    async fn search_filters_channel_and_kind() {
        let s = store().await;
        let alice_slack_dm = MemoryScope::for_owner(Owner::new("alice"))
            .with_channel("slack")
            .with_kind("direct");
        let alice_telegram = MemoryScope::for_owner(Owner::new("alice")).with_channel("telegram");
        store_doc(&s, alice_slack_dm.clone(), "in slack-direct").await;
        store_doc(&s, alice_telegram, "in telegram").await;
        let q = SearchQuery::new("in").scope(alice_slack_dm.clone());
        let hits = s.memory().search(&q).await.expect("test result");
        assert_eq!(hits.len(), 1);
        assert!(hits[0].body.contains("slack-direct"));
    }

    #[tokio::test]
    async fn search_with_empty_query_returns_all_in_scope() {
        let s = store().await;
        store_doc(&s, alice_scope(), "first").await;
        store_doc(&s, alice_scope(), "second").await;
        let q = SearchQuery::new(String::new()).scope(alice_scope());
        let hits = s.memory().search(&q).await.expect("test result");
        assert_eq!(hits.len(), 2);
    }

    #[tokio::test]
    async fn search_respects_limit() {
        let s = store().await;
        for i in 0..5 {
            store_doc(&s, alice_scope(), format!("note {i}")).await;
        }
        let q = SearchQuery::new("note").scope(alice_scope()).limit(2);
        let hits = s.memory().search(&q).await.expect("test result");
        assert_eq!(hits.len(), 2);
    }

    #[tokio::test]
    async fn search_global_filter_matches_any_owner() {
        let s = store().await;
        store_doc(&s, MemoryScope::for_owner(Owner::new("a")), "shared word").await;
        store_doc(&s, MemoryScope::for_owner(Owner::new("b")), "shared word").await;
        let q = SearchQuery::new("shared").scope(MemoryScope::global());
        let hits = s.memory().search(&q).await.expect("test result");
        assert_eq!(hits.len(), 2);
    }

    #[tokio::test]
    async fn fts_operator_text_is_quoted_as_phrase() {
        let s = store().await;
        store_doc(&s, alice_scope(), "literal X OR foo:1 phrase").await;
        store_doc(&s, alice_scope(), "X appears separately from foo").await;

        let q = SearchQuery::new("X OR foo:1").scope(alice_scope());
        let hits = s.memory().search(&q).await.expect("test result");

        assert_eq!(hits.len(), 1);
        assert!(hits[0].body.contains("X OR foo:1"));
    }

    #[tokio::test]
    async fn fts_special_chars_do_not_crash() {
        let s = store().await;
        store_doc(&s, alice_scope(), "safe body").await;
        let q = SearchQuery::new("\"unterminated NEAR/5 foo:bar \\").scope(alice_scope());
        let hits = s.memory().search(&q).await.expect("test result");

        assert!(hits.is_empty());
    }
}
