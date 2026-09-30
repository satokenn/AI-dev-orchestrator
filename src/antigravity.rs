//! Antigravity CLI adapter.

use std::{
    ffi::{OsStr, OsString},
    fs,
    path::Path,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use serde_json::Value;

use crate::{
    AgentProvider, AgentResult, CancellationToken, ModelChoice, ProcessError, ProcessRequest,
    ProcessRunner, ProviderError, ProviderRef, ProviderRequest, ProviderResult, UsageCost,
    UsageMetric,
};

const DEFAULT_EXECUTABLE: &str = "agy";
const PROVIDER_NAME: &str = "antigravity";
const MODEL_LIST_TIMEOUT: Duration = Duration::from_secs(10);

/// One model identifier printed by `agy models`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModelCatalogEntry {
    pub id: String,
    pub label: String,
}

/// Internal, non-wire provenance for a candidate model catalog observation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ModelCatalogSource {
    ProviderCli,
}

/// A point-in-time listing from the Provider CLI. This is not an entitlement or
/// execution guarantee; callers must keep model availability unknown.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModelCatalogObservation {
    pub entries: Vec<ModelCatalogEntry>,
    /// A fixed, non-sensitive diagnostic; never contains process output.
    pub unknown_reason: Option<&'static str>,
    pub source: ModelCatalogSource,
    pub reference: &'static str,
    pub observed_at_ms: i64,
}

impl ModelCatalogObservation {
    fn unknown(reason: &'static str) -> Self {
        Self {
            entries: Vec::new(),
            unknown_reason: Some(reason),
            source: ModelCatalogSource::ProviderCli,
            reference: "agy models",
            observed_at_ms: now_ms(),
        }
    }
}

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

    /// Lists candidate model IDs reported by `agy models`.
    ///
    /// The CLI's human-readable output is not a stable machine contract. Any
    /// unfamiliar, incomplete, or ambiguous output therefore produces an
    /// unknown observation. A listed ID does not prove account entitlement or
    /// successful execution.
    pub fn observe_model_catalog(&self) -> ModelCatalogObservation {
        let result = self.runner.run(
            ProcessRequest::new(self.executable.clone())
                .arg("models")
                .timeout(MODEL_LIST_TIMEOUT),
        );
        let output = match result {
            Ok(output) if !output.output_truncated => output,
            Ok(_) => return ModelCatalogObservation::unknown("output_truncated"),
            Err(ProcessError::TimedOut(_)) => {
                return ModelCatalogObservation::unknown("process_timed_out");
            }
            Err(ProcessError::Spawn(_)) => {
                return ModelCatalogObservation::unknown("process_spawn_failed");
            }
            Err(ProcessError::Io(_)) => {
                return ModelCatalogObservation::unknown("process_io_failed");
            }
            Err(ProcessError::NonZeroExit(_)) => {
                return ModelCatalogObservation::unknown("process_nonzero_exit");
            }
            Err(ProcessError::Cancelled(_)) | Err(ProcessError::CancelledBeforeStart) => {
                return ModelCatalogObservation::unknown("process_cancelled");
            }
            Err(ProcessError::Interrupted { .. }) => {
                return ModelCatalogObservation::unknown("process_stop_unconfirmed");
            }
        };
        let text = match String::from_utf8(output.stdout) {
            Ok(text) => text,
            Err(_) => return ModelCatalogObservation::unknown("output_not_utf8"),
        };
        match parse_model_catalog(&text) {
            Some(entries) => ModelCatalogObservation {
                entries,
                unknown_reason: None,
                source: ModelCatalogSource::ProviderCli,
                reference: "agy models",
                observed_at_ms: now_ms(),
            },
            None => ModelCatalogObservation::unknown("output_empty_or_ambiguous"),
        }
    }

    /// Executes Antigravity while observing a caller-owned cancellation token.
    pub fn execute_with_cancellation(
        &self,
        request: &ProviderRequest,
        cancellation: CancellationToken,
    ) -> Result<ProviderResult, ProviderError> {
        request.validate_model_selection()?;
        Self::validate_workspace(request.workspace())?;

        let mut process_request = ProcessRequest::new(self.executable.clone()).args([
            "-p",
            request.prompt(),
            "--output-format",
            "json",
        ]);
        if let ModelChoice::Named(model) = request.model() {
            process_request = process_request.args(["--model", model.as_str()]);
        }
        let process_request = process_request
            .cwd(request.workspace())
            .timeout(request.timeout());

        let output = self
            .runner
            .run_with_cancellation(process_request, cancellation)
            .map_err(|error| self.map_process_error(error, request.timeout(), request.model()))?;

        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        let parsed = parse_json_result(&stdout);

        if let Some(error) = parsed.as_ref().and_then(|result| result.error.as_deref()) {
            return Err(self.map_diagnostic_error(error, &stderr, request.model()));
        }

        if parsed
            .as_ref()
            .and_then(|result| result.status.as_deref())
            .is_some_and(|status| !status.eq_ignore_ascii_case("SUCCESS"))
        {
            let diagnostic = format_diagnostic(&stdout, &stderr);
            return Err(self.map_diagnostic_error(&diagnostic, "", request.model()));
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
        )
        .with_observed_target(Some(self.provider_ref.clone()), None))
    }

    fn map_process_error(
        &self,
        error: ProcessError,
        timeout: Duration,
        model: &ModelChoice,
    ) -> ProviderError {
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
                self.map_diagnostic_error(&detail, &stderr, model)
            }
            ProcessError::TimedOut(output) => ProviderError::TimedOutWithOutput {
                timeout,
                stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            },
            ProcessError::Cancelled(output) => ProviderError::CancelledWithOutput {
                stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            },
            ProcessError::CancelledBeforeStart => ProviderError::Cancelled,
            ProcessError::Interrupted {
                reason,
                stopped,
                stdout,
                stderr,
                diagnostic,
            } => ProviderError::Interrupted {
                reason,
                confirmed_stopped: stopped,
                stdout: String::from_utf8_lossy(&stdout).into_owned(),
                stderr: String::from_utf8_lossy(&stderr).into_owned(),
                diagnostic,
            },
        }
    }

    fn map_diagnostic_error(
        &self,
        detail: &str,
        stderr: &str,
        model: &ModelChoice,
    ) -> ProviderError {
        if let Some(error) =
            crate::provider::unsupported_model_error(&self.provider_ref, model, detail)
        {
            error
        } else if is_authentication_error(detail, stderr) {
            ProviderError::Unavailable(detail.to_owned())
        } else {
            ProviderError::ExecutionFailed(detail.to_owned())
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

fn parse_model_catalog(output: &str) -> Option<Vec<ModelCatalogEntry>> {
    let mut lines = output.lines().peekable();
    if lines
        .peek()
        .is_some_and(|line| line.trim_end_matches('\r') == "Fetching available models...")
    {
        lines.next();
    }
    let mut entries = Vec::new();
    for line in lines {
        let line = line.trim_end_matches('\r');
        if line.is_empty() {
            continue;
        }
        let (id, label) = line.split_once('\t')?;
        if !is_model_id(id)
            || label.trim().is_empty()
            || label.chars().any(char::is_control)
            || entries
                .iter()
                .any(|entry: &ModelCatalogEntry| entry.id == id)
        {
            return None;
        }
        entries.push(ModelCatalogEntry {
            id: id.to_owned(),
            label: label.trim().to_owned(),
        });
    }
    (!entries.is_empty()).then_some(entries)
}

fn is_model_id(id: &str) -> bool {
    let mut bytes = id.bytes();
    bytes
        .next()
        .is_some_and(|byte| byte.is_ascii_alphanumeric())
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || b"._:/@+-".contains(&byte))
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}

impl AgentProvider for AntigravityProvider {
    fn provider_ref(&self) -> &ProviderRef {
        &self.provider_ref
    }

    fn execute(&self, request: &ProviderRequest) -> Result<ProviderResult, ProviderError> {
        self.execute_with_cancellation(request, CancellationToken::new())
    }

    fn check_availability(&self) -> Result<(), ProviderError> {
        AntigravityProvider::check_availability(self)
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
        ProcessError::CancelledBeforeStart => "process was cancelled before start".to_owned(),
        ProcessError::Interrupted { diagnostic, .. } => diagnostic,
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

    #[test]
    fn parses_observed_model_catalog_fixture() {
        for fixture in [
            "Fetching available models...\nclaude-sonnet-4\tClaude Sonnet 4\ngemini-2.5-pro\tGemini 2.5 Pro\n",
            "claude-sonnet-4\tClaude Sonnet 4\ngemini-2.5-pro\tGemini 2.5 Pro\n",
        ] {
            let entries = parse_model_catalog(fixture).expect("fixture format");
            assert_eq!(entries.len(), 2);
            assert_eq!(entries[0].id, "claude-sonnet-4");
            assert_eq!(entries[0].label, "Claude Sonnet 4");
        }
        assert_eq!(
            ModelCatalogSource::ProviderCli,
            ModelCatalogObservation::unknown("test").source
        );
    }

    #[test]
    fn rejects_empty_duplicate_and_ambiguous_catalogs() {
        for output in [
            "",
            "Fetching available models...\n",
            "Fetching available models...\nmodel-without-label\n",
            "Fetching available models...\nmodel\tLabel\textra\n",
            "Fetching available models...\nmodel\tFirst\nmodel\tDuplicate\n",
            "Fetching available models...\nmodel\u{1b}secret\tLabel\n",
            "Fetching available models...\nmodel\tLabel\u{7}\n",
            "Fetching available models...\nmodel name\tLabel\n",
            "Unexpected output\nmodel\tLabel\n",
        ] {
            assert!(parse_model_catalog(output).is_none(), "accepted {output:?}");
        }
    }

    #[test]
    fn failed_and_timed_out_catalog_commands_are_unknown() {
        for (script, reason) in [
            (
                "#!/bin/sh\nprintf 'secret-token'\necho 'secret-key' >&2\nexit 7\n",
                "process_nonzero_exit",
            ),
            ("#!/bin/sh\nsleep 20\n", "process_timed_out"),
        ] {
            let path = std::env::temp_dir().join(format!(
                "agy-models-fixture-{}-{}",
                std::process::id(),
                now_ms()
            ));
            std::fs::write(&path, script).expect("write fixture");
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
                    .expect("make executable");
            }
            let observation = AntigravityProvider::with_executable(&path).observe_model_catalog();
            assert!(observation.entries.is_empty());
            assert_eq!(observation.unknown_reason, Some(reason));
            assert!(!observation.unknown_reason.unwrap().contains("secret"));
            let _ = std::fs::remove_file(path);
        }
    }
}
