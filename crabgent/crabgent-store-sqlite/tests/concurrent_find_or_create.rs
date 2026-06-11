//! Concurrent `find_or_create` race pin for the `SQLite` backend.
//!
//! Pins the contract that two concurrent `find_or_create` calls with the same
//! `(owner, thread, scope)` tuple produce exactly one row. `SQLite` does not
//! support `NULLS NOT DISTINCT` in unique indexes, so the race is closed by
//! wrapping the SELECT+INSERT in a `BEGIN IMMEDIATE` transaction: the second
//! peer blocks on the RESERVED write lock until the first commits, then
//! re-SELECTs the winner.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use crabgent_core::{MemoryScope, Owner, ThreadId};
use crabgent_store::{SessionStore, Store};
use crabgent_store_sqlite::{SqliteStore, arm_pause_after_select_miss};
use sqlx::Row;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePool, SqlitePoolOptions};
use tokio::sync::Barrier;
use uuid::Uuid;

/// Per-test on-disk `SQLite` database. WAL + `busy_timeout` + multi-connection pool
/// so that two concurrent `find_or_create` calls actually contend on the
/// `BEGIN IMMEDIATE` write lock instead of being serialized at the pool layer
/// (the workspace `open_in_memory` helper caps `max_connections` at 1).
struct ConcurrentSqliteCtx {
    store: Arc<SqliteStore>,
    pool: SqlitePool,
    path: PathBuf,
}

impl Drop for ConcurrentSqliteCtx {
    fn drop(&mut self) {
        // Best-effort cleanup of the per-test on-disk database and its WAL
        // sidecars. Tests use UUID-suffixed filenames, so a stale file is
        // harmless even if cleanup fails (Drop runs on a synchronous path
        // and cannot await the pool to close gracefully).
        drop(std::fs::remove_file(&self.path));
        let mut wal = self.path.clone();
        wal.as_mut_os_string().push("-wal");
        drop(std::fs::remove_file(&wal));
        let mut shm = self.path.clone();
        shm.as_mut_os_string().push("-shm");
        drop(std::fs::remove_file(&shm));
    }
}

async fn concurrent_ctx() -> ConcurrentSqliteCtx {
    let path = std::env::temp_dir().join(format!(
        "crabgent-sqlite-race-{}.db",
        Uuid::now_v7().simple()
    ));
    let opts = SqliteConnectOptions::new()
        .filename(&path)
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal)
        .busy_timeout(Duration::from_secs(5));
    let pool = SqlitePoolOptions::new()
        .max_connections(4)
        .connect_with(opts)
        .await
        .expect("connect sqlite test pool");
    let store = SqliteStore::from_pool(pool.clone())
        .await
        .expect("open sqlite store");
    ConcurrentSqliteCtx {
        store: Arc::new(store),
        pool,
        path,
    }
}

fn unique_owner(suffix: &str) -> Owner {
    Owner::new(format!("sqlite-race-{suffix}-{}", Uuid::now_v7()))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn same_scope_concurrent_resolves_to_one_row() {
    let ctx = concurrent_ctx().await;
    let store = Arc::clone(&ctx.store);
    let pool = ctx.pool.clone();
    let owner = unique_owner("same-scope");
    let thread = ThreadId::new("thread-shared");
    let scope = MemoryScope::default();
    let select_miss_barrier = Arc::new(Barrier::new(3));
    let release_barrier = Arc::new(Barrier::new(3));
    let pause_guard = arm_pause_after_select_miss(
        &owner,
        Arc::clone(&select_miss_barrier),
        Arc::clone(&release_barrier),
    );

    let store_a = Arc::clone(&store);
    let owner_a = owner.clone();
    let thread_a = thread.clone();
    let scope_a = scope.clone();
    let handle_a = tokio::spawn(async move {
        store_a
            .session()
            .find_or_create(&owner_a, Some(&thread_a), &scope_a)
            .await
    });

    let store_b = Arc::clone(&store);
    let owner_b = owner.clone();
    let thread_b = thread.clone();
    let scope_b = scope.clone();
    let handle_b = tokio::spawn(async move {
        store_b
            .session()
            .find_or_create(&owner_b, Some(&thread_b), &scope_b)
            .await
    });

    tokio::time::timeout(Duration::from_secs(2), select_miss_barrier.wait())
        .await
        .expect("both tasks should pause after SELECT miss");
    tokio::time::timeout(Duration::from_secs(2), release_barrier.wait())
        .await
        .expect("release paused find_or_create tasks");

    let session_a = handle_a.await.expect("task a").expect("find_or_create a");
    let session_b = handle_b.await.expect("task b").expect("find_or_create b");
    drop(pause_guard);

    assert_eq!(
        session_a.id, session_b.id,
        "concurrent find_or_create must converge on one session row",
    );

    let row_count: i64 = sqlx::query("SELECT COUNT(*) AS c FROM sessions WHERE owner = ?")
        .bind(owner.as_str())
        .fetch_one(&pool)
        .await
        .expect("count rows")
        .try_get("c")
        .expect("count column");
    assert_eq!(
        row_count, 1,
        "exactly one persisted row expected for shared scope tuple",
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn distinct_scope_agent_concurrent_keeps_two_rows() {
    let ctx = concurrent_ctx().await;
    let store = Arc::clone(&ctx.store);
    let pool = ctx.pool.clone();
    let owner = unique_owner("distinct-agent");
    let scope_a = MemoryScope {
        agent: Some("agent_alpha".to_owned()),
        ..MemoryScope::default()
    };
    let scope_b = MemoryScope {
        agent: Some("agent_beta".to_owned()),
        ..MemoryScope::default()
    };
    let barrier = Arc::new(Barrier::new(2));

    let store_a = Arc::clone(&store);
    let owner_a = owner.clone();
    let barrier_a = Arc::clone(&barrier);
    let handle_a = tokio::spawn(async move {
        barrier_a.wait().await;
        store_a
            .session()
            .find_or_create(&owner_a, None, &scope_a)
            .await
    });

    let store_b = Arc::clone(&store);
    let owner_b = owner.clone();
    let barrier_b = Arc::clone(&barrier);
    let handle_b = tokio::spawn(async move {
        barrier_b.wait().await;
        store_b
            .session()
            .find_or_create(&owner_b, None, &scope_b)
            .await
    });

    let session_a = handle_a.await.expect("task a").expect("find_or_create a");
    let session_b = handle_b.await.expect("task b").expect("find_or_create b");

    assert_ne!(
        session_a.id, session_b.id,
        "distinct scope.agent must yield distinct rows",
    );

    let row_count: i64 = sqlx::query("SELECT COUNT(*) AS c FROM sessions WHERE owner = ?")
        .bind(owner.as_str())
        .fetch_one(&pool)
        .await
        .expect("count rows")
        .try_get("c")
        .expect("count column");
    assert_eq!(
        row_count, 2,
        "two distinct rows expected for distinct agents"
    );
}
