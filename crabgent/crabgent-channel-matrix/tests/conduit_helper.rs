#[path = "support/conduit_config.rs"]
mod conduit_config;

#[test]
fn conduit_image_tag_is_pinned() {
    assert_eq!(
        conduit_config::CONDUIT_IMAGE,
        "matrixconduit/matrix-conduit"
    );
    assert_eq!(conduit_config::CONDUIT_TAG, "v0.10.6");
    assert_eq!(conduit_config::CONDUIT_PORT, 6167);
}
