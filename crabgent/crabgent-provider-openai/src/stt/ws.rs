//! `OpenAI` realtime transcription transport.

use std::sync::Arc;

use async_trait::async_trait;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use crabgent_core::{SttError, SttEvent, SttEventStream, SttModelId, SttRequest, SttResponse};
use futures::{SinkExt, StreamExt, stream};
use serde_json::json;
use tokio::net::TcpStream;
use tokio::sync::Mutex;
use tokio_tungstenite::tungstenite::Error as WsError;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::{HeaderName, HeaderValue, StatusCode};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};

use crate::auth::AuthStrategy;
use crate::stt::events::parse_openai_stt_event;
use crate::stt::{REALTIME_ENDPOINT, SttWsClient};

const AUDIO_CHUNK_BYTES: usize = 64 * 1024;
const REALTIME_SAMPLE_RATE_HZ: u32 = 24_000;
const REALTIME_CHANNELS: u16 = 1;
const REALTIME_BITS_PER_SAMPLE: u16 = 16;
const WAV_HEADER_BYTES: usize = 12;
const WAV_CHUNK_HEADER_BYTES: usize = 8;

type WsStream = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// Production realtime WebSocket client backed by `tokio-tungstenite`.
#[derive(Default)]
pub struct TungsteniteWsClient {
    stream: Mutex<Option<WsStream>>,
}

impl TungsteniteWsClient {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            stream: Mutex::const_new(None),
        }
    }
}

#[async_trait]
impl SttWsClient for TungsteniteWsClient {
    fn session_client(&self) -> Arc<dyn SttWsClient> {
        Arc::new(Self::new())
    }

    #[crabgent_log::instrument(level = "debug", skip(self, headers))]
    async fn connect(&self, url: &str, headers: Vec<(String, String)>) -> Result<(), SttError> {
        let mut request = url.into_client_request().map_err(|err| {
            crabgent_log::warn!(error = %err, "openai realtime request build failed");
            SttError::Network
        })?;
        for (name, value) in headers {
            let header_name: HeaderName = name.parse().map_err(|err| {
                crabgent_log::warn!(error = %err, "openai realtime header name rejected");
                SttError::Network
            })?;
            let header_value: HeaderValue = value.parse().map_err(|err| {
                crabgent_log::warn!(error = %err, "openai realtime header value rejected");
                SttError::Network
            })?;
            request.headers_mut().insert(header_name, header_value);
        }
        let (stream, _) = connect_async(request)
            .await
            .map_err(|err| map_ws_connect_error(&err))?;
        *self.stream.lock().await = Some(stream);
        Ok(())
    }

    async fn send(&self, message: Message) -> Result<(), SttError> {
        let mut guard = self.stream.lock().await;
        let stream = guard.as_mut().ok_or(SttError::Network)?;
        stream.send(message).await.map_err(|err| {
            crabgent_log::warn!(error = %err, "openai realtime websocket send failed");
            SttError::Network
        })
    }

    async fn next(&self) -> Result<Option<Message>, SttError> {
        let mut guard = self.stream.lock().await;
        let stream = guard.as_mut().ok_or(SttError::Network)?;
        stream.next().await.transpose().map_err(|err| {
            crabgent_log::warn!(error = %err, "openai realtime websocket receive failed");
            SttError::Network
        })
    }

    async fn close(&self) -> Result<(), SttError> {
        let mut guard = self.stream.lock().await;
        let Some(stream) = guard.as_mut() else {
            return Ok(());
        };
        stream.send(Message::Close(None)).await.map_err(|err| {
            crabgent_log::warn!(error = %err, "openai realtime websocket close failed");
            SttError::Network
        })
    }
}

#[crabgent_log::instrument(level = "debug", skip(ws_client, auth, req))]
pub(super) async fn stream_realtime(
    ws_client: Arc<dyn SttWsClient>,
    auth: &dyn AuthStrategy,
    req: SttRequest,
) -> Result<SttEventStream, SttError> {
    let model = req.model.clone();
    let audio = realtime_audio_bytes(&req)?;
    let ws_client = ws_client.session_client();
    let url = realtime_url(auth.base_url());
    ws_client.connect(&url, auth_headers(auth)?).await?;
    ws_client
        .send(Message::Text(session_update(&model).to_string().into()))
        .await?;
    send_audio(&ws_client, &audio).await?;

    Ok(Box::pin(stream::unfold(
        RealtimeStreamState {
            ws_client,
            model,
            accumulated_text: String::new(),
            closed: false,
        },
        advance_stream,
    )))
}

async fn send_audio(ws_client: &Arc<dyn SttWsClient>, audio: &[u8]) -> Result<(), SttError> {
    for chunk in audio.chunks(AUDIO_CHUNK_BYTES) {
        let audio = BASE64_STANDARD.encode(chunk);
        let event = json!({
            "type": "input_audio_buffer.append",
            "audio": audio,
        });
        ws_client
            .send(Message::Text(event.to_string().into()))
            .await?;
    }

    ws_client
        .send(Message::Text(
            json!({"type": "input_audio_buffer.commit"})
                .to_string()
                .into(),
        ))
        .await
}

fn realtime_audio_bytes(req: &SttRequest) -> Result<Vec<u8>, SttError> {
    if req.payload.mime() != "audio/wav" {
        crabgent_log::warn!(
            mime = %req.payload.mime(),
            "openai realtime received unsupported audio container"
        );
        return Err(SttError::Backend(
            "openai realtime transcription requires 24 kHz mono pcm16 WAV input".to_owned(),
        ));
    }
    wav_pcm16_24khz_mono_data(req.payload.bytes().as_ref())
}

fn wav_pcm16_24khz_mono_data(bytes: &[u8]) -> Result<Vec<u8>, SttError> {
    if read_tag(bytes, 0)? != b"RIFF"
        || read_tag(bytes, 8)? != b"WAVE"
        || bytes.len() < WAV_HEADER_BYTES
    {
        return Err(SttError::Decode);
    }

    let mut fmt = None;
    let mut data = None;
    let mut offset = WAV_HEADER_BYTES;
    while offset + WAV_CHUNK_HEADER_BYTES <= bytes.len() {
        let tag = read_tag(bytes, offset)?;
        let chunk_len =
            usize::try_from(read_u32_le(bytes, offset + 4)?).map_err(|_err| SttError::Decode)?;
        let chunk_start = offset + WAV_CHUNK_HEADER_BYTES;
        let chunk_end = chunk_start.checked_add(chunk_len).ok_or(SttError::Decode)?;
        let chunk = bytes.get(chunk_start..chunk_end).ok_or(SttError::Decode)?;
        match tag {
            b"fmt " => fmt = Some(parse_wav_fmt(chunk)?),
            b"data" => data = Some(chunk.to_vec()),
            _ => {}
        }
        offset = chunk_end + (chunk_len % 2);
    }

    validate_wav_fmt(fmt.ok_or(SttError::Decode)?)?;
    data.ok_or(SttError::Decode)
}

#[derive(Clone, Copy)]
struct WavFmt {
    audio_format: u16,
    channels: u16,
    sample_rate: u32,
    bits_per_sample: u16,
}

fn parse_wav_fmt(chunk: &[u8]) -> Result<WavFmt, SttError> {
    if chunk.len() < 16 {
        return Err(SttError::Decode);
    }
    Ok(WavFmt {
        audio_format: read_u16_le(chunk, 0)?,
        channels: read_u16_le(chunk, 2)?,
        sample_rate: read_u32_le(chunk, 4)?,
        bits_per_sample: read_u16_le(chunk, 14)?,
    })
}

fn validate_wav_fmt(fmt: WavFmt) -> Result<(), SttError> {
    if fmt.audio_format == 1
        && fmt.channels == REALTIME_CHANNELS
        && fmt.sample_rate == REALTIME_SAMPLE_RATE_HZ
        && fmt.bits_per_sample == REALTIME_BITS_PER_SAMPLE
    {
        Ok(())
    } else {
        Err(SttError::Backend(
            "openai realtime transcription requires 24 kHz mono pcm16 WAV input".to_owned(),
        ))
    }
}

fn read_tag(bytes: &[u8], offset: usize) -> Result<&[u8; 4], SttError> {
    let end = offset.checked_add(4).ok_or(SttError::Decode)?;
    bytes
        .get(offset..end)
        .and_then(|chunk| chunk.try_into().ok())
        .ok_or(SttError::Decode)
}

fn read_u16_le(bytes: &[u8], offset: usize) -> Result<u16, SttError> {
    let end = offset.checked_add(2).ok_or(SttError::Decode)?;
    let raw: [u8; 2] = bytes
        .get(offset..end)
        .and_then(|chunk| chunk.try_into().ok())
        .ok_or(SttError::Decode)?;
    Ok(u16::from_le_bytes(raw))
}

fn read_u32_le(bytes: &[u8], offset: usize) -> Result<u32, SttError> {
    let end = offset.checked_add(4).ok_or(SttError::Decode)?;
    let raw: [u8; 4] = bytes
        .get(offset..end)
        .and_then(|chunk| chunk.try_into().ok())
        .ok_or(SttError::Decode)?;
    Ok(u32::from_le_bytes(raw))
}

fn session_update(model: &SttModelId) -> serde_json::Value {
    json!({
        "type": "session.update",
        "session": {
            "modalities": ["audio"],
            "input_audio_format": "pcm16",
            "input_audio_sample_rate_hz": REALTIME_SAMPLE_RATE_HZ,
            "input_audio_transcription": {
                "model": model.as_str(),
            },
            "turn_detection": null,
        },
    })
}

fn auth_headers(auth: &dyn AuthStrategy) -> Result<Vec<(String, String)>, SttError> {
    auth.auth_headers()
        .into_iter()
        .map(|(name, value)| {
            let value = value.to_str().map_err(|err| {
                crabgent_log::warn!(error = %err, "openai stt auth header is not valid text");
                SttError::Auth(auth_failed())
            })?;
            Ok((name.as_str().to_owned(), value.to_owned()))
        })
        .collect()
}

fn auth_failed() -> String {
    "openai stt authentication failed".to_owned()
}

fn map_ws_connect_error(error: &WsError) -> SttError {
    if let Some(status) = ws_http_status(error) {
        return map_ws_http_status(status);
    }
    crabgent_log::warn!(error = %error, "openai realtime websocket connect failed");
    SttError::Network
}

fn ws_http_status(error: &WsError) -> Option<StatusCode> {
    match error {
        WsError::Http(response) => Some(response.status()),
        _ => None,
    }
}

fn map_ws_http_status(status: StatusCode) -> SttError {
    match status.as_u16() {
        401 | 403 => map_ws_auth_status(status),
        _ => map_ws_non_auth_status(status),
    }
}

fn map_ws_auth_status(status: StatusCode) -> SttError {
    crabgent_log::warn!(status = %status, "openai realtime websocket authentication failed");
    SttError::Auth(auth_failed())
}

fn map_ws_non_auth_status(status: StatusCode) -> SttError {
    crabgent_log::warn!(status = %status, "openai realtime websocket handshake failed");
    SttError::Network
}

fn realtime_url(base_url: &str) -> String {
    let base = base_url.trim_end_matches('/');
    let websocket_base = if let Some(rest) = base.strip_prefix("https://") {
        format!("wss://{rest}")
    } else if let Some(rest) = base.strip_prefix("http://") {
        format!("ws://{rest}")
    } else {
        base.to_owned()
    };
    format!("{websocket_base}{REALTIME_ENDPOINT}")
}

struct RealtimeStreamState {
    ws_client: Arc<dyn SttWsClient>,
    model: SttModelId,
    accumulated_text: String,
    closed: bool,
}

async fn advance_stream(
    mut state: RealtimeStreamState,
) -> Option<(Result<SttEvent, SttError>, RealtimeStreamState)> {
    if state.closed {
        return None;
    }

    loop {
        let msg = match state.ws_client.next().await {
            Ok(Some(msg)) => msg,
            Ok(None) => return None,
            Err(error) => {
                state.closed = true;
                return Some((Err(error), state));
            }
        };
        let Some(event) = parse_openai_stt_event(&msg) else {
            continue;
        };
        let event = state.apply_event(event).await;
        return Some((Ok(event), state));
    }
}

impl RealtimeStreamState {
    async fn apply_event(&mut self, event: SttEvent) -> SttEvent {
        match event {
            SttEvent::Delta(delta) => {
                self.accumulated_text.push_str(&delta);
                SttEvent::Delta(delta)
            }
            SttEvent::Final(mut response) => {
                if response.text.is_empty() {
                    response = SttResponse {
                        text: self.accumulated_text.clone(),
                        model: self.model.clone(),
                        segments: response.segments,
                        audio_events: response.audio_events,
                        language: response.language,
                    };
                }
                self.closed = true;
                if let Err(err) = self.ws_client.close().await {
                    crabgent_log::debug!(error = %err, "openai realtime close after final failed");
                }
                SttEvent::Final(response)
            }
            other => other,
        }
    }
}
