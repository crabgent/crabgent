//! Concurrent `find_or_create` race pin.
//!
//! Pins the contract that two concurrent `find_or_create` calls with the same
//! `(owner, thread, scope)` tuple produce exactly one row, regardless of
//! interleaving. The race is closed by the `sessions_scope_distinct` partial
//! unique index (`NULLS NOT DISTINCT`, PG 15+) plus
//! `INSERT ... ON CONFLICT DO NOTHING RETURNING`.

#[path = "../src/test_helpers.rs"]
mod test_helpers;

use std::sync::Arc;
use std::time::Duration;

use crabgent_core::{MemoryScope, Owner, ThreadId};
use crabgent_store::SessionStore;
use crabgent_store_postgres::{PostgresStore, arm_pause_after_select_miss};
use sqlx::Row;
use test_helpers::postgres_test_ctx;
use tokio::sync::Barrier;
use uuid::Uuid;

fn unique_owner(suffix: &str) -> Owner {
    Owner::new(format!("pg-race-{suffix}-{}", Uuid::now_v7()))
}

/// Two concurrent `find_or_create` calls with the same scope tuple resolve to
/// exactly one row. With the unique scope-tuple index, the second call's INSERT
/// is dropped by `DO NOTHING` and the re-SELECT returns the winner's row.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn same_scope_concurrent_resolves_to_one_row() {
    let ctx = postgres_test_ctx().await;
    let store = Arc::new(PostgresStore::from_pool(ctx.pool.clone()));
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
            .session_store()
            .find_or_create(&owner_a, Some(&thread_a), &scope_a)
            .await
    });

    let store_b = Arc::clone(&store);
    let owner_b = owner.clone();
    let thread_b = thread.clone();
    let scope_b = scope.clone();
    let handle_b = tokio::spawn(async move {
        store_b
            .session_store()
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

    let row_count: i64 = sqlx::query("SELECT COUNT(*) AS c FROM sessions WHERE owner = $1")
        .bind(owner.as_str())
        .fetch_one(&ctx.pool)
        .await
        .expect("count rows")
        .try_get("c")
        .expect("count column");
    assert_eq!(
        row_count, 1,
        "exactly one persisted row expected for shared scope tuple",
    );
}

/// Two concurrent `find_or_create` calls with the same owner but distinct
/// `scope.agent` values produce two distinct rows. Sanity check that the
/// unique index does not over-coalesce.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn distinct_scope_agent_concurrent_keeps_two_rows() {
    let ctx = postgres_test_ctx().await;
    let store = Arc::new(PostgresStore::from_pool(ctx.pool.clone()));
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
            .session_store()
            .find_or_create(&owner_a, None, &scope_a)
            .await
    });

    let store_b = Arc::clone(&store);
    let owner_b = owner.clone();
    let barrier_b = Arc::clone(&barrier);
    let handle_b = tokio::spawn(async move {
        barrier_b.wait().await;
        store_b
            .session_store()
            .find_or_create(&owner_b, None, &scope_b)
            .await
    });

    let session_a = handle_a.await.expect("task a").expect("find_or_create a");
    let session_b = handle_b.await.expect("task b").expect("find_or_create b");

    assert_ne!(
        session_a.id, session_b.id,
        "distinct scope.agent must yield distinct rows",
    );

    let row_count: i64 = sqlx::query("SELECT COUNT(*) AS c FROM sessions WHERE owner = $1")
        .bind(owner.as_str())
        .fetch_one(&ctx.pool)
        .await
        .expect("count rows")
        .try_get("c")
        .expect("count column");
    assert_eq!(
        row_count, 2,
        "two distinct rows expected for distinct agents"
    );
}
