//! `bash` builtin: spawn a shell command and return stdout/stderr/exit.
//!
//! Caveat: this tool runs commands raw, with no sandbox. In any
//! non-homelab context register a sandboxing hook (or replace the
//! built-in with a sandboxed variant). The repository README documents
//! the expected security posture.

use std::io::ErrorKind;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use async_trait::async_trait;
#[cfg(unix)]
use nix::errno::Errno;
#[cfg(unix)]
use nix::sys::signal::{Signal, killpg};
#[cfg(unix)]
use nix::unistd::Pid;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::{Child, ChildStderr, ChildStdout, Command};
use tokio::task::JoinHandle;

use crate::error::ToolError;
use crate::tool::{Tool, ToolCtx, parse_args};

const DEFAULT_TIMEOUT_MS: u64 = 30_000;
const STDOUT_CAP: usize = 200_000;
const STDERR_CAP: usize = 50_000;
const TERM_GRACE: Duration = Duration::from_millis(100);
const CHILD_EXIT_GRACE: Duration = Duration::from_secs(5);
const OUTPUT_DRAIN_GRACE: Duration = Duration::from_secs(5);

#[derive(Debug, Deserialize)]
struct Args {
    command: String,
    timeout_ms: Option<u64>,
    cwd: Option<PathBuf>,
}

/// Run a shell command. Caveat: no sandbox. See module docs and README.
pub struct BashTool {
    default_timeout_ms: u64,
}

impl BashTool {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            default_timeout_ms: DEFAULT_TIMEOUT_MS,
        }
    }

    #[must_use]
    pub const fn with_default_timeout_ms(default_timeout_ms: u64) -> Self {
        Self { default_timeout_ms }
    }
}

impl Default for BashTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for BashTool {
    fn name(&self) -> &'static str {
        "bash"
    }
    fn description(&self) -> &'static str {
        "Run a shell command via `bash -c`. Returns stdout, stderr, exit_code, timed_out. Output is capped per stream. NO SANDBOX: in non-homelab contexts register a sandboxing hook. See the repository README security section."
    }
    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "command": {"type": "string"},
                "timeout_ms": {"type": "integer", "default": 30_000},
                "cwd": {"type": "string"}
            },
            "required": ["command"]
        })
    }
    async fn execute(&self, args: Value, ctx: &ToolCtx) -> Result<Value, ToolError> {
        let args: Args = parse_args(args)?;
        let timeout = Duration::from_millis(args.timeout_ms.unwrap_or(self.default_timeout_ms));
        let mut cmd = Command::new("bash");
        cmd.arg("-c")
            .arg(&args.command)
            .kill_on_drop(true)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        configure_process_group(&mut cmd);
        if let Some(cwd) = args.cwd.as_ref() {
            cmd.current_dir(cwd);
        }
        run(cmd, timeout, ctx).await
    }
}

async fn run(mut cmd: Command, timeout: Duration, ctx: &ToolCtx) -> Result<Value, ToolError> {
    let cancel = ctx.cancel.clone();
    let mut child = cmd.spawn().map_err(|e| ToolError::Io(e.to_string()))?;
    let stdout = take_stdout(&mut child)?;
    let stderr = take_stderr(&mut child)?;
    let stdout_task = tokio::spawn(read_pipe(stdout, STDOUT_CAP));
    let stderr_task = tokio::spawn(read_pipe(stderr, STDERR_CAP));

    tokio::select! {
        biased;
        () = wait_cancel(cancel.as_ref()) => {
            terminate_child(&mut child).await?;
            drain_cancelled_output(stdout_task, stderr_task).await;
            Err(ToolError::Cancelled)
        }
        () = tokio::time::sleep(timeout) => {
            terminate_child(&mut child).await?;
            let (stdout, stderr) = collect_output(stdout_task, stderr_task).await?;
            Ok(format_output(&stdout, &stderr, None, true))
        }
        status = child.wait() => {
            let status = status.map_err(|e| ToolError::Io(e.to_string()))?;
            let (stdout, stderr) = collect_output(stdout_task, stderr_task).await?;
            Ok(format_output(&stdout, &stderr, status.code(), false))
        }
    }
}

#[cfg(unix)]
fn configure_process_group(cmd: &mut Command) {
    cmd.process_group(0);
}

#[cfg(not(unix))]
fn configure_process_group(_cmd: &mut Command) {}

fn take_stdout(child: &mut Child) -> Result<ChildStdout, ToolError> {
    child
        .stdout
        .take()
        .ok_or_else(|| ToolError::Io("bash stdout pipe missing".into()))
}

fn take_stderr(child: &mut Child) -> Result<ChildStderr, ToolError> {
    child
        .stderr
        .take()
        .ok_or_else(|| ToolError::Io("bash stderr pipe missing".into()))
}

async fn terminate_child(child: &mut Child) -> Result<(), ToolError> {
    if child_exited(child)? {
        return Ok(());
    }
    let child_group_pid = child_group(child)?;
    request_child_termination(child_group_pid)?;
    tokio::time::sleep(TERM_GRACE).await;
    kill_child(child, child_group_pid).await?;
    if child_exited(child)? {
        return Ok(());
    }
    wait_after_kill(child).await
}

fn child_exited(child: &mut Child) -> Result<bool, ToolError> {
    child
        .try_wait()
        .map(|status| status.is_some())
        .map_err(|e| ToolError::Io(e.to_string()))
}

#[cfg(unix)]
fn child_group(child: &Child) -> Result<Option<Pid>, ToolError> {
    child.id().map(child_pid).transpose()
}

#[cfg(not(unix))]
fn child_group(_child: &Child) -> Result<(), ToolError> {
    Ok(())
}

#[cfg(unix)]
fn request_child_termination(child_group: Option<Pid>) -> Result<(), ToolError> {
    signal_child_group(child_group, Signal::SIGTERM)?;
    Ok(())
}

#[cfg(not(unix))]
fn request_child_termination(_child_group: ()) -> Result<(), ToolError> {
    Ok(())
}

#[cfg(unix)]
async fn kill_child(child: &mut Child, child_group: Option<Pid>) -> Result<(), ToolError> {
    if signal_child_group(child_group, Signal::SIGKILL)? {
        return Ok(());
    }
    kill_direct_child(child).await
}

#[cfg(unix)]
fn signal_child_group(child_group: Option<Pid>, signal: Signal) -> Result<bool, ToolError> {
    let Some(pid) = child_group else {
        return Ok(false);
    };

    // Best-effort process-group signalling:
    // `ESRCH` means the group is already gone, `ECHILD` means no child tasks remain,
    // and `EPERM` means the group can no longer be signaled in this context.
    // In each case we continue with direct child kill fallback.
    match killpg(pid, signal) {
        Ok(()) | Err(Errno::ESRCH) => Ok(true),
        Err(Errno::ECHILD | Errno::EPERM) => Ok(false),
        Err(error) => Err(ToolError::Io(format!(
            "failed to signal child process group {pid}: {error}"
        ))),
    }
}

#[cfg(test)]
#[cfg(unix)]
mod signal_tests {
    use super::{Signal, signal_child_group};

    #[test]
    fn signal_child_group_without_process_group_is_noop() {
        let result = signal_child_group(None, Signal::SIGTERM);

        assert!(matches!(result, Ok(false)));
    }
}

#[cfg(unix)]
fn child_pid(pid: u32) -> Result<Pid, ToolError> {
    i32::try_from(pid)
        .map(Pid::from_raw)
        .map_err(|err| ToolError::Io(format!("bash child pid {pid} exceeds pid_t range: {err}")))
}

#[cfg(not(unix))]
async fn kill_child(child: &mut Child, _child_group: ()) -> Result<(), ToolError> {
    kill_direct_child(child).await
}

async fn kill_direct_child(child: &mut Child) -> Result<(), ToolError> {
    match child.kill().await {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == ErrorKind::InvalidInput => Ok(()),
        Err(err) => Err(ToolError::Io(err.to_string())),
    }
}

async fn wait_after_kill(child: &mut Child) -> Result<(), ToolError> {
    match tokio::time::timeout(CHILD_EXIT_GRACE, child.wait()).await {
        Ok(Ok(_)) => Ok(()),
        Ok(Err(err)) if err.kind() == ErrorKind::InvalidInput => Ok(()),
        Ok(Err(err)) => Err(ToolError::Io(err.to_string())),
        Err(_) => kill_direct_child(child).await,
    }
}

async fn read_pipe<R>(mut pipe: R, cap: usize) -> Result<Vec<u8>, ToolError>
where
    R: AsyncRead + Unpin + Send + 'static,
{
    let mut bytes = Vec::new();
    let mut buf = [0_u8; 4096];
    while bytes.len() <= cap {
        let remaining = cap.saturating_add(1).saturating_sub(bytes.len());
        let read_len = remaining.min(buf.len());
        let read_buf = buf
            .get_mut(..read_len)
            .ok_or_else(|| ToolError::Io("internal bash read buffer range invalid".into()))?;
        let read = pipe
            .read(read_buf)
            .await
            .map_err(|e| ToolError::Io(e.to_string()))?;
        if read == 0 {
            return Ok(bytes);
        }
        let chunk = buf
            .get(..read)
            .ok_or_else(|| ToolError::Io("internal bash read chunk range invalid".into()))?;
        bytes.extend_from_slice(chunk);
    }
    discard_remaining(pipe).await?;
    Ok(bytes)
}

async fn discard_remaining<R>(mut pipe: R) -> Result<(), ToolError>
where
    R: AsyncRead + Unpin,
{
    let mut buf = [0_u8; 4096];
    loop {
        let read = pipe
            .read(&mut buf)
            .await
            .map_err(|e| ToolError::Io(e.to_string()))?;
        if read == 0 {
            return Ok(());
        }
    }
}

type ReadTask = JoinHandle<Result<Vec<u8>, ToolError>>;

async fn collect_output(
    stdout_task: ReadTask,
    stderr_task: ReadTask,
) -> Result<(Vec<u8>, Vec<u8>), ToolError> {
    tokio::time::timeout(
        OUTPUT_DRAIN_GRACE,
        join_output_tasks(stdout_task, stderr_task),
    )
    .await
    .map_err(|_elapsed| ToolError::Io("timed out draining bash output".into()))?
}

async fn join_output_tasks(
    stdout_task: ReadTask,
    stderr_task: ReadTask,
) -> Result<(Vec<u8>, Vec<u8>), ToolError> {
    let stdout = join_reader(stdout_task, "stdout").await?;
    let stderr = join_reader(stderr_task, "stderr").await?;
    Ok((stdout, stderr))
}

async fn join_reader(task: ReadTask, stream_name: &str) -> Result<Vec<u8>, ToolError> {
    match task.await {
        Ok(result) => result,
        Err(err) => Err(ToolError::Io(format!(
            "failed to join bash {stream_name} reader: {err}"
        ))),
    }
}

async fn drain_cancelled_output(stdout_task: ReadTask, stderr_task: ReadTask) {
    // Silent cleanup path: reader failures are not actionable after cancellation.
    if let Err(_err) = collect_output(stdout_task, stderr_task).await {}
}

async fn wait_cancel(cancel: Option<&tokio_util::sync::CancellationToken>) {
    match cancel {
        Some(t) => t.cancelled().await,
        None => std::future::pending().await,
    }
}

fn format_output(stdout: &[u8], stderr: &[u8], exit_code: Option<i32>, timed_out: bool) -> Value {
    let stdout = truncate(stdout, STDOUT_CAP);
    let stderr = truncate(stderr, STDERR_CAP);
    json!({
        "stdout": stdout,
        "stderr": stderr,
        "exit_code": exit_code,
        "timed_out": timed_out
    })
}

fn truncate(bytes: &[u8], cap: usize) -> String {
    let mut s = super::safe_truncate(bytes, cap);
    if bytes.len() > cap {
        s.push_str(super::TRUNCATE_MARKER);
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::subject::Subject;

    fn ctx() -> ToolCtx {
        ToolCtx::new(Subject::new("u"))
    }

    #[tokio::test]
    async fn echo_returns_stdout() {
        let tool = BashTool::new();
        let r = tool
            .execute(json!({"command": "echo hello"}), &ctx())
            .await
            .expect("ok");
        let stdout = r["stdout"].as_str().expect("stdout");
        assert!(stdout.starts_with("hello"));
        assert_eq!(r["exit_code"], 0);
        assert_eq!(r["timed_out"], false);
    }

    #[tokio::test]
    async fn nonzero_exit_is_reported() {
        let tool = BashTool::new();
        let r = tool
            .execute(json!({"command": "exit 7"}), &ctx())
            .await
            .expect("ok");
        assert_eq!(r["exit_code"], 7);
    }

    #[tokio::test]
    async fn stderr_captured() {
        let tool = BashTool::new();
        let r = tool
            .execute(json!({"command": "echo oops 1>&2"}), &ctx())
            .await
            .expect("ok");
        assert!(r["stderr"].as_str().expect("stderr").contains("oops"));
    }

    #[tokio::test]
    async fn invalid_args_errors() {
        let tool = BashTool::new();
        let r = tool.execute(json!({"oops": 1}), &ctx()).await;
        assert!(matches!(r, Err(ToolError::InvalidArgs(_))));
    }

    #[test]
    fn schema_includes_command_required() {
        let tool = BashTool::new();
        let schema = tool.parameters_schema();
        assert!(
            schema["required"]
                .as_array()
                .expect("required")
                .iter()
                .any(|v| v == "command")
        );
    }

    #[test]
    fn truncate_respects_utf8_boundary_at_cap() {
        let output = truncate(b"abc\xc3\xa4xyz", 4);

        assert_eq!(output, "abc\n... [truncated]");
        assert!(!output.contains('\u{FFFD}'));
    }
}
