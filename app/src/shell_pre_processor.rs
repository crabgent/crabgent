//! `ShellPreProcessor`: cron pre-processor that runs `CronJob.pre_command`
//! through a shell and decides whether the LLM run should fire.
//!
//! Semantics mirror clawtool's `pre_command` column:
//!
//! - `pre_command` is `None` / empty → `Passthrough` (default flow runs).
//! - `sh -c <pre_command>` exit-code `0` with empty stdout → `Skip`
//!   (nothing changed, suppress the LLM run).
//! - exit-code `0` with stdout → `Deliver(stdout)` directly.
//! - exit-code non-zero → `RunLlm(combined)` where `combined` is the job
//!   prompt followed by a separator and the captured stdout. Non-zero is
//!   the explicit "fire LLM" signal in clawtool scripts (see
//!   `GTD Pull-Watch` in postgres).
//! - spawn error / timeout → `Deliver(error)` directly. A pre-command
//!   failure means the prompt context is incomplete; running the naked
//!   prompt would produce confusing user-visible output.

use std::time::Duration;

use async_trait::async_trait;
use crabgent_cron::{CronPreProcessResult, CronPreProcessor};
use crabgent_log::{debug, info, warn};
use crabgent_store::records::CronJob;
use tokio::process::Command;

const STDOUT_SEPARATOR: &str = "\n\n---PRE-COMMAND-OUTPUT---\n";
const DEFAULT_TIMEOUT: Duration = Duration::from_mins(1);

pub struct ShellPreProcessor {
    shell: String,
    timeout: Duration,
}

impl Default for ShellPreProcessor {
    fn default() -> Self {
        Self {
            shell: "sh".to_string(),
            timeout: DEFAULT_TIMEOUT,
        }
    }
}

impl ShellPreProcessor {
    pub fn new() -> Self {
        Self::default()
    }

    #[allow(dead_code)]
    pub const fn with_timeout(mut self, t: Duration) -> Self {
        self.timeout = t;
        self
    }
}

#[async_trait]
impl CronPreProcessor for ShellPreProcessor {
    async fn pre_process(&self, job: &CronJob) -> CronPreProcessResult {
        let cmd = match job.pre_command.as_deref() {
            Some(s) if !s.trim().is_empty() => s,
            _ => return CronPreProcessResult::Passthrough,
        };
        debug!(job = %job.name, "shell-pre: running pre_command");
        let exec = Command::new(&self.shell)
            .arg("-c")
            .arg(cmd)
            .stdin(std::process::Stdio::null())
            .output();
        let output = match tokio::time::timeout(self.timeout, exec).await {
            Ok(Ok(o)) => o,
            Ok(Err(err)) => {
                warn!(job = %job.name, "shell-pre: spawn failed: {err}");
                return CronPreProcessResult::Deliver(format!(
                    "Cron `{}`: pre_command could not start.",
                    job.name
                ));
            }
            Err(_) => {
                warn!(job = %job.name, timeout = ?self.timeout, "shell-pre: pre_command timed out");
                return CronPreProcessResult::Deliver(format!(
                    "Cron `{}`: pre_command timed out after {}.",
                    job.name,
                    format_duration(self.timeout)
                ));
            }
        };
        let stdout = String::from_utf8_lossy(&output.stdout);
        if output.status.success() {
            if stdout.trim().is_empty() {
                info!(job = %job.name, "shell-pre: pre_command exit=0, skipping LLM run");
                return CronPreProcessResult::Skip;
            }
            info!(
                job = %job.name,
                stdout_bytes = output.stdout.len(),
                "shell-pre: pre_command exit=0 with stdout, delivering directly",
            );
            return CronPreProcessResult::Deliver(stdout.into_owned());
        }
        let code = output.status.code().unwrap_or(-1);
        info!(
            job = %job.name,
            exit = code,
            stdout_bytes = output.stdout.len(),
            "shell-pre: pre_command non-zero, firing LLM with appended stdout",
        );
        let mut combined = job.prompt.clone();
        combined.push_str(STDOUT_SEPARATOR);
        combined.push_str(&stdout);
        CronPreProcessResult::RunLlm(combined)
    }
}

fn format_duration(duration: Duration) -> String {
    if duration.as_secs() > 0 {
        format!("{}s", duration.as_secs())
    } else {
        format!("{}ms", duration.as_millis())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use crabgent_core::MemoryScope;
    use crabgent_store::CronJobId;
    use crabgent_store::records::CronSchedule;

    fn fixture(pre_command: Option<&str>) -> CronJob {
        CronJob {
            id: CronJobId::new(),
            name: "demo".into(),
            scope: MemoryScope::default(),
            prompt: "do the thing".into(),
            schedule: CronSchedule::every(60),
            enabled: true,
            run_once: false,
            model_override: None,
            reasoning_effort_override: None,
            pre_command: pre_command.map(str::to_string),
            delivery_ctx: serde_json::Value::Null,
            last_run: None,
            next_run: Utc::now(),
            created_at: Utc::now(),
            claimed_at: None,
        }
    }

    #[tokio::test]
    async fn none_is_passthrough() {
        let p = ShellPreProcessor::new();
        let job = fixture(None);
        assert!(matches!(
            p.pre_process(&job).await,
            CronPreProcessResult::Passthrough
        ));
    }

    #[tokio::test]
    async fn empty_is_passthrough() {
        let p = ShellPreProcessor::new();
        let job = fixture(Some("   "));
        assert!(matches!(
            p.pre_process(&job).await,
            CronPreProcessResult::Passthrough
        ));
    }

    #[tokio::test]
    async fn exit_zero_is_skip() {
        let p = ShellPreProcessor::new();
        let job = fixture(Some("exit 0"));
        assert!(matches!(
            p.pre_process(&job).await,
            CronPreProcessResult::Skip
        ));
    }

    #[tokio::test]
    async fn exit_nonzero_runs_llm_with_stdout() {
        let p = ShellPreProcessor::new();
        let job = fixture(Some("echo hello-world; exit 1"));
        match p.pre_process(&job).await {
            CronPreProcessResult::RunLlm(combined) => {
                assert!(combined.starts_with("do the thing"));
                assert!(combined.contains("---PRE-COMMAND-OUTPUT---"));
                assert!(combined.contains("hello-world"));
            }
            other => panic!("expected RunLlm, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn spawn_failure_is_delivered() {
        let p = ShellPreProcessor {
            shell: "/this/shell/does/not/exist".into(),
            timeout: Duration::from_secs(5),
        };
        let job = fixture(Some("true"));
        match p.pre_process(&job).await {
            CronPreProcessResult::Deliver(message) => {
                assert!(message.contains("demo"));
                assert!(message.contains("could not start"));
            }
            other => panic!("expected Deliver, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn timeout_is_delivered() {
        let p = ShellPreProcessor::new().with_timeout(Duration::from_millis(50));
        let job = fixture(Some("sleep 5"));
        match p.pre_process(&job).await {
            CronPreProcessResult::Deliver(message) => {
                assert!(message.contains("demo"));
                assert!(message.contains("timed out"));
            }
            other => panic!("expected Deliver, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn exit_zero_with_stdout_is_delivered() {
        let p = ShellPreProcessor::new();
        let job = fixture(Some("echo ready; exit 0"));
        match p.pre_process(&job).await {
            CronPreProcessResult::Deliver(message) => {
                assert!(message.contains("ready"));
            }
            other => panic!("expected Deliver, got {other:?}"),
        }
    }
}
