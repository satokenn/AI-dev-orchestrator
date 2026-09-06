//! Codex CLI adapter for the common agent provider contract.

use std::{
    ffi::{OsStr, OsString},
    fs,
    path::Path,
    time::Duration,
};

use crate::{
    AgentProvider, AgentResult, CancellationToken, ProcessError, ProcessRequest, ProcessRunner,
    ProviderError, ProviderRef, ProviderRequest, ProviderResult,
};

const CODEX_COMMAND: &str = "codex";
const CODEX_PROVIDER_NAME: &str = "codex";

/// Executes Codex CLI in non-interactive mode within a requested workspace.
#[derive(Clone, Debug)]
pub struct CodexProvider {
    reference: ProviderRef,
    executable: OsString,
    command_prefix: Vec<OsString>,
    runner: ProcessRunner,
}

impl Default for CodexProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl CodexProvider {
    /// Creates a provider that resolves `codex` through the current `PATH`.
    #[must_use]
    pub fn new() -> Self {
        Self {
            reference: ProviderRef::new(CODEX_PROVIDER_NAME),
            executable: OsString::from(CODEX_COMMAND),
            command_prefix: Vec::new(),
            runner: ProcessRunner,
        }
    }

    /// Creates a provider using a specific executable path.
    #[must_use]
    pub fn with_executable(executable: impl Into<OsString>) -> Self {
        Self {
            executable: executable.into(),
            ..Self::new()
        }
    }

    #[must_use]
    pub fn executable(&self) -> &OsStr {
        &self.executable
    }

    /// Checks whether the Codex CLI can be started.
    ///
    /// Authentication is intentionally checked by the first headless
    /// execution because `codex --version` does not contact the model service.
    pub fn check_availability(&self) -> Result<(), ProviderError> {
        self.runner
            .run(ProcessRequest::new(self.executable.clone()).arg("--version"))
            .map(|_| ())
            .map_err(|error| match error {
                ProcessError::Spawn(error) => ProviderError::Unavailable(format!(
                    "failed to start {}: {error}",
                    self.executable.to_string_lossy()
                )),
                other => ProviderError::Unavailable(format!("Codex CLI unavailable: {other:?}")),
            })
    }

    #[cfg(test)]
    fn with_command_prefix(
        executable: impl Into<OsString>,
        command_prefix: impl IntoIterator<Item = impl Into<OsString>>,
    ) -> Self {
        Self {
            executable: executable.into(),
            command_prefix: command_prefix.into_iter().map(Into::into).collect(),
            ..Self::new()
        }
    }

    fn process_request(&self, request: &ProviderRequest) -> ProcessRequest {
        ProcessRequest::new(self.executable.clone())
            .args(self.command_prefix.iter().cloned())
            .args([
                OsString::from("exec"),
                OsString::from("--json"),
                OsString::from("--sandbox"),
                OsString::from("workspace-write"),
                OsString::from("--ephemeral"),
            ])
            .arg(request.prompt().to_owned())
            .cwd(request.workspace().to_owned())
            .timeout(request.timeout())
    }

    fn validate_workspace(workspace: &Path) -> Result<(), ProviderError> {
        let metadata = fs::metadata(workspace).map_err(|error| {
            ProviderError::InvalidRequest(format!(
                "workspace '{}' cannot be inspected: {error}",
                workspace.display()
            ))
        })?;
        if !metadata.is_dir() {
            return Err(ProviderError::InvalidRequest(format!(
                "workspace '{}' is not a directory",
                workspace.display()
            )));
        }
        Ok(())
    }

    fn execute_process(
        &self,
        request: &ProviderRequest,
        cancellation: CancellationToken,
    ) -> Result<ProviderResult, ProviderError> {
        if request.workspace().as_os_str().is_empty() {
            return Err(ProviderError::InvalidRequest(
                "workspace path must not be empty".to_owned(),
            ));
        }
        Self::validate_workspace(request.workspace())?;
        let output = self
            .runner
            .run_with_cancellation(self.process_request(request), cancellation)
            .map_err(|error| self.map_process_error(error, request.timeout()))?;

        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        let exit_status = output.exit_code();
        let agent_result = Some(AgentResult::new(stdout.clone(), output.status.success()));
        Ok(ProviderResult::new(
            stdout,
            stderr,
            exit_status,
            agent_result,
            None,
        ))
    }

    fn map_process_error(&self, error: ProcessError, timeout: Duration) -> ProviderError {
        match error {
            ProcessError::Spawn(error) if error.kind() == std::io::ErrorKind::NotFound => {
                ProviderError::Unavailable(format!(
                    "Codex CLI '{}' was not found; install it and ensure it is on PATH",
                    self.executable.to_string_lossy()
                ))
            }
            ProcessError::Spawn(error) => {
                ProviderError::Unavailable(format!("Codex CLI could not be started: {error}"))
            }
            ProcessError::Io(error) => {
                ProviderError::ExecutionFailed(format!("Codex process I/O failed: {error}"))
            }
            ProcessError::TimedOut(_) => ProviderError::TimedOut { timeout },
            ProcessError::Cancelled(_) => ProviderError::Cancelled,
            ProcessError::NonZeroExit(output) => {
                let diagnostic =
                    output_diagnostic(&output.stdout, &output.stderr, output.exit_code());
                if looks_like_authentication_failure(&diagnostic) {
                    ProviderError::Unavailable(format!(
                        "Codex CLI authentication failed; run `codex login`: {diagnostic}"
                    ))
                } else {
                    ProviderError::ExecutionFailed(format!(
                        "Codex CLI exited unsuccessfully: {diagnostic}"
                    ))
                }
            }
        }
    }
}

impl AgentProvider for CodexProvider {
    fn provider_ref(&self) -> &ProviderRef {
        &self.reference
    }

    fn execute(&self, request: &ProviderRequest) -> Result<ProviderResult, ProviderError> {
        self.execute_process(request, CancellationToken::new())
    }

    fn execute_with_cancellation(
        &self,
        request: &ProviderRequest,
        cancellation: CancellationToken,
    ) -> Result<ProviderResult, ProviderError> {
        self.execute_process(request, cancellation)
    }
}

fn output_diagnostic(stdout: &[u8], stderr: &[u8], exit_code: Option<i32>) -> String {
    let stdout = String::from_utf8_lossy(stdout).trim().to_owned();
    let stderr = String::from_utf8_lossy(stderr).trim().to_owned();
    let output = match (stdout.is_empty(), stderr.is_empty()) {
        (true, true) => String::from("no diagnostic output"),
        (false, true) => format!("stdout: {stdout}"),
        (true, false) => format!("stderr: {stderr}"),
        (false, false) => format!("stdout: {stdout}; stderr: {stderr}"),
    };
    format!("exit status {exit_code:?}; {output}")
}

fn looks_like_authentication_failure(diagnostic: &str) -> bool {
    let diagnostic = diagnostic.to_ascii_lowercase();
    [
        "not logged in",
        "login required",
        "please log in",
        "authentication",
        "unauthorized",
        "unauthenticated",
        "api key",
        "openai_api_key",
        "codex_api_key",
    ]
    .iter()
    .any(|marker| diagnostic.contains(marker))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{thread, time::Duration};

    fn request(timeout: Duration) -> ProviderRequest {
        ProviderRequest::new(std::env::temp_dir(), "do the task", timeout)
    }

    #[cfg(unix)]
    fn shell_provider(script: &str) -> CodexProvider {
        CodexProvider::with_command_prefix("sh", ["-c", script, "fake-codex"])
    }

    #[test]
    fn reports_provider_identity() {
        assert_eq!(CodexProvider::new().provider_ref().as_str(), "codex");
    }

    #[cfg(unix)]
    #[test]
    fn executes_non_interactively_in_the_requested_workspace() {
        let provider = shell_provider("printf 'out:%s:%s' \"$PWD\" \"$6\"; printf err >&2");
        let result = provider.execute(&request(Duration::from_secs(1))).unwrap();

        assert!(result.stdout().starts_with("out:"));
        assert!(result.stdout().contains("do the task"));
        assert_eq!(result.stderr(), "err");
        assert_eq!(result.exit_status(), Some(0));
        assert!(result.agent_result().unwrap().reported_success());
    }

    #[test]
    fn reports_a_missing_cli_as_unavailable() {
        let provider = CodexProvider::with_executable("codex-command-that-is-not-installed");
        let error = provider
            .execute(&request(Duration::from_secs(1)))
            .unwrap_err();

        assert!(
            matches!(error, ProviderError::Unavailable(message) if message.contains("not found"))
        );
    }

    #[test]
    fn availability_check_reports_a_missing_cli() {
        let provider = CodexProvider::with_executable("codex-command-that-is-not-installed");
        let error = provider.check_availability().unwrap_err();

        assert!(
            matches!(error, ProviderError::Unavailable(message) if message.contains("codex-command-that-is-not-installed"))
        );
    }

    #[cfg(unix)]
    #[test]
    fn diagnoses_authentication_failures() {
        let provider = shell_provider("printf 'Not logged in' >&2; exit 1");
        let error = provider
            .execute(&request(Duration::from_secs(1)))
            .unwrap_err();

        assert!(
            matches!(error, ProviderError::Unavailable(message) if message.contains("codex login"))
        );
    }

    #[cfg(unix)]
    #[test]
    fn diagnoses_other_execution_failures_with_captured_output() {
        let provider = shell_provider("printf 'partial stdout'; printf 'details' >&2; exit 7");
        let error = provider
            .execute(&request(Duration::from_secs(1)))
            .unwrap_err();

        assert!(matches!(
            error,
            ProviderError::ExecutionFailed(message)
                if message.contains("exit status Some(7)")
                    && message.contains("partial stdout")
                    && message.contains("details")
        ));
    }

    #[cfg(unix)]
    #[test]
    fn maps_timeout_and_cancellation_from_process_runner() {
        let provider = shell_provider("sleep 10");
        let timeout = provider
            .execute(&request(Duration::from_millis(20)))
            .unwrap_err();
        assert!(matches!(timeout, ProviderError::TimedOut { .. }));

        let cancellation = CancellationToken::new();
        let other = cancellation.clone();
        let thread = thread::spawn(move || {
            provider.execute_with_cancellation(&request(Duration::from_secs(10)), other)
        });
        thread::sleep(Duration::from_millis(20));
        cancellation.cancel();

        assert!(matches!(
            thread.join().unwrap(),
            Err(ProviderError::Cancelled)
        ));
    }

    #[test]
    fn rejects_a_workspace_that_is_not_a_directory() {
        let provider = CodexProvider::new();
        let request = ProviderRequest::new(
            std::env::temp_dir().join("codex-provider-workspace-does-not-exist"),
            "do the task",
            Duration::from_secs(1),
        );

        assert!(matches!(
            provider.execute(&request),
            Err(ProviderError::InvalidRequest(_))
        ));
    }
}
