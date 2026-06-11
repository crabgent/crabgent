//! Optional local tmux tool for trusted single-user agents.
//!
//! This is deliberately not a channel adapter. Tmux is an explicit tool an
//! agent can use when the user asks it to inspect or post into a tmux window.
//! Normal agent/user notification uses Matrix, Telegram, or the in-process TUI
//! channel.
//!
//! The send path mirrors the tmux-pair plugin's escape recipe so that agent
//! TUIs reliably accept the message even while busy with a tool call:
//!
//! - Multi-line payloads go through `load-buffer` + `paste-buffer -d`
//!   so bracketed-paste suppresses per-newline submit.
//! - Single-line payloads go through `send-keys -l` (literal).
//! - After paste, wait for either the paste marker (`Pasted text` /
//!   text-probe) or a settle window, then submit with the `Enter`
//!   keysym (NOT `C-m`: claude-code distinguishes the two and
//!   sometimes swallows `C-m` after multi-line pastes).
//! - Submit is a burst of three `Enter` keys with retries until the
//!   composer shows the idle prompt (`❯` / `›`) or the
//!   `esc to interrupt` spinner footer.
//!
//! `read` returns the current pane buffer via `tmux capture-pane -p -S -N`.

use std::process::Stdio;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use crabgent_core::{Subject, Tool, ToolCtx, ToolError};
use crabgent_log::{debug, warn};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tokio::time::sleep;

const DEFAULT_WINDOW: &str = "main";
const DEFAULT_READ_LINES: usize = 100;
const MAX_READ_LINES: usize = 5_000;
const PASTE_SETTLE_MULTILINE: Duration = Duration::from_millis(1500);
const PASTE_SETTLE_SINGLE: Duration = Duration::from_millis(300);
const PASTE_RENDER_DEADLINE: Duration = Duration::from_secs(5);
const SUBMIT_BURST: usize = 3;
const SUBMIT_BURST_SPACING: Duration = Duration::from_millis(400);
const SUBMIT_MAX_ITER: usize = 20;
const SUBMIT_WAIT_INITIAL: Duration = Duration::from_millis(1500);
const SUBMIT_WAIT_CAP: Duration = Duration::from_secs(4);

pub struct TmuxTool {
    default_window: String,
}

impl TmuxTool {
    pub fn new(window: impl Into<String>) -> Self {
        Self {
            default_window: window.into(),
        }
    }
}

impl Default for TmuxTool {
    fn default() -> Self {
        Self::new(DEFAULT_WINDOW)
    }
}

#[async_trait]
impl Tool for TmuxTool {
    fn name(&self) -> &'static str {
        "tmux"
    }

    fn description(&self) -> &'static str {
        "Inspect or post into a local tmux window. Trusted local operational tool. \
         Use only when the user explicitly asks to look at or send something \
         into tmux. Args: `op` is `list`, `read`, or `send`; `window` is the \
         tmux window name; `lines` caps read output; `body` is \
         required for send. Do not use this for normal user notification, TUI \
         delivery, Matrix replies, background-task completion, or automatic \
         delegation."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "op": {
                    "type": "string",
                    "enum": ["list", "read", "send"]
                },
                "window": {
                    "type": ["string", "null"],
                    "description": "tmux window name. Defaults to the configured window for read/send."
                },
                "lines": {
                    "type": ["integer", "null"],
                    "minimum": 1,
                    "maximum": MAX_READ_LINES,
                    "description": "Number of scrollback lines for op=read. Default 100."
                },
                "body": {
                    "type": ["string", "null"],
                    "description": "Text to paste and submit for op=send."
                }
            },
            "required": ["op"]
        })
    }

    async fn execute(&self, args: Value, ctx: &ToolCtx) -> Result<Value, ToolError> {
        let args: TmuxArgs =
            serde_json::from_value(args).map_err(|err| ToolError::InvalidArgs(err.to_string()))?;
        match args.op.as_str() {
            "list" => Ok(json!({ "windows": list_windows().await? })),
            "read" => {
                let window = args.window.as_deref().unwrap_or(&self.default_window);
                let pane = find_pane_for_window(window).await?.ok_or_else(|| {
                    ToolError::NotFound(format!("tmux window {window:?} not found"))
                })?;
                let lines = args
                    .lines
                    .unwrap_or(DEFAULT_READ_LINES)
                    .clamp(1, MAX_READ_LINES);
                let body = capture_pane(&pane, lines).await?;
                Ok(json!({
                    "window": window,
                    "pane": pane,
                    "lines": lines,
                    "body": body
                }))
            }
            "send" => {
                let window = args.window.as_deref().unwrap_or(&self.default_window);
                let body = args
                    .body
                    .as_deref()
                    .filter(|body| !body.trim().is_empty())
                    .ok_or_else(|| ToolError::InvalidArgs("body is required for send".into()))?;
                let payload = identity_wrap(&ctx.subject, body);
                deliver(window, &payload).await?;
                Ok(json!({
                    "window": window,
                    "sent": true,
                    "submitted": true
                }))
            }
            other => Err(ToolError::InvalidArgs(format!(
                "unsupported tmux op {other:?}"
            ))),
        }
    }
}

#[derive(Debug, Deserialize)]
struct TmuxArgs {
    op: String,
    window: Option<String>,
    lines: Option<usize>,
    body: Option<String>,
}

async fn capture_pane(pane: &str, lines: usize) -> Result<String, ToolError> {
    let start = format!("-{lines}");
    let output = Command::new("tmux")
        .args(["capture-pane", "-t", pane, "-p", "-J", "-S", start.as_str()])
        .output()
        .await
        .map_err(|e| ToolError::Io(format!("tmux capture-pane spawn: {e}")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        return Err(ToolError::Io(format!(
            "tmux capture-pane failed: status={} stderr={stderr}",
            output.status
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn identity_wrap(ctx: &Subject, body: &str) -> String {
    if body.starts_with("[FROM:") {
        return body.to_owned();
    }
    let id = ctx.id();
    let agent = ctx.attr("agent").unwrap_or_else(|| {
        // Pre-channel subjects ("agent:local") and bare ids ("nova") have
        // no `agent` attr; fall back to the subject id stripped of the
        // legacy "agent:" prefix so the marker is never just "agent".
        id.strip_prefix("agent:")
            .unwrap_or(if id.is_empty() { "agent" } else { id })
    });
    let mut header = format!("[FROM: {agent}");
    if let Some(channel) = ctx.attr("channel") {
        header.push('@');
        header.push_str(channel);
    }
    if let Some(participant) = ctx.attr("participant_id") {
        header.push_str(" user=");
        header.push_str(participant);
    }
    if let Some(conv) = ctx.attr("conv") {
        header.push_str(" conv=");
        header.push_str(conv);
    }
    header.push(']');
    format!("{header} {body}")
}

async fn deliver(window: &str, payload: &str) -> Result<(), ToolError> {
    let pane = find_pane_for_window(window)
        .await?
        .ok_or_else(|| ToolError::NotFound(format!("tmux window {window:?} not found")))?;
    debug!(window, pane = %pane, "tmux: resolved pane");
    paste_payload(&pane, payload).await?;
    submit_with_retry(&pane).await;
    Ok(())
}

async fn find_pane_for_window(window: &str) -> Result<Option<String>, ToolError> {
    Ok(list_windows()
        .await?
        .into_iter()
        .find(|entry| entry.window == window)
        .map(|entry| entry.pane))
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
struct TmuxWindow {
    pane: String,
    window: String,
}

async fn list_windows() -> Result<Vec<TmuxWindow>, ToolError> {
    let output = Command::new("tmux")
        .args(["list-panes", "-a", "-F", "#{pane_id} #{window_name}"])
        .output()
        .await
        .map_err(|e| ToolError::Io(format!("tmux list-panes spawn: {e}")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        return Err(ToolError::Io(format!(
            "tmux list-panes failed: status={} stderr={stderr}",
            output.status
        )));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut out = Vec::new();
    for line in stdout.lines() {
        let mut parts = line.splitn(2, char::is_whitespace);
        let Some(pane) = parts.next() else { continue };
        let Some(name) = parts.next() else { continue };
        out.push(TmuxWindow {
            pane: pane.trim().to_owned(),
            window: name.trim().to_owned(),
        });
    }
    Ok(out)
}

async fn paste_payload(pane: &str, payload: &str) -> Result<(), ToolError> {
    if payload.contains('\n') {
        paste_multiline(pane, payload).await?;
        wait_paste_rendered(pane, payload).await;
        sleep(PASTE_SETTLE_MULTILINE).await;
    } else {
        send_keys_literal(pane, payload).await?;
        sleep(PASTE_SETTLE_SINGLE).await;
    }
    Ok(())
}

async fn paste_multiline(pane: &str, payload: &str) -> Result<(), ToolError> {
    let buf = format!(
        "crabgent-tmux-{pid}-{ts}",
        pid = std::process::id(),
        ts = Instant::now().elapsed().as_nanos() ^ rand_seed(),
    );
    let mut child = Command::new("tmux")
        .args(["load-buffer", "-b", buf.as_str(), "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| ToolError::Io(format!("tmux load-buffer spawn: {e}")))?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(payload.as_bytes())
            .await
            .map_err(|e| ToolError::Io(format!("tmux load-buffer stdin: {e}")))?;
        drop(stdin);
    }
    let status = child
        .wait()
        .await
        .map_err(|e| ToolError::Io(format!("tmux load-buffer wait: {e}")))?;
    if !status.success() {
        return Err(ToolError::Io(format!(
            "tmux load-buffer failed: status={status}"
        )));
    }
    let paste = Command::new("tmux")
        .args(["paste-buffer", "-b", buf.as_str(), "-t", pane, "-d"])
        .output()
        .await
        .map_err(|e| ToolError::Io(format!("tmux paste-buffer spawn: {e}")))?;
    if !paste.status.success() {
        let stderr = String::from_utf8_lossy(&paste.stderr).trim().to_owned();
        return Err(ToolError::Io(format!(
            "tmux paste-buffer failed: status={} stderr={stderr}",
            paste.status
        )));
    }
    Ok(())
}

fn rand_seed() -> u128 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    std::process::id().hash(&mut hasher);
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos())
        .hash(&mut hasher);
    u128::from(hasher.finish())
}

async fn send_keys_literal(pane: &str, payload: &str) -> Result<(), ToolError> {
    let output = Command::new("tmux")
        .args(["send-keys", "-t", pane, "-l", payload])
        .output()
        .await
        .map_err(|e| ToolError::Io(format!("tmux send-keys spawn: {e}")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        return Err(ToolError::Io(format!(
            "tmux send-keys failed: status={} stderr={stderr}",
            output.status
        )));
    }
    Ok(())
}

async fn wait_paste_rendered(pane: &str, payload: &str) {
    let deadline = Instant::now() + PASTE_RENDER_DEADLINE;
    let probe: String = payload
        .split('\n')
        .find(|line| !line.trim().is_empty())
        .unwrap_or("")
        .chars()
        .take(40)
        .collect();
    while Instant::now() < deadline {
        let tail = capture_tail(pane, 10).await;
        if tail.contains("Pasted text") || (!probe.is_empty() && tail.contains(&probe)) {
            return;
        }
        sleep(Duration::from_millis(200)).await;
    }
}

async fn submit_with_retry(pane: &str) {
    for attempt in 0..SUBMIT_MAX_ITER {
        for _ in 0..SUBMIT_BURST {
            let _ = Command::new("tmux")
                .args(["send-keys", "-t", pane, "Enter"])
                .output()
                .await;
            sleep(SUBMIT_BURST_SPACING).await;
        }
        let wait = SUBMIT_WAIT_INITIAL + Duration::from_millis(300 * attempt as u64);
        sleep(wait.min(SUBMIT_WAIT_CAP)).await;
        let tail = capture_tail(pane, 12).await;
        if composer_settled(&tail) {
            return;
        }
    }
    warn!(
        pane,
        "tmux: composer still busy after {SUBMIT_MAX_ITER} submit bursts; \
         message may not have been accepted"
    );
}

async fn capture_tail(pane: &str, lines: usize) -> String {
    let lower = format!("-{lines}");
    match Command::new("tmux")
        .args(["capture-pane", "-t", pane, "-p", "-S", lower.as_str()])
        .output()
        .await
    {
        Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout).into_owned(),
        Ok(out) => {
            let stderr = String::from_utf8_lossy(&out.stderr).trim().to_owned();
            warn!(pane, status = %out.status, stderr, "tmux: capture-pane failed");
            String::new()
        }
        Err(err) => {
            warn!(pane, error = %err, "tmux: capture-pane spawn failed");
            String::new()
        }
    }
}

fn composer_settled(tail: &str) -> bool {
    let lowered = tail.to_lowercase();
    if lowered.contains("esc to interrupt") {
        return true;
    }
    for line in tail.lines() {
        let stripped = line
            .trim()
            .trim_matches(|c: char| matches!(c, '│' | '┃' | '▌' | '▐' | '▏' | '▕' | '|' | ' '));
        if stripped == "❯" || stripped == "›" {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_wrap_adds_prefix_from_subject_id() {
        let s = Subject::new("nova");
        let out = identity_wrap(&s, "hi");
        assert_eq!(out, "[FROM: nova] hi");
    }

    #[test]
    fn identity_wrap_strips_agent_namespace() {
        let s = Subject::new("agent:delta");
        let out = identity_wrap(&s, "ping");
        assert_eq!(out, "[FROM: delta] ping");
    }

    #[test]
    fn identity_wrap_keeps_existing_prefix() {
        let s = Subject::new("nova");
        let out = identity_wrap(&s, "[FROM: upstream] body");
        assert_eq!(out, "[FROM: upstream] body");
    }

    #[test]
    fn identity_wrap_includes_channel_user_and_conv_for_matrix_subject() {
        let s = Subject::new("matrix:@local%3Aserver")
            .with_attr("agent", "local")
            .with_attr("channel", "matrix")
            .with_attr("participant_id", "@alice:example.org")
            .with_attr("conv", "matrix:!room:server");
        let out = identity_wrap(&s, "ping");
        assert_eq!(
            out,
            "[FROM: local@matrix user=@alice:example.org conv=matrix:!room:server] ping"
        );
    }

    #[test]
    fn identity_wrap_uses_agent_attr_over_subject_id() {
        let s = Subject::new("matrix:@local%3Aserver").with_attr("agent", "local");
        let out = identity_wrap(&s, "hi");
        assert_eq!(out, "[FROM: local] hi");
    }

    #[test]
    fn tmux_tool_schema_exposes_list_read_send() {
        let schema = TmuxTool::default().parameters_schema();
        assert_eq!(schema["properties"]["op"]["enum"][0], "list");
        assert_eq!(schema["properties"]["op"]["enum"][1], "read");
        assert_eq!(schema["properties"]["op"]["enum"][2], "send");
    }

    #[test]
    fn tmux_tool_description_rejects_notification_delivery() {
        let description = TmuxTool::default().description();
        assert!(description.contains("explicitly asks"));
        assert!(description.contains("Do not use this for normal user notification"));
    }

    #[test]
    fn composer_settled_detects_claude_prompt() {
        assert!(composer_settled("│ ❯ │"));
        assert!(composer_settled("  ❯  "));
    }

    #[test]
    fn composer_settled_detects_codex_prompt() {
        assert!(composer_settled("  ›  "));
    }

    #[test]
    fn composer_settled_detects_esc_interrupt_footer() {
        assert!(composer_settled("foo\nbar (esc to interrupt) baz"));
    }

    #[test]
    fn composer_settled_false_when_no_marker() {
        assert!(!composer_settled("nothing relevant here"));
    }
}
