//! `ElevenLabs` batch speech-to-text transport.

use std::time::Duration;

use bytes::Bytes;
use crabgent_core::{SttError, SttRequest, SttResponse};
use crabgent_log::warn;
use crabgent_provider_transport::read_text_body;
use reqwest::multipart::{Form, Part};
use secrecy::ExposeSecret;
use serde::Deserialize;

use crate::config::ElevenLabsConfig;
use crate::words::{self, RawWord, build_segments};

const BATCH_ENDPOINT: &str = "/v1/speech-to-text";
const MAX_STT_ERROR_BODY_BYTES: usize = 64 * 1024;
const STT_ERROR_BODY_TIMEOUT: Duration = Duration::from_secs(30);

pub async fn transcribe_batch(
    client: &reqwest::Client,
    config: &ElevenLabsConfig,
    req: SttRequest,
) -> Result<SttResponse, SttError> {
    let model = req.model.clone();
    let form = build_form(&req)?;
    let endpoint = endpoint_url(config.api_base());
    let response = client
        .post(&endpoint)
        .header("xi-api-key", config.api_key.expose_secret())
        .multipart(form)
        .send()
        .await
        .map_err(|error| {
            warn!(%endpoint, %error, "elevenlabs speech-to-text send failed");
            SttError::Network
        })?;

    let status = response.status();
    if status.is_success() {
        let raw = response
            .json::<RawTranscriptionResponse>()
            .await
            .map_err(|err| {
                warn!(error = %err, "elevenlabs speech-to-text decode failed");
                SttError::Decode
            })?;
        let (words, audio_events) = words::parse_words(&raw.words);
        let segments = build_segments(&raw.text, words);
        return Ok(SttResponse {
            text: raw.text,
            model,
            segments,
            audio_events,
            language: raw.language_code,
        });
    }

    if matches!(status.as_u16(), 401 | 403) {
        return Err(SttError::Auth(
            "elevenlabs authentication failed".to_owned(),
        ));
    }

    let body_len = read_text_body(
        response,
        None,
        STT_ERROR_BODY_TIMEOUT,
        MAX_STT_ERROR_BODY_BYTES,
    )
    .await
    .map(|body| body.len())
    .unwrap_or_default();
    warn!(status = %status, body_len, "elevenlabs speech-to-text request failed");

    Err(SttError::Backend(
        "elevenlabs speech-to-text request failed".to_owned(),
    ))
}

fn build_form(req: &SttRequest) -> Result<Form, SttError> {
    let filename = req
        .payload
        .filename
        .clone()
        .unwrap_or_else(|| "audio".to_owned());
    let bytes = Bytes::copy_from_slice(req.payload.bytes().as_ref());
    let file_part = Part::stream(reqwest::Body::from(bytes))
        .file_name(filename)
        .mime_str(req.payload.mime())
        .map_err(|err| {
            warn!(error = %err, "elevenlabs speech-to-text multipart mime rejected");
            SttError::Decode
        })?;

    let mut form = Form::new()
        .part("file", file_part)
        .text("model_id", req.model.as_str().to_owned())
        .text("timestamps_granularity", "word")
        .text("tag_audio_events", "true")
        .text("diarize", "true");
    if let Some(language) = &req.language {
        form = form.text("language_code", language.clone());
    }
    Ok(form)
}

fn endpoint_url(base_url: &str) -> String {
    format!("{}{}", base_url.trim_end_matches('/'), BATCH_ENDPOINT)
}

#[derive(Deserialize)]
struct RawTranscriptionResponse {
    text: String,
    #[serde(default)]
    language_code: Option<String>,
    #[serde(default)]
    words: Vec<RawWord>,
}
