use std::{
    collections::BTreeMap,
    ffi::OsString,
    io,
    path::PathBuf,
    process::Stdio,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use phi_kernel::TurnCancel;
#[cfg(unix)]
use process_wrap::tokio::ProcessSession;
use process_wrap::tokio::{ChildWrapper, CommandWrap, KillOnDrop};
#[cfg(windows)]
use process_wrap::tokio::{CreationFlags, JobObject};
use serde::{Deserialize, Serialize};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    process::Command,
};

pub struct ProcessRequest {
    pub executable: PathBuf,
    pub arguments: Vec<OsString>,
    pub cwd: PathBuf,
    pub environment: BTreeMap<OsString, OsString>,
    pub remove_environment: Vec<OsString>,
    pub stdin: Vec<u8>,
    pub timeout: Duration,
    pub output_limit: usize,
}

impl ProcessRequest {
    pub fn new(executable: PathBuf, cwd: PathBuf, timeout: Duration, output_limit: usize) -> Self {
        Self {
            executable,
            cwd,
            timeout,
            output_limit,
            arguments: Vec::new(),
            environment: BTreeMap::new(),
            remove_environment: Vec::new(),
            stdin: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CapturedOutput {
    pub text: String,
    pub truncated: bool,
    pub invalid_utf8: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProcessTermination {
    Exited,
    Cancelled,
    TimedOut,
}

#[derive(Debug)]
pub struct ProcessOutcome {
    pub termination: ProcessTermination,
    pub exit_code: Option<i32>,
    pub success: bool,
    pub stdout: CapturedOutput,
    pub stderr: CapturedOutput,
    pub elapsed: Duration,
}

#[derive(Default)]
struct Capture {
    bytes: Vec<u8>,
    truncated: bool,
}
impl Capture {
    fn append(&mut self, bytes: &[u8], limit: usize) {
        let keep = bytes.len().min(limit.saturating_sub(self.bytes.len()));
        self.bytes.extend_from_slice(&bytes[..keep]);
        self.truncated |= keep != bytes.len();
    }
    fn snapshot(&self, limit: usize) -> CapturedOutput {
        let mut bytes = self.bytes.as_slice();
        if self.truncated
            && let Err(error) = std::str::from_utf8(bytes)
            && error.error_len().is_none()
        {
            bytes = &bytes[..error.valid_up_to()];
        }
        let mut text = String::from_utf8_lossy(bytes).into_owned();
        let expanded = text.len() > limit;
        if expanded {
            let mut keep = limit;
            while !text.is_char_boundary(keep) {
                keep -= 1;
            }
            text.truncate(keep);
        }
        CapturedOutput {
            text,
            truncated: self.truncated || expanded,
            invalid_utf8: std::str::from_utf8(bytes).is_err(),
        }
    }
}

#[derive(Clone, Default)]
pub struct ProcessSupervisor;

struct ManagedChild {
    process: Box<dyn ChildWrapper>,
    reaped: bool,
}

impl std::ops::Deref for ManagedChild {
    type Target = dyn ChildWrapper;
    fn deref(&self) -> &Self::Target {
        self.process.as_ref()
    }
}

impl std::ops::DerefMut for ManagedChild {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.process.as_mut()
    }
}

impl Drop for ManagedChild {
    fn drop(&mut self) {
        // KillOnDrop alone targets the leader on Unix. This guard addresses its group
        // as well when a panic or dropped future skips the explicit awaitable cleanup.
        if !self.reaped {
            let _ = self.process.start_kill();
        }
    }
}

impl ProcessSupervisor {
    /// No shell. Completion includes group termination, reaping, and bounded pipe draining.
    pub async fn run(
        &self,
        request: ProcessRequest,
        cancel: &TurnCancel,
    ) -> io::Result<ProcessOutcome> {
        let started = Instant::now();
        if cancel.is_cancelled() {
            return Ok(ProcessOutcome {
                termination: ProcessTermination::Cancelled,
                exit_code: None,
                success: false,
                stdout: CapturedOutput::default(),
                stderr: CapturedOutput::default(),
                elapsed: started.elapsed(),
            });
        }
        let mut command = Command::new(&request.executable);
        command
            .args(&request.arguments)
            .current_dir(&request.cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for variable in &request.remove_environment {
            command.env_remove(variable);
        }
        command.envs(&request.environment);
        let mut command = CommandWrap::from(command);
        command.wrap(KillOnDrop);
        #[cfg(unix)]
        command.wrap(ProcessSession);
        #[cfg(windows)]
        {
            command.wrap(CreationFlags(
                windows::Win32::System::Threading::CREATE_NO_WINDOW,
            ));
            command.wrap(JobObject);
        }
        let mut child = ManagedChild {
            process: command.spawn()?,
            reaped: false,
        };
        let stdout = Arc::new(Mutex::new(Capture::default()));
        let stderr = Arc::new(Mutex::new(Capture::default()));
        let mut stdout_task = tokio::spawn(Self::drain(
            child.stdout().take().expect("piped stdout"),
            stdout.clone(),
            request.output_limit,
        ));
        let mut stderr_task = tokio::spawn(Self::drain(
            child.stderr().take().expect("piped stderr"),
            stderr.clone(),
            request.output_limit,
        ));
        let mut input = child.stdin().take().expect("piped stdin");
        let mut input_task = tokio::spawn(async move {
            input.write_all(&request.stdin).await?;
            input.shutdown().await
        });
        // Do not cancel a group wait future: some implementations already wait on a
        // completion port in a blocking task. Poll the leader, then terminate and join.
        let deadline = tokio::time::Instant::now() + request.timeout;
        let (termination, status) = loop {
            if cancel.is_cancelled() {
                break (ProcessTermination::Cancelled, None);
            }
            match child.try_wait() {
                Ok(Some(status)) => break (ProcessTermination::Exited, Some(Ok(status))),
                Ok(None) => {}
                Err(error) => break (ProcessTermination::Exited, Some(Err(error))),
            }
            tokio::select! {
                biased;
                () = cancel.cancelled() => break (ProcessTermination::Cancelled, None),
                () = tokio::time::sleep_until(deadline) => break (ProcessTermination::TimedOut, None),
                () = tokio::time::sleep(Duration::from_millis(20)) => {},
            }
        };
        // Also kill descendants after the leader exits normally: calls never own background jobs.
        let kill_result = child.start_kill();
        let waited = child.wait().await;
        child.reaped = waited.is_ok()
            && (kill_result.is_ok()
                || kill_result.as_ref().is_err_and(|error| {
                    error.kind() == io::ErrorKind::NotFound || error.raw_os_error() == Some(3)
                }));
        let status = match status {
            Some(status) => status,
            None => waited,
        };
        // A dead group may report NotFound/ESRCH. Other cleanup errors are surfaced.
        input_task.abort();
        let _ = (&mut input_task).await;
        let mut drain_error = None;
        for (task, capture) in [(&mut stdout_task, &stdout), (&mut stderr_task, &stderr)] {
            match tokio::time::timeout(Duration::from_secs(2), &mut *task).await {
                Ok(Ok(Ok(()))) => {}
                Ok(Ok(Err(error))) => drain_error = Some(error),
                Ok(Err(error)) => drain_error = Some(io::Error::other(error)),
                Err(_) => {
                    task.abort();
                    let _ = task.await;
                    capture
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .truncated = true;
                }
            }
        }
        if let Err(error) = kill_result
            && error.kind() != io::ErrorKind::NotFound
            && error.raw_os_error() != Some(3)
        {
            return Err(error);
        }
        if let Some(error) = drain_error {
            return Err(error);
        }
        let status = status?;
        let stdout = stdout
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .snapshot(request.output_limit);
        let stderr = stderr
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .snapshot(request.output_limit);
        Ok(ProcessOutcome {
            termination,
            exit_code: status.code(),
            success: termination == ProcessTermination::Exited && status.success(),
            stdout,
            stderr,
            elapsed: started.elapsed(),
        })
    }

    async fn drain(
        mut pipe: impl AsyncRead + Unpin,
        capture: Arc<Mutex<Capture>>,
        limit: usize,
    ) -> io::Result<()> {
        let mut buffer = [0_u8; 8192];
        loop {
            let count = pipe.read(&mut buffer).await?;
            if count == 0 {
                return Ok(());
            }
            capture
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .append(&buffer[..count], limit);
        }
    }
}
