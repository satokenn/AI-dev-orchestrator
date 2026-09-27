//! A single request boundary for supervisor-directed Provider attempts.
//!
//! This core service accepts explicit Provider / Model and BaseInput requests.
//! It does not select a target or expose Provider output and raw diagnostics.

use std::{path::PathBuf, time::Duration};

use rusqlite::{OptionalExtension, TransactionBehavior, params};

use crate::{
    Attempt, AttemptFailureReason, AttemptId, AttemptSemantics, AttemptState, CancellationToken,
    DomainError, LedgerError, ModelChoice, ModelRef, OperationId, ProviderError, ProviderRef,
    ProviderRequest, ProviderResolver, SqliteExecutionLedger, TaskId, TaskRole, TaskState,
    UsageCost, UsageMetric, WorkspaceError, WorkspaceManager,
    artifact::{ArtifactError, ArtifactManager},
    execution_ledger::{
        attempt_state_to_str, failure_reason_to_str, model_choice_kind, model_choice_name,
        task_state_to_str,
    },
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BaseInput {
    repository: PathBuf,
    commit: String,
}

/// A previously captured Task artifact selected as this Attempt's input.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ArtifactInput {
    artifact_id: String,
}

impl ArtifactInput {
    #[must_use]
    pub fn new(artifact_id: impl Into<String>) -> Self {
        Self {
            artifact_id: artifact_id.into(),
        }
    }

    #[must_use]
    pub fn artifact_id(&self) -> &str {
        &self.artifact_id
    }
}

/// Explicitly chooses an initial commit or a prior Artifact as Attempt input.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AttemptInput {
    Base(BaseInput),
    Artifact(ArtifactInput),
}

impl BaseInput {
    #[must_use]
    pub fn new(repository: impl Into<PathBuf>, commit: impl Into<String>) -> Self {
        Self {
            repository: repository.into(),
            commit: commit.into(),
        }
    }

    #[must_use]
    pub fn repository(&self) -> &PathBuf {
        &self.repository
    }

    #[must_use]
    pub fn commit(&self) -> &str {
        &self.commit
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AttemptRunRequest {
    request_id: String,
    task_id: TaskId,
    expected_revision: u64,
    provider_id: ProviderRef,
    model_id: ModelChoice,
    instruction: String,
    role: TaskRole,
    input: AttemptInput,
    timeout: Option<Duration>,
}

impl AttemptRunRequest {
    #[allow(clippy::too_many_arguments)]
    #[must_use]
    pub fn new(
        request_id: impl Into<String>,
        task_id: TaskId,
        expected_revision: u64,
        provider_id: ProviderRef,
        model_id: ModelChoice,
        instruction: impl Into<String>,
        role: TaskRole,
        input: BaseInput,
    ) -> Self {
        Self {
            request_id: request_id.into(),
            task_id,
            expected_revision,
            provider_id,
            model_id,
            instruction: instruction.into(),
            role,
            input: AttemptInput::Base(input),
            timeout: None,
        }
    }

    #[allow(clippy::too_many_arguments)]
    #[must_use]
    pub fn with_artifact(
        request_id: impl Into<String>,
        task_id: TaskId,
        expected_revision: u64,
        provider_id: ProviderRef,
        model_id: ModelChoice,
        instruction: impl Into<String>,
        role: TaskRole,
        input: ArtifactInput,
    ) -> Self {
        Self {
            request_id: request_id.into(),
            task_id,
            expected_revision,
            provider_id,
            model_id,
            instruction: instruction.into(),
            role,
            input: AttemptInput::Artifact(input),
            timeout: None,
        }
    }

    #[must_use]
    pub fn input(&self) -> &AttemptInput {
        &self.input
    }

    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ServiceOperationStatus {
    Accepted,
    Running,
    Completed,
    Failed,
    Cancelled,
    RecoveryRequired,
}

impl ServiceOperationStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Accepted => "accepted",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::RecoveryRequired => "recovery_required",
        }
    }

    fn from_str(value: &str) -> Result<Self, ServiceError> {
        match value {
            "accepted" => Ok(Self::Accepted),
            "running" => Ok(Self::Running),
            "completed" => Ok(Self::Completed),
            "failed" => Ok(Self::Failed),
            "cancelled" => Ok(Self::Cancelled),
            "recovery_required" => Ok(Self::RecoveryRequired),
            _ => Err(ServiceError::InvalidStoredState),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OperationAcceptance {
    operation_id: OperationId,
    attempt_id: AttemptId,
    revision: u64,
    status: ServiceOperationStatus,
}

impl OperationAcceptance {
    #[must_use]
    pub fn operation_id(&self) -> &OperationId {
        &self.operation_id
    }
    #[must_use]
    pub fn attempt_id(&self) -> &AttemptId {
        &self.attempt_id
    }
    #[must_use]
    pub const fn revision(&self) -> u64 {
        self.revision
    }
    #[must_use]
    pub const fn status(&self) -> ServiceOperationStatus {
        self.status
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OperationSnapshot {
    operation_id: OperationId,
    task_id: TaskId,
    attempt_id: AttemptId,
    input_artifact_id: Option<String>,
    output_artifact_id: Option<String>,
    status: ServiceOperationStatus,
    attempt_state: AttemptState,
    requested_provider: ProviderRef,
    requested_model: ModelChoice,
    observed_provider: Option<ProviderRef>,
    observed_model: Option<ModelRef>,
    usage: Vec<UsageMetric>,
    workspace_path: Option<PathBuf>,
    workspace_branch: Option<String>,
    diagnostic_code: Option<String>,
    accepted_at_ms: i64,
    started_at_ms: Option<i64>,
    finished_at_ms: Option<i64>,
}

impl OperationSnapshot {
    #[must_use]
    pub fn operation_id(&self) -> &OperationId {
        &self.operation_id
    }
    #[must_use]
    pub fn task_id(&self) -> &TaskId {
        &self.task_id
    }
    #[must_use]
    pub fn attempt_id(&self) -> &AttemptId {
        &self.attempt_id
    }
    #[must_use]
    pub fn input_artifact_id(&self) -> Option<&str> {
        self.input_artifact_id.as_deref()
    }
    #[must_use]
    pub fn output_artifact_id(&self) -> Option<&str> {
        self.output_artifact_id.as_deref()
    }
    #[must_use]
    pub const fn status(&self) -> ServiceOperationStatus {
        self.status
    }
    #[must_use]
    pub const fn attempt_state(&self) -> AttemptState {
        self.attempt_state
    }
    #[must_use]
    pub fn requested_provider(&self) -> &ProviderRef {
        &self.requested_provider
    }
    #[must_use]
    pub fn requested_model(&self) -> &ModelChoice {
        &self.requested_model
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
    pub fn usage(&self) -> &[UsageMetric] {
        &self.usage
    }
    #[must_use]
    pub fn workspace_path(&self) -> Option<&std::path::Path> {
        self.workspace_path.as_deref()
    }
    #[must_use]
    pub fn workspace_branch(&self) -> Option<&str> {
        self.workspace_branch.as_deref()
    }
    #[must_use]
    pub fn diagnostic_code(&self) -> Option<&str> {
        self.diagnostic_code.as_deref()
    }
    #[must_use]
    pub const fn accepted_at_ms(&self) -> i64 {
        self.accepted_at_ms
    }
    #[must_use]
    pub const fn started_at_ms(&self) -> Option<i64> {
        self.started_at_ms
    }
    #[must_use]
    pub const fn finished_at_ms(&self) -> Option<i64> {
        self.finished_at_ms
    }
}

#[derive(Debug)]
pub enum ServiceError {
    InvalidRequest(&'static str),
    NamedModelRequiresCatalog,
    UnknownProvider,
    ProviderUnavailable,
    TaskNotFound,
    StaleRevision { expected: u64, actual: u64 },
    Busy(OperationId),
    PolicyDenied(&'static str),
    IdempotencyConflict,
    OperationNotFound,
    InvalidStoredState,
    InvalidStateTransition(DomainError),
    Workspace(WorkspaceError),
    Artifact(ArtifactError),
    Ledger(LedgerError),
    OperationLedger(crate::operation_ledger::LedgerError),
    RecoveryLockRequired,
    RecoveryLockMismatch,
    Sqlite(rusqlite::Error),
}

impl std::fmt::Display for ServiceError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidRequest(reason) => {
                write!(formatter, "invalid operation request: {reason}")
            }
            Self::NamedModelRequiresCatalog => formatter
                .write_str("named models are disabled until a trusted model catalog is configured"),
            Self::UnknownProvider => formatter.write_str("requested Provider is not registered"),
            Self::ProviderUnavailable => formatter.write_str("requested Provider is unavailable"),
            Self::TaskNotFound => formatter.write_str("requested Task was not found"),
            Self::StaleRevision { expected, actual } => write!(
                formatter,
                "stale Task revision: expected {expected}, actual {actual}"
            ),
            Self::Busy(id) => write!(
                formatter,
                "Task already has active operation {}",
                id.as_str()
            ),
            Self::PolicyDenied(reason) => write!(formatter, "operation denied by policy: {reason}"),
            Self::IdempotencyConflict => {
                formatter.write_str("request ID was already used with a different payload")
            }
            Self::OperationNotFound => formatter.write_str("operation was not found"),
            Self::InvalidStoredState => {
                formatter.write_str("invalid stored Operation Service state")
            }
            Self::InvalidStateTransition(error) => error.fmt(formatter),
            Self::Workspace(_) => formatter.write_str("workspace preparation failed"),
            Self::Artifact(_) => formatter.write_str("artifact operation failed"),
            Self::Ledger(error) => error.fmt(formatter),
            Self::OperationLedger(error) => error.fmt(formatter),
            Self::RecoveryLockRequired => formatter
                .write_str("persistent Operation Service requires a process-wide ledger lock"),
            Self::RecoveryLockMismatch => {
                formatter.write_str("process-wide ledger lock belongs to a different ledger")
            }
            Self::Sqlite(error) => write!(formatter, "operation service database error: {error}"),
        }
    }
}

impl std::error::Error for ServiceError {}

impl From<LedgerError> for ServiceError {
    fn from(error: LedgerError) -> Self {
        Self::Ledger(error)
    }
}
impl From<crate::operation_ledger::LedgerError> for ServiceError {
    fn from(error: crate::operation_ledger::LedgerError) -> Self {
        Self::OperationLedger(error)
    }
}
impl From<rusqlite::Error> for ServiceError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Sqlite(error)
    }
}
impl From<WorkspaceError> for ServiceError {
    fn from(error: WorkspaceError) -> Self {
        Self::Workspace(error)
    }
}
impl From<ArtifactError> for ServiceError {
    fn from(error: ArtifactError) -> Self {
        Self::Artifact(error)
    }
}
impl From<DomainError> for ServiceError {
    fn from(error: DomainError) -> Self {
        Self::InvalidStateTransition(error)
    }
}

pub struct OperationService<'a, P> {
    ledger: &'a SqliteExecutionLedger,
    workspaces: &'a WorkspaceManager,
    providers: &'a P,
    max_attempts: usize,
    default_timeout: Duration,
    run_lock: Option<crate::operation_ledger::LedgerRunLock>,
}

impl<'a, P: ProviderResolver> OperationService<'a, P> {
    /// Creates a service and, for a persistent ledger, acquires an exclusive process lock.
    /// The service holds the lock until it is dropped, so recovery cannot race a live run.
    pub fn new(
        ledger: &'a SqliteExecutionLedger,
        workspaces: &'a WorkspaceManager,
        providers: &'a P,
        max_attempts: usize,
        default_timeout: Duration,
    ) -> Result<Self, ServiceError> {
        let run_lock = match ledger.ledger_path() {
            Some(path) => Some(crate::operation_ledger::LedgerRunLock::acquire(path)?),
            None => None,
        };
        Self::new_with_run_lock(
            ledger,
            workspaces,
            providers,
            max_attempts,
            default_timeout,
            run_lock,
        )
    }

    /// Builds a service with a process lock already held by the caller.
    /// Persistent ledgers require a matching lock; incomplete operations are recovered
    /// before the service is returned.
    pub fn new_with_run_lock(
        ledger: &'a SqliteExecutionLedger,
        workspaces: &'a WorkspaceManager,
        providers: &'a P,
        max_attempts: usize,
        default_timeout: Duration,
        run_lock: Option<crate::operation_ledger::LedgerRunLock>,
    ) -> Result<Self, ServiceError> {
        if max_attempts == 0 {
            return Err(ServiceError::PolicyDenied("max_attempts must be positive"));
        }
        if default_timeout.is_zero() {
            return Err(ServiceError::PolicyDenied(
                "default timeout must be positive",
            ));
        }
        match (ledger.ledger_path(), run_lock.as_ref()) {
            (Some(_), None) => return Err(ServiceError::RecoveryLockRequired),
            (Some(_), Some(lock)) if !ledger.matches_run_lock(lock) => {
                return Err(ServiceError::RecoveryLockMismatch);
            }
            (None, Some(_)) => return Err(ServiceError::RecoveryLockMismatch),
            _ => {}
        }
        let service = Self {
            ledger,
            workspaces,
            providers,
            max_attempts,
            default_timeout,
            run_lock,
        };
        service.recover_incomplete_operations()?;
        if ledger.ledger_path().is_some() {
            ArtifactManager::new(workspaces, ledger).recover_pending()?;
        }
        Ok(service)
    }

    fn idempotent_acceptance(
        &self,
        request: &AttemptRunRequest,
    ) -> Result<Option<OperationAcceptance>, ServiceError> {
        let connection = self.ledger.lock_connection()?;
        Self::idempotent_acceptance_with_connection(
            &connection,
            request,
            self.workspaces.repository_root(),
        )
    }

    fn idempotent_acceptance_with_connection(
        connection: &rusqlite::Connection,
        request: &AttemptRunRequest,
        repository_root: &std::path::Path,
    ) -> Result<Option<OperationAcceptance>, ServiceError> {
        let existing = connection
            .query_row(
                "SELECT operation.id,operation.attempt_id,operation.expected_revision,operation.task_id,operation.provider,operation.model_kind,operation.model_name,
                    operation.instruction,operation.role,operation.repository,operation.base_commit,operation.timeout_override_ms,relation.input_artifact_id
             FROM service_operations operation LEFT JOIN service_attempt_artifacts relation
               ON relation.task_id=operation.task_id AND relation.attempt_id=operation.attempt_id
             WHERE operation.request_id=?1",
                params![request.request_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, String>(5)?,
                        row.get::<_, Option<String>>(6)?,
                        row.get::<_, String>(7)?,
                        row.get::<_, String>(8)?,
                        row.get::<_, String>(9)?,
                        row.get::<_, String>(10)?,
                        row.get::<_, Option<i64>>(11)?,
                        row.get::<_, Option<String>>(12)?,
                    ))
                },
            )
            .optional()?;
        let Some((
            id,
            attempt,
            revision,
            task,
            provider,
            kind,
            name,
            instruction,
            role,
            repository,
            commit,
            timeout_override,
            input_artifact_id,
        )) = existing
        else {
            return Ok(None);
        };
        let request_override = request
            .timeout
            .map(|value| i64::try_from(value.as_millis()).unwrap_or(i64::MAX));
        let input_matches = match &request.input {
            AttemptInput::Base(input) => {
                let requested_repository = std::fs::canonicalize(input.repository()).ok();
                input_artifact_id.is_none()
                    && requested_repository
                        .as_ref()
                        .is_some_and(|path| path.to_string_lossy() == repository)
                    && commit == input.commit()
            }
            AttemptInput::Artifact(input) => {
                input_artifact_id.as_deref() == Some(input.artifact_id())
                    && repository == repository_root.to_string_lossy()
            }
        };
        if task != request.task_id.as_str()
            || revision as u64 != request.expected_revision
            || provider != request.provider_id.as_str()
            || kind != model_choice_kind(&request.model_id)
            || name.as_deref() != model_choice_name(&request.model_id)
            || instruction != request.instruction
            || role != request.role.as_str()
            || !input_matches
            || timeout_override != request_override
        {
            return Err(ServiceError::IdempotencyConflict);
        }
        Ok(Some(OperationAcceptance {
            operation_id: OperationId::new(id),
            attempt_id: AttemptId::new(attempt),
            revision: revision as u64 + 1,
            status: ServiceOperationStatus::Accepted,
        }))
    }

    /// Atomically accepts the operation, activates the Task, and creates its queued Attempt.
    pub fn submit_attempt(
        &self,
        request: &AttemptRunRequest,
    ) -> Result<OperationAcceptance, ServiceError> {
        if request.request_id.trim().is_empty() {
            return Err(ServiceError::InvalidRequest("request_id must not be empty"));
        }
        if matches!(request.input, AttemptInput::Artifact(_)) {
            return Err(ServiceError::PolicyDenied(
                "ArtifactInput requires a successful changes_requested ReviewVerdict",
            ));
        }
        if let Some(accepted) = self.idempotent_acceptance(request)? {
            return Ok(accepted);
        }
        if request.instruction.trim().is_empty() {
            return Err(ServiceError::InvalidRequest(
                "instruction must not be empty",
            ));
        }
        if matches!(request.model_id, ModelChoice::Named(_)) {
            return Err(ServiceError::NamedModelRequiresCatalog);
        }
        let timeout = request.timeout.unwrap_or(self.default_timeout);
        if timeout.is_zero() {
            return Err(ServiceError::InvalidRequest("timeout must be positive"));
        }
        let timeout_ms = u64::try_from(timeout.as_millis())
            .map_err(|_| ServiceError::InvalidRequest("timeout is too large"))?;
        if timeout_ms == 0 || timeout_ms > i64::MAX as u64 {
            return Err(ServiceError::InvalidRequest(
                "timeout is outside supported range",
            ));
        }

        let (repository, base_commit, input_artifact_id) = match &request.input {
            AttemptInput::Base(input) => {
                let requested_repo = std::fs::canonicalize(input.repository())
                    .map_err(|_| ServiceError::InvalidRequest("repository is unavailable"))?;
                if requested_repo != self.workspaces.repository_root() {
                    return Err(ServiceError::PolicyDenied(
                        "BaseInput repository does not match the configured workspace",
                    ));
                }
                self.workspaces.verify_base_commit(input.commit())?;
                (requested_repo, input.commit().to_owned(), None)
            }
            AttemptInput::Artifact(input) => {
                let artifact = ArtifactManager::new(self.workspaces, self.ledger)
                    .verify_input(&request.task_id, input.artifact_id())?;
                (
                    self.workspaces.repository_root().to_owned(),
                    artifact.base_commit().to_owned(),
                    Some(input.artifact_id().to_owned()),
                )
            }
        };
        let provider = self
            .providers
            .resolve(&request.provider_id)
            .map_err(|_| ServiceError::UnknownProvider)?;
        if provider.provider_ref() != &request.provider_id {
            return Err(ServiceError::UnknownProvider);
        }

        let mut connection = self.ledger.lock_connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(accepted) = Self::idempotent_acceptance_with_connection(
            &transaction,
            request,
            self.workspaces.repository_root(),
        )? {
            return Ok(accepted);
        }

        let mut task = self
            .ledger
            .get_task_with_connection(&transaction, &request.task_id)?
            .ok_or(ServiceError::TaskNotFound)?;
        let actual_revision: i64 = transaction
            .query_row(
                "SELECT revision FROM service_task_revisions WHERE task_id=?1",
                params![request.task_id.as_str()],
                |row| row.get(0),
            )
            .optional()?
            .ok_or(ServiceError::TaskNotFound)?;
        let actual_revision =
            u64::try_from(actual_revision).map_err(|_| ServiceError::InvalidStoredState)?;
        if actual_revision != request.expected_revision {
            return Err(ServiceError::StaleRevision {
                expected: request.expected_revision,
                actual: actual_revision,
            });
        }
        if matches!(
            task.state(),
            TaskState::Completed | TaskState::Failed | TaskState::Cancelled
        ) {
            return Err(ServiceError::PolicyDenied("Task is closed"));
        }
        if task.attempts().len() >= self.max_attempts {
            return Err(ServiceError::PolicyDenied("maximum attempts reached"));
        }
        if request.role.as_str() != "implementer" {
            return Err(ServiceError::PolicyDenied(
                "only implementer Attempts are currently representable",
            ));
        }
        let busy: Option<String> = transaction.query_row(
            "SELECT id FROM service_operations WHERE task_id=?1 AND status IN ('accepted','running','recovery_required') ORDER BY rowid LIMIT 1",
            params![request.task_id.as_str()], |row| row.get(0),
        ).optional()?;
        if let Some(id) = busy {
            return Err(ServiceError::Busy(OperationId::new(id)));
        }
        if !task.attempts().is_empty() {
            return Err(ServiceError::PolicyDenied(
                "follow-up Attempt relation requires explicit supporting evidence",
            ));
        }

        if task.state() == TaskState::Pending {
            task.start()?;
        }
        let row_id: i64 = transaction.query_row(
            "SELECT COALESCE(MAX(rowid),0)+1 FROM service_operations",
            [],
            |row| row.get(0),
        )?;
        let accepted_at = now_ms();
        let operation_id = OperationId::new(format!("service-op-{row_id}-{accepted_at}"));
        let attempt_id = AttemptId::new(format!("service-attempt-{row_id}-{accepted_at}"));
        let attempt = Attempt::new_provider_call_v2(
            attempt_id.clone(),
            request.provider_id.clone(),
            request.model_id.clone(),
        );
        task.add_attempt(attempt.clone())?;
        transaction.execute(
            "UPDATE tasks SET state=?2 WHERE id=?1",
            params![request.task_id.as_str(), task_state_to_str(task.state())],
        )?;
        insert_queued_attempt(
            &transaction,
            &request.task_id,
            &attempt,
            1,
            &request.role,
            "initial",
        )?;
        let new_revision = actual_revision
            .checked_add(1)
            .ok_or(ServiceError::InvalidStoredState)?;
        transaction.execute(
            "UPDATE service_task_revisions SET revision=?2 WHERE task_id=?1",
            params![request.task_id.as_str(), new_revision as i64],
        )?;
        let timeout_override_ms = request
            .timeout
            .map(|value| i64::try_from(value.as_millis()).unwrap_or(i64::MAX));
        transaction.execute(
                "INSERT INTO service_operations(id,request_id,task_id,attempt_id,expected_revision,provider,model_kind,model_name,instruction,role,repository,base_commit,timeout_override_ms,timeout_ms,status,accepted_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,'accepted',?15)",
            params![operation_id.as_str(), request.request_id, request.task_id.as_str(), attempt_id.as_str(), actual_revision as i64,
                request.provider_id.as_str(), model_choice_kind(&request.model_id), model_choice_name(&request.model_id), request.instruction,
                request.role.as_str(), repository.to_string_lossy().as_ref(), base_commit, timeout_override_ms, timeout_ms as i64, accepted_at],
        )?;
        transaction.execute("INSERT INTO service_attempt_artifacts(task_id,attempt_id,input_artifact_id) VALUES(?1,?2,?3)", params![request.task_id.as_str(), attempt_id.as_str(), input_artifact_id])?;
        transaction.commit()?;
        Ok(OperationAcceptance {
            operation_id,
            attempt_id,
            revision: new_revision,
            status: ServiceOperationStatus::Accepted,
        })
    }

    /// Starts a previously accepted operation. Repeated calls never rerun a non-accepted request.
    pub fn run(
        &self,
        operation_id: &OperationId,
        cancellation: CancellationToken,
    ) -> Result<OperationSnapshot, ServiceError> {
        let stored = self.load_request(operation_id)?;
        if stored.status != ServiceOperationStatus::Accepted {
            return self.get_operation(operation_id);
        }
        if !self.claim_operation(operation_id)? {
            return self.get_operation(operation_id);
        }
        if stored.input_artifact_id.is_some() {
            self.finish_without_start(
                operation_id,
                "review_evidence_required",
                ServiceOperationStatus::Failed,
            )?;
            return self.get_operation(operation_id);
        }
        let provider = match self.providers.resolve(&stored.provider) {
            Ok(provider) if provider.provider_ref() == &stored.provider => provider,
            _ => {
                self.finish_without_start(
                    operation_id,
                    "unknown_provider",
                    ServiceOperationStatus::Failed,
                )?;
                return self.get_operation(operation_id);
            }
        };
        if provider.check_availability().is_err() {
            self.finish_without_start(
                operation_id,
                "provider_unavailable",
                ServiceOperationStatus::Failed,
            )?;
            return self.get_operation(operation_id);
        }

        // Persist the deterministic locator before creating the worktree. A crash
        // after `git worktree add` must not leave a retained workspace untracked.
        self.record_workspace_locator(operation_id, &stored.task_id, &stored.attempt_id)?;
        let workspace = match self.workspaces.create_at_base(
            &stored.task_id,
            &stored.attempt_id,
            &stored.base_commit,
        ) {
            Ok(workspace) => workspace,
            Err(_) => {
                self.finish_without_start(
                    operation_id,
                    "workspace_unavailable",
                    ServiceOperationStatus::Failed,
                )?;
                return self.get_operation(operation_id);
            }
        };
        let prepared = if let Some(input_id) = stored.input_artifact_id.as_deref() {
            ArtifactManager::new(self.workspaces, self.ledger)
                .materialize(&workspace, &stored.task_id, &stored.attempt_id, input_id)
                .is_ok()
        } else {
            self.workspaces
                .validate_provider_workspace_at_base(workspace.path(), &stored.base_commit)
                .is_ok()
        };
        if !prepared {
            self.finish_without_start(
                operation_id,
                if stored.input_artifact_id.is_some() {
                    "artifact_unavailable"
                } else {
                    "workspace_unavailable"
                },
                ServiceOperationStatus::Failed,
            )?;
            return self.get_operation(operation_id);
        }
        self.mark_attempt_started(operation_id)?;
        let timeout = Duration::from_millis(stored.timeout_ms);
        let provider_request = ProviderRequest::new(
            workspace.path(),
            stored.instruction,
            timeout,
            ModelChoice::ProviderDefault,
        );
        let provider_result = provider.execute_with_cancellation(&provider_request, cancellation);
        match provider_result {
            Ok(result) => {
                let usage = result.usage().cloned();
                match ArtifactManager::new(self.workspaces, self.ledger).capture(
                    &workspace,
                    &stored.task_id,
                    &stored.attempt_id,
                    stored.input_artifact_id.as_deref(),
                    &stored.base_commit,
                ) {
                    Ok(_) => {}
                    Err(_) => {
                        self.finish_recovery_required(operation_id, "artifact_capture_failed")?;
                        return self.get_operation(operation_id);
                    }
                }
                self.finish_attempt(
                    operation_id,
                    ServiceOperationStatus::Completed,
                    None,
                    result.observed_provider().cloned(),
                    result.observed_model().cloned(),
                    usage,
                    None,
                )?;
            }
            Err(error) => {
                if matches!(
                    error.kind(),
                    ProviderError::Interrupted {
                        confirmed_stopped: false,
                        ..
                    }
                ) {
                    self.finish_recovery_required(operation_id, "provider_interrupted")?;
                    return self.get_operation(operation_id);
                }
                if ArtifactManager::new(self.workspaces, self.ledger)
                    .capture(
                        &workspace,
                        &stored.task_id,
                        &stored.attempt_id,
                        stored.input_artifact_id.as_deref(),
                        &stored.base_commit,
                    )
                    .is_err()
                {
                    self.finish_recovery_required(operation_id, "artifact_capture_failed")?;
                    return self.get_operation(operation_id);
                }
                let (status, reason, code) = match error.kind() {
                    ProviderError::TimedOut { .. } | ProviderError::TimedOutWithOutput { .. } => (
                        ServiceOperationStatus::Failed,
                        AttemptFailureReason::Timeout,
                        "timeout",
                    ),
                    ProviderError::Cancelled | ProviderError::CancelledWithOutput => (
                        ServiceOperationStatus::Cancelled,
                        AttemptFailureReason::Cancelled,
                        "cancelled",
                    ),
                    ProviderError::Interrupted { .. } => (
                        ServiceOperationStatus::Failed,
                        AttemptFailureReason::Provider,
                        "provider_interrupted",
                    ),
                    ProviderError::UnsupportedModel { .. } => (
                        ServiceOperationStatus::Failed,
                        AttemptFailureReason::Provider,
                        "unknown_model",
                    ),
                    _ => (
                        ServiceOperationStatus::Failed,
                        AttemptFailureReason::Provider,
                        "provider_failed",
                    ),
                };
                self.finish_attempt(
                    operation_id,
                    status,
                    Some(reason),
                    None,
                    None,
                    None,
                    Some(code),
                )?;
            }
        }
        self.get_operation(operation_id)
    }

    pub fn get_operation(
        &self,
        operation_id: &OperationId,
    ) -> Result<OperationSnapshot, ServiceError> {
        let connection = self.ledger.lock_connection()?;
        let record = connection.query_row(
            "SELECT task_id,attempt_id,status,provider,model_kind,model_name,observed_provider,observed_model,workspace_path,workspace_branch,diagnostic_code,accepted_at,started_at,finished_at
             FROM service_operations WHERE id=?1",
            params![operation_id.as_str()], |row| Ok((row.get::<_, String>(0)?,row.get::<_, String>(1)?,row.get::<_, String>(2)?,row.get::<_, String>(3)?,row.get::<_, String>(4)?,row.get::<_, Option<String>>(5)?,row.get::<_, Option<String>>(6)?,row.get::<_, Option<String>>(7)?,row.get::<_, Option<String>>(8)?,row.get::<_, Option<String>>(9)?,row.get::<_, Option<String>>(10)?,row.get::<_, i64>(11)?,row.get::<_, Option<i64>>(12)?,row.get::<_, Option<i64>>(13)?)),
        ).optional()?.ok_or(ServiceError::OperationNotFound)?;
        let (
            task,
            attempt,
            status,
            provider,
            kind,
            name,
            observed_provider,
            observed_model,
            workspace_path,
            workspace_branch,
            diagnostic,
            accepted,
            started,
            finished,
        ) = record;
        let requested_model =
            crate::execution_ledger::requested_model_from_storage(Some(&kind), name.as_deref())?
                .ok_or(ServiceError::InvalidStoredState)?;
        let attempt_status: String = connection.query_row(
            "SELECT state FROM attempts WHERE task_id=?1 AND id=?2",
            params![task, attempt],
            |row| row.get(0),
        )?;
        let mut statement=connection.prepare("SELECT name,value,unit FROM service_operation_usage WHERE operation_id=?1 ORDER BY sequence")?;
        let usage = statement
            .query_map(params![operation_id.as_str()], |row| {
                Ok(UsageMetric::new(
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let (input_artifact_id, output_artifact_id): (Option<String>, Option<String>) = connection.query_row(
            "SELECT relation.input_artifact_id,relation.output_artifact_id FROM service_attempt_artifacts relation WHERE relation.task_id=?1 AND relation.attempt_id=?2",
            params![task, attempt], |row| Ok((row.get(0)?, row.get(1)?)),
        ).optional()?.unwrap_or((None, None));
        Ok(OperationSnapshot {
            operation_id: operation_id.clone(),
            task_id: TaskId::new(task),
            attempt_id: AttemptId::new(attempt),
            input_artifact_id,
            output_artifact_id,
            status: ServiceOperationStatus::from_str(&status)?,
            attempt_state: parse_attempt_state(&attempt_status)?,
            requested_provider: ProviderRef::new(provider),
            requested_model,
            observed_provider: observed_provider.map(ProviderRef::new),
            observed_model: observed_model.map(ModelRef::new),
            usage,
            workspace_path: workspace_path.map(PathBuf::from),
            workspace_branch,
            diagnostic_code: diagnostic,
            accepted_at_ms: accepted,
            started_at_ms: started,
            finished_at_ms: finished,
        })
    }

    fn record_workspace_locator(
        &self,
        operation_id: &OperationId,
        task_id: &TaskId,
        attempt_id: &AttemptId,
    ) -> Result<(), ServiceError> {
        let mut connection = self.ledger.lock_connection()?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (stored_task, stored_attempt): (String, String) = tx.query_row(
            "SELECT task_id,attempt_id FROM service_operations WHERE id=?1 AND attempt_id=?2 AND status='running'",
            params![operation_id.as_str(), attempt_id.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        if stored_task != task_id.as_str() || stored_attempt != attempt_id.as_str() {
            return Err(ServiceError::InvalidStoredState);
        }
        tx.execute(
            "UPDATE service_operations SET workspace_path=?2,workspace_branch=?3 WHERE id=?1",
            params![
                operation_id.as_str(),
                self.workspaces
                    .worktree_path(task_id, attempt_id)
                    .to_string_lossy()
                    .as_ref(),
                self.workspaces.branch_name(task_id, attempt_id)
            ],
        )?;
        bump_revision(&tx, task_id)?;
        tx.commit()?;
        Ok(())
    }

    fn finish_recovery_required(
        &self,
        operation_id: &OperationId,
        code: &str,
    ) -> Result<(), ServiceError> {
        let mut connection = self.ledger.lock_connection()?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let task_text: String = tx.query_row(
            "SELECT task_id FROM service_operations WHERE id=?1 AND status='running'",
            params![operation_id.as_str()],
            |row| row.get(0),
        )?;
        tx.execute(
            "UPDATE service_operations SET status='recovery_required',finished_at=?2,diagnostic_code=?3 WHERE id=?1",
            params![operation_id.as_str(), now_ms(), code],
        )?;
        bump_revision(&tx, &TaskId::new(task_text))?;
        tx.commit()?;
        Ok(())
    }

    /// Closes operations left running by a prior process while the matching run lock is held.
    /// Called only by service construction, before the service is exposed to callers.
    /// It never replays an operation whose running claim was persisted before a crash.
    fn recover_incomplete_operations(&self) -> Result<Vec<OperationId>, ServiceError> {
        if self.ledger.ledger_path().is_none() {
            return Ok(Vec::new());
        }
        match (self.ledger.ledger_path(), self.run_lock.as_ref()) {
            (Some(_), None) => return Err(ServiceError::RecoveryLockRequired),
            (Some(_), Some(lock)) if !self.ledger.matches_run_lock(lock) => {
                return Err(ServiceError::RecoveryLockMismatch);
            }
            _ => {}
        }
        let mut connection = self.ledger.lock_connection()?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let pending = {
            let mut statement = tx.prepare(
                "SELECT id,task_id,attempt_id FROM service_operations WHERE status='running' ORDER BY rowid",
            )?;
            statement
                .query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                })?
                .collect::<Result<Vec<_>, _>>()?
        };
        let mut recovered = Vec::with_capacity(pending.len());
        for (operation_id, task_text, attempt_text) in pending {
            let task_id = TaskId::new(task_text);
            let attempt_id = AttemptId::new(attempt_text);
            let attempt_state: String = tx.query_row(
                "SELECT state FROM attempts WHERE task_id=?1 AND id=?2",
                params![task_id.as_str(), attempt_id.as_str()],
                |row| row.get(0),
            )?;
            if attempt_state != "queued" && attempt_state != "running" {
                return Err(ServiceError::InvalidStoredState);
            }
            tx.execute(
                "UPDATE service_operations SET status='recovery_required',finished_at=?2,diagnostic_code='interrupted' WHERE id=?1 AND status='running'",
                params![operation_id, now_ms()],
            )?;
            bump_revision(&tx, &task_id)?;
            recovered.push(OperationId::new(operation_id));
        }
        tx.commit()?;
        Ok(recovered)
    }

    fn load_request(&self, operation_id: &OperationId) -> Result<StoredRequest, ServiceError> {
        let connection = self.ledger.lock_connection()?;
        connection.query_row("SELECT operation.task_id,operation.attempt_id,operation.provider,operation.instruction,operation.base_commit,operation.timeout_ms,operation.status,relation.input_artifact_id FROM service_operations operation LEFT JOIN service_attempt_artifacts relation ON relation.task_id=operation.task_id AND relation.attempt_id=operation.attempt_id WHERE operation.id=?1",params![operation_id.as_str()],|row|Ok(StoredRequest{task_id:TaskId::new(row.get::<_,String>(0)?),attempt_id:AttemptId::new(row.get::<_,String>(1)?),provider:ProviderRef::new(row.get::<_,String>(2)?),instruction:row.get(3)?,base_commit:row.get(4)?,timeout_ms:row.get::<_,i64>(5)? as u64,status:ServiceOperationStatus::from_str(&row.get::<_,String>(6)?).map_err(|_|rusqlite::Error::InvalidQuery)?,input_artifact_id:row.get(7)?})).optional()?.ok_or(ServiceError::OperationNotFound)
    }

    /// Claims an accepted operation before availability checks or workspace side effects.
    fn claim_operation(&self, operation_id: &OperationId) -> Result<bool, ServiceError> {
        let mut connection = self.ledger.lock_connection()?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (task_id, status): (String, String) = tx.query_row(
            "SELECT task_id,status FROM service_operations WHERE id=?1",
            params![operation_id.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        if status != "accepted" {
            return Ok(false);
        }
        let started = now_ms();
        let changed=tx.execute("UPDATE service_operations SET status='running',started_at=?2 WHERE id=?1 AND status='accepted'",params![operation_id.as_str(),started])?;
        if changed != 1 {
            return Ok(false);
        }
        bump_revision(&tx, &TaskId::new(task_id))?;
        tx.commit()?;
        Ok(true)
    }

    fn mark_attempt_started(&self, operation_id: &OperationId) -> Result<(), ServiceError> {
        let mut connection = self.ledger.lock_connection()?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (task_id, attempt_id, status): (String, String, String) = tx.query_row(
            "SELECT task_id,attempt_id,status FROM service_operations WHERE id=?1",
            params![operation_id.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        if status != "running" {
            return Err(ServiceError::InvalidStoredState);
        }
        let mut task = self
            .ledger
            .get_task_with_connection(&tx, &TaskId::new(task_id.clone()))?
            .ok_or(ServiceError::TaskNotFound)?;
        let attempt = task
            .attempt_mut(&AttemptId::new(attempt_id.clone()))
            .ok_or(ServiceError::InvalidStoredState)?;
        if attempt.semantics() != AttemptSemantics::ProviderCallV2
            || attempt.state() != AttemptState::Queued
        {
            return Err(ServiceError::InvalidStoredState);
        }
        attempt.start()?;
        let started = now_ms();
        tx.execute(
            "UPDATE attempts SET state='running',started_at=?3 WHERE task_id=?1 AND id=?2",
            params![task_id, attempt_id, started],
        )?;
        bump_revision(&tx, &TaskId::new(task_id))?;
        tx.commit()?;
        Ok(())
    }

    fn finish_without_start(
        &self,
        operation_id: &OperationId,
        code: &str,
        status: ServiceOperationStatus,
    ) -> Result<(), ServiceError> {
        let mut connection = self.ledger.lock_connection()?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (task_text, attempt_text, current): (String, String, String) = tx.query_row(
            "SELECT task_id,attempt_id,status FROM service_operations WHERE id=?1",
            params![operation_id.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        if current != "running" {
            return Err(ServiceError::InvalidStoredState);
        }
        let attempt_state: String = tx.query_row(
            "SELECT state FROM attempts WHERE task_id=?1 AND id=?2",
            params![task_text, attempt_text],
            |row| row.get(0),
        )?;
        if attempt_state != "queued" {
            return Err(ServiceError::InvalidStoredState);
        }
        let changed=tx.execute("UPDATE service_operations SET status=?2,finished_at=?3,diagnostic_code=?4 WHERE id=?1 AND status='running'",params![operation_id.as_str(),status.as_str(),now_ms(),code])?;
        if changed != 1 {
            return Err(ServiceError::InvalidStoredState);
        }
        bump_revision(&tx, &TaskId::new(task_text))?;
        tx.commit()?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn finish_attempt(
        &self,
        operation_id: &OperationId,
        status: ServiceOperationStatus,
        failure: Option<AttemptFailureReason>,
        observed_provider: Option<ProviderRef>,
        observed_model: Option<ModelRef>,
        usage: Option<UsageCost>,
        diagnostic: Option<&str>,
    ) -> Result<(), ServiceError> {
        let mut connection = self.ledger.lock_connection()?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (task_text, attempt_text, current): (String, String, String) = tx.query_row(
            "SELECT task_id,attempt_id,status FROM service_operations WHERE id=?1",
            params![operation_id.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        if current != "running" {
            return Err(ServiceError::InvalidStoredState);
        }
        let task_id = TaskId::new(task_text);
        let attempt_id = AttemptId::new(attempt_text);
        let mut task = self
            .ledger
            .get_task_with_connection(&tx, &task_id)?
            .ok_or(ServiceError::TaskNotFound)?;
        let attempt = task
            .attempt_mut(&attempt_id)
            .ok_or(ServiceError::InvalidStoredState)?;
        if attempt.semantics() != AttemptSemantics::ProviderCallV2 {
            return Err(ServiceError::InvalidStoredState);
        }
        let output_state: Option<String> = tx.query_row(
            "SELECT artifact.state FROM service_attempt_artifacts relation JOIN service_artifacts artifact ON artifact.task_id=relation.task_id AND artifact.id=relation.output_artifact_id WHERE relation.task_id=?1 AND relation.attempt_id=?2",
            params![task_id.as_str(), attempt_id.as_str()],
            |row| row.get(0),
        ).optional()?;
        if output_state.as_deref() != Some("available") {
            return Err(ServiceError::InvalidStoredState);
        }
        if let Some(provider) = observed_provider.clone() {
            attempt.record_observed_target(Some(provider), observed_model.clone());
        }
        if let Some(cost) = usage.clone() {
            attempt.set_usage_cost(cost.clone());
        }
        match status {
            ServiceOperationStatus::Completed => attempt.complete_provider_call()?,
            ServiceOperationStatus::Cancelled => {
                attempt.cancel_with_reason(failure.unwrap_or(AttemptFailureReason::Cancelled))?
            }
            _ => attempt.fail_with_reason(failure.unwrap_or(AttemptFailureReason::Provider))?,
        }
        let finished = now_ms();
        tx.execute("UPDATE attempts SET state=?3,finished_at=?4,failure_reason=?5,observed_provider=?6,observed_model=?7 WHERE task_id=?1 AND id=?2",params![task_id.as_str(),attempt_id.as_str(),attempt_state_to_str(attempt.state()),finished,attempt.failure_reason().map(failure_reason_to_str),attempt.observed_provider().map(ProviderRef::as_str),attempt.observed_model().map(ModelRef::as_str)])?;
        tx.execute(
            "DELETE FROM usage_metrics WHERE task_id=?1 AND attempt_id=?2",
            params![task_id.as_str(), attempt_id.as_str()],
        )?;
        if let Some(cost) = usage {
            for (i, m) in cost.metrics().iter().enumerate() {
                tx.execute("INSERT INTO usage_metrics(task_id,attempt_id,sequence,name,value,unit) VALUES(?1,?2,?3,?4,?5,?6)",params![task_id.as_str(),attempt_id.as_str(),i as i64,m.name(),m.value(),m.unit()])?;
                tx.execute("INSERT INTO service_operation_usage(operation_id,sequence,name,value,unit) VALUES(?1,?2,?3,?4,?5)",params![operation_id.as_str(),i as i64,m.name(),m.value(),m.unit()])?;
            }
        }
        tx.execute("UPDATE service_operations SET status=?2,finished_at=?3,observed_provider=?4,observed_model=?5,diagnostic_code=?6 WHERE id=?1",params![operation_id.as_str(),status.as_str(),finished,attempt.observed_provider().map(ProviderRef::as_str),attempt.observed_model().map(ModelRef::as_str),diagnostic])?;
        bump_revision(&tx, &task_id)?;
        tx.commit()?;
        Ok(())
    }
}

struct StoredRequest {
    task_id: TaskId,
    attempt_id: AttemptId,
    provider: ProviderRef,
    instruction: String,
    base_commit: String,
    timeout_ms: u64,
    status: ServiceOperationStatus,
    input_artifact_id: Option<String>,
}

fn insert_queued_attempt(
    tx: &rusqlite::Transaction<'_>,
    task_id: &TaskId,
    attempt: &Attempt,
    sequence: usize,
    role: &TaskRole,
    relation_kind: &str,
) -> Result<(), ServiceError> {
    tx.execute("INSERT INTO attempts(task_id,id,provider,state,started_at,finished_at,failure_reason,requested_model_kind,requested_model,observed_provider,observed_model,semantics_version) VALUES(?1,?2,?3,'queued',NULL,NULL,NULL,?4,?5,NULL,NULL,'provider_call_v2')",params![task_id.as_str(),attempt.id().as_str(),attempt.provider().as_str(),attempt.requested_model().map(model_choice_kind),attempt.requested_model().and_then(model_choice_name)])?;
    tx.execute(
        "INSERT INTO service_attempt_history(task_id,attempt_id,sequence,role,relation_kind,related_attempt_id)
         VALUES(?1,?2,?3,?4,?5,NULL)",
        params![task_id.as_str(), attempt.id().as_str(), sequence as i64, role.as_str(), relation_kind],
    )?;
    Ok(())
}

fn bump_revision(tx: &rusqlite::Transaction<'_>, task_id: &TaskId) -> Result<(), ServiceError> {
    let changed = tx.execute(
        "UPDATE service_task_revisions SET revision=revision+1 WHERE task_id=?1",
        params![task_id.as_str()],
    )?;
    if changed != 1 {
        return Err(ServiceError::TaskNotFound);
    }
    Ok(())
}

fn parse_attempt_state(value: &str) -> Result<AttemptState, ServiceError> {
    match value {
        "queued" => Ok(AttemptState::Queued),
        "running" => Ok(AttemptState::Running),
        "validating" => Ok(AttemptState::Validating),
        "succeeded" => Ok(AttemptState::Succeeded),
        "failed" => Ok(AttemptState::Failed),
        "cancelled" => Ok(AttemptState::Cancelled),
        _ => Err(ServiceError::InvalidStoredState),
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| {
            i64::try_from(duration.as_millis()).unwrap_or(i64::MAX)
        })
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        process::{Child, Command},
        sync::{
            Arc, Barrier, Condvar, Mutex,
            atomic::{AtomicU64, AtomicUsize, Ordering},
        },
        thread,
        time::Instant,
    };

    use super::*;
    use crate::{
        AgentProvider, AgentResult, ProviderError, ProviderRegistry, ProviderResult, Task,
        UsageCost,
    };

    static NEXT_DIR: AtomicU64 = AtomicU64::new(1);
    const CHILD_LEDGER_PATH: &str = "AI_DEV_ORCHESTRATOR_SERVICE_LOCK_LEDGER";
    const CHILD_REPOSITORY_PATH: &str = "AI_DEV_ORCHESTRATOR_SERVICE_LOCK_REPOSITORY";
    const CHILD_OPERATION_ID: &str = "AI_DEV_ORCHESTRATOR_SERVICE_LOCK_OPERATION";
    const CHILD_MARKER_PATH: &str = "AI_DEV_ORCHESTRATOR_SERVICE_LOCK_MARKER";

    fn sqlite_open_is_busy(error: &crate::LedgerError) -> bool {
        matches!(
            error,
            crate::LedgerError::Sqlite(rusqlite::Error::SqliteFailure(code, _))
                if matches!(
                    code.code,
                    rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked
                )
        )
    }

    struct Repo(PathBuf);

    impl Repo {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "operation-service-{}-{}",
                std::process::id(),
                NEXT_DIR.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&path).unwrap();
            git(&path, &["init", "-b", "main"]);
            git(&path, &["config", "user.email", "service@example.invalid"]);
            git(&path, &["config", "user.name", "Service Test"]);
            fs::write(path.join("README.md"), "base\n").unwrap();
            git(&path, &["add", "README.md"]);
            git(&path, &["commit", "-m", "base"]);
            Self(path)
        }
        fn commit(&self) -> String {
            git(&self.0, &["rev-parse", "HEAD"])
        }
    }

    impl Drop for Repo {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn git(path: &std::path::Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .args(args)
            .current_dir(path)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    }

    fn cleanup_fixture_worktree(
        repo: &Repo,
        workspace: &WorkspaceManager,
        task: &TaskId,
        attempt: &AttemptId,
    ) {
        let path = workspace.worktree_path(task, attempt);
        if path.exists() {
            let path_text = path.to_string_lossy().into_owned();
            git(&repo.0, &["worktree", "remove", "--force", &path_text]);
        }
    }

    #[derive(Clone)]
    struct FakeProvider {
        calls: Arc<AtomicUsize>,
        availability_checks: Arc<AtomicUsize>,
        fail: bool,
        unknown_interrupt: bool,
        provider_failure: Option<FakeProviderFailure>,
        execute_delay: Duration,
        reference: ProviderRef,
        write_output: Option<(String, String)>,
        write_ignored: Option<(String, String)>,
        write_gitignore: Option<String>,
        require_file: Option<(String, String)>,
    }

    #[derive(Clone, Copy)]
    enum FakeProviderFailure {
        ExecutionFailed,
        Cancelled,
        CancelledWithOutput,
    }

    impl AgentProvider for FakeProvider {
        fn provider_ref(&self) -> &ProviderRef {
            &self.reference
        }
        fn execute(&self, request: &ProviderRequest) -> Result<ProviderResult, ProviderError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            thread::sleep(self.execute_delay);
            assert_eq!(request.model(), &ModelChoice::ProviderDefault);
            if let Some((path, contents)) = &self.require_file {
                assert_eq!(
                    fs::read_to_string(request.workspace().join(path)).unwrap(),
                    *contents
                );
            }
            if let Some((path, contents)) = &self.write_output {
                fs::write(request.workspace().join(path), contents).unwrap();
            }
            if let Some((path, contents)) = &self.write_ignored {
                fs::write(request.workspace().join(path), contents).unwrap();
            }
            if let Some(contents) = &self.write_gitignore {
                fs::write(request.workspace().join(".gitignore"), contents).unwrap();
            }
            match self.provider_failure {
                Some(FakeProviderFailure::ExecutionFailed) => {
                    return Err(ProviderError::ExecutionFailed(
                        "RAW_EXECUTION_DIAGNOSTIC_SECRET".into(),
                    ));
                }
                Some(FakeProviderFailure::Cancelled) => return Err(ProviderError::Cancelled),
                Some(FakeProviderFailure::CancelledWithOutput) => {
                    return Err(ProviderError::CancelledWithOutput.with_captured_output(
                        crate::CapturedOutput::new(
                            b"RAW_STDOUT_SECRET".to_vec(),
                            b"RAW_STDERR_SECRET".to_vec(),
                            None,
                            false,
                        ),
                    ));
                }
                None => {}
            }
            if self.unknown_interrupt {
                return Err(ProviderError::Interrupted {
                    reason: crate::StopReason::TimedOut,
                    confirmed_stopped: false,
                });
            }
            if self.fail {
                return Err(ProviderError::TimedOutWithOutput {
                    timeout: request.timeout(),
                });
            }
            Ok(ProviderResult::new(
                "RAW_STDOUT_SECRET",
                "RAW_STDERR_SECRET",
                Some(0),
                Some(AgentResult::new("AGENT_RESULT_SECRET", true)),
                Some(UsageCost::new([UsageMetric::new(
                    "input_tokens",
                    "7",
                    "token",
                )])),
            )
            .with_observed_target(Some(self.reference.clone()), None))
        }
        fn execute_with_cancellation(
            &self,
            request: &ProviderRequest,
            cancellation: CancellationToken,
        ) -> Result<ProviderResult, ProviderError> {
            if !matches!(self.provider_failure, Some(FakeProviderFailure::Cancelled)) {
                return self.execute(request);
            }

            self.calls.fetch_add(1, Ordering::SeqCst);
            while !cancellation.is_cancelled() {
                thread::sleep(Duration::from_millis(1));
            }
            Err(ProviderError::Cancelled)
        }
        fn check_availability(&self) -> Result<(), ProviderError> {
            self.availability_checks.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    struct BlockingProvider {
        reference: ProviderRef,
        calls: Arc<AtomicUsize>,
        state: Arc<(Mutex<BlockingProviderState>, Condvar)>,
    }

    #[derive(Default)]
    struct BlockingProviderState {
        started: bool,
        released: bool,
    }

    impl AgentProvider for BlockingProvider {
        fn provider_ref(&self) -> &ProviderRef {
            &self.reference
        }

        fn execute(&self, _request: &ProviderRequest) -> Result<ProviderResult, ProviderError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let (state, changed) = &*self.state;
            let mut state = state.lock().unwrap();
            state.started = true;
            changed.notify_all();
            while !state.released {
                state = changed.wait(state).unwrap();
            }
            Ok(
                ProviderResult::new("", "", Some(0), Some(AgentResult::new("done", true)), None)
                    .with_observed_target(Some(self.reference.clone()), None),
            )
        }

        fn check_availability(&self) -> Result<(), ProviderError> {
            Ok(())
        }
    }

    struct BlockingResolver(BlockingProvider);

    impl crate::ProviderResolver for BlockingResolver {
        fn resolve(
            &self,
            provider: &ProviderRef,
        ) -> Result<&dyn AgentProvider, crate::ProviderResolutionError> {
            if provider == &self.0.reference {
                Ok(&self.0)
            } else {
                Err(crate::ProviderResolutionError::UnknownProvider {
                    provider: provider.clone(),
                })
            }
        }
    }

    struct FakeResolver(FakeProvider);

    impl crate::ProviderResolver for FakeResolver {
        fn resolve(
            &self,
            provider: &ProviderRef,
        ) -> Result<&dyn AgentProvider, crate::ProviderResolutionError> {
            if provider == &self.0.reference {
                Ok(&self.0)
            } else {
                Err(crate::ProviderResolutionError::UnknownProvider {
                    provider: provider.clone(),
                })
            }
        }
    }

    fn spawn_test_child(test_name: &str, variables: &[(&str, &str)]) -> Child {
        let executable = std::env::current_exe().unwrap();
        let mut command = Command::new(executable);
        command.arg("--exact").arg(test_name).arg("--nocapture");
        for (name, value) in variables {
            command.env(name, value);
        }
        command.spawn().unwrap()
    }

    fn wait_for_file(path: &std::path::Path, timeout: Duration) -> String {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if let Ok(value) = fs::read_to_string(path) {
                return value;
            }
            thread::sleep(Duration::from_millis(10));
        }
        panic!("timed out waiting for child marker {}", path.display());
    }

    fn request(repo: &Repo, task: &TaskId, revision: u64, id: &str) -> AttemptRunRequest {
        AttemptRunRequest::new(
            id,
            task.clone(),
            revision,
            ProviderRef::new("fake"),
            ModelChoice::ProviderDefault,
            "perform the requested work",
            TaskRole::new("implementer"),
            BaseInput::new(&repo.0, repo.commit()),
        )
    }

    fn service_parts(
        fail: bool,
        execute_delay: Duration,
        unknown_interrupt: bool,
        provider_failure: Option<FakeProviderFailure>,
    ) -> (
        Repo,
        SqliteExecutionLedger,
        WorkspaceManager,
        ProviderRegistry,
        Arc<AtomicUsize>,
        Arc<AtomicUsize>,
        TaskId,
    ) {
        let repo = Repo::new();
        let ledger = SqliteExecutionLedger::open_in_memory().unwrap();
        let task_id = TaskId::new("service-task");
        ledger
            .save_task(&Task::new(
                task_id.clone(),
                "task description",
                TaskRole::new("implementer"),
            ))
            .unwrap();
        let workspace = WorkspaceManager::new(&repo.0).unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let checks = Arc::new(AtomicUsize::new(0));
        let mut providers = ProviderRegistry::new();
        providers.register(FakeProvider {
            calls: calls.clone(),
            availability_checks: checks.clone(),
            fail,
            unknown_interrupt,
            provider_failure,
            execute_delay,
            reference: ProviderRef::new("fake"),
            write_output: None,
            write_ignored: None,
            write_gitignore: None,
            require_file: None,
        });
        (repo, ledger, workspace, providers, calls, checks, task_id)
    }

    #[test]
    fn child_service_crashes_after_claim_and_parent_recovers() {
        let Some(ledger_path) = std::env::var_os(CHILD_LEDGER_PATH) else {
            return;
        };
        let ledger = SqliteExecutionLedger::open(ledger_path).unwrap();
        let repository = PathBuf::from(std::env::var_os(CHILD_REPOSITORY_PATH).unwrap());
        let workspace = WorkspaceManager::new(&repository).unwrap();
        let resolver = FakeResolver(FakeProvider {
            calls: Arc::new(AtomicUsize::new(0)),
            availability_checks: Arc::new(AtomicUsize::new(0)),
            fail: false,
            unknown_interrupt: false,
            provider_failure: None,
            execute_delay: Duration::ZERO,
            reference: ProviderRef::new("fake"),
            write_output: None,
            write_ignored: None,
            write_gitignore: None,
            require_file: None,
        });
        let service =
            OperationService::new(&ledger, &workspace, &resolver, 3, Duration::from_secs(30))
                .unwrap();
        let operation_id = OperationId::new(std::env::var(CHILD_OPERATION_ID).unwrap());
        assert!(service.claim_operation(&operation_id).unwrap());
        fs::write(std::env::var_os(CHILD_MARKER_PATH).unwrap(), "claimed").unwrap();
        thread::sleep(Duration::from_secs(60));
    }

    #[test]
    fn child_reports_lock_acquisition_during_active_service() {
        let Some(ledger_path) = std::env::var_os(CHILD_LEDGER_PATH) else {
            return;
        };
        let result = (|| -> Result<&'static str, String> {
            let ledger = match SqliteExecutionLedger::open(ledger_path) {
                Ok(ledger) => ledger,
                Err(error) if sqlite_open_is_busy(&error) => return Ok("busy"),
                Err(error) => return Err(error.to_string()),
            };
            let repository = PathBuf::from(std::env::var_os(CHILD_REPOSITORY_PATH).unwrap());
            let workspace =
                WorkspaceManager::new(&repository).map_err(|error| error.to_string())?;
            let resolver = FakeResolver(FakeProvider {
                calls: Arc::new(AtomicUsize::new(0)),
                availability_checks: Arc::new(AtomicUsize::new(0)),
                fail: false,
                unknown_interrupt: false,
                provider_failure: None,
                execute_delay: Duration::ZERO,
                reference: ProviderRef::new("fake"),
                write_output: None,
                write_ignored: None,
                write_gitignore: None,
                require_file: None,
            });
            match OperationService::new(&ledger, &workspace, &resolver, 3, Duration::from_secs(30))
            {
                Err(ServiceError::OperationLedger(crate::OperationLedgerError::LockBusy)) => {
                    Ok("busy")
                }
                Err(error) => Err(error.to_string()),
                Ok(_service) => Ok("constructed"),
            }
        })();
        let marker_value = result
            .map(str::to_owned)
            .unwrap_or_else(|error| format!("error:{error}"));
        fs::write(std::env::var_os(CHILD_MARKER_PATH).unwrap(), marker_value).unwrap();
    }

    #[test]
    fn persistent_service_lock_blocks_another_process_during_provider_execution() {
        let repo = Repo::new();
        let ledger_path = repo.0.join("execution.sqlite3");
        let ledger = SqliteExecutionLedger::open(&ledger_path).unwrap();
        let task_id = TaskId::new("locked-service-task");
        ledger
            .save_task(&Task::new(
                task_id.clone(),
                "task",
                TaskRole::new("implementer"),
            ))
            .unwrap();
        let workspace = WorkspaceManager::new(&repo.0).unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let state = Arc::new((Mutex::new(BlockingProviderState::default()), Condvar::new()));
        let providers = BlockingResolver(BlockingProvider {
            reference: ProviderRef::new("fake"),
            calls: calls.clone(),
            state: state.clone(),
        });
        let service =
            OperationService::new(&ledger, &workspace, &providers, 3, Duration::from_secs(30))
                .unwrap();
        let accepted = service
            .submit_attempt(&request(&repo, &task_id, 0, "process-lock"))
            .unwrap();
        let marker = repo.0.join("child-lock-result");
        let ledger_path_text = ledger_path.to_string_lossy().into_owned();
        let marker_text = marker.to_string_lossy().into_owned();
        thread::scope(|scope| {
            let run = scope.spawn(|| {
                service
                    .run(accepted.operation_id(), CancellationToken::new())
                    .unwrap()
            });
            let (provider_state, changed) = &*state;
            let provider_state = provider_state.lock().unwrap();
            let (mut provider_state, _) = changed
                .wait_timeout_while(provider_state, Duration::from_secs(10), |state| {
                    !state.started
                })
                .unwrap();
            let provider_started = provider_state.started;
            if !provider_started {
                provider_state.released = true;
                changed.notify_all();
                drop(provider_state);
                let _ = run.join();
                panic!("provider did not start before the deadline");
            }
            drop(provider_state);
            let mut child = spawn_test_child(
                "operation_service::tests::child_reports_lock_acquisition_during_active_service",
                &[
                    (CHILD_LEDGER_PATH, &ledger_path_text),
                    (CHILD_REPOSITORY_PATH, repo.0.to_str().unwrap()),
                    (CHILD_MARKER_PATH, &marker_text),
                ],
            );
            let marker_deadline = Instant::now() + Duration::from_secs(30);
            let mut marker_value = None;
            while Instant::now() < marker_deadline {
                if let Ok(value) = fs::read_to_string(&marker) {
                    marker_value = Some(value);
                    break;
                }
                thread::sleep(Duration::from_millis(10));
            }
            let Some(marker_value) = marker_value else {
                let _ = child.kill();
                let _ = child.wait();
                let (provider_state, changed) = &*state;
                provider_state.lock().unwrap().released = true;
                changed.notify_all();
                let _ = run.join();
                panic!("competing service did not report its lock result");
            };
            let child_success = child.wait().unwrap().success();
            let (provider_state, changed) = &*state;
            provider_state.lock().unwrap().released = true;
            changed.notify_all();
            let result = run.join().unwrap();
            assert_eq!(marker_value, "busy");
            assert!(child_success);
            assert_eq!(result.status(), ServiceOperationStatus::Completed);
        });
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn service_rejects_a_lock_for_a_different_ledger_identity() {
        let repo = Repo::new();
        let ledger_path = repo.0.join("execution.sqlite3");
        let other_path = repo.0.join("other.sqlite3");
        let ledger = SqliteExecutionLedger::open(&ledger_path).unwrap();
        let other_lock = crate::LedgerRunLock::acquire(&other_path).unwrap();
        let workspace = WorkspaceManager::new(&repo.0).unwrap();
        let resolver = FakeResolver(FakeProvider {
            calls: Arc::new(AtomicUsize::new(0)),
            availability_checks: Arc::new(AtomicUsize::new(0)),
            fail: false,
            unknown_interrupt: false,
            provider_failure: None,
            execute_delay: Duration::ZERO,
            reference: ProviderRef::new("fake"),
            write_output: None,
            write_ignored: None,
            write_gitignore: None,
            require_file: None,
        });
        assert!(matches!(
            OperationService::new_with_run_lock(
                &ledger,
                &workspace,
                &resolver,
                3,
                Duration::from_secs(30),
                Some(other_lock),
            ),
            Err(ServiceError::RecoveryLockMismatch)
        ));
    }

    #[test]
    fn process_crash_releases_lock_and_allows_service_recovery() {
        let repo = Repo::new();
        let ledger_path = repo.0.join("execution.sqlite3");
        let ledger = SqliteExecutionLedger::open(&ledger_path).unwrap();
        let task_id = TaskId::new("crashed-service-task");
        ledger
            .save_task(&Task::new(
                task_id.clone(),
                "task",
                TaskRole::new("implementer"),
            ))
            .unwrap();
        let workspace = WorkspaceManager::new(&repo.0).unwrap();
        let resolver = FakeResolver(FakeProvider {
            calls: Arc::new(AtomicUsize::new(0)),
            availability_checks: Arc::new(AtomicUsize::new(0)),
            fail: false,
            unknown_interrupt: false,
            provider_failure: None,
            execute_delay: Duration::ZERO,
            reference: ProviderRef::new("fake"),
            write_output: None,
            write_ignored: None,
            write_gitignore: None,
            require_file: None,
        });
        let service =
            OperationService::new(&ledger, &workspace, &resolver, 3, Duration::from_secs(30))
                .unwrap();
        let accepted = service
            .submit_attempt(&request(&repo, &task_id, 0, "process-crash-recovery"))
            .unwrap();
        drop(service);
        drop(ledger);

        let marker = repo.0.join("child-claimed");
        let ledger_path_text = ledger_path.to_string_lossy().into_owned();
        let repository_path = repo.0.to_string_lossy().into_owned();
        let operation_id = accepted.operation_id().as_str().to_owned();
        let marker_text = marker.to_string_lossy().into_owned();
        let mut child = spawn_test_child(
            "operation_service::tests::child_service_crashes_after_claim_and_parent_recovers",
            &[
                (CHILD_LEDGER_PATH, &ledger_path_text),
                (CHILD_REPOSITORY_PATH, &repository_path),
                (CHILD_OPERATION_ID, &operation_id),
                (CHILD_MARKER_PATH, &marker_text),
            ],
        );
        assert_eq!(wait_for_file(&marker, Duration::from_secs(30)), "claimed");
        child.kill().unwrap();
        assert!(!child.wait().unwrap().success());

        let ledger = SqliteExecutionLedger::open(&ledger_path).unwrap();
        let workspace = WorkspaceManager::new(&repo.0).unwrap();
        let service =
            OperationService::new(&ledger, &workspace, &resolver, 3, Duration::from_secs(30))
                .unwrap();
        let operation = service.get_operation(accepted.operation_id()).unwrap();
        assert_eq!(operation.status(), ServiceOperationStatus::RecoveryRequired);
        assert_eq!(operation.diagnostic_code(), Some("interrupted"));
    }

    #[test]
    fn named_model_is_fail_closed_before_provider_side_effects() {
        let (repo, ledger, workspace, providers, calls, checks, task_id) =
            service_parts(false, Duration::ZERO, false, None);
        let service =
            OperationService::new(&ledger, &workspace, &providers, 3, Duration::from_secs(30))
                .unwrap();
        let mut req = request(&repo, &task_id, 0, "named-model");
        req.model_id = ModelChoice::Named(ModelRef::new("unverified-model"));
        assert!(matches!(
            service.submit_attempt(&req),
            Err(ServiceError::NamedModelRequiresCatalog)
        ));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert_eq!(checks.load(Ordering::SeqCst), 0);
        assert!(
            ledger
                .get_task(&task_id)
                .unwrap()
                .unwrap()
                .attempts()
                .is_empty()
        );
    }

    #[test]
    fn non_implementer_role_is_rejected_before_persistence() {
        let (repo, ledger, workspace, providers, calls, _, task_id) =
            service_parts(false, Duration::ZERO, false, None);
        let service =
            OperationService::new(&ledger, &workspace, &providers, 3, Duration::from_secs(30))
                .unwrap();
        let mut req = request(&repo, &task_id, 0, "unsupported-role");
        req.role = TaskRole::new("reviewer");
        assert!(matches!(
            service.submit_attempt(&req),
            Err(ServiceError::PolicyDenied(
                "only implementer Attempts are currently representable"
            ))
        ));
        let connection = ledger.lock_connection().unwrap();
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM attempts", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM service_attempt_history", [], |row| {
                    row.get::<_, i64>(0)
                })
                .unwrap(),
            0
        );
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn base_input_acceptance_is_atomic_idempotent_and_records_safe_success() {
        let (repo, ledger, workspace, providers, calls, checks, task_id) =
            service_parts(false, Duration::ZERO, false, None);
        let service =
            OperationService::new(&ledger, &workspace, &providers, 3, Duration::from_secs(30))
                .unwrap();
        let req = request(&repo, &task_id, 0, "request-1");
        let accepted = service.submit_attempt(&req).unwrap();
        assert_eq!(accepted.status(), ServiceOperationStatus::Accepted);
        assert_eq!(accepted.revision(), 1);
        let replay = service.submit_attempt(&req).unwrap();
        assert_eq!(replay.operation_id(), accepted.operation_id());
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert!(matches!(
            service.submit_attempt(&request(&repo, &task_id, 1, "request-2")),
            Err(ServiceError::Busy(_))
        ));
        assert!(matches!(
            service.submit_attempt(&request(&repo, &task_id, 0, "request-3")),
            Err(ServiceError::StaleRevision { .. })
        ));

        let result = service
            .run(accepted.operation_id(), CancellationToken::new())
            .unwrap();
        assert_eq!(result.status(), ServiceOperationStatus::Completed);
        assert_eq!(result.attempt_state(), AttemptState::Succeeded);
        assert!(result.output_artifact_id().is_some());
        assert_eq!(result.observed_model(), None);
        assert_eq!(
            result.usage(),
            [UsageMetric::new("input_tokens", "7", "token")]
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(checks.load(Ordering::SeqCst), 1);
        let saved = ledger.get_task(&task_id).unwrap().unwrap();
        let attempt = saved.attempt(result.attempt_id()).unwrap();
        assert_eq!(attempt.semantics(), AttemptSemantics::ProviderCallV2);
        assert!(attempt.validation_results().is_empty());
        assert!(attempt.agent_result().is_none());
        let connection = ledger.lock_connection().unwrap();
        let agent_rows: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM agent_results WHERE task_id=?1 AND attempt_id=?2",
                params![task_id.as_str(), result.attempt_id().as_str()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(agent_rows, 0);
        let unsafe_text:i64=connection.query_row("SELECT COUNT(*) FROM service_operations WHERE id=?1 AND (instruction LIKE '%RAW_STDOUT_SECRET%' OR instruction LIKE '%RAW_STDERR_SECRET%' OR diagnostic_code LIKE '%SECRET%')",params![result.operation_id().as_str()],|row|row.get(0)).unwrap();
        assert_eq!(unsafe_text, 0);
        let initial_relation: (i64, String, String, Option<String>) = connection.query_row(
            "SELECT sequence, role, relation_kind, related_attempt_id FROM service_attempt_history
             WHERE task_id=?1 AND attempt_id=?2",
            params![task_id.as_str(), result.attempt_id().as_str()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        ).unwrap();
        assert_eq!(
            initial_relation,
            (1, "implementer".into(), "initial".into(), None)
        );
        let revision: u64 = connection
            .query_row(
                "SELECT revision FROM service_task_revisions WHERE task_id=?1",
                params![task_id.as_str()],
                |row| row.get::<_, i64>(0),
            )
            .unwrap() as u64;
        drop(connection);
        assert!(matches!(
            service.submit_attempt(&request(&repo, &task_id, revision, "unsupported-follow-up")),
            Err(ServiceError::PolicyDenied(
                "follow-up Attempt relation requires explicit supporting evidence"
            ))
        ));
        let connection = ledger.lock_connection().unwrap();
        let history_rows: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM service_attempt_history WHERE task_id=?1",
                params![task_id.as_str()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(history_rows, 1);
        assert_eq!(
            connection
                .query_row::<i64, _, _>(
                    "SELECT revision FROM service_task_revisions WHERE task_id=?1",
                    params![task_id.as_str()],
                    |row| row.get(0)
                )
                .unwrap() as u64,
            revision
        );
        drop(connection);
        assert!(
            workspace
                .worktree_path(&task_id, result.attempt_id())
                .exists()
        );
        assert_eq!(
            result.workspace_path(),
            Some(
                workspace
                    .worktree_path(&task_id, result.attempt_id())
                    .as_path()
            )
        );
        assert!(result.workspace_branch().is_some());
        cleanup_fixture_worktree(&repo, &workspace, &task_id, result.attempt_id());
    }

    #[test]
    fn ignored_provider_output_blocks_artifact_success_and_preserves_workspace() {
        let repo = Repo::new();
        fs::write(repo.0.join(".gitignore"), "*.excluded\n").unwrap();
        git(&repo.0, &["add", ".gitignore"]);
        git(&repo.0, &["commit", "-m", "ignore generated output"]);
        let ledger = SqliteExecutionLedger::open_in_memory().unwrap();
        let task_id = TaskId::new("ignored-output-task");
        ledger
            .save_task(&Task::new(
                task_id.clone(),
                "task with ignored output",
                TaskRole::new("implementer"),
            ))
            .unwrap();
        let workspace = WorkspaceManager::new(&repo.0).unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let providers = FakeResolver(FakeProvider {
            calls: calls.clone(),
            availability_checks: Arc::new(AtomicUsize::new(0)),
            fail: false,
            unknown_interrupt: false,
            provider_failure: None,
            execute_delay: Duration::ZERO,
            reference: ProviderRef::new("fake"),
            write_output: None,
            write_ignored: Some(("secret.excluded".into(), "not in artifact".into())),
            write_gitignore: None,
            require_file: None,
        });
        let service =
            OperationService::new(&ledger, &workspace, &providers, 3, Duration::from_secs(30))
                .unwrap();
        let accepted = service
            .submit_attempt(&request(&repo, &task_id, 0, "ignored-output"))
            .unwrap();
        let result = service
            .run(accepted.operation_id(), CancellationToken::new())
            .unwrap();

        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(result.status(), ServiceOperationStatus::RecoveryRequired);
        assert_eq!(result.diagnostic_code(), Some("artifact_capture_failed"));
        assert_eq!(result.output_artifact_id(), None);
        let workspace_path = result.workspace_path().unwrap();
        assert_eq!(
            fs::read_to_string(workspace_path.join("secret.excluded")).unwrap(),
            "not in artifact"
        );
        let artifact_count: i64 = ledger
            .lock_connection()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM service_artifacts", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(artifact_count, 0);
        cleanup_fixture_worktree(&repo, &workspace, &task_id, result.attempt_id());
    }

    #[test]
    fn artifact_input_requires_review_evidence_before_acceptance() {
        let (repo, ledger, workspace, providers, calls, _, task_id) =
            service_parts(false, Duration::ZERO, false, None);
        let service =
            OperationService::new(&ledger, &workspace, &providers, 4, Duration::from_secs(30))
                .unwrap();
        let first = service
            .submit_attempt(&request(&repo, &task_id, 0, "artifact-evidence-first"))
            .unwrap();
        let result = service
            .run(first.operation_id(), CancellationToken::new())
            .unwrap();
        assert_eq!(result.status(), ServiceOperationStatus::Completed);
        let artifact_id = result.output_artifact_id().unwrap().to_owned();
        let revision: u64 = ledger
            .lock_connection()
            .unwrap()
            .query_row(
                "SELECT revision FROM service_task_revisions WHERE task_id=?1",
                params![task_id.as_str()],
                |row| row.get::<_, i64>(0),
            )
            .unwrap() as u64;
        let input = AttemptRunRequest::with_artifact(
            "artifact-evidence-follow-up",
            task_id.clone(),
            revision,
            ProviderRef::new("fake"),
            ModelChoice::ProviderDefault,
            "revise the reviewed Artifact",
            TaskRole::new("implementer"),
            ArtifactInput::new(artifact_id),
        );
        assert!(matches!(
            service.submit_attempt(&input),
            Err(ServiceError::PolicyDenied(
                "ArtifactInput requires a successful changes_requested ReviewVerdict"
            ))
        ));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        let connection = ledger.lock_connection().unwrap();
        let counts: (i64, i64, i64) = connection
            .query_row(
                "SELECT (SELECT COUNT(*) FROM attempts WHERE task_id=?1),\
                        (SELECT COUNT(*) FROM service_operations WHERE task_id=?1),\
                        (SELECT COUNT(*) FROM service_attempt_history WHERE task_id=?1)",
                params![task_id.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(counts, (1, 1, 1));
        assert_eq!(
            connection
                .query_row(
                    "SELECT revision FROM service_task_revisions WHERE task_id=?1",
                    params![task_id.as_str()],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap() as u64,
            revision
        );
        drop(connection);
        cleanup_fixture_worktree(&repo, &workspace, &task_id, result.attempt_id());
    }

    #[test]
    fn provider_timeout_is_recorded_without_raw_streams() {
        let (repo, ledger, workspace, providers, calls, _, task_id) =
            service_parts(true, Duration::ZERO, false, None);
        let service =
            OperationService::new(&ledger, &workspace, &providers, 3, Duration::from_secs(30))
                .unwrap();
        let accepted = service
            .submit_attempt(&request(&repo, &task_id, 0, "timeout-request"))
            .unwrap();
        let result = service
            .run(accepted.operation_id(), CancellationToken::new())
            .unwrap();
        assert_eq!(result.status(), ServiceOperationStatus::Failed);
        assert_eq!(result.attempt_state(), AttemptState::Failed);
        assert_eq!(result.diagnostic_code(), Some("timeout"));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        let connection = ledger.lock_connection().unwrap();
        let count:i64=connection.query_row("SELECT COUNT(*) FROM service_operations WHERE id=?1 AND (diagnostic_code LIKE '%RAW%' OR instruction LIKE '%RAW_STDOUT_SECRET%')",params![result.operation_id().as_str()],|row|row.get(0)).unwrap();
        assert_eq!(count, 0);
        drop(connection);
        cleanup_fixture_worktree(&repo, &workspace, &task_id, result.attempt_id());
    }

    #[test]
    fn provider_failures_and_cancelled_output_are_persisted_without_raw_diagnostics() {
        for (request_id, failure, status, attempt_state, diagnostic) in [
            (
                "provider-execution-failure",
                FakeProviderFailure::ExecutionFailed,
                ServiceOperationStatus::Failed,
                AttemptState::Failed,
                "provider_failed",
            ),
            (
                "provider-cancelled",
                FakeProviderFailure::Cancelled,
                ServiceOperationStatus::Cancelled,
                AttemptState::Cancelled,
                "cancelled",
            ),
            (
                "provider-cancelled-output",
                FakeProviderFailure::CancelledWithOutput,
                ServiceOperationStatus::Cancelled,
                AttemptState::Cancelled,
                "cancelled",
            ),
        ] {
            let (repo, ledger, workspace, providers, calls, _, task_id) =
                service_parts(false, Duration::ZERO, false, Some(failure));
            let service =
                OperationService::new(&ledger, &workspace, &providers, 3, Duration::from_secs(30))
                    .unwrap();
            let accepted = service
                .submit_attempt(&request(&repo, &task_id, 0, request_id))
                .unwrap();
            let cancellation = CancellationToken::new();
            let cancellation_driver = if matches!(failure, FakeProviderFailure::Cancelled) {
                let driver_token = cancellation.clone();
                Some(thread::spawn(move || {
                    thread::sleep(Duration::from_millis(10));
                    driver_token.cancel();
                }))
            } else {
                None
            };
            let result = service.run(accepted.operation_id(), cancellation).unwrap();
            if let Some(driver) = cancellation_driver {
                driver.join().unwrap();
            }

            assert_eq!(result.status(), status);
            assert_eq!(result.attempt_state(), attempt_state);
            assert_eq!(result.diagnostic_code(), Some(diagnostic));
            assert_eq!(calls.load(Ordering::SeqCst), 1);
            let snapshot_text = format!("{result:?}");
            assert!(!snapshot_text.contains("RAW_STDOUT_SECRET"));
            assert!(!snapshot_text.contains("RAW_STDERR_SECRET"));
            assert_eq!(
                service
                    .run(accepted.operation_id(), CancellationToken::new())
                    .unwrap(),
                result
            );
            assert_eq!(calls.load(Ordering::SeqCst), 1);
            let connection = ledger.lock_connection().unwrap();
            let raw_output_count: i64 = connection
                .query_row(
                    "SELECT COUNT(*) FROM service_operations WHERE id=?1 AND (instruction LIKE '%RAW_STDOUT_SECRET%' OR instruction LIKE '%RAW_STDERR_SECRET%' OR diagnostic_code LIKE '%RAW_%')",
                    params![result.operation_id().as_str()],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(raw_output_count, 0);
            drop(connection);
            cleanup_fixture_worktree(&repo, &workspace, &task_id, result.attempt_id());
        }
    }

    #[test]
    fn unconfirmed_provider_stop_preserves_running_attempt_as_unknown() {
        let (repo, ledger, workspace, providers, calls, _, task_id) =
            service_parts(false, Duration::ZERO, true, None);
        let service =
            OperationService::new(&ledger, &workspace, &providers, 3, Duration::from_secs(30))
                .unwrap();
        let accepted = service
            .submit_attempt(&request(&repo, &task_id, 0, "unknown-interrupt"))
            .unwrap();
        let result = service
            .run(accepted.operation_id(), CancellationToken::new())
            .unwrap();
        assert_eq!(result.status(), ServiceOperationStatus::RecoveryRequired);
        assert_eq!(result.attempt_state(), AttemptState::Running);
        assert_eq!(result.diagnostic_code(), Some("provider_interrupted"));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        let connection = ledger.lock_connection().unwrap();
        let status: String = connection
            .query_row(
                "SELECT status FROM service_operations WHERE id=?1",
                params![result.operation_id().as_str()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(status, "recovery_required");
        let raw: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM service_operations WHERE id=?1 AND (diagnostic_code LIKE '%RAW%' OR workspace_path LIKE '%RAW%')",
                params![result.operation_id().as_str()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(raw, 0);
        drop(connection);
        let repeated = service
            .run(accepted.operation_id(), CancellationToken::new())
            .unwrap();
        assert_eq!(repeated.status(), ServiceOperationStatus::RecoveryRequired);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        cleanup_fixture_worktree(&repo, &workspace, &task_id, result.attempt_id());
    }

    #[cfg(unix)]
    #[test]
    fn post_checkout_ignored_file_is_preserved_and_provider_is_not_started() {
        use std::os::unix::fs::PermissionsExt;

        let (repo, ledger, workspace, providers, calls, _, task_id) =
            service_parts(false, Duration::ZERO, false, None);
        fs::write(repo.0.join(".gitignore"), "hook-secret.txt\n").unwrap();
        git(&repo.0, &["add", ".gitignore"]);
        git(&repo.0, &["commit", "-m", "ignore hook fixture"]);
        let base = repo.commit();
        let hooks = repo.0.join(".git").join("hooks");
        fs::create_dir_all(&hooks).unwrap();
        let hooks_path = hooks.to_string_lossy().into_owned();
        git(
            &repo.0,
            &["config", "--local", "core.hooksPath", &hooks_path],
        );
        let hook = hooks.join("post-checkout");
        fs::write(
            &hook,
            "#!/bin/sh\nprintf 'hook output must be preserved\\n' > hook-secret.txt\n",
        )
        .unwrap();
        let mut permissions = fs::metadata(&hook).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&hook, permissions).unwrap();

        let service =
            OperationService::new(&ledger, &workspace, &providers, 3, Duration::from_secs(30))
                .unwrap();
        let mut req = request(&repo, &task_id, 0, "post-checkout-hook");
        req.input = AttemptInput::Base(BaseInput::new(&repo.0, base.clone()));
        let accepted = service.submit_attempt(&req).unwrap();
        let result = service
            .run(accepted.operation_id(), CancellationToken::new())
            .unwrap();

        assert_eq!(result.status(), ServiceOperationStatus::Failed);
        assert_eq!(result.attempt_state(), AttemptState::Queued);
        assert_eq!(result.diagnostic_code(), Some("workspace_unavailable"));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        let workspace_path = result.workspace_path().unwrap();
        assert_eq!(git(workspace_path, &["rev-parse", "HEAD"]), base);
        assert_eq!(
            fs::read_to_string(workspace_path.join("hook-secret.txt")).unwrap(),
            "hook output must be preserved\n"
        );

        // The test owns this fixture and removes it explicitly after checking preservation.
        cleanup_fixture_worktree(&repo, &workspace, &task_id, result.attempt_id());
    }

    #[test]
    fn failed_acceptance_rolls_back_operation_task_and_attempt_together() {
        let (repo, ledger, workspace, providers, calls, _, task_id) =
            service_parts(false, Duration::ZERO, false, None);
        let connection = ledger.lock_connection().unwrap();
        connection.execute_batch("CREATE TRIGGER reject_service_attempt BEFORE INSERT ON attempts WHEN NEW.semantics_version='provider_call_v2' BEGIN SELECT RAISE(ABORT, 'test transaction rollback'); END;").unwrap();
        drop(connection);
        let service =
            OperationService::new(&ledger, &workspace, &providers, 3, Duration::from_secs(30))
                .unwrap();
        assert!(
            service
                .submit_attempt(&request(&repo, &task_id, 0, "rollback-request"))
                .is_err()
        );
        assert_eq!(
            ledger.get_task(&task_id).unwrap().unwrap().state(),
            TaskState::Pending
        );
        assert!(
            ledger
                .get_task(&task_id)
                .unwrap()
                .unwrap()
                .attempts()
                .is_empty()
        );
        let connection = ledger.lock_connection().unwrap();
        let operation_count: i64 = connection
            .query_row("SELECT COUNT(*) FROM service_operations", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(operation_count, 0);
        let history_count: i64 = connection
            .query_row("SELECT COUNT(*) FROM service_attempt_history", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(history_count, 0);
        let revision: i64 = connection
            .query_row(
                "SELECT revision FROM service_task_revisions WHERE task_id=?1",
                params![task_id.as_str()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(revision, 0);
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn concurrent_run_claims_execute_provider_only_once() {
        let repo = Repo::new();
        let ledger = SqliteExecutionLedger::open_in_memory().unwrap();
        let task_id = TaskId::new("concurrent-task");
        ledger
            .save_task(&Task::new(
                task_id.clone(),
                "task description",
                TaskRole::new("implementer"),
            ))
            .unwrap();
        let workspace = WorkspaceManager::new(&repo.0).unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let providers = FakeResolver(FakeProvider {
            calls: calls.clone(),
            availability_checks: Arc::new(AtomicUsize::new(0)),
            fail: false,
            unknown_interrupt: false,
            provider_failure: None,
            execute_delay: Duration::from_millis(250),
            reference: ProviderRef::new("fake"),
            write_output: None,
            write_ignored: None,
            write_gitignore: None,
            require_file: None,
        });
        let service =
            OperationService::new(&ledger, &workspace, &providers, 3, Duration::from_secs(30))
                .unwrap();
        let accepted = service
            .submit_attempt(&request(&repo, &task_id, 0, "concurrent-run"))
            .unwrap();
        let barrier = Arc::new(Barrier::new(3));
        let service_ref = &service;
        thread::scope(|scope| {
            let first_barrier = barrier.clone();
            let first_id = accepted.operation_id().clone();
            let first = scope.spawn(move || {
                first_barrier.wait();
                service_ref
                    .run(&first_id, CancellationToken::new())
                    .unwrap()
            });
            let second_barrier = barrier.clone();
            let second_id = accepted.operation_id().clone();
            let second = scope.spawn(move || {
                second_barrier.wait();
                service_ref
                    .run(&second_id, CancellationToken::new())
                    .unwrap()
            });
            barrier.wait();
            let first = first.join().unwrap();
            let second = second.join().unwrap();
            assert!(matches!(
                first.status(),
                ServiceOperationStatus::Running | ServiceOperationStatus::Completed
            ));
            assert!(matches!(
                second.status(),
                ServiceOperationStatus::Running | ServiceOperationStatus::Completed
            ));
        });
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        let snapshot = service.get_operation(accepted.operation_id()).unwrap();
        cleanup_fixture_worktree(&repo, &workspace, &task_id, snapshot.attempt_id());
    }

    #[test]
    fn startup_recovery_closes_running_operation_without_replay() {
        let repo = Repo::new();
        let ledger_path = repo.0.join("execution.sqlite3");
        let ledger = SqliteExecutionLedger::open(&ledger_path).unwrap();
        let task_id = TaskId::new("service-task");
        ledger
            .save_task(&Task::new(
                task_id.clone(),
                "task description",
                TaskRole::new("implementer"),
            ))
            .unwrap();
        let workspace = WorkspaceManager::new(&repo.0).unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let mut providers = ProviderRegistry::new();
        providers.register(FakeProvider {
            calls: calls.clone(),
            availability_checks: Arc::new(AtomicUsize::new(0)),
            fail: false,
            unknown_interrupt: false,
            provider_failure: None,
            execute_delay: Duration::ZERO,
            reference: ProviderRef::new("fake"),
            write_output: None,
            write_ignored: None,
            write_gitignore: None,
            require_file: None,
        });
        let service =
            OperationService::new(&ledger, &workspace, &providers, 3, Duration::from_secs(30))
                .unwrap();
        let accepted = service
            .submit_attempt(&request(&repo, &task_id, 0, "crash-recovery"))
            .unwrap();
        assert!(service.claim_operation(accepted.operation_id()).unwrap());
        service
            .record_workspace_locator(accepted.operation_id(), &task_id, accepted.attempt_id())
            .unwrap();
        // Simulate a process crash immediately after the filesystem side effect,
        // before validation or the Provider call can start.
        let retained_workspace = workspace
            .create_at_base(&task_id, accepted.attempt_id(), &repo.commit())
            .unwrap();
        let retained_path = retained_workspace.path().to_path_buf();
        let retained_branch = retained_workspace.branch().to_owned();
        drop(service);
        drop(ledger);
        drop(workspace);

        let ledger = SqliteExecutionLedger::open(&ledger_path).unwrap();
        let workspace = WorkspaceManager::new(&repo.0).unwrap();
        let service =
            OperationService::new(&ledger, &workspace, &providers, 3, Duration::from_secs(30))
                .unwrap();
        let snapshot = service.get_operation(accepted.operation_id()).unwrap();
        assert_eq!(snapshot.status(), ServiceOperationStatus::RecoveryRequired);
        assert_eq!(snapshot.attempt_state(), AttemptState::Queued);
        assert_eq!(snapshot.diagnostic_code(), Some("interrupted"));
        assert_eq!(snapshot.workspace_path(), Some(retained_path.as_path()));
        assert_eq!(snapshot.workspace_branch(), Some(retained_branch.as_str()));
        assert!(snapshot.workspace_path().unwrap().exists());
        let repeated = service
            .run(accepted.operation_id(), CancellationToken::new())
            .unwrap();
        assert_eq!(repeated.status(), ServiceOperationStatus::RecoveryRequired);
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        cleanup_fixture_worktree(&repo, &workspace, &task_id, accepted.attempt_id());
    }

    #[test]
    fn second_in_memory_service_does_not_recover_another_service_live_operation() {
        let (repo, ledger, workspace, providers, _, _, task_id) =
            service_parts(false, Duration::ZERO, false, None);
        let first =
            OperationService::new(&ledger, &workspace, &providers, 3, Duration::from_secs(30))
                .unwrap();
        let accepted = first
            .submit_attempt(&request(&repo, &task_id, 0, "memory-live-operation"))
            .unwrap();
        assert!(first.claim_operation(accepted.operation_id()).unwrap());

        let second =
            OperationService::new(&ledger, &workspace, &providers, 3, Duration::from_secs(30))
                .unwrap();
        let snapshot = second.get_operation(accepted.operation_id()).unwrap();
        assert_eq!(snapshot.status(), ServiceOperationStatus::Running);
    }
}
