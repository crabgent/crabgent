//! `OpenAI` `OAuth 2.0` Authorization Code + PKCE flow.
//!
//! Same client ID and endpoints as Codex CLI. Browser-based login
//! with local callback server on port 1455. Adapted from
//! `~/Projects/clawtool-contrib/src/openai/auth.rs` (Apache-2.0),
//! stripped of clawtool vault/keychain integration. File-based
//! persistence at the caller-supplied path.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use crabgent_log::warn;
use crabgent_provider_openai::AuthStrategy;
use crabgent_provider_openai::auth::{CODEX_INSTALLATION_ID, cache_scope_id_from};
use crabgent_provider_openai::wire::WireFormatDyn;
use crabgent_provider_openai::wire::responses::ResponsesWire;
use reqwest::header::{AUTHORIZATION, HeaderName, HeaderValue, USER_AGENT};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};

const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const AUTH_URL: &str = "https://auth.openai.com/oauth/authorize";
const TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
const SCOPES: &str = "openid profile email offline_access";
const ORIGINATOR: &str = "codex_cli_rs";
const CALLBACK_PORT: u16 = 1455;
const CALLBACK_PATH: &str = "/auth/callback";
const ACCESS_TOKEN_EXPIRY_BUFFER_SECONDS: i64 = 300;
const TOKEN_REFRESH_INTERVAL_DAYS: i64 = 8;
const OPENAI_BETA_HEADER: HeaderName = HeaderName::from_static("openai-beta");
const ORIGINATOR_HEADER: HeaderName = HeaderName::from_static("originator");
const CHATGPT_ACCOUNT_ID_HEADER: HeaderName = HeaderName::from_static("chatgpt-account-id");
const SESSION_ID_HEADER_UNDERSCORE: HeaderName = HeaderName::from_static("session_id");
const SESSION_ID_HEADER_DASH: HeaderName = HeaderName::from_static("session-id");
const THREAD_ID_HEADER_UNDERSCORE: HeaderName = HeaderName::from_static("thread_id");
const THREAD_ID_HEADER_DASH: HeaderName = HeaderName::from_static("thread-id");
const CLIENT_REQUEST_ID_HEADER: HeaderName = HeaderName::from_static("x-client-request-id");
const CODEX_WINDOW_ID_HEADER: HeaderName = HeaderName::from_static("x-codex-window-id");
const CODEX_USER_AGENT: &str = "codex_cli_rs/0.59.0";

/// Persisted `OpenAI` OAuth token with refresh capability.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenAiToken {
    pub access_token: String,
    #[serde(default)]
    pub id_token: Option<String>,
    #[serde(default)]
    pub refresh_token: Option<String>,
    #[serde(default)]
    pub account_id: Option<String>,
    #[serde(default)]
    pub expires_at: Option<i64>,
    #[serde(default)]
    pub last_refresh: Option<i64>,
}

#[derive(Debug, Clone)]
pub struct OpenAiTokenSource {
    pub path: PathBuf,
    pub token: OpenAiToken,
}

pub struct RefreshingCodexOAuthAuth {
    path: PathBuf,
    token: std::sync::RwLock<OpenAiToken>,
    refresh_lock: tokio::sync::Mutex<()>,
    wire: ResponsesWire,
}

impl RefreshingCodexOAuthAuth {
    #[must_use]
    pub fn new(source: &OpenAiTokenSource) -> Self {
        Self {
            path: source.path.clone(),
            token: std::sync::RwLock::new(source.token.clone()),
            refresh_lock: tokio::sync::Mutex::new(()),
            wire: ResponsesWire,
        }
    }

    fn current_token(&self) -> OpenAiToken {
        self.token
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    fn replace_token(&self, fresh: OpenAiToken) {
        *self
            .token
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = fresh;
    }
}

#[async_trait]
impl AuthStrategy for RefreshingCodexOAuthAuth {
    fn base_url(&self) -> &'static str {
        "https://chatgpt.com"
    }

    fn auth_headers(&self) -> Vec<(HeaderName, HeaderValue)> {
        let token = self.current_token();
        let mut headers = Vec::with_capacity(5);
        push_bearer_header(&mut headers, &token.access_token);
        push_static_header(&mut headers, OPENAI_BETA_HEADER, "responses=experimental");
        push_static_header(&mut headers, ORIGINATOR_HEADER, ORIGINATOR);
        push_static_header(&mut headers, USER_AGENT, CODEX_USER_AGENT);
        if let Some(account_id) = &token.account_id {
            push_static_header(&mut headers, CHATGPT_ACCOUNT_ID_HEADER, account_id);
        }
        headers
    }

    fn request_headers(&self, ctx: &crabgent_core::RunCtx) -> Vec<(HeaderName, HeaderValue)> {
        let scope = cache_scope_id_from(ctx);
        let mut headers = Vec::with_capacity(6);
        push_static_header(&mut headers, SESSION_ID_HEADER_UNDERSCORE, &scope);
        push_static_header(&mut headers, SESSION_ID_HEADER_DASH, &scope);
        push_static_header(&mut headers, THREAD_ID_HEADER_UNDERSCORE, &scope);
        push_static_header(&mut headers, THREAD_ID_HEADER_DASH, &scope);
        push_static_header(&mut headers, CLIENT_REQUEST_ID_HEADER, &scope);
        push_static_header(&mut headers, CODEX_WINDOW_ID_HEADER, CODEX_INSTALLATION_ID);
        headers
    }

    fn wire(&self) -> &dyn WireFormatDyn {
        &self.wire
    }

    fn supports_model_discovery(&self) -> bool {
        false
    }

    fn stt_endpoint_url(&self) -> String {
        "https://chatgpt.com/backend-api/transcribe".to_owned()
    }

    fn stt_accepts_model_field(&self) -> bool {
        false
    }

    fn stream_only(&self) -> bool {
        true
    }

    fn supports_hosted_web_search(&self) -> bool {
        true
    }

    fn supports_hosted_image_generation(&self) -> bool {
        true
    }

    async fn refresh_after_auth_error(
        &self,
    ) -> std::result::Result<bool, crabgent_core::ProviderError> {
        let _guard = self.refresh_lock.lock().await;
        let current = self.current_token();
        match read_token(&self.path) {
            Ok(Some(disk)) if disk.access_token != current.access_token => {
                self.replace_token(disk);
                return Ok(true);
            }
            Ok(_) => {}
            Err(err) => {
                warn!(error = %err, path = %self.path.display(), "openai token reload after auth failure failed");
            }
        }
        match refresh(&current).await {
            Ok(fresh) => {
                if let Err(err) = write_token(&self.path, &fresh) {
                    warn!(error = %err, path = %self.path.display(), "openai refreshed token persistence failed");
                }
                self.replace_token(fresh);
                Ok(true)
            }
            Err(err) => {
                warn!(error = %err, path = %self.path.display(), "openai token refresh after auth failure failed");
                Ok(false)
            }
        }
    }
}

fn push_bearer_header(headers: &mut Vec<(HeaderName, HeaderValue)>, token: &str) {
    push_static_header(headers, AUTHORIZATION, &format!("Bearer {token}"));
}

fn push_static_header(headers: &mut Vec<(HeaderName, HeaderValue)>, name: HeaderName, value: &str) {
    match HeaderValue::from_str(value) {
        Ok(value) => headers.push((name, value)),
        Err(err) => warn!(
            header = %name,
            reason = %err,
            "openai header value rejected, dropping"
        ),
    }
}

impl OpenAiToken {
    fn is_expired(&self) -> bool {
        self.expires_at.is_some_and(|exp| {
            chrono::Utc::now().timestamp() >= exp - ACCESS_TOKEN_EXPIRY_BUFFER_SECONDS
        })
    }

    fn needs_proactive_refresh(&self) -> bool {
        if self.is_expired() {
            return true;
        }
        self.last_refresh.is_some_and(|last| {
            last < chrono::Utc::now().timestamp() - TOKEN_REFRESH_INTERVAL_DAYS * 24 * 60 * 60
        })
    }
}

/// Default token path under XDG config home.
pub fn default_token_path() -> Result<PathBuf> {
    let home = dirs_home().context("resolve $HOME")?;
    Ok(home
        .join(".config")
        .join(crate::brand::app_config_name())
        .join("credentials")
        .join("openai_oauth_token"))
}

fn dirs_home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

/// Read the cached token from disk. Returns `None` when the file is
/// missing.
pub fn read_token(path: &Path) -> Result<Option<OpenAiToken>> {
    match std::fs::read_to_string(path) {
        Ok(raw) => {
            let token: OpenAiToken = serde_json::from_str(&raw)
                .with_context(|| format!("parse openai token at {}", path.display()))?;
            Ok(Some(token))
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err).with_context(|| format!("read openai token at {}", path.display())),
    }
}

/// Atomic write of token JSON to `path` with 0o600 permissions and a
/// 0o700 parent directory. Replaces any existing file.
pub fn write_token(path: &Path, token: &OpenAiToken) -> Result<()> {
    use std::fs;
    use std::io::Write as _;

    let parent = path
        .parent()
        .context("openai token path has no parent directory")?;
    fs::create_dir_all(parent)
        .with_context(|| format!("create token directory {}", parent.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(parent, fs::Permissions::from_mode(0o700))
            .with_context(|| format!("set 0o700 permissions on {}", parent.display()))?;
    }

    let json = serde_json::to_string_pretty(token)?;
    let tmp = path.with_extension("tmp");
    let mut opts = fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut file = opts
        .open(&tmp)
        .with_context(|| format!("open token tmp {}", tmp.display()))?;
    file.write_all(json.as_bytes())
        .with_context(|| format!("write token tmp {}", tmp.display()))?;
    file.sync_all().ok();
    drop(file);
    fs::rename(&tmp, path)
        .with_context(|| format!("rename token {} -> {}", tmp.display(), path.display()))?;
    Ok(())
}

/// Load cached token, refresh proactively if near expiry, persist
/// updated token. Returns `Ok(None)` if no token is cached or the
/// refresh fails permanently.
pub async fn load_or_refresh(path: &Path) -> Result<Option<OpenAiToken>> {
    let Some(token) = read_token(path)? else {
        return Ok(None);
    };
    if !token.needs_proactive_refresh() {
        return Ok(Some(token));
    }
    if token.refresh_token.is_some() {
        match refresh(&token).await {
            Ok(fresh) => {
                if let Err(err) = write_token(path, &fresh) {
                    warn!(error = %err, "openai token refresh persistence failed");
                }
                return Ok(Some(fresh));
            }
            Err(err) => {
                warn!(error = %err, "openai token refresh failed; user must re-login");
                return Ok(None);
            }
        }
    }
    Ok(None)
}

/// Run the interactive browser-based OAuth login flow. Binds a local
/// HTTP server on port 1455 for the OAuth callback, opens the user's
/// default browser, waits up to 120s for the redirect.
pub async fn login() -> Result<OpenAiToken> {
    let redirect_uri = format!("http://localhost:{CALLBACK_PORT}{CALLBACK_PATH}");
    let (verifier, challenge) = pkce_challenge();
    let state = uuid::Uuid::new_v4().simple().to_string();

    let auth_url = format!(
        "{AUTH_URL}?\
         response_type=code\
         &client_id={}\
         &redirect_uri={}\
         &scope={}\
         &code_challenge={}\
         &code_challenge_method=S256\
         &state={}\
         &id_token_add_organizations=true\
         &codex_cli_simplified_flow=true\
         &originator={}\
         &prompt=login",
        pct_encode(CLIENT_ID),
        pct_encode(&redirect_uri),
        pct_encode(SCOPES),
        pct_encode(&challenge),
        pct_encode(&state),
        pct_encode(ORIGINATOR),
    );

    let listener = tokio::net::TcpListener::bind(format!("127.0.0.1:{CALLBACK_PORT}"))
        .await
        .with_context(|| format!("port {CALLBACK_PORT} already in use (Codex CLI running?)"))?;

    let (tx, rx) = tokio::sync::oneshot::channel::<String>();
    let tx = Arc::new(tokio::sync::Mutex::new(Some(tx)));
    let expected_state = state.clone();

    let app = axum::Router::new().route(
        CALLBACK_PATH,
        axum::routing::get({
            let tx = Arc::clone(&tx);
            move |query: axum::extract::Query<CallbackQuery>| {
                let tx = Arc::clone(&tx);
                let expected = expected_state.clone();
                async move {
                    if query.state.as_deref() != Some(expected.as_str()) {
                        return axum::response::Html("<h2>Error: state mismatch</h2>".to_owned());
                    }
                    if let Some(ref err) = query.error {
                        return axum::response::Html(format!(
                            "<h2>Error: {err}</h2><p>{}</p>",
                            query.error_description.as_deref().unwrap_or("")
                        ));
                    }
                    let code = query.code.clone().unwrap_or_default();
                    let maybe_sender = tx.lock().await.take();
                    if let Some(sender) = maybe_sender {
                        sender.send(code).ok();
                    }
                    axum::response::Html(
                        "<h2>OK</h2><p>Login successful. Close this tab.</p>".to_owned(),
                    )
                }
            }
        }),
    );

    let server_handle = tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });

    eprintln!();
    eprintln!("  Opening browser for OpenAI login...");
    eprintln!("  URL: {auth_url}");
    eprintln!();
    open_browser(&auth_url);

    let code = tokio::time::timeout(std::time::Duration::from_mins(2), rx)
        .await
        .context("OAuth timeout (120s)")?
        .context("callback channel closed")?;

    server_handle.abort();

    exchange_code(&code, &verifier, &redirect_uri).await
}

async fn exchange_code(code: &str, verifier: &str, redirect_uri: &str) -> Result<OpenAiToken> {
    let resp = reqwest::Client::new()
        .post(TOKEN_URL)
        .form(&[
            ("grant_type", "authorization_code"),
            ("client_id", CLIENT_ID),
            ("code", code),
            ("redirect_uri", redirect_uri),
            ("code_verifier", verifier),
        ])
        .send()
        .await
        .context("token exchange failed")?;

    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        bail!("token exchange returned {status}: {text}");
    }
    parse_token_response(resp, None).await
}

/// Refresh an expired access token using the cached `refresh_token`.
pub async fn refresh(current: &OpenAiToken) -> Result<OpenAiToken> {
    let refresh_token = current
        .refresh_token
        .as_deref()
        .context("missing refresh_token")?;

    let resp = reqwest::Client::new()
        .post(TOKEN_URL)
        .json(&json!({
            "grant_type": "refresh_token",
            "client_id": CLIENT_ID,
            "refresh_token": refresh_token,
        }))
        .send()
        .await
        .context("token refresh failed")?;

    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        bail!("token refresh returned {status}: {text}");
    }
    parse_token_response(resp, Some(current)).await
}

async fn parse_token_response(
    resp: reqwest::Response,
    previous: Option<&OpenAiToken>,
) -> Result<OpenAiToken> {
    let body: serde_json::Value = resp.json().await.context("parse token response body")?;
    let now = chrono::Utc::now().timestamp();

    let access_token = body
        .get("access_token")
        .and_then(serde_json::Value::as_str)
        .map(String::from)
        .or_else(|| previous.map(|t| t.access_token.clone()))
        .context("missing access_token")?;

    let id_token = body
        .get("id_token")
        .and_then(serde_json::Value::as_str)
        .map(String::from)
        .or_else(|| previous.and_then(|t| t.id_token.clone()));

    let expires_at = body
        .get("expires_in")
        .and_then(serde_json::Value::as_i64)
        .map(|s| now + s)
        .or_else(|| parse_jwt_expiration(&access_token))
        .or_else(|| previous.and_then(|t| t.expires_at));

    let refresh_token = body
        .get("refresh_token")
        .and_then(serde_json::Value::as_str)
        .map(String::from)
        .or_else(|| previous.and_then(|t| t.refresh_token.clone()));

    let account_id = account_id_from_tokens(id_token.as_deref(), &access_token)
        .or_else(|| previous.and_then(|t| t.account_id.clone()));

    Ok(OpenAiToken {
        access_token,
        id_token,
        refresh_token,
        expires_at,
        account_id,
        last_refresh: Some(now),
    })
}

fn account_id_from_tokens(id_token: Option<&str>, access_token: &str) -> Option<String> {
    id_token
        .and_then(extract_account_id)
        .or_else(|| extract_account_id(access_token))
}

fn extract_account_id(token: &str) -> Option<String> {
    let claims = decode_jwt_payload(token)?;
    claims
        .get("https://api.openai.com/auth")
        .and_then(|auth| auth["chatgpt_account_id"].as_str())
        .map(String::from)
}

fn parse_jwt_expiration(token: &str) -> Option<i64> {
    decode_jwt_payload(token).and_then(|claims| claims.get("exp")?.as_i64())
}

fn decode_jwt_payload(token: &str) -> Option<serde_json::Value> {
    let mut parts = token.split('.');
    let (_header, payload, _sig) = match (parts.next(), parts.next(), parts.next()) {
        (Some(h), Some(p), Some(s)) if !h.is_empty() && !p.is_empty() && !s.is_empty() => (h, p, s),
        _ => return None,
    };
    let bytes = URL_SAFE_NO_PAD.decode(payload).ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn pkce_challenge() -> (String, String) {
    let verifier = format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple(),
    );
    let digest = Sha256::digest(verifier.as_bytes());
    let challenge = URL_SAFE_NO_PAD.encode(digest);
    (verifier, challenge)
}

#[derive(Debug, Deserialize)]
struct CallbackQuery {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
    error_description: Option<String>,
}

fn pct_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(char::from(b));
        } else {
            out.push('%');
            out.push(char::from(HEX[usize::from(b >> 4)]));
            out.push(char::from(HEX[usize::from(b & 0xf)]));
        }
    }
    out
}

const HEX: [u8; 16] = *b"0123456789ABCDEF";

fn open_browser(url: &str) {
    #[cfg(target_os = "macos")]
    let cmd = "open";
    #[cfg(not(target_os = "macos"))]
    let cmd = "xdg-open";
    std::process::Command::new(cmd).arg(url).spawn().ok();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn jwt(payload: &serde_json::Value) -> String {
        let header = URL_SAFE_NO_PAD.encode(br#"{"alg":"none"}"#);
        let body = URL_SAFE_NO_PAD.encode(payload.to_string().as_bytes());
        format!("{header}.{body}.sig")
    }

    #[test]
    fn extract_account_id_from_jwt() {
        let token = jwt(&json!({
            "https://api.openai.com/auth": { "chatgpt_account_id": "acc-1" }
        }));
        assert_eq!(extract_account_id(&token).as_deref(), Some("acc-1"));
    }

    #[test]
    fn parse_jwt_expiration_extracts_exp_claim() {
        let token = jwt(&json!({ "exp": 1_700_000_000_i64 }));
        assert_eq!(parse_jwt_expiration(&token), Some(1_700_000_000));
    }

    #[test]
    fn token_needs_refresh_when_past_expiry() {
        let token = OpenAiToken {
            access_token: String::new(),
            id_token: None,
            refresh_token: None,
            account_id: None,
            expires_at: Some(chrono::Utc::now().timestamp() - 1),
            last_refresh: None,
        };
        assert!(token.needs_proactive_refresh());
    }

    #[test]
    fn token_not_refreshing_when_fresh() {
        let token = OpenAiToken {
            access_token: String::new(),
            id_token: None,
            refresh_token: None,
            account_id: None,
            expires_at: Some(chrono::Utc::now().timestamp() + 24 * 60 * 60),
            last_refresh: Some(chrono::Utc::now().timestamp()),
        };
        assert!(!token.needs_proactive_refresh());
    }

    #[test]
    fn write_read_roundtrip() {
        let dir = tempdir();
        let path = dir.join("openai_oauth_token");
        let token = OpenAiToken {
            access_token: "at".into(),
            id_token: Some("id".into()),
            refresh_token: Some("rt".into()),
            account_id: Some("acc".into()),
            expires_at: Some(123),
            last_refresh: Some(456),
        };
        write_token(&path, &token).expect("write");
        let read = read_token(&path).expect("read").expect("token");
        assert_eq!(read.access_token, "at");
        assert_eq!(read.account_id.as_deref(), Some("acc"));
    }

    fn tempdir() -> PathBuf {
        let id = uuid::Uuid::new_v4().simple().to_string();
        let p = std::env::temp_dir().join(format!("crabgent-oauth-test-{id}"));
        std::fs::create_dir_all(&p).unwrap();
        p
    }
}
