//! GitHub Copilot CLI adapter for the common agent provider contract.

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

const COPILOT_COMMAND: &str = "copilot";
const COPILOT_PROVIDER_NAME: &str = "copilot";
const DEFAULT_ALLOWED_TOOLS: [&str; 2] = ["write", "shell"];

/// Executes GitHub Copilot CLI in non-interactive mode within a requested workspace.
#[derive(Clone, Debug)]
pub struct CopilotProvider {
    reference: ProviderRef,
    executable: OsString,
    command_prefix: Vec<OsString>,
    allowed_tools: Vec<String>,
    runner: ProcessRunner,
}

impl Default for CopilotProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl CopilotProvider {
    /// Creates a provider that resolves `copilot` through the current `PATH`.
    #[must_use]
    pub fn new() -> Self {
        Self {
            reference: ProviderRef::new(COPILOT_PROVIDER_NAME),
            executable: OsString::from(COPILOT_COMMAND),
            command_prefix: Vec::new(),
            allowed_tools: DEFAULT_ALLOWED_TOOLS
                .iter()
                .map(ToString::to_string)
                .collect(),
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

    /// Sets the comma-separated tool permissions passed to Copilot CLI.
    ///
    /// The default is `write,shell`, which lets a coding agent edit files and
    /// run repository commands without an interactive approval prompt. Callers
    /// can provide a narrower set when their workflow does not need both.
    #[must_use]
    pub fn with_allowed_tools(
        mut self,
        allowed_tools: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        self.allowed_tools = allowed_tools.into_iter().map(Into::into).collect();
        self
    }

    #[must_use]
    pub fn executable(&self) -> &OsStr {
        &self.executable
    }

    #[must_use]
    pub fn allowed_tools(&self) -> &[String] {
        &self.allowed_tools
    }

    #[cfg(test)]
    fn with_command_prefix(mut self, script: impl Into<OsString>) -> Self {
        self.executable = OsString::from("sh");
        self.command_prefix = vec!["-c".into(), script.into(), "fake-copilot".into()];
        self
    }

    /// Checks whether the Copilot CLI can be started.
    ///
    /// Authentication is intentionally checked by the first headless
    /// execution because `copilot --version` only checks the local CLI.
    pub fn check_availability(&self) -> Result<(), ProviderError> {
        self.runner
            .run(ProcessRequest::new(self.executable.clone()).arg("--version"))
            .map(|_| ())
            .map_err(|error| match error {
                ProcessError::Spawn(error) => ProviderError::Unavailable(format!(
                    "GitHub Copilot CLI '{}' was not found or could not be started: {error}",
                    self.executable.to_string_lossy()
                )),
                ProcessError::Io(error) => ProviderError::Unavailable(format!(
                    "GitHub Copilot CLI availability check failed: {error}"
                )),
                ProcessError::NonZeroExit(output) => ProviderError::Unavailable(format!(
                    "GitHub Copilot CLI availability check failed: {}",
                    output_diagnostic(&output.stdout, &output.stderr, output.exit_code())
                )),
                ProcessError::TimedOut(_) | ProcessError::Cancelled(_) => {
                    ProviderError::Unavailable(
                        "GitHub Copilot CLI availability check did not complete".to_owned(),
                    )
                }
            })
    }

    fn process_request(&self, request: &ProviderRequest) -> ProcessRequest {
        ProcessRequest::new(self.executable.clone())
            .args(self.command_prefix.iter().cloned())
            .args(["-p", request.prompt(), "-s", "--no-ask-user"])
            .arg(format!("--allow-tool={}", self.allowed_tools.join(",")))
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
        if self.allowed_tools.is_empty() {
            return Err(ProviderError::InvalidRequest(
                "at least one Copilot CLI tool permission is required".to_owned(),
            ));
        }
        Self::validate_workspace(request.workspace())?;

        let output = self
            .runner
            .run_with_cancellation(self.process_request(request), cancellation)
            .map_err(|error| self.map_process_error(error, request.timeout()))?;

        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        let agent_result = Some(AgentResult::new(stdout.clone(), output.status.success()));
        Ok(ProviderResult::new(
            stdout,
            stderr,
            output.exit_code(),
            agent_result,
            None,
        ))
    }

    fn map_process_error(&self, error: ProcessError, timeout: Duration) -> ProviderError {
        match error {
            ProcessError::Spawn(error) if error.kind() == std::io::ErrorKind::NotFound => {
                ProviderError::Unavailable(format!(
                    "GitHub Copilot CLI '{}' was not found; install it and ensure it is on PATH",
                    self.executable.to_string_lossy()
                ))
            }
            ProcessError::Spawn(error) => ProviderError::Unavailable(format!(
                "GitHub Copilot CLI could not be started: {error}"
            )),
            ProcessError::Io(error) => {
                ProviderError::ExecutionFailed(format!("Copilot process I/O failed: {error}"))
            }
            ProcessError::TimedOut(_) => ProviderError::TimedOut { timeout },
            ProcessError::Cancelled(_) => ProviderError::Cancelled,
            ProcessError::NonZeroExit(output) => {
                let diagnostic =
                    output_diagnostic(&output.stdout, &output.stderr, output.exit_code());
                if looks_like_authentication_failure(&diagnostic) {
                    ProviderError::Unavailable(format!(
                        "GitHub Copilot CLI authentication failed; run `copilot` and use `/login`: {diagnostic}"
                    ))
                } else {
                    ProviderError::ExecutionFailed(format!(
                        "GitHub Copilot CLI exited unsuccessfully: {diagnostic}"
                    ))
                }
            }
        }
    }
}

impl AgentProvider for CopilotProvider {
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
        "copilot requests",
        "gh_token",
        "github_token",
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

    #[test]
    fn reports_provider_identity_and_default_permissions() {
        let provider = CopilotProvider::new();
        assert_eq!(provider.provider_ref().as_str(), "copilot");
        assert_eq!(provider.allowed_tools(), ["write", "shell"]);
    }

    #[test]
    fn allows_callers_to_restrict_tool_permissions() {
        let provider = CopilotProvider::new().with_allowed_tools(["write"]);
        assert_eq!(provider.allowed_tools(), ["write"]);
    }

    #[cfg(unix)]
    #[test]
    fn executes_non_interactively_in_the_requested_workspace() {
        let provider = CopilotProvider::with_executable("sh").with_command_prefix(
            "test \"$1\" = \"-p\"; test \"$2\" = \"do the task\"; test \"$3\" = \"-s\"; test \"$4\" = \"--no-ask-user\"; test \"$5\" = \"--allow-tool=write,shell\"; printf 'out:%s' \"$PWD\"; printf err >&2",
        );
        let result = provider.execute(&request(Duration::from_secs(1))).unwrap();
        let expected_workspace = std::fs::canonicalize(std::env::temp_dir()).unwrap();

        assert!(result.stdout().starts_with("out:"));
        assert!(
            result
                .stdout()
                .contains(expected_workspace.to_string_lossy().as_ref())
        );
        assert_eq!(result.stderr(), "err");
        assert_eq!(result.exit_status(), Some(0));
        assert!(result.agent_result().unwrap().reported_success());
    }

    #[test]
    fn reports_a_missing_cli_as_unavailable() {
        let provider = CopilotProvider::with_executable("copilot-command-that-is-not-installed");
        let error = provider
            .execute(&request(Duration::from_secs(1)))
            .unwrap_err();

        assert!(
            matches!(error, ProviderError::Unavailable(message) if message.contains("not found"))
        );
    }

    #[test]
    fn availability_check_reports_a_missing_cli() {
        let provider = CopilotProvider::with_executable("copilot-command-that-is-not-installed");
        let error = provider.check_availability().unwrap_err();

        assert!(matches!(
            error,
            ProviderError::Unavailable(message)
                if message.contains("copilot-command-that-is-not-installed")
        ));
    }

    #[cfg(unix)]
    #[test]
    fn diagnoses_authentication_failures() {
        let provider = CopilotProvider::with_executable("sh")
            .with_command_prefix("printf 'Not logged in' >&2; exit 1");
        let error = provider
            .execute(&request(Duration::from_secs(1)))
            .unwrap_err();

        assert!(matches!(
            error,
            ProviderError::Unavailable(message) if message.contains("/login")
        ));
    }

    #[cfg(unix)]
    #[test]
    fn diagnoses_other_execution_failures_with_captured_output() {
        let provider = CopilotProvider::with_executable("sh")
            .with_command_prefix("printf 'partial stdout'; printf 'details' >&2; exit 7");
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
        let provider = CopilotProvider::with_executable("sh").with_command_prefix("sleep 10");
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
    fn rejects_an_empty_tool_permission_list() {
        let provider = CopilotProvider::new().with_allowed_tools(std::iter::empty::<String>());
        assert!(matches!(
            provider.execute(&request(Duration::from_secs(1))),
            Err(ProviderError::InvalidRequest(message))
                if message.contains("tool permission")
        ));
    }

    #[test]
    fn rejects_a_workspace_that_is_not_a_directory() {
        let provider = CopilotProvider::new();
        let request = ProviderRequest::new(
            std::env::temp_dir().join("copilot-provider-workspace-does-not-exist"),
            "do the task",
            Duration::from_secs(1),
        );

        assert!(matches!(
            provider.execute(&request),
            Err(ProviderError::InvalidRequest(_))
        ));
    }
}
