//! `OpenAI` batch transcription transport.

use std::time::Duration;

use bytes::Bytes;
use crabgent_core::{SttError, SttRequest, SttResponse};
use crabgent_log::{debug, warn};
use crabgent_provider_transport::read_text_body;
use reqwest::multipart::{Form, Part};
use serde::Deserialize;

use crate::auth::AuthStrategy;

const MAX_STT_ERROR_BODY_BYTES: usize = 64 * 1024;
const STT_ERROR_BODY_TIMEOUT: Duration = Duration::from_secs(30);

pub(super) async fn transcribe_batch(
    client: &reqwest::Client,
    auth: &dyn AuthStrategy,
    _base_url: &str,
    req: SttRequest,
) -> Result<SttResponse, SttError> {
    let model = req.model.clone();
    let accepts_model_field = auth.stt_accepts_model_field();
    let form = build_form(&req, accepts_model_field)?;
    let response =
        send_transcription_request(client, auth, form, &req, accepts_model_field).await?;
    handle_transcription_response(response, model).await
}

async fn send_transcription_request(
    client: &reqwest::Client,
    auth: &dyn AuthStrategy,
    form: Form,
    req: &SttRequest,
    accepts_model_field: bool,
) -> Result<reqwest::Response, SttError> {
    let url = auth.stt_endpoint_url();
    debug!(url = %url, accepts_model_field, mime = %req.payload.mime(), bytes = req.payload.bytes().len(), "openai stt request preparing");
    let builder = authenticated_builder(client.post(&url).multipart(form), auth);
    builder.send().await.map_err(|err| {
        warn!(error = %err, "openai stt network error");
        SttError::Network
    })
}

fn authenticated_builder(
    mut builder: reqwest::RequestBuilder,
    auth: &dyn AuthStrategy,
) -> reqwest::RequestBuilder {
    for (name, value) in auth.auth_headers() {
        builder = builder.header(name, value);
    }
    builder
}

async fn handle_transcription_response(
    response: reqwest::Response,
    model: crabgent_core::SttModelId,
) -> Result<SttResponse, SttError> {
    let status = response.status();
    if status.is_success() {
        return parse_success_response(response, model).await;
    }
    handle_error_response(response, status).await
}

async fn parse_success_response(
    response: reqwest::Response,
    model: crabgent_core::SttModelId,
) -> Result<SttResponse, SttError> {
    let raw = response
        .json::<RawTranscriptionResponse>()
        .await
        .map_err(|err| {
            warn!(error = %err, "openai stt decode failed");
            SttError::Decode
        })?;
    Ok(SttResponse {
        text: raw.text,
        model,
        segments: Vec::new(),
        audio_events: Vec::new(),
        language: None,
    })
}

async fn handle_error_response(
    response: reqwest::Response,
    status: reqwest::StatusCode,
) -> Result<SttResponse, SttError> {
    if is_auth_status(status) {
        return Err(map_auth_error(status));
    }
    let body = read_stt_error_body(response).await;
    warn!(status = %status, body_len = body.len(), "openai stt request failed");
    Err(map_status_error(status))
}

async fn read_stt_error_body(response: reqwest::Response) -> String {
    let body = read_text_body(
        response,
        None,
        STT_ERROR_BODY_TIMEOUT,
        MAX_STT_ERROR_BODY_BYTES,
    )
    .await;
    match body {
        Ok(body) => body,
        Err(error) => {
            warn!(error = %error, "openai stt error body read failed");
            String::new()
        }
    }
}

const fn is_auth_status(status: reqwest::StatusCode) -> bool {
    matches!(status.as_u16(), 401 | 403)
}

fn map_auth_error(status: reqwest::StatusCode) -> SttError {
    warn!(status = %status, "openai stt authentication failed");
    SttError::Auth("openai stt authentication failed".to_owned())
}

fn map_status_error(status: reqwest::StatusCode) -> SttError {
    SttError::Backend(format!("openai stt request failed: status={status}"))
}

fn build_form(req: &SttRequest, accepts_model_field: bool) -> Result<Form, SttError> {
    // Upstream channels may set `filename` to a free-form caption
    // (Matrix `m.audio` body, etc). The Codex transcribe endpoint
    // 500s on filenames that contain colons or non-ASCII, so always
    // synthesise a safe `audio.<ext>` from the declared mime instead
    // of trusting whatever the channel passed in.
    let filename = format!("audio.{}", mime_extension(req.payload.mime()));
    let bytes = Bytes::copy_from_slice(req.payload.bytes().as_ref());
    let file_part = Part::stream(reqwest::Body::from(bytes))
        .file_name(filename)
        .mime_str(req.payload.mime())
        .map_err(|err| {
            warn!(error = %err, "openai stt multipart mime rejected");
            SttError::Decode
        })?;

    let mut form = Form::new().part("file", file_part);
    form = form.text("response_format", "json");
    if accepts_model_field {
        form = form.text("model", req.model.as_str().to_owned());
    }
    if let Some(language) = &req.language {
        form = form.text("language", language.clone());
    }
    Ok(form)
}

fn mime_extension(mime: &str) -> &str {
    match mime {
        "audio/ogg" | "audio/opus" => "ogg",
        "audio/mpeg" | "audio/mp3" => "mp3",
        "audio/wav" | "audio/x-wav" | "audio/wave" => "wav",
        "audio/flac" | "audio/x-flac" => "flac",
        "audio/mp4" | "audio/aac" | "audio/x-m4a" => "m4a",
        "audio/webm" => "webm",
        _ => "bin",
    }
}

#[derive(Deserialize)]
struct RawTranscriptionResponse {
    text: String,
}
