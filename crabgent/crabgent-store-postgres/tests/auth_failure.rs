#[path = "../src/test_helpers.rs"]
mod test_helpers;

use crabgent_store::StoreError;
use crabgent_store_postgres::{PostgresStore, PostgresStoreConfig};
use test_helpers::postgres_test_ctx;

/// Uses a fresh container by default. When `PG_TEST_DSN` is set, the test reuses
/// that server endpoint and only mutates the password part of the DSN.
#[tokio::test]
async fn auth_failure_no_dsn_leak() {
    let ctx = postgres_test_ctx().await;
    let password = "secret-test-password-99999";
    let mut bad_url = url::Url::parse(&ctx.dsn).expect("test DSN must parse as URL");
    bad_url
        .set_password(Some(password))
        .expect("test DSN must allow password mutation");
    let bad_dsn = bad_url.to_string();
    assert_ne!(bad_dsn, ctx.dsn);

    let Err(err) = PostgresStore::open(PostgresStoreConfig::new(bad_dsn.clone())).await else {
        panic!("bad password must fail");
    };
    let rendered = err.to_string();

    assert!(matches!(err, StoreError::Backend(_)));
    assert!(rendered.contains("postgres connection unavailable"));
    assert!(!rendered.contains(password));
    assert!(!rendered.contains(&bad_dsn));
}
