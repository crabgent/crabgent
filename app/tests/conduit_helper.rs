mod support;

#[tokio::test]
async fn conduit_image_tag_is_pinned() {
    assert_eq!(support::CONDUIT_IMAGE, "matrixconduit/matrix-conduit");
    assert_eq!(support::CONDUIT_TAG, "v0.9.0");
}
