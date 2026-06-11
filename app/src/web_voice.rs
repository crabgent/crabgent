//! Browser voice-to-voice dashboard.
//!
//! The page lives under `/admin/voice` and uses the existing admin cookie.
//! It drives a live agent kernel over WebSocket, then synthesizes the final
//! assistant text server-side with the configured TTS route.

use std::collections::HashMap;
use std::sync::Arc;

use axum::{
    Router,
    extract::{
        Path, State,
        ws::{Message as WsMessage, Utf8Bytes, WebSocket, WebSocketUpgrade},
    },
    http::{HeaderMap, StatusCode, header},
    response::{Html, IntoResponse, Redirect, Response},
    routing::get,
};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use crabgent_channel::SpeakerIdentificationRequest;
use crabgent_core::message::ContentBlock;
use crabgent_core::{
    AudioPayload, Event, ForcedAlignmentRequest, Message, ModelId, ModelTarget, ReasoningEffort,
    RunId, RunRequest, SpeakerIdentity, SttRequest, SttResponse, Subject, TtsRequest, VoiceSignals,
    WebSearchConfig,
};
use crabgent_log::{info, warn};
use futures::{SinkExt, StreamExt};
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::agent::{AgentVoiceStt, AgentVoiceTts};
use crate::agent_message::ORIGIN_OWNER_ATTR;

const COOKIE_NAME: &str = "crabgent_admin";
const PAGE_HTML: &str = include_str!("../templates/voice.html");

const WEB_VOICE_PROMPT: &str = r#"## Browser voice session
Du bist in einem Live-Gespräch im Browser. Die User-Eingabe kommt aus serverseitigem STT und kann kurze Erkennungsfehler enthalten.

Antworte so, dass es gesprochen natürlich klingt: kurze Sätze, klare Pausenstellen, keine langen Tabellen, keine Markdown-Layouts, außer der User verlangt das explizit. Wenn das STT offensichtlich unsicher ist, frage knapp nach.

Wenn ein `<voice ... identified_speaker="..." speaker_confidence="..."/>` Tag vorhanden ist, ist das eine lokale Sprechererkennungs-Vermutung. Nutze sie für Anrede und Kontext, aber rate bei niedriger Confidence keine Identität.

Die Audioausgabe wird vom Web-Dashboard aus deinem finalen Text erzeugt. Nutze keine channel_send- oder voice_reply-Tools nur für die Audioausgabe dieser Web-Session."#;

#[derive(Clone)]
pub struct WebVoiceAgent {
    pub name: String,
    pub kernel: Arc<crabgent_core::Kernel>,
    pub model: String,
    pub system_prompt: String,
    pub fallbacks: Vec<String>,
    pub max_turns: Option<u32>,
    pub inject_registry: crabgent_hook_inject::InjectionRegistry,
    pub tts: Option<AgentVoiceTts>,
    pub stt: Option<AgentVoiceStt>,
}

struct AgentEntry {
    kernel: Arc<crabgent_core::Kernel>,
    model: String,
    system_prompt: String,
    fallbacks: Vec<ModelTarget>,
    max_turns: Option<u32>,
    inject_registry: crabgent_hook_inject::InjectionRegistry,
    tts: Option<AgentVoiceTts>,
    stt: Option<AgentVoiceStt>,
}

#[derive(Clone)]
struct VoiceState {
    agents: Arc<HashMap<String, Arc<AgentEntry>>>,
    expected_hash: Arc<String>,
}

type WsSink = futures::stream::SplitSink<WebSocket, WsMessage>;
type WsStream = futures::stream::SplitStream<WebSocket>;

#[derive(Debug, Deserialize)]
struct ClientFrame {
    #[serde(default)]
    prompt: Option<String>,
    #[serde(default)]
    audio: Option<ClientAudioFrame>,
    #[serde(default)]
    stop: bool,
    #[serde(default)]
    metrics: Option<VoiceMetrics>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    effort: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ClientAudioFrame {
    mime: String,
    data: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct VoiceMetrics {
    #[serde(default)]
    speech_ms: Option<u32>,
    #[serde(default)]
    pause_ms: Option<u32>,
    #[serde(default)]
    pause_count: Option<u32>,
    #[serde(default)]
    words: Option<u32>,
    #[serde(default)]
    rate: Option<u32>,
    #[serde(default)]
    energy: Option<String>,
    #[serde(default)]
    source: Option<String>,
    #[serde(default)]
    claimed_speaker: Option<String>,
}

enum ClientInput {
    Prompt {
        prompt: String,
        metrics: Option<VoiceMetrics>,
        options: TurnOptions,
    },
    Audio {
        audio: ClientAudioFrame,
        metrics: Option<VoiceMetrics>,
        options: TurnOptions,
    },
    Stop,
    Ignore,
    Closed,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct TurnOptions {
    model: Option<String>,
    effort: Option<ReasoningEffort>,
}

#[derive(Serialize)]
struct AudioFrame {
    mime: String,
    data: String,
    text: String,
    alignment: Value,
}

#[derive(Serialize)]
struct AgentListFrame {
    agents: Vec<AgentInfo>,
}

#[derive(Serialize)]
struct AgentInfo {
    name: String,
    stt: bool,
    tts: bool,
    model: String,
    reasoning_effort: Option<String>,
    models: Vec<ModelChoice>,
}

#[derive(Serialize)]
struct ModelChoice {
    id: String,
    label: String,
    provider: String,
    supports_effort: bool,
}

#[allow(clippy::literal_string_with_formatting_args)]
pub fn build_router(agents: Vec<WebVoiceAgent>, auth_token: &SecretString) -> Router {
    let mut map = HashMap::new();
    for agent in agents {
        let name = agent.name;
        let entry = AgentEntry {
            kernel: agent.kernel,
            model: agent.model,
            system_prompt: agent.system_prompt,
            fallbacks: agent
                .fallbacks
                .iter()
                .map(|m| ModelTarget::id(ModelId::new(m)))
                .collect(),
            max_turns: agent.max_turns,
            inject_registry: agent.inject_registry,
            tts: agent.tts,
            stt: agent.stt,
        };
        info!(agent = %name, "admin-voice: route mounted GET /admin/voice/ws/{}", name);
        map.insert(name, Arc::new(entry));
    }
    let state = VoiceState {
        agents: Arc::new(map),
        expected_hash: Arc::new(sha256_hex(auth_token.expose_secret().as_bytes())),
    };
    Router::new()
        .route("/admin/voice", get(page))
        .route("/admin/api/voice/agents", get(agent_list))
        .route("/admin/voice/ws/{agent}", get(upgrade))
        .with_state(state)
}

async fn page(State(state): State<VoiceState>, headers: HeaderMap) -> Response {
    if !is_authed(&state, &headers) {
        return Redirect::to("/admin/login").into_response();
    }
    Html(PAGE_HTML).into_response()
}

async fn agent_list(State(state): State<VoiceState>, headers: HeaderMap) -> Response {
    if !is_authed(&state, &headers) {
        return unauthorized_json();
    }
    let mut agents: Vec<AgentInfo> = state
        .agents
        .iter()
        .map(|(name, entry)| AgentInfo {
            name: name.clone(),
            stt: entry.stt.is_some(),
            tts: entry.tts.is_some(),
            model: entry.model.clone(),
            reasoning_effort: entry.default_effort().map(str::to_owned),
            models: entry.model_choices(),
        })
        .collect();
    agents.sort_unstable_by(|a, b| a.name.cmp(&b.name));
    axum::Json(AgentListFrame { agents }).into_response()
}

async fn upgrade(
    State(state): State<VoiceState>,
    Path(agent): Path<String>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    if !is_authed(&state, &headers) {
        return (StatusCode::UNAUTHORIZED, "unauthorized").into_response();
    }
    let Some(entry) = state.agents.get(&agent).cloned() else {
        warn!(agent, "admin-voice: unknown agent");
        return (StatusCode::NOT_FOUND, "unknown agent").into_response();
    };
    ws.on_upgrade(move |socket| handle_socket(socket, entry, agent))
}

async fn handle_socket(socket: WebSocket, entry: Arc<AgentEntry>, agent: String) {
    let (mut sink, mut stream) = socket.split();
    info!(agent, "admin-voice: client connected");
    if send_status(&mut sink, &entry).await.is_err() {
        return;
    }

    loop {
        let input = match read_client_input(&mut stream, &agent, false).await {
            ClientInput::Prompt {
                prompt,
                metrics,
                options,
            } => (prompt, metrics, options),
            ClientInput::Audio {
                audio,
                metrics,
                options,
            } => match transcribe_audio(&mut sink, &entry, audio, metrics.as_ref()).await {
                Ok(Some((prompt, metrics))) => (prompt, metrics, options),
                Ok(None) => continue,
                Err(()) => break,
            },
            ClientInput::Stop | ClientInput::Ignore => continue,
            ClientInput::Closed => break,
        };

        let (prompt, metrics, options) = input;
        let (run_id, req) = build_voice_request(&entry, &agent, prompt, metrics.as_ref(), &options);
        let turn = drive_turn(&mut sink, &mut stream, &entry, &agent, &run_id, req).await;
        match turn {
            TurnResult::Final(text) => {
                if send_audio_reply(&mut sink, &mut stream, Arc::clone(&entry), text).await {
                    break;
                }
            }
            TurnResult::Cancelled | TurnResult::Errored => {}
            TurnResult::Closed => {
                let _ = crate::usage_relay::take(&run_id);
                break;
            }
        }
        let _ = crate::usage_relay::take(&run_id);
        if send_status(&mut sink, &entry).await.is_err() {
            break;
        }
    }
    info!(agent, "admin-voice: client disconnected");
}

enum TurnResult {
    Final(String),
    Cancelled,
    Errored,
    Closed,
}

async fn drive_turn(
    sink: &mut WsSink,
    stream: &mut WsStream,
    entry: &AgentEntry,
    agent: &str,
    run_id: &RunId,
    req: RunRequest,
) -> TurnResult {
    let cancel = CancellationToken::new();
    let run = entry.kernel.run_streaming(req, Some(&cancel));
    tokio::pin!(run);
    loop {
        tokio::select! {
            item = run.next() => {
                let Some(item) = item else {
                    return TurnResult::Errored;
                };
                match item {
                    Ok(Event::Final(text)) => {
                        if send_event(sink, &Event::Final(text.clone())).await.is_err() {
                            return TurnResult::Closed;
                        }
                        return TurnResult::Final(text);
                    }
                    Ok(event) => {
                        if send_event(sink, &event).await.is_err() {
                            return TurnResult::Closed;
                        }
                    }
                    Err(err) => {
                        let _ = send_json(sink, "turn_error", json!({"message": err.to_string()})).await;
                        return TurnResult::Errored;
                    }
                }
            }
            input = read_client_input(stream, agent, true) => {
                match input {
                    ClientInput::Prompt { prompt, .. } => {
                        entry.inject_registry.submit_user_text(run_id, prompt).await;
                        if send_json(sink, "notice", json!({"message": "steering injected"})).await.is_err() {
                            cancel.cancel();
                            return TurnResult::Closed;
                        }
                    }
                    ClientInput::Audio { .. } => {
                        if send_json(sink, "notice", json!({"message": "audio ignored during active turn"})).await.is_err() {
                            cancel.cancel();
                            return TurnResult::Closed;
                        }
                    }
                    ClientInput::Stop => {
                        cancel.cancel();
                        let _ = send_json(sink, "cancelled", json!({"message": "stopped"})).await;
                        return TurnResult::Cancelled;
                    }
                    ClientInput::Ignore => {}
                    ClientInput::Closed => {
                        cancel.cancel();
                        return TurnResult::Closed;
                    }
                }
            }
        }
    }
}

async fn send_audio_reply(
    sink: &mut WsSink,
    stream: &mut WsStream,
    entry: Arc<AgentEntry>,
    text: String,
) -> bool {
    let Some(tts) = entry.tts.clone() else {
        let _ = send_json(
            sink,
            "tts_unavailable",
            json!({"message": "agent has no voice.tts route"}),
        )
        .await;
        return false;
    };
    let clean = text.trim().to_owned();
    if clean.is_empty() {
        return false;
    }
    let synth = tokio::spawn(async move { synthesize_reply(tts, clean).await });
    await_synthesis_or_stop(sink, stream, synth).await
}

async fn await_synthesis_or_stop(
    sink: &mut WsSink,
    stream: &mut WsStream,
    synth: JoinHandle<Result<AudioFrame, String>>,
) -> bool {
    tokio::pin!(synth);
    loop {
        tokio::select! {
            result = &mut synth => {
                match result {
                    Ok(Ok(frame)) => {
                        return send_json(sink, "audio", json!(frame)).await.is_err();
                    }
                    Ok(Err(err)) => {
                        let _ = send_json(sink, "tts_error", json!({"message": err})).await;
                        return false;
                    }
                    Err(err) => {
                        let _ = send_json(sink, "tts_error", json!({"message": err.to_string()})).await;
                        return false;
                    }
                }
            }
            input = read_client_input(stream, "voice", true) => {
                match input {
                    ClientInput::Stop => {
                        synth.abort();
                        let _ = send_json(sink, "tts_cancelled", json!({"message": "stopped"})).await;
                        return false;
                    }
                    ClientInput::Closed => {
                        synth.abort();
                        return true;
                    }
                    ClientInput::Prompt { .. } | ClientInput::Audio { .. } | ClientInput::Ignore => {}
                }
            }
        }
    }
}

async fn transcribe_audio(
    sink: &mut WsSink,
    entry: &AgentEntry,
    audio: ClientAudioFrame,
    metrics: Option<&VoiceMetrics>,
) -> Result<Option<(String, Option<VoiceMetrics>)>, ()> {
    let Some(stt) = entry.stt.as_ref() else {
        if send_json(
            sink,
            "stt_unavailable",
            json!({"message": "agent has no STT route"}),
        )
        .await
        .is_err()
        {
            return Err(());
        }
        return Ok(None);
    };
    if send_json(sink, "transcribing", json!({})).await.is_err() {
        return Err(());
    }
    let (text, voice) = match transcribe_with_stt(stt, audio).await {
        Ok(value) => value,
        Err(message) => {
            if send_json(sink, "stt_error", json!({"message": message}))
                .await
                .is_err()
            {
                return Err(());
            }
            return Ok(None);
        }
    };
    let trimmed = text.trim().to_owned();
    if trimmed.is_empty() {
        let _ = send_json(sink, "empty_audio", json!({})).await;
        return Ok(None);
    }
    if send_json(
        sink,
        "transcript",
        json!({
            "text": trimmed,
            "voice": voice,
        }),
    )
    .await
    .is_err()
    {
        return Err(());
    }
    Ok(Some((
        render_prompt(&trimmed, metrics, voice.as_ref()),
        metrics.cloned(),
    )))
}

async fn transcribe_with_stt(
    stt: &AgentVoiceStt,
    audio: ClientAudioFrame,
) -> Result<(String, Option<VoiceSignals>), String> {
    let data = audio.data.trim();
    let encoded = data
        .split_once(',')
        .map_or(data, |(_prefix, body)| body)
        .trim();
    let bytes = BASE64.decode(encoded).map_err(|err| err.to_string())?;
    let mime = audio
        .mime
        .split_once(';')
        .map_or(audio.mime.as_str(), |(head, _)| head)
        .trim()
        .to_owned();
    let payload = AudioPayload::new(bytes, mime, Some("web-voice-input.webm".to_owned()))
        .map_err(|err| err.to_string())?;
    let payload_for_identity = payload.clone();
    let response = stt
        .provider
        .transcribe(SttRequest {
            payload,
            model: stt.model.clone(),
            language: Some("de".to_owned()),
        })
        .await
        .map_err(|err| err.to_string())?;
    let mut voice = crabgent_prosody::voice_signals(&response, &stt.prosody);
    if let Some(identities) =
        identify_web_speaker(stt, payload_for_identity, response.clone()).await
    {
        voice = merge_speaker_identities(voice, identities);
    }
    Ok((response.text, voice))
}

async fn identify_web_speaker(
    stt: &AgentVoiceStt,
    payload: AudioPayload,
    transcription: SttResponse,
) -> Option<Vec<SpeakerIdentity>> {
    let identifier = stt.speaker_identifier.as_ref()?;
    match identifier
        .identify(SpeakerIdentificationRequest {
            payload,
            transcription,
            subject: Subject::new("web_voice"),
        })
        .await
    {
        Ok(identities) => Some(identities),
        Err(error) => {
            warn!(%error, "web voice speaker identification failed");
            None
        }
    }
}

fn merge_speaker_identities(
    mut voice: Option<VoiceSignals>,
    identities: Vec<SpeakerIdentity>,
) -> Option<VoiceSignals> {
    if identities.is_empty() {
        return voice;
    }
    let signals = voice.get_or_insert_with(VoiceSignals::default);
    signals.speaker_identities = identities;
    voice
}

async fn synthesize_reply(tts: AgentVoiceTts, text: String) -> Result<AudioFrame, String> {
    let response = tts
        .provider
        .synthesize(TtsRequest {
            text: text.clone(),
            model: tts.model.clone(),
            voice: tts.voice.clone(),
            format: tts.format,
        })
        .await
        .map_err(|err| err.to_string())?;
    let alignment = if let Some(provider) = tts.alignment {
        let payload = AudioPayload::new(
            response.audio.as_ref().to_vec(),
            response.mime.clone(),
            Some("web-voice-reply".to_owned()),
        )
        .map_err(|err| err.to_string())?;
        provider
            .align(ForcedAlignmentRequest {
                payload,
                text: text.clone(),
            })
            .await
            .map_or_else(
                |err| json!({"ok": false, "error": err.to_string()}),
                |response| crate::voice_output::alignment_summary(&response),
            )
    } else {
        Value::Null
    };
    Ok(AudioFrame {
        mime: response.mime,
        data: BASE64.encode(response.audio.as_ref()),
        text,
        alignment,
    })
}

async fn read_client_input(stream: &mut WsStream, agent: &str, active_turn: bool) -> ClientInput {
    let Some(frame) = stream.next().await else {
        return ClientInput::Closed;
    };
    let text = match frame {
        Ok(WsMessage::Text(t)) => t.to_string(),
        Ok(WsMessage::Close(_)) => return ClientInput::Closed,
        Ok(WsMessage::Ping(_) | WsMessage::Pong(_) | WsMessage::Binary(_)) => {
            return ClientInput::Ignore;
        }
        Err(err) => {
            if active_turn {
                warn!(agent, error = %err, "admin-voice: recv error during active turn");
            } else {
                warn!(agent, error = %err, "admin-voice: recv error");
            }
            return ClientInput::Closed;
        }
    };
    match parse_frame(&text) {
        ParsedFrame::Prompt {
            prompt,
            metrics,
            options,
        } => ClientInput::Prompt {
            prompt,
            metrics,
            options,
        },
        ParsedFrame::Audio {
            audio,
            metrics,
            options,
        } => ClientInput::Audio {
            audio,
            metrics,
            options,
        },
        ParsedFrame::Stop => ClientInput::Stop,
        ParsedFrame::Ignore => ClientInput::Ignore,
    }
}

enum ParsedFrame {
    Prompt {
        prompt: String,
        metrics: Option<VoiceMetrics>,
        options: TurnOptions,
    },
    Audio {
        audio: ClientAudioFrame,
        metrics: Option<VoiceMetrics>,
        options: TurnOptions,
    },
    Stop,
    Ignore,
}

fn parse_frame(text: &str) -> ParsedFrame {
    if let Ok(frame) = serde_json::from_str::<ClientFrame>(text) {
        if frame.stop {
            return ParsedFrame::Stop;
        }
        let options = turn_options(&frame);
        if let Some(audio) = frame.audio {
            if audio.mime.trim().is_empty() || audio.data.trim().is_empty() {
                return ParsedFrame::Ignore;
            }
            return ParsedFrame::Audio {
                audio,
                metrics: frame.metrics,
                options,
            };
        }
        return frame.prompt.map_or(ParsedFrame::Ignore, |prompt| {
            let prompt = prompt.trim().to_owned();
            if prompt.is_empty() {
                ParsedFrame::Ignore
            } else {
                ParsedFrame::Prompt {
                    prompt,
                    metrics: frame.metrics,
                    options,
                }
            }
        });
    }
    let prompt = text.trim().to_owned();
    if prompt.is_empty() {
        ParsedFrame::Ignore
    } else {
        ParsedFrame::Prompt {
            prompt,
            metrics: None,
            options: TurnOptions::default(),
        }
    }
}

fn turn_options(frame: &ClientFrame) -> TurnOptions {
    TurnOptions {
        model: frame
            .model
            .as_deref()
            .map(str::trim)
            .filter(|model| !model.is_empty())
            .map(str::to_owned),
        effort: frame.effort.as_deref().and_then(parse_voice_effort),
    }
}

fn parse_voice_effort(raw: &str) -> Option<ReasoningEffort> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "low" => Some(ReasoningEffort::Low),
        "medium" => Some(ReasoningEffort::Medium),
        "high" => Some(ReasoningEffort::High),
        _ => None,
    }
}

fn build_voice_request(
    entry: &AgentEntry,
    agent: &str,
    prompt: String,
    metrics: Option<&VoiceMetrics>,
    options: &TurnOptions,
) -> (RunId, RunRequest) {
    build_voice_request_with(
        VoiceRequestConfig {
            agent,
            default_model: &entry.model,
            fallbacks: &entry.fallbacks,
            max_turns: entry.max_turns,
            system_prompt: &entry.system_prompt,
        },
        prompt,
        metrics,
        options,
    )
}

#[derive(Clone, Copy)]
struct VoiceRequestConfig<'a> {
    agent: &'a str,
    default_model: &'a str,
    fallbacks: &'a [ModelTarget],
    max_turns: Option<u32>,
    system_prompt: &'a str,
}

fn build_voice_request_with(
    config: VoiceRequestConfig<'_>,
    prompt: String,
    metrics: Option<&VoiceMetrics>,
    options: &TurnOptions,
) -> (RunId, RunRequest) {
    let text = if prompt.trim_start().starts_with(r#"<voice crabgent="1""#) {
        prompt
    } else {
        render_prompt(&prompt, metrics, None)
    };
    let messages = vec![Message::user(vec![ContentBlock::Text { text }])];
    let run_id = RunId::new();
    let explicit_model = options
        .model
        .as_deref()
        .map(|model| ModelTarget::id(ModelId::new(model)));
    let req = RunRequest {
        run_id: run_id.clone(),
        subject: voice_subject(config.agent),
        model: ModelTarget::id(ModelId::new(config.default_model)),
        explicit_model,
        session_model_override: None,
        fallbacks: config.fallbacks.to_vec(),
        messages,
        system_prompt: Some(format!("{}\n\n{WEB_VOICE_PROMPT}", config.system_prompt)),
        max_turns: config.max_turns,
        temperature: None,
        max_tokens: None,
        cancel_reason: None,
        pause: None,
        reasoning_effort: options.effort,
        web_search: WebSearchConfig::default(),
    };
    (run_id, req)
}

fn voice_subject(agent: &str) -> Subject {
    Subject::new(format!("web_voice:{agent}"))
        .with_attr("agent", agent)
        .with_attr(ORIGIN_OWNER_ATTR, format!("tui:{agent}"))
}

impl AgentEntry {
    fn default_effort(&self) -> Option<&'static str> {
        self.kernel
            .models()
            .get(&ModelId::new(&self.model))
            .and_then(|info| info.caps.reasoning_effort)
            .map(ReasoningEffort::as_str)
    }

    fn model_choices(&self) -> Vec<ModelChoice> {
        let mut models: Vec<ModelChoice> = self
            .kernel
            .models()
            .list()
            .map(|info| ModelChoice {
                id: info.id.as_str().to_owned(),
                label: if info.display_name.trim().is_empty() {
                    info.id.as_str().to_owned()
                } else {
                    info.display_name.clone()
                },
                provider: info.provider.clone(),
                supports_effort: info.caps.reasoning_effort.is_some(),
            })
            .collect();
        models.sort_unstable_by(|a, b| a.id.cmp(&b.id));
        models
    }
}

fn render_prompt(
    prompt: &str,
    metrics: Option<&VoiceMetrics>,
    voice: Option<&VoiceSignals>,
) -> String {
    let mut attrs = vec![
        r#"crabgent="1""#.to_owned(),
        r#"source="web-dashboard""#.to_owned(),
    ];
    if let Some(voice) = voice {
        if !voice.audio_events.is_empty() {
            let events = voice
                .audio_events
                .iter()
                .map(|event| xml_escape_attr(&event.label))
                .collect::<Vec<_>>()
                .join(",");
            attrs.push(format!(r#"events="{events}""#));
        }
        match voice.speakers.as_slice() {
            [speaker] => attrs.push(format!(r#"speaker="{}""#, xml_escape_attr(speaker))),
            speakers if !speakers.is_empty() => {
                let speakers = speakers
                    .iter()
                    .map(|speaker| xml_escape_attr(speaker))
                    .collect::<Vec<_>>()
                    .join(",");
                attrs.push(format!(r#"speakers="{speakers}""#));
            }
            _ => {}
        }
        match voice.speaker_identities.as_slice() {
            [identity] => {
                attrs.push(format!(
                    r#"identified_speaker="{}""#,
                    xml_escape_attr(&identity_name(identity))
                ));
                attrs.push(format!(r#"speaker_confidence="{}""#, identity.confidence));
                attrs.push(format!(
                    r#"speaker_source="{}""#,
                    xml_escape_attr(&identity.source)
                ));
                if let Some(label) = identity.speaker_label.as_deref() {
                    attrs.push(format!(r#"speaker_label="{}""#, xml_escape_attr(label)));
                }
            }
            identities if !identities.is_empty() => {
                let identities = identities
                    .iter()
                    .map(identity_name)
                    .map(|identity| xml_escape_attr(&identity))
                    .collect::<Vec<_>>()
                    .join(",");
                attrs.push(format!(r#"identified_speakers="{identities}""#));
            }
            _ => {}
        }
        push_attr(&mut attrs, "pause_ms", voice.pause_ms);
        if let Some(rate) = voice.speech_rate_wpm {
            attrs.push(format!(r#"rate="{rate}""#));
        }
        if voice.hesitation_count > 0 {
            attrs.push(format!(r#"hesitations="{}""#, voice.hesitation_count));
        }
        if let Some(energy) = voice.energy_band {
            attrs.push(format!(r#"energy="{energy:?}""#).to_lowercase());
        }
    }
    if let Some(metrics) = metrics {
        if let Some(claimed) = clean_claimed_speaker(metrics.claimed_speaker.as_deref()) {
            attrs.push(format!(
                r#"claimed_speaker="{}""#,
                xml_escape_attr(&claimed)
            ));
        }
        push_attr(&mut attrs, "speech_ms", metrics.speech_ms);
        push_attr(&mut attrs, "browser_pause_ms", metrics.pause_ms);
        push_attr(&mut attrs, "browser_pause_count", metrics.pause_count);
        push_attr(&mut attrs, "words", metrics.words);
        push_attr(&mut attrs, "browser_rate", metrics.rate);
        if voice.and_then(|v| v.energy_band).is_none()
            && let Some(energy) = metrics.energy.as_deref().filter(|v| !v.trim().is_empty())
        {
            attrs.push(format!(r#"energy="{}""#, xml_escape_attr(energy)));
        }
        if let Some(source) = metrics.source.as_deref().filter(|v| !v.trim().is_empty()) {
            attrs.push(format!(r#"input="{}""#, xml_escape_attr(source)));
        }
    }
    format!("<voice {}/>\n{}", attrs.join(" "), prompt)
}

fn push_attr(attrs: &mut Vec<String>, name: &str, value: Option<u32>) {
    if let Some(value) = value {
        attrs.push(format!(r#"{name}="{value}""#));
    }
}

fn identity_name(identity: &SpeakerIdentity) -> String {
    identity
        .display
        .as_deref()
        .filter(|display| !display.trim().is_empty())
        .unwrap_or(&identity.id)
        .to_owned()
}

fn clean_claimed_speaker(value: Option<&str>) -> Option<String> {
    let value = value?.trim();
    if value.is_empty() {
        return None;
    }
    Some(value.chars().take(64).collect())
}

fn xml_escape_attr(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

async fn send_status(sink: &mut WsSink, entry: &AgentEntry) -> Result<(), axum::Error> {
    send_json(
        sink,
        "status",
        json!({
            "model": entry.model,
            "tts": entry.tts.is_some(),
            "stt": entry.stt.is_some(),
        }),
    )
    .await
}

async fn send_event(sink: &mut WsSink, event: &Event) -> Result<(), axum::Error> {
    let json = serde_json::to_string(event)
        .unwrap_or_else(|_| r#"{"kind":"turn_error","data":"event serialize failed"}"#.to_owned());
    sink.send(WsMessage::Text(Utf8Bytes::from(json))).await
}

async fn send_json(sink: &mut WsSink, kind: &str, data: Value) -> Result<(), axum::Error> {
    let json = serde_json::json!({ "kind": kind, "data": data }).to_string();
    sink.send(WsMessage::Text(Utf8Bytes::from(json))).await
}

fn is_authed(state: &VoiceState, headers: &HeaderMap) -> bool {
    let Some(cookie_header) = headers.get(header::COOKIE) else {
        return false;
    };
    let Ok(value) = cookie_header.to_str() else {
        return false;
    };
    for pair in value.split(';') {
        let pair = pair.trim();
        let Some((k, v)) = pair.split_once('=') else {
            continue;
        };
        if k.trim() == COOKIE_NAME {
            return constant_eq(v.trim().as_bytes(), state.expected_hash.as_bytes());
        }
    }
    false
}

fn unauthorized_json() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        axum::Json(serde_json::json!({"error": "unauthorized"})),
    )
        .into_response()
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write;
        let _ = write!(out, "{byte:02x}");
    }
    out
}

fn constant_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff: u8 = 0;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_frame_reads_stop() {
        assert!(matches!(parse_frame(r#"{"stop":true}"#), ParsedFrame::Stop));
    }

    #[test]
    fn parse_frame_reads_prompt_and_metrics() {
        let ParsedFrame::Prompt {
            prompt,
            metrics,
            options,
        } = parse_frame(
            r#"{"prompt":" hallo ","metrics":{"rate":120,"pause_count":2,"claimed_speaker":"partner"},"model":"gpt-5-mini","effort":"low"}"#,
        )
        else {
            panic!("expected prompt");
        };
        assert_eq!(prompt, "hallo");
        let metrics = metrics.expect("metrics");
        assert_eq!(metrics.rate, Some(120));
        assert_eq!(metrics.pause_count, Some(2));
        assert_eq!(metrics.claimed_speaker.as_deref(), Some("partner"));
        assert_eq!(options.model.as_deref(), Some("gpt-5-mini"));
        assert_eq!(options.effort, Some(ReasoningEffort::Low));
    }

    #[test]
    fn render_prompt_adds_voice_marker() {
        let metrics = VoiceMetrics {
            speech_ms: Some(2000),
            pause_ms: Some(350),
            words: Some(4),
            rate: Some(120),
            energy: Some("medium".to_owned()),
            claimed_speaker: Some("User & Partner".to_owned()),
            ..VoiceMetrics::default()
        };
        let rendered = render_prompt("mach kurz", Some(&metrics), None);
        assert!(rendered.starts_with(r#"<voice crabgent="1" source="web-dashboard""#));
        assert!(rendered.contains(r#"claimed_speaker="User &amp; Partner""#));
        assert!(rendered.contains(r#"speech_ms="2000""#));
        assert!(rendered.ends_with("\nmach kurz"));
    }

    #[test]
    fn render_prompt_adds_speaker_markers() {
        let voice = VoiceSignals {
            speakers: vec!["speaker_0".to_owned(), "speaker_1".to_owned()],
            ..VoiceSignals::default()
        };
        let rendered = render_prompt("mach kurz", None, Some(&voice));

        assert!(rendered.contains(r#"speakers="speaker_0,speaker_1""#));
    }

    #[test]
    fn render_prompt_adds_identified_speaker_marker() {
        let voice = VoiceSignals {
            speaker_identities: vec![
                SpeakerIdentity::new("user", "local-voiceprint", 82).with_display("User"),
            ],
            ..VoiceSignals::default()
        };
        let rendered = render_prompt("mach kurz", None, Some(&voice));

        assert!(rendered.contains(r#"identified_speaker="User""#));
        assert!(rendered.contains(r#"speaker_confidence="82""#));
        assert!(rendered.contains(r#"speaker_source="local-voiceprint""#));
    }

    #[test]
    fn build_voice_request_sets_agent_and_memory_owner_attrs() {
        let subject = voice_subject("assistant");

        assert_eq!(subject.id(), "web_voice:assistant");
        assert_eq!(subject.attr("agent"), Some("assistant"));
        assert_eq!(subject.attr(ORIGIN_OWNER_ATTR), Some("tui:assistant"));
    }

    #[test]
    fn build_voice_request_keeps_default_and_sets_explicit_model_options() {
        let options = TurnOptions {
            model: Some("gpt-5-mini".to_owned()),
            effort: Some(ReasoningEffort::Low),
        };
        let (_, req) = build_voice_request_with(
            VoiceRequestConfig {
                agent: "assistant",
                default_model: "gpt-5.5",
                fallbacks: &[],
                max_turns: Some(7),
                system_prompt: "base prompt",
            },
            "hallo".to_owned(),
            None,
            &options,
        );

        assert_eq!(req.model, ModelTarget::id(ModelId::new("gpt-5.5")));
        assert_eq!(
            req.explicit_model,
            Some(ModelTarget::id(ModelId::new("gpt-5-mini")))
        );
        assert_eq!(req.reasoning_effort, Some(ReasoningEffort::Low));
    }

    #[test]
    fn constant_eq_rejects_different_lengths() {
        assert!(!constant_eq(b"abc", b"abcd"));
    }
}
