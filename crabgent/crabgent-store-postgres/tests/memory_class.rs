#[path = "../src/test_helpers.rs"]
mod test_helpers;

use chrono::{Duration, Utc};
use crabgent_core::{MemoryScope, Owner, SearchQuery};
use crabgent_store::{MemoryDoc, MemoryStore};
use crabgent_store_postgres::PostgresStore;
use test_helpers::{postgres_test_ctx, postgres_unmigrated_test_ctx};
use uuid::Uuid;

const MIGRATIONS_BEFORE_CLASS_SPLIT: [&str; 11] = [
    include_str!("../migrations/20260512000001_initial_schema.sql"),
    include_str!("../migrations/20260512000002_memory_and_fts.sql"),
    include_str!("../migrations/20260512000003_session_scope_search.sql"),
    include_str!("../migrations/20260512000004_cron_model_target.sql"),
    include_str!("../migrations/20260512000005_memory_class_lifecycle.sql"),
    include_str!("../migrations/20260514021456_cron_scope_field.sql"),
    include_str!("../migrations/20260514021457_global_model_override.sql"),
    include_str!("../migrations/20260516000001_session_compaction_summary.sql"),
    include_str!("../migrations/20260516000002_session_archives.sql"),
    include_str!("../migrations/20260518000001_switch_fts_to_german.sql"),
    include_str!("../migrations/20260521000001_session_scope_unique_index.sql"),
];

fn owner(test_name: &str) -> Owner {
    Owner::new(format!("pg-memory-class-{test_name}-{}", Uuid::now_v7()))
}

fn scope(test_name: &str) -> MemoryScope {
    MemoryScope::for_owner(owner(test_name))
}

async fn store_doc(store: &PostgresStore, doc: &MemoryDoc) {
    store.memory_store().store(doc).await.expect("store memory");
}

fn doc(scope: MemoryScope, body: &str) -> MemoryDoc {
    MemoryDoc::new(scope, body)
}

#[tokio::test]
async fn roundtrip_each_class() {
    let ctx = postgres_test_ctx().await;
    let store = PostgresStore::from_pool(ctx.pool.clone());
    for class in [
        "semantic",
        "episodic",
        "notes",
        "user_profile",
        "skill",
        "tools",
    ] {
        let mut doc = doc(scope("roundtrip"), class);
        doc.class = Some(class.to_owned());
        doc.importance = Some(0.75);
        let id = doc.id.clone();
        store_doc(&store, &doc).await;
        let stored = store
            .memory_store()
            .get(&id)
            .await
            .expect("test result")
            .expect("doc exists");
        assert_eq!(stored.class.as_deref(), Some(class));
        assert_eq!(stored.importance, Some(0.75));
    }
}

#[tokio::test]
async fn migration_20260521000002_forward_step_reclasses_pre_migration_rows() {
    let ctx = postgres_unmigrated_test_ctx().await;

    for sql in MIGRATIONS_BEFORE_CLASS_SPLIT {
        sqlx::raw_sql(sql)
            .execute(&ctx.pool)
            .await
            .expect("apply postgres migration before class split");
    }

    let cases = [
        ("# notes\nfoo bar", "semantic", "notes"),
        (
            "# user: alice\nLocal-first preferences",
            "semantic",
            "user_profile",
        ),
        ("# skill: wetter-garten\nyaml", "semantic", "skill"),
        ("# tools\nlist of tools", "semantic", "tools"),
        ("no prefix", "semantic", "semantic"),
        ("# skill: episodic-skill\nbody", "episodic", "episodic"),
        ("Hello\n# user: bot", "semantic", "semantic"),
    ];

    let now = Utc::now();
    let mut ids = Vec::with_capacity(cases.len());
    for (body, class, _) in cases {
        let id = Uuid::now_v7();
        sqlx::query(
            "INSERT INTO memory_docs (id, owner, body, class, created_at, updated_at) \
             VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(id)
        .bind("pg-forward-step")
        .bind(body)
        .bind(class)
        .bind(now)
        .bind(now)
        .execute(&ctx.pool)
        .await
        .expect("seed raw postgres memory row");
        ids.push(id);
    }

    sqlx::raw_sql(include_str!(
        "../migrations/20260521000002_split_kind_from_semantic_discriminator.sql"
    ))
    .execute(&ctx.pool)
    .await
    .expect("apply postgres class split migration");

    for (i, (_, _, expected_class)) in cases.iter().enumerate() {
        let (actual_class,): (String,) =
            sqlx::query_as("SELECT class FROM memory_docs WHERE id = $1")
                .bind(ids[i])
                .fetch_one(&ctx.pool)
                .await
                .expect("load raw postgres memory class");
        assert_eq!(
            actual_class, *expected_class,
            "case {i} should yield class={expected_class}"
        );
    }
}

#[tokio::test]
async fn migration_20260521000002_reclasses_semantic_rows_by_body_prefix() {
    let ctx = postgres_test_ctx().await;
    let store = PostgresStore::from_pool(ctx.pool.clone());
    let scope = scope("reclass-migration");

    let cases = [
        (
            "# user: alice\nLocal-first preferences",
            "semantic",
            "user_profile",
        ),
        ("# notes\nfoo bar", "semantic", "notes"),
        ("# skill: wetter-garten\nyaml", "semantic", "skill"),
        ("# tools\nlist of tools", "semantic", "tools"),
        // Conflict: non-semantic class with matching prefix stays untouched.
        ("# skill: episodic-skill", "episodic", "episodic"),
        // No prefix: stays semantic.
        ("no prefix here", "semantic", "semantic"),
        // Near-prefix without newline delimiter: must not false-positive.
        ("# notesworthy\nfoo", "semantic", "semantic"),
        ("# toolsmith\nbar", "semantic", "semantic"),
    ];

    let mut ids = Vec::with_capacity(cases.len());
    for (body, class, _) in cases {
        let mut doc = doc(scope.clone(), body);
        doc.class = Some(class.to_owned());
        ids.push(doc.id.clone());
        store_doc(&store, &doc).await;
    }

    // Replay migration SQL against the seeded rows. The migration is data-only
    // and idempotent: rows already at their target class are filtered out by
    // the WHERE class='semantic' guard.
    let sql =
        include_str!("../migrations/20260521000002_split_kind_from_semantic_discriminator.sql");
    sqlx::raw_sql(sql)
        .execute(&ctx.pool)
        .await
        .expect("replay migration 20260521000002");

    for (i, (_, _, expected_class)) in cases.iter().enumerate() {
        let stored = store
            .memory_store()
            .get(&ids[i])
            .await
            .expect("get result")
            .expect("doc exists");
        assert_eq!(
            stored.class.as_deref(),
            Some(*expected_class),
            "case {i} body prefix should yield class={expected_class}"
        );
    }
}

#[tokio::test]
async fn expired_filtered_default() {
    let ctx = postgres_test_ctx().await;
    let store = PostgresStore::from_pool(ctx.pool.clone());
    let scope = scope("expired-default");
    let mut doc = doc(scope.clone(), "expired token");
    doc.expires_at = Some(Utc::now() - Duration::minutes(1));
    store_doc(&store, &doc).await;

    let hits = store
        .memory_store()
        .search(&SearchQuery::new("expired").scope(scope))
        .await
        .expect("test result");
    assert!(hits.is_empty());
}

#[tokio::test]
async fn expired_visible_with_flag() {
    let ctx = postgres_test_ctx().await;
    let store = PostgresStore::from_pool(ctx.pool.clone());
    let scope = scope("expired-visible");
    let mut doc = doc(scope.clone(), "expired token");
    doc.expires_at = Some(Utc::now() - Duration::minutes(1));
    store_doc(&store, &doc).await;

    let hits = store
        .memory_store()
        .search(&SearchQuery::new("expired").scope(scope).include_expired())
        .await
        .expect("test result");
    assert_eq!(hits.len(), 1);
}

#[tokio::test]
async fn archived_filtered_default() {
    let ctx = postgres_test_ctx().await;
    let store = PostgresStore::from_pool(ctx.pool.clone());
    let scope = scope("archived-default");
    let mut doc = doc(scope.clone(), "archived token");
    doc.archived_at = Some(Utc::now());
    store_doc(&store, &doc).await;

    let hits = store
        .memory_store()
        .search(&SearchQuery::new("archived").scope(scope))
        .await
        .expect("test result");
    assert!(hits.is_empty());
}

#[tokio::test]
async fn archived_visible_with_flag() {
    let ctx = postgres_test_ctx().await;
    let store = PostgresStore::from_pool(ctx.pool.clone());
    let scope = scope("archived-visible");
    let mut doc = doc(scope.clone(), "archived token");
    doc.archived_at = Some(Utc::now());
    store_doc(&store, &doc).await;

    let hits = store
        .memory_store()
        .search(&SearchQuery::new("archived").scope(scope).include_archived())
        .await
        .expect("test result");
    assert_eq!(hits.len(), 1);
}

#[tokio::test]
async fn archive_unarchive_cycle() {
    let ctx = postgres_test_ctx().await;
    let store = PostgresStore::from_pool(ctx.pool.clone());
    let scope = scope("archive-cycle");
    let doc = doc(scope.clone(), "cycle token");
    let id = doc.id.clone();
    store_doc(&store, &doc).await;

    assert!(
        store
            .memory_store()
            .archive(&id, Utc::now())
            .await
            .expect("test result")
    );
    assert!(
        store
            .memory_store()
            .search(&SearchQuery::new("cycle").scope(scope.clone()))
            .await
            .expect("test result")
            .is_empty()
    );
    assert!(
        store
            .memory_store()
            .unarchive(&id)
            .await
            .expect("test result")
    );
    assert_eq!(
        store
            .memory_store()
            .search(&SearchQuery::new("cycle").scope(scope))
            .await
            .expect("test result")
            .len(),
        1
    );
}

#[tokio::test]
async fn extend_expiry_postpones() {
    let ctx = postgres_test_ctx().await;
    let store = PostgresStore::from_pool(ctx.pool.clone());
    let scope = scope("extend-expiry");
    let mut doc = doc(scope.clone(), "expiry token");
    doc.expires_at = Some(Utc::now() - Duration::minutes(1));
    let id = doc.id.clone();
    store_doc(&store, &doc).await;
    assert!(
        store
            .memory_store()
            .search(&SearchQuery::new("expiry").scope(scope.clone()))
            .await
            .expect("test result")
            .is_empty()
    );

    assert!(
        store
            .memory_store()
            .extend_expiry(&id, Some(Utc::now() + Duration::hours(1)))
            .await
            .expect("test result")
    );
    assert_eq!(
        store
            .memory_store()
            .search(&SearchQuery::new("expiry").scope(scope))
            .await
            .expect("test result")
            .len(),
        1
    );
}

#[tokio::test]
async fn class_filter_search() {
    let ctx = postgres_test_ctx().await;
    let store = PostgresStore::from_pool(ctx.pool.clone());
    let scope = scope("class-filter");
    let mut semantic = doc(scope.clone(), "shared class token");
    semantic.class = Some("semantic".to_owned());
    let mut episodic = doc(scope.clone(), "shared class token");
    episodic.class = Some("episodic".to_owned());
    let episodic_id = episodic.id.clone();
    store_doc(&store, &semantic).await;
    store_doc(&store, &episodic).await;

    let hits = store
        .memory_store()
        .search(&SearchQuery::new("shared").scope(scope).class("episodic"))
        .await
        .expect("test result");
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].id, episodic_id);
}
