use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use crate::process_runner::ProcessOutput;
use crate::{AgentResult, CancellationToken, ModelChoice, ModelRef, ProviderRef, UsageCost};

/// Raw output captured from a provider process, before UTF-8 decoding.
#[derive(Clone, Eq, PartialEq)]
pub struct CapturedOutput {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    exit_status: Option<i32>,
    stdout_truncated: bool,
    stderr_truncated: bool,
}

impl CapturedOutput {
    /// Explicitly exposes sensitive, unredacted process bytes to a trusted caller.
    /// The caller must redact known secrets before persistence or returning the bytes.
    #[must_use]
    pub fn expose_raw_bytes_for_trusted_processing(&self) -> (&[u8], &[u8]) {
        (&self.stdout, &self.stderr)
    }
    #[must_use]
    pub fn new(
        stdout: Vec<u8>,
        stderr: Vec<u8>,
        exit_status: Option<i32>,
        truncated: bool,
    ) -> Self {
        Self::with_stream_truncation(stdout, stderr, exit_status, truncated, truncated)
    }
    #[must_use]
    pub fn with_stream_truncation(
        stdout: Vec<u8>,
        stderr: Vec<u8>,
        exit_status: Option<i32>,
        stdout_truncated: bool,
        stderr_truncated: bool,
    ) -> Self {
        Self {
            stdout,
            stderr,
            exit_status,
            stdout_truncated,
            stderr_truncated,
        }
    }
    #[must_use]
    pub fn from_process_output(output: &ProcessOutput) -> Self {
        Self::with_stream_truncation(
            output.stdout.clone(),
            output.stderr.clone(),
            output.exit_code(),
            output.stdout_truncated,
            output.stderr_truncated,
        )
    }
    #[must_use]
    pub fn expose_stdout_bytes_for_trusted_processing(&self) -> &[u8] {
        &self.stdout
    }
    #[must_use]
    pub fn expose_stderr_bytes_for_trusted_processing(&self) -> &[u8] {
        &self.stderr
    }
    #[must_use]
    pub const fn exit_status(&self) -> Option<i32> {
        self.exit_status
    }
    #[must_use]
    pub const fn truncated(&self) -> bool {
        self.stdout_truncated || self.stderr_truncated
    }
    #[must_use]
    pub const fn stdout_truncated(&self) -> bool {
        self.stdout_truncated
    }
    #[must_use]
    pub const fn stderr_truncated(&self) -> bool {
        self.stderr_truncated
    }
}

impl std::fmt::Debug for CapturedOutput {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CapturedOutput")
            .field("stdout_bytes", &self.stdout.len())
            .field("stderr_bytes", &self.stderr.len())
            .field("exit_status", &self.exit_status)
            .field("stdout_truncated", &self.stdout_truncated)
            .field("stderr_truncated", &self.stderr_truncated)
            .finish()
    }
}

/// Provider-independent input for one agent execution.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProviderRequest {
    workspace: PathBuf,
    prompt: String,
    timeout: Duration,
    model: ModelChoice,
}

impl ProviderRequest {
    #[must_use]
    pub fn new(
        workspace: impl Into<PathBuf>,
        prompt: impl Into<String>,
        timeout: Duration,
        model: ModelChoice,
    ) -> Self {
        Self {
            workspace: workspace.into(),
            prompt: prompt.into(),
            timeout,
            model,
        }
    }
    #[must_use]
    pub fn workspace(&self) -> &Path {
        &self.workspace
    }
    #[must_use]
    pub fn prompt(&self) -> &str {
        &self.prompt
    }
    #[must_use]
    pub const fn timeout(&self) -> Duration {
        self.timeout
    }
    #[must_use]
    pub const fn model(&self) -> &ModelChoice {
        &self.model
    }
    pub(crate) fn validate_model_selection(&self) -> Result<(), ProviderError> {
        if matches!(&self.model, ModelChoice::Named(model) if model.as_str().is_empty()) {
            return Err(ProviderError::InvalidRequest(
                "named model identifier must not be empty".into(),
            ));
        }
        Ok(())
    }
}

/// The minimum provider output needed by the orchestrator.
#[derive(Clone, Eq, PartialEq)]
pub struct ProviderResult {
    stdout: String,
    stderr: String,
    exit_status: Option<i32>,
    agent_result: Option<AgentResult>,
    usage: Option<UsageCost>,
    observed_provider: Option<ProviderRef>,
    observed_model: Option<ModelRef>,
    captured_output: Option<CapturedOutput>,
    diagnostics: Vec<String>,
}

impl ProviderResult {
    #[must_use]
    pub fn new(
        stdout: impl Into<String>,
        stderr: impl Into<String>,
        exit_status: Option<i32>,
        agent_result: Option<AgentResult>,
        usage: Option<UsageCost>,
    ) -> Self {
        Self {
            stdout: stdout.into(),
            stderr: stderr.into(),
            exit_status,
            agent_result,
            usage,
            observed_provider: None,
            observed_model: None,
            captured_output: None,
            diagnostics: Vec::new(),
        }
    }
    #[must_use]
    pub fn with_observed_target(
        mut self,
        provider: Option<ProviderRef>,
        model: Option<ModelRef>,
    ) -> Self {
        self.observed_provider = provider;
        self.observed_model = model;
        self
    }
    #[must_use]
    pub fn with_captured_output(mut self, output: CapturedOutput) -> Self {
        self.captured_output = Some(output);
        self
    }
    #[must_use]
    /// Adds safe, redacted diagnostic summaries; raw provider output must not be stored here.
    pub fn with_diagnostics(mut self, diagnostics: impl IntoIterator<Item = String>) -> Self {
        self.diagnostics = diagnostics.into_iter().collect();
        self
    }
    #[must_use]
    /// Explicitly exposes potentially sensitive provider output to a trusted caller.
    /// Redact known secrets before persistence or returning the text.
    pub fn expose_stdout_for_trusted_processing(&self) -> &str {
        &self.stdout
    }
    #[must_use]
    /// Explicitly exposes potentially sensitive provider output to a trusted caller.
    /// Redact known secrets before persistence or returning the text.
    pub fn expose_stderr_for_trusted_processing(&self) -> &str {
        &self.stderr
    }
    #[must_use]
    pub const fn exit_status(&self) -> Option<i32> {
        self.exit_status
    }
    #[must_use]
    pub fn agent_result(&self) -> Option<&AgentResult> {
        self.agent_result.as_ref()
    }
    #[must_use]
    pub fn usage(&self) -> Option<&UsageCost> {
        self.usage.as_ref()
    }
    #[must_use]
    pub fn observed_provider(&self) -> Option<&ProviderRef> {
        self.observed_provider.as_ref()
    }
    #[must_use]
    pub fn observed_model(&self) -> Option<&ModelRef> {
        self.observed_model.as_ref()
    }
    #[must_use]
    pub fn expose_captured_output_for_trusted_processing(&self) -> Option<&CapturedOutput> {
        self.captured_output.as_ref()
    }
    #[must_use]
    pub fn diagnostic_summaries(&self) -> &[String] {
        &self.diagnostics
    }
}

impl std::fmt::Debug for ProviderResult {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ProviderResult")
            .field("stdout", &"<redacted>")
            .field("stderr", &"<redacted>")
            .field("exit_status", &self.exit_status)
            .field("agent_result", &"<redacted>")
            .field("usage", &self.usage)
            .field("observed_provider", &self.observed_provider)
            .field("observed_model", &self.observed_model)
            .field("captured_output", &self.captured_output)
            .field("diagnostic_count", &self.diagnostics.len())
            .finish()
    }
}

#[derive(Clone, Eq, PartialEq)]
pub enum ProviderError {
    InvalidRequest(String),
    ExecutionFailed(String),
    TimedOut {
        timeout: Duration,
    },
    Cancelled,
    TimedOutWithOutput {
        timeout: Duration,
    },
    CancelledWithOutput,
    Interrupted {
        reason: crate::StopReason,
        confirmed_stopped: bool,
    },
    Unavailable(String),
    UnsupportedModel {
        provider: ProviderRef,
        model: ModelRef,
    },
    /// Wraps a semantic provider error with the unmodified process streams.
    WithCapturedOutput {
        error: Box<ProviderError>,
        output: CapturedOutput,
    },
}

impl ProviderError {
    #[must_use]
    pub fn with_captured_output(self, output: CapturedOutput) -> Self {
        Self::WithCapturedOutput {
            error: Box::new(self),
            output,
        }
    }
    #[must_use]
    /// Explicitly exposes sensitive, unredacted process bytes to a trusted caller.
    /// Redact known secrets before persistence or returning any exposed bytes.
    pub fn expose_captured_output_for_trusted_processing(&self) -> Option<&CapturedOutput> {
        match self {
            Self::WithCapturedOutput { output, .. } => Some(output),
            _ => None,
        }
    }
    /// Returns the semantic error variant, unwrapping any captured-output envelope.
    #[must_use]
    pub fn kind(&self) -> &ProviderError {
        match self {
            Self::WithCapturedOutput { error, .. } => error.kind(),
            _ => self,
        }
    }
}

impl std::fmt::Debug for ProviderError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::WithCapturedOutput { error, output } => formatter
                .debug_struct("ProviderError::WithCapturedOutput")
                .field("error", error)
                .field("captured_output", output)
                .finish(),
            Self::InvalidRequest(_) => formatter
                .debug_tuple("ProviderError::InvalidRequest")
                .field(&"<redacted>")
                .finish(),
            Self::ExecutionFailed(_) => formatter
                .debug_tuple("ProviderError::ExecutionFailed")
                .field(&"<redacted>")
                .finish(),
            Self::Unavailable(_) => formatter
                .debug_tuple("ProviderError::Unavailable")
                .field(&"<redacted>")
                .finish(),
            Self::TimedOut { timeout } => formatter
                .debug_struct("ProviderError::TimedOut")
                .field("timeout", timeout)
                .finish(),
            Self::Cancelled => formatter.write_str("ProviderError::Cancelled"),
            Self::TimedOutWithOutput { timeout } => formatter
                .debug_struct("ProviderError::TimedOutWithOutput")
                .field("timeout", timeout)
                .finish(),
            Self::CancelledWithOutput => formatter.write_str("ProviderError::CancelledWithOutput"),
            Self::Interrupted {
                reason,
                confirmed_stopped,
            } => formatter
                .debug_struct("ProviderError::Interrupted")
                .field("reason", reason)
                .field("confirmed_stopped", confirmed_stopped)
                .finish(),
            Self::UnsupportedModel { provider, model } => formatter
                .debug_struct("ProviderError::UnsupportedModel")
                .field("provider", provider)
                .field("model", model)
                .finish(),
        }
    }
}

pub(crate) fn unsupported_model_error(
    provider: &ProviderRef,
    choice: &ModelChoice,
    diagnostic: &str,
) -> Option<ProviderError> {
    let ModelChoice::Named(model) = choice else {
        return None;
    };
    let diagnostic = diagnostic.to_ascii_lowercase();
    let recognized = [
        "invalid model selection",
        "unknown model",
        "unsupported model",
        "model not supported",
        "model is not supported",
        "model is not recognized",
        "model does not exist",
        "model not found",
    ]
    .iter()
    .any(|marker| diagnostic.contains(marker));
    recognized.then(|| ProviderError::UnsupportedModel {
        provider: provider.clone(),
        model: model.clone(),
    })
}

impl std::fmt::Display for ProviderError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidRequest(message) => {
                write!(formatter, "invalid provider request: {message}")
            }
            Self::ExecutionFailed(message) => {
                write!(formatter, "provider execution failed: {message}")
            }
            Self::TimedOut { timeout } => {
                write!(formatter, "provider timed out after {timeout:?}")
            }
            Self::Cancelled => formatter.write_str("provider execution was cancelled"),
            Self::TimedOutWithOutput { timeout, .. } => {
                write!(formatter, "provider timed out after {timeout:?}")
            }
            Self::CancelledWithOutput => formatter.write_str("provider execution was cancelled"),
            Self::Interrupted {
                reason,
                confirmed_stopped,
                ..
            } => write!(
                formatter,
                "provider execution interrupted ({reason:?}, stopped={confirmed_stopped})"
            ),
            Self::Unavailable(message) => write!(formatter, "provider unavailable: {message}"),
            Self::UnsupportedModel { provider, model } => write!(
                formatter,
                "provider {} does not support model {}",
                provider.as_str(),
                model.as_str()
            ),
            Self::WithCapturedOutput { error, .. } => write!(formatter, "{error}"),
        }
    }
}

impl std::error::Error for ProviderError {}

/// Common boundary for all agent CLI providers.
pub trait AgentProvider {
    fn provider_ref(&self) -> &ProviderRef;
    fn execute(&self, request: &ProviderRequest) -> Result<ProviderResult, ProviderError>;

    /// Checks provider availability before an attempt is created.
    fn check_availability(&self) -> Result<(), ProviderError>;

    /// Captures current, non-sensitive Provider facts for read-only Task context.
    /// Implementations without an authoritative probe report unknown values.
    fn observe_current_at(&self, observed_at_ms: i64) -> crate::ProviderObservation {
        crate::ProviderObservation::unsupported_adapter(self.provider_ref().clone(), observed_at_ms)
    }

    /// Executes a request while observing a caller-owned cancellation signal.
    ///
    /// Providers that support process cancellation should override this method.
    /// The default preserves compatibility for providers whose execution model
    /// cannot yet be interrupted.
    fn execute_with_cancellation(
        &self,
        request: &ProviderRequest,
        _cancellation: CancellationToken,
    ) -> Result<ProviderResult, ProviderError> {
        self.execute(request)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeProvider {
        reference: ProviderRef,
        supported_model: Option<&'static str>,
    }

    #[test]
    fn debug_and_display_never_include_provider_output() {
        let secret = "sentinel-provider-secret";
        let captured = CapturedOutput::new(
            secret.as_bytes().to_vec(),
            b"stderr sentinel".to_vec(),
            Some(7),
            false,
        );
        let result = ProviderResult::new(
            secret,
            "stderr sentinel",
            Some(7),
            Some(AgentResult::new(secret, false)),
            None,
        )
        .with_captured_output(captured.clone());
        let rendered_result = format!("{result:?}");
        assert!(!rendered_result.contains(secret));
        assert!(!rendered_result.contains("stderr sentinel"));

        let error = ProviderError::ExecutionFailed("provider process failed".into())
            .with_captured_output(captured);
        assert!(!format!("{error:?}").contains(secret));
        assert!(!format!("{error:?}").contains("stderr sentinel"));
        assert!(!format!("{error}").contains(secret));
        assert!(!format!("{error}").contains("stderr sentinel"));

        let caller_constructed = ProviderError::ExecutionFailed(secret.into());
        assert!(!format!("{caller_constructed:?}").contains(secret));
    }

    impl AgentProvider for FakeProvider {
        fn provider_ref(&self) -> &ProviderRef {
            &self.reference
        }

        fn execute(&self, request: &ProviderRequest) -> Result<ProviderResult, ProviderError> {
            if let ModelChoice::Named(model) = request.model() {
                if self.supported_model != Some(model.as_str()) {
                    return Err(ProviderError::UnsupportedModel {
                        provider: self.reference.clone(),
                        model: model.clone(),
                    });
                }
            }
            let observed_model = match request.model() {
                ModelChoice::Named(model) => Some(model.clone()),
                ModelChoice::ProviderDefault => None,
            };
            Ok(ProviderResult::new(
                format!("ran in {}", request.workspace().display()),
                "",
                Some(0),
                Some(AgentResult::new(request.prompt(), true)),
                Some(UsageCost::default()),
            )
            .with_observed_target(Some(self.reference.clone()), observed_model))
        }

        fn check_availability(&self) -> Result<(), ProviderError> {
            Ok(())
        }
    }

    #[test]
    fn fake_provider_satisfies_the_common_contract() {
        let provider = FakeProvider {
            reference: ProviderRef::new("codex"),
            supported_model: Some("gpt-test"),
        };
        let request = ProviderRequest::new(
            "/tmp/worktree",
            "do the task",
            Duration::from_secs(30),
            ModelChoice::ProviderDefault,
        );
        let result = provider.execute(&request).unwrap();

        assert_eq!(provider.provider_ref().as_str(), "codex");
        assert!(
            result
                .expose_stdout_for_trusted_processing()
                .contains("/tmp/worktree")
        );
        assert_eq!(result.expose_stderr_for_trusted_processing(), "");
        assert_eq!(result.exit_status(), Some(0));
        assert_eq!(result.agent_result().unwrap().summary(), "do the task");
        assert!(result.usage().is_some());
    }

    #[test]
    fn provider_error_describes_cancellation() {
        assert_eq!(
            ProviderError::Cancelled.to_string(),
            "provider execution was cancelled"
        );
    }

    #[test]
    fn provider_result_can_represent_missing_exit_status_and_usage() {
        let result = ProviderResult::new("partial", "timeout", None, None, None);
        assert_eq!(result.exit_status(), None);
        assert!(result.agent_result().is_none());
        assert!(result.usage().is_none());
    }

    #[test]
    fn fake_provider_contract_separates_requested_and_observed_model() {
        let provider = FakeProvider {
            reference: ProviderRef::new("fake"),
            supported_model: Some("gpt-test"),
        };
        let named_request = ProviderRequest::new(
            "/tmp/worktree",
            "do the task",
            Duration::from_secs(30),
            ModelChoice::Named(ModelRef::new("gpt-test")),
        );
        let named_result = provider.execute(&named_request).unwrap();
        assert_eq!(
            named_request.model(),
            &ModelChoice::Named(ModelRef::new("gpt-test"))
        );
        assert_eq!(named_result.observed_model().unwrap().as_str(), "gpt-test");

        let default_request = ProviderRequest::new(
            "/tmp/worktree",
            "do the task",
            Duration::from_secs(30),
            ModelChoice::ProviderDefault,
        );
        let default_result = provider.execute(&default_request).unwrap();
        assert_eq!(default_request.model(), &ModelChoice::ProviderDefault);
        assert!(default_result.observed_model().is_none());

        let unsupported_request = ProviderRequest::new(
            "/tmp/worktree",
            "do the task",
            Duration::from_secs(30),
            ModelChoice::Named(ModelRef::new("unknown-model")),
        );
        assert!(matches!(
            provider.execute(&unsupported_request),
            Err(ProviderError::UnsupportedModel { model, .. }) if model.as_str() == "unknown-model"
        ));
    }
}
