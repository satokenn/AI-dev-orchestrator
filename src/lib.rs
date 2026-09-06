#![deny(unsafe_code)]

//! AI Dev Orchestrator core domain.

mod codex_provider;
mod domain;
mod process_runner;
mod provider;

pub use codex_provider::CodexProvider;
pub use domain::{
    AgentResult, Attempt, AttemptId, AttemptState, DomainError, ProviderRef, Task, TaskId,
    TaskRole, TaskState, UsageCost, UsageMetric, ValidationResult,
};
pub use process_runner::{
    CancellationToken, ProcessError, ProcessOutput, ProcessRequest, ProcessRunner,
};
pub use provider::{AgentProvider, ProviderError, ProviderRequest, ProviderResult};

/// Reports whether the workspace crate is available.
#[must_use]
pub const fn is_ready() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::is_ready;

    #[test]
    fn workspace_is_ready() {
        assert!(is_ready());
    }
}
