use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use crate::process_runner::ProcessOutput;
use crate::{AgentResult, CancellationToken, ModelChoice, ModelRef, ProviderRef, UsageCost};

/// Raw output captured from a provider process, before UTF-8 decoding.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CapturedOutput {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    exit_status: Option<i32>,
    stdout_truncated: bool,
    stderr_truncated: bool,
}

impl CapturedOutput {
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
    pub fn stdout(&self) -> &[u8] {
        &self.stdout
    }
    #[must_use]
    pub fn stderr(&self) -> &[u8] {
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
#[derive(Clone, Debug, Eq, PartialEq)]
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
    pub fn with_diagnostics(mut self, diagnostics: impl IntoIterator<Item = String>) -> Self {
        self.diagnostics = diagnostics.into_iter().collect();
        self
    }
    #[must_use]
    pub fn stdout(&self) -> &str {
        &self.stdout
    }
    #[must_use]
    pub fn stderr(&self) -> &str {
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
    pub fn captured_output(&self) -> Option<&CapturedOutput> {
        self.captured_output.as_ref()
    }
    #[must_use]
    pub fn diagnostics(&self) -> &[String] {
        &self.diagnostics
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProviderError {
    InvalidRequest(String),
    ExecutionFailed(String),
    TimedOut {
        timeout: Duration,
    },
    Cancelled,
    TimedOutWithOutput {
        timeout: Duration,
        stdout: String,
        stderr: String,
    },
    CancelledWithOutput {
        stdout: String,
        stderr: String,
    },
    Interrupted {
        reason: crate::StopReason,
        confirmed_stopped: bool,
        stdout: String,
        stderr: String,
        diagnostic: String,
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
    pub fn captured_output(&self) -> Option<&CapturedOutput> {
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
            Self::CancelledWithOutput { .. } => {
                formatter.write_str("provider execution was cancelled")
            }
            Self::Interrupted {
                reason,
                confirmed_stopped,
                diagnostic,
                ..
            } => write!(
                formatter,
                "provider execution interrupted ({reason:?}, stopped={confirmed_stopped}): {diagnostic}"
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
        assert!(result.stdout().contains("/tmp/worktree"));
        assert_eq!(result.stderr(), "");
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
