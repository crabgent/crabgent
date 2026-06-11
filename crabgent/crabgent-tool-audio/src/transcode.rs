//! Transcode a retained clip into a format the Chat-audio model accepts.
//!
//! `OpenAI`'s Chat Completions `input_audio` content part accepts only `wav`
//! and `mp3`. Inbound voice from Telegram and Matrix is Ogg/Opus, and iOS
//! clients send m4a/aac, so a clip fetched from the [`AudioStore`] must be
//! transcoded before it can be sent to the audio model. We shell out to
//! `ffmpeg` (read the clip on stdin, write mp3 to stdout) instead of linking
//! a decoder: one external binary covers every container and codec, and a
//! missing binary degrades to a soft error rather than becoming a build-time
//! dependency.
//!
//! The function performs no logging and never panics. A missing `ffmpeg`, a
//! non-zero exit, or a hung process all map to a [`TranscodeError`]; the
//! caller treats every variant as a local prep failure that does not trip the
//! audio circuit breaker.
//!
//! [`AudioStore`]: crabgent_channel::AudioStore

use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::AsyncWriteExt;
use tokio::process::Command;

/// MIME types the Chat-audio `input_audio` part accepts as-is; everything
/// else is transcoded. Compared case-insensitively.
const PASSTHROUGH_MIMES: &[&str] = &["audio/wav", "audio/x-wav", "audio/mpeg", "audio/mp3"];

/// Wall-clock cap on one `ffmpeg` invocation. A short voice clip transcodes
/// in well under a second; this only bounds a hung or pathological process.
const TRANSCODE_TIMEOUT: Duration = Duration::from_secs(10);

/// Bitrate of the mp3 handed to the audio model. 64 kbps mono keeps the
/// prosodic detail tone analysis needs while staying roughly an order of
/// magnitude smaller than the equivalent wav.
const MP3_BITRATE: &str = "64k";

/// Sample rate of the transcoded clip, in hertz. Mono speech needs no more.
const MP3_SAMPLE_RATE: &str = "16000";

/// MIME declared on the transcoded payload so the provider wire maps it to
/// the `mp3` `input_audio` format.
const MP3_MIME: &str = "audio/mpeg";

/// Failure modes of [`ensure_chat_audio`].
///
/// No variant carries the clip bytes, a file path, or `ffmpeg` stderr: the
/// messages are fixed and safe to surface to an operator log.
#[derive(Debug, thiserror::Error)]
pub enum TranscodeError {
    /// `ffmpeg` is not installed or not on `PATH`.
    #[error("ffmpeg not available")]
    NotAvailable,
    /// `ffmpeg` exited non-zero or produced no output.
    #[error("ffmpeg transcode failed")]
    Failed,
    /// The transcode did not finish within [`TRANSCODE_TIMEOUT`].
    #[error("ffmpeg transcode timed out")]
    Timeout,
    /// Spawning `ffmpeg` or writing the clip to its stdin failed.
    #[error("ffmpeg io error")]
    Io(#[source] std::io::Error),
}

/// Whether `mime` is already a Chat-audio format and needs no transcode.
fn is_passthrough(mime: &str) -> bool {
    PASSTHROUGH_MIMES
        .iter()
        .any(|allowed| allowed.eq_ignore_ascii_case(mime))
}

/// Ensure `bytes` are in a format the Chat-audio model accepts.
///
/// `wav`/`mp3` clips pass through unchanged. Everything else (Ogg/Opus from
/// Telegram and Matrix, m4a/aac from iOS, webm, flac) is transcoded to mono
/// 16 kHz mp3 by piping it through `ffmpeg`. Returns the bytes to send plus
/// the MIME to declare on the payload.
///
/// # Errors
///
/// Returns [`TranscodeError`] when `ffmpeg` is missing, fails, times out, or
/// cannot be fed the clip. Passthrough never errors.
pub async fn ensure_chat_audio(
    bytes: Arc<[u8]>,
    mime: String,
) -> Result<(Arc<[u8]>, String), TranscodeError> {
    if is_passthrough(&mime) {
        return Ok((bytes, mime));
    }
    let mp3 = transcode_to_mp3(bytes).await?;
    Ok((Arc::from(mp3.as_slice()), MP3_MIME.to_owned()))
}

/// Pipe `input` through `ffmpeg` and return the mono 16 kHz mp3 bytes.
///
/// stdin is written by a detached task while stdout is drained by
/// `wait_with_output`, so the OS pipe buffer cannot deadlock a large clip.
/// `kill_on_drop` reaps the child if the timeout fires.
async fn transcode_to_mp3(input: Arc<[u8]>) -> Result<Vec<u8>, TranscodeError> {
    let mut child = Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-i",
            "pipe:0",
            "-ar",
            MP3_SAMPLE_RATE,
            "-ac",
            "1",
            "-b:a",
            MP3_BITRATE,
            "-f",
            "mp3",
            "pipe:1",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|err| match err.kind() {
            std::io::ErrorKind::NotFound => TranscodeError::NotAvailable,
            _ => TranscodeError::Io(err),
        })?;

    let Some(mut stdin) = child.stdin.take() else {
        return Err(TranscodeError::Failed);
    };
    // Write the clip on a detached task and drop stdin so ffmpeg sees EOF.
    // Draining stdout concurrently below prevents a pipe-buffer deadlock.
    let writer = tokio::spawn(async move {
        // Best-effort: a broken pipe here surfaces as a non-zero ffmpeg exit
        // below, which maps to `Failed`. Nothing to propagate from the task.
        drop(stdin.write_all(&input).await);
    });

    let output = match tokio::time::timeout(TRANSCODE_TIMEOUT, child.wait_with_output()).await {
        Ok(Ok(output)) => output,
        Ok(Err(err)) => {
            writer.abort();
            return Err(TranscodeError::Io(err));
        }
        Err(_elapsed) => {
            // The dropped wait future drops the child; kill_on_drop reaps it.
            writer.abort();
            return Err(TranscodeError::Timeout);
        }
    };
    // The writer only ever returns the unit value; joining it just reaps the
    // task. A join error (writer panic) is irrelevant to the transcode result.
    drop(writer.await);

    if !output.status.success() || output.stdout.is_empty() {
        return Err(TranscodeError::Failed);
    }
    Ok(output.stdout)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passthrough_mimes_are_recognized() {
        assert!(is_passthrough("audio/wav"));
        assert!(is_passthrough("audio/x-wav"));
        assert!(is_passthrough("audio/mpeg"));
        assert!(is_passthrough("audio/mp3"));
        assert!(is_passthrough("AUDIO/WAV"), "match is case-insensitive");
    }

    #[test]
    fn non_chat_mimes_are_not_passthrough() {
        assert!(!is_passthrough("audio/ogg"));
        assert!(!is_passthrough("audio/opus"));
        assert!(!is_passthrough("audio/mp4"));
        assert!(!is_passthrough("audio/webm"));
        assert!(!is_passthrough("audio/flac"));
    }

    #[tokio::test]
    async fn wav_clip_passes_through_unchanged() {
        let bytes: Arc<[u8]> = Arc::from(b"RIFFfake-wav".as_slice());
        let (out, mime) = ensure_chat_audio(Arc::clone(&bytes), "audio/wav".to_owned())
            .await
            .expect("passthrough never errors");
        assert_eq!(out.as_ref(), bytes.as_ref(), "bytes are unchanged");
        assert_eq!(mime, "audio/wav", "mime is unchanged");
    }

    #[tokio::test]
    async fn mp3_clip_passes_through_unchanged() {
        let bytes: Arc<[u8]> = Arc::from(b"ID3fake-mp3".as_slice());
        let (out, mime) = ensure_chat_audio(Arc::clone(&bytes), "audio/mpeg".to_owned())
            .await
            .expect("passthrough never errors");
        assert_eq!(out.as_ref(), bytes.as_ref());
        assert_eq!(mime, "audio/mpeg");
    }

    #[tokio::test]
    async fn garbage_opus_clip_is_a_soft_error() {
        // Non-audio bytes declared as opus: ffmpeg (if present) exits non-zero
        // and we map that to `Failed`; if ffmpeg is absent the spawn maps to
        // `NotAvailable`. Either way the transcode is a clean `Err`, never a
        // panic and never a bogus success.
        let bytes: Arc<[u8]> = Arc::from(b"not actually opus".as_slice());
        let result = ensure_chat_audio(bytes, "audio/opus".to_owned()).await;
        assert!(result.is_err(), "garbage input cannot transcode");
    }
}
