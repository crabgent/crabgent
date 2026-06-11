use crabgent_core::message::{IMAGE_PAYLOAD_MAX_BYTES, ImagePayload};
use serde_json::json;

#[test]
fn image_payload_deserialize_rejects_oversized_pre_decode() {
    let wire_data_max = IMAGE_PAYLOAD_MAX_BYTES * 4 / 3 + 16;
    let value = json!({
        "mime": "image/png",
        "data": "!".repeat(wire_data_max + 1),
    });

    let err = serde_json::from_value::<ImagePayload>(value)
        .expect_err("oversized wire data rejected before base64 decode");

    let message = err.to_string();
    assert!(
        message.contains(&format!(
            "image payload exceeds {IMAGE_PAYLOAD_MAX_BYTES} bytes"
        )),
        "{message}"
    );
    assert!(
        !message.contains("Invalid byte"),
        "expected pre-decode size rejection, got: {message}"
    );
}
