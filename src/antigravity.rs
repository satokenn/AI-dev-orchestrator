//! Antigravity CLI adapter.

use std::{
    ffi::{OsStr, OsString},
    fs,
    path::Path,
    time::Duration,
};

use serde_json::Value;

use crate::{
    AgentProvider, AgentResult, CancellationToken, ProcessError, ProcessRequest, ProcessRunner,
    ProviderError, ProviderRef, ProviderRequest, ProviderResult, UsageCost, UsageMetric,
};

const DEFAULT_EXECUTABLE: &str = "agy";
const PROVIDER_NAME: &str = "antigravity";

/// Provider for the official Antigravity CLI (`agy`).
#[derive(Clone, Debug)]
pub struct AntigravityProvider {
    executable: OsString,
    runner: ProcessRunner,
    provider_ref: ProviderRef,
}

impl Default for AntigravityProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl AntigravityProvider {
    /// Creates a provider that invokes `agy` from `PATH`.
    #[must_use]
    pub fn new() -> Self {
        Self::with_executable(DEFAULT_EXECUTABLE)
    }

    /// Creates a provider with a custom executable path.
    ///
    /// This is useful for installations with a non-standard command path and
    /// for deterministic tests that use a fake CLI executable.
    #[must_use]
    pub fn with_executable(executable: impl Into<OsString>) -> Self {
        Self {
            executable: executable.into(),
            runner: ProcessRunner,
            provider_ref: ProviderRef::new(PROVIDER_NAME),
        }
    }

    #[must_use]
    pub fn executable(&self) -> &OsStr {
        &self.executable
    }

    /// Checks that the CLI can be started and reports installation problems.
    ///
    /// Authentication is checked by the first headless execution because the
    /// CLI's version command does not contact the model service.
    pub fn check_availability(&self) -> Result<(), ProviderError> {
        self.runner
            .run(ProcessRequest::new(self.executable.clone()).arg("--version"))
            .map(|_| ())
            .map_err(|error| match error {
                ProcessError::Spawn(error) => ProviderError::Unavailable(format_spawn_error(
                    self.executable.as_os_str(),
                    error,
                )),
                other => ProviderError::Unavailable(process_error_message(other)),
            })
    }

    /// Executes Antigravity while observing a caller-owned cancellation token.
    pub fn execute_with_cancellation(
        &self,
        request: &ProviderRequest,
        cancellation: CancellationToken,
    ) -> Result<ProviderResult, ProviderError> {
        Self::validate_workspace(request.workspace())?;

        let process_request = ProcessRequest::new(self.executable.clone())
            .args(["-p", request.prompt(), "--output-format", "json"])
            .cwd(request.workspace())
            .timeout(request.timeout());

        let output = self
            .runner
            .run_with_cancellation(process_request, cancellation)
            .map_err(|error| self.map_process_error(error, request.timeout()))?;

        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        let parsed = parse_json_result(&stdout);

        if let Some(error) = parsed.as_ref().and_then(|result| result.error.as_deref()) {
            return Err(if is_authentication_error(error, &stderr) {
                ProviderError::Unavailable(error.to_owned())
            } else {
                ProviderError::ExecutionFailed(error.to_owned())
            });
        }

        if parsed
            .as_ref()
            .and_then(|result| result.status.as_deref())
            .is_some_and(|status| !status.eq_ignore_ascii_case("SUCCESS"))
        {
            let diagnostic = format_diagnostic(&stdout, &stderr);
            return Err(if is_authentication_error(&diagnostic, "") {
                ProviderError::Unavailable(diagnostic)
            } else {
                ProviderError::ExecutionFailed(diagnostic)
            });
        }

        let agent_result = parsed.as_ref().map_or_else(
            || Some(AgentResult::new(stdout.clone(), true)),
            |result| {
                Some(AgentResult::new(
                    result.response.clone().unwrap_or_else(|| stdout.clone()),
                    result
                        .status
                        .as_deref()
                        .is_none_or(|status| status.eq_ignore_ascii_case("SUCCESS")),
                ))
            },
        );

        Ok(ProviderResult::new(
            stdout,
            stderr,
            output.exit_code(),
            agent_result,
            parsed.and_then(|result| result.usage),
        ))
    }

    fn map_process_error(&self, error: ProcessError, timeout: Duration) -> ProviderError {
        match error {
            ProcessError::Spawn(error) => {
                ProviderError::Unavailable(format_spawn_error(self.executable.as_os_str(), error))
            }
            ProcessError::Io(error) => ProviderError::ExecutionFailed(error.to_string()),
            ProcessError::NonZeroExit(output) => {
                let stdout = String::from_utf8_lossy(&output.stdout);
                let stderr = String::from_utf8_lossy(&output.stderr);
                let detail = parse_json_result(&stdout)
                    .and_then(|result| result.error)
                    .unwrap_or_else(|| format_diagnostic(&stdout, &stderr));
                if is_authentication_error(&detail, &stderr) {
                    ProviderError::Unavailable(detail)
                } else {
                    ProviderError::ExecutionFailed(detail)
                }
            }
            ProcessError::TimedOut(_) => ProviderError::TimedOut { timeout },
            ProcessError::Cancelled(_) => ProviderError::Cancelled,
        }
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
}

impl AgentProvider for AntigravityProvider {
    fn provider_ref(&self) -> &ProviderRef {
        &self.provider_ref
    }

    fn execute(&self, request: &ProviderRequest) -> Result<ProviderResult, ProviderError> {
        self.execute_with_cancellation(request, CancellationToken::new())
    }

    fn execute_with_cancellation(
        &self,
        request: &ProviderRequest,
        cancellation: CancellationToken,
    ) -> Result<ProviderResult, ProviderError> {
        Self::execute_with_cancellation(self, request, cancellation)
    }
}

#[derive(Debug)]
struct ParsedResult {
    status: Option<String>,
    response: Option<String>,
    error: Option<String>,
    usage: Option<UsageCost>,
}

fn parse_json_result(stdout: &str) -> Option<ParsedResult> {
    let value = serde_json::from_str::<Value>(stdout).ok()?;
    let object = value.as_object()?;
    let usage = object.get("usage").and_then(parse_usage);

    Some(ParsedResult {
        status: object
            .get("status")
            .and_then(Value::as_str)
            .map(str::to_owned),
        response: object
            .get("response")
            .and_then(Value::as_str)
            .map(str::to_owned),
        error: object
            .get("error")
            .and_then(Value::as_str)
            .map(str::to_owned),
        usage,
    })
}

fn parse_usage(value: &Value) -> Option<UsageCost> {
    let object = value.as_object()?;
    Some(UsageCost::new(object.iter().map(|(name, value)| {
        UsageMetric::new(name, usage_value(value), "tokens")
    })))
}

fn usage_value(value: &Value) -> String {
    value
        .as_str()
        .map_or_else(|| value.to_string(), str::to_owned)
}

fn is_authentication_error(message: &str, stderr: &str) -> bool {
    let combined = format!("{message}\n{stderr}").to_ascii_lowercase();
    [
        "authentication required",
        "not authenticated",
        "unauthenticated",
        "login required",
        "not logged in",
        "unauthorized",
        "sign in",
        "api key",
        "gemini_api_key",
    ]
    .iter()
    .any(|marker| combined.contains(marker))
}

fn format_spawn_error(executable: &OsStr, error: std::io::Error) -> String {
    if error.kind() == std::io::ErrorKind::NotFound {
        format!(
            "Antigravity CLI '{}' was not found; install it and ensure it is on PATH",
            executable.to_string_lossy()
        )
    } else {
        format!("failed to start {}: {error}", executable.to_string_lossy())
    }
}

fn process_error_message(error: ProcessError) -> String {
    match error {
        ProcessError::Spawn(error) | ProcessError::Io(error) => error.to_string(),
        ProcessError::NonZeroExit(output)
        | ProcessError::TimedOut(output)
        | ProcessError::Cancelled(output) => format_diagnostic(
            &String::from_utf8_lossy(&output.stdout),
            &String::from_utf8_lossy(&output.stderr),
        ),
    }
}

fn format_diagnostic(stdout: &str, stderr: &str) -> String {
    match (stdout.trim(), stderr.trim()) {
        ("", "") => "Antigravity CLI exited without diagnostics".to_owned(),
        (stdout, "") => stdout.to_owned(),
        ("", stderr) => stderr.to_owned(),
        (stdout, stderr) => format!("stdout: {stdout}; stderr: {stderr}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_json_response_and_usage() {
        let result = parse_json_result(
            r#"{"status":"SUCCESS","response":"done","usage":{"input_tokens":12}}"#,
        )
        .expect("valid result");

        assert_eq!(result.status.as_deref(), Some("SUCCESS"));
        assert_eq!(result.response.as_deref(), Some("done"));
        assert_eq!(result.usage.expect("usage").metrics()[0].value(), "12");
    }

    #[test]
    fn recognizes_authentication_diagnostics() {
        assert!(is_authentication_error("authentication required", ""));
        assert!(!is_authentication_error(
            "model failed",
            "permission denied"
        ));
    }
}
