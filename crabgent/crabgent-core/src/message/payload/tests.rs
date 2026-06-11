use super::*;
use serde_json::json;

#[test]
fn image_payload_construct_via_new() {
    let payload = ImagePayload::new(vec![1_u8, 2, 3], "image/png").expect("valid image payload");

    assert_eq!(payload.bytes().as_ref(), &[1, 2, 3]);
    assert_eq!(payload.mime(), "image/png");
}

#[test]
fn image_payload_serde_roundtrip() {
    let payload = ImagePayload::new(b"hello".to_vec(), "image/jpeg").expect("valid image payload");

    let value = serde_json::to_value(&payload).expect("ser");
    assert_eq!(value, json!({"mime": "image/jpeg", "data": "aGVsbG8="}));
    assert!(value["data"].is_string());

    let back: ImagePayload = serde_json::from_value(value).expect("de");
    assert_eq!(back.bytes().as_ref(), b"hello");
    assert_eq!(back.mime(), "image/jpeg");
}

#[test]
fn image_payload_validation_rejects_oversized_size_hint() {
    let sentinel = [0_u8; 64];
    let err = validate_payload(
        "image",
        IMAGE_PAYLOAD_MAX_BYTES + 1,
        "image/png",
        IMAGE_PAYLOAD_ALLOWED_MIMES,
        IMAGE_PAYLOAD_MAX_BYTES,
    )
    .expect_err("oversized rejected");

    assert!(
        sentinel.len() <= 64,
        "oversized test must not allocate the full payload"
    );
    assert_eq!(
        err,
        PayloadError::TooLarge {
            kind: "image",
            max_bytes: IMAGE_PAYLOAD_MAX_BYTES
        }
    );
}

#[test]
fn image_payload_deserialize_rejects_unsupported_mime() {
    let value = json!({
        "mime": "application/octet-stream",
        "data": BASE64_STANDARD.encode(b"hello"),
    });

    let err = serde_json::from_value::<ImagePayload>(value).expect_err("unsupported MIME rejected");

    assert!(
        err.to_string()
            .contains("unsupported MIME type: application/octet-stream")
    );
}

#[test]
fn serde_roundtrip_audio_payload() {
    let payload = AudioPayload::new(b"hello".to_vec(), "audio/wav", Some("clip.wav".into()))
        .expect("valid audio payload");

    let value = serde_json::to_value(&payload).expect("ser");
    assert_eq!(
        value,
        json!({"mime": "audio/wav", "data": "aGVsbG8=", "filename": "clip.wav"})
    );

    let back: AudioPayload = serde_json::from_value(value).expect("de");
    assert_eq!(back.bytes().as_ref(), b"hello");
    assert_eq!(back.mime(), "audio/wav");
    assert_eq!(back.filename.as_deref(), Some("clip.wav"));
}

#[test]
fn audio_payload_accepts_tts_specific_mimes() {
    for mime in ["audio/aac", "audio/L16"] {
        let payload = AudioPayload::new(b"hello".to_vec(), mime, None).expect("valid TTS MIME");

        assert_eq!(payload.mime(), mime);
        assert_eq!(payload.bytes().as_ref(), b"hello");
    }
}

#[test]
fn serde_rejects_unknown_audio_mime() {
    let json = r#"{"mime":"audio/aiff","data":"aGVsbG8=","filename":"clip.aiff"}"#;

    let err = serde_json::from_str::<AudioPayload>(json).expect_err("audio MIME rejected");

    assert!(
        err.to_string()
            .contains("unsupported MIME type: audio/aiff")
    );
}

#[test]
fn serde_roundtrip_file_payload() {
    let payload =
        FilePayload::new(b"hello".to_vec(), "text/plain", "note.txt").expect("valid file payload");

    let value = serde_json::to_value(&payload).expect("ser");
    assert_eq!(
        value,
        json!({"mime": "text/plain", "data": "aGVsbG8=", "filename": "note.txt"})
    );

    let back: FilePayload = serde_json::from_value(value).expect("de");
    assert_eq!(back.bytes().as_ref(), b"hello");
    assert_eq!(back.mime(), "text/plain");
    assert_eq!(back.filename, "note.txt");
}

#[test]
fn audio_payload_validation_rejects_oversized_size_hint() {
    let sentinel = [0_u8; 64];
    let err = validate_payload(
        "audio",
        AUDIO_PAYLOAD_MAX_BYTES + 1,
        "audio/wav",
        AUDIO_PAYLOAD_ALLOWED_MIMES,
        AUDIO_PAYLOAD_MAX_BYTES,
    )
    .expect_err("oversized rejected");

    assert!(
        sentinel.len() <= 64,
        "oversized test must not allocate the full payload"
    );
    assert_eq!(
        err,
        PayloadError::TooLarge {
            kind: "audio",
            max_bytes: AUDIO_PAYLOAD_MAX_BYTES
        }
    );
}

#[test]
fn file_payload_validation_rejects_oversized_size_hint() {
    let sentinel = [0_u8; 64];
    let err = validate_payload(
        "file",
        FILE_PAYLOAD_MAX_BYTES + 1,
        "text/plain",
        FILE_PAYLOAD_ALLOWED_MIMES,
        FILE_PAYLOAD_MAX_BYTES,
    )
    .expect_err("oversized rejected");

    assert!(
        sentinel.len() <= 64,
        "oversized test must not allocate the full payload"
    );
    assert_eq!(
        err,
        PayloadError::TooLarge {
            kind: "file",
            max_bytes: FILE_PAYLOAD_MAX_BYTES
        }
    );
}

#[test]
fn debug_masks_bytes_content() {
    let audio = AudioPayload::new(vec![1_u8, 2, 3], "audio/wav", Some("clip.wav".into()))
        .expect("valid audio payload");
    let file =
        FilePayload::new(vec![4_u8, 5, 6], "text/plain", "note.txt").expect("valid file payload");

    let audio_debug = format!("{audio:?}");
    let file_debug = format!("{file:?}");

    assert!(audio_debug.contains("len=3"));
    assert!(file_debug.contains("len=3"));
    assert!(!audio_debug.contains("1, 2, 3"));
    assert!(!file_debug.contains("4, 5, 6"));
}
