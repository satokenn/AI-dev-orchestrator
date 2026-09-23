//! Run external commands with bounded, observable process lifecycles.

use std::ffi::OsString;
use std::io::{self, Read};
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

/// A command invocation independent of a shell.
#[derive(Debug, Clone)]
pub struct ProcessRequest {
    pub command: OsString,
    pub args: Vec<OsString>,
    pub cwd: Option<PathBuf>,
    pub env: Vec<(OsString, OsString)>,
    pub timeout: Option<Duration>,
}

impl ProcessRequest {
    #[must_use]
    pub fn new(command: impl Into<OsString>) -> Self {
        Self {
            command: command.into(),
            args: Vec::new(),
            cwd: None,
            env: Vec::new(),
            timeout: None,
        }
    }
    #[must_use]
    pub fn arg(mut self, arg: impl Into<OsString>) -> Self {
        self.args.push(arg.into());
        self
    }
    #[must_use]
    pub fn args(mut self, args: impl IntoIterator<Item = impl Into<OsString>>) -> Self {
        self.args.extend(args.into_iter().map(Into::into));
        self
    }
    #[must_use]
    pub fn cwd(mut self, cwd: impl Into<PathBuf>) -> Self {
        self.cwd = Some(cwd.into());
        self
    }
    #[must_use]
    pub fn env(mut self, key: impl Into<OsString>, value: impl Into<OsString>) -> Self {
        self.env.push((key.into(), value.into()));
        self
    }
    #[must_use]
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }
}

/// Captured result of a process that was allowed to exit normally.
#[derive(Debug)]
pub struct ProcessOutput {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub status: ExitStatus,
}

impl ProcessOutput {
    #[must_use]
    pub fn exit_code(&self) -> Option<i32> {
        self.status.code()
    }
}

/// A cloneable signal used to request cancellation of a running process.
#[derive(Debug, Clone, Default)]
pub struct CancellationToken {
    cancelled: Arc<AtomicBool>,
}

impl CancellationToken {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }
}

/// Failure categories are distinct so callers can choose a retry policy.
#[derive(Debug)]
pub enum ProcessError {
    Spawn(io::Error),
    Io(io::Error),
    NonZeroExit(ProcessOutput),
    TimedOut(ProcessOutput),
    Cancelled(ProcessOutput),
}

/// Executes external commands and owns their complete lifecycle.
#[derive(Debug, Default, Clone, Copy)]
pub struct ProcessRunner;

impl ProcessRunner {
    /// Runs a command without an external cancellation signal.
    pub fn run(&self, request: ProcessRequest) -> Result<ProcessOutput, ProcessError> {
        self.run_with_cancellation(request, CancellationToken::new())
    }

    /// Runs a command, stopping it when cancelled or when its timeout elapses.
    pub fn run_with_cancellation(
        &self,
        request: ProcessRequest,
        token: CancellationToken,
    ) -> Result<ProcessOutput, ProcessError> {
        let timeout = request.timeout;
        let mut command = Command::new(&request.command);
        command
            .args(&request.args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(cwd) = request.cwd {
            command.current_dir(cwd);
        }
        for (key, value) in request.env {
            command.env(key, value);
        }

        let mut child = command.spawn().map_err(ProcessError::Spawn)?;
        let stdout = take_pipe(&mut child, true)?;
        let stderr = take_pipe(&mut child, false)?;
        let started = Instant::now();
        let reason = loop {
            if token.is_cancelled() {
                break Some(StopReason::Cancelled);
            }
            if timeout.is_some_and(|timeout| started.elapsed() >= timeout) {
                break Some(StopReason::TimedOut);
            }
            if child.try_wait().map_err(ProcessError::Io)?.is_some() {
                break None;
            }
            thread::sleep(Duration::from_millis(5));
        };
        if reason.is_some() {
            if let Err(error) = child.kill() {
                if error.kind() != io::ErrorKind::InvalidInput {
                    return Err(ProcessError::Io(error));
                }
            }
        }
        let status = child.wait().map_err(ProcessError::Io)?;
        let output = ProcessOutput {
            stdout: join_pipe(stdout)?,
            stderr: join_pipe(stderr)?,
            status,
        };
        match reason {
            Some(StopReason::Cancelled) => Err(ProcessError::Cancelled(output)),
            Some(StopReason::TimedOut) => Err(ProcessError::TimedOut(output)),
            None if !output.status.success() => Err(ProcessError::NonZeroExit(output)),
            None => Ok(output),
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum StopReason {
    Cancelled,
    TimedOut,
}

fn take_pipe(
    child: &mut Child,
    stdout: bool,
) -> Result<thread::JoinHandle<io::Result<Vec<u8>>>, ProcessError> {
    let pipe = if stdout {
        child.stdout.take().map(spawn_reader)
    } else {
        child.stderr.take().map(spawn_reader)
    };
    pipe.ok_or_else(|| ProcessError::Io(io::Error::other("missing output pipe")))
}

fn spawn_reader<R: Read + Send + 'static>(mut pipe: R) -> thread::JoinHandle<io::Result<Vec<u8>>> {
    thread::spawn(move || {
        let mut bytes = Vec::new();
        pipe.read_to_end(&mut bytes).map(|_| bytes)
    })
}

fn join_pipe(handle: thread::JoinHandle<io::Result<Vec<u8>>>) -> Result<Vec<u8>, ProcessError> {
    handle
        .join()
        .map_err(|_| ProcessError::Io(io::Error::other("output reader panicked")))?
        .map_err(ProcessError::Io)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn shell() -> &'static str {
        if cfg!(windows) { "cmd" } else { "sh" }
    }
    fn shell_args(script: &str) -> Vec<OsString> {
        if cfg!(windows) {
            vec!["/C".into(), script.into()]
        } else {
            vec!["-c".into(), script.into()]
        }
    }

    #[test]
    fn captures_output_and_exit_code() {
        let request = ProcessRequest::new(shell()).args(shell_args("printf out; printf err >&2"));
        let output = ProcessRunner.run(request).expect("process succeeds");
        assert_eq!(output.stdout, b"out");
        assert_eq!(output.stderr, b"err");
        assert_eq!(output.exit_code(), Some(0));
    }

    #[test]
    fn distinguishes_spawn_and_non_zero_failures() {
        assert!(matches!(
            ProcessRunner.run(ProcessRequest::new("definitely-not-a-command")),
            Err(ProcessError::Spawn(_))
        ));
        let request =
            ProcessRequest::new(shell()).args(shell_args("printf out; printf err >&2; exit 7"));
        match ProcessRunner.run(request) {
            Err(ProcessError::NonZeroExit(output)) => {
                assert_eq!(output.exit_code(), Some(7));
                assert_eq!(output.stdout, b"out");
                assert_eq!(output.stderr, b"err");
            }
            result => panic!("unexpected result: {result:?}"),
        }
    }

    #[cfg(unix)]
    #[test]
    fn stops_a_timed_out_process() {
        let request = ProcessRequest::new("sleep")
            .arg("10")
            .timeout(Duration::from_millis(20));
        assert!(matches!(
            ProcessRunner.run(request),
            Err(ProcessError::TimedOut(_))
        ));
    }

    #[cfg(unix)]
    #[test]
    fn cancellation_stops_a_running_process() {
        let token = CancellationToken::new();
        let other = token.clone();
        let thread = std::thread::spawn(move || {
            ProcessRunner.run_with_cancellation(ProcessRequest::new("sleep").arg("10"), other)
        });
        std::thread::sleep(Duration::from_millis(20));
        token.cancel();
        assert!(matches!(
            thread.join().expect("runner thread"),
            Err(ProcessError::Cancelled(_))
        ));
    }
}
