use crabgent_channel::{AudioRejection, AudioValidator, MAX_AUDIO_BYTES};

fn wav_bytes() -> Vec<u8> {
    b"RIFF\0\0\0\0WAVE".to_vec()
}

fn ogg_bytes() -> Vec<u8> {
    b"OggS\0\0\0\0".to_vec()
}

fn ogg_opus_bytes() -> Vec<u8> {
    let mut bytes = vec![0_u8; 36];
    bytes
        .get_mut(..4)
        .expect("test buffer has OggS header space")
        .copy_from_slice(b"OggS");
    bytes
        .get_mut(28..36)
        .expect("test buffer has OpusHead header space")
        .copy_from_slice(b"OpusHead");
    bytes
}

#[test]
fn too_large_rejected() {
    let validator = AudioValidator::new();
    let over_limit = usize::try_from(MAX_AUDIO_BYTES).expect("test size fits usize") + 1;
    let bytes = vec![0_u8; over_limit];

    let err = validator
        .validate(&bytes, "audio/wav")
        .expect_err("oversized audio rejected");

    assert!(matches!(err, AudioRejection::TooLarge));
}

#[test]
fn unsupported_mime_rejected() {
    let validator = AudioValidator::new();

    let err = validator
        .validate(&wav_bytes(), "audio/aiff")
        .expect_err("unsupported MIME rejected");

    assert!(matches!(err, AudioRejection::UnsupportedMime));
}

#[test]
fn magic_byte_mismatch_rejected() {
    let validator = AudioValidator::new();

    let err = validator
        .validate(&wav_bytes(), "audio/mp3")
        .expect_err("MIME mismatch rejected");

    assert!(matches!(err, AudioRejection::MagicByteMismatch));
}

#[test]
fn ok_opus_passes() {
    let validator = AudioValidator::new();

    validator
        .validate(&ogg_opus_bytes(), "audio/opus")
        .expect("Ogg Opus accepted");
}

#[test]
fn ok_ogg_passes() {
    let validator = AudioValidator::new();

    validator
        .validate(&ogg_bytes(), "audio/ogg")
        .expect("Ogg accepted");
}

#[test]
fn ok_wav_passes() {
    let validator = AudioValidator::new();

    validator
        .validate(&wav_bytes(), "audio/wav")
        .expect("WAV accepted");
}
