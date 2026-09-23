//! Run external commands with bounded, observable process lifecycles.

use std::ffi::OsString;
use std::io::{self, Read};
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::Arc;
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant};

const MAX_CAPTURE_BYTES_PER_STREAM: usize = 1024 * 1024;
const STOP_GRACE_PERIOD: Duration = Duration::from_millis(200);
const PIPE_DRAIN_PERIOD: Duration = Duration::from_millis(200);

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
    /// Whether stdout or stderr exceeded the per-stream capture limit.
    pub output_truncated: bool,
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
    cancelled: Arc<Mutex<bool>>,
}

impl CancellationToken {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
    pub fn cancel(&self) {
        *self
            .cancelled
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = true;
    }
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        *self
            .cancelled
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
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
    CancelledBeforeStart,
    /// Stop was requested, but the managed process group could not be
    /// confirmed stopped within the grace period.
    Interrupted {
        reason: StopReason,
        stopped: bool,
        stdout: Vec<u8>,
        stderr: Vec<u8>,
        diagnostic: String,
    },
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

        configure_process_group(&mut command);
        let mut child = token
            .spawn_if_not_cancelled(&mut command)
            .map_err(ProcessError::Spawn)?
            .ok_or(ProcessError::CancelledBeforeStart)?;
        let stdout = Arc::new(Mutex::new(CapturedBytes::default()));
        let stderr = Arc::new(Mutex::new(CapturedBytes::default()));
        let stdout_reader = take_pipe(&mut child, true, Arc::clone(&stdout))?;
        let stderr_reader = take_pipe(&mut child, false, Arc::clone(&stderr))?;
        let started = Instant::now();
        let mut reason = loop {
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
        let stopped = if let Some(_reason) = reason {
            stop_process_group(&mut child, STOP_GRACE_PERIOD)?
        } else {
            true
        };
        if !stopped {
            return Err(ProcessError::Interrupted {
                reason: reason.expect("a stop was requested"),
                stopped: false,
                stdout: captured(&stdout).bytes,
                stderr: captured(&stderr).bytes,
                diagnostic: "managed process group did not stop within the grace period".into(),
            });
        }
        let status = child.wait().map_err(ProcessError::Io)?;
        let drain_deadline = Instant::now() + PIPE_DRAIN_PERIOD;
        let stdout_done = join_pipe_until(stdout_reader, drain_deadline);
        let stderr_done = join_pipe_until(stderr_reader, drain_deadline);
        if (stdout_done.is_err() || stderr_done.is_err()) && reason.is_none() {
            reason = Some(StopReason::PipeHeld);
            let stopped = stop_process_group(&mut child, STOP_GRACE_PERIOD)?;
            if !stopped {
                return Err(ProcessError::Interrupted {
                    reason: StopReason::PipeHeld,
                    stopped: false,
                    stdout: captured(&stdout).bytes,
                    stderr: captured(&stderr).bytes,
                    diagnostic: "a descendant kept an output pipe open after the command exited"
                        .into(),
                });
            }
        }
        let stdout_capture = captured(&stdout);
        let stderr_capture = captured(&stderr);
        let output = ProcessOutput {
            stdout: stdout_capture.bytes,
            stderr: stderr_capture.bytes,
            status,
            output_truncated: stdout_capture.truncated || stderr_capture.truncated,
        };
        match reason {
            Some(StopReason::Cancelled) => Err(ProcessError::Cancelled(output)),
            Some(StopReason::TimedOut) => Err(ProcessError::TimedOut(output)),
            Some(StopReason::PipeHeld) => Err(ProcessError::Interrupted {
                reason: StopReason::PipeHeld,
                stopped: true,
                stdout: output.stdout,
                stderr: output.stderr,
                diagnostic: "descendant process held an output pipe open".into(),
            }),
            None if !output.status.success() => Err(ProcessError::NonZeroExit(output)),
            None => Ok(output),
        }
    }
}

impl CancellationToken {
    fn spawn_if_not_cancelled(&self, command: &mut Command) -> io::Result<Option<Child>> {
        let cancelled = self
            .cancelled
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if *cancelled {
            return Ok(None);
        }
        let child = command.spawn()?;
        Ok(Some(child))
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum StopReason {
    Cancelled,
    TimedOut,
    PipeHeld,
}

fn take_pipe(
    child: &mut Child,
    stdout: bool,
    capture: Arc<Mutex<CapturedBytes>>,
) -> Result<thread::JoinHandle<io::Result<()>>, ProcessError> {
    let pipe = if stdout {
        child.stdout.take().map(|pipe| spawn_reader(pipe, capture))
    } else {
        child.stderr.take().map(|pipe| spawn_reader(pipe, capture))
    };
    pipe.ok_or_else(|| ProcessError::Io(io::Error::other("missing output pipe")))
}

fn spawn_reader<R: Read + Send + 'static>(
    mut pipe: R,
    capture: Arc<Mutex<CapturedBytes>>,
) -> thread::JoinHandle<io::Result<()>> {
    thread::spawn(move || {
        let mut buffer = [0; 8192];
        loop {
            let count = pipe.read(&mut buffer)?;
            if count == 0 {
                return Ok(());
            }
            let mut capture = capture
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let available = MAX_CAPTURE_BYTES_PER_STREAM.saturating_sub(capture.bytes.len());
            let retained = count.min(available);
            capture.bytes.extend_from_slice(&buffer[..retained]);
            capture.truncated |= retained < count;
        }
    })
}

#[derive(Default)]
struct CapturedBytes {
    bytes: Vec<u8>,
    truncated: bool,
}

fn captured(capture: &Mutex<CapturedBytes>) -> CapturedBytes {
    capture
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
}

impl Clone for CapturedBytes {
    fn clone(&self) -> Self {
        Self {
            bytes: self.bytes.clone(),
            truncated: self.truncated,
        }
    }
}

fn join_pipe_until(
    handle: thread::JoinHandle<io::Result<()>>,
    deadline: Instant,
) -> Result<(), ()> {
    while !handle.is_finished() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(2));
    }
    if !handle.is_finished() {
        return Err(());
    }
    handle.join().map_err(|_| ())?.map_err(|_| ())
}

#[cfg(unix)]
fn configure_process_group(command: &mut Command) {
    use std::os::unix::process::CommandExt;
    command.process_group(0);
}
#[cfg(windows)]
fn configure_process_group(_command: &mut Command) {}

fn stop_process_group(child: &mut Child, grace: Duration) -> Result<bool, ProcessError> {
    #[cfg(unix)]
    {
        let group = format!("-{}", child.id());
        let term_sent = Command::new("/bin/kill")
            .args(["-TERM", "--", &group])
            .output()
            .is_ok_and(|output| output.status.success());
        if !term_sent {
            if process_group_exists(&group)? {
                let _ = child.kill();
                let _ = child.wait().map_err(ProcessError::Io)?;
                return Ok(false);
            }
            let _ = child.wait().map_err(ProcessError::Io)?;
            return Ok(true);
        }
        let deadline = Instant::now() + grace;
        while Instant::now() < deadline && process_group_exists(&group)? {
            let _ = child.try_wait().map_err(ProcessError::Io)?;
            thread::sleep(Duration::from_millis(5));
        }
        if process_group_exists(&group)? {
            let killed = Command::new("/bin/kill")
                .args(["-KILL", "--", &group])
                .output()
                .is_ok_and(|output| output.status.success());
            if !killed && process_group_exists(&group)? {
                let _ = child.kill();
                let _ = child.wait().map_err(ProcessError::Io)?;
                return Ok(false);
            }
        }
        let deadline = Instant::now() + grace;
        while Instant::now() < deadline && process_group_exists(&group)? {
            let _ = child.try_wait().map_err(ProcessError::Io)?;
            thread::sleep(Duration::from_millis(5));
        }
        if process_group_exists(&group)? {
            return Ok(false);
        }
    }
    #[cfg(windows)]
    {
        let tree_stopped = Command::new("taskkill")
            .args(["/PID", &child.id().to_string(), "/T", "/F"])
            .output()
            .is_ok_and(|output| output.status.success());
        if !tree_stopped {
            let _ = child.kill();
            let _ = child.wait().map_err(ProcessError::Io)?;
            return Ok(false);
        }
    }
    let deadline = Instant::now() + grace;
    loop {
        match child.try_wait().map_err(ProcessError::Io)? {
            Some(_) => return Ok(true),
            None if Instant::now() < deadline => thread::sleep(Duration::from_millis(5)),
            None => return Ok(false),
        }
    }
}

#[cfg(unix)]
fn process_group_exists(group: &str) -> Result<bool, ProcessError> {
    let output = Command::new("/bin/kill")
        .args(["-0", "--", group])
        .output()
        .map_err(ProcessError::Io)?;
    if output.status.success() {
        return Ok(true);
    }
    let diagnostic = String::from_utf8_lossy(&output.stderr);
    Ok(!diagnostic.contains("No such process"))
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
                | Err(ProcessError::Interrupted {
                    reason: StopReason::TimedOut,
                    ..
                })
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
                | Err(ProcessError::Interrupted {
                    reason: StopReason::Cancelled,
                    ..
                })
        ));
    }

    #[cfg(unix)]
    #[test]
    fn timeout_stops_descendants_that_keep_output_pipes_open() {
        let started = Instant::now();
        let request = ProcessRequest::new("sh")
            .args(["-c", "sleep 1 & echo child-started; wait"])
            .timeout(Duration::from_millis(50));
        match ProcessRunner.run(request) {
            Err(ProcessError::TimedOut(output)) => {
                assert!(String::from_utf8_lossy(&output.stdout).contains("child-started"));
            }
            Err(ProcessError::Interrupted {
                reason: StopReason::TimedOut,
                stopped: false,
                stdout,
                ..
            }) => {
                assert!(String::from_utf8_lossy(&stdout).contains("child-started"));
            }
            other => panic!("unexpected result: {other:?}"),
        }
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[cfg(unix)]
    #[test]
    fn caps_captured_output_per_stream() {
        let request = ProcessRequest::new(shell()).args(shell_args(&format!(
            "head -c {} /dev/zero",
            MAX_CAPTURE_BYTES_PER_STREAM + 1024
        )));
        let output = ProcessRunner.run(request).expect("process succeeds");
        assert_eq!(output.stdout.len(), MAX_CAPTURE_BYTES_PER_STREAM);
        assert!(output.output_truncated);
    }

    #[cfg(unix)]
    #[test]
    fn does_not_spawn_when_cancelled_before_start() {
        let token = CancellationToken::new();
        token.cancel();
        let error = ProcessRunner
            .run_with_cancellation(ProcessRequest::new("definitely-not-a-command"), token);
        assert!(matches!(error, Err(ProcessError::CancelledBeforeStart)));
    }
}
