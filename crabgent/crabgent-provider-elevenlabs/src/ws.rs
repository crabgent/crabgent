//! `ElevenLabs` realtime speech-to-text transport.

use std::sync::Arc;

use async_trait::async_trait;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use crabgent_core::{SttError, SttEvent, SttEventStream, SttModelId, SttRequest, SttResponse};
use futures::{SinkExt, StreamExt, stream};
use secrecy::ExposeSecret;
use serde_json::json;
use tokio::net::TcpStream;
use tokio::sync::Mutex;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::{HeaderName, HeaderValue};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};

use crate::config::ElevenLabsConfig;
use crate::events::parse_elevenlabs_stt_event;

const REALTIME_ENDPOINT: &str = "/v1/speech-to-text/realtime";
const AUDIO_CHUNK_BYTES: usize = 64 * 1024;
const REALTIME_SAMPLE_RATE_HZ: u32 = 16_000;

type WsStream = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// WebSocket transport contract for `ElevenLabs` realtime STT.
#[async_trait]
pub trait SttWsClient: Send + Sync {
    /// Return a client instance isolated to one streaming request.
    fn session_client(&self) -> Arc<dyn SttWsClient>;
    async fn connect(&self, url: &str, headers: Vec<(String, String)>) -> Result<(), SttError>;
    async fn send(&self, message: Message) -> Result<(), SttError>;
    async fn next(&self) -> Result<Option<Message>, SttError>;
    async fn close(&self) -> Result<(), SttError>;
}

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
            crabgent_log::warn!(error = %err, "elevenlabs realtime request build failed");
            SttError::Network
        })?;
        for (name, value) in headers {
            let header_name: HeaderName = name.parse().map_err(|err| {
                crabgent_log::warn!(error = %err, "elevenlabs realtime header name rejected");
                SttError::Network
            })?;
            let header_value: HeaderValue = value.parse().map_err(|err| {
                crabgent_log::warn!(error = %err, "elevenlabs realtime header value rejected");
                SttError::Network
            })?;
            request.headers_mut().insert(header_name, header_value);
        }
        let (stream, _) = connect_async(request).await.map_err(|err| {
            crabgent_log::warn!(error = %err, "elevenlabs realtime websocket connect failed");
            SttError::Network
        })?;
        *self.stream.lock().await = Some(stream);
        Ok(())
    }

    async fn send(&self, message: Message) -> Result<(), SttError> {
        let mut guard = self.stream.lock().await;
        let stream = guard.as_mut().ok_or(SttError::Network)?;
        stream.send(message).await.map_err(|err| {
            crabgent_log::warn!(error = %err, "elevenlabs realtime websocket send failed");
            SttError::Network
        })
    }

    async fn next(&self) -> Result<Option<Message>, SttError> {
        let mut guard = self.stream.lock().await;
        let stream = guard.as_mut().ok_or(SttError::Network)?;
        stream.next().await.transpose().map_err(|err| {
            crabgent_log::warn!(error = %err, "elevenlabs realtime websocket receive failed");
            SttError::Network
        })
    }

    async fn close(&self) -> Result<(), SttError> {
        let mut guard = self.stream.lock().await;
        let Some(stream) = guard.as_mut() else {
            return Ok(());
        };
        stream.send(Message::Close(None)).await.map_err(|err| {
            crabgent_log::warn!(error = %err, "elevenlabs realtime websocket close failed");
            SttError::Network
        })
    }
}

#[crabgent_log::instrument(level = "debug", skip(ws_client, config, req))]
pub async fn stream_realtime(
    ws_client: Arc<dyn SttWsClient>,
    config: &ElevenLabsConfig,
    req: SttRequest,
) -> Result<SttEventStream, SttError> {
    let ws_client = ws_client.session_client();
    let model = req.model.clone();
    let url = realtime_url(config.api_base(), &model);
    ws_client
        .connect(&url, auth_headers(config.api_key.expose_secret()))
        .await?;
    send_audio(&ws_client, &req).await?;

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

async fn send_audio(ws_client: &Arc<dyn SttWsClient>, req: &SttRequest) -> Result<(), SttError> {
    let chunks: Vec<&[u8]> = req.payload.bytes().chunks(AUDIO_CHUNK_BYTES).collect();
    if chunks.is_empty() {
        return send_audio_chunk(ws_client, "", true).await;
    }

    for (index, chunk) in chunks.iter().enumerate() {
        let audio = BASE64_STANDARD.encode(chunk);
        send_audio_chunk(ws_client, &audio, index + 1 == chunks.len()).await?;
    }
    Ok(())
}

async fn send_audio_chunk(
    ws_client: &Arc<dyn SttWsClient>,
    audio: &str,
    commit: bool,
) -> Result<(), SttError> {
    let event = json!({
        "message_type": "input_audio_chunk",
        "audio_base_64": audio,
        "sample_rate": REALTIME_SAMPLE_RATE_HZ,
        "commit": commit,
    });
    ws_client
        .send(Message::Text(event.to_string().into()))
        .await
}

fn auth_headers(api_key: &str) -> Vec<(String, String)> {
    vec![("xi-api-key".to_owned(), api_key.to_owned())]
}

fn realtime_url(base_url: &str, model: &SttModelId) -> String {
    let base = base_url.trim_end_matches('/');
    let websocket_base = if let Some(rest) = base.strip_prefix("https://") {
        format!("wss://{rest}")
    } else if let Some(rest) = base.strip_prefix("http://") {
        format!("ws://{rest}")
    } else {
        base.to_owned()
    };
    format!(
        "{websocket_base}{REALTIME_ENDPOINT}?model_id={}&include_timestamps=true",
        model.as_str()
    )
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
        let Some(event) = parse_elevenlabs_stt_event(&msg) else {
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
                    crabgent_log::debug!(error = %err, "elevenlabs realtime close after final failed");
                }
                SttEvent::Final(response)
            }
            other => other,
        }
    }
}
