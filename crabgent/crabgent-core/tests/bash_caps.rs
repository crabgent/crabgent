use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use crabgent_core::{BashTool, Subject, Tool, ToolCtx, ToolError};
use serde_json::json;
use tempfile::tempdir;
use tokio_util::sync::CancellationToken;

const STDOUT_CAP: usize = 200_000;
const TRUNCATE_MARKER: &str = "\n... [truncated]";

fn ctx() -> ToolCtx {
    ToolCtx::new(Subject::new("u"))
}

#[tokio::test]
async fn stdout_flood_is_capped() {
    let tool = BashTool::new();
    let out = tool
        .execute(
            json!({"command": "yes x | head -c 100000000", "timeout_ms": 5000}),
            &ctx(),
        )
        .await
        .expect("bash run");
    let stdout = out
        .get("stdout")
        .and_then(serde_json::Value::as_str)
        .expect("stdout should be a string");

    assert!(stdout.ends_with(TRUNCATE_MARKER));
    assert!(stdout.len() <= STDOUT_CAP + TRUNCATE_MARKER.len());
}

#[tokio::test]
async fn timeout_kills_background_process_group() {
    let dir = tempdir().expect("tempdir");
    let shell_pid = dir.path().join("shell.pid");
    let background_pid = dir.path().join("background.pid");
    let tool = BashTool::with_default_timeout_ms(200);

    let out = tool
        .execute(
            json!({"command": background_sleep_command(&shell_pid, &background_pid)}),
            &ctx(),
        )
        .await
        .expect("bash timeout returns output");

    assert_eq!(out.get("timed_out"), Some(&json!(true)));
    assert!(
        process_gone_eventually(read_pid(&shell_pid)).await,
        "shell process should exit after BashTool timeout"
    );
    assert!(
        process_gone_eventually(read_pid(&background_pid)).await,
        "background process should exit after BashTool timeout"
    );
}

#[tokio::test]
async fn timeout_sigkills_background_process_after_sigterm_is_ignored() {
    let dir = tempdir().expect("tempdir");
    let shell_pid = dir.path().join("term-ignore-shell.pid");
    let background_pid = dir.path().join("term-ignore-background.pid");
    let tool = BashTool::with_default_timeout_ms(200);

    let out = tool
        .execute(
            json!({"command": sigterm_ignored_background_command(&shell_pid, &background_pid)}),
            &ctx(),
        )
        .await
        .expect("bash timeout returns output");

    assert_eq!(out.get("timed_out"), Some(&json!(true)));
    assert!(
        process_gone_eventually(read_pid(&shell_pid)).await,
        "shell process should exit after SIGKILL fallback"
    );
    assert!(
        process_gone_eventually(read_pid(&background_pid)).await,
        "background process should exit after SIGKILL fallback"
    );
}

#[tokio::test]
async fn cancellation_kills_background_process_group() {
    let dir = tempdir().expect("tempdir");
    let shell_pid = dir.path().join("cancel-shell.pid");
    let background_pid = dir.path().join("cancel-background.pid");
    let tool = BashTool::new();
    let token = CancellationToken::new();
    let token_clone = token.clone();
    let command = background_sleep_command(&shell_pid, &background_pid);
    let task = tokio::spawn(async move {
        let local_ctx = ToolCtx::new(Subject::new("u")).with_cancel(token_clone);
        tool.execute(json!({"command": command}), &local_ctx).await
    });

    let shell = read_pid_eventually(&shell_pid)
        .await
        .expect("shell pid file should be written");
    let background = read_pid_eventually(&background_pid)
        .await
        .expect("background pid file should be written");
    token.cancel();
    let result = task.await.expect("join");

    assert!(matches!(result, Err(ToolError::Cancelled)));
    assert!(
        process_gone_eventually(shell).await,
        "shell process should exit after cancellation"
    );
    assert!(
        process_gone_eventually(background).await,
        "background process should exit after cancellation"
    );
}

fn background_sleep_command(shell_pid: &Path, background_pid: &Path) -> String {
    format!(
        "echo $$ > {}; sleep 60 & echo $! > {}; wait",
        shell_quote(shell_pid),
        shell_quote(background_pid)
    )
}

fn sigterm_ignored_background_command(shell_pid: &Path, background_pid: &Path) -> String {
    format!(
        "echo $$ > {}; (trap '' TERM; exec sleep 60) & echo $! > {}; wait",
        shell_quote(shell_pid),
        shell_quote(background_pid)
    )
}

fn shell_quote(path: &Path) -> String {
    let escaped = path.to_string_lossy().replace('\'', "'\\''");
    format!("'{escaped}'")
}

async fn read_pid_eventually(pid_file: &Path) -> Option<u32> {
    for _ in 0..100 {
        if let Ok(pid) = std::fs::read_to_string(pid_file)
            && let Some(pid) = try_parse_pid(&pid)
        {
            return Some(pid);
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    None
}

fn read_pid(pid_file: &Path) -> u32 {
    parse_pid(&std::fs::read_to_string(pid_file).expect("pid file"))
}

fn try_parse_pid(pid: &str) -> Option<u32> {
    pid.trim().parse().ok()
}

fn parse_pid(pid: &str) -> u32 {
    pid.trim().parse().expect("pid")
}

async fn process_gone_eventually(pid: u32) -> bool {
    for _ in 0..100 {
        if !process_exists(pid) {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    false
}

fn process_exists(pid: u32) -> bool {
    Command::new("kill")
        .arg("-0")
        .arg(pid.to_string())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}
