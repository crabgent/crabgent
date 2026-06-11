use chrono::{Duration, Utc};
use crabgent_core::{MemoryScope, Owner, SearchQuery};
use crabgent_store::{MemoryDoc, MemoryStore};
use crabgent_store_sqlite::SqliteStore;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use uuid::Uuid;

const MIGRATIONS_BEFORE_011: [&str; 10] = [
    include_str!("../migrations/001_initial_schema.sql"),
    include_str!("../migrations/002_memory_and_fts.sql"),
    include_str!("../migrations/003_session_scope_search.sql"),
    include_str!("../migrations/004_cron_model_target.sql"),
    include_str!("../migrations/005_memory_class_lifecycle.sql"),
    include_str!("../migrations/006_cron_scope_field.sql"),
    include_str!("../migrations/007_global_model_override.sql"),
    include_str!("../migrations/008_session_compaction_summary.sql"),
    include_str!("../migrations/009_session_archives.sql"),
    include_str!("../migrations/010_memory_embedding_blob.sql"),
];

async fn store() -> SqliteStore {
    SqliteStore::open_in_memory()
        .await
        .expect("open sqlite store")
}

fn scope() -> MemoryScope {
    MemoryScope::for_owner(Owner::new("sqlite-memory-class"))
}

async fn store_doc(store: &SqliteStore, doc: &MemoryDoc) {
    store.memory().store(doc).await.expect("store memory");
}

fn doc(body: &str) -> MemoryDoc {
    MemoryDoc::new(scope(), body)
}

#[tokio::test]
async fn roundtrip_each_class() {
    let store = store().await;
    for class in [
        "semantic",
        "episodic",
        "notes",
        "user_profile",
        "skill",
        "tools",
    ] {
        let mut doc = doc(class);
        doc.class = Some(class.to_owned());
        doc.importance = Some(0.75);
        let id = doc.id.clone();
        store_doc(&store, &doc).await;
        let stored = store
            .memory()
            .get(&id)
            .await
            .expect("test result")
            .expect("doc exists");
        assert_eq!(stored.class.as_deref(), Some(class));
        assert_eq!(stored.importance, Some(0.75));
    }
}

#[tokio::test]
async fn expired_filtered_default() {
    let store = store().await;
    let mut doc = doc("expired token");
    doc.expires_at = Some(Utc::now() - Duration::minutes(1));
    store_doc(&store, &doc).await;

    let hits = store
        .memory()
        .search(&SearchQuery::new("expired").scope(scope()))
        .await
        .expect("test result");
    assert!(hits.is_empty());
}

#[tokio::test]
async fn expired_visible_with_flag() {
    let store = store().await;
    let mut doc = doc("expired token");
    doc.expires_at = Some(Utc::now() - Duration::minutes(1));
    store_doc(&store, &doc).await;

    let hits = store
        .memory()
        .search(&SearchQuery::new("expired").scope(scope()).include_expired())
        .await
        .expect("test result");
    assert_eq!(hits.len(), 1);
}

#[tokio::test]
async fn archived_filtered_default() {
    let store = store().await;
    let mut doc = doc("archived token");
    doc.archived_at = Some(Utc::now());
    store_doc(&store, &doc).await;

    let hits = store
        .memory()
        .search(&SearchQuery::new("archived").scope(scope()))
        .await
        .expect("test result");
    assert!(hits.is_empty());
}

#[tokio::test]
async fn archived_visible_with_flag() {
    let store = store().await;
    let mut doc = doc("archived token");
    doc.archived_at = Some(Utc::now());
    store_doc(&store, &doc).await;

    let hits = store
        .memory()
        .search(
            &SearchQuery::new("archived")
                .scope(scope())
                .include_archived(),
        )
        .await
        .expect("test result");
    assert_eq!(hits.len(), 1);
}

#[tokio::test]
async fn archive_unarchive_cycle() {
    let store = store().await;
    let doc = doc("cycle token");
    let id = doc.id.clone();
    store_doc(&store, &doc).await;

    assert!(
        store
            .memory()
            .archive(&id, Utc::now())
            .await
            .expect("test result")
    );
    assert!(
        store
            .memory()
            .search(&SearchQuery::new("cycle").scope(scope()))
            .await
            .expect("test result")
            .is_empty()
    );
    assert!(store.memory().unarchive(&id).await.expect("test result"));
    assert_eq!(
        store
            .memory()
            .search(&SearchQuery::new("cycle").scope(scope()))
            .await
            .expect("test result")
            .len(),
        1
    );
}

#[tokio::test]
async fn extend_expiry_postpones() {
    let store = store().await;
    let mut doc = doc("expiry token");
    doc.expires_at = Some(Utc::now() - Duration::minutes(1));
    let id = doc.id.clone();
    store_doc(&store, &doc).await;
    assert!(
        store
            .memory()
            .search(&SearchQuery::new("expiry").scope(scope()))
            .await
            .expect("test result")
            .is_empty()
    );

    assert!(
        store
            .memory()
            .extend_expiry(&id, Some(Utc::now() + Duration::hours(1)))
            .await
            .expect("test result")
    );
    assert_eq!(
        store
            .memory()
            .search(&SearchQuery::new("expiry").scope(scope()))
            .await
            .expect("test result")
            .len(),
        1
    );
}

#[tokio::test]
async fn migration_011_forward_step_reclasses_pre_migration_semantic_rows_by_body_prefix() {
    let opts = SqliteConnectOptions::new()
        .in_memory(true)
        .shared_cache(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(opts)
        .await
        .expect("open raw sqlite pool");

    for sql in MIGRATIONS_BEFORE_011 {
        sqlx::raw_sql(sql)
            .execute(&pool)
            .await
            .expect("apply sqlite migration before 011");
    }

    let cases = [
        (
            "# notes\nfoo bar",
            "semantic",
            "notes",
            "notes header should reclass",
        ),
        (
            "# user: alice\nLocal-first preferences",
            "semantic",
            "user_profile",
            "user header should reclass",
        ),
        (
            "# skill: wetter-garten\nyaml",
            "semantic",
            "skill",
            "skill header should reclass",
        ),
        (
            "# tools\nlist of tools",
            "semantic",
            "tools",
            "tools header should reclass",
        ),
        (
            "no prefix",
            "semantic",
            "semantic",
            "semantic row without header should stay semantic",
        ),
        (
            "# skill: episodic-skill\nbody",
            "episodic",
            "episodic",
            "non-semantic row should stay episodic",
        ),
        (
            "Hello\n# user: bot",
            "semantic",
            "semantic",
            "mid-document user header should stay semantic",
        ),
    ];

    let now = Utc::now().to_rfc3339();
    let mut ids = Vec::with_capacity(cases.len());
    for (body, class, _, _) in cases {
        let id = Uuid::now_v7().to_string();
        sqlx::query(
            "INSERT INTO memory (id, owner, body, class, created_at, updated_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        )
        .bind(&id)
        .bind("sqlite-forward-step")
        .bind(body)
        .bind(class)
        .bind(&now)
        .bind(&now)
        .execute(&pool)
        .await
        .expect("seed raw sqlite memory row");
        ids.push(id);
    }

    sqlx::raw_sql(include_str!(
        "../migrations/011_split_kind_from_semantic_discriminator.sql"
    ))
    .execute(&pool)
    .await
    .expect("apply sqlite migration 011");

    for (i, (_, _, expected_class, message)) in cases.iter().enumerate() {
        let (actual_class,): (String,) = sqlx::query_as("SELECT class FROM memory WHERE id = ?1")
            .bind(&ids[i])
            .fetch_one(&pool)
            .await
            .expect("load raw sqlite memory class");
        assert_eq!(actual_class, *expected_class, "{message}");
    }
}

#[tokio::test]
async fn migration_011_reclasses_semantic_rows_by_body_prefix() {
    // Build pool outside SqliteStore so the test can replay the migration SQL
    // against the same database after seeding post-migration rows that look
    // like the pre-migration shape (class='semantic' + Markdown header prefix).
    let opts = SqliteConnectOptions::new()
        .in_memory(true)
        .shared_cache(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(opts)
        .await
        .expect("open pool");
    let pool_handle = pool.clone();
    let store = SqliteStore::from_pool(pool).await.expect("migrate store");

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
        ("no prefix", "semantic", "semantic"),
        // Near-prefix without newline delimiter: must not false-positive.
        ("# notesworthy\nfoo", "semantic", "semantic"),
        ("# toolsmith\nbar", "semantic", "semantic"),
    ];

    let mut ids = Vec::with_capacity(cases.len());
    for (body, class, _) in cases {
        let mut doc = MemoryDoc::new(scope(), body);
        doc.class = Some(class.to_owned());
        ids.push(doc.id.clone());
        store_doc(&store, &doc).await;
    }

    // Replay migration 011 SQL against the seeded rows. The migration is
    // data-only and idempotent: rows already at their target class are
    // filtered out by the WHERE class='semantic' guard.
    let sql = include_str!("../migrations/011_split_kind_from_semantic_discriminator.sql");
    sqlx::raw_sql(sql)
        .execute(&pool_handle)
        .await
        .expect("replay migration 011");

    for (i, (_, _, expected_class)) in cases.iter().enumerate() {
        let stored = store
            .memory()
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
async fn class_filter_search() {
    let store = store().await;
    let mut semantic = doc("shared class token");
    semantic.class = Some("semantic".to_owned());
    let mut episodic = doc("shared class token");
    episodic.class = Some("episodic".to_owned());
    let episodic_id = episodic.id.clone();
    store_doc(&store, &semantic).await;
    store_doc(&store, &episodic).await;

    let hits = store
        .memory()
        .search(&SearchQuery::new("shared").scope(scope()).class("episodic"))
        .await
        .expect("test result");
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].id, episodic_id);
}
