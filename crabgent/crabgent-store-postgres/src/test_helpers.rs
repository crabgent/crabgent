#![cfg(any(test, feature = "test-support"))]
//! Shared Postgres integration-test helpers.

use std::{
    env,
    sync::{Arc, Mutex, OnceLock},
    time::Duration,
};

use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use testcontainers::runners::AsyncRunner;
use testcontainers::{ContainerAsync, ImageExt};
use testcontainers_modules::postgres::Postgres;
use tokio::sync::Mutex as AsyncMutex;

use crabgent_store_postgres::{DEFAULT_EMBEDDING_DIM, PostgresStore, PostgresStoreConfig};

/// Pinned Postgres image for integration tests.
pub const PG_IMAGE: &str = "pgvector/pgvector:pg18";
const PG_DB: &str = "postgres";
const PG_USER: &str = "postgres";
const PG_PASSWORD: &str = "postgres";
const PG_MAPPED_PORT_ATTEMPTS: usize = 480;
const PG_MAPPED_PORT_POLL: Duration = Duration::from_millis(250);
const TEST_STORE_MAX_CONNECTIONS: u32 = 4;
static SHARED_POSTGRES: OnceLock<SharedPostgresSlot> = OnceLock::new();

struct SharedPostgresSlot {
    init_lock: AsyncMutex<()>,
    shared: Mutex<Option<Arc<SharedPostgres>>>,
}

struct SharedPostgres {
    _container: ContainerAsync<Postgres>,
    admin_pool: PgPool,
    /// DSN for the container's admin database, used to open a fresh, dedicated
    /// `PgPool` inside `PostgresTestCtx::drop`. Reusing `admin_pool` here is
    /// not safe: the shared pool's connection futures are bound to the
    /// `#[tokio::test]` runtime that produced it, but `Drop` runs cleanup on
    /// a separate `std::thread` plus a new `current_thread` runtime, so
    /// driving the pool from that runtime stalls until `acquire_timeout`
    /// fires with `PoolTimedOut`. See Plan Amendment v2.5 for the full
    /// repro and the won't-fix rationale.
    admin_dsn: String,
    host: String,
    container_port: u16,
}

struct DatabaseCleanup {
    admin_dsn: String,
    shared: Option<Arc<SharedPostgres>>,
}

/// Test context. Dropping it drops the per-test database.
pub struct PostgresTestCtx {
    pub db_name: String,
    pub pool: PgPool,
    pub dsn: String,
    pub container_port: Option<u16>,
    cleanup: Option<DatabaseCleanup>,
}

/// Start or connect to a Postgres test database and run migrations.
pub async fn postgres_test_ctx() -> PostgresTestCtx {
    postgres_test_ctx_with_embedding_dim(DEFAULT_EMBEDDING_DIM).await
}

/// Start or connect to a Postgres test database with a custom vector dimension.
pub async fn postgres_test_ctx_with_embedding_dim(embedding_dim: usize) -> PostgresTestCtx {
    let (db_name, dsn, container_port, cleanup) = test_database().await;
    migrated_ctx_from_dsn(db_name, dsn, container_port, cleanup, embedding_dim).await
}

/// Start or connect to a fresh test database without running migrations.
pub async fn postgres_unmigrated_test_ctx() -> PostgresTestCtx {
    let (db_name, dsn, container_port, cleanup) = test_database().await;
    unmigrated_ctx_from_dsn(db_name, dsn, container_port, cleanup).await
}

async fn test_database() -> (String, String, Option<u16>, Option<DatabaseCleanup>) {
    if let Ok(dsn) = env::var("PG_TEST_DSN") {
        return external_database(dsn).await;
    }
    container_database().await
}

async fn container_database() -> (String, String, Option<u16>, Option<DatabaseCleanup>) {
    let shared = shared_postgres().await;
    let db_name = unique_db_name();
    sqlx::query(sqlx::AssertSqlSafe(format!("CREATE DATABASE {db_name}")))
        .execute(&shared.admin_pool)
        .await
        .expect("create postgres test database");
    let dsn = postgres_dsn(&shared.host, shared.container_port, &db_name);
    let container_port = Some(shared.container_port);
    let cleanup = Some(DatabaseCleanup {
        admin_dsn: shared.admin_dsn.clone(),
        shared: Some(shared),
    });

    (db_name, dsn, container_port, cleanup)
}

async fn shared_postgres() -> Arc<SharedPostgres> {
    let slot = shared_postgres_slot();
    if let Some(shared) = slot.current() {
        return shared;
    }

    let _init = slot.init_lock.lock().await;
    if let Some(shared) = slot.current() {
        return shared;
    }

    let shared = Arc::new(start_shared_postgres().await);
    slot.set(Arc::clone(&shared));
    shared
}

async fn start_shared_postgres() -> SharedPostgres {
    let container = Postgres::default()
        .with_db_name(PG_DB)
        .with_user(PG_USER)
        .with_password(PG_PASSWORD)
        .with_name(pg_image_name())
        .with_tag(pg_tag())
        .with_startup_timeout(Duration::from_mins(2))
        .start()
        .await
        .expect("start postgres testcontainer");
    let host = container.get_host().await.expect("read postgres host");
    let container_port = mapped_pg_port(&container)
        .await
        .expect("read postgres mapped port");
    let host = host.to_string();
    let admin_dsn = postgres_dsn(&host, container_port, PG_DB);
    let admin_pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&admin_dsn)
        .await
        .expect("open postgres admin pool");

    SharedPostgres {
        _container: container,
        admin_pool,
        admin_dsn,
        host,
        container_port,
    }
}

async fn external_database(
    admin_dsn: String,
) -> (String, String, Option<u16>, Option<DatabaseCleanup>) {
    let db_name = unique_db_name();
    let dsn = test_database_dsn(&admin_dsn, &db_name);
    let admin_pool = PgPoolOptions::new()
        .max_connections(1)
        .connect(&admin_dsn)
        .await
        .expect("open external postgres admin pool");
    sqlx::query(sqlx::AssertSqlSafe(format!("CREATE DATABASE {db_name}")))
        .execute(&admin_pool)
        .await
        .expect("create external postgres test database");
    admin_pool.close().await;

    (
        db_name,
        dsn,
        None,
        Some(DatabaseCleanup {
            admin_dsn,
            shared: None,
        }),
    )
}

async fn migrated_ctx_from_dsn(
    db_name: String,
    dsn: String,
    container_port: Option<u16>,
    cleanup: Option<DatabaseCleanup>,
    embedding_dim: usize,
) -> PostgresTestCtx {
    let config = PostgresStoreConfig::builder(dsn.clone())
        .embedding_dim(embedding_dim)
        .max_connections(TEST_STORE_MAX_CONNECTIONS)
        .build();
    let store = PostgresStore::open(config)
        .await
        .expect("open postgres test store");
    let pool = store.session_store().pool().clone();

    PostgresTestCtx {
        db_name,
        pool,
        dsn,
        container_port,
        cleanup,
    }
}

async fn unmigrated_ctx_from_dsn(
    db_name: String,
    dsn: String,
    container_port: Option<u16>,
    cleanup: Option<DatabaseCleanup>,
) -> PostgresTestCtx {
    let pool = PgPoolOptions::new()
        .max_connections(TEST_STORE_MAX_CONNECTIONS)
        .connect(&dsn)
        .await
        .expect("open unmigrated postgres test pool");

    PostgresTestCtx {
        db_name,
        pool,
        dsn,
        container_port,
        cleanup,
    }
}

fn unique_db_name() -> String {
    format!("test_{}", uuid::Uuid::now_v7().simple())
}

async fn mapped_pg_port(container: &ContainerAsync<Postgres>) -> Result<u16, String> {
    let mut last_error = None;
    for _ in 0..PG_MAPPED_PORT_ATTEMPTS {
        match container.get_host_port_ipv4(5432).await {
            Ok(port) => return Ok(port),
            Err(error) => {
                last_error = Some(format!("{error:?}"));
            }
        }
        tokio::time::sleep(PG_MAPPED_PORT_POLL).await;
    }

    Err(format!(
        "read postgres mapped port after {} attempts: {}",
        PG_MAPPED_PORT_ATTEMPTS,
        last_error.unwrap_or_else(|| "unknown error".to_owned())
    ))
}

fn postgres_dsn(host: &str, port: u16, db_name: &str) -> String {
    format!("postgres://{PG_USER}:{PG_PASSWORD}@{host}:{port}/{db_name}?sslmode=disable")
}

fn test_database_dsn(admin_dsn: &str, db_name: &str) -> String {
    let mut url = url::Url::parse(admin_dsn).expect("PG_TEST_DSN must parse as URL");
    url.set_path(db_name);
    url.to_string()
}

fn pg_tag() -> &'static str {
    PG_IMAGE
        .strip_prefix("pgvector/pgvector:")
        .expect("PG_IMAGE must use pgvector/pgvector:<tag>")
}

fn pg_image_name() -> &'static str {
    PG_IMAGE
        .split_once(':')
        .map(|(name, _tag)| name)
        .expect("PG_IMAGE must include a tag")
}

impl Drop for PostgresTestCtx {
    fn drop(&mut self) {
        let _ = (&self.dsn, self.container_port);
        let Some(cleanup) = self.cleanup.take() else {
            return;
        };

        // Cleanup runs on a dedicated `std::thread` plus a new
        // `current_thread` runtime so it does not depend on the calling test
        // still owning a live tokio runtime. The DROP DATABASE call uses a
        // fresh `PgPool` whose connection I/O is bound to that cleanup
        // runtime; reusing a shared pool here would bind those futures to the
        // parent test runtime and deadlock with `PoolTimedOut`.
        let admin_dsn = cleanup.admin_dsn;
        let replacement_pool = PgPoolOptions::new()
            .max_connections(1)
            .connect_lazy(&admin_dsn)
            .expect("build postgres cleanup placeholder pool");
        let shared = cleanup.shared;
        let db_name = std::mem::take(&mut self.db_name);
        let pool = std::mem::replace(&mut self.pool, replacement_pool);
        std::thread::spawn(move || {
            drop(pool);
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("build postgres cleanup runtime");
            rt.block_on(async move {
                let admin_pool = PgPoolOptions::new()
                    .max_connections(1)
                    .connect(&admin_dsn)
                    .await
                    .expect("open postgres cleanup admin pool");
                sqlx::query(sqlx::AssertSqlSafe(format!(
                    "DROP DATABASE IF EXISTS {db_name} WITH (FORCE)"
                )))
                .execute(&admin_pool)
                .await
                .expect("drop postgres test database");
                admin_pool.close().await;
            });
            if let Some(shared) = shared {
                drop_shared_postgres_if_idle(shared, &rt);
            }
        })
        .join()
        .expect("join postgres cleanup thread");
    }
}

impl SharedPostgresSlot {
    fn new() -> Self {
        Self {
            init_lock: AsyncMutex::new(()),
            shared: Mutex::new(None),
        }
    }

    fn current(&self) -> Option<Arc<SharedPostgres>> {
        self.shared
            .lock()
            .expect("lock shared postgres slot")
            .clone()
    }

    fn set(&self, shared: Arc<SharedPostgres>) {
        *self.shared.lock().expect("lock shared postgres slot") = Some(shared);
    }

    fn take_if_idle(&self, shared: &Arc<SharedPostgres>) -> Option<Arc<SharedPostgres>> {
        let mut guard = self.shared.lock().expect("lock shared postgres slot");
        let slot_shared = guard.as_ref()?;
        if !Arc::ptr_eq(slot_shared, shared) || Arc::strong_count(shared) != 2 {
            return None;
        }
        guard.take()
    }
}

fn shared_postgres_slot() -> &'static SharedPostgresSlot {
    SHARED_POSTGRES.get_or_init(SharedPostgresSlot::new)
}

fn drop_shared_postgres_if_idle(shared: Arc<SharedPostgres>, rt: &tokio::runtime::Runtime) {
    let Some(slot_shared) = shared_postgres_slot().take_if_idle(&shared) else {
        return;
    };

    rt.block_on(slot_shared.admin_pool.close());
    let _runtime_guard = rt.enter();
    drop(slot_shared);
    drop(shared);
}

const _: () = {
    let _ = postgres_test_ctx;
    let _ = postgres_unmigrated_test_ctx;
    let _ = unmigrated_ctx_from_dsn;
};
