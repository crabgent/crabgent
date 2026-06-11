//! Spawn the configured command, write the JSON envelope on its stdin,
//! read one JSON line from stdout, and terminate the child after either
//! a decision or a timeout. The script does the actual hook work; this
//! module is the IPC plumbing.

use std::process::Stdio;
use std::time::Duration;

use crabgent_log::warn;
use serde_json::Value;
use thiserror::Error;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::time::timeout;

use crate::proto::HookOutput;

const CHILD_EXIT_GRACE: Duration = Duration::from_secs(1);

#[derive(Debug, Error)]
pub enum RunnerError {
    #[error("empty command")]
    EmptyCommand,
    #[error("spawn failed: {0}")]
    Spawn(#[source] std::io::Error),
    #[error("subprocess stdio not captured")]
    StdioMissing,
    #[error("write to subprocess stdin failed: {0}")]
    WriteStdin(#[source] std::io::Error),
    #[error("read from subprocess stdout failed: {0}")]
    ReadStdout(#[source] std::io::Error),
    #[error("subprocess exited without writing JSON")]
    EmptyStdout,
    #[error("subprocess output is not valid JSON: {0}")]
    BadJson(#[source] serde_json::Error),
    #[error("subprocess timed out after {0:?}")]
    Timeout(Duration),
}

pub async fn run(
    cmd: &[String],
    input: &Value,
    call_timeout: Duration,
) -> Result<HookOutput, RunnerError> {
    let (program, args) = cmd.split_first().ok_or(RunnerError::EmptyCommand)?;
    let payload = serde_json::to_vec(input).map_err(RunnerError::BadJson)?;

    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(RunnerError::Spawn)?;

    let stdin = child.stdin.take().ok_or(RunnerError::StdioMissing)?;
    let stdout = child.stdout.take().ok_or(RunnerError::StdioMissing)?;

    if let Ok(res) = timeout(call_timeout, pump_stdio(stdin, stdout, payload)).await {
        terminate_child(&mut child).await;
        res
    } else {
        terminate_child(&mut child).await;
        Err(RunnerError::Timeout(call_timeout))
    }
}

async fn pump_stdio(
    mut stdin: ChildStdin,
    stdout: ChildStdout,
    payload: Vec<u8>,
) -> Result<HookOutput, RunnerError> {
    stdin
        .write_all(&payload)
        .await
        .map_err(RunnerError::WriteStdin)?;
    stdin
        .write_all(b"\n")
        .await
        .map_err(RunnerError::WriteStdin)?;
    stdin.shutdown().await.map_err(RunnerError::WriteStdin)?;
    drop(stdin);

    let mut reader = BufReader::new(stdout);
    let mut line = String::new();
    let read = reader
        .read_line(&mut line)
        .await
        .map_err(RunnerError::ReadStdout)?;
    if read == 0 {
        return Err(RunnerError::EmptyStdout);
    }
    serde_json::from_str(line.trim()).map_err(RunnerError::BadJson)
}

async fn terminate_child(child: &mut Child) {
    if child_already_exited(child) {
        return;
    }
    start_child_kill(child);
    wait_after_kill(child).await;
}

fn child_already_exited(child: &mut Child) -> bool {
    match child.try_wait() {
        Ok(Some(_)) => true,
        Ok(None) => false,
        Err(e) => {
            warn!(error = %e, "subprocess try_wait failed before termination");
            false
        }
    }
}

fn start_child_kill(child: &mut Child) {
    if let Err(e) = child.start_kill() {
        warn!(error = %e, "subprocess start_kill failed");
    }
}

async fn wait_after_kill(child: &mut Child) {
    let result = timeout(CHILD_EXIT_GRACE, child.wait()).await;
    log_wait_after_kill(result);
}

fn log_wait_after_kill(
    result: Result<Result<std::process::ExitStatus, std::io::Error>, tokio::time::error::Elapsed>,
) {
    if let Ok(wait_result) = result {
        log_wait_result(wait_result);
    } else {
        warn!(grace = ?CHILD_EXIT_GRACE, "subprocess did not exit after kill");
    }
}

fn log_wait_result(result: Result<std::process::ExitStatus, std::io::Error>) {
    if let Err(e) = result {
        warn!(error = %e, "subprocess wait failed after termination");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tokio::time::Instant;

    fn sh(script: &str) -> Vec<String> {
        vec!["sh".into(), "-c".into(), script.into()]
    }

    #[tokio::test]
    async fn returns_continue_when_subprocess_echoes_continue() {
        let cmd = sh(r#"cat > /dev/null; printf '{"decision":"continue"}\n'"#);
        let out = run(&cmd, &json!({}), Duration::from_secs(2))
            .await
            .expect("run ok");
        assert!(matches!(out, HookOutput::Continue));
    }

    #[tokio::test]
    async fn returns_replace_with_value() {
        let cmd = sh(r#"cat > /dev/null; printf '{"decision":"replace","value":{"x":1}}\n'"#);
        let out = run(&cmd, &json!({}), Duration::from_secs(2))
            .await
            .expect("run ok");
        match out {
            HookOutput::Replace { value } => assert_eq!(value["x"], 1),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn forwards_stdin_payload_to_script() {
        // Echo script reads stdin and writes back a Replace whose value
        // is the parsed input event field, proving the input was piped.
        let cmd = sh(r#"input=$(cat); printf '{"decision":"replace","value":%s}\n' "$input""#);
        let out = run(
            &cmd,
            &json!({"event": "before_llm"}),
            Duration::from_secs(2),
        )
        .await
        .expect("run ok");
        match out {
            HookOutput::Replace { value } => assert_eq!(value["event"], "before_llm"),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn empty_stdout_yields_error() {
        let cmd = sh("cat > /dev/null");
        let err = run(&cmd, &json!({}), Duration::from_secs(2))
            .await
            .expect_err("must error");
        assert!(matches!(err, RunnerError::EmptyStdout));
    }

    #[tokio::test]
    async fn invalid_json_yields_bad_json() {
        let cmd = sh(r"cat > /dev/null; printf 'not json\n'");
        let err = run(&cmd, &json!({}), Duration::from_secs(2))
            .await
            .expect_err("must error");
        assert!(matches!(err, RunnerError::BadJson(_)));
    }

    #[tokio::test]
    async fn timeout_kills_long_running_subprocess() {
        let cmd = sh("sleep 5");
        let err = run(&cmd, &json!({}), Duration::from_millis(80))
            .await
            .expect_err("must error");
        assert!(matches!(err, RunnerError::Timeout(_)));
    }

    #[tokio::test]
    async fn returns_after_valid_stdout_even_if_child_keeps_running() {
        let pid_path = std::env::temp_dir().join(format!(
            "crabgent-hook-subprocess-{}-valid-then-sleep.pid",
            std::process::id()
        ));
        let script = format!(
            r#"cat > /dev/null; printf '%s\n' "$$" > "{}"; printf '{{"decision":"continue"}}\n'; exec sleep 30"#,
            pid_path.display()
        );
        let cmd = sh(&script);
        let started = Instant::now();
        let out = run(&cmd, &json!({}), Duration::from_secs(1))
            .await
            .expect("run ok");

        assert!(matches!(out, HookOutput::Continue));
        assert!(started.elapsed() < Duration::from_secs(2));
        let pid: u32 = std::fs::read_to_string(&pid_path)
            .expect("pid file")
            .trim()
            .parse()
            .expect("pid");
        assert!(!pid_exists(pid));
        std::fs::remove_file(pid_path).expect("remove pid file");
    }

    #[tokio::test]
    async fn stderr_flood_does_not_block_stdout() {
        let cmd = sh(
            r#"cat > /dev/null; i=0; while [ "$i" -lt 3000 ]; do printf 'stderr flood %04d xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx\n' "$i" >&2; i=$((i + 1)); done; printf '{"decision":"continue"}\n'"#,
        );
        let started = Instant::now();
        let out = run(&cmd, &json!({}), Duration::from_secs(2))
            .await
            .expect("run ok");

        assert!(matches!(out, HookOutput::Continue));
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    fn pid_exists(pid: u32) -> bool {
        std::process::Command::new("sh")
            .arg("-c")
            .arg(format!("kill -0 {pid} 2>/dev/null"))
            .status()
            .is_ok_and(|status| status.success())
    }

    #[tokio::test]
    async fn empty_command_yields_empty_command_err() {
        let cmd: Vec<String> = vec![];
        let err = run(&cmd, &json!({}), Duration::from_secs(1))
            .await
            .expect_err("must error");
        assert!(matches!(err, RunnerError::EmptyCommand));
    }

    #[tokio::test]
    async fn spawn_error_for_nonexistent_program() {
        let cmd = vec!["definitely-not-a-real-binary-xyzzy".into()];
        let err = run(&cmd, &json!({}), Duration::from_secs(1))
            .await
            .expect_err("must error");
        assert!(matches!(err, RunnerError::Spawn(_)));
    }
}
