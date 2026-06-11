#[path = "support/mod.rs"]
mod support;

#[tokio::test]
async fn shared_conduit_container_reused() {
    let real_server = std::env::var("MATRIX_HOMESERVER").is_ok()
        && std::env::var("MATRIX_USER").is_ok()
        && std::env::var("MATRIX_PASSWORD").is_ok();
    if real_server {
        return;
    }

    let Some(first) = support::matrix_test_ctx()
        .await
        .expect("first Matrix test context should initialize")
    else {
        return;
    };
    let Some(second) = support::matrix_test_ctx()
        .await
        .expect("second Matrix test context should initialize")
    else {
        return;
    };

    assert_eq!(first.homeserver_url, second.homeserver_url);
    assert_ne!(first.user, second.user);
}
