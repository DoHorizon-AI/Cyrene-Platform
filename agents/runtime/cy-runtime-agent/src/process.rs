//! ┌─────────────────────────────────────────────────────────────────────┐
//! │  📄 process.rs                                                      │
//! │  Module: cy_runtime_agent::process                                  │
//! │  Role: Supervise one child process group without host privileges.   │
//! │                                                                     │
//! │  模块职责：无宿主机权限地监督单个 workload 进程组并执行有界停止。         │
//! └─────────────────────────────────────────────────────────────────────┘

use std::collections::BTreeMap;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{ExitStatus, Stdio};
use std::time::Duration;

use nix::sys::signal::{killpg, Signal};
use nix::unistd::Pid;
use thiserror::Error;
use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::mpsc;

/// Child process supervision failure.
#[derive(Debug, Error)]
pub enum ChildSupervisorError {
    #[error("child process is already running")]
    AlreadyRunning,
    #[error("child process is not running")]
    NotRunning,
    #[error("failed to spawn or wait for child: {0}")]
    Io(#[from] std::io::Error),
    #[error("failed to signal child process group: {0}")]
    Signal(String),
}

/// Observed terminal child fact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChildExit {
    pub code: Option<i32>,
    pub success: bool,
}

/// Whether graceful termination completed before forced escalation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StopOutcome {
    pub exit: ChildExit,
    pub forced: bool,
}

/// Child output channel consumed by the Runtime Agent event forwarder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkloadOutputStream {
    Stdout,
    Stderr,
}

/// One complete UTF-8-lossy workload output line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkloadOutput {
    pub stream: WorkloadOutputStream,
    pub line: String,
}

/// Single-child process-group supervisor used inside an ordinary container.
pub struct ChildSupervisor {
    command: Vec<String>,
    child: Option<Child>,
    output: Option<mpsc::Receiver<WorkloadOutput>>,
}

impl ChildSupervisor {
    pub fn new(command: Vec<String>) -> Self {
        Self {
            command,
            child: None,
            output: None,
        }
    }

    pub fn is_running(&self) -> bool {
        self.child.is_some()
    }

    /// Spawn the fixed launch-time command in its own process group.
    pub async fn start(
        &mut self,
        working_directory: Option<PathBuf>,
        additions: &BTreeMap<String, String>,
    ) -> Result<(), ChildSupervisorError> {
        if self.child.is_some() {
            return Err(ChildSupervisorError::AlreadyRunning);
        }
        let mut command = Command::new(&self.command[0]);
        command.args(&self.command[1..]);
        command.envs(additions);
        command.stdout(Stdio::piped()).stderr(Stdio::piped());
        if let Some(path) = working_directory {
            command.current_dir(path);
        }
        command.as_std_mut().process_group(0);
        let mut child = command.spawn()?;
        let stdout = child.stdout.take().ok_or_else(|| {
            ChildSupervisorError::Io(std::io::Error::other("child stdout pipe unavailable"))
        })?;
        let stderr = child.stderr.take().ok_or_else(|| {
            ChildSupervisorError::Io(std::io::Error::other("child stderr pipe unavailable"))
        })?;
        let (sender, receiver) = mpsc::channel(1024);
        tokio::spawn(capture_output(
            stdout,
            WorkloadOutputStream::Stdout,
            sender.clone(),
        ));
        tokio::spawn(capture_output(stderr, WorkloadOutputStream::Stderr, sender));
        self.output = Some(receiver);
        self.child = Some(child);
        Ok(())
    }

    /// Drain up to `limit` captured lines without blocking process supervision.
    pub fn drain_output(&mut self, limit: usize) -> Vec<WorkloadOutput> {
        let mut output = Vec::new();
        let Some(receiver) = self.output.as_mut() else {
            return output;
        };
        while output.len() < limit {
            match receiver.try_recv() {
                Ok(line) => output.push(line),
                Err(mpsc::error::TryRecvError::Empty | mpsc::error::TryRecvError::Disconnected) => {
                    break;
                }
            }
        }
        output
    }

    pub fn try_wait(&mut self) -> Result<Option<ChildExit>, ChildSupervisorError> {
        let Some(child) = self.child.as_mut() else {
            return Ok(None);
        };
        let status = child.try_wait()?;
        if let Some(status) = status {
            self.child = None;
            return Ok(Some(child_exit(status)));
        }
        Ok(None)
    }

    /// Send SIGTERM to the process group, then SIGKILL only after the deadline.
    pub async fn stop(
        &mut self,
        grace_period: Duration,
    ) -> Result<StopOutcome, ChildSupervisorError> {
        let mut child = self.child.take().ok_or(ChildSupervisorError::NotRunning)?;
        let process_id = child.id().ok_or(ChildSupervisorError::NotRunning)?;
        let group = Pid::from_raw(i32::try_from(process_id).map_err(|_| {
            ChildSupervisorError::Signal("child process id exceeds signed range".to_string())
        })?);
        killpg(group, Signal::SIGTERM)
            .map_err(|error| ChildSupervisorError::Signal(error.to_string()))?;
        match tokio::time::timeout(grace_period, child.wait()).await {
            Ok(status) => Ok(StopOutcome {
                exit: child_exit(status?),
                forced: false,
            }),
            Err(_) => {
                killpg(group, Signal::SIGKILL)
                    .map_err(|error| ChildSupervisorError::Signal(error.to_string()))?;
                Ok(StopOutcome {
                    exit: child_exit(child.wait().await?),
                    forced: true,
                })
            }
        }
    }
}

async fn capture_output<R>(
    stream: R,
    kind: WorkloadOutputStream,
    sender: mpsc::Sender<WorkloadOutput>,
) where
    R: AsyncRead + Unpin,
{
    let mut lines = BufReader::new(stream).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        if sender
            .send(WorkloadOutput { stream: kind, line })
            .await
            .is_err()
        {
            break;
        }
    }
}

fn child_exit(status: ExitStatus) -> ChildExit {
    ChildExit {
        code: status.code(),
        success: status.success(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn graceful_stop_targets_the_child_process_group() {
        let mut child = ChildSupervisor::new(vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            "trap 'exit 0' TERM; while :; do sleep 0.05; done".to_string(),
        ]);
        child.start(None, &BTreeMap::new()).await.unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        let outcome = child.stop(Duration::from_secs(2)).await.unwrap();
        assert!(!outcome.forced);
        assert!(outcome.exit.success);
    }
}
