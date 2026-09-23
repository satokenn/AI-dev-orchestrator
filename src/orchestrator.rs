//! Application service for one Task / Attempt execution.

use std::{fmt, path::Path, time::Duration};

use crate::{
    AgentProvider, Attempt, AttemptId, DomainError, ExecutionPolicy, PlannerDecision, PolicyError,
    ProviderError, ProviderRequest, ProviderResolutionError, ProviderResolver, ProviderResult,
    RetryPolicy, Task, ValidationResult, Validator, ValidatorError, Workspace, WorkspaceError,
    WorkspaceManager,
};

/// Boundary used by the application service to prepare and validate an agent workspace.
pub trait WorkspaceManagerPort {
    fn create_for_task_attempt(
        &self,
        task: &Task,
        attempt: &Attempt,
    ) -> Result<Workspace, WorkspaceError>;
    fn validate_provider_workspace(&self, path: &Path) -> Result<(), WorkspaceError>;
}

impl WorkspaceManagerPort for WorkspaceManager {
    fn create_for_task_attempt(
        &self,
        task: &Task,
        attempt: &Attempt,
    ) -> Result<Workspace, WorkspaceError> {
        WorkspaceManager::create_for_task_attempt(self, task, attempt)
    }

    fn validate_provider_workspace(&self, path: &Path) -> Result<(), WorkspaceError> {
        WorkspaceManager::validate_provider_workspace(self, path)
    }
}

/// The information retained after an agent execution and one aggregate validation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OrchestrationReport {
    attempt: Attempt,
    workspace: Workspace,
    provider_result: ProviderResult,
    validation_result: ValidationResult,
}

impl OrchestrationReport {
    #[must_use]
    pub const fn attempt(&self) -> &Attempt {
        &self.attempt
    }

    #[must_use]
    pub const fn workspace(&self) -> &Workspace {
        &self.workspace
    }

    #[must_use]
    pub const fn provider_result(&self) -> &ProviderResult {
        &self.provider_result
    }

    #[must_use]
    pub const fn validation_result(&self) -> &ValidationResult {
        &self.validation_result
    }
}

/// Failure before a complete Attempt report can be returned.
///
/// The failed Attempt and, once created, its Workspace are included so callers can
/// inspect diagnostics and decide what to do with the retained worktree.
#[derive(Debug)]
pub enum OrchestratorError {
    Domain(DomainError),
    Workspace {
        error: Box<WorkspaceError>,
        attempt: Box<Attempt>,
        workspace: Option<Box<Workspace>>,
    },
    Provider {
        error: ProviderError,
        attempt: Box<Attempt>,
        workspace: Box<Workspace>,
    },
    Validator {
        error: ValidatorError,
        attempt: Box<Attempt>,
        workspace: Box<Workspace>,
    },
    MaxAttempts {
        max_attempts: usize,
    },
    UnknownProvider(ProviderResolutionError),
    RetryNotAllowed,
    Policy(PolicyError),
}

impl OrchestratorError {
    #[must_use]
    pub fn attempt(&self) -> Option<&Attempt> {
        match self {
            Self::Domain(_) => None,
            Self::Workspace { attempt, .. }
            | Self::Provider { attempt, .. }
            | Self::Validator { attempt, .. } => Some(attempt.as_ref()),
            Self::MaxAttempts { .. }
            | Self::UnknownProvider(_)
            | Self::RetryNotAllowed
            | Self::Policy(_) => None,
        }
    }

    #[must_use]
    pub fn workspace(&self) -> Option<&Workspace> {
        match self {
            Self::Domain(_) => None,
            Self::Workspace { workspace, .. } => workspace.as_deref(),
            Self::Provider { workspace, .. } | Self::Validator { workspace, .. } => {
                Some(workspace.as_ref())
            }
            Self::MaxAttempts { .. }
            | Self::UnknownProvider(_)
            | Self::RetryNotAllowed
            | Self::Policy(_) => None,
        }
    }

    #[must_use]
    pub fn provider_error(&self) -> Option<&ProviderError> {
        match self {
            Self::Provider { error, .. } => Some(error),
            _ => None,
        }
    }
}

impl fmt::Display for OrchestratorError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Domain(error) => error.fmt(formatter),
            Self::Workspace { error, .. } => {
                write!(formatter, "workspace preparation failed: {error}")
            }
            Self::Provider { error, .. } => write!(formatter, "provider execution failed: {error}"),
            Self::Validator { error, .. } => {
                write!(formatter, "validation execution failed: {error}")
            }
            Self::MaxAttempts { max_attempts } => {
                write!(formatter, "maximum attempts reached: {max_attempts}")
            }
            Self::UnknownProvider(error) => error.fmt(formatter),
            Self::RetryNotAllowed => formatter.write_str("retry is not allowed by policy"),
            Self::Policy(error) => write!(formatter, "execution policy rejected request: {error}"),
        }
    }
}

impl std::error::Error for OrchestratorError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Domain(error) => Some(error),
            Self::Workspace { error, .. } => Some(error),
            Self::Provider { error, .. } => Some(error),
            Self::Validator { error, .. } => Some(error),
            Self::MaxAttempts { .. } | Self::UnknownProvider(_) | Self::RetryNotAllowed => None,
            Self::Policy(error) => Some(error),
        }
    }
}

/// Coordinates one Provider execution followed by one aggregate validation.
#[derive(Debug)]
pub struct Orchestrator<W, P, V> {
    workspace_manager: W,
    provider: P,
    validator: V,
}

impl<W, P, V> Orchestrator<W, P, V>
where
    W: WorkspaceManagerPort,
    P: AgentProvider,
    V: Validator,
{
    #[must_use]
    pub fn new(workspace_manager: W, provider: P, validator: V) -> Self {
        Self {
            workspace_manager,
            provider,
            validator,
        }
    }

    /// Executes one Attempt using the Task description as the Provider prompt.
    ///
    /// A successful mechanical validation only succeeds the Attempt. The Task remains
    /// Active so a caller can apply its own completion policy. Workspaces are deliberately
    /// not cleaned up here; callers retain the branch and uncommitted diagnostics.
    pub fn execute(
        &self,
        task: &mut Task,
        attempt_id: AttemptId,
        timeout: Duration,
    ) -> Result<OrchestrationReport, OrchestratorError> {
        self.execute_provider(task, attempt_id, timeout, &self.provider)
    }

    fn execute_provider(
        &self,
        task: &mut Task,
        attempt_id: AttemptId,
        timeout: Duration,
        provider: &dyn AgentProvider,
    ) -> Result<OrchestrationReport, OrchestratorError> {
        match task.state() {
            crate::TaskState::Pending => task.start().map_err(OrchestratorError::Domain)?,
            crate::TaskState::Active => {}
            state => {
                return Err(OrchestratorError::Domain(crate::DomainError::TaskClosed {
                    state,
                }));
            }
        }
        task.add_attempt(Attempt::new(
            attempt_id.clone(),
            provider.provider_ref().clone(),
        ))
        .map_err(OrchestratorError::Domain)?;
        task.attempt_mut(&attempt_id)
            .expect("newly added attempt must be owned by its task")
            .start()
            .map_err(OrchestratorError::Domain)?;

        let workspace = match self.workspace_manager.create_for_task_attempt(
            task,
            task.attempt(&attempt_id)
                .expect("newly added attempt must be owned by its task"),
        ) {
            Ok(workspace) => workspace,
            Err(error) => {
                fail_attempt_with_reason(
                    task,
                    &attempt_id,
                    crate::AttemptFailureReason::Workspace,
                )?;
                return Err(OrchestratorError::Workspace {
                    error: Box::new(error),
                    attempt: Box::new(attempt_snapshot(task, &attempt_id)),
                    workspace: None,
                });
            }
        };
        if let Err(error) = self
            .workspace_manager
            .validate_provider_workspace(workspace.path())
        {
            fail_attempt_with_reason(task, &attempt_id, crate::AttemptFailureReason::Workspace)?;
            return Err(OrchestratorError::Workspace {
                error: Box::new(error),
                attempt: Box::new(attempt_snapshot(task, &attempt_id)),
                workspace: Some(Box::new(workspace)),
            });
        }

        let request = ProviderRequest::new(workspace.path(), task.description(), timeout);
        let provider_result = match provider.execute(&request) {
            Ok(result) => result,
            Err(error) => {
                fail_attempt_with_provider_error(task, &attempt_id, &error)?;
                return Err(OrchestratorError::Provider {
                    error,
                    attempt: Box::new(attempt_snapshot(task, &attempt_id)),
                    workspace: Box::new(workspace),
                });
            }
        };

        if let Some(agent_result) = provider_result.agent_result().cloned() {
            task.attempt_mut(&attempt_id)
                .expect("newly added attempt must be owned by its task")
                .record_agent_result(agent_result)
                .map_err(OrchestratorError::Domain)?;
        }
        if let Some(usage) = provider_result.usage().cloned() {
            task.attempt_mut(&attempt_id)
                .expect("newly added attempt must be owned by its task")
                .set_usage_cost(usage);
        }
        task.attempt_mut(&attempt_id)
            .expect("newly added attempt must be owned by its task")
            .finish()
            .map_err(OrchestratorError::Domain)?;
        let validation_result = match self.validator.validate(workspace.path()) {
            Ok(result) => result,
            Err(error) => {
                fail_attempt_with_reason(
                    task,
                    &attempt_id,
                    crate::AttemptFailureReason::Validation,
                )?;
                return Err(OrchestratorError::Validator {
                    error,
                    attempt: Box::new(attempt_snapshot(task, &attempt_id)),
                    workspace: Box::new(workspace),
                });
            }
        };
        task.attempt_mut(&attempt_id)
            .expect("newly added attempt must be owned by its task")
            .apply_validation(validation_result.clone())
            .map_err(OrchestratorError::Domain)?;

        Ok(OrchestrationReport {
            attempt: attempt_snapshot(task, &attempt_id),
            workspace,
            provider_result,
            validation_result,
        })
    }

    /// Alias for callers that name the operation as a run.
    pub fn run(
        &self,
        task: &mut Task,
        attempt_id: AttemptId,
        timeout: Duration,
    ) -> Result<OrchestrationReport, OrchestratorError> {
        self.execute(task, attempt_id, timeout)
    }
}

impl<W, P, V> Orchestrator<W, P, V>
where
    W: WorkspaceManagerPort,
    P: ProviderResolver + AgentProvider,
    V: Validator,
{
    /// Consumes planner intent while keeping IDs, state transitions, and provider
    /// resolution in Rust-owned orchestration code.
    pub fn execute_decision(
        &self,
        task: &mut Task,
        decision: &PlannerDecision,
        attempt_id: AttemptId,
        timeout: Duration,
        policy: RetryPolicy,
    ) -> Result<OrchestrationReport, OrchestratorError> {
        if !policy.is_valid() || task.attempts().len() >= policy.max_attempts() {
            if task.state() == crate::TaskState::Active {
                let _ = task.fail();
            }
            return Err(OrchestratorError::MaxAttempts {
                max_attempts: policy.max_attempts(),
            });
        }
        let provider = self
            .provider
            .resolve(decision.provider())
            .map_err(OrchestratorError::UnknownProvider)?;
        self.execute_provider(task, attempt_id, timeout, provider)
    }

    /// Variant where the orchestrator owns Attempt ID generation.
    pub fn execute_decision_generated(
        &self,
        task: &mut Task,
        decision: &PlannerDecision,
        timeout: Duration,
        policy: RetryPolicy,
    ) -> Result<OrchestrationReport, OrchestratorError> {
        let mut sequence = task.attempts().len() + 1;
        let attempt_id = loop {
            let candidate = AttemptId::new(format!("attempt-{sequence}"));
            if task.attempt(&candidate).is_none() {
                break candidate;
            }
            sequence += 1;
        };
        self.execute_decision(task, decision, attempt_id, timeout, policy)
    }

    /// Hard-gated planner path. Timeout is always selected by `ExecutionPolicy`.
    pub fn execute_decision_with_policy(
        &self,
        task: &mut Task,
        decision: &PlannerDecision,
        attempt_id: AttemptId,
        policy: &ExecutionPolicy,
    ) -> Result<OrchestrationReport, OrchestratorError> {
        // Keep this order: all checks happen before Task/Attempt/workspace mutation.
        if matches!(
            task.state(),
            crate::TaskState::Completed | crate::TaskState::Failed | crate::TaskState::Cancelled
        ) {
            return Err(OrchestratorError::Policy(PolicyError::TaskClosed {
                state: task.state(),
            }));
        }
        if task.attempt(&attempt_id).is_some() {
            return Err(OrchestratorError::Policy(PolicyError::DuplicateAttempt {
                id: attempt_id,
            }));
        }
        if policy.max_attempts() == 0 {
            return Err(OrchestratorError::Policy(PolicyError::MaxAttemptsZero));
        }
        if task.attempts().len() >= policy.max_attempts() {
            return Err(OrchestratorError::Policy(PolicyError::MaxAttemptsReached {
                max_attempts: policy.max_attempts(),
            }));
        }
        let provider = self.provider.resolve(decision.provider()).map_err(|_| {
            OrchestratorError::Policy(PolicyError::UnknownProvider {
                provider: decision.provider().clone(),
            })
        })?;
        let timeout = policy
            .timeout_for(decision.provider())
            .map_err(OrchestratorError::Policy)?;
        provider.check_availability().map_err(|error| {
            OrchestratorError::Policy(PolicyError::ProviderUnavailable {
                provider: decision.provider().clone(),
                reason: error.to_string(),
            })
        })?;
        self.execute_provider(task, attempt_id, timeout, provider)
    }

    /// Preferred hard-gated planner entry. The decision must have been validated
    /// against the immutable planner request before it reaches orchestration.
    pub fn execute_validated_decision_with_policy(
        &self,
        task: &mut Task,
        decision: &crate::ValidatedPlannerDecision,
        attempt_id: AttemptId,
        policy: &ExecutionPolicy,
    ) -> Result<OrchestrationReport, OrchestratorError> {
        self.execute_decision_with_policy(task, decision.decision(), attempt_id, policy)
    }

    /// Generated-ID variant of the hard-gated planner path.
    pub fn execute_decision_generated_with_policy(
        &self,
        task: &mut Task,
        decision: &PlannerDecision,
        policy: &ExecutionPolicy,
    ) -> Result<OrchestrationReport, OrchestratorError> {
        let mut sequence = task.attempts().len() + 1;
        loop {
            let id = AttemptId::new(format!("attempt-{sequence}"));
            if task.attempt(&id).is_none() {
                return self.execute_decision_with_policy(task, decision, id, policy);
            }
            sequence += 1;
        }
    }

    /// Hard-gated retry using the provider and timeout selected by the policy.
    pub fn retry_after_with_policy(
        &self,
        task: &mut Task,
        failure: &OrchestratorError,
        attempt_id: AttemptId,
        policy: &ExecutionPolicy,
    ) -> Result<OrchestrationReport, OrchestratorError> {
        let source = failure
            .attempt()
            .ok_or(OrchestratorError::RetryNotAllowed)?;
        if !matches!(
            source.state(),
            crate::AttemptState::Failed | crate::AttemptState::Cancelled
        ) {
            return Err(OrchestratorError::RetryNotAllowed);
        }
        let provider = task
            .attempts()
            .last()
            .map(|attempt| attempt.provider().clone())
            .ok_or(OrchestratorError::RetryNotAllowed)?;
        self.execute_decision_with_policy(
            task,
            &PlannerDecision::execute(provider, "retry"),
            attempt_id,
            policy,
        )
    }

    /// Hard-gated escalation to a planner-selected provider.
    pub fn escalate_with_policy(
        &self,
        task: &mut Task,
        provider: crate::ProviderRef,
        attempt_id: AttemptId,
        policy: &ExecutionPolicy,
    ) -> Result<OrchestrationReport, OrchestratorError> {
        self.execute_decision_with_policy(
            task,
            &PlannerDecision::execute(provider, "escalation"),
            attempt_id,
            policy,
        )
    }

    /// Starts the next attempt only when the preceding provider failure is allowed
    /// by policy. The failed attempt remains untouched and the new ID gets a new workspace.
    pub fn retry_after(
        &self,
        task: &mut Task,
        failure: &OrchestratorError,
        attempt_id: AttemptId,
        timeout: Duration,
        policy: RetryPolicy,
    ) -> Result<OrchestrationReport, OrchestratorError> {
        let source = failure
            .attempt()
            .ok_or(OrchestratorError::RetryNotAllowed)?;
        if !matches!(
            source.state(),
            crate::AttemptState::Failed | crate::AttemptState::Cancelled
        ) {
            return Err(OrchestratorError::RetryNotAllowed);
        }
        if let Some(error) = failure.provider_error() {
            if !policy.allows(error) {
                return Err(OrchestratorError::RetryNotAllowed);
            }
        }
        let provider = task
            .attempts()
            .last()
            .map(|attempt| attempt.provider().clone())
            .ok_or(OrchestratorError::RetryNotAllowed)?;
        self.execute_decision(
            task,
            &PlannerDecision::execute(provider, "retry"),
            attempt_id,
            timeout,
            policy,
        )
    }

    /// Starts a new attempt against the provider selected by the planner.
    pub fn escalate(
        &self,
        task: &mut Task,
        provider: crate::ProviderRef,
        attempt_id: AttemptId,
        timeout: Duration,
        policy: RetryPolicy,
    ) -> Result<OrchestrationReport, OrchestratorError> {
        self.execute_decision(
            task,
            &PlannerDecision::execute(provider, "escalation"),
            attempt_id,
            timeout,
            policy,
        )
    }
}

/// Name emphasizing that this type is an application service.
pub type OrchestratorService<W, P, V> = Orchestrator<W, P, V>;

fn attempt_snapshot(task: &Task, attempt_id: &AttemptId) -> Attempt {
    task.attempt(attempt_id)
        .expect("newly added attempt must be owned by its task")
        .clone()
}

fn fail_attempt_with_reason(
    task: &mut Task,
    attempt_id: &AttemptId,
    reason: crate::AttemptFailureReason,
) -> Result<(), OrchestratorError> {
    task.attempt_mut(attempt_id)
        .expect("newly added attempt must be owned by its task")
        .fail_with_reason(reason)
        .map_err(OrchestratorError::Domain)
}

fn fail_attempt_with_provider_error(
    task: &mut Task,
    attempt_id: &AttemptId,
    error: &ProviderError,
) -> Result<(), OrchestratorError> {
    let attempt = task
        .attempt_mut(attempt_id)
        .expect("newly added attempt must be owned by its task");
    if matches!(error, ProviderError::Cancelled) {
        attempt
            .cancel_with_reason(crate::AttemptFailureReason::Cancelled)
            .map_err(OrchestratorError::Domain)
    } else if matches!(error, ProviderError::TimedOut { .. }) {
        attempt
            .fail_with_reason(crate::AttemptFailureReason::Timeout)
            .map_err(OrchestratorError::Domain)
    } else {
        attempt
            .fail_with_reason(crate::AttemptFailureReason::Provider)
            .map_err(OrchestratorError::Domain)
    }
}
