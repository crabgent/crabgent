#[tokio::test]
#[ignore = "requires MATRIX_HOMESERVER, MATRIX_USER, MATRIX_ACCESS_TOKEN, MATRIX_DEVICE_ID"]
async fn smoke_matrix_environment_is_present() {
    for key in [
        "MATRIX_HOMESERVER",
        "MATRIX_USER",
        "MATRIX_ACCESS_TOKEN",
        "MATRIX_DEVICE_ID",
    ] {
        assert!(
            std::env::var(key).is_ok(),
            "missing required smoke-test env var {key}"
        );
    }
}
