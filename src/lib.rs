#![deny(unsafe_code)]

//! AI Dev Orchestrator core domain.

mod antigravity;
pub mod cli;
mod codex_provider;
mod copilot_provider;
mod domain;
mod execution_ledger;
mod github_workflow;
mod orchestrator;
mod planner;
mod process_runner;
mod provider;
mod retry;
mod validator;
mod workspace;

pub use antigravity::AntigravityProvider;
pub use codex_provider::CodexProvider;
pub use copilot_provider::CopilotProvider;
pub use domain::{
    AgentResult, Attempt, AttemptFailureReason, AttemptId, AttemptState, DomainError, ProviderRef,
    Task, TaskId, TaskRole, TaskState, UsageCost, UsageMetric, ValidationCheckResult,
    ValidationResult,
};
pub use execution_ledger::{AttemptRecord, ExecutionLedger, LedgerError, SqliteExecutionLedger};
pub use github_workflow::{
    CommitRequest, GhIssueSource, GhPullRequestGateway, GhRepositoryEffects, GitHubWorkflow,
    IssueExecutor, IssueRef, IssueSnapshot, IssueSource, PreparedPublication, PublicationLedger,
    PublicationPhase, PublicationRecord, PublishResult, PullRequestGateway, PullRequestPayload,
    PushRequest, RepositoryEffects, ValidatedPublication, WorkflowError, issue_to_task,
    prepare_issue_publication,
};
pub use orchestrator::{
    OrchestrationReport, Orchestrator, OrchestratorError, OrchestratorService, WorkspaceManagerPort,
};
pub use planner::{
    AvailableProvider, CodexPlanner, ExecutionIntent, Planner, PlannerDecision, PlannerError,
    PlannerProvider, PlannerRequest, PlannerService, ProviderAvailability,
    ValidatedPlannerDecision,
};
pub use process_runner::{
    CancellationToken, ProcessError, ProcessOutput, ProcessRequest, ProcessRunner,
};
pub use provider::{AgentProvider, ProviderError, ProviderRequest, ProviderResult};
pub use retry::{
    ExecutionPolicy, PolicyError, ProviderRegistry, ProviderResolutionError, ProviderResolver,
    RetryPolicy,
};
pub use validator::{
    CommandValidator, RustValidator, ValidationCheck, Validator, ValidatorError,
    default_rust_checks,
};
pub use workspace::{GitError, Workspace, WorkspaceError, WorkspaceManager};

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
