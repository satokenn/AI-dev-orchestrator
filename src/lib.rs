#![deny(unsafe_code)]

//! AI Dev Orchestrator core domain.

mod antigravity;
mod artifact;
pub mod artifact_publication;
pub mod cli;
mod codex_provider;
mod copilot_provider;
mod domain;
mod execution_ledger;
mod github_workflow;
mod operation_ledger;
mod operation_service;
mod orchestrator;
mod planner;
mod process_runner;
mod provider;
mod retry;
mod validator;
mod workspace;

pub use antigravity::AntigravityProvider;
pub use artifact::{
    ArtifactCodexDecisionRecord, ArtifactPublicationPermit, ArtifactRecord, ArtifactState,
    ArtifactValidationRecord, CodexDecisionKind,
};
pub use artifact_publication::{
    ArtifactPublicationGateway, ArtifactPublicationPayload, DraftPullRequest,
    GitHubArtifactPublicationGateway, PublicationGatewayError, SecretScanError, SecretScanResult,
    SecretScanner,
};
pub use codex_provider::CodexProvider;
pub use copilot_provider::CopilotProvider;
pub use domain::{
    AgentResult, Attempt, AttemptFailureReason, AttemptId, AttemptSemantics, AttemptState,
    DomainError, ModelChoice, ModelRef, ProviderRef, Task, TaskId, TaskRole, TaskState, UsageCost,
    UsageMetric, ValidationCheckResult, ValidationResult,
};
pub use execution_ledger::{AttemptRecord, ExecutionLedger, LedgerError, SqliteExecutionLedger};
pub use github_workflow::{
    CommitRequest, GhIssueSource, GhPullRequestGateway, GhRepositoryEffects, GitHubWorkflow,
    IssueExecutor, IssueRef, IssueSnapshot, IssueSource, PreparedPublication, PublicationLedger,
    PublicationPhase, PublicationRecord, PublishResult, PullRequestGateway, PullRequestPayload,
    PushRequest, RepositoryEffects, ValidatedPublication, WorkflowError, issue_to_task,
    prepare_issue_publication,
};
pub use operation_ledger::{
    EventKind, ExecutionLedger as OperationLedger, LedgerError as OperationLedgerError,
    LogReference, OperationEvent, OperationId, OperationRecord, OperationRequest, OperationStatus,
    PublicationReference, RecoveryRecord, ReviewRecord,
    SqliteExecutionLedger as SqliteOperationLedger, UsageRecord, ValidationRecord,
};
pub use operation_service::{
    ArtifactInput, ArtifactPublicationAcceptance, ArtifactPublicationPhase,
    ArtifactPublicationRequest, ArtifactPublicationSnapshot, AttemptInput, AttemptRunRequest,
    BaseInput, OperationAcceptance, OperationService, OperationSnapshot, ServiceError,
    ServiceOperationStatus, TaskCreateRequest, TaskCreationResult, TaskIssueSnapshot,
    TaskRequestSnapshot, TaskSource,
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
    CancellationToken, ProcessError, ProcessOutput, ProcessRequest, ProcessRunner, StopReason,
};
pub use provider::{AgentProvider, CapturedOutput, ProviderError, ProviderRequest, ProviderResult};
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
