//! TOML-driven configuration layout.

use std::{
    collections::HashMap,
    fmt, fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use anyhow::{Context, Result};
use crabgent_core::TtsAudioFormat;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct Config {
    pub sqlite_path: PathBuf,
    #[serde(default)]
    pub openai: Option<OpenAi>,
    #[serde(default)]
    pub cortecs: Option<CortecsConfig>,
    #[serde(default)]
    pub google: Option<GoogleConfig>,
    pub agents: Vec<Agent>,
    #[serde(default)]
    pub memory: MemoryConfig,
    pub stt: Option<SttConfig>,
    /// Voice-perception pipeline (global, shared by every agent). When
    /// present and `enabled`, inbound voice messages are retained and a
    /// `<voice/>` tag surfaces HOW the user spoke (pauses, rate, laughter,
    /// speaker labels, energy). `[voice.audio]` adds the `hear_again` pull
    /// tool and is also the `OpenAI` STT backend for inbound voice;
    /// `[voice.tts]` adds explicit opt-in spoken replies through `ElevenLabs`;
    /// `[voice.divergence]` adds
    /// the speculative text-vs-prosody hook. Both audio-native perception
    /// features need `[voice.audio]`, because a Codex-OAuth chat model cannot
    /// take audio input.
    #[serde(default)]
    pub voice: Option<VoiceConfig>,
    #[serde(default)]
    pub mcp: McpClientConfig,
    /// Embedding provider for hybrid memory recall (FTS + vector
    /// cosine). When absent, the memory tool falls back to FTS-only,
    /// same as before the embedding feature landed upstream.
    #[serde(default)]
    pub embedding: Option<EmbeddingConfig>,
    /// Outbound HTTP MCP server (multi-agent path-routed). When set,
    /// the host binds on `mcp_server.bind` and exposes
    /// `POST /mcp/<agent_name>` per agent that has `mcp_bearer_token`.
    #[serde(default)]
    pub mcp_server: Option<McpServerHostConfig>,
    /// Web admin UI (memory browser/editor). Mounted on the same axum
    /// server as `mcp_server`. Requires `[mcp_server]` to be present
    /// because it reuses the bind address. Auth is a static cookie key.
    #[serde(default)]
    pub web: Option<WebAdminConfig>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "provider", rename_all = "snake_case")]
pub enum EmbeddingConfig {
    /// Local FastEmbed-rs BGE-M3 (1024 dim, ONNX model auto-downloads
    /// on first use). Bit-compatible with Cortecs cloud bge-m3 so
    /// pre-computed embeddings can drop in 1:1. Requires the
    /// `fastembed` cargo feature and an AVX2-capable CPU.
    Fastembed,
    /// OpenAI-compatible `/v1/embeddings` server (Infinity, TEI,
    /// Cortecs, vLLM, `OpenAI` itself). Network call, no local model
    /// load. `base_url` must include the `/v1` suffix; the upstream
    /// `OpenAiEmbeddingProvider` appends `/embeddings`. Use
    /// `api_key = ""` for local servers that do not gate by token.
    Openai {
        base_url: String,
        model: String,
        #[serde(default = "default_openai_embed_dim")]
        dim: usize,
        #[serde(default)]
        api_key: String,
    },
}

const fn default_openai_embed_dim() -> usize {
    1024
}

#[derive(Clone, Deserialize)]
pub struct WebAdminConfig {
    /// Static bearer-equivalent token. The login form accepts this
    /// verbatim; the resulting cookie value is `sha256(token)` hex so
    /// the raw token never sits in browser storage.
    pub auth_token: String,
}

impl fmt::Debug for WebAdminConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WebAdminConfig")
            .field("auth_token", &"[REDACTED]")
            .finish()
    }
}

#[derive(Clone, Deserialize)]
pub struct McpServerHostConfig {
    /// `host:port` (e.g. `127.0.0.1:3100`). Default off — leave the
    /// block out to disable the server entirely.
    pub bind: String,
}

impl fmt::Debug for McpServerHostConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("McpServerHostConfig")
            .field("bind", &self.bind)
            .finish()
    }
}

#[derive(Debug, Default, Deserialize, Clone)]
pub struct OpenAi {
    /// Path to the persisted OAuth token JSON. Defaults to
    /// `~/.config/<app>/credentials/openai_oauth_token`.
    pub token_path: Option<PathBuf>,
    /// Public `OpenAI` API key (sk-...). When set, the provider uses
    /// `ApiKeyAuth` against the public chat-completions endpoint and the
    /// OAuth token loader is skipped entirely. Takes precedence over
    /// `token_path` when both are configured.
    pub api_key: Option<String>,
    /// Optional dedicated `OpenAI` API key for image generation through
    /// `/v1/images/generations`. Codex-OAuth image generation uses the hosted
    /// Responses image tool instead. This key is a fallback for hosts without
    /// Codex-OAuth.
    pub image_api_key: Option<String>,
}

#[derive(Clone, Deserialize)]
pub struct CortecsConfig {
    pub api_key: String,
    #[serde(default = "default_cortecs_base_url")]
    pub base_url: String,
}

impl fmt::Debug for CortecsConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CortecsConfig")
            .field("api_key", &"[REDACTED]")
            .field("base_url", &self.base_url)
            .finish()
    }
}

fn default_cortecs_base_url() -> String {
    "https://api.cortecs.ai".to_owned()
}

#[derive(Debug, Default, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum AgentProvider {
    Anthropic,
    #[default]
    OpenAi,
    Google,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GoogleConfig {
    pub api_key: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MemoryConfig {
    #[serde(default = "default_memory_persist_hook")]
    pub persist_hook: bool,
    /// When true, the `MemoryRecallHook` injects the top-N matching
    /// memories into the trailing user message on the first
    /// `before_llm` of each run. Costs an extra embedding round-trip
    /// and 1-2 KB of context tokens per turn but lets an agent answer
    /// known-fact questions without a separate `memory.search` tool
    /// call. Off by default; opt-in per-deployment in `[memory]`.
    #[serde(default)]
    pub auto_recall: bool,
    /// Hit count for `auto_recall`. Clamped to 1..=20. Default 5.
    #[serde(default)]
    pub auto_recall_limit: Option<u32>,
    /// Canonical-user identity map. Each entry groups the per-channel
    /// owner strings that belong to one human so memory recall is
    /// scoped to the person, not the channel identity. Empty by
    /// default: recall then falls back to owner-string isolation.
    #[serde(default)]
    pub users: Vec<UserIdentity>,
}

/// One human's set of per-channel owner strings. `owners` lists every
/// `Subject::id()` form that maps to the same person (e.g. a Matrix
/// MXID and a Telegram numeric id). Recall for any one of these owners
/// returns memories stored under all of them.
#[derive(Debug, Clone, Deserialize)]
pub struct UserIdentity {
    /// Informational canonical label for the human (e.g. "user").
    /// Deserialized from config but not otherwise read: recall keys on
    /// `owners`.
    #[allow(dead_code)]
    pub canonical: String,
    pub owners: Vec<String>,
}

impl Default for MemoryConfig {
    fn default() -> Self {
        Self {
            persist_hook: default_memory_persist_hook(),
            auto_recall: false,
            auto_recall_limit: None,
            users: Vec::new(),
        }
    }
}

const fn default_memory_persist_hook() -> bool {
    true
}

#[derive(Debug, Clone, Deserialize)]
pub struct SttConfig {
    pub elevenlabs: Option<ElevenLabsSttConfig>,
    #[serde(default)]
    pub openai: Option<OpenAiSttConfig>,
}

#[derive(Clone, Deserialize)]
pub struct ElevenLabsSttConfig {
    pub api_key: String,
    #[serde(default)]
    pub api_base: Option<String>,
}

impl fmt::Debug for ElevenLabsSttConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ElevenLabsSttConfig")
            .field("api_key", &"[REDACTED]")
            .field("api_base", &self.api_base)
            .finish()
    }
}

/// Enable OpenAI-backed voice transcription.
///
/// Kept for config compatibility: when this block is present,
/// the host routes inbound voice through `[voice.audio]` and the
/// audio-native model configured there. The `model` field is no longer sent to
/// the legacy transcription endpoint.
#[derive(Debug, Default, Clone, Deserialize)]
pub struct OpenAiSttConfig {
    #[serde(default = "default_openai_stt_model")]
    pub model: String,
}

fn default_openai_stt_model() -> String {
    "gpt-audio".to_owned()
}

/// Voice-perception configuration. `enabled` turns on audio retention plus
/// prosody (the `<voice/>` tag). The optional `[voice.audio]` block adds an
/// audio-native model route (`hear_again` tool); `[voice.divergence]` adds the
/// speculative text-vs-prosody perception hook on top of that route.
#[derive(Debug, Clone, Deserialize)]
pub struct VoiceConfig {
    /// Master switch. When false the whole pipeline is inert (legacy
    /// flat-text transcription).
    pub enabled: bool,
    /// Directory for retained audio blobs. Defaults to `<sqlite_dir>/audio`.
    #[serde(default)]
    pub audio_store_path: Option<PathBuf>,
    /// Largest retained blob in bytes. Defaults to the channel crate's
    /// `AUDIO_PAYLOAD_MAX_BYTES` (25 MiB).
    #[serde(default)]
    pub audio_max_bytes: Option<usize>,
    /// Retained-audio TTL in seconds for the sweeper. When absent, retained
    /// audio is not swept (lives until manual cleanup).
    #[serde(default)]
    pub retention_ttl_secs: Option<u64>,
    /// Prosody compute tunables.
    #[serde(default)]
    pub prosody: ProsodyTomlConfig,
    /// Audio-native model route. Present enables `hear_again`.
    #[serde(default)]
    pub audio: Option<AudioRouteConfig>,
    /// Explicit spoken-output route. Present and enabled registers
    /// `voice_reply`, backed by upstream provider-neutral TTS plus optional
    /// forced alignment.
    #[serde(default)]
    pub tts: Option<VoiceTtsConfig>,
    /// Speculative text-vs-prosody divergence routing. Requires `[voice.audio]`.
    #[serde(default)]
    pub divergence: Option<DivergenceTomlConfig>,
    /// Optional deployment-local speaker identification from reference samples.
    #[serde(default)]
    pub speaker_id: Option<SpeakerIdTomlConfig>,
    /// Shared audio-call circuit-breaker tunables.
    #[serde(default)]
    pub circuit: Option<CircuitTomlConfig>,
}

/// Tunables mapped onto `crabgent_prosody::ProsodyConfig`.
#[derive(Debug, Clone, Deserialize)]
pub struct ProsodyTomlConfig {
    #[serde(default = "default_word_timing")]
    pub word_timing: bool,
    #[serde(default = "default_hesitation_ms")]
    pub hesitation_threshold_ms: u32,
}

// `default()` reuses the serde default helpers so the defaults live in one
// place; clippy attributes its const-fn suggestion to the whole impl block.
#[allow(clippy::missing_const_for_fn)]
impl Default for ProsodyTomlConfig {
    fn default() -> Self {
        Self {
            word_timing: default_word_timing(),
            hesitation_threshold_ms: default_hesitation_ms(),
        }
    }
}

const fn default_word_timing() -> bool {
    true
}

const fn default_hesitation_ms() -> u32 {
    600
}

/// Audio-native model route (`hear_again` + divergence push). The api key
/// must be one that can reach the `OpenAI` Audio API (a `sk-` key); a
/// Codex-OAuth token cannot.
#[derive(Clone, Deserialize)]
pub struct AudioRouteConfig {
    pub api_key: String,
    #[serde(default = "default_audio_model")]
    pub model: String,
}

/// Deployment-local speaker identification route.
///
/// Samples are local files or directories. Upstream never sees these profile ids
/// or paths; they are injected through the generic `SpeakerIdentifier` surface.
#[derive(Debug, Clone, Deserialize)]
pub struct SpeakerIdTomlConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_speaker_id_threshold")]
    pub threshold: u8,
    #[serde(default = "default_speaker_id_margin")]
    pub margin: u8,
    #[serde(default)]
    pub ffmpeg_path: Option<PathBuf>,
    #[serde(default)]
    pub profiles: Vec<SpeakerProfileTomlConfig>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SpeakerProfileTomlConfig {
    pub id: String,
    #[serde(default)]
    pub display: Option<String>,
    #[serde(default)]
    pub samples: Vec<PathBuf>,
}

const fn default_speaker_id_threshold() -> u8 {
    68
}

const fn default_speaker_id_margin() -> u8 {
    6
}

impl fmt::Debug for AudioRouteConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AudioRouteConfig")
            .field("api_key", &"[REDACTED]")
            .field("model", &self.model)
            .finish()
    }
}

fn default_audio_model() -> String {
    // `gpt-4o-audio-preview` was retired by OpenAI (404 model_not_found); the
    // successor audio-input model is `gpt-audio`.
    "gpt-audio".to_owned()
}

/// `ElevenLabs` text-to-speech route for explicit spoken replies.
#[derive(Clone, Deserialize)]
pub struct VoiceTtsConfig {
    /// Defaults to true when the `[voice.tts]` block exists.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Optional override. When absent, `[stt.elevenlabs].api_key` is reused.
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub api_base: Option<String>,
    #[serde(default = "default_elevenlabs_tts_model")]
    pub model: String,
    /// `ElevenLabs` voice id. Required when this block is enabled.
    #[serde(default)]
    pub voice: String,
    #[serde(default)]
    pub format: TtsAudioFormat,
    /// Run forced alignment on generated speech and return timing feedback
    /// to the agent. Defaults to true.
    #[serde(default = "default_true")]
    pub forced_alignment: bool,
    #[serde(default)]
    pub settings: Option<VoiceTtsSettingsConfig>,
}

impl fmt::Debug for VoiceTtsConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("VoiceTtsConfig")
            .field("enabled", &self.enabled)
            .field("api_key", &self.api_key.as_ref().map(|_| "[REDACTED]"))
            .field("api_base", &self.api_base)
            .field("model", &self.model)
            .field("voice", &self.voice)
            .field("format", &self.format)
            .field("forced_alignment", &self.forced_alignment)
            .field("settings", &self.settings)
            .finish()
    }
}

/// `ElevenLabs` per-request voice settings for explicit spoken replies.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct VoiceTtsSettingsConfig {
    #[serde(default)]
    pub stability: Option<f32>,
    #[serde(default)]
    pub similarity_boost: Option<f32>,
    #[serde(default)]
    pub style: Option<f32>,
    #[serde(default)]
    pub speed: Option<f32>,
    #[serde(default)]
    pub use_speaker_boost: Option<bool>,
}

fn default_elevenlabs_tts_model() -> String {
    "eleven_multilingual_v2".to_owned()
}

const fn default_true() -> bool {
    true
}

/// Thresholds mapped onto `crabgent_prosody::DivergenceConfig`. `enabled`
/// gates the `DivergenceHook` independently of the `hear_again` tool.
#[derive(Debug, Clone, Deserialize)]
pub struct DivergenceTomlConfig {
    pub enabled: bool,
    #[serde(default = "default_flat_max_wpm")]
    pub flat_max_wpm: u16,
    #[serde(default = "default_animated_min_wpm")]
    pub animated_min_wpm: u16,
    #[serde(default = "default_flat_min_pause_ms")]
    pub flat_min_pause_ms: u32,
}

const fn default_flat_max_wpm() -> u16 {
    110
}

const fn default_animated_min_wpm() -> u16 {
    200
}

const fn default_flat_min_pause_ms() -> u32 {
    700
}

/// Tunables mapped onto `crabgent_tool_audio::AudioCircuitConfig`.
#[derive(Debug, Clone, Deserialize)]
pub struct CircuitTomlConfig {
    #[serde(default = "default_max_consecutive_failures")]
    pub max_consecutive_failures: u32,
    #[serde(default = "default_per_call_timeout_secs")]
    pub per_call_timeout_secs: u64,
    #[serde(default = "default_cooldown_secs")]
    pub cooldown_secs: u64,
    #[serde(default = "default_max_send_bytes")]
    pub max_send_bytes: usize,
}

const fn default_max_consecutive_failures() -> u32 {
    3
}

const fn default_per_call_timeout_secs() -> u64 {
    5
}

const fn default_cooldown_secs() -> u64 {
    30
}

const fn default_max_send_bytes() -> usize {
    10 * 1024 * 1024
}

#[derive(Debug, Default, Clone, Deserialize)]
pub struct McpClientConfig {
    #[serde(default)]
    pub servers: Vec<McpServerEntry>,
}

#[derive(Clone, Deserialize)]
pub struct McpServerEntry {
    pub name: String,
    pub base_url: String,
    #[serde(default)]
    pub token: Option<String>,
    /// Optional allowlist of unprefixed tool names to register from this
    /// server. Absent exposes every discovered tool; present registers
    /// only the listed ones. Use to pull a single capability, e.g.
    /// `tools = ["chat"]` so this agent can message a peer agent without
    /// inheriting that peer's full (and possibly dangerous) toolset.
    #[serde(default)]
    pub tools: Option<Vec<String>>,
}

impl fmt::Debug for McpServerEntry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("McpServerEntry")
            .field("name", &self.name)
            .field("base_url", &self.base_url)
            .field("token", &self.token.as_ref().map(|_| "[REDACTED]"))
            .field("tools", &self.tools)
            .finish()
    }
}

#[derive(Debug, Deserialize)]
pub struct Agent {
    pub name: String,
    pub bot_token: Option<String>,
    pub bot_user_id: Option<String>,
    pub bot_username: Option<String>,
    pub pair_token: Option<String>,
    pub matrix: Option<MatrixAgentConfig>,
    pub model: String,
    pub system_prompt: String,
    pub max_turns: Option<u32>,
    pub holidays_country: Option<String>,
    pub holidays_subdivision: Option<String>,
    #[serde(default)]
    pub provider: AgentProvider,
    /// Ordered fallback chain. When the primary model returns a
    /// retryable provider error (5xx, retryable stream, transport,
    /// timeout, short retry-after 429), the kernel re-attempts the run
    /// against these ids in order. Each id must resolve to a registered
    /// provider (main or secondary). Empty by default.
    #[serde(default)]
    pub fallback_models: Vec<String>,
    /// Per-agent bearer token for the multi-agent MCP HTTP server.
    /// When `[mcp_server]` is configured AND this field is set, the
    /// server mounts `POST /mcp/<name>` and requires
    /// `Authorization: Bearer <token>` on every request. Agents
    /// without this field are not exposed over MCP.
    #[serde(default)]
    pub mcp_bearer_token: Option<String>,
    /// Per-agent bearer token for the TUI WebSocket bridge. When absent,
    /// the TUI falls back to `mcp_bearer_token` for backwards compatibility.
    /// Use this for agents that should be reachable through `/tui/<name>`
    /// without exposing `POST /mcp/<name>`.
    #[serde(default)]
    pub tui_bearer_token: Option<String>,
    /// Per-agent `reasoning_effort` override for `OpenAI` models (gpt-5.x).
    /// Accepts `"low"`, `"medium"`, or `"high"`. When absent or invalid,
    /// the kernel falls back to the model's capability default (typically
    /// `Low` for known `OpenAI` models). Ignored by providers that don't
    /// consume this field.
    #[serde(default)]
    pub reasoning_effort: Option<String>,
    /// Enable provider-hosted web search for this agent.
    #[serde(default)]
    pub web_search: bool,
    /// Optional cap on hosted web-search results per request.
    #[serde(default)]
    pub web_search_max_uses: Option<u32>,
    /// Swap the dumb `ToolCacheHook` preview-cache for
    /// `crabgent-tool-compact`'s filter-based recoverable compaction
    /// (`recall` tool, ops `recall_raw`/`expand`). Register one or the
    /// other, never both. Default false keeps the preview-cache.
    #[serde(default)]
    pub tool_compact: bool,
    /// Optional local tmux operational tool. Disabled by default because it
    /// can inspect and post into local terminal panes. Enable only for trusted
    /// single-user agents.
    #[serde(default)]
    pub tmux: TmuxConfig,
}

#[derive(Debug, Default, Deserialize, Clone)]
pub struct TmuxConfig {
    #[serde(default)]
    pub enabled: bool,
    /// Window used when the agent calls `tmux.read` or `tmux.send` without an
    /// explicit window. Defaults to the agent name.
    #[serde(default)]
    pub default_window: Option<String>,
}

#[derive(Deserialize)]
pub struct MatrixAgentConfig {
    pub homeserver: String,
    pub user: String,
    pub access_token: String,
    pub device_id: String,
    #[serde(default)]
    pub allowed_users: Vec<String>,
    pub restricted_tools: Option<Vec<String>>,
    /// When true, skip the `ChannelScopePolicy` channel-scope gate and
    /// use `AllowAllPolicy` for memory/cron/session-search scope checks.
    /// Use for trusted single-user deployments where the human should be
    /// able to read all memories regardless of which DM the conversation
    /// happens in. Default false.
    #[serde(default)]
    pub loose_policy: bool,
}

impl fmt::Debug for MatrixAgentConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MatrixAgentConfig")
            .field("homeserver", &self.homeserver)
            .field("user", &self.user)
            .field("access_token", &"[REDACTED]")
            .field("device_id", &self.device_id)
            .field("allowed_users", &self.allowed_users)
            .field("restricted_tools", &self.restricted_tools)
            .field("loose_policy", &self.loose_policy)
            .finish()
    }
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        let mut cfg = Self::load_unresolved(path)?;
        cfg.resolve_secret_refs()?;
        cfg.validate()?;
        Ok(cfg)
    }

    pub fn load_unresolved(path: &Path) -> Result<Self> {
        let raw = fs::read_to_string(path)
            .with_context(|| format!("read config from {}", path.display()))?;
        let cfg: Self = toml::from_str(&raw).with_context(|| "parse config TOML")?;
        cfg.validate()?;
        Ok(cfg)
    }

    fn resolve_secret_refs(&mut self) -> Result<()> {
        let mut references = Vec::new();
        resolve_secret_refs_with(self, |reference| {
            references.push(reference.to_owned());
            Ok(reference.to_owned())
        })?;
        references.sort();
        references.dedup();

        let secrets = resolve_secretctl_secret_refs(&references)?;
        resolve_secret_refs_with(self, |reference| {
            secrets
                .get(reference)
                .cloned()
                .with_context(|| format!("secretctl did not return {reference}"))
        })
    }

    fn validate(&self) -> Result<()> {
        anyhow::ensure!(!self.agents.is_empty(), "config has no [[agents]] entries");
        let uses_anthropic = self
            .agents
            .iter()
            .any(|agent| matches!(agent.provider, AgentProvider::Anthropic));
        anyhow::ensure!(
            !uses_anthropic,
            "provider=\"anthropic\" is disabled in this deployment; use provider=\"openai\" or \"google\""
        );
        validate_stt(self.stt.as_ref())?;
        for a in &self.agents {
            anyhow::ensure!(!a.name.is_empty(), "agent name must not be empty");
            validate_fallback_models(a)?;
            if let Some(matrix) = &a.matrix {
                validate_matrix(&a.name, matrix)?;
            }
            if has_telegram_config(a) {
                validate_telegram(a)?;
            }
        }
        if let Some(web) = &self.web {
            anyhow::ensure!(
                !web.auth_token.trim().is_empty(),
                "[web].auth_token is empty",
            );
            anyhow::ensure!(
                self.mcp_server.is_some(),
                "[web] is set but [mcp_server] is missing; the admin UI reuses the MCP server bind address",
            );
        }
        if let Some(cortecs) = &self.cortecs {
            anyhow::ensure!(
                !cortecs.api_key.trim().is_empty(),
                "[cortecs].api_key is empty",
            );
            anyhow::ensure!(
                !cortecs.base_url.trim().is_empty(),
                "[cortecs].base_url is empty",
            );
        }
        validate_voice(self.voice.as_ref(), self.stt.as_ref())?;
        validate_stt_voice(self.stt.as_ref(), self.voice.as_ref())?;
        Ok(())
    }

    #[must_use]
    pub fn image_cache_path(&self) -> PathBuf {
        let base = self
            .sqlite_path
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        base.join("image-cache")
    }

    /// Append-only JSONL sink for tool-error audit records, written by
    /// the per-agent `ErrorAuditHook` and consumed by
    /// `scripts/agent-error-review.py`. Lives next to the store so it
    /// shares the data directory created at startup.
    #[must_use]
    pub fn error_audit_path(&self) -> PathBuf {
        let base = self
            .sqlite_path
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        base.join("error_audit.jsonl")
    }
}

fn resolve_secret_refs_with<F>(cfg: &mut Config, mut resolve: F) -> Result<()>
where
    F: FnMut(&str) -> Result<String>,
{
    if let Some(openai) = &mut cfg.openai {
        resolve_secret_option(&mut openai.api_key, "openai.api_key", &mut resolve)?;
        resolve_secret_option(
            &mut openai.image_api_key,
            "openai.image_api_key",
            &mut resolve,
        )?;
    }
    if let Some(cortecs) = &mut cfg.cortecs {
        resolve_secret_string(&mut cortecs.api_key, "cortecs.api_key", &mut resolve)?;
    }
    if let Some(google) = &mut cfg.google {
        resolve_secret_string(&mut google.api_key, "google.api_key", &mut resolve)?;
    }
    if let Some(EmbeddingConfig::Openai { api_key, .. }) = &mut cfg.embedding {
        resolve_secret_string(api_key, "embedding.api_key", &mut resolve)?;
    }
    if let Some(web) = &mut cfg.web {
        resolve_secret_string(&mut web.auth_token, "web.auth_token", &mut resolve)?;
    }
    if let Some(stt) = &mut cfg.stt
        && let Some(eleven) = &mut stt.elevenlabs
    {
        resolve_secret_string(&mut eleven.api_key, "stt.elevenlabs.api_key", &mut resolve)?;
    }
    if let Some(voice) = &mut cfg.voice {
        if let Some(audio) = &mut voice.audio {
            resolve_secret_string(&mut audio.api_key, "voice.audio.api_key", &mut resolve)?;
        }
        if let Some(tts) = &mut voice.tts {
            resolve_secret_option(&mut tts.api_key, "voice.tts.api_key", &mut resolve)?;
        }
    }
    for (idx, server) in cfg.mcp.servers.iter_mut().enumerate() {
        let path = format!("mcp.servers[{idx}].token");
        resolve_secret_option(&mut server.token, &path, &mut resolve)?;
    }
    for (idx, agent) in cfg.agents.iter_mut().enumerate() {
        let prefix = format!("agents[{idx}]");
        resolve_secret_option(
            &mut agent.bot_token,
            &format!("{prefix}.bot_token"),
            &mut resolve,
        )?;
        resolve_secret_option(
            &mut agent.pair_token,
            &format!("{prefix}.pair_token"),
            &mut resolve,
        )?;
        resolve_secret_option(
            &mut agent.mcp_bearer_token,
            &format!("{prefix}.mcp_bearer_token"),
            &mut resolve,
        )?;
        resolve_secret_option(
            &mut agent.tui_bearer_token,
            &format!("{prefix}.tui_bearer_token"),
            &mut resolve,
        )?;
        if let Some(matrix) = &mut agent.matrix {
            resolve_secret_string(
                &mut matrix.access_token,
                &format!("{prefix}.matrix.access_token"),
                &mut resolve,
            )?;
        }
    }
    Ok(())
}

fn resolve_secret_option<F>(value: &mut Option<String>, path: &str, resolve: &mut F) -> Result<()>
where
    F: FnMut(&str) -> Result<String>,
{
    if let Some(value) = value {
        resolve_secret_string(value, path, resolve)?;
    }
    Ok(())
}

fn resolve_secret_string<F>(value: &mut String, path: &str, resolve: &mut F) -> Result<()>
where
    F: FnMut(&str) -> Result<String>,
{
    let reference = value.trim();
    if !is_secret_ref(reference) {
        return Ok(());
    }
    *value = resolve(reference).with_context(|| format!("resolve secret ref for {path}"))?;
    Ok(())
}

fn is_secret_ref(value: &str) -> bool {
    value.starts_with("op://") || value.starts_with("keychain://")
}

pub fn resolve_secret_if_ref(value: &str) -> Result<String> {
    let reference = value.trim();
    if is_secret_ref(reference) {
        return resolve_secretctl_secret(reference);
    }
    Ok(value.to_owned())
}

fn resolve_secretctl_secret(reference: &str) -> Result<String> {
    let output = Command::new(secretctl_binary())
        .arg("get")
        .arg(reference)
        .stdin(Stdio::null())
        .output()
        .with_context(|| "spawn secretctl get")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("secretctl get failed: {}", stderr.trim());
    }
    String::from_utf8(output.stdout).with_context(|| "secretctl returned non-UTF-8 secret")
}

fn resolve_secretctl_secret_refs(references: &[String]) -> Result<HashMap<String, String>> {
    let mut out = HashMap::new();
    if references.is_empty() {
        return Ok(out);
    }
    if references.len() == 1 {
        let reference = &references[0];
        out.insert(reference.clone(), resolve_secretctl_secret(reference)?);
        return Ok(out);
    }

    let output = Command::new(secretctl_binary())
        .arg("get-many")
        .args(references)
        .stdin(Stdio::null())
        .output()
        .with_context(|| "spawn secretctl get-many")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("secretctl get-many failed: {}", stderr.trim());
    }
    parse_secretctl_many_output(&output.stdout)
}

fn parse_secretctl_many_output(stdout: &[u8]) -> Result<HashMap<String, String>> {
    let text =
        String::from_utf8(stdout.to_vec()).with_context(|| "secretctl returned non-UTF-8 refs")?;
    let mut out = HashMap::new();
    for line in text.lines() {
        let (reference, hex) = line
            .split_once('\t')
            .with_context(|| "secretctl get-many returned malformed line")?;
        let bytes = decode_hex_secret(hex)?;
        let secret =
            String::from_utf8(bytes).with_context(|| "secretctl returned non-UTF-8 secret")?;
        out.insert(reference.to_owned(), secret);
    }
    Ok(out)
}

fn decode_hex_secret(hex: &str) -> Result<Vec<u8>> {
    anyhow::ensure!(
        hex.len().is_multiple_of(2),
        "secretctl get-many returned odd-length hex"
    );
    let mut out = Vec::with_capacity(hex.len() / 2);
    let bytes = hex.as_bytes();
    let mut idx = 0;
    while idx < bytes.len() {
        let hi = hex_digit(bytes[idx])?;
        let lo = hex_digit(bytes[idx + 1])?;
        out.push((hi << 4) | lo);
        idx += 2;
    }
    Ok(out)
}

fn hex_digit(byte: u8) -> Result<u8> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        _ => anyhow::bail!("secretctl get-many returned non-hex byte"),
    }
}

fn secretctl_binary() -> PathBuf {
    if let Some(path) = std::env::var_os("SECRETCTL_BIN") {
        return PathBuf::from(path);
    }
    if let Some(home) = std::env::var_os("HOME") {
        let local = PathBuf::from(home).join(".local/bin/secretctl");
        if local.exists() {
            return local;
        }
    }
    PathBuf::from("secretctl")
}

fn validate_fallback_models(agent: &Agent) -> Result<()> {
    for model in &agent.fallback_models {
        let normalized = model.trim().to_ascii_lowercase();
        anyhow::ensure!(
            !(normalized.starts_with("claude-") || normalized.starts_with("anthropic/")),
            "agent {} fallback model {} uses disabled Anthropic provider; use an OpenAI or Google model",
            agent.name,
            model
        );
    }
    Ok(())
}

fn validate_stt(stt: Option<&SttConfig>) -> Result<()> {
    let Some(stt) = stt else {
        return Ok(());
    };
    anyhow::ensure!(
        stt.elevenlabs.is_some() || stt.openai.is_some(),
        "stt block needs at least one of [stt.elevenlabs] or [stt.openai]"
    );
    if let Some(eleven) = &stt.elevenlabs {
        anyhow::ensure!(
            !eleven.api_key.trim().is_empty(),
            "stt.elevenlabs.api_key is empty"
        );
        if let Some(api_base) = &eleven.api_base {
            anyhow::ensure!(
                !api_base.trim().is_empty(),
                "stt.elevenlabs.api_base is empty"
            );
        }
    }
    if let Some(openai) = &stt.openai {
        anyhow::ensure!(!openai.model.trim().is_empty(), "stt.openai.model is empty");
    }
    Ok(())
}

fn validate_voice(voice: Option<&VoiceConfig>, stt: Option<&SttConfig>) -> Result<()> {
    let Some(voice) = voice else {
        return Ok(());
    };
    if !voice.enabled {
        return Ok(());
    }
    if let Some(audio) = &voice.audio {
        anyhow::ensure!(
            !audio.api_key.trim().is_empty(),
            "[voice.audio].api_key is empty"
        );
        anyhow::ensure!(
            !audio.model.trim().is_empty(),
            "[voice.audio].model is empty"
        );
    }
    if let Some(tts) = &voice.tts
        && tts.enabled
    {
        let has_api_key = tts
            .api_key
            .as_ref()
            .is_some_and(|key| !key.trim().is_empty())
            || stt
                .and_then(|cfg| cfg.elevenlabs.as_ref())
                .is_some_and(|cfg| !cfg.api_key.trim().is_empty());
        anyhow::ensure!(
            has_api_key,
            "[voice.tts] needs api_key or [stt.elevenlabs].api_key"
        );
        anyhow::ensure!(!tts.model.trim().is_empty(), "[voice.tts].model is empty");
        anyhow::ensure!(!tts.voice.trim().is_empty(), "[voice.tts].voice is empty");
        validate_voice_tts_settings(tts.settings.as_ref())?;
    }
    if voice.divergence.as_ref().is_some_and(|d| d.enabled) {
        anyhow::ensure!(
            voice.audio.is_some(),
            "[voice.divergence] is enabled but [voice.audio] is missing; the divergence hook needs an audio-native model route"
        );
    }
    if let Some(speaker_id) = &voice.speaker_id
        && speaker_id.enabled
    {
        validate_speaker_id(speaker_id)?;
    }
    Ok(())
}

fn validate_speaker_id(cfg: &SpeakerIdTomlConfig) -> Result<()> {
    anyhow::ensure!(cfg.threshold <= 100, "[voice.speaker_id].threshold > 100");
    anyhow::ensure!(cfg.margin <= 100, "[voice.speaker_id].margin > 100");
    for (idx, profile) in cfg.profiles.iter().enumerate() {
        let prefix = format!("[voice.speaker_id.profiles][{idx}]");
        anyhow::ensure!(!profile.id.trim().is_empty(), "{prefix}.id is empty");
        if let Some(display) = &profile.display {
            anyhow::ensure!(!display.trim().is_empty(), "{prefix}.display is empty");
        }
    }
    Ok(())
}

fn validate_voice_tts_settings(settings: Option<&VoiceTtsSettingsConfig>) -> Result<()> {
    let Some(settings) = settings else {
        return Ok(());
    };
    validate_optional_unit(settings.stability, "[voice.tts.settings].stability")?;
    validate_optional_unit(
        settings.similarity_boost,
        "[voice.tts.settings].similarity_boost",
    )?;
    validate_optional_unit(settings.style, "[voice.tts.settings].style")?;
    validate_optional_range(settings.speed, "[voice.tts.settings].speed", 0.7, 1.2)
}

fn validate_optional_unit(value: Option<f32>, field: &str) -> Result<()> {
    validate_optional_range(value, field, 0.0, 1.0)
}

fn validate_optional_range(value: Option<f32>, field: &str, min: f32, max: f32) -> Result<()> {
    let Some(value) = value else {
        return Ok(());
    };
    anyhow::ensure!(value.is_finite(), "{field} must be finite");
    anyhow::ensure!(
        (min..=max).contains(&value),
        "{field} must be between {min} and {max}"
    );
    Ok(())
}

fn validate_stt_voice(stt: Option<&SttConfig>, voice: Option<&VoiceConfig>) -> Result<()> {
    if stt.is_none_or(|s| s.openai.is_none()) {
        return Ok(());
    }
    let Some(voice) = voice else {
        anyhow::bail!(
            "[stt.openai] now uses the audio-native voice route; configure `[voice]` and `[voice.audio]`"
        );
    };
    anyhow::ensure!(
        voice.enabled,
        "[stt.openai] now uses the audio-native voice route; set `[voice].enabled = true`"
    );
    anyhow::ensure!(
        voice.audio.is_some(),
        "[stt.openai] now uses the audio-native voice route; configure `[voice.audio]`"
    );
    Ok(())
}

fn validate_matrix(agent_name: &str, matrix: &MatrixAgentConfig) -> Result<()> {
    anyhow::ensure!(
        !matrix.homeserver.is_empty(),
        "{agent_name} matrix.homeserver empty"
    );
    anyhow::ensure!(!matrix.user.is_empty(), "{agent_name} matrix.user empty");
    anyhow::ensure!(
        !matrix.access_token.is_empty(),
        "{agent_name} matrix.access_token empty"
    );
    anyhow::ensure!(
        !matrix.device_id.is_empty(),
        "{agent_name} matrix.device_id empty"
    );
    Ok(())
}

fn validate_telegram(agent: &Agent) -> Result<()> {
    let name = &agent.name;
    anyhow::ensure!(
        agent.bot_token.as_deref().is_some_and(|v| !v.is_empty()),
        "{name} bot_token empty"
    );
    anyhow::ensure!(
        agent.pair_token.as_deref().is_some_and(|v| !v.is_empty()),
        "{name} pair_token empty"
    );
    anyhow::ensure!(
        agent.bot_user_id.as_deref().is_some_and(|v| !v.is_empty()),
        "{name} bot_user_id empty"
    );
    anyhow::ensure!(
        agent.bot_username.as_deref().is_some_and(|v| !v.is_empty()),
        "{name} bot_username empty"
    );
    Ok(())
}

fn has_telegram_config(agent: &Agent) -> bool {
    [
        agent.bot_token.as_deref(),
        agent.bot_user_id.as_deref(),
        agent.bot_username.as_deref(),
        agent.pair_token.as_deref(),
    ]
    .into_iter()
    .any(|value| value.is_some())
}

#[cfg(test)]
mod tests {
    use crabgent_core::TtsAudioFormat;

    use super::{
        Agent, AgentProvider, Config, MatrixAgentConfig, McpClientConfig, MemoryConfig,
        ProsodyTomlConfig, TmuxConfig, VoiceConfig, VoiceTtsConfig, VoiceTtsSettingsConfig,
        is_secret_ref, parse_secretctl_many_output, resolve_secret_string, validate_fallback_models,
        validate_voice,
    };

    #[test]
    fn debug_redacts_access_token() {
        let config = MatrixAgentConfig {
            homeserver: "https://matrix.example.org".to_owned(),
            user: "@bot:example.org".to_owned(),
            access_token: "secret-token".to_owned(),
            device_id: "DEVICE".to_owned(),
            allowed_users: Vec::new(),
            restricted_tools: None,
            loose_policy: false,
        };

        let debug = format!("{config:?}");

        assert!(debug.contains("[REDACTED]"));
        assert!(!debug.contains("secret-token"));
    }

    #[test]
    fn voice_tts_settings_validate_ranges() {
        let mut voice = VoiceConfig {
            enabled: true,
            audio_store_path: None,
            audio_max_bytes: None,
            retention_ttl_secs: None,
            prosody: ProsodyTomlConfig::default(),
            audio: None,
            tts: Some(VoiceTtsConfig {
                enabled: true,
                api_key: Some("xi-key".to_owned()),
                api_base: None,
                model: "eleven_multilingual_v2".to_owned(),
                voice: "voice-id".to_owned(),
                format: TtsAudioFormat::Mp3,
                forced_alignment: true,
                settings: Some(VoiceTtsSettingsConfig {
                    stability: Some(0.35),
                    similarity_boost: Some(0.75),
                    style: Some(0.55),
                    speed: Some(1.0),
                    use_speaker_boost: Some(true),
                }),
            }),
            divergence: None,
            speaker_id: None,
            circuit: None,
        };

        validate_voice(Some(&voice), None).expect("valid TTS settings");

        voice.tts.as_mut().expect("tts").settings = Some(VoiceTtsSettingsConfig {
            speed: Some(1.5),
            ..VoiceTtsSettingsConfig::default()
        });
        let err = validate_voice(Some(&voice), None).expect_err("invalid speed");

        assert!(
            err.to_string()
                .contains("[voice.tts.settings].speed must be between 0.7 and 1.2")
        );
    }

    #[test]
    fn fallback_models_reject_disabled_anthropic_ids() {
        let mut agent = Agent {
            name: "nova".to_owned(),
            bot_token: Some("token".to_owned()),
            bot_user_id: Some("1".to_owned()),
            bot_username: Some("nova".to_owned()),
            pair_token: Some("pair".to_owned()),
            matrix: None,
            model: "gpt-5.5".to_owned(),
            system_prompt: "prompt".to_owned(),
            max_turns: None,
            holidays_country: None,
            holidays_subdivision: None,
            provider: AgentProvider::OpenAi,
            fallback_models: vec!["gpt-5.4-mini".to_owned()],
            mcp_bearer_token: None,
            tui_bearer_token: None,
            reasoning_effort: None,
            web_search: false,
            web_search_max_uses: None,
            tool_compact: false,
            tmux: TmuxConfig::default(),
        };

        validate_fallback_models(&agent).expect("openai fallback is valid");

        agent.fallback_models = vec!["claude-haiku-4-5-20251001".to_owned()];
        let err = validate_fallback_models(&agent).expect_err("anthropic fallback is rejected");

        assert!(err.to_string().contains("uses disabled Anthropic provider"));
    }

    #[test]
    fn local_only_agent_is_valid() {
        let cfg = Config {
            sqlite_path: "data/crabgent.sqlite".into(),
            openai: None,
            cortecs: None,
            google: None,
            agents: vec![Agent {
                name: "local".to_owned(),
                bot_token: None,
                bot_user_id: None,
                bot_username: None,
                pair_token: None,
                matrix: None,
                model: "gpt-5.5".to_owned(),
                system_prompt: "prompt".to_owned(),
                max_turns: None,
                holidays_country: None,
                holidays_subdivision: None,
                provider: AgentProvider::OpenAi,
                fallback_models: Vec::new(),
                mcp_bearer_token: None,
                tui_bearer_token: Some("token".to_owned()),
                reasoning_effort: None,
                web_search: false,
                web_search_max_uses: None,
                tool_compact: false,
                tmux: TmuxConfig::default(),
            }],
            memory: MemoryConfig::default(),
            stt: None,
            voice: None,
            mcp: McpClientConfig::default(),
            embedding: None,
            mcp_server: None,
            web: None,
        };

        cfg.validate().expect("local-only agent is valid");
    }

    #[test]
    fn secret_refs_are_limited_to_supported_sources() {
        assert!(is_secret_ref("op://Example/crabgent-local/web_auth_token"));
        assert!(is_secret_ref("keychain://service/account"));
        assert!(!is_secret_ref("plain-token"));
        assert!(!is_secret_ref("https://example.test/token"));
    }

    #[test]
    fn secret_string_resolution_keeps_plaintext_and_resolves_refs() {
        let mut calls = Vec::new();

        let mut plain = "plain-token".to_owned();
        resolve_secret_string(&mut plain, "test.plain", &mut |reference| {
            calls.push(reference.to_owned());
            Ok("resolved-secret".to_owned())
        })
        .expect("plain value stays valid");
        assert_eq!(plain, "plain-token");
        assert!(calls.is_empty());

        let mut referenced = "op://Example/crabgent-local/web_auth_token".to_owned();
        resolve_secret_string(&mut referenced, "test.ref", &mut |reference| {
            calls.push(reference.to_owned());
            Ok("resolved-secret".to_owned())
        })
        .expect("secret ref resolves");
        assert_eq!(referenced, "resolved-secret");
        assert_eq!(calls, ["op://Example/crabgent-local/web_auth_token"]);
    }

    #[test]
    fn secretctl_many_output_decodes_hex_secrets() {
        let parsed = parse_secretctl_many_output(
            b"op://Private/item/a\t6f6e655c6e\nop://Private/item/b\t74776f\n",
        )
        .expect("parse");

        assert_eq!(parsed["op://Private/item/a"], "one\\n");
        assert_eq!(parsed["op://Private/item/b"], "two");
    }
}
