//! Panic-safe `BEGIN IMMEDIATE` wrapper for `SessionStore::find_or_create`.

use sqlx::Sqlite;
use sqlx::pool::PoolConnection;
use sqlx::sqlite::{SqliteConnection, SqlitePool};

use crabgent_core::{MemoryScope, Owner, ThreadId};
use crabgent_store::{Session, StoreError};

use crate::retry::map_sqlx_error;
use crate::session::find_or_create::find_or_create_inside_tx;

struct ImmediateTransaction {
    conn: PoolConnection<Sqlite>,
    disarmed: bool,
}

struct TxFinalization {
    sql: &'static str,
    name: &'static str,
    is_commit: bool,
}

impl ImmediateTransaction {
    const fn new(conn: PoolConnection<Sqlite>) -> Self {
        Self {
            conn,
            disarmed: false,
        }
    }

    fn conn_mut(&mut self) -> &mut SqliteConnection {
        &mut self.conn
    }

    const fn disarm(&mut self) {
        self.disarmed = true;
    }
}

impl Drop for ImmediateTransaction {
    fn drop(&mut self) {
        if !self.disarmed {
            self.conn.close_on_drop();
        }
    }
}

pub(super) async fn find_or_create_in_immediate_tx(
    pool: &SqlitePool,
    owner: &Owner,
    thread: Option<&ThreadId>,
    scope: &MemoryScope,
) -> Result<Session, StoreError> {
    // `BEGIN IMMEDIATE` serialises the read+write through the RESERVED
    // write lock (busy_timeout=5s) so a concurrent peer re-SELECTs the
    // winner. `sqlx::Pool::begin()` is `BEGIN DEFERRED` in sqlx 0.8.
    let mut tx = begin_immediate_tx(pool).await?;
    pause_after_begin_if_configured(owner.as_str()).await;
    let result = run_find_or_create_inside(&mut tx, owner, thread, scope).await;
    finalize_immediate_tx(tx, owner, result).await
}

async fn begin_immediate_tx(pool: &SqlitePool) -> Result<ImmediateTransaction, StoreError> {
    let handle = pool
        .acquire()
        .await
        .map_err(|e| map_sqlx_error("session.find_or_create.acquire", &e))?;
    let mut tx = ImmediateTransaction::new(handle);
    sqlx::query("BEGIN IMMEDIATE")
        .execute(tx.conn_mut())
        .await
        .map_err(|e| map_sqlx_error("session.find_or_create.begin", &e))?;
    Ok(tx)
}

async fn run_find_or_create_inside(
    tx: &mut ImmediateTransaction,
    owner: &Owner,
    thread: Option<&ThreadId>,
    scope: &MemoryScope,
) -> Result<Session, StoreError> {
    find_or_create_inside_tx(tx.conn_mut(), owner, thread, scope).await
}

async fn finalize_immediate_tx(
    mut tx: ImmediateTransaction,
    owner: &Owner,
    result: Result<Session, StoreError>,
) -> Result<Session, StoreError> {
    let is_commit = result.is_ok();
    let finalization = tx_finalization(is_commit, owner);
    match execute_finalization(&mut tx, &finalization).await {
        Ok(()) => {
            tx.disarm();
            result
        }
        Err(err) => handle_finalization_error(&finalization, &err, result),
    }
}

fn tx_finalization(is_commit: bool, owner: &Owner) -> TxFinalization {
    let sql = if is_commit { "COMMIT" } else { "ROLLBACK" };
    TxFinalization {
        sql: override_finalize_sql_if_configured(sql, owner.as_str()),
        name: tx_finalization_name(is_commit),
        is_commit,
    }
}

const fn tx_finalization_name(is_commit: bool) -> &'static str {
    if is_commit {
        "session.find_or_create.commit"
    } else {
        "session.find_or_create.rollback"
    }
}

async fn execute_finalization(
    tx: &mut ImmediateTransaction,
    finalization: &TxFinalization,
) -> Result<(), sqlx::Error> {
    sqlx::query(finalization.sql).execute(tx.conn_mut()).await?;
    Ok(())
}

fn handle_finalization_error(
    finalization: &TxFinalization,
    err: &sqlx::Error,
    result: Result<Session, StoreError>,
) -> Result<Session, StoreError> {
    crabgent_log::warn!(
        operation = "session.find_or_create",
        stage = finalization.sql,
        error = %err,
        "sqlite transaction finalize failed; discarding pool connection",
    );
    if finalization.is_commit {
        return Err(map_sqlx_error(finalization.name, err));
    }
    result
}

#[cfg(test)]
pub struct PauseAfterBeginGuard;

#[cfg(test)]
pub struct CommitSqlGuard;

#[cfg(test)]
pub fn pause_after_begin(
    owner: impl Into<String>,
    notify: std::sync::Arc<tokio::sync::Notify>,
) -> PauseAfterBeginGuard {
    let mut slot = after_begin_hook()
        .lock()
        .expect("after-begin hook mutex should not be poisoned");
    *slot = Some(AfterBeginHook {
        owner: owner.into(),
        notify,
    });
    PauseAfterBeginGuard
}

#[cfg(test)]
pub fn force_commit_sql(owner: impl Into<String>, sql: &'static str) -> CommitSqlGuard {
    let mut slot = commit_sql_override()
        .lock()
        .expect("commit SQL override mutex should not be poisoned");
    *slot = Some(CommitSqlOverride {
        owner: owner.into(),
        sql,
    });
    CommitSqlGuard
}

#[cfg(test)]
impl Drop for PauseAfterBeginGuard {
    fn drop(&mut self) {
        let mut slot = after_begin_hook()
            .lock()
            .expect("after-begin hook mutex should not be poisoned");
        *slot = None;
    }
}

#[cfg(test)]
impl Drop for CommitSqlGuard {
    fn drop(&mut self) {
        let mut slot = commit_sql_override()
            .lock()
            .expect("commit SQL override mutex should not be poisoned");
        *slot = None;
    }
}

#[cfg(test)]
#[derive(Clone)]
struct AfterBeginHook {
    owner: String,
    notify: std::sync::Arc<tokio::sync::Notify>,
}

#[cfg(test)]
struct CommitSqlOverride {
    owner: String,
    sql: &'static str,
}

#[cfg(test)]
fn after_begin_hook() -> &'static std::sync::Mutex<Option<AfterBeginHook>> {
    static HOOK: std::sync::OnceLock<std::sync::Mutex<Option<AfterBeginHook>>> =
        std::sync::OnceLock::new();
    HOOK.get_or_init(|| std::sync::Mutex::new(None))
}

#[cfg(test)]
fn commit_sql_override() -> &'static std::sync::Mutex<Option<CommitSqlOverride>> {
    static OVERRIDE: std::sync::OnceLock<std::sync::Mutex<Option<CommitSqlOverride>>> =
        std::sync::OnceLock::new();
    OVERRIDE.get_or_init(|| std::sync::Mutex::new(None))
}

#[cfg(test)]
async fn pause_after_begin_if_configured(owner: &str) {
    let hook = after_begin_hook()
        .lock()
        .expect("after-begin hook mutex should not be poisoned")
        .clone();
    if let Some(hook) = hook
        && hook.owner == owner
    {
        hook.notify.notify_one();
        std::future::pending::<()>().await;
    }
}

#[cfg(not(test))]
async fn pause_after_begin_if_configured(_owner: &str) {}

#[cfg(test)]
fn override_finalize_sql_if_configured(sql: &'static str, owner: &str) -> &'static str {
    if sql != "COMMIT" {
        return sql;
    }
    let slot = commit_sql_override()
        .lock()
        .expect("commit SQL override mutex should not be poisoned");
    match slot.as_ref() {
        Some(override_sql) if override_sql.owner == owner => override_sql.sql,
        _ => sql,
    }
}

#[cfg(not(test))]
const fn override_finalize_sql_if_configured(sql: &'static str, _owner: &str) -> &'static str {
    sql
}
