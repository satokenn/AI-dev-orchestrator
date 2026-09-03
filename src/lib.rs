#![deny(unsafe_code)]

//! AI Dev Orchestrator core domain.

mod domain;

pub use domain::{
    AgentResult, Attempt, AttemptId, AttemptState, DomainError, ProviderRef, Task, TaskId,
    TaskRole, TaskState, UsageCost, UsageMetric, ValidationResult,
};

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
