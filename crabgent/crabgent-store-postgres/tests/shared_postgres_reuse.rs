#[path = "../src/test_helpers.rs"]
mod test_helpers;

use test_helpers::postgres_test_ctx;

#[tokio::test]
async fn shared_postgres_container_reused() {
    if std::env::var("PG_TEST_DSN").is_ok() {
        return;
    }

    let first = postgres_test_ctx().await;
    let second = postgres_test_ctx().await;

    assert_eq!(first.container_port, second.container_port);
    assert!(first.container_port.is_some());
    assert_ne!(first.db_name, second.db_name);
}
