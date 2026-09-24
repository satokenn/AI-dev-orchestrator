//! Codex CLI adapter for the common agent provider contract.

use std::{
    ffi::{OsStr, OsString},
    fs,
    path::Path,
    time::Duration,
};

use serde_json::Value;

use crate::{
    AgentProvider, AgentResult, CancellationToken, CapturedOutput, ModelChoice, ProcessError,
    ProcessRequest, ProcessRunner, ProviderError, ProviderRef, ProviderRequest, ProviderResult,
    UsageCost, UsageMetric,
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
        let mut process = ProcessRequest::new(self.executable.clone())
            .args(self.command_prefix.iter().cloned())
            .args([
                OsString::from("exec"),
                OsString::from("--json"),
                OsString::from("--sandbox"),
                OsString::from("workspace-write"),
                OsString::from("--ephemeral"),
            ]);
        if let ModelChoice::Named(model) = request.model() {
            process = process.args([OsString::from("--model"), OsString::from(model.as_str())]);
        }
        process
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
        request.validate_model_selection()?;
        if request.workspace().as_os_str().is_empty() {
            return Err(ProviderError::InvalidRequest(
                "workspace path must not be empty".to_owned(),
            ));
        }
        Self::validate_workspace(request.workspace())?;
        let output = self
            .runner
            .run_with_cancellation(self.process_request(request), cancellation)
            .map_err(|error| self.map_process_error(error, request.timeout(), request.model()))?;

        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        let exit_status = output.exit_code();
        let captured_output = CapturedOutput::from_process_output(&output);
        let parsed = parse_codex_jsonl(&stdout, output.output_truncated).map_err(|message| {
            ProviderError::ExecutionFailed(message).with_captured_output(captured_output.clone())
        })?;
        let result = ProviderResult::new(
            stdout,
            stderr,
            exit_status,
            parsed.agent_result,
            parsed.usage,
        )
        .with_observed_target(Some(self.reference.clone()), None)
        .with_captured_output(captured_output)
        .with_diagnostics(parsed.diagnostics);
        Ok(result)
    }

    fn map_process_error(
        &self,
        error: ProcessError,
        timeout: Duration,
        model: &ModelChoice,
    ) -> ProviderError {
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
            ProcessError::TimedOut(output) => {
                let captured = CapturedOutput::from_process_output(&output);
                ProviderError::TimedOutWithOutput {
                    timeout,
                    stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
                    stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
                }
                .with_captured_output(captured)
            }
            ProcessError::Cancelled(output) => {
                let captured = CapturedOutput::from_process_output(&output);
                ProviderError::CancelledWithOutput {
                    stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
                    stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
                }
                .with_captured_output(captured)
            }
            ProcessError::CancelledBeforeStart => ProviderError::Cancelled,
            ProcessError::Interrupted {
                reason,
                stopped,
                stdout,
                stderr,
                output_truncated: _,
                stdout_truncated,
                stderr_truncated,
                diagnostic,
            } => ProviderError::Interrupted {
                reason,
                confirmed_stopped: stopped,
                stdout: String::from_utf8_lossy(&stdout).into_owned(),
                stderr: String::from_utf8_lossy(&stderr).into_owned(),
                diagnostic,
            }
            .with_captured_output(CapturedOutput::with_stream_truncation(
                stdout,
                stderr,
                None,
                stdout_truncated,
                stderr_truncated,
            )),
            ProcessError::NonZeroExit(output) => {
                let diagnostic =
                    output_diagnostic(&output.stdout, &output.stderr, output.exit_code());
                let captured = CapturedOutput::from_process_output(&output);
                if let Some(error) =
                    crate::provider::unsupported_model_error(&self.reference, model, &diagnostic)
                {
                    error.with_captured_output(captured)
                } else if looks_like_authentication_failure(&diagnostic) {
                    ProviderError::Unavailable(format!(
                        "Codex CLI authentication failed; run `codex login`: {diagnostic}"
                    ))
                    .with_captured_output(captured)
                } else {
                    ProviderError::ExecutionFailed(format!(
                        "Codex CLI exited unsuccessfully: {diagnostic}"
                    ))
                    .with_captured_output(captured)
                }
            }
        }
    }
}

struct ParsedCodexJsonl {
    agent_result: Option<AgentResult>,
    usage: Option<UsageCost>,
    diagnostics: Vec<String>,
}

/// Parses the stable Codex JSONL events needed by the provider contract.
/// Unknown event types are ignored, and the caller retains the original stream.
fn parse_codex_jsonl(stdout: &str, truncated: bool) -> Result<ParsedCodexJsonl, String> {
    if truncated {
        return Err("Codex JSONL stdout was truncated before parsing".into());
    }
    let mut final_message = None;
    let mut usage_events = Vec::new();
    let mut completed_turn_without_usage = false;
    let mut diagnostics = Vec::new();
    let mut fatal_errors = Vec::new();

    for (index, line) in stdout.lines().enumerate() {
        let line = line.trim_end_matches('\r');
        if line.trim().is_empty() {
            continue;
        }
        let event: Value = serde_json::from_str(line)
            .map_err(|_| format!("Codex CLI emitted malformed JSONL at line {}", index + 1))?;
        let Some(event) = event.as_object() else {
            return Err(format!(
                "Codex JSONL event at line {} is not an object",
                index + 1
            ));
        };
        match event.get("type").and_then(Value::as_str) {
            Some("item.completed") => {
                let Some(item) = event.get("item").and_then(Value::as_object) else {
                    return Err(format!(
                        "Codex item.completed event at line {} has no item object",
                        index + 1
                    ));
                };
                match item.get("type").and_then(Value::as_str) {
                    Some("agent_message") => {
                        let Some(text) = item.get("text").and_then(Value::as_str) else {
                            return Err(format!(
                                "Codex agent_message at line {} has no text",
                                index + 1
                            ));
                        };
                        final_message = Some(text.to_owned());
                    }
                    Some("error") => {
                        if let Some(message) = item.get("message").and_then(Value::as_str) {
                            diagnostics.push(message.to_owned());
                        }
                    }
                    _ => {}
                }
            }
            Some("turn.completed") => {
                if let Some(usage) = event.get("usage") {
                    let Some(usage) = usage.as_object() else {
                        return Err(format!(
                            "Codex turn.completed usage at line {} is not an object",
                            index + 1
                        ));
                    };
                    usage_events.push(usage.clone());
                } else {
                    completed_turn_without_usage = true;
                }
            }
            Some("turn.failed") => {
                let message = event
                    .get("error")
                    .and_then(Value::as_object)
                    .and_then(|error| error.get("message"))
                    .and_then(Value::as_str)
                    .unwrap_or("Codex CLI reported a failed turn");
                fatal_errors.push(message.to_owned());
            }
            Some("error") => {
                let message = event
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("Codex CLI reported an unrecoverable stream error");
                fatal_errors.push(message.to_owned());
            }
            _ => {}
        }
    }

    if !fatal_errors.is_empty() {
        return Err(fatal_errors.join("; "));
    }

    const FIELDS: [&str; 5] = [
        "input_tokens",
        "cached_input_tokens",
        "cache_write_input_tokens",
        "output_tokens",
        "reasoning_output_tokens",
    ];
    let mut metrics = Vec::new();
    if !usage_events.is_empty() && !completed_turn_without_usage {
        for name in FIELDS {
            let mut total = 0_u64;
            let mut known = true;
            for event in &usage_events {
                let Some(value) = event.get(name).and_then(Value::as_i64) else {
                    known = false;
                    break;
                };
                let Ok(value) = u64::try_from(value) else {
                    known = false;
                    break;
                };
                let Some(sum) = total.checked_add(value) else {
                    known = false;
                    break;
                };
                total = sum;
            }
            if known {
                metrics.push(UsageMetric::new(name, total.to_string(), "tokens"));
            }
        }
    }
    let usage = (!metrics.is_empty()).then(|| UsageCost::new(metrics));
    let agent_result = final_message.map(|message| AgentResult::new(message, true));
    Ok(ParsedCodexJsonl {
        agent_result,
        usage,
        diagnostics,
    })
}

impl AgentProvider for CodexProvider {
    fn provider_ref(&self) -> &ProviderRef {
        &self.reference
    }

    fn execute(&self, request: &ProviderRequest) -> Result<ProviderResult, ProviderError> {
        self.execute_process(request, CancellationToken::new())
    }

    fn check_availability(&self) -> Result<(), ProviderError> {
        CodexProvider::check_availability(self)
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
        ProviderRequest::new(
            std::env::temp_dir(),
            "do the task",
            timeout,
            ModelChoice::ProviderDefault,
        )
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
        let provider = shell_provider(
            "printf '{\"type\":\"item.completed\",\"item\":{\"type\":\"agent_message\",\"text\":\"%s:%s\"}}\\n' \"$PWD\" \"$6\"; printf err >&2",
        );
        let result = provider.execute(&request(Duration::from_secs(1))).unwrap();

        assert!(result.agent_result().unwrap().summary().starts_with('/'));
        assert!(
            result
                .agent_result()
                .unwrap()
                .summary()
                .contains("do the task")
        );
        assert_eq!(result.stderr(), "err");
        assert_eq!(result.exit_status(), Some(0));
        assert!(result.agent_result().unwrap().reported_success());
        assert_eq!(result.observed_provider().unwrap().as_str(), "codex");
        assert!(result.observed_model().is_none());
        assert_eq!(
            result.captured_output().unwrap().stdout(),
            result.stdout().as_bytes()
        );
    }

    #[cfg(unix)]
    #[test]
    fn passes_named_model_to_cli_and_types_unsupported_model() {
        let request = ProviderRequest::new(
            std::env::temp_dir(),
            "do the task",
            Duration::from_secs(1),
            ModelChoice::Named(crate::ModelRef::new("gpt-test")),
        );
        let provider = shell_provider(
            "test \"$6\" = --model; test \"$7\" = gpt-test; printf '%s\\n' '{\"type\":\"item.completed\",\"item\":{\"type\":\"agent_message\",\"text\":\"ok\"}}'",
        );
        assert!(provider.execute(&request).is_ok());
        let empty_model_request = ProviderRequest::new(
            std::env::temp_dir(),
            "do the task",
            Duration::from_secs(1),
            ModelChoice::Named(crate::ModelRef::new("")),
        );
        assert!(matches!(
            provider.execute(&empty_model_request),
            Err(ProviderError::InvalidRequest(message)) if message.contains("model identifier")
        ));

        let provider = shell_provider("printf 'unknown model' >&2; exit 1");
        let error = provider.execute(&request).unwrap_err();
        assert!(matches!(
            error.kind(),
            ProviderError::UnsupportedModel { model, .. } if model.as_str() == "gpt-test"
        ));
        assert_eq!(error.captured_output().unwrap().stderr(), b"unknown model");
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
            matches!(error.kind(), ProviderError::Unavailable(message) if message.contains("codex login"))
        );
        assert_eq!(error.captured_output().unwrap().stderr(), b"Not logged in");
    }

    #[cfg(unix)]
    #[test]
    fn diagnoses_other_execution_failures_with_captured_output() {
        let provider = shell_provider("printf 'partial stdout'; printf 'details' >&2; exit 7");
        let error = provider
            .execute(&request(Duration::from_secs(1)))
            .unwrap_err();

        assert!(matches!(
            error.kind(),
            ProviderError::ExecutionFailed(message)
                if message.contains("exit status Some(7)")
                    && message.contains("partial stdout")
                    && message.contains("details")
        ));
        let output = error.captured_output().unwrap();
        assert_eq!(output.stdout(), b"partial stdout");
        assert_eq!(output.stderr(), b"details");
        assert_eq!(output.exit_status(), Some(7));
        assert!(!output.truncated());
    }

    #[cfg(unix)]
    #[test]
    fn maps_timeout_and_cancellation_from_process_runner() {
        let provider = shell_provider("printf partial; exec sleep 10");
        let timeout = provider
            .execute(&request(Duration::from_secs(1)))
            .unwrap_err();
        assert!(
            matches!(timeout.kind(), ProviderError::TimedOutWithOutput { stdout, .. } if stdout == "partial")
        );
        assert_eq!(timeout.captured_output().unwrap().stdout(), b"partial");

        let cancellation = CancellationToken::new();
        let other = cancellation.clone();
        let thread = thread::spawn(move || {
            provider.execute_with_cancellation(&request(Duration::from_secs(10)), other)
        });
        thread::sleep(Duration::from_millis(20));
        cancellation.cancel();

        let cancelled = thread.join().unwrap().unwrap_err();
        assert!(matches!(
            cancelled.kind(),
            ProviderError::CancelledWithOutput { .. }
        ));
        assert_eq!(cancelled.captured_output().unwrap().stdout(), b"partial");
    }

    #[test]
    fn fixture_extracts_final_message_and_all_reported_token_metrics() {
        let parsed =
            parse_codex_jsonl(include_str!("../tests/fixtures/codex/success.jsonl"), false)
                .unwrap();
        assert_eq!(parsed.agent_result.unwrap().summary(), "task complete");
        let metrics = parsed.usage.unwrap();
        let values = metrics
            .metrics()
            .iter()
            .map(|metric| (metric.name(), metric.value(), metric.unit()))
            .collect::<Vec<_>>();
        assert_eq!(
            values,
            vec![
                ("input_tokens", "12", "tokens"),
                ("cached_input_tokens", "3", "tokens"),
                ("cache_write_input_tokens", "2", "tokens"),
                ("output_tokens", "5", "tokens"),
                ("reasoning_output_tokens", "1", "tokens"),
            ]
        );
    }

    #[test]
    fn latest_agent_message_wins_and_unknown_events_are_ignored() {
        let parsed = parse_codex_jsonl(
            "{\"type\":\"future.event\",\"payload\":{}}\n{\"type\":\"item.completed\",\"item\":{\"type\":\"agent_message\",\"text\":\"first\"}}\n{\"type\":\"item.completed\",\"item\":{\"type\":\"agent_message\",\"text\":\"last\"}}\n",
            false,
        ).unwrap();
        assert_eq!(parsed.agent_result.unwrap().summary(), "last");
        assert!(parsed.usage.is_none());
    }

    #[test]
    fn empty_output_and_unknown_usage_remain_unknown() {
        let empty = parse_codex_jsonl("", false).unwrap();
        assert!(empty.agent_result.is_none());
        assert!(empty.usage.is_none());

        let parsed = parse_codex_jsonl(
            "{\"type\":\"turn.completed\",\"usage\":{\"input_tokens\":12,\"output_tokens\":-1}}\n",
            false,
        )
        .unwrap();
        let usage = parsed.usage.unwrap();
        let metrics = usage.metrics();
        assert_eq!(metrics.len(), 1);
        assert_eq!(metrics[0].name(), "input_tokens");
        assert_eq!(metrics[0].value(), "12");
        assert!(parse_codex_jsonl("{broken\n", false).is_err());
        assert!(parse_codex_jsonl("{}", true).is_err());
    }

    #[test]
    #[cfg(unix)]
    fn fatal_jsonl_events_fail_without_losing_the_raw_stream() {
        let provider = shell_provider(
            "printf '%s\\n' '{\"type\":\"turn.failed\",\"error\":{\"message\":\"model failed\"}}'",
        );
        let error = provider
            .execute(&request(Duration::from_secs(1)))
            .unwrap_err();
        assert!(
            matches!(error.kind(), ProviderError::ExecutionFailed(message) if message == "model failed")
        );
        assert!(
            String::from_utf8_lossy(error.captured_output().unwrap().stdout())
                .contains("model failed")
        );
    }

    #[test]
    fn rejects_a_workspace_that_is_not_a_directory() {
        let provider = CodexProvider::new();
        let request = ProviderRequest::new(
            std::env::temp_dir().join("codex-provider-workspace-does-not-exist"),
            "do the task",
            Duration::from_secs(1),
            ModelChoice::ProviderDefault,
        );

        assert!(matches!(
            provider.execute(&request),
            Err(ProviderError::InvalidRequest(_))
        ));
    }
}
