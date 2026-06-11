use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use crabgent_core::{MemoryScope, Owner};
use crabgent_store::{SessionStore, Store};
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use uuid::Uuid;

use crate::backend::SqliteStore;
use crate::session::immediate_transaction::{force_commit_sql, pause_after_begin};

struct FileSqliteStore {
    store: SqliteStore,
    path: PathBuf,
}

impl Drop for FileSqliteStore {
    fn drop(&mut self) {
        drop(std::fs::remove_file(&self.path));
        let mut wal = self.path.clone();
        wal.as_mut_os_string().push("-wal");
        drop(std::fs::remove_file(&wal));
        let mut shm = self.path.clone();
        shm.as_mut_os_string().push("-shm");
        drop(std::fs::remove_file(&shm));
    }
}

async fn file_store() -> FileSqliteStore {
    let path = std::env::temp_dir().join(format!(
        "crabgent-sqlite-find-or-create-abort-{}.db",
        Uuid::now_v7().simple()
    ));
    let opts = SqliteConnectOptions::new()
        .filename(&path)
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal)
        .busy_timeout(Duration::from_millis(100));
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(opts)
        .await
        .expect("connect sqlite test pool");
    let store = SqliteStore::from_pool(pool)
        .await
        .expect("open sqlite store");
    FileSqliteStore { store, path }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn aborted_find_or_create_after_begin_does_not_poison_pool() {
    let ctx = file_store().await;
    let notify = Arc::new(tokio::sync::Notify::new());
    let paused_owner = Owner::new("sqlite-abort-paused");
    let pause_guard = pause_after_begin(paused_owner.as_str(), Arc::clone(&notify));
    let paused_store = ctx.store.session().clone();
    let handle = tokio::spawn(async move {
        paused_store
            .find_or_create(&paused_owner, None, &MemoryScope::default())
            .await
    });

    tokio::time::timeout(Duration::from_secs(2), notify.notified())
        .await
        .expect("paused find_or_create should reach BEGIN IMMEDIATE");
    handle.abort();
    let join_err = handle
        .await
        .expect_err("paused find_or_create task should be aborted");
    assert!(join_err.is_cancelled(), "unexpected join error: {join_err}");
    drop(pause_guard);

    let owner = Owner::new("sqlite-abort-after");
    let next = tokio::time::timeout(
        Duration::from_secs(2),
        ctx.store
            .session()
            .find_or_create(&owner, None, &MemoryScope::default()),
    )
    .await
    .expect("next find_or_create should not hang on an orphan transaction");

    next.expect("next find_or_create should use a healthy connection");
}

#[tokio::test]
async fn find_or_create_returns_commit_failure() {
    let ctx = file_store().await;
    let owner = Owner::new("sqlite-commit-fails");
    let guard = force_commit_sql(owner.as_str(), "COMMIT invalid");

    let err = ctx
        .store
        .session()
        .find_or_create(&owner, None, &MemoryScope::default())
        .await
        .expect_err("forced commit failure must propagate");

    drop(guard);
    assert!(
        matches!(err, crabgent_store::StoreError::Backend(ref msg) if msg.contains("session.find_or_create.commit")),
        "unexpected error: {err:?}"
    );
}
