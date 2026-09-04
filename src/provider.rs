use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use crate::{AgentResult, ProviderRef, UsageCost};

/// Provider-independent input for one agent execution.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProviderRequest {
    workspace: PathBuf,
    prompt: String,
    timeout: Duration,
}

impl ProviderRequest {
    #[must_use]
    pub fn new(workspace: impl Into<PathBuf>, prompt: impl Into<String>, timeout: Duration) -> Self {
        Self {
            workspace: workspace.into(),
            prompt: prompt.into(),
            timeout,
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
}

/// The minimum provider output needed by the orchestrator.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProviderResult {
    stdout: String,
    stderr: String,
    exit_status: Option<i32>,
    agent_result: Option<AgentResult>,
    usage: Option<UsageCost>,
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
        }
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
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProviderError {
    InvalidRequest(String),
    ExecutionFailed(String),
    TimedOut { timeout: Duration },
    Unavailable(String),
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
            Self::Unavailable(message) => write!(formatter, "provider unavailable: {message}"),
        }
    }
}

impl std::error::Error for ProviderError {}

/// Common boundary for all agent CLI providers.
pub trait AgentProvider {
    fn provider_ref(&self) -> &ProviderRef;
    fn execute(&self, request: &ProviderRequest) -> Result<ProviderResult, ProviderError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeProvider {
        reference: ProviderRef,
    }

    impl AgentProvider for FakeProvider {
        fn provider_ref(&self) -> &ProviderRef {
            &self.reference
        }

        fn execute(&self, request: &ProviderRequest) -> Result<ProviderResult, ProviderError> {
            Ok(ProviderResult::new(
                format!("ran in {}", request.workspace().display()),
                "",
                Some(0),
                Some(AgentResult::new(request.prompt(), true)),
                Some(UsageCost::default()),
            ))
        }
    }

    #[test]
    fn fake_provider_satisfies_the_common_contract() {
        let provider = FakeProvider { reference: ProviderRef::new("codex") };
        let request = ProviderRequest::new(
            "/tmp/worktree",
            "do the task",
            Duration::from_secs(30),
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
    fn provider_result_can_represent_missing_exit_status_and_usage() {
        let result = ProviderResult::new("partial", "timeout", None, None, None);
        assert_eq!(result.exit_status(), None);
        assert!(result.agent_result().is_none());
        assert!(result.usage().is_none());
    }
}
